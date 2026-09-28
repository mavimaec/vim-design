# VIM Design — Authoring Tool (house modeler)

Direction set 2026-08-23: the web demo evolves into an authoring tool for modeling a
simple house. This document records the data model and UI rules; ARCHITECTURE.md remains
the kernel contract. Expect revision — the direction is exploratory by design ("no
expectation to get this right on the first shot"), but decisions here are chosen to
avoid known failures of existing tools (see the pitfall table, §7).

## 1. Geolocation (Site)

- A **`Site` entity** (singleton per document — `CreateSite` is rejected if one exists)
  holds `latitude`, `longitude`, `elevation_m`, and `true_north_deg` (bearing of +Y,
  for future sun studies). Being an entity makes it undoable, serialized, and visible
  to the dirty pump like everything else.
- **Default: downtown Montreal** — `45.5019 N, −73.5674 W, 36 m` (authored by the app
  at new-document time, not hardcoded in the library).
- **Modeling never happens in geographic coordinates.** The model lives in local
  right-handed Z-up Cartesian meters around one scene origin; `Site` is *metadata
  mapping that origin to Earth*. There is exactly **one** origin — no survey-point /
  project-base-point dual-origin scheme (a notorious Revit confusion generator).

## 2. Levels

- A **`Level` entity**: `name`, `elevation_m` (relative to scene origin),
  `is_building_story: bool` (true ⇒ a floor a person can stand on), `color: RGBA`
  (user-customizable), `extent_m` (half-size of the display square). No input slots.
- A level **evaluates to a Frame** (origin `(0, 0, elevation)`, axes = world X/Y/Z) —
  this is what makes it a construction plane (§3).
- **Display**: bounded square translucent colored planes at their elevations. These are
  a *view overlay* drawn by the renderer from Level params — never tessellated BREP,
  never mesh owners. (Model/view separation: a level has no volume.)
- **Ordering** is by elevation, always derived — there is no stored order to corrupt.
  "Reordering" is editing elevations; names are free-form.
- Levels can be added/updated/removed retroactively at any time. **Removal**: the
  library rejects `DeleteLevel` while dependents exist (standard reject-if-dependents);
  the UI prompts the user, and on confirmation submits the cascade form
  (`DeleteLevel { cascade: true }`), which expands to a leaf-first subgraph delete
  **in one undo group** — so undoing a confirmed level deletion restores the level
  *and* every associated element atomically. (Contrast: Revit deletes hosted elements
  with a warning many users miss, and undo granularity is not guaranteed.)
  *Library precision (updated 2026-08-23, orphan-sweep milestone)*: the cascade
  deletes the level's **transitive dependent closure** (attached control points and
  everything downstream; associated elements — association is mandatory, so every
  element of the level is in the closure — and their instances), and every cascaded
  element deletion additionally runs the **orphan sweep** over its construction-input
  closure. Result: a level cascade leaves **zero orphaned geometry** — the earlier
  "merely-associated elements keep their geometry" behavior is obsolete. Survivors are
  only entities still referenced elsewhere (shared inputs, selection scopes) and the
  never-swept kinds (Site, remaining Levels, Materials, Planes, Selections). The UI
  confirmation dialog wording must be updated accordingly in the next demo milestone.

## 3. Construction planes

- **Definition**: a construction plane is any entity whose evaluation yields a `Frame`
  (origin + orthonormal basis). v1: `Level` only. Planned: `FaceFrame` — a frame
  derived from a planar face addressed by `SubRef` (e.g. a wall face hosting a window).
- **Attachment**: `ControlPoint` gains an optional `plane` input slot (the
  accepted-kinds list on that slot is the extension point for future `FaceFrame`
  producers — acceptance only widens, so existing documents keep validating). When
  wired, its stored coordinates are interpreted as `(u, v, w)` in the frame's basis — `w` is the
  "relative offset up or down in Z from that level". Because attachment is an ordinary
  dependency edge, **changing a level's elevation re-evaluates every attached point and
  everything downstream** — floor plates, walls, columns ride along. This is the core
  parametric payoff and needs no new machinery.
- **Basis stability rule (mandatory, same spirit as provenance naming, ARCHITECTURE
  §3.4)**: a frame's basis must be a *deterministic function of stable inputs*, never
  of kernel output order or float noise. Levels are trivial (world axes). For future
  `FaceFrame`s: normal from the face, in-plane X = the world axis most orthogonal to
  the normal, projected and normalized, with a fixed documented tie-break — so a wall
  face that shifts slightly never flips or spins its sketch basis ("oriented sensibly
  to avoid undesirable rotations"). Fusion/SketchUp-style sketch-plane flipping is the
  frame version of the topological naming problem, and it is designed out the same way.
- **Attach/detach conversions preserve world position**: when the UI attaches an
  existing point to a plane (or detaches it), it computes the equivalent local/world
  coordinates and submits both the rewire and the coordinate change in one command
  group — the geometry does not jump. The library stays dumb (slot + params); the
  conversion arithmetic is the intent layer's job.
  *Library precision (Phase A, 2026-08-23)*: the "one command group" is literally one
  command — `UpdateControlPointPlane { id, plane: Option, position: Option }` carries
  the rewire and the optional simultaneous coordinate rewrite atomically (one undo
  step). Passing `position: None` keeps the stored coordinates verbatim, which
  *reinterprets* them in the new frame (the point may jump — that is the caller's
  explicit choice).

## 4. Association vs. attachment (two different relationships)

- **Attachment** (geometric): control point → construction plane. Moves geometry.
- **Association** (data): `Element` has a **required** `level` slot (decision
  2026-08-23: every element is associated with exactly one level; element creation is
  structurally impossible in a document with no levels). "This wall belongs to
  Level 2" — for organization, filtering, schedules, and the deletion cascade. It has
  **no geometric effect** (*library precision*: the rewire dirties the element
  parametrically — the pump reports it, and the mesh is re-delivered byte-identical;
  the element evaluator ignores the slot. `CreateElement` takes the level id;
  `UpdateElementLevel { element, level }` re-associates — there is no dissociated
  state. Pre-1.0 serialization note: a loaded document containing an element without
  a level is malformed and rejected with the typed load error); an element associated with a level whose geometry is
  attached to a different plane is legal (useful for e.g. a roof associated with the
  top story but modeled from a sloped plane).
- Every new element is associated with the **active level** at creation time.
- **Element deletion sweeps orphans** (2026-08-23): `DeleteElement { sweep_orphans }`
  defaults to sweeping — after the element goes, its construction-input closure is
  reference-count-collected leaf-first in the same undo group, so deleting an element
  removes its private geometry completely while shared inputs (party-wall points,
  selection-scoped entities) survive by the ordinary dependent rules.
  `sweep_orphans: false` is the keep-geometry escape hatch for re-grouping. Flagged
  decisions: the sweep touches only construction kinds — **`Plane` is excluded as
  authored reference geometry**, along with Site/Level/Material/Selection/Element/
  Instance; and **`DeleteInstance` never sweeps** — deleting the last placement must
  not destroy the reusable element definition.

## 5. Active level and active construction plane (session state, not document state)

- There is always exactly **one active level** (association target) and **one active
  construction plane** (modeling target). By default the active plane *is* the active
  level's plane; picking a face frame later changes the plane without changing the
  level.
- Both live in the **application session, never in the document**: they are authoring
  cursor state, like the camera — putting them in the undo history would make undo
  teleport the user's context (ARCHITECTURE §14 guardrail: view state never routes
  through the undoable command system). If per-document persistence is wanted later,
  it becomes non-undoable document metadata — never entities, never deltas.

## 6. Roadmap (phased, revisable)

1. **Phase A (library)**: `Site` + `Level` entities, `Frame` evaluated type,
   ControlPoint `plane` slot with (u,v,w) interpretation, Element `level` slot,
   `DeleteLevel { cascade }`, tests (elevation edit re-evaluates attached chain;
   cascade = one undo group; pump reports level edits).
2. **Phase B (app)**: project settings panel (lat/long/elev), level manager (list
   sorted by elevation, add/rename/re-elevate, story checkbox, color picker,
   remove-with-prompt), level overlay rendering, active level/plane selection,
   existing demo objects re-authored as level-attached.
3. **Phase C (house)**: footprint drawing on the active plane → floor plates, walls,
   columns; then stud assemblies, beams, joists, ceilings, roofs.
4. **Parked for brainstorming**: room shapes/volumes for space partitioning (needs
   formalization — candidate approaches: closed-profile prisms between levels;
   boundary-driven auto-detection from walls; explicit space entities). Staircases
   (multi-level spanning elements — will stress the one-level association model;
   expect design iteration).

## 7. Pitfalls of existing tools, and the guard here

| Pitfall (tool) | Guard in this design |
|---|---|
| Deleting a level silently deletes hosted elements; easy to miss the warning (Revit) | Library *rejects* the delete; UI must explicitly confirm; cascade is one undo group, fully restorable |
| Dual origins — survey point vs. project base point confusion (Revit) | One origin; `Site` is metadata, modeling is always local meters |
| Modeling in geographic coordinates → float precision loss far from origin | Geographic coords never enter geometry; meters-around-origin only |
| Sketch-plane basis flips/spins when the host face shifts (Fusion, SketchUp) | Deterministic basis rule with fixed tie-break (§3), the frame analog of provenance naming |
| Story-sensitivity surprises — elements shift unexpectedly when story elevations change (ArchiCAD) | Attachment is an explicit, visible dependency edge with an explicit `w` offset; nothing is implicitly story-sensitive; association (data) is separated from attachment (geometry) |
| Hosted elements hard to re-host (Revit) | Attachment is an ordinary rewire command — undoable, composable, with world-position-preserving conversion in the UI |
| Active view/level state polluting undo history | Session state, never document state (§5) |
| Stored story order drifting from elevations | Order is always derived from elevation, never stored |

## 8. Web authoring app

The GitHub Pages site (`www/app.html`, `AuthorApp`) is a mobile + desktop authoring
front end over the same document. The element list it shows is derived from the
document after every change; every action is one undo step; the document persists in
the browser (`localStorage`, VIMD bytes as base64) and exports/imports as `.vimd`.

**Tools.** Select (properties, delete), Floor plate, Hole, Wall, Window / Door. Plan view
(orthographic onto the active plane, cut 1.2 m above it) and 3D view both accept
drawing. Snapping, in priority order: the first vertex (closes the outline), existing
corners, existing edges (plate outlines and wall base lines — tracing a plate edge is
how walls go onto a plate), horizontal/vertical alignment with the previous and first
vertex, the grid. Outlines the kernel would accept but that are wrong (a self-crossing
or zero-area face) are rejected live in the UI.

**Model tree.** A panel beside the tool dock (desktop) or a bottom sheet (phone, from the
top bar) lists the model by story level, highest first. Under each level: its workplanes
(nested, with their offset) and its elements grouped Floors / Walls / Other. Tapping an
element selects it, frames it, and opens its properties; tapping a level or a workplane
makes it the active plane (session state, no undo step). "+" on a level adds a
workplane; a workplane's pencil opens its settings. Collapsed groups are remembered.

**Workplanes** (§10) are construction planes nested in a level: a ceiling, a sill plane.
A new one sits 2.40 m above its level (0.30 m above a parent workplane) in the parent's
color. Its sheet edits the name, the offset (one undo step; a drag coalesces), and the
color; "Workplane inside" nests another. Deleting one that holds anything shows what
goes with it — nested workplanes and the elements drawn on them — and says which walls
only reach up to it (they keep their current height); one Undo restores everything.
Menu → Levels lists each level's workplanes too. The active plane is shown by the chip
("Ground › Ceiling +2.40 m"); drawing happens on it, its grid is shown, other planes
show faint outlines, and new elements are associated with its root level. A workplane
moves with its level (transform-only for what is on it).

**Floor plates are sketches, authored in Edit Mode.** A floor plate is an element whose
member is a `Sketch` (§9) hanging below its level. The Floor tool opens a new plate in
Edit Mode; the pencil ("Edit shape") in a plate's properties opens an existing one; the
Hole tool opens the tapped plate with the Void tool armed. Edit Mode is modal:

- **Enter / leave.** ✓ keeps the changes as ONE undo step of the main history; ✗
  reverts the document to its state at entry (byte-identical). Inside, Undo/Redo step
  through single edits and never past the entry. A new plate is created with its first
  face; ✓ with no faces leaves nothing (an existing plate left without faces is
  deleted). Menu and level switching are unavailable while editing; autosave pauses
  and saves when the session ends, so a closed tab comes back as it was before editing.
- **Tools.** Selection mode Points | Edges | Faces; Solid face and Void face (polygon or
  rectangle); Split line (two points; splits every face it crosses); Delete. The
  thickness panel edits the selected faces — thickness for solids, depth or "Through"
  for voids — or the defaults for new faces. Every edit is one `UpdateSketch`; slider
  and typing gestures coalesce into one step, and the 3D mesh follows live.
- **Gestures.** Tap selects (shift/ctrl adds on desktop), tap empty space clears; drag
  on an item moves the selection (snapping to other points, alignment, grid; a move that
  would cross a face snaps back); drag on empty space draws a selection box; hold on an
  edge in Points mode inserts a point there; two fingers always navigate.
- **Deletes.** A face takes its private points with it; an edge merges its two points
  into the first one in loop order; a point is removed and its neighbours reconnect.
- **Legacy plates** (extrusion + hole wires, from before sketches) still display, select,
  and delete. The pencil converts one in place (same element, name, and level; outline →
  solid face with its thickness, each hole → through void; the old construction chain
  is deleted) inside the Edit Mode session, so ✗ undoes the conversion too.

**One history.** Every change is one step of one linear undo history, in or out of an
Edit Mode, with one Undo / Redo control in the same place in every mode (toasts carry
no Undo). An Edit Mode is a span of that history: inside it Undo stops at the entry;
✓ keeps its steps as they are; ✗ undoes back to the entry and drops them.

**View.** Fit (always shown, also in Edit Modes) frames the element being edited, else
the selection, else the model, in the part of the canvas the panels leave free; the
camera stays near the model. The View menu switches Shaded / Shaded + wireframe /
Wireframe (the triangle edges) and shows the focus's triangle count. Two fingers pan
or zoom — classified once, then locked until a finger lifts; in 3D a clearly dominant
twist orbits.

**Walls are wall runs** (§11). The Wall tool draws one `WallRun` per run — open, or
closed by tapping the first point — as one element; the drawn line is one face of the
walls and the thickness grows to its left (a closed loop is made counter-clockwise, so
it grows inward; "flip side" reverses the run). The library miters the joins at any
angle (beveled past 4 thicknesses); the preview shows the same footprint. The height is
**Fixed**, or **Up to** a plane (a level or a workplane, plus an offset): the run's top
follows it, and deleting that plane disconnects the run at its current height. A
base-only run moves by transform only when its level is dragged.

**Wall Edit Mode** (the pencil) edits the run in plan on its base plane: Points | Edges
selection, drag to move (snapped, validated by `wall_run::validate`), hold on a segment
to insert a point, Delete (a point merges its segments; an edge merges its points into
the first), Extend from an end, Closed / Open, and the run's thickness, side, and height
mode. Each edit is one `wall_run::ops` call and one `UpdateWallRun`.

**Openings mode** (the Openings tool, with its Window | Door choice): tap any wall, in
plan or 3D, to place the preset (at first a window 1.2 × 1.2 m at a 0.9 m sill and a door
0.9 × 2.1 m, cut through the bottom edge; then the last size chosen per kind) centred on the tap along that segment; tap an opening to
select it; drag it along the segment (a window also up and down) on a 0.1 m grid,
inside the segment's clear span (clear of the corner joins); set its width, height,
sill, and Through or niche depth; Delete. "Wall" faces the segment worked on. Each
change is one structured-opening operation and one `UpdateWallRun`.

**Earlier walls** convert when worked on (the pencil, or an opening placed in them),
inside the session so ✗ undoes it: legacy extrusion walls on the level become `Wall`s,
then the connected chain of `Wall`s (end to end, or butt joined) becomes ONE run —
corner to corner, on the first wall's height mode, its rectangular voids openings in
place (`wall_run::from_walls`); a wall of another height keeps its shape as its
segment's profile. They still display, select, and delete before that.

**The item just created is the selection.** A face drawn in a floor's Edit Mode (solid
or void), an opening just placed, and a wall run just drawn are selected, so the panel's
values change them at once (the panel says "New void · depth", "New window · Wall 1",
"Changes apply to Wall 1 and the next walls"). Those values are also the defaults for
the next item. A wall run stays "fresh" until the next point or another tool; on phones,
while a face tool stays armed, the drawing bar carries the new face's stepper.

**Remembered settings** (session state, saved with the page's session): floor
thickness; void depth and Through; wall thickness, height, side, height mode, top plane,
and offset; window and door width / height / sill / niche per kind, and the niche depth;
the outline shape per tool (floor, wall, room); the room wall thickness. New walls start
at 0.114 m: a 2x4 stud (89 mm) with 12.7 mm gypsum board on both faces
(`walls::PARTITION_THICKNESS_M`); the wall thickness steps by 5 mm.

**Copy / Paste** (the pill under the view pill on phones, above it on desktop; Ctrl/Cmd+C
and V). A copy belongs to the mode it was taken in: faces (a floor's Edit Mode), the
selected opening (Openings mode), or a whole floor plate or wall run (Select). Paste ARMS
a placement: a preview follows the pointer, each tap places one copy (one undo step), and
it stays armed until Esc, the Paste button, another tool, or leaving the mode. Faces and
elements land with their bounding-box centre at the tap, moved by whole snap steps; an
element copy is named by the numbering rule ("Floor plate 2", "Kitchen 2"). Openings land
on the tapped wall segment centred at the tap, each kept inside the clear span (refused
with a toast when it does not fit).

**Dock.** Select, Floor, Hole, Wall, Openings (and Rooms with `?rooms`). Snap moved to the
view pill: it is an input aid for every mode, and the pill is also visible inside the
Edit Modes, where the dock is replaced.

**Rooms (preview, `?rooms`).** A temporary app-side adapter (`authoring::rooms`) follows
the library contract of §12 until the app switches to it: the Rooms tool (rectangle or
polygon; "Room 001", then the next number), plan overlays (region fill, name + area
label, the wall network as bands, a hidden wall dashed), a Rooms group per level in the
Model tree (select, Bring forward / Send backward, the pencil), the room page (rename,
order, the plane's room wall thickness and height, delete), Room Edit Mode (points and
edges; "Hidden wall" on the selected edges), and openings in room walls (anchored to the
covering room edge). Room changes join the one Undo / Redo. The preview draws no wall
solids and keeps the rooms beside the session, not in the document.

**Attachment.** Sketches and wall runs live on their construction plane, in the space
of its root level, so a level elevation edit moves everything on it (and on its
workplanes) as a transform only — except runs whose top follows another plane, which
re-mesh.

**Known limitations.**
- Separately drawn runs are not joined to each other (a run's own joins are exact).
- Custom elevation profiles of a run's segments (gables) are kept but not yet editable
  in the app; converting a wall chain drops no shape, but a changed profile is not
  re-fitted to new joins.

## 9. Edit Mode data model: the `Sketch` entity

Edit Mode edits the 2D **profile** of an element: move, insert, and delete points,
edges, and faces; split a face with a line; several disjoint faces in one element;
subtractive "hole" faces that may reach outside the material; and a thickness **per
face**. The profile is one entity, a `Sketch`, stored as plain data in its params:

```rust
pub struct Sketch { pub points: Vec<SketchPoint>, pub faces: Vec<SketchFace> }
pub struct SketchPoint { pub id: u32, pub uv: [f64; 2] }
pub struct SketchFace { pub id: u32, pub points: Vec<u32>, pub kind: SketchFaceKind }
pub enum SketchFaceKind { Solid { thickness: f64 }, Void { depth: Option<f64> } }
pub enum SketchDirection { Below, Above }
// Params::Sketch { sketch: Sketch, direction: SketchDirection }; slot 0 = plane (Level)
```

- **Coordinates** are `(u, v)` meters in the plane's frame. The plane is a Level today
  (a future face frame fits the same slot).
- **Ids** of points and faces are local to the sketch and stable across edits: a
  selection or a provenance name keeps pointing at the same thing. New ids are the
  largest existing id plus one; an id is never reused within the sketch.
- **Faces** are closed loops of point ids. **Edges** are derived: consecutive loop
  points (last to first included). Two faces share an edge when both loops contain the
  same unordered point pair, so moving a shared point or edge moves both faces
  (planar-graph semantics).
- **Face kinds.** `Solid { thickness }` is material from the plane to `thickness`
  meters away from it. `Void { depth }` removes material from the plane to `depth`
  meters away (`None`: through everything). A void deeper than the material cuts
  through; a void shallower than it makes a **pocket** (an indentation). A void may
  extend outside the material (it only removes what it overlaps) or lie fully outside
  (no effect). Where a void and a solid overlap within the void's depth, the void wins.
- **Direction.** `Below` hangs the material under the plane (a floor plate: its top is
  at the level elevation); `Above` stands it on the plane (for later wall and roof use).
  The direction is set at creation; `UpdateSketch` keeps it.
- **One edit = one command.** The app computes the edited sketch with the library's
  topology operations (`vim_design_lib::sketch::ops`) and stores it with one
  `UpdateSketch` — one undo step; coalesced updates merge a drag into one step.
- **Validity, two tiers.** The commands reject a structurally invalid sketch
  (`InvalidSketch`: duplicate ids, a loop over a missing point, fewer than three
  distinct points or an immediately repeated point, a non-finite coordinate, a
  thickness or depth that is not finite and positive). Geometry the kernel cannot use
  — a loop that crosses or touches itself, zero area — is accepted and reported as a
  per-entity evaluation error (the previous mesh stays). `sketch::validate` and
  `validate_face` give the same answers live, for UI feedback.

**Layered evaluation.** At each point of the plane, the solid interval is
`[0, thickest solid face covering it]` and the removed interval is
`[0, deepest void covering it]`. Every distinct thickness and depth is a layer
boundary; for the layer between boundaries `a < b`:

```text
footprint = union(solid faces with thickness >= b) - union(void faces with depth >= b or None)
```

Each polygon of the footprint (with its holes) becomes a prism from depth `a` to `b`.
A polygon that repeats unchanged in the next layer extends its prism instead, so a
plain plate or a plate with through holes is a single prism. The element's geometry is
the set of prisms; stacked prisms (pockets, faces of different thickness) touch along
internal faces. The 2D booleans run in `i_overlay` (robust polygon booleans in pure
Rust, 64-bit integer engine); slivers under the tolerance area are dropped. A sketch
with no material left has no mesh (not an error).

**Provenance.** Generated faces are named from the stable sketch ids: a side face by
the sketch face and point pair of the boundary edge that sweeps it
(`SketchSide { face, a, b }`, `a < b`), a cap by its depth in micrometers and whether
it faces the plane (`SketchCap { depth_um, toward_plane }`).

**Topology operations** (`sketch::ops`, pure functions, typed `SketchError`, never
panic; every result is structurally valid and has no unused points):

| Operation | Semantics |
|---|---|
| `add_face(sketch, uv_loop, kind)` | New face (next face id) from a loop; duplicates dropped, loop normalized counter-clockwise; a vertex within 1 µm of an existing point reuses it (corner-to-corner faces share edges). A vertex on the middle of an existing edge is not inserted into it. |
| `move_points` / `set_point` / `move_edges` / `move_faces` | Translate the union of the addressed points once; faces sharing a moved point follow. |
| `insert_point_on_edge(sketch, a, b, t)` | New point (next point id) at `t ∈ (0, 1)` from `a`, inserted into **every** loop that has edge `a`-`b` in either direction. |
| `split_faces(sketch, start, end)` | Every stretch of the segment that runs through a face's interior (an entry crossing then an exit crossing) splits that face: the crossing points are inserted into all loops sharing those boundary edges, and the face becomes two faces sharing the new edge, with the same kind. The face keeps its id for the part **left** of the drawn direction; the right part gets a new id. A non-convex face entered and left several times is split at **every** stretch. Segment parts outside faces, and stretches along a boundary, are ignored; `NothingToSplit` if no face was split. |
| `delete_faces(sketch, ids)` | Remove the faces, then points no face uses. |
| `delete_edges(sketch, pairs)` | Merge each edge's two points into the edge's **first** point in loop order, read in the lowest-id face that has the edge; the kept point keeps its position. Loops drop immediate repeats; faces with fewer than three points are removed, then unused points. |
| `delete_points(sketch, ids)` | Remove the points; each loop reconnects the neighbours; faces with fewer than three points are removed, then unused points. |
| `validate(sketch)` / `validate_face(sketch, id)` | Structural checks plus simple-polygon checks (crossing, touching, repeated point, zero area). |
| `edges`, `face_polygon`, `next_point_id`, `next_face_id` | Derived helpers (unique undirected edges with their faces; a face's (u, v) polygon; the next ids). |

**Compatibility.** `Sketch` was added by appending variants at the end of the
serialized enums, so documents saved before it load and resave byte-identically (a
saved authoring project is kept as a regression fixture).

## 10. Walls and workplanes data model

### Workplanes

A **`Workplane`** is a construction plane nested under a level or another workplane —
a ceiling plane, a sill plane, a parapet base. Levels stay the stories; workplanes are
the non-story planes the user nests inside a story.

```rust
// Params::Workplane { name: String, offset_m: f64, color: [f32; 4], extent_m: f64 }
// slot 0 = parent (required): Level | Workplane
```

- It evaluates to the parent's frame moved `offset_m` along the parent's normal (world
  axes, so the basis is trivial). Moving the parent moves the workplane and everything
  on it. The graph's cycle check keeps parent chains acyclic.
- Every construction-plane slot accepts it: control point `plane`, sketch `plane`,
  wall `base` and `top`.
- **Association stays level-only**: an element drawn on a workplane is associated with
  the workplane's **root level** (`workplane::root_level(doc, plane)`).
- `workplane::plane_elevation(doc, plane)` is a plane's height above the scene origin
  from params only (the root elevation plus the chain's offsets, added in the same
  order as evaluation, so it matches the evaluated frame exactly).
- Deleting a level that has workplanes is rejected (dependents); the cascade form
  deletes the workplanes and everything on them, in one undo step. `DeleteWorkplane`
  itself is the plain reject-if-dependents delete.
- Translation factoring: geometry on a workplane lives in its **root level's** local
  space with the workplane offset baked into local z. Dragging the root level is a
  transform-only update; editing the workplane offset re-evaluates what is on it.

Commands: `CreateWorkplane { parent, name, offset_m, color, extent_m }`,
`UpdateWorkplane { id, parent, name, offset_m, color, extent_m (all Option), coalesce }`,
`DeleteWorkplane { id }`. A non-finite offset is rejected (`InvalidCommand`).

### Walls

A **`Wall`** is a reference line on a construction plane plus an editable elevation
profile — the same Edit Mode model as floor plates, turned upright.

```rust
// Params::Wall {
//     start: [f64; 2], end: [f64; 2],  // reference line in the base plane's (u, v)
//     height_m: f64,                   // the top reference when `top` is unwired
//     top_offset_m: f64,               // added to the top plane when `top` is wired
//     profile: Sketch,                 // elevation: u along the wall from start, v up
//     top_points: Vec<u32>,            // profile points whose v is measured from the top
// }
// slot 0 = base (required): Level | Workplane;  slot 1 = top (optional): Level | Workplane
```

- **Frame.** The profile's u runs along `start -> end` from `start`, v runs up from the
  base plane (its normal). Material grows to the **left** of `start -> end` (the drawn
  line is one face of the wall, as in the wall tool).
- **Top reference H.** With `top` unwired, `H = height_m`. With `top` wired,
  `H = (top plane height - base plane height) + top_offset_m` — the wall height follows
  the top plane (connect a wall to the story above, or to a ceiling workplane).
  `wall::wall_top_height(doc, wall)` gives H from params, exactly as evaluation
  computes it.
- **Top anchors.** A point in `top_points` stores its v relative to H; other points
  store v relative to the base. The **effective** profile (anchored v + H) is what
  evaluates and what the user sees and edits. Changing H moves the anchored points and
  nothing else: the top edge follows, windows keep their sill height.
- **Faces.** Solid faces are wall material, each with its own thickness (a thicker
  pilaster face is legal). Void faces are openings: `depth: None` goes through (a
  window); a depth under the thickness is a niche. A **door** is a void that crosses
  the bottom edge — ordinary subtractive shaping, no special case. Evaluation is the
  layered sketch algorithm (§9) in the wall's vertical frame; provenance is
  `SketchSide`/`SketchCap` with the wall as owner. A wall is an `Element` member (or a
  standalone mesh owner); the orphan sweep and the level cascade collect it (the base
  and the top plane are both dependencies, so either one's cascade takes the wall).
- **Validity.** The commands reject (`InvalidWall`) a zero-length or non-finite line, a
  height that is not finite and positive, a non-finite top offset, a structurally
  invalid profile, or an anchored id that is not a profile point
  (`wall::validate_structure` names the problem). H at or below zero and a
  self-crossing effective profile are per-entity evaluation errors; the previous mesh
  stays.
- **Factoring.** A wall with only a base on root level l lives in l's local space:
  dragging l is transform-only. A wall with a top constraint depends on two planes and
  is evaluated in world space (a drag of either plane re-evaluates it).

Commands: `CreateWall { base, top: Option, start, end, height_m, top_offset_m, profile,
top_points }`, `UpdateWall { id, base, top: Option<Option>, start, end, height_m,
top_offset_m, profile, top_points (all Option), coalesce }` (the merged result is
validated like a create), `DeleteWall { id }`.

**Default profile.** `wall::default_profile(length, thickness) -> (Sketch, Vec<u32>)`:
points 0 `(0, 0)`, 1 `(length, 0)`, 2 `(length, 0*)`, 3 `(0, 0*)` where `*` marks the
top-anchored corners (v = 0 from the top), one solid face (id 0) with `thickness`, and
anchors `[2, 3]`. The default wall is exactly H high in both height modes.

**Editing** (`wall::ops`, pure, typed `SketchError`): the sketch operations of §9 —
`add_face`, `move_points`, `set_point`, `move_edges`, `move_faces`,
`insert_point_on_edge`, `split_faces`, `delete_faces`, `delete_edges`, `delete_points`
— each taking `(profile, top_points, H, ...)` in **effective** coordinates and
returning the stored `(profile, top_points)` for one `UpdateWall`. Anchor rules: a
surviving point keeps its anchor (moved or not); a removed point loses it (a merged
edge keeps the kept point's anchor); a point an operation creates is anchored exactly
when it lies on an edge between two anchored points (a point inserted on the top edge
follows the top). `set_anchor(profile, top_points, H, ids, top)` re-anchors points
without moving them. A point whose effective position an operation does not change
keeps its stored coordinates bit for bit. `wall::effective_profile(profile, top_points,
H)` and `wall::stored_profile(effective, top_points, H)` convert between the two forms.

**Compatibility.** Workplanes and walls were added by appending variants at the end
of the serialized enums; documents saved before (including Sketch floor plates) load
and resave byte-identically (kept as regression fixtures).

## 11. Wall runs

A **`WallRun`** is a wall drawn as a thickened polyline: one entity for a whole chain of
wall segments, with exact joins at any angle. It is the wall the wall tool draws; the
single-segment `Wall` of §10 stays and evaluates unchanged.

```rust
// Params::WallRun {
//     points: Vec<RunPoint>,           // RunPoint { id: u32, uv: [f64; 2] } on the base plane
//     closed: bool,                    // a segment from the last point back to the first
//     thickness_m: f64,                // material to the LEFT of the direction of travel
//     height_m: f64,                   // the top reference when `top` is unwired
//     top_offset_m: f64,               // added to the top plane when `top` is wired
//     openings: Vec<Opening>,
//     profiles: Vec<SegmentProfile>,
// }
// Opening { id, segment, offset_m, sill_m, width_m, height_m, kind: Window | Door,
//           depth_m: Option<f64> }
// SegmentProfile { segment, profile: Sketch, top_points: Vec<u32> }
// slot 0 = base (required): Level | Workplane;  slot 1 = top (optional): Level | Workplane
```

- **Segments.** Segment k runs from point k to point k + 1 (and, when closed, from the
  last point to the first). A segment is named by its **start point id**; openings and
  profiles refer to segments by that id. Point and opening ids are run-local and stable.
- **Top reference H** as for walls: `height_m`, or the top plane's height above the base
  plus `top_offset_m` (`wall_run::run_top_height(doc, run)` from params).
- **Joins.** The footprint is the reference polyline offset by the thickness with miter
  joins: two segments meet exactly on the bisector through their shared point, at any
  angle. On the outside of a turn whose miter would reach more than
  `MITER_LIMIT` (4) thicknesses from the corner, the corner is beveled (the chord between
  the two offset-line ends, split at its midpoint). Open ends are square. A run that
  folds back on itself is an error.
- **Join zones and the clear span.** Near each join a segment is a wedge. Along the
  reference line, the wedge reaches from the segment end to the far end of the join cut.
  `wall_run::segment_clear_span(run, segment) -> (min, max)` is the span between the two
  zones: openings must lie in it. Within a join zone a custom profile must be a band from
  the base to one straight top edge (a gable slope may run through the zone; an apex may
  not).
- **Openings.** A window is the rectangle `offset_m .. offset_m + width_m` along the
  reference line from the segment start, `sill_m .. sill_m + height_m` up from the base.
  A door starts at the base (its sill is ignored) and its void extends below the base, so
  it always cuts the bottom edge. `depth_m: None` goes through; `Some(d)` is a niche `d`
  deep from the reference face. Openings in one segment must not overlap.
- **Segment profiles.** A `Sketch` in the segment's elevation (u along the reference line
  from the segment start, v up), with `top_points` measured from H as for walls. Its
  solid faces are wall material (their thickness is ignored: the run's thickness
  applies); its void faces are extra openings. A segment without a profile is the plain
  rectangle up to H.
- **Faces.** `RunFace { segment, part }`: `Reference` (on the reference line),
  `Opposite`, `Top`, `Bottom`, `Start`, `End` (a square end or the join face),
  `Opening { opening }` (the reveals), `NicheBack { depth_um }`, `ProfileVoid { face }`.
- **Evaluation.** Per segment: the two join wedges are planar polyhedra (the footprint
  clipped at the clear span, under the band's top plane), and the middle is the segment
  profile over the clear span with its openings, built as layered prisms (§9) in the
  segment's vertical frame. The direct planar mesher removes the faces where pieces and
  segments touch, so a straight run is 12 triangles, a 90-degree corner 20, a closed
  rectangle 32. Evaluation takes about 0.2 ms per segment (0.4 ms with three openings)
  in a release build.
- **Validity.** The commands reject (`InvalidWallRun`) too few points (two; three when
  closed), duplicate ids, non-finite values, a thickness or height that is not positive,
  an opening on an unknown segment or with an invalid size, and an invalid segment
  profile (`wall_run::validate_structure`). `wall_run::validate` also checks the
  geometry: no zero-length segment, no self-crossing reference line, no fold-back join,
  a clear span on every segment, openings inside their clear span and not overlapping,
  no overlapping material (parallel segments closer than the thickness). A run that fails
  it still commits; its evaluation is a per-entity error and the previous mesh stays.
- **Factoring and cascades** as for walls: a base-only run on a root level (or on a
  workplane under it) is level-local, so a drag of that level is transform-only; a run
  with a top is evaluated in world space. It is an `Element` member (or a standalone
  mesh owner) and the orphan sweep collects it. Deleting its base plane deletes it;
  deleting only its top plane (`DeleteLevel { cascade: true }`,
  `DeleteWorkplaneCascade`) **disconnects** it: `height_m` becomes its current H (at
  least `MIN_DISCONNECTED_WALL_HEIGHT_M`) and the top slot is emptied, in the same undo
  step. The same rule applies to `Wall`.

Commands: `CreateWallRun { base, top: Option, points, closed, thickness_m, height_m,
top_offset_m, openings, profiles }`, `UpdateWallRun { id, base, top: Option<Option>,
points, closed, thickness_m, height_m, top_offset_m, openings, profiles (all Option),
coalesce }` (the merged result is validated like a create), `DeleteWallRun { id }`.

**Editing** (`wall_run::ops`, pure functions of `WallRunData`, typed `WallRunError`;
each result is checked with `validate`, and a value an operation does not change is
copied bit for bit):

| Operation | Effect |
|---|---|
| `move_points(run, ids, delta)`, `set_point(run, id, uv)` | move points; openings keep their offsets |
| `move_edges(run, segments, delta)` | move both end points of each segment |
| `insert_point(run, segment, offset_m, H) -> (run, id)` | split a segment; openings go to the part they lie in (an opening across the point is an error); a custom profile splits at the point |
| `delete_points(run, ids)` | the neighbouring segments merge; each opening keeps its plan position on the merged segment, or the edit is an error; merged segments lose their custom profile |
| `delete_edges(run, segments)` | each segment's end point merges into its FIRST point; the segment's openings and profile go; the next segment's openings keep their plan position or the edit is an error |
| `extend(run, Start \| End, uv) -> (run, id)` | add a point at one end of an open run |
| `set_closed(run, closed)` | close, or open (the closing segment, with its openings and profile, goes) |
| `add_opening(run, opening) -> (run, id)` | the opening gets the next free id |
| `move_opening(run, id, [along, up])` | clamped to the clear span; a window's sill clamps at the base, a door stays on it |
| `set_opening(run, opening)`, `delete_opening(run, id)` | replace or remove by id |

`wall_run::from_walls(doc, &[wall]) -> Result<(WallRunData, base, top), FromWallsError>`
converts a chain of `Wall`s (each starting where the previous one ends, one base and
top, one thickness) into a run: rectangular voids become openings (a void that reaches
the base is a door), and a wall whose remaining profile is not the plain rectangle keeps
it as a segment profile. `WallRunData::into_params` gives the `Params`.

**Compatibility.** `WallRun`, `RunFace`, `DeleteWorkplaneCascade`, and the wall-run
commands were appended at the end of their serialized enums; the saved fixtures of the
earlier milestones (v1, v2, v3) load, evaluate, and resave byte-identically.

## 12. Rooms and room layouts

A **`Room`** is a named area on a construction plane. Rooms are data: the walls come
from the plane's **`RoomLayout`**, which owns the arrangement of its rooms and generates
ONE wall network from it. A boundary that two rooms share is one wall; T and X junctions
are exact.

```rust
// Params::Room {
//     name: String,               // default "Room 001", "Room 002", ... (room::default_name)
//     precedence: i32,            // a higher room cuts into a lower one
//     boundary: Vec<RunPoint>,    // closed, counter-clockwise; interior on the left of each edge
//     hidden_edges: Vec<u32>,     // edges (start point ids) that generate no wall
// }
// slot 0 = plane (required): Level | Workplane
//
// Params::RoomLayout {
//     thickness_m: f64,           // DEFAULT_PARTITION_THICKNESS_M = 0.114 for new layouts
//     height_m: f64,              // the top reference when `top` is unwired
//     top_offset_m: f64,          // added to the top plane when `top` is wired
//     openings: Vec<RoomOpening>,
// }
// RoomOpening { id, room: EntityId, edge: u32, offset_m, sill_m, width_m, height_m,
//               kind: Window | Door, depth_m: Option<f64> }
// slot 0 = plane (required); slot 1 = top (optional); slot 2 = rooms (multi: Room)
```

- **Default thickness.** `DEFAULT_PARTITION_THICKNESS_M` (0.114 m, in `wall` and
  re-exported by `room_layout`) is a stud-and-gypsum partition: a 2x4 stud (89 mm actual)
  with one 12.7 mm gypsum board on each face. The app uses it for new walls and layouts.
- **Effective regions.** Rooms are ranked by precedence (higher first; ties by entity
  id). A room's effective region is its boundary minus the boundaries of all rooms ranked
  above it, so regions never overlap: where rooms overlap, the higher room's shape cuts
  into the lower one. A region can be `Whole`, in `Pieces(n)`, `Empty` (entirely under
  higher rooms), or `Invalid` (the room fails `room::validate`: a self-crossing, zero-area,
  or clockwise boundary). `Engine::room_regions(layout)` gives every room's region
  (polygons with holes, area, status) for overlays and labels.
- **Wall graph.** The edges of all effective regions, split at every vertex on them and
  deduplicated. Each segment knows the room edges that cover it (collinear overlap). A
  segment is hidden (no wall) when ANY covering room edge is hidden: hiding the shared
  edge in either room removes the wall, and both rooms stay distinct rooms (a dining
  nook open to the kitchen). A segment where a higher room cuts into a lower one is
  covered by the higher room's edge.
- **Footprint.** Walls are centered on the visible segments, `thickness_m` wide: one
  rectangle per segment, square at both ends, plus a kite at every outside corner (a gap
  wider than a half turn between two consecutive walls at a node) up to the miter point
  of the two wall faces, or a bevel when that point is farther than `MITER_LIMIT` (4) half
  thicknesses from the node. Inside corners, straight runs, T and X junctions need no
  filler: the rectangles meet exactly, at any angle. A wall end with no other wall at its
  node (next to a hidden edge) is square. The pieces are united with `i_overlay`.
  Limits: at a sharp inside corner the two wall faces meet `h / sin(gap / 2)` from the
  node; walls shorter than that keep their square ends there (the footprint is still the
  union of the two walls, only the inside corner stays open).
- **Openings** are anchored to a room edge (`room`, `edge`, `offset_m` from the edge start
  to the opening's left edge). `room_layout::opening_span(input, room, edge)` gives the
  spans of the edge where an opening can go: where visible wall covers the edge, clear of
  the other walls at its junctions (`h (1 + |cos a|) / sin a` for a wall at angle `a`, half
  the thickness at a right angle). A window is `sill_m .. sill_m + height_m`; a door starts
  0.1 m below the plane and cuts the whole wall bottom. `depth_m: None` goes through;
  `Some(d)` is a niche `d` deep from the face on the ROOM's side (the interior, left of
  the room edge). An opening on a missing edge or outside every span is left out, the
  layout reports it (`LayoutIssue::OpeningDoesNotFit`), and the rest evaluates.
- **Height layers.** Heights 0, H, and every opening's bottom and top (clamped to the
  wall) split the wall into layers; each layer is the footprint minus the openings active
  in it, one prism per plan piece. The direct planar mesher removes the faces between
  layers. A 6-room layout with 10 openings evaluates in about 1.3 ms and meshes in about
  2.2 ms (release).
- **Faces.** `RoomWall { room, edge, part }`: `Inside` is the face toward that room edge's
  room (a shared wall has an `Inside` face for each room), `Outside` a face toward no room
  (named by the first covering room edge in rank order), `End` a wall end or bevel,
  `Opening { opening }` the reveals, `NicheBack { opening }`. Horizontal faces are
  `LayoutCap { z_um, up }` (wall tops, bottoms, sills, heads).
- **Per-entity state.** An invalid room or a misfit opening makes the layout a per-entity
  error WITH a current value (`Engine::room_layout_issues`); the mesh shows everything
  else. A non-positive H is an ordinary evaluation error (the previous mesh stays).
- **Factoring and cascades** as for walls: a layout without a top on a root level (or a
  workplane under it) is level-local, so dragging that level is transform-only; with a
  top it is evaluated in world space. Deleting the plane deletes the layout and its
  rooms; deleting only the top plane disconnects the layout (`height_m` becomes its
  current H). Rooms and layouts are `Element` members (a room adds no geometry) and the
  orphan sweep collects them.

Commands:

| Command | Effect |
|---|---|
| `CreateRoom { plane, name, precedence, boundary, hidden_edges, layout: Option }` | with `layout`, also adds the room to it (one undo step); `InvalidRoom` when structurally invalid |
| `UpdateRoom { id, plane, name, precedence, boundary, hidden_edges (all Option), coalesce }` | openings on an edge the edit changes keep their plan position on the edge that now contains them (same step) |
| `DeleteRoom { id }` | leaves its layout, the layout's openings on its edges go, then the room (one step; rejected while an element holds it) |
| `CreateRoomLayout { plane, top, rooms, thickness_m, height_m, top_offset_m, openings }` | `InvalidRoomLayout` for a bad size, a duplicate or invalid opening, an opening on a room outside the layout, or a room on another plane or already in a layout |
| `UpdateRoomLayout { id, plane, top: Option<Option>, rooms, thickness_m, height_m, top_offset_m, openings (all Option), coalesce }` | validated like a create |
| `DeleteRoomLayout { id }` | the rooms stay |

**Editing** (pure, typed errors):

- `room::from_polygon(name, precedence, points)` and `room::from_rectangle(name,
  precedence, a, b)` make a valid counter-clockwise room (ids 0, 1, 2, ...);
  `room::default_name(doc)` gives the next "Room NNN".
- `room::ops` (`RoomError`, each result checked with `room::validate`): `move_points`,
  `set_point`, `move_edges`, `insert_point(room, edge, offset_m) -> (room, id)` (the new
  edge keeps the split edge's hidden flag), `delete_points` (neighbouring edges merge),
  `delete_edges` (the end point merges into the FIRST point), `set_hidden(room, edges,
  hidden)`.
- `room_layout::ops` on a `LayoutInput` (`room_layout::inputs(doc, layout)`):
  `add_room(rooms, room)`, `remove_room(input, room) -> (rooms, data)` (with its
  openings), `ranking`, `bring_forward` / `send_backward` (the precedence that moves a
  room one place, or `None` at the end), `bring_to_front` / `send_to_back`,
  `add_opening -> (data, id)`, `move_opening(input, id, [along, up])` (clamped into the
  nearest span; a window's sill at the base, a door stays on it), `set_opening`,
  `delete_opening`. Opening operations check the layout's structure and that the touched
  opening fits.
- `room_layout::validate(input)`: every room valid and every opening fitting.

**Compatibility.** `Room`, `RoomLayout`, `RoomWall`, `LayoutCap`, and the room commands
were appended at the end of their serialized enums; fixture v4 (wall runs with openings
and a gable, sketch plates, workplanes, `Wall`s) was saved before this change and, with
v1 to v3, loads, evaluates, and resaves byte-identically.

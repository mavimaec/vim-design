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

**Walls** are the library's `Wall` entity (§10). The drawn line is one face of the wall;
the thickness grows to the left of the drawing direction (to the right with "flip
side": the stored reference line is then reversed, since a `Wall` always has its
material on the left). A closed loop is normalized counter-clockwise first, so an
unflipped loop grows inward. Butt joins at corners: at a convex corner (turning toward
the thickness side) the next segment's start is trimmed by the thickness; at a reflex
corner the previous segment's end is extended by it. Each segment becomes one `Wall` on
the active plane with the default profile (a rectangle whose top corners are anchored to
the top), owned by one element.

**Wall height.** The wall tool bar and the wall properties offer **Fixed** (a height) or
**Up to** a plane (any level or workplane, plus an offset): the wall's top slot is wired
to that plane, so the height follows it — drag the level above and the walls re-mesh to
it, while their windows keep their sill height. A top that would not clear the base (or
the wall's openings) is refused. Switching back to Fixed keeps the current height. Walls
with a top plane are evaluated in world space; walls with only a base plane move by
transform only when their level is dragged.

**Wall Edit Mode.** The pencil on a wall opens its elevation (orthographic, facing the
wall: `u` along it from its start, `v` up from its base) with the same Edit Mode as floor
plates (§9 operations through `wall::ops`, one `UpdateWall` per edit, ✓ one undo step,
✗ byte-identical), plus two quick tools: **Window** (1.2 × 1.2 m at a 0.9 m sill) and
**Door** (0.9 × 2.1 m, reaching 5 cm below the base so it cuts the bottom edge) —
preset voids placed where tapped, then edited like any face. A void with a depth under
the thickness is a niche. **Anchor Bottom | Top** re-anchors the selected points: a
top-anchored point (square handle) follows the wall height, a bottom-anchored one
(round) keeps its height above the base. A point inserted on the top edge follows the
top, so a gable is: hold on the top edge, drag the new point up. The Window tool in the
main dock (with its Window | Door choice) opens the tapped wall's Edit Mode with the
preset armed, and comes back after ✓ for the next wall. The properties list a wall's
openings (window / door / niche) with delete.

**Legacy walls** (extrusion + window hole wires, from before the `Wall` entity) still
display, select, and delete; their height and thickness stay editable. The pencil — or
the Window tool — converts one in place inside the Edit Mode session (same element,
name, and level; its base line becomes the reference line, its level the base, a fixed
height, the profile a solid face of its thickness with the top corners anchored, each
window a through void; the old construction chain is deleted), so ✗ undoes the
conversion too.

**Attachment.** Sketches and walls live on their construction plane, in the space of its
root level, so a level elevation edit moves everything on it (and on its workplanes) as
a transform only — except walls whose top follows another plane, which re-mesh.

**Known limitations.**
- Butt joins are exact only at 90°; at other angles the corner blocks overlap or leave
  a sliver. Joins are computed when a run is drawn: separately drawn runs are not
  joined, and a later thickness edit does not re-trim neighbours.
- Walls are not re-joined after edits: moving or re-heightening one does not re-trim its
  neighbours, and a gable does not trim the walls it meets.

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

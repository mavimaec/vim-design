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
  *Library precision (Phase A, 2026-08-23)*: the cascade deletes exactly the level's
  **transitive dependent closure** — attached control points and everything downstream
  of them; associated elements and their instances. *Inputs* of cascaded entities that
  are not themselves dependents of the level survive: an element that is merely
  associated (not attached) loses its element wrapper and instances, but its member
  geometry remains (and its solid producer resurfaces as a standalone mesh). Fully
  removing such an element's geometry means attaching it (or deleting it explicitly);
  the UI prompt should say which will happen.

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
- **Association** (data): `Element` gains an optional `level` slot — "this wall belongs
  to Level 2" for organization, filtering, schedules, and the deletion cascade. It has
  **no geometric effect** (*library precision*: the rewire dirties the element
  parametrically — the pump reports it, and the mesh is re-delivered byte-identical;
  the element evaluator ignores the slot. Association commands are the additive
  `UpdateElementLevel { element, level: Option }` — `Create/UpdateElement` keep their
  frozen shapes for existing callers, and the app sets the active-level association
  right after create); an element associated with a level whose geometry is
  attached to a different plane is legal (useful for e.g. a roof associated with the
  top story but modeled from a sloped plane).
- Every new element is associated with the **active level** at creation time.

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

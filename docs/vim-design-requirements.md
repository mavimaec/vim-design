# vim-design: Architecture Requirements

Source: design discussion between Martin (CTO, VIM) and Claude, November 2026.
Audience: the vim-design implementation agent.

Revision 2 (2026-11-13): massing is now reference-only geometry (R3.11 to R3.14, replacing the earlier stage 1 envelope input); a new explicit envelope stage (R3.15 to R3.21); pinning is a document-level action with defined partition semantics (R5.2, R5.14 to R5.19); later section 5 requirements renumbered (R5.20 to R5.22). The default stage list (R3.5) is updated accordingly.

## How to read this document

Every requirement carries a status tag:

**[Decided]** Martin explicitly stated or agreed to it. Implement as written.

**[Proposed]** Recommended during the discussion and consistent with the decisions, but not explicitly confirmed. Implement as the default, and flag any deviation or difficulty.

**[Open]** Unresolved. Do not lock in; build so the choice can change, and raise the question when it blocks work.

Requirements are numbered so commits, tests, and questions can reference them (for example, "implements R6.4").

---

## 1. Product scope

**R1.1 [Decided]** vim-design is a BIM authoring tool (not only a viewer) whose geometry kernel is written in Rust and can be relied upon for production work.

**R1.2 [Decided]** The geometric domain must cover simple BIM elements, complex MEP elements, and designer-modeled curved walls. No subsystem may assume planar-only or right-angle-only geometry.

**R1.3 [Decided]** The core workflow composes buildings from reusable, parametric assets (room layouts, systems, units), derives walls, slabs, ceilings, and eventually MEP routing procedurally, and feeds a live material price sheet so designers get immediate cost feedback.

**R1.4 [Decided]** Exploring layout permutations (for example, alternative single-family or residential configurations) is a target capability, built on the same evaluation machinery.

---

## 2. Core data model

**R2.1 [Decided]** There are two layers of data. The authored layer holds design intent: graph topology, parameters, expressions, overrides, library pins. The derived layer holds everything the evaluation graph computes. The derived layer is a cache and must be fully reproducible from the authored layer.

**R2.2 [Decided]** Commands may target derived outputs by stable ID (for example, one window emitted by a facade system). Such a command writes an authored override record keyed to that stable ID. It never mutates cached derived state, because re-evaluation would erase it. Override records are reapplied on every evaluation.

**R2.3 [Decided]** Every derived element has a stable ID derived from its position in the evaluation (call path), never from geometry or from sequence numbers. Proposed derivation: ID = hash(stage, parent ID, semantic key), where the semantic key is geometry-independent (layout slot, template slot, provenance pair).

**R2.4 [Proposed]** If an override's target ID no longer exists after evaluation, the override is orphaned and surfaced as a diagnostic. Orphaned overrides are never silently dropped.

**R2.5 [Decided]** Design options are variants: override layers on a shared subgraph, with an active flag, not copies of the subgraph. Copies are reserved for options whose topology differs substantially. Rationale: fixes to the base design must flow into every option.

**R2.6 [Proposed]** Provide explicit conversion commands: detach (template instance to standalone), promote (standalone to template or library asset), and bake (a generated element plus its patches converted into an explicit declaration).

**R2.7 [Open]** Asset taxonomy. The starting concept is AssetGroup (container), AssetSystem (procedural, parametric, with named outputs), and AssetUnit (countable leaf). Discussion noted that procedural versus static and composite versus leaf are orthogonal: a procedural generator can emit units or groups. Proposed direction: every asset is a definition with parameters, optionally backed by a generator; leaf status is a property of the evaluated output, not of the authoring mode.

---

## 3. Evaluation graph (staged DAG)

**R3.1 [Decided]** The design is evaluated as a directed acyclic graph of pure functions. Shared definitions (shared parameters, materials, families and types, asset libraries) sit upstream and are referenced by stable ID, never copied downstream.

**R3.2 [Decided]** The DAG topology is user-definable (by users or AI-assisted users), starting from a default template. Multiple upstream sequences (for example, per-discipline partitions) may exist.

**R3.3 [Decided]** What is fixed is not the topology but the type contracts between nodes. Footprints, boundary graphs, assembly assignments, wall axes, and so on are well-defined types; any topology that type-checks is valid.

**R3.4 [Decided]** Separate sequences must converge at the earliest stage where their elements need to interact (wall joins, slab openings driven by shafts, cuts), not only at final output. Convergence by placement transform alone is equivalent to Revit links and prevents interaction.

**R3.5 [Proposed]** Default template stages, in dependency order:

0. Shared definitions and library pins.
1. Datums: planning module, level elevations, structural grid.
2. Spine: cores, stairs, shafts, plumbing stacks. Spans levels and emits keep-outs per level.
3. Space layout: room polygons per level (precedence stack), respecting the spine.
4. Boundary graph: edges and vertices classified by adjacency, with provenance. Exterior walls originate here, from room edges facing outside.
5. Assembly resolution: selector cascade assigns default assemblies per edge and face.
6. Envelope: envelope surface, assembly patches, facade system replacements, roofs and other additions (R3.15 to R3.21).
7. Primary geometry: walls with joins, slabs, ceilings, envelope elements.
8. Openings: doors, windows, other cutters, placed and cut.
9. Room contents: casework and furnishings laid out against finished faces.
10. Structure detail: headers, framing, beams.
11. MEP: fixtures, then systems, at selectable fidelity.
12. Takeoff: typed, classified quantities.
13. Pricing: quantities times a rate table.

Massing (R3.11 to R3.14) is not a stage: it is a reference side branch that nothing authoritative depends on.

**R3.6 [Proposed]** Each stage's outputs pass through that stage's override layer before downstream consumption, so an overridden wall carries its hosted elements with it.

**R3.7 [Decided]** Validation (code checks, clearances, clashes) is a side branch that consumes outputs and emits diagnostics. It never feeds back into the DAG. Fixes happen by changing authored inputs.

**R3.8 [Proposed]** Dependencies are tracked per port, not per element. Example: a wall exposes WallAxis, WallAssembly, and WallBody ports (see section 7), which breaks the apparent door-wall cycle.

**R3.9 [Proposed]** Known cycles are resolved by convention, not iteration: wall datum convention (centerline or finished face, chosen once), plenum depth as an assembly parameter (MEP versus ceiling height), fixed head heights per assembly (structure versus openings), room template dimension ranges (contents versus room size).

**R3.10 [Proposed]** Evaluation is lazy and incremental (Salsa-style memoized queries), partitioned by document, level, and room or edge, so a local edit re-evaluates only the affected region. Lowering from IR to the evaluation graph is itself incremental.

### Massing (reference geometry)

**R3.11 [Decided]** Massing volumes are reference-only sketch geometry that helps the user work out the general form of the building. Nothing authoritative depends on them: massing is a side branch of the DAG feeding visualization and optional one-shot utilities. (Revision 2: this replaces the earlier decision that massing was the stage 1 envelope input.)

**R3.12 [Decided]** Mass types and colors exist only to help the user visualize the overall mass and the partitions of space. Masses can be modeled free-form in 3D or generated as stacked per-level slices, with free-form boolean operations shaping the building and its facade.

**R3.13 [Decided]** Masses are layered in a priority stack that defines the order of boolean operations. Two masses of different types produce a cut (the higher-priority mass cuts into the lower one), mirroring how room polygons cut each other.

Proposed details for R3.13: the result is a typed partition of space in which the highest-priority mass claims each point; overlapping masses of the same type union; a void type claims space as empty (courtyards, recesses, notched facades); the stack is a strict total order with list position authoritative.

**R3.14 [Proposed]** Implementation order: stacked per-level slices first, reusing the integer room-stack engine (R6) and extruding between level datums; free-form 3D masses second. Because massing is reference-only, mesh booleans are acceptable for free-form masses, decoupling massing from the B-rep kernel decision. Massing must still evaluate deterministically, since every client sees it and utilities consume it.

**R3.14a [Open]** A "copy out rooms" utility that slices the massing at level datums and writes room polygons as authored inputs. If built, it is a one-shot command, never a DAG edge. To be refined later.

### Envelope stage

**R3.15 [Decided]** The default DAG includes an explicit envelope stage. It may reference existing room control points and edges to define new elements or to patch existing walls and wall assemblies.

**R3.16 [Decided]** The envelope stage must support, at minimum: patching a wall around part of a single-family living room as an exterior wall with an updated assembly; and assigning a curtain panel definition to a high-rise facade.

**R3.17 [Proposed]** The envelope stage runs after default assembly resolution (stage 5) and before primary geometry (stage 7). The cascade assigns sensible default assemblies to every exterior edge first, so single-family envelope work is mostly exceptions, and geometry is generated once from the combined result.

**R3.18 [Proposed]** The stage first derives an envelope surface: exterior boundary edges stacked vertically across levels into continuous facade faces, plus roof surfaces. Facade systems host on regions of that surface, so panel grids stay continuous across level and room boundaries. Patches can still target individual room edges and control points.

**R3.19 [Proposed]** The stage supports three operations:

- **Patch:** change the assembly on an edge or edge span.
- **Replace:** suppress the generated walls on a set of edges and substitute a facade system. A curtain wall has its own geometry, a glazed-panel opening model (not cutters), and its own takeoff, so it replaces walls rather than patching their assemblies.
- **Add:** elements with no wall counterpart, such as roofs, parapets, cornices, and canopies. Roofs are defined in this stage.

**R3.20 [Proposed]** Envelope targets are selected either by explicit references to authored room control points and edges (typical for single-family exceptions) or by selectors such as exterior edges, level ranges, and orientation (typical for high-rise facades). Anchors resolve through provenance (R6.7); when an anchored edge stops being exterior after a layout change, the orphan mechanism (R7.13) applies.

**R3.21 [Proposed]** Rooms clip against envelope roof surfaces (attics, sloped ceilings, knee walls) and handle double-height spaces, not only 2D footprints. The envelope stage belongs to the architect's document by default; on large projects it may be a separate document owned by a facade consultant, in which case replacing the architect's exterior walls is a cross-document operation subject to the same permission treatment as cross-document cutters (R8).

---

## 4. Representation: IR, DSL, serialization

**R4.1 [Decided]** A building is defined as a program. The IR is authoritative at runtime and is saved as DSL text.

**R4.2 [Decided]** Canonical formatting is mandatory. There is exactly one textual form for any IR.

**R4.3 [Decided]** Comments are attached to IR nodes as metadata, so they survive GUI edits, moves, and renames.

**R4.4 [Decided]** Parameters can be expressions (math operator nodes wired as parameter values, as inputs to or outputs from systems).

**R4.5 [Decided, direction]** The DSL uses a Rust-like syntax subset. KDL was considered and rejected.

**R4.6 [Proposed]** Implement the DSL as a strict subset of real Rust syntax: parse with `syn`, print with `prettyplease`, validate that only the allowed subset appears, then lower to the IR. Consequences:

- Only doc comments (`///`, `//!`) survive tokenization. The parser must reject plain `//` comments explicitly so hand-written comments are never silently lost.
- Units are numeric literal suffixes (`450mm`, `4.2m`, `30deg`), read via `syn` literal suffixes.
- Files parse as a statement list (block body), not as a Rust module.
- Rust reserved words are unavailable (`override` is reserved; overrides use `set(...)`).
- Subset violations and type errors need custom diagnostics with spans.
- The DSL is declarative and total even though it looks like Rust.

**R4.7 [Proposed]** Commands mutate the IR, never the text. Text edits (by hand or by AI) are parsed, diffed against the current IR, and converted into a command batch that goes through the same server ordering, permission checks, and concurrency rules as GUI commands. There is exactly one mutation path.

**R4.8 [Proposed]** Pipeline: text to lossless syntax tree, to authored IR (name resolution, import pinning, type checking, stable ID assignment), to evaluation graph, to lazy incremental evaluation.

**R4.9 [Proposed]** Round-trip invariants, enforced by property-based tests (`proptest`), parser fuzzing (`cargo-fuzz`), and golden files from day one:

- parse(print(IR)) == IR
- print(parse(text)) == text for any canonical text

**R4.10 [Proposed]** Grammar rules for round-tripping: one way to write every construct; comment ownership rule (a comment attaches to the following node); invalid text is rejected rather than recovered (error recovery deferred until editor support); a format version header in every file; grammar migrations operate on the IR, not on text.

**R4.11 [Proposed]** Stable IDs derived from declaration paths stay implicit in text. Where an ID cannot be re-derived (renamed or moved nodes), it is written explicitly (`#[id = "..."]`).

**R4.12 [Proposed]** Derived elements are never serialized. Files stay proportional to design intent, not building size.

**R4.13 [Proposed]** Expressions: one IR node per expression holding a typed AST, with free variables as edges to other nodes' outputs. Not one IR node per operator. A node-graph view of an expression is a GUI projection of its AST.

**R4.14 [Proposed]** Expression language rules: dimensional types (length times length is area; length plus angle is a type error); total by construction (arithmetic, comparisons, conditionals, fixed function library; no recursion or unbounded loops); domain functions (`snap`, `clamp`, `min`, `max`, grid rounding); errors are typed values that propagate to diagnostics, never panics.

**R4.15 [Proposed]** Canonical expression printing: minimal parentheses by precedence, no constant folding, shared subexpressions printed as named `let` bindings.

**R4.16 [Open]** Display units: always print a canonical unit (simplest), or preserve the author's unit as metadata.

**R4.17 [Proposed]** Checkpoints are canonical DSL text; binary formats are used only for caches and transport.

---

## 5. Collaboration and synchronization

**R5.1 [Decided]** Collaboration requires a live connection to the server. There is no asynchronous collaboration and no concept of a link. All participants operate on one live DAG.

**R5.2 [Decided]** A Document is a partition of the DAG. It is the unit of permissions (preventing edits by members without authority over that part of the graph) and the unit of pinning (R5.14).

**R5.3 [Decided]** Persistence is versioned checkpoints plus a command tail. Full history from inception is not required; log segments are retained as long as undo, audit, or rebasing need them.

**R5.4 [Decided]** Commands follow a command pattern. Commands can change graph topology and node parameters.

**R5.5 [Decided]** Concurrency semantics are defined up front. Proposed: the server assigns a total order; each command carries the versions of the nodes it read; parameter edits rebase with last-writer-wins per property; structural conflicts (deleting a node someone just referenced) are rejected, and rejection is a first-class, well-communicated outcome.

**R5.6 [Decided]** Thick clients compute geometry deterministically, inspired by Factorio's deterministic lockstep multiplayer: only commands are transmitted; joining clients load a checkpoint and replay the command tail to catch up.

**R5.7 [Decided, to validate]** The server ideally runs no geometry kernel. It orders commands and validates authored-layer invariants only: permissions, types and units, reference acyclicity, ID existence in the authored layer. An implementation test must determine whether any command's acceptance (not just its effect) requires geometry.

**R5.8 [Proposed]** Commands referencing derived IDs either apply or orphan deterministically after evaluation on every client, so the server never needs to check derived existence.

**R5.9 [Proposed]** Client-side prediction: each client keeps a confirmed state plus a queue of pending local commands, and replays pending commands on top when the server confirms others' commands. Continuous gestures (drags) are predicted locally and committed as one command on release.

**R5.10 [Decided]** Undo is an inverse command computed against current state, never a rollback, so undoing one user's change never reverts another user's later edit.

**R5.11 [Proposed]** Desync detection: hash node outputs keyed by stable ID (a Merkle tree over the DAG) and compare hashes, not geometry. Since clients evaluate only partitions in view, a headless reference evaluator outside the critical path can audit hashes.

**R5.12 [Proposed]** All clients in a session run an identical engine build (version-locked).

**R5.13 [Open]** Sync transport: the current dirty-queue design versus a versioned change stream (monotonic revisions, resume from revision N or take a snapshot). To be decided by experiment.

**R5.14 [Decided]** Pinning is a document-level action: a pin is a read-only snapshot of one document's partition at a revision (for example, an architectural snapshot for an MEP consultant). The consumer can later rebase onto a newer pin or onto the live document.

**R5.15 [Proposed]** Every node belongs to exactly one document; partitions are disjoint and together cover the whole DAG. Derived elements belong to the document of the node that generated them. Cross-document references are either live or pinned: `Live(doc)` or `Pin(doc, revision)`.

**R5.16 [Proposed]** A pin captures its dependency closure: the document's root at a revision plus the resolved versions of everything it depends on (shared definitions, libraries, other documents). Otherwise upstream changes would alter a supposedly frozen snapshot. Nodes are content-addressed, so a pin is a new root sharing immutable nodes, and rebase diffing is a Merkle comparison.

**R5.17 [Proposed]** Each document declares an interface: the outputs other documents may reference (levels, room boundaries, wall axes, structural grid, and so on). Rebasing diffs the interface outputs between two pins and flags only the consumer overrides and cut rules that touch what changed. References into a document's internals are discouraged and break on refactors.

**R5.18 [Proposed]** Stable IDs are identical in a pin and in the live document. Cutters reference their hosts by stable ID, so a live host gathers cutters that a consumer placed against a pinned copy, and applies the cut cascade and clash rules (R8) as usual.

**R5.19 [Proposed]** Pinning requires only read permission, since a pin is a snapshot of what the user can already see. Named pins serve as formal issued sets (for example, "Architecture, issued for coordination"), since AEC deliverables carry contractual weight; a project-wide issue is a set of document pins at matching revisions. The document is the only unit of pinning: if a consumer needs part of a document, split the document. Offline work is not supported (consequence of R5.1).

**R5.20 [Decided]** An addressable unique identification system underpins everything, so upstream dependencies (for example, an updated MEP asset library) can be updated into the DAG.

**R5.21 [Proposed]** Library references separate identity (publisher, asset ID, stable across versions) from version (content hash). The DAG references identity plus a pinned hash; upgrades are explicit, diffable commands that rebase dependent overrides. One version of each shared definition per document (no diamond dependencies).

**R5.22 [Open]** Whether branches (design options, permutation explorations, AI-generated proposals) are merged back through a review workflow.

---

## 6. Layout: room polygon stack

**R6.1 [Decided]** Users lay out floors by dragging room polygons layered in a precedence stack. Higher-precedence rooms cut lower ones. Users edit polygons by adding, removing, and modifying control points and edges.

**R6.2 [Proposed]** The stack is a strict total order. List position is authoritative; layer numbers, if kept, are grouping only.

**R6.3 [Proposed]** Layout geometry uses integer coordinates on the planning grid, so coincidence is exact and no tolerance is needed.

**R6.4 [Proposed]** Control points receive IDs at creation and keep them forever (never renumbered or reused). Rectangle sugar assigns its four IDs deterministically.

**R6.5 [Proposed]** An edge's identity is its start control point. Inserting a point on edge e(p2) yields e(p2) (shortened) and a new e(p5).

**R6.6 [Proposed]** Control-point edits and stack reordering migrate dependent references (anchors, offsets) within the same command, like a rename refactoring updating call sites. Undo reverses both. This is authored-layer integer arithmetic and needs no kernel on the server.

**R6.7 [Proposed]** Evaluating the stack produces the planar boundary graph (rooms as faces, walls as edges, joints as vertices). Every derived boundary segment records provenance: (room, authored edge, parameter interval on that edge). Derived wall segment IDs come from the provenance pair of authored edges.

**R6.8 [Proposed]** The boundary graph is complete within each level's room layout: enclosed space not claimed by a room becomes circulation or an explicit unassigned face; silent gaps are not allowed. (With massing reference-only, the level footprint is defined by the rooms, not by massing.)

**R6.9 [Open]** Coincident-edge addressing. Martin stated that for coincident edges, the higher-precedence room's edge is used for addressing. The later anchoring-mode design (section 7) may make this rule unnecessary, since no mode addresses coincident polygon edges directly. Confirm before implementing.

---

## 7. Hosted elements: doors, walls, openings

**R7.1 [Decided]** Door placement must support both modes: doors associated with a room's edge (for example, repeated hospital patient rooms) and free-form doors placed after layout (for example, single-family homes).

**R7.2 [Proposed, adopted in discussion]** Placement variants:

- **RoomOwned:** declared inside a room template, anchored in the room's local frame via edge roles (front, headwall), copied with every instance. Mirrored instances flip door handing.
- **Between (connection-anchored):** declared at floor level, placed along the shared boundary of two rooms, positioned relative to semantic joints named by topology (for example, the joint where a third room meets the shared span).
- **OnWallAxis:** placed along an explicit wall's axis, relative to its control points (for freely drawn walls).
- **LevelBound:** fixed position and rotation in a level's frame (for example, a freestanding door in a gallery).

**R7.3 [Proposed]** Positions are expressed relative to anchors (offset along an edge from a named end, or centered between two control points), never as absolute coordinates. Drag gestures infer the anchor end and snap to module or sensible increments.

**R7.4 [Proposed]** A drag edits the declaration in place when the user owns the declaration and the drag maps to a parameter; otherwise it writes an override patch on the element's stable ID. Read-only upstream elements always receive patches. The client decides using IR provenance and the permission model.

**R7.5 [Decided]** Doors are placed relative to a wall axis (OnWallAxis), and the wall body accepts doors in its Openings list.

**R7.6 [Proposed]** Wall ports: WallAxis (location curve and control points), WallAssembly (layers and thickness), WallBody (solid with openings). Doors depend on WallAxis for position and WallAssembly for jamb depth; WallBody depends on all three plus the cut profiles of its openings.

**R7.7 [Proposed]** Openings is a typed input whose default is a derived query (every cutter placed on this axis), with explicit `.plus()` and `.minus()` edits. A LevelBound element cuts only when explicitly added; a derived cutter can be removed (for example, a surface-mounted decorative door).

**R7.8 [Proposed]** Openings accept any Cutter (doors, windows, niches, MEP sleeves). The same pattern applies to SlabBody and RoofBody.

**R7.9 [Proposed]** Cutting is always explicit (via placement or the Openings list), never by geometric coincidence. A wall dragged through a LevelBound door produces a clash diagnostic, not an opening.

**R7.10 [Proposed]** Reference cycles between anchored elements (door A relative to door B and vice versa) are rejected at command time.

**R7.11 [Proposed]** On curved walls, offsets are arc length. Whether a straight door flattens the wall locally or the door curves is defined by the door's library type.

**R7.12 [Decided]** When a layout change breaks an element's anchor, the element is orphaned but kept in the scene. The user can drag it onto another host to re-attach it, convert it to LevelBound, or delete it.

**R7.13 [Decided]** Orphaned is a derived state, not written into the IR. The authored placement is unchanged; evaluation fails to resolve it. Consequences:

- If the anchor becomes valid again (layout change reverted, undo, concurrent edits), the element re-attaches automatically.
- The orphan's displayed position is computed deterministically from current inputs (for example, the nearest point between the two anchor rooms' boundaries, with fixed rounding). Never a remembered "last valid" position.
- Orphans cut nothing, remain in takeoff and pricing flagged as unresolved, and appear in a problems list with the three actions inline.
- Re-attaching rewrites the placement while keeping the stable ID, so overrides and clash history carry over.

**R7.14 [Decided]** The orphan mechanism applies to every anchored element (windows, casework, fixtures), built once rather than as a door feature.

---

## 8. Cutting priority and clashes

**R8.1 [Decided]** Cutters carry a cut priority so lower-priority elements (for example, ductwork) cannot cut through higher-priority ones (for example, structural columns and beams).

**R8.2 [Proposed]** Priority also orders general body overlaps (column through wall, beam into slab): the higher-priority body cuts the lower one. Each category has a default priority. Equal-priority overlaps raise a diagnostic rather than being silently tie-broken.

**R8.3 [Decided]** A cutting-rule cascade, using the same selector mechanism as the assembly cascade, decides what cutters may do to hosts.

**R8.4 [Decided]** Policies are binary: Allow (optionally constrained by zone and size) or Deny. There is no approval state in the engine. Disallowed cuts and constraint violations become clashes resolved in coordination and review meetings.

**R8.5 [Proposed]** Clash detection is derived; clash records are authored. A clash's ID derives from the (cutter ID, host ID) pair. Status, assignment, and notes live in a coordination partition. Acceptance records the content hashes of both elements and reopens automatically if either changes materially.

**R8.6 [Proposed]** Meeting resolutions land as ordinary data in the owning partition: the MEP owner reroutes, the structural owner adds a scoped CutRule targeting the specific cutter and host IDs, or the host is redesigned. Engineering cut limits are owned by the structural partition.

**R8.7 [Proposed]** Structural bodies feed MEP routing as keep-outs, so procedural routing avoids most denied penetrations upstream.

---

## 9. Determinism and numerics

**R9.1 [Decided]** Geometry evaluation must be bit-identical across all clients (required by R5.6).

**R9.2 [Proposed]** Integers by layer:

- Authored inputs (coordinates, lengths, parameter values) are stored as integer micrometers (`i64`).
- The 2D layout and boundary graph use integer coordinates, exact predicates in `i128`, and snap rounding for intersection points.
- The 3D B-rep (with curves) uses f64 with exact predicates and inexact constructions (CGAL EPICK pattern, Shewchuk-style adaptive predicates) and tolerance-based topology with deterministic tie-breaking.
- Expressions evaluate in deterministic f64; results feeding integer-typed parameters are quantized with round-half-even.

**R9.3 [Proposed]** f64 determinism checklist: one engine build per session; the `libm` crate for all transcendentals (never std); explicit `mul_add` only; no approximate reciprocal intrinsics; no relaxed SIMD in WASM; ordered containers or fixed hashers wherever iteration order affects output; fixed partitioning and ordered reduction for parallelism; explicit floating-point environment at thread start; no GPU computation for authoritative results.

**R9.4 [Proposed]** Output hashes may quantize to the integer grid as a safety net, but bit-identical evaluation is the guarantee.

**R9.5 [Open]** B-rep kernel choice: Rust candidates (truck and its forks) versus OpenCascade. Must satisfy R1.2 and R9.1.

---

## 10. Takeoff and pricing

**R10.1 [Decided]** The layout process feeds a dynamic price sheet with quick material cost feedback.

**R10.2 [Proposed]** Takeoff produces typed quantities (count, length, area, volume, weight) at a selectable level of detail, tagged with a classification (Uniformat for elemental, MasterFormat for trade).

**R10.3 [Proposed]** Pricing is a separate stage (quantities times rates) so a rate change never re-runs geometry. Rate tables are swappable, pinned data, since rates are regional and volatile. Labor is a large share of residential cost; material-only pricing must be labeled as such.

**R10.4 [Proposed]** Price updates are incremental over the dependency graph so drags update totals interactively.

**R10.5 [Proposed]** Assemblies are assigned by a selector cascade (rules keyed on room-type pairs, room types, exterior edges), with specificity then order resolving conflicts.

**R10.6 [Open]** Who authors takeoff rules: VIM, the designer, or the estimator.

---

## 11. Suggested first spikes

1. **Determinism:** run the same evaluation natively on x86-64, ARM, and WASM; compare output hashes (R9).
2. **DSL round trip:** `syn` plus `prettyplease` with declarations, expressions, and overrides; property-based round-trip tests (R4.6, R4.9).
3. **Layout stack and anchors:** integer polygon stack to boundary graph with provenance; doors anchored in each mode; anchors migrating on control-point edits; orphaning and re-attachment; an envelope patch anchored to a room edge (R6, R7, R3.20). The same stack engine should then extrude per-level massing slices (R3.14).
4. **Lockstep sync:** checkpoint plus command-tail join; Merkle hash desync detection; client-side prediction for a drag (R5).
5. **Kernel-free server:** identify any command whose acceptance requires geometry (R5.7).

---

## 12. Illustrative DSL sketch

Not normative; shows the direction of R4 and R7. An earlier full-house mockup (`home.vd.rs`) predates the anchoring-mode, massing, and envelope decisions (it treats massing as the authoritative envelope); its `Door::between` and `connections` lists correspond to the Between mode.

```rust
#![vim_design(format = 1)]
#![package(rooms = "vim/residential-rooms@2.3.1#3f9a6d10")]

/// Clearance around islands.
let clearance = 450mm;

/// Connection-anchored door between hall and bedroom.
let bedroom_2_door = Door {
    kind: doors::Swing { width: 813mm },
    placement: Between {
        rooms: (upper.hall, upper.bedroom_2),
        at: Along { from: Joint(upper.primary), offset: 150mm },
    },
};

/// Freestanding gallery door; cuts nothing.
let gallery_door = Door {
    kind: doors::Pivot { width: 1219mm },
    placement: LevelBound {
        frame: levels.main,
        at: grid(5, 6),
        rotation: 30deg,
    },
};

/// Envelope stage: patch one wall span, replace a high-rise facade.
let envelope = Envelope {
    patches: [
        AssemblyPatch {
            span: main.living.edge(3).from(main.living.point(3), 3048mm),
            assembly: asm::Exterior2x6StoneVeneer,
        },
    ],
    systems: [
        CurtainWall {
            region: Facade::exterior().levels(2..=40).facing([South, East]),
            grid: PanelGrid { vertical: 1524mm, horizontal: LevelLines },
            panels: facade::UnitizedVision,
        },
    ],
};

/// Override on a generated element, by stable ID.
set(main.kitchen.cabinets.north.slot[3], Cabinet::Patch { width: 914mm });

/// Design option as a variant layer.
let island_option = Variant {
    target: main.kitchen,
    active: false,
    patch: rooms::Kitchen::Patch {
        nook: None,
        island: Some(Island {
            length: 2134mm,
            clearance: max(1067mm, main.kitchen.width / 5),
        }),
    },
};
```

---

## 13. Reference material

Pixar USD composition (references, inherits, variants, specializes, strength ordering) for layering and overrides. Salsa (rust-analyzer) for incremental memoized queries. `rowan`, `syn`, and `prettyplease` for syntax trees and canonical printing. Terraform `moved` blocks and `plan` for refactor-safe identity and semantic diffs. Starlark for a deliberately restricted declarative language. Factorio Friday Facts on deterministic lockstep, latency hiding, and join catch-up. Shewchuk's adaptive predicates, CGAL's exact-predicates-inexact-constructions kernel, Clipper, and snap rounding (Hobby; Guibas and Marimont) for robust geometry. CGA shape grammar (Müller et al., 2006) for split rules with fixed and floating sizes. Sketch-n-Sketch (Chugh et al., PLDI 2016) for mapping direct manipulation back to program edits.

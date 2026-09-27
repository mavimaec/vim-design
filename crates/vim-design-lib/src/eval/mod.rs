//! Geometry evaluation layer — turns the parametric graph into BREP
//! solids and tessellated meshes, exposed through a poll facade
//! (docs/ARCHITECTURE.md §§3.3, 6).
//!
//! # The facade contract (consumed by VimDesignWeb and, later, the FFI)
//!
//! One [`Engine`] per [`Document`](crate::Document); the caller owns the
//! pair and drives it:
//!
//! ```
//! use vim_design_lib::{Command, Document};
//! use vim_design_lib::eval::Engine;
//!
//! let mut doc = Document::new();
//! let mut engine = Engine::new();
//! let _ = doc.submit(Command::CreateCylinder {
//!     center: [0.0, 0.0, 0.0], radius: 0.5, height: 2.0,
//! });
//! engine.evaluate_pending(&mut doc);          // after commits
//! let updates = engine.poll_updates(&doc);    // once per frame
//! assert_eq!(updates.meshes.len(), 1);
//! assert_eq!(updates.committed_generation, updates.evaluated_generation);
//! ```
//!
//! - [`Engine::evaluate_pending`] consumes `Document::take_dirty()` and
//!   synchronously re-evaluates the dirty closure in topological waves
//!   (parallel within a wave via rayon under the `parallel` feature;
//!   sequential on wasm), then re-tessellates affected mesh owners at
//!   the document's chordal tolerance (default 1 mm).
//! - [`Engine::poll_updates`] returns the changed-set since the previous
//!   poll ([`Updates`]): coalesced latest-state upserts keyed by stable
//!   ids, explicit tombstones, per-entity error transitions, the
//!   parametric changed-set, and the settledness counters
//!   (`evaluated == committed` and `pending_count == 0` means
//!   quiescent). Apply removals before upserts; a delete + recreate
//!   between polls arrives as a plain upsert.
//!
//! # The dirty pump: `params_changed` (docs/ARCHITECTURE.md §6.3)
//!
//! [`Updates::params_changed`] reports the entity ids **directly
//! touched** by committed deltas — the delta targets, never the
//! downstream dirty closure — recorded at the document's single commit
//! gate, so user edits, undo, redo, composites, and scripts all report
//! through one path (undo is not a special case in UI code) and a
//! rejected command's rolled-back deltas never appear. Semantics:
//! - Accumulation is a **set**: an entity touched 500 times between
//!   polls appears once; the widget reads current state from the
//!   document at poll time.
//! - **Deleted ids are reported**: the deletion touched the id, and the
//!   bound widget needs to hear it went away. Reported once per drain;
//!   distinguish update vs removal via `Document::entity(id)`. A
//!   delete + undo between polls reports the id once, entity alive.
//! - **Single consumer**: draining clears; the report means "since
//!   *your* last poll".
//! - **Interest filter**: [`Engine::set_params_watch`] trims
//!   `params_changed` to the ids the UI displays; `None` (default)
//!   reports everything. The filter applies at drain time against the
//!   current watch set and never touches mesh/instance/error reporting;
//!   dirt accumulated before a watch change is filtered by the new set,
//!   and non-matching accumulated ids are discarded by the drain.
//! - Ids reach the engine's accumulator via [`Engine::evaluate_pending`]
//!   (the same stroke that pumps geometry), matching the eager
//!   submit → evaluate → poll cycle.
//!
//! # Mesh ownership (the standalone-solid + chamfer rule)
//!
//! [`MeshUpdate::id`] is a **mesh owner**:
//! - every `Element` entity (mesh = its members' solids merged), and
//! - every solid producer (`Extrusion`, `Revolve`, `Solid`, `Chamfer`)
//!   that is **not consumed** — i.e. not wired into any element's
//!   members slot and not targeted by any chamfer. A chamfer *replaces*
//!   its target as mesh owner: the chamfered solid IS the target's
//!   render shape, so creating a chamfer tombstones the target's
//!   standalone mesh and deleting the chamfer hands it back.
//!
//! Wrapping a producer into an element tombstones its standalone mesh
//! and re-delivers the geometry under the element id. Renderers draw an
//! element's mesh once per [`InstanceUpdate`] referencing it; a mesh
//! owner with no instances (all standalone solids, and elements the user
//! has not instanced) is drawn once at identity.
//!
//! `Site` and `Level` entities are **never mesh owners and never
//! tessellated** (docs/AUTHORING.md §§1–2): a level evaluates to a
//! construction-plane [`Evaluated::Frame`], and its translucent display
//! square is a renderer overlay the app draws directly from
//! `Params::Level` (elevation, color, extent) — model/view separation, a
//! level has no volume. An element's `level` association slot is
//! data-only: rewiring it re-delivers a byte-identical mesh, and level
//! *value* changes (an elevation drag) never re-evaluate the element
//! through that edge — the association is exempt from change-driven
//! propagation because the element evaluator does not read it
//! (docs/AUTHORING.md §4; cascade/dependent semantics are structural
//! and unaffected). Element-wrapped attached owners therefore get the
//! same transform-only elevation drags as standalone ones.
//!
//! # Evaluation performance: early cutoff and translation factoring
//!
//! **Early cutoff.** Only entities whose own params/wiring changed (the
//! commit-gate roots) are forced to re-evaluate; everything downstream
//! re-evaluates only when an input's *result* actually changed. Change
//! detection is exact equality on the plain-data [`Evaluated`] variants
//! (`Point`, `Plane`, `Curve`/`Edge`, `Wire`, `Frame`, `Site`,
//! `Material`, `Instance`) plus error-state transitions; kernel-handle
//! variants (`Face`, `Solid`, `SolidSet`) are **not comparable** and
//! always count as changed when they re-evaluate (documented
//! limitation). Headline consequence: a Level edit that does not move
//! the frame (name/color/extent/story flag) does zero downstream work —
//! the pump still reports the level in `params_changed`.
//!
//! **Translation factoring (level-local evaluation) — OPT-IN via
//! [`Engine::set_translation_factoring`], default OFF.** While off, the
//! engine behaves exactly as before this milestone (world-baked meshes,
//! identity base transforms) so renderers that ignore base transforms
//! keep rendering correctly; a renderer opts in the moment it composes
//! `instance ∘ base`. When enabled: an entity whose entire *spatial*
//! input closure is attached to one Level evaluates in that level's
//! LOCAL space (the level origin treated as zero):
//! attached control points evaluate to their stored `(u, v, w)`
//! verbatim, and everything built from them — curves, faces, solids,
//! provenance names, paints, chamfers — is level-local. The mesh owner
//! then delivers a **local mesh** plus [`MeshUpdate::base_transform`]
//! (the level origin). An elevation drag therefore re-evaluates nothing
//! downstream and re-tessellates nothing: the poll carries only
//! [`Updates::base_transforms`] entries (never overlapping `meshes`),
//! and undoing the drag is transform-only too.
//!
//! *Qualification rule:* `Level(l)` space is assigned bottom-up — a
//! control point wired to level `l`, and any entity all of whose
//! spatial inputs are in `Level(l)`. Disqualified to world space: any
//! unattached point, mixed-level attachment, or an explicit `Plane`
//! input (conservative); and kernel *handles* never cross spaces — a
//! Face/Solid consumed by a world-space entity is demoted to world
//! itself (its plain-data wire/curve inputs are translated at
//! consumption instead). Disqualified owners keep the full re-eval path
//! and identity base transforms — exactly today's behavior.
//!
//! *Renderer contract:* `world = instance_transform ∘ base_transform`
//! (base applied first); standalone owners render at `base_transform`
//! alone. Note that `Evaluated::Point` (and every derived value) for
//! level-attached geometry is level-LOCAL — compose with the level's
//! frame origin for world positions.
//!
//! # Provenance naming & SubRefs (docs/ARCHITECTURE.md §3.4)
//!
//! Extrusion/revolve evaluators name their generated faces by the stable
//! ids of the inputs that produced them: `Side { source: edge-entity }`
//! for lateral faces (hole-wire sides included), `CapStart`/`CapEnd` for
//! the sweep caps (revolves get caps only for partial angles). Generated
//! **edges** are addressed as the intersection of two named faces
//! (`ProvenancePath::SharedEdge`). Names are re-derived from geometry on
//! every evaluation — kernel face indices never cross this facade — and
//! survive downstream operations: a chamfered solid keeps its upstream
//! face names, and each blend face is named by the `SharedEdge` path of
//! the edge it replaced. [`Engine::resolve_subref`] resolves a `SubRef`
//! against the owner's current solid; a reference that no longer matches
//! (source edge deleted, cap gone on an angle change) is a typed
//! [`EvalErrorKind::UnresolvedSubRef`] — never a silent re-bind.
//!
//! # Provenance queries — set-valued targeting and the liveness contract
//!
//! A `ProvenanceQuery` (`SubRefSet { owner, query }`) targets a *class*
//! of generated topology — `RimEdges`, `SideFaces`, `Caps`,
//! `VerticalEdges` (each filterable to the outer wire or the hole
//! wires), and `Union`. **The liveness contract:** a query is expanded
//! against the owner's *current* solid on every owner re-evaluation, and
//! an **empty expansion is a valid result, not an error** — paint
//! `SideFaces { HolesOnly }` or chamfer `RimEdges { HolesOnly }` on a
//! hole-less profile and nothing happens *yet*; add a hole later and the
//! new walls are painted / the new rim is blended automatically, because
//! expansion re-runs. No extra dirtying machinery exists or is needed:
//! the consumer already depends on the owner through an ordinary graph
//! edge, so any topology-changing upstream edit re-evaluates it. A
//! chamfer whose query targets all expand empty passes its target's
//! solid through unchanged (a no-op, not an error).
//! [`Engine::resolve_query`] reports a query's current face/edge counts;
//! it errors only on structural mismatch (no evaluated geometry, or an
//! owner without a sweep provenance model).
//!
//! # Materials
//!
//! [`Submesh::material`] carries a `Material` entity id (or `None` for
//! the caller's default material); readers fetch color/roughness from
//! the document's `Params::Material`. A solid's default material is its
//! profile face's material; on top of that, `UpdateSubFaceMaterial`
//! paints individual *generated* faces by provenance path, and the
//! submeshes split accordingly (one submesh per material group). Paints
//! survive upstream edits and chamfers because they are keyed by
//! provenance, not by face index (the anti-topological-naming rule).
//! Chamfers render their target's paints; blend faces get the default.
//!
//! # Sketches
//!
//! A `Sketch` entity (see [`crate::sketch`]) evaluates to an
//! [`Evaluated::SolidSet`] of prisms, all tagged with the sketch's id. It
//! is a solid producer for mesh ownership: an element member, or a
//! standalone owner when no element wraps it. Material hangs from the
//! plane on the `direction` side; at each point of the plane the solid
//! depth is the thickest solid face covering it and the removed depth is
//! the deepest void covering it (a void without depth removes
//! everything). Each distinct thickness or depth is a layer boundary; a
//! layer's footprint is the union of the solid faces thick enough for it
//! minus the union of the voids deep enough for it, and each footprint
//! polygon becomes a prism for that layer. A polygon that repeats
//! unchanged in the next layer extends its prism, so a plain plate is
//! one prism. Stacked prisms touch along internal faces; the element's
//! mesh is their union (its volume is exact, but it is not one closed
//! shell where layers meet).
//!
//! A sketch with no material left (only voids, or voids that remove
//! every solid) has no mesh: its owner's mesh is tombstoned, and no
//! error is reported. A face loop that crosses itself or has no area is
//! a per-entity [`EvalErrorKind::Degenerate`] error on the sketch (the
//! last good mesh stays).
//!
//! Provenance of sketch prisms is named from sketch-local ids, which are
//! stable across edits:
//! - a lateral face is `SketchSide { face, a, b }`: the boundary edge
//!   between sketch points `a < b` of sketch face `face` that sweeps it.
//!   When several sketch edges carry the same boundary (a solid and a
//!   void sharing an edge), the name comes from a face that is active in
//!   that layer, solid faces before voids, then the lowest face id;
//! - a cap is `SketchCap { depth_um, toward_plane }`: its depth from the
//!   plane in micrometers, and whether it faces the plane (the top of a
//!   floor plate) or away from it. Caps at one depth and facing share the
//!   name.
//!
//! `Engine::resolve_subref` resolves these names across all prisms of a
//! sketch owner. Provenance queries (`SubRefSet`) do not expand on sketch
//! output.
//!
//! A sketch on a level evaluates in that level's local space
//! (translation factoring): a level elevation edit re-places the owner
//! with a base transform and re-evaluates nothing.
//!
//! # Workplanes and walls
//!
//! A `Workplane` evaluates to its parent's frame moved along the
//! parent's normal. Every [`Evaluated::Frame`] carries `level_offset`,
//! its origin relative to its root level summed along the parent chain.
//! Geometry on any construction plane lives in the space of the plane's
//! ROOT level and uses `level_offset` as its local origin, so dragging
//! the root level re-places owners with base transforms only; a frame
//! whose axes and level offset did not change does not re-evaluate
//! level-local consumers.
//!
//! A `Wall` evaluates to an [`Evaluated::SolidSet`] of prisms, owned by
//! the wall: its effective profile (top-anchored points raised to the
//! top reference height) placed in the wall's vertical frame, with the
//! same layered algorithm and provenance names as sketches. A wall with
//! only a base plane is level-local; a wall with a top plane is
//! evaluated in world space, so moving either plane re-evaluates it. A
//! top reference at or below the base is a per-entity
//! [`EvalErrorKind::Degenerate`] error.
//!
//! # Chamfer
//!
//! A `Chamfer` blends edges of its `target` producer via
//! monstertruck-fillet (flat `Chamfer` profile, constant distance).
//! Edges are addressed by `Params::Chamfer::sub_edges` (`SharedEdge`
//! SubRefs whose owner is the target) and/or authored `Edge` entities in
//! the edges slot (matched to coincident solid edges — e.g. a profile
//! edge coincides with the extrusion's bottom rim). Straight edges
//! between planar faces are the supported class (docs §5.2);
//! curved-edge failures surface as typed per-entity errors.
//!
//! # Errors (docs/ARCHITECTURE.md §6.4)
//!
//! Geometric failures are per-entity: the entity keeps its last
//! successful value and mesh (stale), downstream evaluates against the
//! stale geometry where possible, and the failure surfaces in
//! [`Updates::errors`] with a typed [`EvalDiag`]. Fixing the parameters
//! clears the error via [`Updates::errors_cleared`]. `Selection` and
//! `SectionBox` are not evaluated in this milestone and report
//! [`EvalErrorKind::NotYetImplemented`] without affecting the rest of
//! the scene (a chamfer fed by a `Selection` inherits a clean upstream
//! error); extrusions along spline paths report
//! [`EvalErrorKind::NotYetSupported`].
//!
//! `Solid`-from-faces evaluates, but the kernel requires a closed
//! manifold shell with *shared* edge/vertex topology; independently
//! authored faces are not stitched yet (v1), so such solids typically
//! report a typed kernel error until the stitching follow-up lands. The
//! practical v1 solid producers are `Extrusion` and `Revolve`.

mod engine;
mod evaluate;
mod types;

pub use engine::Engine;
pub use types::{
    BaseTransformUpdate, EvalDiag, EvalErrorKind, EvalState, Evaluated, IDENTITY_TRANSFORM,
    InstanceUpdate, Mesh, MeshUpdate,
    QueryResolution, SubRefResolution, Submesh, Updates,
};

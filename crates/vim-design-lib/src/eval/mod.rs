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
//!   ids, explicit tombstones, per-entity error transitions, and the
//!   settledness counters (`evaluated == committed` and
//!   `pending_count == 0` means quiescent). Apply removals before
//!   upserts; a delete + recreate between polls arrives as a plain
//!   upsert.
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
    EvalDiag, EvalErrorKind, EvalState, Evaluated, InstanceUpdate, Mesh, MeshUpdate,
    QueryResolution, SubRefResolution, Submesh, Updates,
};

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
//! # Mesh ownership (the standalone-solid rule)
//!
//! [`MeshUpdate::id`] is a **mesh owner**:
//! - every `Element` entity (mesh = its members' solids merged, one
//!   [`Submesh`] per member), and
//! - every solid producer (`Extrusion`, `Revolve`, `Solid`) that is
//!   **not** wired into any element's members slot — it surfaces as an
//!   implicit standalone mesh keyed by its own entity id.
//!
//! Wrapping a producer into an element tombstones its standalone mesh
//! and re-delivers the geometry under the element id. Renderers draw an
//! element's mesh once per [`InstanceUpdate`] referencing it; a mesh
//! owner with no instances (all standalone solids, and elements the user
//! has not instanced) is drawn once at identity.
//!
//! # Materials
//!
//! [`Submesh::material`] carries a `Material` entity id (or `None` for
//! the caller's default material); readers fetch color/roughness from
//! the document's `Params::Material`. v1 assigns whole-solid materials —
//! an extrusion/revolve inherits its profile face's material; per-face
//! materials on generated topology arrive with `SubRef` resolution.
//!
//! # Errors (docs/ARCHITECTURE.md §6.4)
//!
//! Geometric failures are per-entity: the entity keeps its last
//! successful value and mesh (stale), downstream evaluates against the
//! stale geometry where possible, and the failure surfaces in
//! [`Updates::errors`] with a typed [`EvalDiag`]. Fixing the parameters
//! clears the error via [`Updates::errors_cleared`]. `Selection`,
//! `Chamfer`, and `SectionBox` are not evaluated in this milestone and
//! report [`EvalErrorKind::NotYetImplemented`] without affecting the
//! rest of the scene; extrusions along spline paths report
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
    Submesh, Updates,
};

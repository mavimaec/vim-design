//! VIM Design core library — the parametric layer.
//!
//! Implements the dependency-graph substrate and delta-based command
//! system from docs/ARCHITECTURE.md: entities with typed slots (§3),
//! the four-primitive delta kernel with mechanical inversion (§4.1),
//! undo/redo with coalescing (§4.2), structural-only validation, dirty
//! tracking (§6.1), and deterministic serialization (§10). The [`eval`]
//! module is the geometry evaluation layer (§§3.3, 6): it turns the
//! parametric graph into BREP solids and tessellated meshes via the
//! `monstertruck` kernel behind the [`kernel`] seam (§5), exposed
//! through the [`eval::Engine`] poll facade (§6.3).

pub mod command;
pub mod delta;
pub mod document;
pub mod entity;
pub mod eval;
pub mod graph;
pub mod id;
pub mod kernel;
mod planar_mesh;
pub mod selection;
pub mod serialization;
pub mod sketch;
pub mod status;
pub mod subref;
pub mod wall;
pub mod wall_run;
pub mod room;
pub mod room_layout;
pub mod plan_span;
pub mod workplane;

pub use command::{Command, CommandOutput};
pub use delta::{Delta, SlotIdx, apply_delta};
pub use document::{CommandGroup, Document, DocumentSettings};
pub use entity::{EntityKind, EntityRecord, Params, SlotDecl, SlotValue, slots};
pub use eval::{
    BaseTransformUpdate, EvalDiag, EvalErrorKind, EvalState, Evaluated, IDENTITY_TRANSFORM,
    InstanceUpdate, Mesh, MeshUpdate,
    QueryResolution, SubRefResolution, Submesh, Updates,
};
pub use graph::GraphState;
pub use id::{EntityId, IdAllocator};
pub use selection::{PredicateAst, SelectionScope};
pub use status::VimStatus;
pub use subref::{
    CapId, EdgeTarget, FaceTarget, ProvenancePath, ProvenanceQuery, Ref, SubRef,
    SubRefSet, WireFilter,
};

/// The library's semantic version (from Cargo.toml).
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod tests {
    #[test]
    fn version_is_nonempty_semver() {
        let v = super::version();
        assert!(!v.is_empty());
        assert_eq!(v.split('.').count(), 3);
    }
}

//! VIM Design core library — the parametric layer.
//!
//! Implements the dependency-graph substrate and delta-based command
//! system from docs/ARCHITECTURE.md: entities with typed slots (§3),
//! the four-primitive delta kernel with mechanical inversion (§4.1),
//! undo/redo with coalescing (§4.2), structural-only validation, dirty
//! tracking for the (future) evaluation layer (§6.1), and deterministic
//! serialization (§10). Geometry evaluation and the `monstertruck`
//! kernel integration are the next milestone; the [`kernel`] module is
//! still the compile/link probe.

pub mod command;
pub mod delta;
pub mod document;
pub mod entity;
pub mod graph;
pub mod id;
pub mod kernel;
pub mod selection;
pub mod serialization;
pub mod status;
pub mod subref;

pub use command::{Command, CommandOutput};
pub use delta::{Delta, SlotIdx, apply_delta};
pub use document::{CommandGroup, Document, DocumentSettings};
pub use entity::{EntityKind, EntityRecord, Params, SlotDecl, SlotValue, slots};
pub use graph::GraphState;
pub use id::{EntityId, IdAllocator};
pub use selection::{PredicateAst, SelectionScope};
pub use status::VimStatus;
pub use subref::{ProvenancePath, Ref, SubRef};

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

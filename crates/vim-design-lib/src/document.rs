//! Placeholder `Document`.
//!
//! The real document (entities, dependency graph, command stacks) is being
//! designed in parallel; this exists only so the FFI/web/test crates have a
//! concrete type to hold behind their handles.

use crate::id::{EntityId, IdAllocator};

/// A VIM Design document: the accumulating state of one "VimDesign" object.
#[derive(Debug, Default)]
pub struct Document {
    ids: IdAllocator,
    /// Placeholder generation counter (bumped once per committed command
    /// in the real design; here it just proves the struct is mutable state).
    generation: u64,
}

impl Document {
    pub fn new() -> Self {
        Self::default()
    }

    /// Allocate a fresh entity id (placeholder; real allocation happens
    /// inside command application).
    pub fn allocate_id(&mut self) -> EntityId {
        self.generation += 1;
        self.ids.allocate()
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn document_allocates_distinct_ids() {
        let mut doc = Document::new();
        let a = doc.allocate_id();
        let b = doc.allocate_id();
        assert_ne!(a, b);
        assert_eq!(doc.generation(), 2);
    }
}

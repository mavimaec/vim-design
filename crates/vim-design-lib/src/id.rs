//! Entity identity: per-document monotonic `u64` ids, never reused.
//! See docs/ARCHITECTURE.md §3.1.

/// Stable identifier for an entity within one document.
///
/// Ids are allocated monotonically and never reused, so stale ids fail
/// lookup instead of aliasing a recycled slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EntityId(pub u64);

impl EntityId {
    /// Sentinel for "no entity"; never allocated.
    pub const INVALID: EntityId = EntityId(0);
}

/// Monotonic id allocator. The next-id counter is part of the persisted
/// parametric state (docs/ARCHITECTURE.md §10).
#[derive(Debug, Clone)]
pub struct IdAllocator {
    next: u64,
}

impl IdAllocator {
    pub fn new() -> Self {
        // 0 is reserved as EntityId::INVALID.
        Self { next: 1 }
    }

    /// Allocate the next id. Ids are never reused.
    pub fn allocate(&mut self) -> EntityId {
        let id = EntityId(self.next);
        self.next = self.next.saturating_add(1);
        id
    }
}

impl Default for IdAllocator {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_monotonic_and_never_invalid() {
        let mut alloc = IdAllocator::new();
        let a = alloc.allocate();
        let b = alloc.allocate();
        assert_ne!(a, EntityId::INVALID);
        assert!(b > a);
    }
}

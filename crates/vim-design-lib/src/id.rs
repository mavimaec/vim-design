//! Entity identity: per-document monotonic `u64` ids, never reused.
//! See docs/ARCHITECTURE.md §3.1.

use serde::{Deserialize, Serialize};

/// Stable identifier for an entity within one document.
///
/// Ids are allocated monotonically and never reused, so stale ids fail
/// lookup instead of aliasing a recycled slot.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
)]
pub struct EntityId(pub u64);

impl EntityId {
    /// Sentinel for "no entity"; never allocated.
    pub const INVALID: EntityId = EntityId(0);
}

/// Monotonic id allocator. The next-id counter is part of the persisted
/// parametric state (docs/ARCHITECTURE.md §10).
///
/// The counter is deliberately NOT delta-tracked: undoing a `Create` does
/// not roll it back, so a subsequent create after undo can never collide
/// with an id still referenced by the redo stack (see `Document`).
#[derive(Debug, Clone)]
pub struct IdAllocator {
    next: u64,
}

impl IdAllocator {
    pub fn new() -> Self {
        // 0 is reserved as EntityId::INVALID.
        Self { next: 1 }
    }

    /// Restore an allocator whose next id is `next` (used by load).
    /// Values below 1 are clamped so `EntityId::INVALID` is never issued.
    pub fn with_next(next: u64) -> Self {
        Self { next: next.max(1) }
    }

    /// Allocate the next id. Ids are never reused.
    pub fn allocate(&mut self) -> EntityId {
        let id = EntityId(self.next);
        self.next = self.next.saturating_add(1);
        id
    }

    /// The id the next `allocate` call would return (persisted on save).
    pub fn peek_next(&self) -> u64 {
        self.next
    }

    /// Raise the counter so it is strictly above `id` (never lowers it).
    pub fn ensure_above(&mut self, id: EntityId) {
        if self.next <= id.0 {
            self.next = id.0.saturating_add(1);
        }
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

    #[test]
    fn ensure_above_never_lowers() {
        let mut alloc = IdAllocator::with_next(10);
        alloc.ensure_above(EntityId(3));
        assert_eq!(alloc.peek_next(), 10);
        alloc.ensure_above(EntityId(10));
        assert_eq!(alloc.peek_next(), 11);
    }

    #[test]
    fn with_next_clamps_below_one() {
        let mut alloc = IdAllocator::with_next(0);
        assert_ne!(alloc.allocate(), EntityId::INVALID);
    }
}

//! Status codes for every fallible operation.
//!
//! The `VimStatus` codes described in docs/ARCHITECTURE.md §§1, 8, 9.
//! The FFI layer mirrors this enum as a `#[repr(C)]` enum; keep the
//! discriminants in sync (append-only — never renumber).

/// Result status for VIM Design operations. `Ok` is always 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum VimStatus {
    Ok = 0,
    NullArgument = 1,
    InvalidHandle = 2,
    /// A referenced entity id does not exist in the document.
    EntityNotFound = 3,
    /// Deletion rejected: other entities depend on the target
    /// (docs/ARCHITECTURE.md §3.2 — reject-if-dependents). The dependent
    /// list is queryable via `Document::dependents`.
    HasDependents = 4,
    /// A wiring change would make the dependency graph cyclic.
    WouldCreateCycle = 5,
    /// Serialized data has an unknown magic or a newer format version.
    UnsupportedVersion = 6,
    InternalPanic = 7,
    /// Serialized payload failed to decode or violates graph invariants.
    MalformedData = 8,
    /// An entity wired into a slot has a kind the slot does not accept.
    SlotKindMismatch = 9,
    /// Slot index out of range for the entity kind's slot table.
    SlotIndexOutOfRange = 10,
    /// Single value supplied for a multi slot or vice versa, or the
    /// record's input vector length does not match its slot table.
    SlotShapeMismatch = 11,
    /// A required slot was left empty.
    MissingRequiredSlot = 12,
    /// `SetParams` attempted to change an entity's kind, or params of the
    /// wrong variant were supplied for the entity kind.
    ParamsKindMismatch = 13,
    /// A delta's recorded prior state does not match the document (stale
    /// delta); applying it would make mechanical inversion unsound.
    DeltaMismatch = 14,
    /// `Insert` with an id that already exists.
    DuplicateEntityId = 15,
    /// The entity exists but has a different kind than the command targets
    /// (e.g. `DeleteLine` on a `Circle` id).
    WrongEntityKind = 16,
    NothingToUndo = 17,
    NothingToRedo = 18,
    /// Command arguments are structurally invalid (e.g. a composite update
    /// aimed at a subgraph that no longer has the expected shape).
    InvalidCommand = 19,
    /// Internal serialization failure (should not occur; never a panic).
    SerializationFailed = 20,
    /// Creating a second instance of a singleton entity kind (`Site`) —
    /// enforced at the delta gate, so composites and speculative apply
    /// cannot smuggle one in (docs/AUTHORING.md §1).
    SingletonExists = 21,
    /// A sketch fails structural validation (duplicate ids, a loop that
    /// references a missing point or has fewer than three distinct
    /// points, a non-finite coordinate, a thickness or depth that is not
    /// finite and positive). `sketch::validate_structure` names the
    /// exact problem.
    InvalidSketch = 22,
    /// A wall fails structural validation (a zero-length or non-finite
    /// reference line, a height that is not finite and positive, a
    /// non-finite top offset, an invalid profile, or a top-anchored
    /// point that is not in the profile). `wall::validate_structure`
    /// names the exact problem.
    InvalidWall = 23,
}

impl VimStatus {
    pub fn is_ok(self) -> bool {
        self == VimStatus::Ok
    }
}

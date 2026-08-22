//! Status codes for every fallible operation.
//!
//! Placeholder subset of the `VimStatus` codes described in
//! docs/ARCHITECTURE.md §§1, 8, 9. The FFI layer mirrors this enum as a
//! `#[repr(C)]` enum; keep the discriminants in sync.

/// Result status for VIM Design operations. `Ok` is always 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum VimStatus {
    Ok = 0,
    NullArgument = 1,
    InvalidHandle = 2,
    EntityNotFound = 3,
    HasDependents = 4,
    WouldCreateCycle = 5,
    UnsupportedVersion = 6,
    InternalPanic = 7,
}

impl VimStatus {
    pub fn is_ok(self) -> bool {
        self == VimStatus::Ok
    }
}

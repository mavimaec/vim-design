//! VIM Design core library — skeleton.
//!
//! This crate intentionally contains only build-system scaffolding:
//! entity id allocation, a placeholder [`Document`], and the [`VimStatus`]
//! error enum. The entity/command/dependency-graph design is happening in
//! parallel (see docs/ARCHITECTURE.md) and will replace these placeholders.

pub mod document;
pub mod id;
pub mod kernel;
pub mod status;

pub use document::Document;
pub use id::{EntityId, IdAllocator};
pub use status::VimStatus;

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

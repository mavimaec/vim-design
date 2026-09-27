//! The authoring layer shared by the web apps: pure Rust (compiled and
//! unit-tested natively as well as on wasm) — document operations,
//! element-model derivation, polygon validation, snapping, and the
//! sketch state machine. The wasm-only apps (`author`, `demo`) drive it
//! from browser input.

pub mod geom;
pub mod model;
pub mod ops;
pub mod sketch;
pub mod snap;

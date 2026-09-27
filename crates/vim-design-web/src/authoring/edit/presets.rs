//! Opening presets for walls: a window or a door placed with one tap
//! in Openings mode (`authoring::openings`). The user sizes them
//! afterwards; these are the starting dimensions (meters), and the one
//! place they are defined (the page reads them through
//! `AuthorApp::presets_json`).

pub const WINDOW_WIDTH_M: f64 = 1.2;
pub const WINDOW_HEIGHT_M: f64 = 1.2;
/// Height of a window's bottom edge above the wall base.
pub const WINDOW_SILL_M: f64 = 0.9;
pub const DOOR_WIDTH_M: f64 = 0.9;
pub const DOOR_HEIGHT_M: f64 = 2.1;
/// A door void starts this far below the wall base, so it cuts the wall's
/// bottom edge instead of leaving a sliver of wall under it.
pub const DOOR_BELOW_BASE_M: f64 = 0.05;

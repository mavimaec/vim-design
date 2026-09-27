//! Opening presets for wall profiles: a window or a door placed with one
//! tap. Both are through voids in the wall's elevation profile (u along
//! the wall, v up from its base); the user edits their points like any
//! other face afterwards.

use crate::authoring::geom::P2;
use crate::authoring::snap::grid_round;

pub const WINDOW_WIDTH_M: f64 = 1.2;
pub const WINDOW_HEIGHT_M: f64 = 1.2;
/// Height of a window's bottom edge above the wall base.
pub const WINDOW_SILL_M: f64 = 0.9;
pub const DOOR_WIDTH_M: f64 = 0.9;
pub const DOOR_HEIGHT_M: f64 = 2.1;
/// A door void starts this far below the wall base, so it cuts the wall's
/// bottom edge instead of leaving a sliver of wall under it.
pub const DOOR_BELOW_BASE_M: f64 = 0.05;
/// Grid the preset's centre snaps to along the wall.
pub const PRESET_SNAP_STEP_M: f64 = 0.1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Opening {
    Window,
    Door,
}

/// The preset outline centred at `u` along the wall (counter-clockwise).
pub fn preset_outline(kind: Opening, u: f64) -> Vec<P2> {
    let u = grid_round(u, PRESET_SNAP_STEP_M);
    let (w, v0, v1) = match kind {
        Opening::Window => (WINDOW_WIDTH_M, WINDOW_SILL_M, WINDOW_SILL_M + WINDOW_HEIGHT_M),
        Opening::Door => (DOOR_WIDTH_M, -DOOR_BELOW_BASE_M, DOOR_HEIGHT_M),
    };
    let (u0, u1) = (u - w / 2.0, u + w / 2.0);
    vec![[u0, v0], [u1, v0], [u1, v1], [u0, v1]]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_and_door_presets() {
        let w = preset_outline(Opening::Window, 2.04);
        assert_eq!(w, vec![[1.4, 0.9], [2.6, 0.9], [2.6, 2.1], [1.4, 2.1]]);
        let d = preset_outline(Opening::Door, 1.0);
        assert!(d.iter().any(|p| p[1] < 0.0), "the door cuts the bottom edge");
        assert!((d[1][0] - d[0][0] - DOOR_WIDTH_M).abs() < 1e-12);
        assert!((d[2][1] - DOOR_HEIGHT_M).abs() < 1e-12);
    }
}

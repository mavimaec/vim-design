//! Openings of a wall — windows and doors — as the Openings mode edits
//! them: a rectangle along the wall (offset from the wall's start to its
//! left edge, sill, width, height) that goes through or is a niche.
//!
//! Modeled on the library's structured `Opening` (coming with
//! `WallRun`); until then an opening is stored as a rectangular void face
//! of its wall's profile (a door's void reaches below the base so it
//! cuts the bottom edge), and [`openings_of`] reads them back.

use vim_design_lib::sketch::{Sketch, SketchFaceKind, face_polygon};

use super::edit::presets::{DOOR_BELOW_BASE_M, DOOR_HEIGHT_M, DOOR_WIDTH_M, WINDOW_HEIGHT_M, WINDOW_SILL_M, WINDOW_WIDTH_M};
use super::geom::P2;
use super::walls::WINDOW_MARGIN_M;

/// Openings snap to this grid (offset, sill, and sizes): steps per meter
/// (dividing by it keeps grid values exact decimals: 1.2, not 1.2000…02).
pub const OPENING_SNAP_PER_M: f64 = 10.0;

fn snap(v: f64) -> f64 {
    (v * OPENING_SNAP_PER_M).round() / OPENING_SNAP_PER_M
}
/// Clearance of an opening from each end of its wall, beyond the wall
/// thickness (the corner block of a join).
pub const OPENING_END_CLEARANCE_M: f64 = 0.1;
/// Smallest opening side and lowest window sill (meters).
pub const MIN_OPENING_SIZE_M: f64 = 0.2;
pub const MIN_SILL_M: f64 = 0.1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpeningKind {
    Window,
    Door,
}

impl OpeningKind {
    pub fn name(self) -> &'static str {
        match self {
            OpeningKind::Window => "window",
            OpeningKind::Door => "door",
        }
    }
}

/// One opening in wall-local meters (u along the wall from its start,
/// v up from its base).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OpeningRect {
    /// The void face storing it.
    pub face: u32,
    pub kind: OpeningKind,
    /// From the wall's start to the opening's left edge.
    pub offset: f64,
    /// Bottom above the base (0 for a door).
    pub sill: f64,
    pub width: f64,
    pub height: f64,
    /// `None`: through the wall; `Some(d)`: a niche `d` deep.
    pub depth: Option<f64>,
}

impl OpeningRect {
    /// The preset of `kind` centred at `u` along the wall.
    pub fn preset(kind: OpeningKind, u: f64) -> Self {
        let (width, height, sill) = match kind {
            OpeningKind::Window => (WINDOW_WIDTH_M, WINDOW_HEIGHT_M, WINDOW_SILL_M),
            OpeningKind::Door => (DOOR_WIDTH_M, DOOR_HEIGHT_M, 0.0),
        };
        Self { face: 0, kind, offset: u - width / 2.0, sill, width, height, depth: None }
    }

    /// The void's outline (counter-clockwise); a door reaches below the
    /// base.
    pub fn outline(&self) -> Vec<P2> {
        let v0 = match self.kind {
            OpeningKind::Window => self.sill,
            OpeningKind::Door => -DOOR_BELOW_BASE_M,
        };
        let (u0, u1, v1) = (self.offset, self.offset + self.width, self.sill + self.height);
        vec![[u0, v0], [u1, v0], [u1, v1], [u0, v1]]
    }

    /// The rectangle as the user sees it (a door from the base up).
    pub fn visible(&self) -> [P2; 2] {
        [[self.offset, self.sill], [self.offset + self.width, self.sill + self.height]]
    }
}

/// The wall an opening goes into: length, top reference height, and
/// thickness.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WallSpan {
    pub length: f64,
    pub height: f64,
    pub thickness: f64,
}

/// Snap an opening to the grid and keep it on its wall: clear of both
/// ends (the corner blocks), a window above the minimum sill and under
/// the top, a door on the floor. Refused when it cannot fit.
pub fn fit(o: OpeningRect, span: WallSpan) -> Result<OpeningRect, &'static str> {
    let clear = span.thickness + OPENING_END_CLEARANCE_M;
    let mut o = o;
    o.width = snap(o.width.max(MIN_OPENING_SIZE_M));
    o.height = snap(o.height.max(MIN_OPENING_SIZE_M));
    let room = span.length - 2.0 * clear;
    if o.width > room + 1e-9 {
        return Err("The wall is too short for this opening");
    }
    o.offset = snap(o.offset).clamp(clear, span.length - clear - o.width);
    let top = span.height - WINDOW_MARGIN_M;
    match o.kind {
        OpeningKind::Door => o.sill = 0.0,
        OpeningKind::Window => {
            o.sill = snap(o.sill).max(MIN_SILL_M);
            if o.sill + o.height > top {
                o.sill = (top - o.height).max(MIN_SILL_M);
            }
        }
    }
    if o.sill + o.height > top + 1e-9 {
        return Err("The wall is not tall enough for this opening");
    }
    Ok(o)
}

/// The openings stored in a wall's EFFECTIVE profile: its void faces
/// that are axis-aligned rectangles (other voids are shapes, not
/// openings). A void reaching below the base is a door.
pub fn openings_of(effective: &Sketch) -> Vec<OpeningRect> {
    effective
        .faces
        .iter()
        .filter_map(|f| {
            let SketchFaceKind::Void { depth } = f.kind else { return None };
            let poly = face_polygon(effective, f.id).ok()?;
            if poly.len() != 4 {
                return None;
            }
            let (u0, u1) = poly.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), p| (a.min(p[0]), b.max(p[0])));
            let (v0, v1) = poly.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), p| (a.min(p[1]), b.max(p[1])));
            let on_box = poly.iter().all(|p| {
                ((p[0] - u0).abs() < 1e-9 || (p[0] - u1).abs() < 1e-9) && ((p[1] - v0).abs() < 1e-9 || (p[1] - v1).abs() < 1e-9)
            });
            if !on_box || u1 - u0 < 1e-6 || v1 - v0 < 1e-6 {
                return None;
            }
            // Read back to the nanometre (differences of stored
            // coordinates carry float noise: 1.2000000000000002).
            let clean = |x: f64| (x * 1e9).round() / 1e9;
            let (u0, u1, v0, v1) = (clean(u0), clean(u1), clean(v0), clean(v1));
            let door = v0 < 0.0;
            let sill = if door { 0.0 } else { v0 };
            Some(OpeningRect {
                face: f.id,
                kind: if door { OpeningKind::Door } else { OpeningKind::Window },
                offset: u0,
                sill,
                width: clean(u1 - u0),
                height: clean(v1 - sill),
                depth,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPAN: WallSpan = WallSpan { length: 5.0, height: 2.7, thickness: 0.2 };

    #[test]
    fn presets_fit_snap_and_clear_the_ends() {
        let w = fit(OpeningRect::preset(OpeningKind::Window, 2.53), SPAN).expect("window");
        assert_eq!((w.offset, w.sill, w.width, w.height), (1.9, 0.9, 1.2, 1.2));
        // Near the start: pushed clear of the corner block.
        let near = fit(OpeningRect::preset(OpeningKind::Window, 0.1), SPAN).expect("near");
        assert!((near.offset - 0.3).abs() < 1e-9);
        // A door stays on the floor and its void cuts the bottom edge.
        let mut d = OpeningRect::preset(OpeningKind::Door, 4.0);
        d.sill = 0.7;
        let d = fit(d, SPAN).expect("door");
        assert_eq!(d.sill, 0.0);
        assert!(d.outline().iter().any(|p| p[1] < 0.0));
        // A window pushed above the top comes back under it.
        let mut hi = OpeningRect::preset(OpeningKind::Window, 2.5);
        hi.sill = 2.0;
        assert!((fit(hi, SPAN).expect("high").sill - 1.45).abs() < 1e-9);
        // Too wide for the wall.
        let mut wide = OpeningRect::preset(OpeningKind::Window, 2.5);
        wide.width = 4.8;
        assert!(fit(wide, SPAN).is_err());
    }

    #[test]
    fn rectangular_voids_read_back_as_openings() {
        let mut s = Sketch::default();
        let solid = [[0.0, 0.0], [5.0, 0.0], [5.0, 2.7], [0.0, 2.7]];
        s = vim_design_lib::sketch::ops::add_face(&s, &solid, SketchFaceKind::Solid { thickness: 0.2 }).expect("solid");
        let w = fit(OpeningRect::preset(OpeningKind::Window, 1.5), SPAN).expect("w");
        s = vim_design_lib::sketch::ops::add_face(&s, &w.outline(), SketchFaceKind::Void { depth: None }).expect("w");
        let d = fit(OpeningRect::preset(OpeningKind::Door, 3.8), SPAN).expect("d");
        s = vim_design_lib::sketch::ops::add_face(&s, &d.outline(), SketchFaceKind::Void { depth: Some(0.1) }).expect("d");
        let tri = [[4.0, 1.0], [4.5, 1.0], [4.2, 1.5]];
        s = vim_design_lib::sketch::ops::add_face(&s, &tri, SketchFaceKind::Void { depth: None }).expect("tri");
        let o = openings_of(&s);
        assert_eq!(o.len(), 2, "the triangle is a shape, not an opening");
        assert_eq!((o[0].kind, o[0].offset, o[0].sill), (OpeningKind::Window, w.offset, 0.9));
        assert_eq!((o[1].kind, o[1].sill, o[1].depth), (OpeningKind::Door, 0.0, Some(0.1)));
        assert!((o[1].height - DOOR_HEIGHT_M).abs() < 1e-9);
    }
}

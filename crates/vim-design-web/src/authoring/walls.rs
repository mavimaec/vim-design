//! Wall construction from drawn reference lines.
//!
//! A wall run is a polyline of reference points on a level. The drawn
//! line is one FACE of the wall; the thickness grows to the LEFT of the
//! drawing direction (to the right with `flip`). A closed loop is first
//! normalized counter-clockwise, so an unflipped loop grows inward —
//! tracing a floor plate's edge puts the walls on the plate.
//!
//! Each segment becomes one library `Wall` (see `ops::commit_walls`):
//! its reference line and an elevation profile, where windows and doors
//! are void faces.
//!
//! Butt joins at corners, decided per corner by the turn direction
//! relative to the thickness side:
//! - convex corner (turning toward the thickness side): the NEXT
//!   segment's start is trimmed by the thickness, so the previous wall
//!   owns the corner block;
//! - reflex corner (turning away): the PREVIOUS segment's end is
//!   extended by the thickness, filling the gap outside the corner.
//!
//! Exact at 90°; at other angles the blocks overlap or leave a sliver
//! (a mitred join needs non-rectangular profiles).

use super::geom::{
    self, EPS, Invalid, P2, dist, normalized_ccw, self_intersects, validate_outline,
};

/// Default wall thickness: an interior partition — a 2x4 wood stud (89
/// mm actual) with one 12.7 mm (1/2") gypsum board on each face: 89 + 2
/// × 12.7 = 114.4 mm. New walls and room walls start with it; existing
/// walls keep their stored thickness.
pub const PARTITION_THICKNESS_M: f64 = 0.114;

/// Shortest wall segment accepted after joins are applied (meters).
pub const MIN_WALL_LENGTH_M: f64 = 0.05;
/// Clearance a wall keeps above its highest opening when its height is
/// lowered (meters).
pub const WINDOW_MARGIN_M: f64 = 0.05;

/// One wall: its reference line (after joins) and thickness direction.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WallSeg {
    pub start: P2,
    pub end: P2,
    /// Unit vector from the reference face into the wall body.
    pub normal: P2,
}

impl WallSeg {
    pub fn length(&self) -> f64 {
        dist(self.start, self.end)
    }

    /// Footprint polygon (counter-clockwise or clockwise, 4 points).
    pub fn footprint(&self, thickness: f64) -> [P2; 4] {
        let o = |p: P2| [p[0] + self.normal[0] * thickness, p[1] + self.normal[1] * thickness];
        [self.start, self.end, o(self.end), o(self.start)]
    }
}

fn unit(a: P2, b: P2) -> Option<P2> {
    let l = dist(a, b);
    (l > EPS).then(|| [(b[0] - a[0]) / l, (b[1] - a[1]) / l])
}

/// Left normal of a direction.
pub fn left(d: P2) -> P2 {
    [-d[1], d[0]]
}

/// The run as reference points: a closed loop is normalized CCW.
pub fn run_points(points: &[P2], closed: bool) -> Vec<P2> {
    let pts = geom::dedup_closed(points);
    if closed { normalized_ccw(&pts) } else { pts }
}

/// Validate a run and compute its walls with butt joins.
pub fn wall_segments(
    points: &[P2],
    closed: bool,
    thickness: f64,
    flip: bool,
) -> Result<Vec<WallSeg>, Invalid> {
    let pts = run_points(points, closed);
    if closed {
        validate_outline(&pts)?;
    } else {
        if pts.len() < 2 {
            return Err(Invalid::TooFewWallPoints);
        }
        if self_intersects(&pts, false) {
            return Err(Invalid::SelfIntersecting);
        }
    }
    let n = pts.len();
    let count = if closed { n } else { n - 1 };
    let dirs: Vec<P2> = (0..count)
        .map(|i| unit(pts[i], pts[(i + 1) % n]).ok_or(Invalid::WallTooShort))
        .collect::<Result<_, _>>()?;
    let side = if flip { -1.0 } else { 1.0 };
    let mut trim_start = vec![0.0; count];
    let mut extend_end = vec![0.0; count];
    // Corner j joins segment j-1 (previous) and segment j (next).
    let corners: Vec<usize> = if closed { (0..count).collect() } else { (1..count).collect() };
    for j in corners {
        let prev = (j + count - 1) % count;
        let (dp, dn) = (dirs[prev], dirs[j]);
        let turn = dp[0] * dn[1] - dp[1] * dn[0];
        if turn.abs() < 1e-9 {
            continue; // straight continuation: no join needed
        }
        if turn * side > 0.0 {
            trim_start[j] += thickness;
        } else {
            extend_end[prev] += thickness;
        }
    }
    (0..count)
        .map(|i| {
            let (a, b, d) = (pts[i], pts[(i + 1) % n], dirs[i]);
            let start = [a[0] + d[0] * trim_start[i], a[1] + d[1] * trim_start[i]];
            let end = [b[0] + d[0] * extend_end[i], b[1] + d[1] * extend_end[i]];
            let seg = WallSeg {
                start,
                end,
                normal: [left(d)[0] * side, left(d)[1] * side],
            };
            if dist(a, b) - trim_start[i] + extend_end[i] < MIN_WALL_LENGTH_M {
                Err(Invalid::WallTooShort)
            } else {
                Ok(seg)
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: P2, b: P2) -> bool {
        dist(a, b) < 1e-9
    }

    #[test]
    fn closed_loop_grows_inward_with_convex_trims() {
        // Drawn clockwise on purpose: normalized CCW, thickness inward.
        let square = [[0.0, 0.0], [0.0, 3.0], [4.0, 3.0], [4.0, 0.0]];
        let segs = wall_segments(&square, true, 0.2, false).expect("valid loop");
        assert_eq!(segs.len(), 4);
        // CCW order starts at (4,0)? Find the bottom wall (y = 0).
        let bottom = segs.iter().find(|s| s.start[1] == 0.0 && s.end[1] == 0.0).expect("bottom");
        assert!(close(bottom.normal, [0.0, 1.0]), "inward = +y");
        // Every corner is convex: each wall is trimmed by t at its start
        // and runs full length to its end.
        for s in &segs {
            assert!((s.length() - (if s.start[1] == s.end[1] { 3.8 } else { 2.8 })).abs() < 1e-9, "{s:?}");
        }
        // Footprints do not overlap: total area = perimeter band.
        let area: f64 = segs.iter().map(|s| s.length() * 0.2).sum();
        assert!((area - (4.0 * 3.0 - 3.6 * 2.6)).abs() < 1e-9);
    }

    #[test]
    fn flip_grows_outward() {
        let square = [[0.0, 0.0], [4.0, 0.0], [4.0, 3.0], [0.0, 3.0]];
        let segs = wall_segments(&square, true, 0.2, true).expect("valid");
        let bottom = segs.iter().find(|s| s.start[1] == 0.0).expect("bottom");
        assert!(close(bottom.normal, [0.0, -1.0]));
        let area: f64 = segs.iter().map(|s| s.length() * 0.2).sum();
        assert!((area - (4.4 * 3.4 - 4.0 * 3.0)).abs() < 1e-9, "outer band, no overlap");
    }

    #[test]
    fn open_run_convex_and_reflex_joins() {
        // East then north: a LEFT turn with the body on the left = convex.
        let run = [[0.0, 0.0], [4.0, 0.0], [4.0, 2.0]];
        let segs = wall_segments(&run, false, 0.2, false).expect("valid");
        assert!(close(segs[0].start, [0.0, 0.0]) && close(segs[0].end, [4.0, 0.0]));
        assert!(close(segs[1].start, [4.0, 0.2]), "next trimmed at the convex corner");
        // East then south: a RIGHT turn with the body on the left = reflex.
        let run = [[0.0, 0.0], [4.0, 0.0], [4.0, -2.0]];
        let segs = wall_segments(&run, false, 0.2, false).expect("valid");
        assert!(close(segs[0].end, [4.2, 0.0]), "previous extended at the reflex corner");
        assert!(close(segs[1].start, [4.0, 0.0]));
    }

    #[test]
    fn rejections() {
        assert_eq!(wall_segments(&[[0.0, 0.0]], false, 0.2, false), Err(Invalid::TooFewWallPoints));
        let short = [[0.0, 0.0], [4.0, 0.0], [4.0, 0.1]];
        assert_eq!(wall_segments(&short, false, 0.2, false), Err(Invalid::WallTooShort));
        let crossing = [[0.0, 0.0], [4.0, 0.0], [2.0, 2.0], [2.0, -2.0]];
        assert_eq!(wall_segments(&crossing, false, 0.2, false), Err(Invalid::SelfIntersecting));
    }
}

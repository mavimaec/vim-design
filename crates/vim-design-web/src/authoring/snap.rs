//! Coordinate snapping for sketching on a construction plane.
//!
//! Priority (highest first):
//! 1. the sketch's FIRST vertex (closing the loop) — always active, even
//!    with snapping switched off, because it is how a loop is closed;
//! 2. existing vertices of elements on the active plane (plate and hole
//!    corners) — lets new work align with old work;
//! 3. axis alignment with the PREVIOUS vertex and with the FIRST vertex
//!    (horizontal/vertical lock; the free coordinate still snaps to the
//!    grid, and a lock from each anchor can combine into an
//!    intersection);
//! 4. the grid (absolute multiples of the step).
//!
//! Tolerances arrive in meters: the caller converts its screen-pixel
//! tolerance (larger for touch than for mouse) with the current zoom, so
//! snapping feels the same at every zoom level.

use super::geom::{P2, dist};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapKind {
    /// Snapping off: the raw pointer position.
    Free,
    Grid,
    /// Horizontal/vertical lock with the previous and/or first vertex.
    Axis,
    /// An existing vertex of another element.
    Vertex,
    /// A point on an existing edge (plate outline, wall face line).
    Edge,
    /// The sketch's first vertex — placing here closes the loop.
    First,
}

impl SnapKind {
    pub fn name(self) -> &'static str {
        match self {
            SnapKind::Free => "free",
            SnapKind::Grid => "grid",
            SnapKind::Axis => "axis",
            SnapKind::Vertex => "vertex",
            SnapKind::Edge => "edge",
            SnapKind::First => "first",
        }
    }
}

pub struct SnapInput<'a> {
    pub raw: P2,
    pub enabled: bool,
    /// Grid step (meters).
    pub step: f64,
    /// Capture radius (meters) for vertices and axis locks.
    pub tolerance: f64,
    /// First vertex of the sketch, when closing is allowed.
    pub close_target: Option<P2>,
    /// Axis anchors: previous vertex and first vertex (either may be
    /// absent; duplicates are ignored).
    pub prev: Option<P2>,
    pub first: Option<P2>,
    /// Existing element vertices on the active plane.
    pub vertices: &'a [P2],
    /// Existing edges to snap onto (after vertices, before axis locks).
    pub edges: &'a [(P2, P2)],
    /// Extra axis anchors after `prev`/`first` (e.g. other windows'
    /// corners, so sills and heads line up).
    pub align: &'a [P2],
}

#[derive(Debug, Clone, PartialEq)]
pub struct SnapResult {
    pub point: P2,
    pub kind: SnapKind,
    /// Alignment guide segments (anchor -> snapped point) to draw dashed.
    pub guides: Vec<(P2, P2)>,
}

pub fn grid_round(v: f64, step: f64) -> f64 {
    if step <= 0.0 {
        return v;
    }
    let r = (v / step).round() * step;
    // Normalize -0.0 and float noise (0.30000000000000004 -> 0.3).
    let r = (r * 1e6).round() / 1e6;
    if r == 0.0 { 0.0 } else { r }
}

/// Nearest point on an edge within the tolerance. On an axis-aligned
/// edge the along-edge coordinate still snaps to the grid (clamped to
/// the edge), so tracing a plate edge gives round lengths.
fn snap_to_edges(raw: P2, input: &SnapInput) -> Option<P2> {
    let mut best: Option<(f64, P2)> = None;
    for (a, b) in input.edges {
        let ab = [b[0] - a[0], b[1] - a[1]];
        let len2 = ab[0] * ab[0] + ab[1] * ab[1];
        if len2 <= 1e-12 {
            continue;
        }
        let t = (((raw[0] - a[0]) * ab[0] + (raw[1] - a[1]) * ab[1]) / len2).clamp(0.0, 1.0);
        let mut p = [a[0] + ab[0] * t, a[1] + ab[1] * t];
        let d = dist(raw, p);
        if d > input.tolerance || best.is_some_and(|(bd, _)| bd <= d) {
            continue;
        }
        if ab[1].abs() < 1e-9 {
            p[0] = grid_round(raw[0], input.step).clamp(a[0].min(b[0]), a[0].max(b[0]));
        } else if ab[0].abs() < 1e-9 {
            p[1] = grid_round(raw[1], input.step).clamp(a[1].min(b[1]), a[1].max(b[1]));
        }
        best = Some((d, p));
    }
    best.map(|(_, p)| p)
}

pub fn snap(input: &SnapInput) -> SnapResult {
    let raw = input.raw;
    if let Some(first) = input.close_target {
        if dist(raw, first) <= input.tolerance {
            return SnapResult { point: first, kind: SnapKind::First, guides: vec![] };
        }
    }
    if !input.enabled {
        return SnapResult { point: raw, kind: SnapKind::Free, guides: vec![] };
    }
    if let Some(v) = input
        .vertices
        .iter()
        .filter(|v| dist(raw, **v) <= input.tolerance)
        .min_by(|a, b| dist(raw, **a).total_cmp(&dist(raw, **b)))
    {
        return SnapResult { point: *v, kind: SnapKind::Vertex, guides: vec![] };
    }
    if let Some(p) = snap_to_edges(raw, input) {
        return SnapResult { point: p, kind: SnapKind::Edge, guides: vec![] };
    }

    let grid = [grid_round(raw[0], input.step), grid_round(raw[1], input.step)];
    // Candidate locks: (coordinate index fixed, value, anchor, deviation).
    let mut lock_x: Option<(f64, P2, f64)> = None; // vertical line x = anchor.x
    let mut lock_y: Option<(f64, P2, f64)> = None; // horizontal line y = anchor.y
    let mut anchors: Vec<P2> = Vec::with_capacity(2);
    for a in [input.prev, input.first]
        .into_iter()
        .flatten()
        .chain(input.align.iter().copied())
    {
        if anchors.iter().all(|b| dist(*b, a) > 1e-9) {
            anchors.push(a);
        }
    }
    for a in &anchors {
        let dx = (raw[0] - a[0]).abs();
        let dy = (raw[1] - a[1]).abs();
        if dx <= input.tolerance && lock_x.is_none_or(|(_, _, d)| dx < d) {
            lock_x = Some((a[0], *a, dx));
        }
        if dy <= input.tolerance && lock_y.is_none_or(|(_, _, d)| dy < d) {
            lock_y = Some((a[1], *a, dy));
        }
    }
    // Both locks from the same anchor would collapse onto the anchor
    // itself: keep only the tighter one.
    if let (Some((_, ax, dx)), Some((_, ay, dy))) = (lock_x, lock_y) {
        if dist(ax, ay) <= 1e-9 {
            if dx <= dy {
                lock_y = None;
            } else {
                lock_x = None;
            }
        }
    }
    if lock_x.is_none() && lock_y.is_none() {
        return SnapResult { point: grid, kind: SnapKind::Grid, guides: vec![] };
    }
    let point = [
        lock_x.map_or(grid[0], |(x, _, _)| x),
        lock_y.map_or(grid[1], |(y, _, _)| y),
    ];
    let guides = [lock_x, lock_y]
        .into_iter()
        .flatten()
        .map(|(_, anchor, _)| (anchor, point))
        .collect();
    SnapResult { point, kind: SnapKind::Axis, guides }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(raw: P2) -> SnapInput<'static> {
        SnapInput {
            raw,
            enabled: true,
            step: 0.5,
            tolerance: 0.2,
            close_target: None,
            prev: None,
            first: None,
            vertices: &[],
            edges: &[],
            align: &[],
        }
    }

    #[test]
    fn grid_rounding() {
        let r = snap(&input([1.26, -0.74]));
        assert_eq!(r.point, [1.5, -0.5]);
        assert_eq!(r.kind, SnapKind::Grid);
        assert_eq!(grid_round(0.1 * 3.0, 0.1), 0.3);
        assert_eq!(grid_round(-0.01, 0.5), 0.0);
    }

    #[test]
    fn priority_first_then_vertex_then_axis() {
        let verts = [[1.1, 1.1]];
        let mut i = input([1.05, 1.0]);
        i.vertices = &verts;
        i.close_target = Some([1.0, 1.0]);
        assert_eq!(snap(&i).kind, SnapKind::First);
        i.close_target = None;
        let r = snap(&i);
        assert_eq!((r.kind, r.point), (SnapKind::Vertex, [1.1, 1.1]));
        i.vertices = &[];
        i.prev = Some([0.0, 1.13]);
        let r = snap(&i);
        assert_eq!(r.kind, SnapKind::Axis);
        assert_eq!(r.point, [1.0, 1.13], "y locked to prev, x on grid");
        assert_eq!(r.guides.len(), 1);
    }

    #[test]
    fn locks_from_prev_and_first_combine() {
        let mut i = input([3.1, 2.1]);
        i.prev = Some([3.03, 0.0]); // vertical from prev
        i.first = Some([0.0, 2.07]); // horizontal from first
        let r = snap(&i);
        assert_eq!(r.point, [3.03, 2.07]);
        assert_eq!(r.guides.len(), 2);
    }

    #[test]
    fn disabled_is_free_but_still_closes() {
        let mut i = input([1.26, 0.74]);
        i.enabled = false;
        assert_eq!(snap(&i).kind, SnapKind::Free);
        assert_eq!(snap(&i).point, [1.26, 0.74]);
        i.close_target = Some([1.3, 0.7]);
        assert_eq!(snap(&i).kind, SnapKind::First);
    }

    #[test]
    fn edges_capture_with_grid_along_axis_aligned_edges() {
        let edges = [([0.0, 0.0], [6.0, 0.0]), ([6.0, 0.0], [6.0, 4.0])];
        let mut i = input([2.37, 0.12]);
        i.edges = &edges;
        let r = snap(&i);
        assert_eq!((r.kind, r.point), (SnapKind::Edge, [2.5, 0.0]));
        let slanted = [([0.0, 0.0], [4.0, 4.0])];
        i.edges = &slanted;
        i.raw = [2.1, 1.9];
        let r = snap(&i);
        assert_eq!(r.kind, SnapKind::Edge);
        assert!((r.point[0] - 2.0).abs() < 1e-9 && (r.point[1] - 2.0).abs() < 1e-9);
    }

    #[test]
    fn align_anchors_lock_like_prev() {
        let align = [[5.0, 0.9]];
        let mut i = input([1.07, 0.97]);
        i.align = &align;
        let r = snap(&i);
        assert_eq!((r.kind, r.point), (SnapKind::Axis, [1.0, 0.9]));
    }
}

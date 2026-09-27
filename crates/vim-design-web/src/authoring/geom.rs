//! Planar polygon helpers for the authoring app: winding, area, and the
//! validation rules the UI enforces before anything reaches the
//! document.
//!
//! Why app-side: the kernel triangulates self-intersecting (bowtie)
//! outlines and boundary-crossing holes without complaint (probed
//! 2026-08-23) — they produce valid-but-wrong meshes. Rejecting them is
//! therefore the intent layer's job, and it happens here, live, while
//! the user draws.
//!
//! All coordinates are (u, v) meters on one construction plane.

/// A point on the construction plane (u, v), meters.
pub type P2 = [f64; 2];

/// Geometric tolerance for coincidence/touching tests (meters). Coarser
/// than the kernel's 1 µm on purpose: two outlines closer than 1 mm are
/// "touching" for the purposes of the UI rules.
pub const EPS: f64 = 1e-3;

/// Why an outline cannot be committed. `message` is the short toast text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Invalid {
    TooFewPoints,
    ZeroArea,
    SelfIntersecting,
    /// Hole tool: the outline is not strictly inside any floor plate on
    /// the active level.
    HoleOutsidePlate,
    /// Hole tool: no floor plate exists on the active level at all.
    NoPlateOnLevel,
    /// Hole tool: the outline touches or overlaps an existing hole.
    HoleOverlapsHole,
}

impl Invalid {
    pub fn message(self) -> &'static str {
        match self {
            Invalid::TooFewPoints => "Place at least 3 points",
            Invalid::ZeroArea => "The outline has no area",
            Invalid::SelfIntersecting => "The outline crosses itself",
            Invalid::HoleOutsidePlate => "A hole must lie inside a floor plate",
            Invalid::NoPlateOnLevel => "Draw a floor plate on this level first",
            Invalid::HoleOverlapsHole => "Holes must not touch or overlap",
        }
    }

    pub fn code(self) -> &'static str {
        match self {
            Invalid::TooFewPoints => "too_few_points",
            Invalid::ZeroArea => "zero_area",
            Invalid::SelfIntersecting => "self_intersecting",
            Invalid::HoleOutsidePlate => "hole_outside_plate",
            Invalid::NoPlateOnLevel => "no_plate_on_level",
            Invalid::HoleOverlapsHole => "hole_overlaps_hole",
        }
    }
}

fn sub(a: P2, b: P2) -> P2 {
    [a[0] - b[0], a[1] - b[1]]
}

fn cross(a: P2, b: P2) -> f64 {
    a[0] * b[1] - a[1] * b[0]
}

fn dot(a: P2, b: P2) -> f64 {
    a[0] * b[0] + a[1] * b[1]
}

pub fn dist(a: P2, b: P2) -> f64 {
    (a[0] - b[0]).hypot(a[1] - b[1])
}

/// Signed area (shoelace). Positive = counter-clockwise.
pub fn signed_area(points: &[P2]) -> f64 {
    let n = points.len();
    if n < 3 {
        return 0.0;
    }
    let mut sum = 0.0;
    for i in 0..n {
        let a = points[i];
        let b = points[(i + 1) % n];
        sum += a[0] * b[1] - b[0] * a[1];
    }
    sum / 2.0
}

/// The outline with counter-clockwise winding (the orientation every
/// profile in this app is authored with).
pub fn normalized_ccw(points: &[P2]) -> Vec<P2> {
    let mut pts = points.to_vec();
    if signed_area(&pts) < 0.0 {
        pts.reverse();
    }
    pts
}

/// Remove consecutive duplicate points (closing duplicate included).
pub fn dedup_closed(points: &[P2]) -> Vec<P2> {
    let mut out: Vec<P2> = Vec::with_capacity(points.len());
    for p in points {
        if out.last().is_none_or(|q| dist(*q, *p) > EPS) {
            out.push(*p);
        }
    }
    while out.len() > 1 && out.first().zip(out.last()).is_some_and(|(a, b)| dist(*a, *b) <= EPS) {
        out.pop();
    }
    out
}

/// Distance from `p` to segment `ab`.
pub fn point_segment_distance(p: P2, a: P2, b: P2) -> f64 {
    let ab = sub(b, a);
    let len2 = dot(ab, ab);
    if len2 <= f64::EPSILON {
        return dist(p, a);
    }
    let t = (dot(sub(p, a), ab) / len2).clamp(0.0, 1.0);
    dist(p, [a[0] + ab[0] * t, a[1] + ab[1] * t])
}

/// True when segments `ab` and `cd` intersect or come within `EPS` of
/// each other (touching counts).
pub fn segments_touch(a: P2, b: P2, c: P2, d: P2) -> bool {
    let d1 = cross(sub(b, a), sub(c, a));
    let d2 = cross(sub(b, a), sub(d, a));
    let d3 = cross(sub(d, c), sub(a, c));
    let d4 = cross(sub(d, c), sub(b, c));
    if ((d1 > 0.0 && d2 < 0.0) || (d1 < 0.0 && d2 > 0.0))
        && ((d3 > 0.0 && d4 < 0.0) || (d3 < 0.0 && d4 > 0.0))
    {
        return true; // proper crossing
    }
    point_segment_distance(a, c, d) <= EPS
        || point_segment_distance(b, c, d) <= EPS
        || point_segment_distance(c, a, b) <= EPS
        || point_segment_distance(d, a, b) <= EPS
}

/// Self-intersection test for a polyline (`closed` adds the closing
/// edge). Non-adjacent edges must not touch; adjacent edges must not
/// fold back onto each other (a 180° spike).
pub fn self_intersects(points: &[P2], closed: bool) -> bool {
    let n = points.len();
    if n < 3 {
        // Two points: only a zero-length segment could be wrong, and
        // dedup handles that.
        return false;
    }
    let edge_count = if closed { n } else { n - 1 };
    let edge = |i: usize| (points[i], points[(i + 1) % n]);
    for i in 0..edge_count {
        for j in (i + 1)..edge_count {
            let adjacent = j == i + 1 || (closed && i == 0 && j == edge_count - 1);
            let (a, b) = edge(i);
            let (c, d) = edge(j);
            if adjacent {
                // Shared vertex: `b == c` (j = i + 1) or `a == d` (wrap).
                let (shared, p, q) = if j == i + 1 { (b, a, d) } else { (a, b, c) };
                let u = sub(p, shared);
                let v = sub(q, shared);
                let lu = dot(u, u).sqrt();
                let lv = dot(v, v).sqrt();
                if lu <= EPS || lv <= EPS {
                    continue;
                }
                // Fold-back: collinear and pointing the same way from
                // the shared vertex (the edges overlap).
                if cross(u, v).abs() <= EPS * lu.max(lv) && dot(u, v) > 0.0 {
                    return true;
                }
                // A triangle's non-shared endpoints can still touch the
                // other edge only through fold-back (handled above).
                continue;
            }
            if segments_touch(a, b, c, d) {
                return true;
            }
        }
    }
    false
}

/// Point strictly inside the polygon (even-odd rule; points on the
/// boundary count as outside).
pub fn point_in_polygon(p: P2, poly: &[P2]) -> bool {
    let n = poly.len();
    if n < 3 {
        return false;
    }
    for i in 0..n {
        if point_segment_distance(p, poly[i], poly[(i + 1) % n]) <= EPS {
            return false;
        }
    }
    let mut inside = false;
    let mut j = n - 1;
    for i in 0..n {
        let (pi, pj) = (poly[i], poly[j]);
        if (pi[1] > p[1]) != (pj[1] > p[1]) {
            let x = pj[0] + (p[1] - pj[1]) / (pi[1] - pj[1]) * (pi[0] - pj[0]);
            if p[0] < x {
                inside = !inside;
            }
        }
        j = i;
    }
    inside
}

fn edges_touch(a: &[P2], b: &[P2]) -> bool {
    let (na, nb) = (a.len(), b.len());
    for i in 0..na {
        for j in 0..nb {
            if segments_touch(a[i], a[(i + 1) % na], b[j], b[(j + 1) % nb]) {
                return true;
            }
        }
    }
    false
}

/// `inner` lies strictly inside `outer`: every vertex is interior and no
/// edges touch (so the boundaries keep at least `EPS` apart).
pub fn strictly_inside(inner: &[P2], outer: &[P2]) -> bool {
    !inner.is_empty()
        && inner.iter().all(|p| point_in_polygon(*p, outer))
        && !edges_touch(inner, outer)
}

/// The two polygons neither touch nor overlap (no edge contact, and
/// neither contains the other).
pub fn disjoint(a: &[P2], b: &[P2]) -> bool {
    if edges_touch(a, b) {
        return false;
    }
    let a_in_b = a.first().is_some_and(|p| point_in_polygon(*p, b));
    let b_in_a = b.first().is_some_and(|p| point_in_polygon(*p, a));
    !a_in_b && !b_in_a
}

/// Basic outline validity (count, area, simplicity). `points` should
/// already be deduplicated.
pub fn validate_outline(points: &[P2]) -> Result<(), Invalid> {
    if points.len() < 3 {
        return Err(Invalid::TooFewPoints);
    }
    if self_intersects(points, true) {
        return Err(Invalid::SelfIntersecting);
    }
    // After the simplicity check, a (near-)zero area means collinear.
    let perimeter: f64 = (0..points.len())
        .map(|i| dist(points[i], points[(i + 1) % points.len()]))
        .sum();
    if signed_area(points).abs() <= EPS * perimeter.max(1.0) {
        return Err(Invalid::ZeroArea);
    }
    Ok(())
}

/// Axis-aligned rectangle from two opposite corners, counter-clockwise.
pub fn rectangle(a: P2, b: P2) -> Vec<P2> {
    let (x0, x1) = (a[0].min(b[0]), a[0].max(b[0]));
    let (y0, y1) = (a[1].min(b[1]), a[1].max(b[1]));
    vec![[x0, y0], [x1, y0], [x1, y1], [x0, y1]]
}

#[cfg(test)]
mod tests {
    use super::*;

    const SQUARE: [P2; 4] = [[0.0, 0.0], [4.0, 0.0], [4.0, 4.0], [0.0, 4.0]];

    #[test]
    fn winding_and_area() {
        assert_eq!(signed_area(&SQUARE), 16.0);
        let cw: Vec<P2> = SQUARE.iter().rev().copied().collect();
        assert_eq!(signed_area(&cw), -16.0);
        assert_eq!(signed_area(&normalized_ccw(&cw)), 16.0);
    }

    #[test]
    fn valid_square_and_l_shape() {
        assert_eq!(validate_outline(&SQUARE), Ok(()));
        let l = [
            [0.0, 0.0],
            [4.0, 0.0],
            [4.0, 2.0],
            [2.0, 2.0],
            [2.0, 4.0],
            [0.0, 4.0],
        ];
        assert_eq!(validate_outline(&l), Ok(()));
    }

    #[test]
    fn rejects_bowtie_spike_and_collinear() {
        let bowtie = [[0.0, 0.0], [4.0, 4.0], [4.0, 0.0], [0.0, 4.0]];
        assert_eq!(validate_outline(&bowtie), Err(Invalid::SelfIntersecting));
        let spike = [[0.0, 0.0], [4.0, 0.0], [2.0, 0.0], [2.0, 3.0]];
        assert_eq!(validate_outline(&spike), Err(Invalid::SelfIntersecting));
        let collinear = [[0.0, 0.0], [1.0, 0.0], [3.0, 0.0]];
        assert!(validate_outline(&collinear).is_err());
        assert_eq!(validate_outline(&[[0.0, 0.0], [1.0, 0.0]]), Err(Invalid::TooFewPoints));
        // A vertex touching a non-adjacent edge.
        let touching = [[0.0, 0.0], [4.0, 0.0], [4.0, 4.0], [2.0, 0.0], [0.0, 4.0]];
        assert_eq!(validate_outline(&touching), Err(Invalid::SelfIntersecting));
    }

    #[test]
    fn containment_and_disjointness() {
        let hole = [[1.0, 1.0], [2.0, 1.0], [2.0, 2.0], [1.0, 2.0]];
        assert!(strictly_inside(&hole, &SQUARE));
        let touching = [[0.0, 1.0], [2.0, 1.0], [2.0, 2.0], [0.0, 2.0]];
        assert!(!strictly_inside(&touching, &SQUARE));
        let crossing = [[3.0, 1.0], [5.0, 1.0], [5.0, 2.0], [3.0, 2.0]];
        assert!(!strictly_inside(&crossing, &SQUARE));
        let other = [[2.5, 2.5], [3.5, 2.5], [3.5, 3.5], [2.5, 3.5]];
        assert!(disjoint(&hole, &other));
        let overlapping = [[1.5, 1.5], [3.0, 1.5], [3.0, 3.0], [1.5, 3.0]];
        assert!(!disjoint(&hole, &overlapping));
        let sharing_edge = [[2.0, 1.0], [3.0, 1.0], [3.0, 2.0], [2.0, 2.0]];
        assert!(!disjoint(&hole, &sharing_edge));
        let inner = [[1.2, 1.2], [1.8, 1.2], [1.8, 1.8], [1.2, 1.8]];
        assert!(!disjoint(&hole, &inner), "containment is overlap");
    }

    #[test]
    fn dedup_and_rectangle() {
        let pts = [[0.0, 0.0], [0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 0.0]];
        assert_eq!(dedup_closed(&pts), vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0]]);
        let r = rectangle([3.0, 1.0], [1.0, 2.0]);
        assert_eq!(r, vec![[1.0, 1.0], [3.0, 1.0], [3.0, 2.0], [1.0, 2.0]]);
        assert!(signed_area(&r) > 0.0);
    }
}

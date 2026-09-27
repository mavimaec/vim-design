//! Sketches: self-contained 2D profiles on a construction plane.
//!
//! A [`Sketch`] is a planar graph of points and closed face loops in the
//! plane's (u, v) coordinates (meters). Each face is either material
//! (`Solid`, with its own thickness) or a subtractive region (`Void`, with
//! an optional depth). A `Sketch` entity evaluates directly to prisms:
//! material hangs from the plane, and every point of the plane carries
//! the thickest solid covering it minus the deepest void covering it.
//!
//! Point and face ids are local to one sketch and stable across edits, so
//! a selection or a provenance name keeps pointing at the same thing
//! while the user edits. Edges are derived: the consecutive point pairs
//! of a face loop. Two faces share an edge when both loops contain the
//! same unordered point pair.
//!
//! Validity comes in two tiers. Structural validity (unique ids, loops
//! that reference existing points, at least three distinct points per
//! loop, finite coordinates, positive finite thickness/depth) is checked
//! by the commands that store a sketch. Geometric validity (a loop that
//! crosses itself, zero area) is a per-entity evaluation error, and the
//! same checks are available here for live feedback while editing.
//!
//! The editing operations live in [`ops`]: pure functions from a sketch
//! to the edited sketch, which the caller stores with one `UpdateSketch`.

use serde::{Deserialize, Serialize};

pub(crate) mod layers;
pub mod ops;

/// Distance (meters) under which two sketch points are the same point.
pub const POINT_TOLERANCE: f64 = 1e-6;

/// A 2D profile: points plus closed face loops over them.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Sketch {
    pub points: Vec<SketchPoint>,
    pub faces: Vec<SketchFace>,
}

/// A sketch point: a sketch-local id and its (u, v) position in the
/// plane's frame (meters).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SketchPoint {
    pub id: u32,
    pub uv: [f64; 2],
}

/// A sketch face: a closed loop of point ids (the last point connects
/// back to the first) and what the enclosed region does.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SketchFace {
    pub id: u32,
    pub points: Vec<u32>,
    pub kind: SketchFaceKind,
}

/// What a face contributes.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum SketchFaceKind {
    /// Material from the plane to `thickness` meters away from it.
    Solid { thickness: f64 },
    /// Removes material from the plane to `depth` meters away from it;
    /// `None` removes it through everything. A void may extend outside
    /// every solid face (it then only removes what it overlaps).
    Void { depth: Option<f64> },
}

/// The side of the plane material hangs on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SketchDirection {
    /// Against the plane normal: a floor plate hangs below its level, so
    /// its top face sits at the level elevation.
    Below,
    /// Along the plane normal.
    Above,
}

/// A derived edge: an unordered point pair (`a < b`) and the faces whose
/// loops contain it (ascending face ids).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SketchEdge {
    pub a: u32,
    pub b: u32,
    pub faces: Vec<u32>,
}

/// Typed failure of a sketch operation or check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SketchError {
    /// No point has this id.
    UnknownPoint(u32),
    /// No face has this id.
    UnknownFace(u32),
    /// No face loop contains this point pair as consecutive points.
    UnknownEdge(u32, u32),
    /// Two points share this id.
    DuplicatePointId(u32),
    /// Two faces share this id.
    DuplicateFaceId(u32),
    /// The face loop references this missing point.
    MissingPoint { face: u32, point: u32 },
    /// The face loop has fewer than three distinct points, or repeats a
    /// point immediately (a zero-length edge).
    TooFewPoints { face: u32 },
    /// A coordinate or offset is NaN or infinite.
    NonFinite,
    /// A solid thickness is not finite and positive.
    InvalidThickness { face: u32 },
    /// A void depth is not finite and positive.
    InvalidDepth { face: u32 },
    /// An operation parameter is out of range (for example an edge
    /// parameter outside (0, 1), or a zero-length split segment).
    InvalidParameter,
    /// The face loop crosses or touches itself.
    SelfIntersecting { face: u32 },
    /// The face loop encloses no area.
    ZeroArea { face: u32 },
    /// The split segment does not cross any face from one side to the
    /// other.
    NothingToSplit,
    /// A new id would exceed `u32::MAX`.
    IdSpaceExhausted,
}

impl std::fmt::Display for SketchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SketchError::UnknownPoint(id) => write!(f, "no sketch point {id}"),
            SketchError::UnknownFace(id) => write!(f, "no sketch face {id}"),
            SketchError::UnknownEdge(a, b) => write!(f, "no sketch edge {a}-{b}"),
            SketchError::DuplicatePointId(id) => write!(f, "sketch point id {id} is used twice"),
            SketchError::DuplicateFaceId(id) => write!(f, "sketch face id {id} is used twice"),
            SketchError::MissingPoint { face, point } => {
                write!(f, "sketch face {face} uses missing point {point}")
            }
            SketchError::TooFewPoints { face } => {
                write!(f, "sketch face {face} needs at least three distinct points")
            }
            SketchError::NonFinite => write!(f, "a sketch coordinate is not finite"),
            SketchError::InvalidThickness { face } => {
                write!(f, "sketch face {face} thickness must be finite and positive")
            }
            SketchError::InvalidDepth { face } => {
                write!(f, "sketch face {face} depth must be finite and positive")
            }
            SketchError::InvalidParameter => write!(f, "sketch operation parameter out of range"),
            SketchError::SelfIntersecting { face } => {
                write!(f, "sketch face {face} crosses or touches itself")
            }
            SketchError::ZeroArea { face } => write!(f, "sketch face {face} has no area"),
            SketchError::NothingToSplit => write!(f, "the segment splits no sketch face"),
            SketchError::IdSpaceExhausted => write!(f, "sketch ids are exhausted"),
        }
    }
}

impl std::error::Error for SketchError {}

// ---------------------------------------------------------------------
// Lookups and derived data.
// ---------------------------------------------------------------------

impl Sketch {
    /// The point with this id.
    pub fn point(&self, id: u32) -> Option<&SketchPoint> {
        self.points.iter().find(|p| p.id == id)
    }

    /// The face with this id.
    pub fn face(&self, id: u32) -> Option<&SketchFace> {
        self.faces.iter().find(|f| f.id == id)
    }

    /// Position of a point.
    pub fn uv(&self, id: u32) -> Result<[f64; 2], SketchError> {
        self.point(id)
            .map(|p| p.uv)
            .ok_or(SketchError::UnknownPoint(id))
    }
}

/// The id the next new point gets: the largest point id plus one (0 for
/// a sketch without points). Derived, never stored. Saturates at
/// `u32::MAX`; operations that create points report
/// [`SketchError::IdSpaceExhausted`] instead of reusing an id.
pub fn next_point_id(sketch: &Sketch) -> u32 {
    sketch
        .points
        .iter()
        .map(|p| p.id)
        .max()
        .map_or(0, |max| max.saturating_add(1))
}

/// The id the next new face gets: the largest face id plus one (0 for a
/// sketch without faces). Derived, never stored.
pub fn next_face_id(sketch: &Sketch) -> u32 {
    sketch
        .faces
        .iter()
        .map(|f| f.id)
        .max()
        .map_or(0, |max| max.saturating_add(1))
}

/// All derived edges with the faces that use them, sorted by `(a, b)`.
pub fn edges(sketch: &Sketch) -> Vec<SketchEdge> {
    let mut map: std::collections::BTreeMap<(u32, u32), Vec<u32>> =
        std::collections::BTreeMap::new();
    for face in &sketch.faces {
        for (a, b) in loop_edges(&face.points) {
            if a == b {
                continue;
            }
            let key = (a.min(b), a.max(b));
            let users = map.entry(key).or_default();
            if !users.contains(&face.id) {
                users.push(face.id);
            }
        }
    }
    map.into_iter()
        .map(|((a, b), mut faces)| {
            faces.sort_unstable();
            SketchEdge { a, b, faces }
        })
        .collect()
}

/// The (u, v) polygon of a face, in loop order.
pub fn face_polygon(sketch: &Sketch, face_id: u32) -> Result<Vec<[f64; 2]>, SketchError> {
    let face = sketch
        .face(face_id)
        .ok_or(SketchError::UnknownFace(face_id))?;
    face.points
        .iter()
        .map(|id| {
            sketch.uv(*id).map_err(|_| SketchError::MissingPoint {
                face: face_id,
                point: *id,
            })
        })
        .collect()
}

/// Consecutive point pairs of a closed loop, including last -> first.
pub(crate) fn loop_edges(points: &[u32]) -> impl Iterator<Item = (u32, u32)> + '_ {
    let n = points.len();
    points
        .iter()
        .enumerate()
        .filter_map(move |(i, a)| points.get((i + 1) % n.max(1)).map(|b| (*a, *b)))
}

/// True when the loop contains `a`-`b` as consecutive points in either
/// direction.
pub(crate) fn loop_has_edge(points: &[u32], a: u32, b: u32) -> bool {
    loop_edges(points).any(|(p, q)| (p == a && q == b) || (p == b && q == a))
}

// ---------------------------------------------------------------------
// Validation.
// ---------------------------------------------------------------------

fn validate_kind(face: &SketchFace) -> Result<(), SketchError> {
    match face.kind {
        SketchFaceKind::Solid { thickness } => {
            if thickness.is_finite() && thickness > 0.0 {
                Ok(())
            } else {
                Err(SketchError::InvalidThickness { face: face.id })
            }
        }
        SketchFaceKind::Void { depth: Some(depth) } => {
            if depth.is_finite() && depth > 0.0 {
                Ok(())
            } else {
                Err(SketchError::InvalidDepth { face: face.id })
            }
        }
        SketchFaceKind::Void { depth: None } => Ok(()),
    }
}

fn validate_loop_structure(sketch: &Sketch, face: &SketchFace) -> Result<(), SketchError> {
    for id in &face.points {
        if sketch.point(*id).is_none() {
            return Err(SketchError::MissingPoint {
                face: face.id,
                point: *id,
            });
        }
    }
    let mut distinct: Vec<u32> = face.points.clone();
    distinct.sort_unstable();
    distinct.dedup();
    let repeats_immediately = loop_edges(&face.points).any(|(a, b)| a == b);
    if distinct.len() < 3 || repeats_immediately {
        return Err(SketchError::TooFewPoints { face: face.id });
    }
    Ok(())
}

/// Structural validity, as the sketch commands require it: finite
/// coordinates, unique point and face ids, loops that reference existing
/// points with at least three distinct points and no immediately
/// repeated point, and finite positive thickness/depth.
pub fn validate_structure(sketch: &Sketch) -> Result<(), SketchError> {
    let mut point_ids = std::collections::BTreeSet::new();
    for point in &sketch.points {
        if !point.uv.iter().all(|c| c.is_finite()) {
            return Err(SketchError::NonFinite);
        }
        if !point_ids.insert(point.id) {
            return Err(SketchError::DuplicatePointId(point.id));
        }
    }
    let mut face_ids = std::collections::BTreeSet::new();
    for face in &sketch.faces {
        if !face_ids.insert(face.id) {
            return Err(SketchError::DuplicateFaceId(face.id));
        }
        validate_loop_structure(sketch, face)?;
        validate_kind(face)?;
    }
    Ok(())
}

/// Geometric validity of one face: structural checks plus a simple
/// polygon (no crossing or touching of its own boundary, no point
/// visited twice) with non-zero area.
pub fn validate_face(sketch: &Sketch, face_id: u32) -> Result<(), SketchError> {
    let face = sketch
        .face(face_id)
        .ok_or(SketchError::UnknownFace(face_id))?;
    validate_loop_structure(sketch, face)?;
    validate_kind(face)?;
    let mut ids = face.points.clone();
    ids.sort_unstable();
    if ids.windows(2).any(|w| matches!(w, [a, b] if a == b)) {
        return Err(SketchError::SelfIntersecting { face: face_id });
    }
    let polygon = face_polygon(sketch, face_id)?;
    if !polygon.iter().flatten().all(|c| c.is_finite()) {
        return Err(SketchError::NonFinite);
    }
    if geom::self_intersects(&polygon, POINT_TOLERANCE) {
        return Err(SketchError::SelfIntersecting { face: face_id });
    }
    if geom::signed_area(&polygon).abs() <= POINT_TOLERANCE * POINT_TOLERANCE {
        return Err(SketchError::ZeroArea { face: face_id });
    }
    Ok(())
}

/// Structural validity of the whole sketch plus geometric validity of
/// every face; reports the first problem found (faces in stored order).
pub fn validate(sketch: &Sketch) -> Result<(), SketchError> {
    validate_structure(sketch)?;
    for face in &sketch.faces {
        validate_face(sketch, face.id)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------
// Planar geometry helpers (meters, tolerance-aware).
// ---------------------------------------------------------------------

pub(crate) mod geom {
    pub type P2 = [f64; 2];

    pub fn sub(a: P2, b: P2) -> P2 {
        [a[0] - b[0], a[1] - b[1]]
    }

    pub fn add(a: P2, b: P2) -> P2 {
        [a[0] + b[0], a[1] + b[1]]
    }

    pub fn scale(a: P2, s: f64) -> P2 {
        [a[0] * s, a[1] * s]
    }

    pub fn cross(a: P2, b: P2) -> f64 {
        a[0] * b[1] - a[1] * b[0]
    }

    pub fn dot(a: P2, b: P2) -> f64 {
        a[0] * b[0] + a[1] * b[1]
    }

    pub fn dist(a: P2, b: P2) -> f64 {
        let d = sub(a, b);
        dot(d, d).sqrt()
    }

    pub fn lerp(a: P2, b: P2, t: f64) -> P2 {
        add(a, scale(sub(b, a), t))
    }

    /// Shoelace signed area: positive for counter-clockwise.
    pub fn signed_area(polygon: &[P2]) -> f64 {
        let n = polygon.len();
        let twice: f64 = polygon
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let q = polygon.get((i + 1) % n.max(1)).copied().unwrap_or(*p);
                cross(*p, q)
            })
            .sum();
        twice / 2.0
    }

    /// Distance from `p` to the segment `a`-`b`.
    pub fn point_segment_distance(p: P2, a: P2, b: P2) -> f64 {
        let ab = sub(b, a);
        let len2 = dot(ab, ab);
        if len2 <= f64::MIN_POSITIVE {
            return dist(p, a);
        }
        let t = (dot(sub(p, a), ab) / len2).clamp(0.0, 1.0);
        dist(p, lerp(a, b, t))
    }

    /// Intersection points of segments `p1`-`p2` and `q1`-`q2`, as
    /// `(s, point)` with `s` the parameter along `p1`-`p2`. Collinear
    /// overlaps report both overlap ends. Tolerance in meters.
    pub fn segment_intersections(p1: P2, p2: P2, q1: P2, q2: P2, tol: f64) -> Vec<(f64, P2)> {
        let r = sub(p2, p1);
        let s = sub(q2, q1);
        let r_len = dot(r, r).sqrt();
        let s_len = dot(s, s).sqrt();
        if r_len <= tol || s_len <= tol {
            return Vec::new();
        }
        let denom = cross(r, s);
        let qp = sub(q1, p1);
        if denom.abs() <= 1e-12 * r_len * s_len {
            // Parallel: only collinear overlaps intersect.
            if (cross(qp, r) / r_len).abs() > tol {
                return Vec::new();
            }
            let t_of = |p: P2| dot(sub(p, p1), r) / (r_len * r_len);
            let (mut t0, mut t1) = (t_of(q1), t_of(q2));
            if t0 > t1 {
                std::mem::swap(&mut t0, &mut t1);
            }
            let lo = t0.max(0.0);
            let hi = t1.min(1.0);
            let slack = tol / r_len;
            if lo > hi + slack {
                return Vec::new();
            }
            let lo = lo.min(hi);
            let mut out = vec![(lo, lerp(p1, p2, lo))];
            if (hi - lo) * r_len > tol {
                out.push((hi, lerp(p1, p2, hi)));
            }
            return out;
        }
        let t = cross(qp, s) / denom;
        let u = cross(qp, r) / denom;
        let t_slack = tol / r_len;
        let u_slack = tol / s_len;
        if t < -t_slack || t > 1.0 + t_slack || u < -u_slack || u > 1.0 + u_slack {
            return Vec::new();
        }
        let t = t.clamp(0.0, 1.0);
        vec![(t, lerp(p1, p2, t))]
    }

    /// True when `p` lies strictly inside the polygon: inside by the
    /// even-odd rule and farther than `tol` from its boundary.
    pub fn strictly_inside(p: P2, polygon: &[P2], tol: f64) -> bool {
        let n = polygon.len();
        if n < 3 {
            return false;
        }
        let mut inside = false;
        for i in 0..n {
            let (Some(a), Some(b)) = (polygon.get(i), polygon.get((i + 1) % n)) else {
                return false;
            };
            if point_segment_distance(p, *a, *b) <= tol {
                return false;
            }
            if (a[1] > p[1]) != (b[1] > p[1]) {
                let x = a[0] + (p[1] - a[1]) / (b[1] - a[1]) * (b[0] - a[0]);
                if p[0] < x {
                    inside = !inside;
                }
            }
        }
        inside
    }

    /// True when the closed polygon crosses or touches itself: any two
    /// non-adjacent edges meet, or two adjacent edges fold back onto
    /// each other.
    pub fn self_intersects(polygon: &[P2], tol: f64) -> bool {
        let n = polygon.len();
        let edge = |i: usize| -> Option<(P2, P2)> {
            Some((*polygon.get(i % n)?, *polygon.get((i + 1) % n)?))
        };
        for i in 0..n {
            let Some((a, b)) = edge(i) else { return true };
            // Adjacent fold-back: the next edge runs back along this one.
            if let Some((_, c)) = edge(i + 1) {
                let (ab, bc) = (sub(b, a), sub(c, b));
                let (lab, lbc) = (dot(ab, ab).sqrt(), dot(bc, bc).sqrt());
                if lab > tol
                    && lbc > tol
                    && (cross(ab, bc) / (lab * lbc)).abs() <= 1e-12
                    && dot(ab, bc) < 0.0
                {
                    return true;
                }
            }
            for j in (i + 2)..n {
                if i == 0 && j == n - 1 {
                    continue; // adjacent through the wrap-around
                }
                let Some((c, d)) = edge(j) else { return true };
                if !segment_intersections(a, b, c, d, tol).is_empty() {
                    return true;
                }
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square(size: f64) -> Sketch {
        Sketch {
            points: vec![
                SketchPoint { id: 0, uv: [0.0, 0.0] },
                SketchPoint { id: 1, uv: [size, 0.0] },
                SketchPoint { id: 2, uv: [size, size] },
                SketchPoint { id: 3, uv: [0.0, size] },
            ],
            faces: vec![SketchFace {
                id: 0,
                points: vec![0, 1, 2, 3],
                kind: SketchFaceKind::Solid { thickness: 0.3 },
            }],
        }
    }

    #[test]
    fn structural_validation_catches_each_rule() {
        assert_eq!(validate_structure(&square(1.0)), Ok(()));

        let mut dup = square(1.0);
        dup.points.push(SketchPoint { id: 2, uv: [5.0, 5.0] });
        assert_eq!(validate_structure(&dup), Err(SketchError::DuplicatePointId(2)));

        let mut missing = square(1.0);
        if let Some(face) = missing.faces.first_mut() {
            face.points.push(9);
        }
        assert_eq!(
            validate_structure(&missing),
            Err(SketchError::MissingPoint { face: 0, point: 9 })
        );

        let mut short = square(1.0);
        if let Some(face) = short.faces.first_mut() {
            face.points = vec![0, 1, 0];
        }
        assert_eq!(validate_structure(&short), Err(SketchError::TooFewPoints { face: 0 }));

        let mut repeat = square(1.0);
        if let Some(face) = repeat.faces.first_mut() {
            face.points = vec![0, 1, 1, 2];
        }
        assert_eq!(validate_structure(&repeat), Err(SketchError::TooFewPoints { face: 0 }));

        let mut thin = square(1.0);
        if let Some(face) = thin.faces.first_mut() {
            face.kind = SketchFaceKind::Solid { thickness: 0.0 };
        }
        assert_eq!(
            validate_structure(&thin),
            Err(SketchError::InvalidThickness { face: 0 })
        );

        let mut deep = square(1.0);
        if let Some(face) = deep.faces.first_mut() {
            face.kind = SketchFaceKind::Void { depth: Some(f64::NAN) };
        }
        assert_eq!(validate_structure(&deep), Err(SketchError::InvalidDepth { face: 0 }));

        let mut nan = square(1.0);
        if let Some(point) = nan.points.first_mut() {
            point.uv = [f64::INFINITY, 0.0];
        }
        assert_eq!(validate_structure(&nan), Err(SketchError::NonFinite));

        let mut two_faces = square(1.0);
        let copy = two_faces.faces.first().cloned();
        if let Some(face) = copy {
            two_faces.faces.push(face);
        }
        assert_eq!(validate_structure(&two_faces), Err(SketchError::DuplicateFaceId(0)));
    }

    #[test]
    fn geometric_validation_finds_bowties_and_slivers() {
        assert_eq!(validate(&square(2.0)), Ok(()));

        let mut bowtie = square(1.0);
        if let Some(face) = bowtie.faces.first_mut() {
            face.points = vec![0, 2, 1, 3];
        }
        assert_eq!(validate(&bowtie), Err(SketchError::SelfIntersecting { face: 0 }));

        let mut flat = square(1.0);
        if let Some(point) = flat.points.iter_mut().find(|p| p.id == 2) {
            point.uv = [2.0, 0.0];
        }
        if let Some(point) = flat.points.iter_mut().find(|p| p.id == 3) {
            point.uv = [3.0, 0.0];
        }
        assert!(matches!(
            validate(&flat),
            Err(SketchError::ZeroArea { .. } | SketchError::SelfIntersecting { .. })
        ));

        let mut pinch = square(1.0);
        pinch.points.push(SketchPoint { id: 4, uv: [0.5, 0.5] });
        if let Some(face) = pinch.faces.first_mut() {
            face.points = vec![0, 1, 4, 2, 3, 4];
        }
        assert_eq!(validate(&pinch), Err(SketchError::SelfIntersecting { face: 0 }));
    }

    #[test]
    fn edges_report_shared_boundaries() {
        let mut sketch = square(1.0);
        sketch.points.push(SketchPoint { id: 4, uv: [2.0, 0.0] });
        sketch.points.push(SketchPoint { id: 5, uv: [2.0, 1.0] });
        sketch.faces.push(SketchFace {
            id: 1,
            points: vec![1, 4, 5, 2],
            kind: SketchFaceKind::Void { depth: None },
        });
        let all = edges(&sketch);
        assert_eq!(all.len(), 7, "4 + 4 edges, one shared");
        let shared = all.iter().find(|e| e.a == 1 && e.b == 2);
        assert_eq!(shared.map(|e| e.faces.clone()), Some(vec![0, 1]));
        assert_eq!(next_point_id(&sketch), 6);
        assert_eq!(next_face_id(&sketch), 2);
        assert_eq!(next_point_id(&Sketch::default()), 0);
        assert_eq!(
            face_polygon(&sketch, 1),
            Ok(vec![[1.0, 0.0], [2.0, 0.0], [2.0, 1.0], [1.0, 1.0]])
        );
        assert_eq!(face_polygon(&sketch, 7), Err(SketchError::UnknownFace(7)));
    }
}

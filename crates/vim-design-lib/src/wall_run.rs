//! Wall runs: a wall as a thickened polyline with exact joins at any
//! angle.
//!
//! A run is a reference polyline in its base plane's (u, v). Material
//! grows `thickness_m` to the LEFT of the direction of travel. Segment
//! `k` runs from point `k` to point `k + 1` (a closed run also from the
//! last point back to the first) and is named by its START point id.
//!
//! **Joins.** The plan footprint is the reference polyline offset by the
//! thickness with miter joins: consecutive segments meet exactly on the
//! bisector plane through their shared reference point. Where the offset
//! side is the outside of the turn and the miter would reach farther than
//! [`MITER_LIMIT`] thicknesses from the corner, the corner is beveled
//! instead. Open ends are square. A run that folds back on itself
//! (a U-turn) is an error.
//!
//! **Height.** The top reference H is `height_m` above the base, or, when
//! the top slot is wired, the top plane's height above the base plus
//! `top_offset_m` (as for `Wall`).
//!
//! **Elevation.** Every segment is H high, unless it carries a custom
//! [`SegmentProfile`] (a `Sketch` in the segment's elevation: u along the
//! reference line from the segment start, v up; `top_points` measured from
//! H, as for `Wall`). [`Opening`]s are rectangular voids: windows between
//! `sill_m` and `sill_m + height_m`, doors from below the base to
//! `height_m`; `depth_m: None` goes through, `Some(d)` is a niche `d` deep
//! from the reference face.
//!
//! **Join zones.** Near each join the wall is a wedge: along the
//! reference line, from the segment start to the end of the start miter
//! (and likewise at the end). Openings must stay clear of the join zones
//! ([`segment_clear_span`] gives the usable span), and within a join zone
//! a custom profile must be a band from the base to one straight top
//! edge.

use serde::{Deserialize, Serialize};

use crate::document::Document;
use crate::entity::{Params, slot};
use crate::id::EntityId;
use crate::sketch::layers::{OverlayRule, boolean};
use crate::sketch::{POINT_TOLERANCE, Sketch, SketchError, SketchFaceKind};
use crate::subref::RunPart;

mod convert;
pub mod ops;

pub use convert::{FromWallsError, from_walls};

/// Largest miter length, in thicknesses from the reference corner, on
/// the outside of a turn; sharper outside corners are beveled.
pub const MITER_LIMIT: f64 = 4.0;

/// How far below the base a door's void reaches (meters), so it always
/// cuts the bottom edge.
pub const DOOR_BELOW_BASE_M: f64 = 0.1;

/// A reference polyline point: a run-local id and its (u, v) position in
/// the base plane (meters).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct RunPoint {
    pub id: u32,
    pub uv: [f64; 2],
}

/// What an opening is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OpeningKind {
    Window,
    /// Starts at the base (its sill is ignored) and cuts the bottom edge.
    Door,
}

/// A rectangular opening in one segment.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Opening {
    pub id: u32,
    /// The segment's START point id.
    pub segment: u32,
    /// Along the reference line from the segment start to the opening's
    /// left edge (meters).
    pub offset_m: f64,
    /// Height of the bottom above the base (windows; ignored for doors).
    pub sill_m: f64,
    pub width_m: f64,
    pub height_m: f64,
    pub kind: OpeningKind,
    /// `None` goes through the wall; `Some(d)` is a niche `d` deep from
    /// the reference face.
    pub depth_m: Option<f64>,
}

/// A custom elevation profile for one segment (a gable, a stepped top).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SegmentProfile {
    /// The segment's START point id.
    pub segment: u32,
    /// u along the reference line from the segment start, v up from the
    /// base; solid faces are wall material (their thickness is ignored:
    /// the run's thickness applies), void faces are openings.
    pub profile: Sketch,
    /// Profile points whose v is measured from the top reference H.
    pub top_points: Vec<u32>,
}

/// The data of a wall run, mirroring `Params::WallRun`, so the editing
/// operations are pure functions of it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WallRunData {
    pub points: Vec<RunPoint>,
    pub closed: bool,
    pub thickness_m: f64,
    pub height_m: f64,
    pub top_offset_m: f64,
    pub openings: Vec<Opening>,
    pub profiles: Vec<SegmentProfile>,
}

impl WallRunData {
    /// The run data of `Params::WallRun`.
    pub fn from_params(params: &Params) -> Option<WallRunData> {
        match params {
            Params::WallRun {
                points,
                closed,
                thickness_m,
                height_m,
                top_offset_m,
                openings,
                profiles,
            } => Some(WallRunData {
                points: points.clone(),
                closed: *closed,
                thickness_m: *thickness_m,
                height_m: *height_m,
                top_offset_m: *top_offset_m,
                openings: openings.clone(),
                profiles: profiles.clone(),
            }),
            _ => None,
        }
    }

    /// `Params::WallRun` with this data.
    pub fn into_params(self) -> Params {
        Params::WallRun {
            points: self.points,
            closed: self.closed,
            thickness_m: self.thickness_m,
            height_m: self.height_m,
            top_offset_m: self.top_offset_m,
            openings: self.openings,
            profiles: self.profiles,
        }
    }

    /// Number of segments.
    pub fn segment_count(&self) -> usize {
        let n = self.points.len();
        if self.closed { n } else { n.saturating_sub(1) }
    }

    /// Segment ids (start point ids) in run order.
    pub fn segments(&self) -> Vec<u32> {
        self.points
            .iter()
            .take(self.segment_count())
            .map(|p| p.id)
            .collect()
    }

    /// Index of the segment starting at point `segment`.
    pub fn segment_index(&self, segment: u32) -> Option<usize> {
        self.points
            .iter()
            .take(self.segment_count())
            .position(|p| p.id == segment)
    }

    /// Start and end point of segment `index`.
    pub fn segment_ends(&self, index: usize) -> Option<([f64; 2], [f64; 2])> {
        let n = self.points.len();
        let a = self.points.get(index)?;
        let b = self.points.get((index + 1) % n.max(1))?;
        (index < self.segment_count()).then_some((a.uv, b.uv))
    }

    /// Length of segment `segment` (meters).
    pub fn segment_length(&self, segment: u32) -> Option<f64> {
        let (a, b) = self.segment_ends(self.segment_index(segment)?)?;
        Some(dist(a, b))
    }

    /// The next free point id (largest + 1).
    pub fn next_point_id(&self) -> u32 {
        self.points.iter().map(|p| p.id).max().map_or(0, |m| m.saturating_add(1))
    }

    /// The next free opening id (largest + 1).
    pub fn next_opening_id(&self) -> u32 {
        self.openings.iter().map(|o| o.id).max().map_or(0, |m| m.saturating_add(1))
    }
}

/// Typed failure of a wall-run check or operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WallRunError {
    /// Fewer than two points (three for a closed run).
    TooFewPoints,
    /// Two points share this id.
    DuplicatePointId(u32),
    /// No point has this id.
    UnknownPoint(u32),
    /// No segment starts at this point id.
    UnknownSegment(u32),
    /// A coordinate, height, or offset is NaN or infinite.
    NonFinite,
    /// The thickness is not positive.
    InvalidThickness,
    /// The fixed height is not positive.
    InvalidHeight,
    /// The segment starting at this point has no length.
    ZeroLengthSegment(u32),
    /// The reference polyline crosses itself.
    SelfIntersecting,
    /// The run folds back on itself at this point.
    JoinTooAcute(u32),
    /// The joins at both ends of this segment overlap: no clear span.
    SegmentTooShort(u32),
    /// Two openings share this id.
    DuplicateOpeningId(u32),
    /// No opening has this id.
    UnknownOpening(u32),
    /// This opening's size, sill, or depth is invalid.
    InvalidOpening(u32),
    /// This opening reaches into a join zone or past the segment.
    OpeningOutsideClearSpan(u32),
    /// These two openings overlap.
    OpeningsOverlap(u32, u32),
    /// Splitting a segment would cut through this opening.
    OpeningStraddlesSplit(u32),
    /// Two custom profiles for the segment starting at this point.
    DuplicateProfile(u32),
    /// A custom profile fails sketch validation.
    Profile { segment: u32, error: SketchError },
    /// A top-anchored id is not a point of the segment's profile.
    UnknownTopPoint { segment: u32, point: u32 },
    /// Within a join zone, the segment's profile is not a band from the
    /// base to one straight top edge.
    ProfileNotBandAtJoin(u32),
    /// The top reference is at or below the base.
    TopBelowBase,
    /// The operation needs an open run (or a closed one).
    WrongRunKind,
    /// A 2D boolean failed.
    Boolean,
    /// Two segments' material overlaps (parallel segments closer than
    /// the thickness, a closed run too small for its thickness).
    FootprintOverlaps,
    /// An operation parameter is out of range (an insertion point off
    /// its segment, a non-finite delta).
    InvalidParameter,
    /// After removing points, this opening would no longer lie on the
    /// wall line where it was.
    OpeningDisplaced(u32),
}

impl std::fmt::Display for WallRunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WallRunError::TooFewPoints => {
                write!(f, "a wall run needs two points (three when closed)")
            }
            WallRunError::DuplicatePointId(id) => write!(f, "point id {id} is used twice"),
            WallRunError::UnknownPoint(id) => write!(f, "no point {id}"),
            WallRunError::UnknownSegment(id) => write!(f, "no segment starts at point {id}"),
            WallRunError::NonFinite => write!(f, "a coordinate or size is not finite"),
            WallRunError::InvalidThickness => write!(f, "the thickness must be positive"),
            WallRunError::InvalidHeight => write!(f, "the height must be positive"),
            WallRunError::ZeroLengthSegment(id) => write!(f, "segment {id} has no length"),
            WallRunError::SelfIntersecting => write!(f, "the wall line crosses itself"),
            WallRunError::JoinTooAcute(id) => {
                write!(f, "the wall folds back on itself at point {id}")
            }
            WallRunError::SegmentTooShort(id) => {
                write!(f, "segment {id} is too short for the joins at its ends")
            }
            WallRunError::DuplicateOpeningId(id) => write!(f, "opening id {id} is used twice"),
            WallRunError::UnknownOpening(id) => write!(f, "no opening {id}"),
            WallRunError::InvalidOpening(id) => {
                write!(f, "opening {id} needs a positive width and height and a sill at or above the base")
            }
            WallRunError::OpeningOutsideClearSpan(id) => {
                write!(f, "opening {id} must stay clear of the corner joins")
            }
            WallRunError::OpeningsOverlap(a, b) => write!(f, "openings {a} and {b} overlap"),
            WallRunError::OpeningStraddlesSplit(id) => {
                write!(f, "the new point would split opening {id}")
            }
            WallRunError::DuplicateProfile(id) => {
                write!(f, "segment {id} has two custom profiles")
            }
            WallRunError::Profile { segment, error } => {
                write!(f, "segment {segment} profile: {error}")
            }
            WallRunError::UnknownTopPoint { segment, point } => {
                write!(f, "segment {segment}: top-anchored point {point} is not in its profile")
            }
            WallRunError::ProfileNotBandAtJoin(id) => write!(
                f,
                "segment {id}: near a corner join the profile must rise from the base to one straight top edge"
            ),
            WallRunError::TopBelowBase => write!(f, "the wall top is at or below its base"),
            WallRunError::WrongRunKind => write!(f, "not possible for this kind of run"),
            WallRunError::Boolean => write!(f, "a polygon boolean failed"),
            WallRunError::FootprintOverlaps => write!(f, "two parts of the wall overlap"),
            WallRunError::InvalidParameter => write!(f, "a parameter is out of range"),
            WallRunError::OpeningDisplaced(id) => {
                write!(f, "opening {id} would no longer be on the wall line")
            }
        }
    }
}

impl std::error::Error for WallRunError {}

// ---------------------------------------------------------------------
// Plane geometry.
// ---------------------------------------------------------------------

pub(crate) type P2 = [f64; 2];

pub(crate) fn sub(a: P2, b: P2) -> P2 {
    [a[0] - b[0], a[1] - b[1]]
}

pub(crate) fn add(a: P2, b: P2) -> P2 {
    [a[0] + b[0], a[1] + b[1]]
}

pub(crate) fn scale(a: P2, s: f64) -> P2 {
    [a[0] * s, a[1] * s]
}

pub(crate) fn dot(a: P2, b: P2) -> f64 {
    a[0] * b[0] + a[1] * b[1]
}

pub(crate) fn cross(a: P2, b: P2) -> f64 {
    a[0] * b[1] - a[1] * b[0]
}

pub(crate) fn dist(a: P2, b: P2) -> f64 {
    let d = sub(a, b);
    dot(d, d).sqrt()
}

pub(crate) fn unit(a: P2, b: P2) -> Option<P2> {
    let l = dist(a, b);
    (l > POINT_TOLERANCE).then(|| scale(sub(b, a), 1.0 / l))
}

pub(crate) fn left(d: P2) -> P2 {
    [-d[1], d[0]]
}

fn polygon_area(polygon: &[P2]) -> f64 {
    let n = polygon.len();
    polygon
        .iter()
        .enumerate()
        .map(|(i, p)| cross(*p, polygon.get((i + 1) % n.max(1)).copied().unwrap_or(*p)))
        .sum::<f64>()
        / 2.0
}

// ---------------------------------------------------------------------
// Validation.
// ---------------------------------------------------------------------

fn finite(values: &[f64]) -> Result<(), WallRunError> {
    if values.iter().all(|v| v.is_finite()) {
        Ok(())
    } else {
        Err(WallRunError::NonFinite)
    }
}

/// Structural validity, as the wall-run commands require it: enough
/// points with unique ids and finite coordinates, positive thickness and
/// height, finite top offset, openings and profiles on existing segments
/// with unique ids and valid sizes, and structurally valid profiles
/// whose top anchors are profile points.
pub fn validate_structure(run: &WallRunData) -> Result<(), WallRunError> {
    let min_points = if run.closed { 3 } else { 2 };
    if run.points.len() < min_points {
        return Err(WallRunError::TooFewPoints);
    }
    let mut ids = std::collections::BTreeSet::new();
    for point in &run.points {
        finite(&point.uv)?;
        if !ids.insert(point.id) {
            return Err(WallRunError::DuplicatePointId(point.id));
        }
    }
    finite(&[run.thickness_m, run.height_m, run.top_offset_m])?;
    if run.thickness_m <= POINT_TOLERANCE {
        return Err(WallRunError::InvalidThickness);
    }
    if run.height_m <= 0.0 {
        return Err(WallRunError::InvalidHeight);
    }
    let mut opening_ids = std::collections::BTreeSet::new();
    for opening in &run.openings {
        if !opening_ids.insert(opening.id) {
            return Err(WallRunError::DuplicateOpeningId(opening.id));
        }
        if run.segment_index(opening.segment).is_none() {
            return Err(WallRunError::UnknownSegment(opening.segment));
        }
        finite(&[opening.offset_m, opening.sill_m, opening.width_m, opening.height_m])?;
        let sill_ok = opening.kind == OpeningKind::Door || opening.sill_m >= 0.0;
        let depth_ok = opening.depth_m.is_none_or(|d| d.is_finite() && d > 0.0);
        if opening.width_m <= POINT_TOLERANCE || opening.height_m <= POINT_TOLERANCE || !sill_ok || !depth_ok {
            return Err(WallRunError::InvalidOpening(opening.id));
        }
    }
    let mut profiled = std::collections::BTreeSet::new();
    for profile in &run.profiles {
        if run.segment_index(profile.segment).is_none() {
            return Err(WallRunError::UnknownSegment(profile.segment));
        }
        if !profiled.insert(profile.segment) {
            return Err(WallRunError::DuplicateProfile(profile.segment));
        }
        crate::sketch::validate_structure(&profile.profile).map_err(|error| {
            WallRunError::Profile {
                segment: profile.segment,
                error,
            }
        })?;
        for point in &profile.top_points {
            if profile.profile.point(*point).is_none() {
                return Err(WallRunError::UnknownTopPoint {
                    segment: profile.segment,
                    point: *point,
                });
            }
        }
    }
    Ok(())
}

/// Full validity: structure, plus the geometry the evaluation needs —
/// no zero-length segment, no self-crossing reference line, no fold-back
/// join, a clear span on every segment, and openings inside their
/// segment's clear span that do not overlap.
pub fn validate(run: &WallRunData) -> Result<(), WallRunError> {
    validate_structure(run)?;
    let segments = run.segments();
    for (index, segment) in segments.iter().enumerate() {
        let (a, b) = run.segment_ends(index).ok_or(WallRunError::UnknownSegment(*segment))?;
        if dist(a, b) <= POINT_TOLERANCE {
            return Err(WallRunError::ZeroLengthSegment(*segment));
        }
    }
    if polyline_self_intersects(run) {
        return Err(WallRunError::SelfIntersecting);
    }
    for segment in &segments {
        let (lo, hi) = segment_clear_span(run, *segment)?;
        let mut on_segment: Vec<&Opening> =
            run.openings.iter().filter(|o| o.segment == *segment).collect();
        for opening in &on_segment {
            if opening.offset_m < lo - POINT_TOLERANCE
                || opening.offset_m + opening.width_m > hi + POINT_TOLERANCE
            {
                return Err(WallRunError::OpeningOutsideClearSpan(opening.id));
            }
        }
        on_segment.sort_by_key(|o| o.id);
        for (i, x) in on_segment.iter().enumerate() {
            for y in on_segment.iter().skip(i + 1) {
                let along = x.offset_m < y.offset_m + y.width_m - POINT_TOLERANCE
                    && y.offset_m < x.offset_m + x.width_m - POINT_TOLERANCE;
                let (x0, x1) = opening_heights(x);
                let (y0, y1) = opening_heights(y);
                let up = x0 < y1 - POINT_TOLERANCE && y0 < x1 - POINT_TOLERANCE;
                if along && up {
                    return Err(WallRunError::OpeningsOverlap(x.id, y.id));
                }
            }
        }
    }
    footprints_disjoint(run)
}

/// The plan footprint of segment `index`: reference line, end cut,
/// offset line, start cut (counter-clockwise, convex).
fn footprint(run: &WallRunData, index: usize) -> Result<Vec<P2>, WallRunError> {
    let (a, b) = run.segment_ends(index).ok_or(WallRunError::TooFewPoints)?;
    let (start, end) = cuts(run, index)?;
    let mut polygon: Vec<P2> = vec![a, b];
    polygon.extend(end.points.iter().skip(1));
    polygon.extend(start.points.iter().skip(1).rev());
    Ok(polygon)
}

/// Segment footprints may touch but not overlap (two parallel segments
/// closer than the thickness, a closed run too small for its
/// thickness).
fn footprints_disjoint(run: &WallRunData) -> Result<(), WallRunError> {
    let polygons = (0..run.segment_count())
        .map(|i| footprint(run, i))
        .collect::<Result<Vec<_>, _>>()?;
    let total: f64 = polygons.iter().map(|p| polygon_area(p).abs()).sum();
    let union = boolean(&polygons, &[], OverlayRule::Union).map_err(|_| WallRunError::Boolean)?;
    let covered: f64 = union
        .iter()
        .flatten()
        .map(|ring| polygon_area(ring))
        .sum();
    if total - covered > 1e-9 * total.max(1.0) + POINT_TOLERANCE * POINT_TOLERANCE {
        return Err(WallRunError::FootprintOverlaps);
    }
    Ok(())
}

/// Bottom and top of an opening above the base.
fn opening_heights(opening: &Opening) -> (f64, f64) {
    match opening.kind {
        OpeningKind::Door => (-DOOR_BELOW_BASE_M, opening.height_m),
        OpeningKind::Window => (opening.sill_m, opening.sill_m + opening.height_m),
    }
}

fn polyline_self_intersects(run: &WallRunData) -> bool {
    let count = run.segment_count();
    let seg = |i: usize| run.segment_ends(i);
    for i in 0..count {
        for j in (i + 1)..count {
            let adjacent = j == i + 1 || (run.closed && i == 0 && j == count - 1);
            if adjacent {
                continue;
            }
            let (Some((a, b)), Some((c, d))) = (seg(i), seg(j)) else {
                return true;
            };
            if !crate::sketch::geom::segment_intersections(a, b, c, d, POINT_TOLERANCE).is_empty() {
                return true;
            }
        }
    }
    false
}

// ---------------------------------------------------------------------
// Joins.
// ---------------------------------------------------------------------

/// The cut at one end of a segment, from the reference point outward to
/// the offset side, in plan coordinates.
#[derive(Debug, Clone, PartialEq)]
struct Cut {
    points: Vec<P2>,
}

/// The join at run vertex `index` between the incoming and outgoing
/// segment: the shared part of the cut (reference point to the miter
/// point or bevel midpoint) and, for a bevel, the two offset-line ends.
struct Join {
    shared: [P2; 2],
    bevel: Option<(P2, P2)>,
}

fn join_at(run: &WallRunData, index: usize) -> Result<Join, WallRunError> {
    let n = run.points.len();
    let t = run.thickness_m;
    let point = |i: usize| run.points.get(i % n.max(1));
    let (Some(prev), Some(here), Some(next)) = (point(index + n - 1), point(index), point(index + 1)) else {
        return Err(WallRunError::TooFewPoints);
    };
    let d0 = unit(prev.uv, here.uv).ok_or(WallRunError::ZeroLengthSegment(prev.id))?;
    let d1 = unit(here.uv, next.uv).ok_or(WallRunError::ZeroLengthSegment(here.id))?;
    let (n0, n1) = (left(d0), left(d1));
    let p = here.uv;
    let turn = cross(d0, d1);
    if turn.abs() <= 1e-9 {
        if dot(d0, d1) < 0.0 {
            return Err(WallRunError::JoinTooAcute(here.id));
        }
        return Ok(Join {
            shared: [p, add(p, scale(n0, t))],
            bevel: None,
        });
    }
    // Offset lines p + t n0 + a d0 and p + t n1 + b d1 meet at the miter.
    let q0 = add(p, scale(n0, t));
    let q1 = add(p, scale(n1, t));
    let a = cross(sub(q1, q0), d1) / cross(d0, d1);
    let miter = add(q0, scale(d0, a));
    let outside = turn < 0.0; // turning right: the offset side is outside
    if outside && dist(miter, p) > MITER_LIMIT * t {
        let mid = scale(add(q0, q1), 0.5);
        return Ok(Join {
            shared: [p, mid],
            bevel: Some((q0, q1)),
        });
    }
    Ok(Join {
        shared: [p, miter],
        bevel: None,
    })
}

/// Start and end cuts of segment `index`.
fn cuts(run: &WallRunData, index: usize) -> Result<(Cut, Cut), WallRunError> {
    let (a, b) = run.segment_ends(index).ok_or(WallRunError::TooFewPoints)?;
    let segment = run.points.get(index).map_or(0, |p| p.id);
    let dir = unit(a, b).ok_or(WallRunError::ZeroLengthSegment(segment))?;
    let offset = scale(left(dir), run.thickness_m);
    let n = run.points.len();
    let open_start = !run.closed && index == 0;
    let open_end = !run.closed && index + 1 == run.segment_count();
    let start = if open_start {
        Cut { points: vec![a, add(a, offset)] }
    } else {
        let join = join_at(run, index)?;
        let mut points = join.shared.to_vec();
        if let Some((_, own)) = join.bevel {
            points.push(own); // the outgoing segment's offset start
        }
        Cut { points }
    };
    let end = if open_end {
        Cut { points: vec![b, add(b, offset)] }
    } else {
        let join = join_at(run, (index + 1) % n.max(1))?;
        let mut points = join.shared.to_vec();
        if let Some((own, _)) = join.bevel {
            points.push(own); // the incoming segment's offset end
        }
        Cut { points }
    };
    Ok((start, end))
}

/// The usable span of segment `segment` along its reference line, from
/// its start: openings must lie within `(min, max)`. Outside it the
/// wall is a join wedge.
pub fn segment_clear_span(run: &WallRunData, segment: u32) -> Result<(f64, f64), WallRunError> {
    let index = run.segment_index(segment).ok_or(WallRunError::UnknownSegment(segment))?;
    let (a, b) = run.segment_ends(index).ok_or(WallRunError::UnknownSegment(segment))?;
    let dir = unit(a, b).ok_or(WallRunError::ZeroLengthSegment(segment))?;
    let length = dist(a, b);
    let (start, end) = cuts(run, index)?;
    let s = |p: &P2| dot(sub(*p, a), dir);
    let lo = start.points.iter().map(s).fold(0.0_f64, f64::max);
    let hi = end.points.iter().map(s).fold(length, f64::min);
    if hi - lo <= POINT_TOLERANCE {
        return Err(WallRunError::SegmentTooShort(segment));
    }
    Ok((lo, hi))
}

// ---------------------------------------------------------------------
// Evaluation plans (plain data; the evaluator builds kernel solids).
// ---------------------------------------------------------------------

/// A join wedge: a convex plan polygon (counter-clockwise, base-plane
/// (u, v)) under a planar top `z = top.0 + top.1 * s`, where s is the
/// distance along the segment from its start. `sides[i]` names the
/// vertical face on edge i -> i + 1.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct WedgePlan {
    pub polygon: Vec<P2>,
    pub top: (f64, f64),
    pub sides: Vec<RunPart>,
}

/// The middle of a segment: an elevation profile over the clear span,
/// extruded through the thickness toward the left, plus the part names
/// of its faces.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct MiddlePlan {
    /// u along the reference line from the segment start, v up.
    pub profile: Sketch,
    /// Part of each profile face edge (face id, lower point id, higher
    /// point id).
    pub edges: std::collections::BTreeMap<(u32, u32, u32), RunPart>,
}

/// Everything needed to build one segment.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SegmentPlan {
    pub segment: u32,
    pub start: P2,
    pub dir: P2,
    pub thickness: f64,
    pub wedges: Vec<WedgePlan>,
    pub middle: MiddlePlan,
}

/// Clip a convex polygon to the half-plane `dot(p - origin, axis) <= limit`
/// (or `>=` when `keep_below` is false).
fn clip_half_plane(polygon: &[P2], origin: P2, axis: P2, limit: f64, keep_below: bool) -> Vec<P2> {
    let value = |p: P2| {
        let s = dot(sub(p, origin), axis) - limit;
        if keep_below { -s } else { s }
    };
    let n = polygon.len();
    let mut out = Vec::with_capacity(n + 2);
    for i in 0..n {
        let (Some(p), Some(q)) = (polygon.get(i), polygon.get((i + 1) % n)) else { continue };
        let (vp, vq) = (value(*p), value(*q));
        if vp >= -1e-12 {
            out.push(*p);
        }
        if (vp > 1e-12 && vq < -1e-12) || (vp < -1e-12 && vq > 1e-12) {
            let t = vp / (vp - vq);
            out.push(add(*p, scale(sub(*q, *p), t)));
        }
    }
    out.dedup_by(|x, y| dist(*x, *y) <= 1e-12);
    while out.len() > 1 && out.first().zip(out.last()).is_some_and(|(x, y)| dist(*x, *y) <= 1e-12) {
        out.pop();
    }
    out
}

/// The effective elevation profile of a segment: its custom profile
/// (top anchors raised by `height`), or the plain rectangle, with the
/// openings added as voids. Returns the profile and, per face id, the
/// opening id it came from.
fn segment_profile(
    run: &WallRunData,
    segment: u32,
    length: f64,
    height: f64,
) -> Result<(Sketch, Vec<(u32, u32)>), WallRunError> {
    let base = match run.profiles.iter().find(|p| p.segment == segment) {
        Some(custom) => crate::wall::effective_profile(&custom.profile, &custom.top_points, height),
        None => crate::sketch::ops::add_face(
            &Sketch::default(),
            &[[0.0, 0.0], [length, 0.0], [length, height], [0.0, height]],
            SketchFaceKind::Solid { thickness: run.thickness_m },
        )
        .map_err(|error| WallRunError::Profile { segment, error })?,
    };
    let mut profile = base;
    let mut opening_faces = Vec::new();
    for opening in run.openings.iter().filter(|o| o.segment == segment) {
        let (bottom, top) = opening_heights(opening);
        let (x0, x1) = (opening.offset_m, opening.offset_m + opening.width_m);
        let face_id = crate::sketch::next_face_id(&profile);
        profile = crate::sketch::ops::add_face(
            &profile,
            &[[x0, bottom], [x1, bottom], [x1, top], [x0, top]],
            SketchFaceKind::Void { depth: opening.depth_m },
        )
        .map_err(|error| WallRunError::Profile { segment, error })?;
        opening_faces.push((face_id, opening.id));
    }
    Ok((profile, opening_faces))
}

/// The material region of a profile (solids minus voids), as boolean
/// shapes.
fn material(profile: &Sketch) -> Result<Vec<Vec<Vec<P2>>>, WallRunError> {
    let rings = |solid: bool| -> Result<Vec<Vec<P2>>, WallRunError> {
        profile
            .faces
            .iter()
            .filter(|f| matches!(f.kind, SketchFaceKind::Solid { .. }) == solid)
            .map(|f| {
                let mut ring = crate::sketch::face_polygon(profile, f.id)
                    .map_err(|_| WallRunError::Boolean)?;
                if polygon_area(&ring) < 0.0 {
                    ring.reverse();
                }
                Ok(ring)
            })
            .collect()
    };
    boolean(&rings(true)?, &rings(false)?, OverlayRule::Difference).map_err(|_| WallRunError::Boolean)
}

/// The top line `z = a + b s` of the profile over `[s0, s1]`, if the
/// material there is exactly a band from the base to one straight edge.
fn band_top(material: &[Vec<Vec<P2>>], s0: f64, s1: f64) -> Option<(f64, f64)> {
    let subject: Vec<Vec<P2>> = material.iter().flatten().cloned().collect();
    let strip = vec![strip(s0, s1, subject.iter().flatten())];
    let clipped = boolean(&subject, &strip, OverlayRule::Intersect).ok()?;
    let [shape] = clipped.as_slice() else { return None };
    let [ring] = shape.as_slice() else { return None };
    let ring = &without_collinear(ring);
    let near = |p: &P2, s: f64| (p[0] - s).abs() <= POINT_TOLERANCE;
    let at = |s: f64| -> Option<(f64, f64)> {
        let zs: Vec<f64> = ring.iter().filter(|p| near(p, s)).map(|p| p[1]).collect();
        let lo = zs.iter().copied().fold(f64::INFINITY, f64::min);
        let hi = zs.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        (zs.len() == 2 && lo.abs() <= POINT_TOLERANCE).then_some((lo, hi))
    };
    if ring.len() != 4 {
        return None;
    }
    let (_, h0) = at(s0)?;
    let (_, h1) = at(s1)?;
    let b = (h1 - h0) / (s1 - s0);
    Some((h0 - b * s0, b))
}

/// The vertical strip `s0 <= u <= s1` over the v range of `points`.
fn strip<'a>(s0: f64, s1: f64, points: impl Iterator<Item = &'a P2>) -> Vec<P2> {
    let (lo, hi) = points.fold((0.0_f64, 0.0_f64), |(lo, hi), p| (lo.min(p[1]), hi.max(p[1])));
    let (lo, hi) = (lo - 1.0, hi + 1.0);
    vec![[s0, lo], [s1, lo], [s1, hi], [s0, hi]]
}

/// A ring without points on the straight line between their neighbours.
fn without_collinear(ring: &[P2]) -> Vec<P2> {
    let mut out: Vec<P2> = ring.to_vec();
    let mut changed = true;
    while changed && out.len() > 3 {
        changed = false;
        let n = out.len();
        for i in 0..n {
            let (Some(a), Some(p), Some(b)) = (out.get((i + n - 1) % n), out.get(i), out.get((i + 1) % n)) else {
                continue;
            };
            if crate::sketch::geom::point_segment_distance(*p, *a, *b) <= POINT_TOLERANCE {
                out.remove(i);
                changed = true;
                break;
            }
        }
    }
    out
}

/// Plans for every segment of a valid run with top reference `height`.
pub(crate) fn plan(run: &WallRunData, height: f64) -> Result<Vec<SegmentPlan>, WallRunError> {
    validate(run)?;
    if !(height.is_finite() && height > POINT_TOLERANCE) {
        return Err(WallRunError::TopBelowBase);
    }
    let t = run.thickness_m;
    let mut plans = Vec::with_capacity(run.segment_count());
    for (index, segment) in run.segments().into_iter().enumerate() {
        let (a, b) = run.segment_ends(index).ok_or(WallRunError::UnknownSegment(segment))?;
        let dir = unit(a, b).ok_or(WallRunError::ZeroLengthSegment(segment))?;
        let normal = left(dir);
        let length = dist(a, b);
        let (s0, s1) = segment_clear_span(run, segment)?;

        let footprint = footprint(run, index)?;

        let (profile, opening_faces) = segment_profile(run, segment, length, height)?;
        let region = material(&profile)?;

        let s_of = |p: &P2| dot(sub(*p, a), dir);
        let d_of = |p: &P2| dot(sub(*p, a), normal);
        let mut wedges = Vec::new();
        for (at_start, limit) in [(true, s0), (false, s1)] {
            let polygon = clip_half_plane(&footprint, a, dir, limit, at_start);
            if polygon.len() < 3 || polygon_area(&polygon).abs() <= POINT_TOLERANCE * POINT_TOLERANCE {
                continue;
            }
            // The top line: from the band over the zone inside the span,
            // or, when the zone lies outside it (an outside corner), from
            // a probe one thickness (at most half the length) in.
            let probe = t.min(length / 2.0);
            let (lo, hi) = if at_start {
                (0.0, if s0 > POINT_TOLERANCE { s0 } else { probe })
            } else {
                (if length - s1 > POINT_TOLERANCE { s1 } else { length - probe }, length)
            };
            let top = band_top(&region, lo, hi).ok_or(WallRunError::ProfileNotBandAtJoin(segment))?;
            if polygon.iter().any(|p| top.0 + top.1 * s_of(p) <= POINT_TOLERANCE) {
                return Err(WallRunError::ProfileNotBandAtJoin(segment));
            }
            let own_part = if at_start { RunPart::Start } else { RunPart::End };
            let n = polygon.len();
            let sides = (0..n)
                .map(|i| {
                    let (p, q) = (
                        polygon.get(i).copied().unwrap_or(a),
                        polygon.get((i + 1) % n).copied().unwrap_or(a),
                    );
                    if d_of(&p).abs() <= POINT_TOLERANCE && d_of(&q).abs() <= POINT_TOLERANCE {
                        RunPart::Reference
                    } else if (d_of(&p) - t).abs() <= POINT_TOLERANCE && (d_of(&q) - t).abs() <= POINT_TOLERANCE {
                        RunPart::Opposite
                    } else {
                        own_part
                    }
                })
                .collect();
            wedges.push(WedgePlan { polygon, top, sides });
        }

        let middle = middle_plan(run, segment, &profile, &opening_faces, s0, s1)?;
        plans.push(SegmentPlan {
            segment,
            start: a,
            dir,
            thickness: t,
            wedges,
            middle,
        });
    }
    Ok(plans)
}

/// The profile over the clear span `[s0, s1]` (solid faces clipped to it,
/// voids kept) and the part of every face edge.
fn middle_plan(
    run: &WallRunData,
    segment: u32,
    profile: &Sketch,
    opening_faces: &[(u32, u32)],
    s0: f64,
    s1: f64,
) -> Result<MiddlePlan, WallRunError> {
    let strip = vec![strip(s0, s1, profile.points.iter().map(|p| &p.uv))];
    let err = |error| WallRunError::Profile { segment, error };
    let mut middle = Sketch::default();
    // Face id in `middle` -> where it came from.
    let mut origin: Vec<(u32, Option<RunPart>)> = Vec::new();
    for face in &profile.faces {
        let ring = crate::sketch::face_polygon(profile, face.id).map_err(err)?;
        match face.kind {
            SketchFaceKind::Solid { .. } => {
                let clipped = boolean(&[ring], &strip, OverlayRule::Intersect)
                    .map_err(|_| WallRunError::Boolean)?;
                for piece in clipped.iter().filter_map(|shape| shape.first()) {
                    let id = crate::sketch::next_face_id(&middle);
                    middle = crate::sketch::ops::add_face(
                        &middle,
                        piece,
                        SketchFaceKind::Solid { thickness: run.thickness_m },
                    )
                    .map_err(err)?;
                    origin.push((id, None));
                }
            }
            SketchFaceKind::Void { depth } => {
                let part = match opening_faces.iter().find(|(f, _)| *f == face.id) {
                    Some((_, opening)) => RunPart::Opening { opening: *opening },
                    None => RunPart::ProfileVoid { face: face.id },
                };
                let id = crate::sketch::next_face_id(&middle);
                middle = crate::sketch::ops::add_face(&middle, &ring, SketchFaceKind::Void { depth })
                    .map_err(err)?;
                origin.push((id, Some(part)));
            }
        }
    }
    let mut edges = std::collections::BTreeMap::new();
    for face in &middle.faces {
        let source = origin.iter().find(|(id, _)| *id == face.id).and_then(|(_, p)| *p);
        for (a, b) in crate::sketch::loop_edges(&face.points) {
            let (Ok(pa), Ok(pb)) = (middle.uv(a), middle.uv(b)) else { continue };
            let part = match source {
                Some(part) => part,
                None if pa[1].abs() <= POINT_TOLERANCE && pb[1].abs() <= POINT_TOLERANCE => RunPart::Bottom,
                None if (pa[0] - s0).abs() <= POINT_TOLERANCE && (pb[0] - s0).abs() <= POINT_TOLERANCE => {
                    RunPart::Start
                }
                None if (pa[0] - s1).abs() <= POINT_TOLERANCE && (pb[0] - s1).abs() <= POINT_TOLERANCE => {
                    RunPart::End
                }
                None => RunPart::Top,
            };
            edges.insert((face.id, a.min(b), a.max(b)), part);
        }
    }
    Ok(MiddlePlan { profile: middle, edges })
}

// ---------------------------------------------------------------------
// Document helpers.
// ---------------------------------------------------------------------

/// The top reference height of a wall run above its base plane (meters),
/// from params only, computed exactly as evaluation does.
pub fn run_top_height(doc: &Document, run: EntityId) -> Option<f64> {
    let record = doc.entity(run)?;
    let (height_m, top_offset_m) = match &record.params {
        Params::WallRun {
            height_m,
            top_offset_m,
            ..
        } => (*height_m, *top_offset_m),
        _ => return None,
    };
    let plane = |index: usize| record.inputs.get(index).and_then(|s| s.referenced().next());
    match plane(slot::WALL_RUN_TOP) {
        None => Some(height_m),
        Some(top) => {
            let base = crate::workplane::plane_elevation(doc, plane(slot::WALL_RUN_BASE)?)?;
            let top = crate::workplane::plane_elevation(doc, top)?;
            Some((top - base) + top_offset_m)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(points: &[P2], closed: bool) -> WallRunData {
        WallRunData {
            points: points
                .iter()
                .enumerate()
                .map(|(i, uv)| RunPoint { id: i as u32, uv: *uv })
                .collect(),
            closed,
            thickness_m: 0.2,
            height_m: 2.7,
            top_offset_m: 0.0,
            openings: vec![],
            profiles: vec![],
        }
    }

    #[test]
    fn right_angle_clear_spans() {
        // Turning left at (4, 0): the offset (left) side is inside, so
        // the second segment's clear span starts one thickness in.
        let r = run(&[[0.0, 0.0], [4.0, 0.0], [4.0, 3.0]], false);
        assert_eq!(validate(&r), Ok(()));
        let (lo, hi) = segment_clear_span(&r, 0).unwrap_or_default();
        assert!(lo.abs() < 1e-12 && (hi - 3.8).abs() < 1e-12, "{lo} {hi}");
        let (lo, hi) = segment_clear_span(&r, 1).unwrap_or_default();
        assert!((lo - 0.2).abs() < 1e-12 && (hi - 3.0).abs() < 1e-12, "{lo} {hi}");
    }

    #[test]
    fn folds_and_crossings_are_errors() {
        let fold = run(&[[0.0, 0.0], [4.0, 0.0], [1.0, 0.0]], false);
        assert_eq!(validate(&fold), Err(WallRunError::JoinTooAcute(1)));
        let bow = run(&[[0.0, 0.0], [4.0, 4.0], [4.0, 0.0], [0.0, 4.0]], true);
        assert_eq!(validate(&bow), Err(WallRunError::SelfIntersecting));
        let short = run(&[[0.0, 0.0], [4.0, 0.0], [4.0, 0.1], [8.0, 0.1]], false);
        assert_eq!(validate(&short), Err(WallRunError::SegmentTooShort(1)));
    }

    #[test]
    fn sharp_outside_corners_are_beveled() {
        // Turning right by 170 degrees: the outside miter would be far.
        let angle = 170.0_f64.to_radians();
        let r = run(&[[-4.0, 0.0], [0.0, 0.0], [4.0 * angle.cos(), -4.0 * angle.sin()]], false);
        let join = join_at(&r, 1);
        assert!(join.is_ok_and(|j| j.bevel.is_some()));
    }
}

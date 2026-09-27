//! Wall runs in the authoring app: the library's `WallRun` (a whole
//! chain of wall segments as one thickened polyline, mitered joins at any
//! angle; material on the LEFT of the direction of travel).
//!
//! [`RunModel`] is the plan Edit Mode adapter ([`ProfileModel`]): its
//! points are the run's points and every segment is a two-point "face",
//! so the shared hit-testing, marquee, and drag code select points and
//! edges; each edit is one `wall_run::ops` call on the run's data.
//! [`footprint`] is the thickened line drawn live while editing — the
//! same miter and bevel rule as the library (tested against its
//! evaluated volume). [`chain_of`] recovers a chain from the walls of
//! the earlier wall tools (one `Wall` per segment, butt joined) for
//! their conversion.

use vim_design_lib::EntityId;
use vim_design_lib::wall_run::ops::{self as run_ops, RunEnd};
use vim_design_lib::wall_run::{RunPoint, WallRunData, WallRunError};

use super::edit::{Edit, EditError, FaceKind, ProfileFace, ProfileModel, ProfilePoint, ProfileView};
use super::geom::{EPS, Invalid, P2, dist, signed_area};
use super::model::WallModel;
use super::walls::left;

/// A mitered corner longer than this many thicknesses is beveled (the
/// library's `wall_run::MITER_LIMIT`).
pub const MITER_LIMIT: f64 = vim_design_lib::wall_run::MITER_LIMIT;

fn unit(a: P2, b: P2) -> P2 {
    let l = dist(a, b).max(1e-12);
    [(b[0] - a[0]) / l, (b[1] - a[1]) / l]
}

/// User-facing meaning of a wall-run error (the page's toast).
pub fn run_error(e: WallRunError) -> EditError {
    use WallRunError as E;
    EditError::NoEffect(match e {
        E::TooFewPoints => "A wall needs two points (a closed one three)",
        E::ZeroLengthSegment(_) => "Two points of the wall coincide",
        E::SelfIntersecting => return EditError::Invalid(Invalid::SelfIntersecting),
        E::JoinTooAcute(_) => "The wall would fold back on itself",
        E::SegmentTooShort(_) => "A wall segment is too short for its corner joins",
        E::FootprintOverlaps => "The wall is too thick for that shape: it would overlap itself",
        E::OpeningOutsideClearSpan(_) => "An opening must stay clear of the corner joins",
        E::OpeningsOverlap(..) => "Openings must not overlap",
        E::OpeningStraddlesSplit(_) => "A new point cannot split an opening",
        E::OpeningDisplaced(_) => "An opening would no longer be on the wall",
        E::InvalidOpening(_) => "An opening needs a positive size and a sill at or above the base",
        E::ProfileNotBandAtJoin(_) => "Near a corner the wall's shape must be a plain band",
        E::TopBelowBase => "The wall top must be above its base",
        E::WrongRunKind => "Not possible for this wall: open it first",
        E::InvalidThickness => "The thickness must be more than zero",
        E::InvalidHeight => "The height must be more than zero",
        E::InvalidParameter => "Press on a segment away from its ends",
        E::UnknownPoint(_) | E::UnknownSegment(_) | E::UnknownOpening(_) => return EditError::Unknown,
        E::DuplicatePointId(_)
        | E::DuplicateOpeningId(_)
        | E::DuplicateProfile(_)
        | E::NonFinite
        | E::Profile { .. }
        | E::UnknownTopPoint { .. }
        | E::Boolean => "That edit is not possible",
    })
}

/// The segment start ids and end ids of a run, in order.
pub fn segment_pairs(run: &WallRunData) -> Vec<(u32, u32)> {
    let n = run.points.len();
    let count = run.segment_count();
    (0..count).map(|i| (run.points[i].id, run.points[(i + 1) % n].id)).collect()
}

/// The thickened line: for an open run one polygon (the line, then the
/// offset line back); for a closed run the reference ring and the offset
/// ring. The offset is to the left, corners mitered, beveled past
/// [`MITER_LIMIT`] thicknesses.
pub fn footprint(run: &WallRunData) -> Vec<Vec<P2>> {
    let pts: Vec<P2> = run.points.iter().map(|p| p.uv).collect();
    let offset = offset_line(&pts, run.closed, run.thickness_m, false);
    if run.closed {
        vec![pts, offset]
    } else {
        let mut poly = pts;
        poly.extend(offset.into_iter().rev());
        vec![poly]
    }
}

/// Area of the footprint (m²): the plan area of the walls.
pub fn footprint_area(run: &WallRunData) -> f64 {
    let rings = footprint(run);
    if run.closed {
        (signed_area(&rings[0]).abs() - signed_area(&rings[1]).abs()).abs()
    } else {
        signed_area(&rings[0]).abs()
    }
}

/// A run's points reversed (the material side flips to the other face of
/// the line): each segment is renamed by its new start point, and its
/// openings and profile are mirrored along it.
pub fn reversed(run: &WallRunData) -> WallRunData {
    let mut out = run.clone();
    out.points.reverse();
    let pairs = segment_pairs(run);
    // Old segment (a -> b) becomes (b -> a), named b.
    let renamed = |segment: u32| pairs.iter().find(|(a, _)| *a == segment).map_or(segment, |(_, b)| *b);
    for o in &mut out.openings {
        let length = run.segment_length(o.segment).unwrap_or(0.0);
        o.offset_m = length - o.offset_m - o.width_m;
        o.segment = renamed(o.segment);
    }
    for pr in &mut out.profiles {
        let length = run.segment_length(pr.segment).unwrap_or(0.0);
        for p in &mut pr.profile.points {
            p.uv[0] = length - p.uv[0];
        }
        pr.segment = renamed(pr.segment);
    }
    if run.closed {
        // Closed: the reversed loop keeps its first point first, so every
        // segment keeps the "starts at" naming above.
        out.points.rotate_right(1);
    }
    out
}

/// A run being edited in plan: the library's run data and its current
/// top reference height (for splitting profiles).
#[derive(Debug, Clone, PartialEq)]
pub struct RunModel {
    pub data: WallRunData,
    pub height: f64,
}

impl RunModel {
    fn with(&self, data: WallRunData) -> Self {
        Self { data, height: self.height }
    }

    /// The segment (by start id) an edge is.
    fn segment_of(&self, edge: (u32, u32)) -> Option<u32> {
        segment_pairs(&self.data)
            .into_iter()
            .find(|(a, b)| (*a, *b) == edge || (*b, *a) == edge)
            .map(|(a, _)| a)
    }
}

/// The offset line of a run on its thickness side, mitered.
fn offset_line(pts: &[P2], closed: bool, thickness: f64, flip: bool) -> Vec<P2> {
    let n = pts.len();
    if n < 2 {
        return pts.to_vec();
    }
    let side = if flip { -1.0 } else { 1.0 };
    let normal = |i: usize| {
        let l = left(unit(pts[i], pts[(i + 1) % n]));
        [l[0] * side, l[1] * side]
    };
    let count = if closed { n } else { n - 1 };
    let mut out = Vec::with_capacity(n + 2);
    for (i, &p) in pts.iter().enumerate() {
        let prev = if i > 0 { Some(i - 1) } else if closed { Some(count - 1) } else { None };
        let next = if i < count { Some(i) } else { None };
        let off = |nv: P2| [p[0] + nv[0] * thickness, p[1] + nv[1] * thickness];
        match (prev, next) {
            (Some(a), Some(b)) => {
                let (n1, n2) = (normal(a), normal(b));
                let denom = 1.0 + n1[0] * n2[0] + n1[1] * n2[1];
                let m = [(n1[0] + n2[0]) / denom.max(EPS), (n1[1] + n2[1]) / denom.max(EPS)];
                if denom <= EPS || (m[0] * m[0] + m[1] * m[1]).sqrt() > MITER_LIMIT {
                    // Bevel a very sharp corner.
                    out.push(off(n1));
                    out.push(off(n2));
                } else {
                    out.push(off(m));
                }
            }
            (Some(a), None) => out.push(off(normal(a))),
            (None, Some(b)) => out.push(off(normal(b))),
            (None, None) => out.push(p),
        }
    }
    out
}

impl ProfileModel for RunModel {
    fn view(&self) -> ProfileView {
        let kind = FaceKind::Solid { thickness: self.data.thickness_m };
        ProfileView {
            points: self.data.points.iter().map(|p| ProfilePoint { id: p.id, uv: p.uv }).collect(),
            // Each segment is a two-point face: its one edge is the segment.
            faces: segment_pairs(&self.data)
                .iter()
                .enumerate()
                .map(|(i, (a, b))| ProfileFace { id: i as u32, points: vec![*a, *b], kind })
                .collect(),
        }
    }

    fn apply(&self, edit: &Edit) -> Result<Self, EditError> {
        let run = &self.data;
        let segments_of_edges = |edges: &[crate::authoring::edit::EdgeKey]| -> Result<Vec<u32>, EditError> {
            edges.iter().map(|e| self.segment_of((e.0, e.1)).ok_or(EditError::Unknown)).collect()
        };
        let next = match edit {
            Edit::MovePoints { ids, delta } => run_ops::move_points(run, ids, *delta),
            Edit::MoveEdges { edges, delta } => run_ops::move_edges(run, &segments_of_edges(edges)?, *delta),
            Edit::MoveFaces { faces, delta } => {
                let pairs = segment_pairs(run);
                let segs: Vec<u32> = faces.iter().filter_map(|f| pairs.get(*f as usize)).map(|(a, _)| *a).collect();
                run_ops::move_edges(run, &segs, *delta)
            }
            Edit::InsertPoint { edge, uv } => {
                let segment = self.segment_of((edge.0, edge.1)).ok_or(EditError::Unknown)?;
                let index = run.segment_index(segment).ok_or(EditError::Unknown)?;
                let (a, b) = run.segment_ends(index).ok_or(EditError::Unknown)?;
                let d = unit(a, b);
                let along = (uv[0] - a[0]) * d[0] + (uv[1] - a[1]) * d[1];
                run_ops::insert_point(run, segment, along, self.height).map(|(r, _)| r)
            }
            Edit::DeletePoints(ids) => run_ops::delete_points(run, ids),
            Edit::DeleteEdges(edges) => run_ops::delete_edges(run, &segments_of_edges(edges)?),
            Edit::Extend { at_end, uv } => {
                run_ops::extend(run, if *at_end { RunEnd::End } else { RunEnd::Start }, *uv).map(|(r, _)| r)
            }
            Edit::SetClosed(closed) => run_ops::set_closed(run, *closed),
            Edit::AddFace { .. } | Edit::SplitFaces { .. } | Edit::DeleteFaces(_) | Edit::SetKind { .. } => {
                return Err(EditError::NoEffect("That edit does not apply to a wall's plan"));
            }
        };
        next.map(|d| self.with(d)).map_err(run_error)
    }
}

/// A new run's data from drawn points: a closed loop is made
/// counter-clockwise (material inward), `flip` puts the material on the
/// other side (the points reversed).
pub fn new_run(points: &[P2], closed: bool, flip: bool, thickness: f64, height_m: f64, top_offset_m: f64) -> WallRunData {
    let mut pts = points.to_vec();
    if closed && signed_area(&pts) < 0.0 {
        pts.reverse();
    }
    if flip {
        pts.reverse();
    }
    WallRunData {
        points: pts.iter().enumerate().map(|(i, uv)| RunPoint { id: i as u32, uv: *uv }).collect(),
        closed,
        thickness_m: thickness,
        height_m,
        top_offset_m,
        openings: Vec::new(),
        profiles: Vec::new(),
    }
}

/// A chain recovered from walls: its elements in walk order, the corner
/// points (entry corner of each wall, plus the far end when open),
/// whether each wall is walked along its stored direction, and whether
/// the chain closes.
#[derive(Debug, Clone, PartialEq)]
pub struct Chain {
    pub elements: Vec<EntityId>,
    pub points: Vec<P2>,
    pub forward: Vec<bool>,
    pub closed: bool,
}

impl Chain {
    /// The walls in their stored direction (material on the left, as in
    /// a wall run), each with its corner-to-corner line: the order a
    /// wall run of the chain has.
    pub fn stored(&self) -> Vec<(EntityId, P2, P2)> {
        let n = self.points.len();
        let mut out: Vec<(EntityId, P2, P2)> = self
            .elements
            .iter()
            .enumerate()
            .map(|(k, e)| {
                let (entry, exit) = (self.points[k], self.points[(k + 1) % n]);
                if self.forward[k] { (*e, entry, exit) } else { (*e, exit, entry) }
            })
            .collect();
        if self.forward.first() == Some(&false) {
            out.reverse();
        }
        out
    }
}

/// Where the reference lines of two walls meet (their corner), if the
/// ends `a` (of the first) and `b` (of the second) belong to one butt
/// join: each end within its wall's thickness of the lines' crossing.
fn corner(w1: &WallModel, a: P2, w2: &WallModel, b: P2) -> Option<P2> {
    let (d1, d2) = (w1.dir(), w2.dir());
    let cross = d1[0] * d2[1] - d1[1] * d2[0];
    if cross.abs() < 1e-9 {
        return (dist(a, b) < 1e-6).then_some(a);
    }
    // w1.start + d1 * s = w2.start + d2 * r
    let q = [w2.start[0] - w1.start[0], w2.start[1] - w1.start[1]];
    let s = (q[0] * d2[1] - q[1] * d2[0]) / cross;
    let p = [w1.start[0] + d1[0] * s, w1.start[1] + d1[1] * s];
    let reach = |w: &WallModel, e: P2| dist(p, e) <= w.thickness() + 1e-6;
    (reach(w1, a) && reach(w2, b)).then_some(p)
}

/// The chain that `start` belongs to: the walls on the same base plane
/// with the same thickness whose ends meet (end to end, or in the butt
/// joins of the first wall tool), walked from one free end (or around a
/// loop).
pub fn chain_of(walls: &[&WallModel], start: EntityId) -> Option<Chain> {
    let first = walls.iter().position(|w| w.element == start)?;
    let w0 = walls[first];
    let same = |w: &WallModel| w.base == w0.base && (w.thickness() - w0.thickness()).abs() < 1e-9;
    let cands: Vec<&WallModel> = walls.iter().copied().filter(|w| same(w)).collect();
    let me = cands.iter().position(|w| w.element == start)?;
    let end_pt = |i: usize, e: usize| if e == 0 { cands[i].start } else { cands[i].end };
    // Links: (wall, end) -> (wall, end, corner), the nearest join.
    let n = cands.len();
    let mut link: Vec<[Option<(usize, usize, P2)>; 2]> = vec![[None, None]; n];
    for (i, ends) in link.iter_mut().enumerate() {
        for (ei, slot) in ends.iter_mut().enumerate() {
            let mut best: Option<(f64, usize, usize, P2)> = None;
            for j in (0..n).filter(|j| *j != i) {
                for ej in 0..2 {
                    let (a, b) = (end_pt(i, ei), end_pt(j, ej));
                    if let Some(p) = corner(cands[i], a, cands[j], b) {
                        let d = dist(p, a) + dist(p, b);
                        if best.is_none_or(|(bd, ..)| d < bd) {
                            best = Some((d, j, ej, p));
                        }
                    }
                }
            }
            *slot = best.map(|(_, j, ej, p)| (j, ej, p));
        }
    }
    // A join counts only when it is mutual.
    let linked = |i: usize, e: usize| {
        link[i][e].filter(|(j, ej, _)| matches!(link[*j][*ej], Some((k, ek, _)) if k == i && ek == e))
    };
    // Walk back from `me` (entered through its start end) to the run's
    // beginning — or around a loop back to `me`.
    let (mut cur, mut entry) = (me, 0usize);
    for _ in 0..n {
        let Some((j, ej, _)) = linked(cur, entry) else { break };
        // The previous wall leaves through `ej`, so it is entered through
        // its other end.
        (cur, entry) = (j, 1 - ej);
        if (cur, entry) == (me, 0) {
            break;
        }
    }
    let begin = (cur, entry);
    // Walk forward: each wall's entry point is its corner with the
    // previous wall (or its free end).
    let (mut elements, mut points, mut forward) = (Vec::new(), Vec::new(), Vec::new());
    let mut closed = false;
    loop {
        elements.push(cands[cur].element);
        points.push(linked(cur, entry).map_or(end_pt(cur, entry), |(_, _, p)| p));
        forward.push(entry == 0);
        let exit = 1 - entry;
        match linked(cur, exit) {
            Some((j, ej, _)) if (j, ej) == begin => {
                closed = true;
                break;
            }
            Some((j, ej, _)) if elements.len() < n => (cur, entry) = (j, ej),
            Some(_) => return None,
            None => {
                points.push(end_pt(cur, exit));
                break;
            }
        }
    }
    Some(Chain { elements, points, forward, closed })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authoring::edit::EdgeKey;
    use vim_design_lib::wall_run::{Opening, OpeningKind, validate};

    fn room() -> RunModel {
        RunModel { data: new_run(&[[0.0, 0.0], [0.0, 3.0], [4.0, 3.0], [4.0, 0.0]], true, false, 0.2, 2.7, 0.0), height: 2.7 }
    }

    #[test]
    fn new_runs_grow_inward_and_flip_reverses() {
        let r = room().data;
        assert!(signed_area(&r.points.iter().map(|p| p.uv).collect::<Vec<_>>()) > 0.0, "CCW");
        assert_eq!(validate(&r), Ok(()));
        assert!((footprint_area(&r) - (4.0 * 3.0 - 3.6 * 2.6)).abs() < 1e-9, "inward band");
        let f = new_run(&[[0.0, 0.0], [0.0, 3.0], [4.0, 3.0], [4.0, 0.0]], true, true, 0.2, 2.7, 0.0);
        assert!((footprint_area(&f) - (4.4 * 3.4 - 12.0)).abs() < 1e-9, "outward band");
        // An open run at 60° then 135°: the footprint is mitered.
        let open = new_run(&[[0.0, 0.0], [4.0, 0.0], [6.0, 3.464_101_615_137_754], [3.0, 5.0]], false, false, 0.2, 2.7, 0.0);
        assert_eq!(validate(&open), Ok(()));
        assert_eq!(footprint(&open)[0].len(), 8);
    }

    #[test]
    fn plan_edits_are_wall_run_ops() {
        let r = room();
        let (a, b) = (r.data.points[0].id, r.data.points[1].id);
        let ins = r.apply(&Edit::InsertPoint { edge: EdgeKey::new(a, b), uv: [2.0, 0.3] }).expect("insert");
        assert_eq!(ins.data.points.len(), 5);
        let merged = ins.apply(&Edit::DeletePoints(vec![ins.data.points[1].id])).expect("delete point");
        assert_eq!(merged.data.points.len(), 4);
        let e = r.apply(&Edit::DeleteEdges(vec![EdgeKey::new(b, r.data.points[2].id)])).expect("delete edge");
        assert_eq!(e.data.points.len(), 3);
        let open = r.apply(&Edit::SetClosed(false)).expect("open");
        let ext = open.apply(&Edit::Extend { at_end: true, uv: [-2.0, 0.0] }).expect("extend");
        assert_eq!(ext.data.points.len(), 5);
        assert!(matches!(r.apply(&Edit::Extend { at_end: true, uv: [9.0, 9.0] }), Err(EditError::NoEffect(_))));
        // (4, 0) moved to (-1, 2): its segments cross the others.
        let bad = r.apply(&Edit::MovePoints { ids: vec![r.data.points[0].id], delta: [-5.0, 2.0] });
        assert!(bad.is_err(), "a crossing run is refused");
    }

    #[test]
    fn reversing_mirrors_openings_onto_the_renamed_segments() {
        let mut r = room().data;
        let seg = r.points[1].id; // the top segment, from (4,3)? (CCW order)
        let length = r.segment_length(seg).expect("length");
        r.openings.push(Opening { id: 0, segment: seg, offset_m: 1.0, sill_m: 0.9, width_m: 1.2, height_m: 1.2, kind: OpeningKind::Window, depth_m: None });
        assert_eq!(validate(&r), Ok(()));
        let rev = reversed(&r);
        assert_eq!(validate(&rev), Ok(()));
        let o = rev.openings[0];
        assert!((o.offset_m - (length - 2.2)).abs() < 1e-9);
        // The opening is on the same wall line: its end points coincide.
        let at = |run: &WallRunData, o: &Opening, u: f64| {
            let i = run.segment_index(o.segment).expect("segment");
            let (a, b) = run.segment_ends(i).expect("ends");
            let d = unit(a, b);
            [a[0] + d[0] * u, a[1] + d[1] * u]
        };
        let (p0, q1) = (at(&r, &r.openings[0], 1.0), at(&rev, &o, o.offset_m + o.width_m));
        assert!(dist(p0, q1) < 1e-9, "{p0:?} {q1:?}");
        let back = reversed(&rev);
        assert_eq!((back.points.clone(), back.openings[0].segment), (r.points.clone(), r.openings[0].segment), "reversing twice");
        assert!((back.openings[0].offset_m - r.openings[0].offset_m).abs() < 1e-12);
    }
}

//! Wall runs: a polyline of reference points on a construction plane
//! (open or closed) with one thickness, the unit the wall's plan Edit
//! Mode works on. The drawn line is one face of the walls; the
//! thickness grows to its left (to the right with `flip`), as in the
//! wall tool.
//!
//! [`RunModel`] is the Edit Mode adapter ([`ProfileModel`]): its points
//! are the run's points and every segment is a two-point "face", so the
//! shared hit-testing, marquee, and drag code select points and edges.
//! [`RunModel::footprint`] is the thickened line with mitered joins at
//! any angle (drawn live while editing), and [`chain_of`] recovers a run
//! from the walls of the first wall tool (one `Wall` per segment, butt
//! joined), until the library's `WallRun` holds runs directly.

use vim_design_lib::EntityId;

use super::edit::{Edit, EditError, FaceKind, ProfileFace, ProfileModel, ProfilePoint, ProfileView};
use super::geom::{EPS, Invalid, P2, dist, self_intersects, signed_area};
use super::model::WallModel;
use super::walls::{MIN_WALL_LENGTH_M, left};

/// A mitered corner longer than this many thicknesses is beveled (a very
/// sharp corner would otherwise spike far out).
pub const MITER_LIMIT: f64 = 4.0;
/// A point inserted on a segment stays this fraction of the segment away
/// from its ends.
const INSERT_END_FRACTION: f64 = 0.02;

/// A run being edited: points (stable ids), open or closed, thickness,
/// and the thickness side.
#[derive(Debug, Clone, PartialEq)]
pub struct RunModel {
    pub points: Vec<(u32, P2)>,
    pub closed: bool,
    pub thickness: f64,
    pub flip: bool,
}

fn unit(a: P2, b: P2) -> P2 {
    let l = dist(a, b).max(1e-12);
    [(b[0] - a[0]) / l, (b[1] - a[1]) / l]
}

impl RunModel {
    pub fn new(points: &[P2], closed: bool, thickness: f64, flip: bool) -> Self {
        let points = points.iter().enumerate().map(|(i, p)| (i as u32, *p)).collect();
        Self { points, closed, thickness, flip }
    }

    pub fn uvs(&self) -> Vec<P2> {
        self.points.iter().map(|(_, p)| *p).collect()
    }

    fn next_id(&self) -> u32 {
        self.points.iter().map(|(id, _)| id + 1).max().unwrap_or(0)
    }

    fn index(&self, id: u32) -> Option<usize> {
        self.points.iter().position(|(pid, _)| *pid == id)
    }

    /// Segments as point-id pairs, in run order (the closing one last).
    pub fn segments(&self) -> Vec<(u32, u32)> {
        let n = self.points.len();
        let count = if self.closed { n } else { n.saturating_sub(1) };
        (0..count).map(|i| (self.points[i].0, self.points[(i + 1) % n].0)).collect()
    }

    /// The points in the orientation the walls are built with: a closed
    /// run counter-clockwise (so an unflipped loop grows inward).
    pub fn oriented(&self) -> Vec<P2> {
        let mut pts = self.uvs();
        if self.closed && signed_area(&pts) < 0.0 {
            pts.reverse();
        }
        pts
    }

    /// The thickened line: for an open run one polygon (the line, then
    /// the offset line back); for a closed run the reference ring and
    /// the offset ring. Corners are mitered (beveled past
    /// [`MITER_LIMIT`]).
    pub fn footprint(&self) -> Vec<Vec<P2>> {
        let pts = self.oriented();
        let offset = offset_line(&pts, self.closed, self.thickness, self.flip);
        if self.closed {
            vec![pts, offset]
        } else {
            let mut poly = pts;
            poly.extend(offset.into_iter().rev());
            vec![poly]
        }
    }

    /// The live rules: enough points, segments long enough, and neither
    /// the line nor its thickened outline crossing itself.
    pub fn validate(&self) -> Result<(), EditError> {
        let pts = self.oriented();
        let min = if self.closed { 3 } else { 2 };
        if pts.len() < min {
            return Err(EditError::NoEffect("A wall needs at least two points (a closed one three)"));
        }
        let n = pts.len();
        let count = if self.closed { n } else { n - 1 };
        if (0..count).any(|i| dist(pts[i], pts[(i + 1) % n]) < MIN_WALL_LENGTH_M) {
            return Err(EditError::Invalid(Invalid::WallTooShort));
        }
        if self_intersects(&pts, self.closed) {
            return Err(EditError::Invalid(Invalid::SelfIntersecting));
        }
        let rings = self.footprint();
        let crossing = if self.closed {
            let inner = &rings[1];
            // The offset ring must stay a simple ring on the same side.
            self_intersects(inner, true) || signed_area(inner) * signed_area(&rings[0]) <= 0.0
        } else {
            self_intersects(&rings[0], true)
        };
        if crossing {
            return Err(EditError::NoEffect("The wall is too thick for that shape: it would cross itself"));
        }
        Ok(())
    }

    fn validated(self) -> Result<Self, EditError> {
        self.validate().map(|_| self)
    }

    fn move_ids(&self, ids: &[u32], delta: P2) -> Result<Self, EditError> {
        let mut next = self.clone();
        let mut any = false;
        for (id, p) in &mut next.points {
            if ids.contains(id) {
                p[0] += delta[0];
                p[1] += delta[1];
                any = true;
            }
        }
        if !any {
            return Err(EditError::Unknown);
        }
        Ok(next)
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

fn project_t(a: P2, b: P2, p: P2) -> f64 {
    let ab = [b[0] - a[0], b[1] - a[1]];
    let len2 = ab[0] * ab[0] + ab[1] * ab[1];
    if len2 <= f64::EPSILON { 0.5 } else { ((p[0] - a[0]) * ab[0] + (p[1] - a[1]) * ab[1]) / len2 }
}

impl ProfileModel for RunModel {
    fn view(&self) -> ProfileView {
        let kind = FaceKind::Solid { thickness: self.thickness };
        ProfileView {
            points: self.points.iter().map(|(id, uv)| ProfilePoint { id: *id, uv: *uv }).collect(),
            // Each segment is a two-point face: its one edge is the segment.
            faces: self
                .segments()
                .iter()
                .enumerate()
                .map(|(i, (a, b))| ProfileFace { id: i as u32, points: vec![*a, *b], kind })
                .collect(),
        }
    }

    fn apply(&self, edit: &Edit) -> Result<Self, EditError> {
        match edit {
            Edit::MovePoints { ids, delta } => self.move_ids(ids, *delta)?.validated(),
            Edit::MoveEdges { edges, delta } => {
                let ids: Vec<u32> = edges.iter().flat_map(|e| [e.0, e.1]).collect();
                self.move_ids(&ids, *delta)?.validated()
            }
            Edit::MoveFaces { faces, delta } => {
                let segs = self.segments();
                let ids: Vec<u32> = faces.iter().filter_map(|f| segs.get(*f as usize)).flat_map(|(a, b)| [*a, *b]).collect();
                self.move_ids(&ids, *delta)?.validated()
            }
            Edit::InsertPoint { edge, uv } => {
                let (ia, ib) = (self.index(edge.0).ok_or(EditError::Unknown)?, self.index(edge.1).ok_or(EditError::Unknown)?);
                let n = self.points.len();
                // The segment's order in the run (a -> b), wrap included.
                let (first, second) = if (ia + 1) % n == ib { (ia, ib) } else if (ib + 1) % n == ia { (ib, ia) } else {
                    return Err(EditError::Unknown);
                };
                let (a, b) = (self.points[first].1, self.points[second].1);
                let t = project_t(a, b, *uv).clamp(INSERT_END_FRACTION, 1.0 - INSERT_END_FRACTION);
                let mut next = self.clone();
                let p = [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t];
                next.points.insert(first + 1, (self.next_id(), p));
                next.validated()
            }
            Edit::DeletePoints(ids) => {
                // The neighbours of a removed point join: its two
                // segments merge into one.
                let mut next = self.clone();
                next.points.retain(|(id, _)| !ids.contains(id));
                if next.points.len() == self.points.len() {
                    return Err(EditError::Unknown);
                }
                if next.closed && next.points.len() < 3 {
                    next.closed = false;
                }
                next.validated()
            }
            Edit::DeleteEdges(edges) => {
                // An edge's two points merge into its first point (in run
                // order), which keeps its position.
                let mut next = self.clone();
                let n = self.points.len();
                let mut drop = Vec::new();
                for e in edges {
                    let (Some(ia), Some(ib)) = (self.index(e.0), self.index(e.1)) else { continue };
                    let second = if (ia + 1) % n == ib { ib } else { ia };
                    drop.push(self.points[second].0);
                }
                if drop.is_empty() {
                    return Err(EditError::Unknown);
                }
                next.points.retain(|(id, _)| !drop.contains(id));
                if next.closed && next.points.len() < 3 {
                    next.closed = false;
                }
                next.validated()
            }
            Edit::Extend { at_end, uv } => {
                if self.closed {
                    return Err(EditError::NoEffect("A closed wall has no end to extend: open it first"));
                }
                let mut next = self.clone();
                let point = (self.next_id(), *uv);
                if *at_end { next.points.push(point) } else { next.points.insert(0, point) }
                next.validated()
            }
            Edit::SetClosed(closed) => {
                let mut next = self.clone();
                next.closed = *closed;
                next.validated()
            }
            Edit::AddFace { .. } | Edit::SplitFaces { .. } | Edit::DeleteFaces(_) | Edit::SetKind { .. } => {
                Err(EditError::NoEffect("That edit does not apply to a wall's plan"))
            }
        }
    }
}

/// A run recovered from walls: its elements in run order, points, and
/// thickness side.
#[derive(Debug, Clone, PartialEq)]
pub struct Chain {
    pub elements: Vec<EntityId>,
    pub points: Vec<P2>,
    pub closed: bool,
    pub flip: bool,
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

/// The run that `start` belongs to: the walls on the same base plane,
/// with the same thickness and height settings, whose ends meet in butt
/// joins, walked from one free end (or around a loop). A closed run is
/// returned counter-clockwise.
pub fn chain_of(walls: &[&WallModel], start: EntityId) -> Option<Chain> {
    let first = walls.iter().position(|w| w.element == start)?;
    let w0 = walls[first];
    let same = |w: &WallModel| {
        w.base == w0.base
            && w.top == w0.top
            && (w.thickness() - w0.thickness()).abs() < 1e-9
            && (w.top_offset_m - w0.top_offset_m).abs() < 1e-9
            && (w.top.is_some() || (w.height_m - w0.height_m).abs() < 1e-9)
    };
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
    // Material on the left of the stored direction: walking a wall
    // backwards puts it on the right of the walk.
    let mut flip = !forward[0];
    if closed && signed_area(&points) < 0.0 {
        points.reverse();
        // Reversed, the loop's first point stays first.
        points.rotate_right(1);
        elements.reverse();
        flip = !flip;
    }
    Some(Chain { elements, points, closed, flip })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authoring::edit::EdgeKey;

    fn square(flip: bool) -> RunModel {
        RunModel::new(&[[0.0, 0.0], [4.0, 0.0], [4.0, 3.0], [0.0, 3.0]], true, 0.2, flip)
    }

    #[test]
    fn footprint_miters_any_angle() {
        // An open run with a 45° turn: the inner corner is one mitered point.
        let r = RunModel::new(&[[0.0, 0.0], [4.0, 0.0], [6.0, 2.0]], false, 0.2, false);
        let poly = &r.footprint()[0];
        assert_eq!(poly.len(), 6, "3 line points + 3 offset points, no bevel");
        // The mitered point lies on both offset lines (y = 0.2 and the
        // 45° line shifted by 0.2 to its left).
        let m = poly[4];
        assert!((m[1] - 0.2).abs() < 1e-9, "{m:?}");
        let d = (m[1] - m[0] + 4.0) / 2f64.sqrt(); // distance to y = x - 4, left side
        assert!((d - 0.2).abs() < 1e-9, "{m:?}");
        assert_eq!(r.validate(), Ok(()));
        // A closed CW square is built CCW: the inner ring is inward.
        let cw = RunModel::new(&[[0.0, 0.0], [0.0, 3.0], [4.0, 3.0], [4.0, 0.0]], true, 0.2, false);
        let rings = cw.footprint();
        assert!(signed_area(&rings[0]) > 0.0);
        assert!((signed_area(&rings[1]) - 3.6 * 2.6).abs() < 1e-9);
    }

    #[test]
    fn edits_insert_merge_extend_and_validate() {
        let r = square(false);
        let ins = r.apply(&Edit::InsertPoint { edge: EdgeKey::new(0, 1), uv: [2.0, 0.3] }).expect("insert");
        assert_eq!(ins.points.len(), 5);
        assert_eq!(ins.points[1].1, [2.0, 0.0], "projected onto the segment");
        // Deleting a point merges its two segments.
        let merged = ins.apply(&Edit::DeletePoints(vec![ins.points[1].0])).expect("delete point");
        assert_eq!(merged.uvs(), r.uvs());
        // Deleting an edge keeps its first point.
        let e = r.apply(&Edit::DeleteEdges(vec![EdgeKey::new(1, 2)])).expect("delete edge");
        assert_eq!(e.uvs(), vec![[0.0, 0.0], [4.0, 0.0], [0.0, 3.0]]);
        // Open, then extend from the end.
        let open = r.apply(&Edit::SetClosed(false)).expect("open");
        let ext = open.apply(&Edit::Extend { at_end: true, uv: [-2.0, 3.0] }).expect("extend");
        assert_eq!(ext.points.last().map(|p| p.1), Some([-2.0, 3.0]));
        assert!(matches!(r.apply(&Edit::Extend { at_end: true, uv: [9.0, 9.0] }), Err(EditError::NoEffect(_))));
        // A move that folds the run over itself is refused.
        let bad = r.apply(&Edit::MovePoints { ids: vec![3], delta: [5.0, -1.5] });
        assert_eq!(bad, Err(EditError::Invalid(Invalid::SelfIntersecting)));
        // Too thick for a narrow room: the offset ring would invert.
        let thick = RunModel { thickness: 2.0, ..square(false) };
        assert!(matches!(thick.validate(), Err(EditError::NoEffect(_))));
        // Flipped, the thickness goes outside: fine.
        assert_eq!(RunModel { thickness: 2.0, ..square(true) }.validate(), Ok(()));
    }
}

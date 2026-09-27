//! Editing operations on a [`Sketch`]: pure functions from a sketch to the
//! edited sketch. The caller stores the result with one `UpdateSketch`
//! command, so every operation is one undo step.
//!
//! Every operation first checks the input's structure, never panics, and
//! returns a typed [`SketchError`]. Every operation keeps two invariants:
//! the result is structurally valid, and every point is used by at least
//! one face (points left unused are removed). Point and face ids of
//! surviving elements never change; new ids are the next free ids
//! ([`next_point_id`] / [`next_face_id`] of the input, then increasing).
//!
//! Point coincidence uses [`POINT_TOLERANCE`].

use super::geom::{self, P2};
use super::{
    POINT_TOLERANCE, Sketch, SketchError, SketchFace, SketchFaceKind, SketchPoint, face_polygon,
    loop_edges, loop_has_edge, next_face_id, next_point_id, validate_structure,
};

// ---------------------------------------------------------------------
// Shared helpers.
// ---------------------------------------------------------------------

fn finite2(v: P2) -> Result<(), SketchError> {
    if v.iter().all(|c| c.is_finite()) {
        Ok(())
    } else {
        Err(SketchError::NonFinite)
    }
}

fn check_kind(kind: SketchFaceKind, face: u32) -> Result<(), SketchError> {
    match kind {
        SketchFaceKind::Solid { thickness } if !(thickness.is_finite() && thickness > 0.0) => {
            Err(SketchError::InvalidThickness { face })
        }
        SketchFaceKind::Void { depth: Some(depth) } if !(depth.is_finite() && depth > 0.0) => {
            Err(SketchError::InvalidDepth { face })
        }
        _ => Ok(()),
    }
}

/// A fresh point id, or `IdSpaceExhausted` when the largest id is
/// `u32::MAX` (never reuse an id).
fn fresh_point_id(sketch: &Sketch) -> Result<u32, SketchError> {
    if sketch.points.iter().any(|p| p.id == u32::MAX) {
        return Err(SketchError::IdSpaceExhausted);
    }
    Ok(next_point_id(sketch))
}

fn fresh_face_id(sketch: &Sketch) -> Result<u32, SketchError> {
    if sketch.faces.iter().any(|f| f.id == u32::MAX) {
        return Err(SketchError::IdSpaceExhausted);
    }
    Ok(next_face_id(sketch))
}

/// Remove immediate repeats (including last == first) from a loop.
fn dedup_loop(points: &mut Vec<u32>) {
    points.dedup();
    while points.len() > 1 && points.first() == points.last() {
        points.pop();
    }
}

/// Distinct point count of a loop.
fn distinct_count(points: &[u32]) -> usize {
    let mut ids = points.to_vec();
    ids.sort_unstable();
    ids.dedup();
    ids.len()
}

/// Drop faces with fewer than three distinct points, then drop points no
/// face uses.
fn prune(sketch: &mut Sketch) {
    for face in &mut sketch.faces {
        dedup_loop(&mut face.points);
    }
    sketch.faces.retain(|f| distinct_count(&f.points) >= 3);
    let used: std::collections::BTreeSet<u32> = sketch
        .faces
        .iter()
        .flat_map(|f| f.points.iter().copied())
        .collect();
    sketch.points.retain(|p| used.contains(&p.id));
}

/// True when some face loop has `a`-`b` as consecutive points.
fn edge_exists(sketch: &Sketch, a: u32, b: u32) -> bool {
    a != b && sketch.faces.iter().any(|f| loop_has_edge(&f.points, a, b))
}

/// Insert point `new` between `a` and `b` in every loop that has the edge
/// `a`-`b` (either direction).
fn insert_into_edge(sketch: &mut Sketch, a: u32, b: u32, new: u32) {
    for face in &mut sketch.faces {
        if !loop_has_edge(&face.points, a, b) {
            continue;
        }
        let n = face.points.len();
        let mut out = Vec::with_capacity(n + 1);
        for (i, p) in face.points.iter().enumerate() {
            out.push(*p);
            let q = face.points.get((i + 1) % n).copied().unwrap_or(*p);
            if (*p == a && q == b) || (*p == b && q == a) {
                out.push(new);
            }
        }
        face.points = out;
    }
}

fn translate_points(
    sketch: &Sketch,
    ids: &std::collections::BTreeSet<u32>,
    delta: P2,
) -> Result<Sketch, SketchError> {
    finite2(delta)?;
    let mut out = sketch.clone();
    for point in &mut out.points {
        if ids.contains(&point.id) {
            point.uv = geom::add(point.uv, delta);
            finite2(point.uv)?;
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------
// Creation.
// ---------------------------------------------------------------------

/// Add a face from a (u, v) loop. Consecutive duplicate vertices
/// (including a closing duplicate of the first vertex) are dropped, the
/// loop is normalized counter-clockwise, and a vertex that coincides with
/// an existing point reuses that point, so faces drawn corner to corner
/// share their edges. Vertices that land on the middle of an existing
/// edge are not inserted into it. The face gets [`next_face_id`].
///
/// Geometric validity (for example a self-crossing loop) is not checked
/// here; see [`super::validate_face`].
pub fn add_face(
    sketch: &Sketch,
    uv_loop: &[P2],
    kind: SketchFaceKind,
) -> Result<Sketch, SketchError> {
    validate_structure(sketch)?;
    let face_id = fresh_face_id(sketch)?;
    check_kind(kind, face_id)?;
    for uv in uv_loop {
        finite2(*uv)?;
    }
    let mut vertices: Vec<P2> = Vec::with_capacity(uv_loop.len());
    for uv in uv_loop {
        if vertices
            .last()
            .is_none_or(|q| geom::dist(*q, *uv) > POINT_TOLERANCE)
        {
            vertices.push(*uv);
        }
    }
    while vertices.len() > 1
        && matches!((vertices.first(), vertices.last()),
            (Some(a), Some(b)) if geom::dist(*a, *b) <= POINT_TOLERANCE)
    {
        vertices.pop();
    }
    if vertices.len() < 3 {
        return Err(SketchError::TooFewPoints { face: face_id });
    }
    if geom::signed_area(&vertices) < 0.0 {
        vertices.reverse();
    }

    let mut out = sketch.clone();
    let mut loop_ids: Vec<u32> = Vec::with_capacity(vertices.len());
    for uv in vertices {
        let existing = out
            .points
            .iter()
            .filter(|p| geom::dist(p.uv, uv) <= POINT_TOLERANCE)
            .min_by(|a, b| {
                geom::dist(a.uv, uv)
                    .partial_cmp(&geom::dist(b.uv, uv))
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then(a.id.cmp(&b.id))
            })
            .map(|p| p.id);
        let id = match existing {
            Some(id) => id,
            None => {
                let id = fresh_point_id(&out)?;
                out.points.push(SketchPoint { id, uv });
                id
            }
        };
        loop_ids.push(id);
    }
    dedup_loop(&mut loop_ids);
    if distinct_count(&loop_ids) < 3 {
        return Err(SketchError::TooFewPoints { face: face_id });
    }
    out.faces.push(SketchFace {
        id: face_id,
        points: loop_ids,
        kind,
    });
    prune(&mut out);
    Ok(out)
}

// ---------------------------------------------------------------------
// Moving.
// ---------------------------------------------------------------------

/// Move points by `delta`. Every face that uses a moved point follows.
pub fn move_points(sketch: &Sketch, ids: &[u32], delta: P2) -> Result<Sketch, SketchError> {
    validate_structure(sketch)?;
    for id in ids {
        sketch.uv(*id)?;
    }
    translate_points(sketch, &ids.iter().copied().collect(), delta)
}

/// Move one point to `uv`.
pub fn set_point(sketch: &Sketch, id: u32, uv: P2) -> Result<Sketch, SketchError> {
    validate_structure(sketch)?;
    finite2(uv)?;
    let current = sketch.uv(id)?;
    translate_points(
        sketch,
        &std::iter::once(id).collect(),
        geom::sub(uv, current),
    )
}

/// Move edges (point pairs, either order) by `delta`: the union of their
/// points moves once.
pub fn move_edges(
    sketch: &Sketch,
    pairs: &[(u32, u32)],
    delta: P2,
) -> Result<Sketch, SketchError> {
    validate_structure(sketch)?;
    let mut ids = std::collections::BTreeSet::new();
    for (a, b) in pairs {
        if !edge_exists(sketch, *a, *b) {
            return Err(SketchError::UnknownEdge(*a, *b));
        }
        ids.insert(*a);
        ids.insert(*b);
    }
    translate_points(sketch, &ids, delta)
}

/// Move faces by `delta`: the union of their points moves once. Points
/// shared with other faces move too, so neighbours stretch to follow.
pub fn move_faces(sketch: &Sketch, face_ids: &[u32], delta: P2) -> Result<Sketch, SketchError> {
    validate_structure(sketch)?;
    let mut ids = std::collections::BTreeSet::new();
    for face_id in face_ids {
        let face = sketch
            .face(*face_id)
            .ok_or(SketchError::UnknownFace(*face_id))?;
        ids.extend(face.points.iter().copied());
    }
    translate_points(sketch, &ids, delta)
}

// ---------------------------------------------------------------------
// Refining.
// ---------------------------------------------------------------------

/// Insert a new point on edge `a`-`b` at parameter `t` in (0, 1) measured
/// from `a`. The point goes into EVERY loop that contains the edge
/// (either direction), so a boundary shared by two faces stays shared.
/// The new point's id is [`next_point_id`] of the input.
pub fn insert_point_on_edge(sketch: &Sketch, a: u32, b: u32, t: f64) -> Result<Sketch, SketchError> {
    validate_structure(sketch)?;
    if !(t.is_finite() && t > 0.0 && t < 1.0) {
        return Err(SketchError::InvalidParameter);
    }
    if !edge_exists(sketch, a, b) {
        return Err(SketchError::UnknownEdge(a, b));
    }
    let uv = geom::lerp(sketch.uv(a)?, sketch.uv(b)?, t);
    let mut out = sketch.clone();
    let id = fresh_point_id(&out)?;
    out.points.push(SketchPoint { id, uv });
    insert_into_edge(&mut out, a, b, id);
    Ok(out)
}

/// The id of a point of `face_id`'s loop at `uv`: an existing loop point
/// within tolerance, or a new point inserted into the loop edge `uv` lies
/// on (and into every other loop sharing that edge).
fn ensure_vertex(sketch: &mut Sketch, face_id: u32, uv: P2) -> Result<Option<u32>, SketchError> {
    let face = sketch
        .face(face_id)
        .ok_or(SketchError::UnknownFace(face_id))?
        .clone();
    for id in &face.points {
        if geom::dist(sketch.uv(*id)?, uv) <= POINT_TOLERANCE {
            return Ok(Some(*id));
        }
    }
    for (a, b) in loop_edges(&face.points) {
        let (pa, pb) = (sketch.uv(a)?, sketch.uv(b)?);
        if geom::point_segment_distance(uv, pa, pb) <= POINT_TOLERANCE {
            let id = fresh_point_id(sketch)?;
            sketch.points.push(SketchPoint { id, uv });
            insert_into_edge(sketch, a, b, id);
            return Ok(Some(id));
        }
    }
    Ok(None)
}

/// Where the segment enters and leaves the interior of `polygon`: the
/// consecutive pairs of boundary crossings (sorted along the segment)
/// whose midpoint lies strictly inside.
fn chords(polygon: &[P2], start: P2, end: P2) -> Vec<(P2, P2)> {
    let n = polygon.len();
    let mut crossings: Vec<(f64, P2)> = Vec::new();
    for i in 0..n {
        let (Some(a), Some(b)) = (polygon.get(i), polygon.get((i + 1) % n)) else {
            continue;
        };
        crossings.extend(geom::segment_intersections(start, end, *a, *b, POINT_TOLERANCE));
    }
    crossings.sort_by(|x, y| x.0.partial_cmp(&y.0).unwrap_or(std::cmp::Ordering::Equal));
    crossings.dedup_by(|x, y| geom::dist(x.1, y.1) <= POINT_TOLERANCE);
    crossings
        .windows(2)
        .filter_map(|pair| match pair {
            [(_, p), (_, q)] => {
                let mid = geom::lerp(*p, *q, 0.5);
                geom::strictly_inside(mid, polygon, POINT_TOLERANCE).then_some((*p, *q))
            }
            _ => None,
        })
        .collect()
}

/// Split faces with the segment `seg_start`-`seg_end`.
///
/// For each face the segment crosses, every stretch of the segment that
/// runs through the face's interior (an entry crossing followed by an
/// exit crossing) splits the face in two: the two crossing points are
/// inserted into the face loop and into every loop sharing those
/// boundary edges, and the face is replaced by two faces sharing the new
/// edge. Both inherit the kind and thickness/depth. The face keeps its
/// id for the part on the LEFT of the drawn direction; the part on the
/// right gets a new id. A non-convex face that the segment enters and
/// leaves several times is split at every such stretch (three pieces for
/// two stretches, and so on). Parts of the segment outside every face
/// are ignored; a segment that runs along a boundary splits nothing.
///
/// Returns [`SketchError::NothingToSplit`] when no face was split.
pub fn split_faces(sketch: &Sketch, seg_start: P2, seg_end: P2) -> Result<Sketch, SketchError> {
    validate_structure(sketch)?;
    finite2(seg_start)?;
    finite2(seg_end)?;
    if geom::dist(seg_start, seg_end) <= POINT_TOLERANCE {
        return Err(SketchError::InvalidParameter);
    }
    let mut out = sketch.clone();
    let mut original: Vec<u32> = out.faces.iter().map(|f| f.id).collect();
    original.sort_unstable();
    let mut split_any = false;
    for face_id in original {
        let polygon = face_polygon(&out, face_id)?;
        let mut family = vec![face_id];
        for (p, q) in chords(&polygon, seg_start, seg_end) {
            let mid = geom::lerp(p, q, 0.5);
            let mut target = None;
            for candidate in &family {
                let candidate_polygon = face_polygon(&out, *candidate)?;
                if geom::strictly_inside(mid, &candidate_polygon, POINT_TOLERANCE) {
                    target = Some(*candidate);
                    break;
                }
            }
            let Some(target) = target else { continue };
            let (Some(pid), Some(qid)) = (
                ensure_vertex(&mut out, target, p)?,
                ensure_vertex(&mut out, target, q)?,
            ) else {
                continue;
            };
            let Some(face) = out.face(target).cloned() else {
                continue;
            };
            let n = face.points.len();
            let (Some(i), Some(j)) = (
                face.points.iter().position(|id| *id == pid),
                face.points.iter().position(|id| *id == qid),
            ) else {
                continue;
            };
            if i == j || (i + 1) % n == j || (j + 1) % n == i {
                continue; // the stretch runs along an existing edge
            }
            let walk = |from: usize, to: usize| -> Vec<u32> {
                let mut ids = Vec::new();
                let mut k = from;
                loop {
                    if let Some(id) = face.points.get(k) {
                        ids.push(*id);
                    }
                    if k == to {
                        break;
                    }
                    k = (k + 1) % n;
                }
                ids
            };
            // Walking the loop from the entry point p to the exit point q
            // follows the loop orientation; for a counter-clockwise loop
            // that part lies to the right of p -> q.
            let p_to_q = walk(i, j);
            let q_to_p = walk(j, i);
            let ccw = geom::signed_area(&face_polygon(&out, target)?) > 0.0;
            let (left, right) = if ccw { (q_to_p, p_to_q) } else { (p_to_q, q_to_p) };
            let new_id = fresh_face_id(&out)?;
            if let Some(stored) = out.faces.iter_mut().find(|f| f.id == target) {
                stored.points = left;
            }
            out.faces.push(SketchFace {
                id: new_id,
                points: right,
                kind: face.kind,
            });
            family.push(new_id);
            split_any = true;
        }
    }
    if !split_any {
        return Err(SketchError::NothingToSplit);
    }
    prune(&mut out);
    Ok(out)
}

// ---------------------------------------------------------------------
// Deleting.
// ---------------------------------------------------------------------

/// Delete faces, then the points no remaining face uses.
pub fn delete_faces(sketch: &Sketch, ids: &[u32]) -> Result<Sketch, SketchError> {
    validate_structure(sketch)?;
    for id in ids {
        sketch.face(*id).ok_or(SketchError::UnknownFace(*id))?;
    }
    let mut out = sketch.clone();
    out.faces.retain(|f| !ids.contains(&f.id));
    prune(&mut out);
    Ok(out)
}

/// Delete edges by merging each edge's two points into one. The point
/// kept is the edge's FIRST point in loop sequence, read in the
/// lowest-id face that contains the edge (for loop `[.., a, b, ..]`, or
/// `[b, .., a]` through the wrap-around, `a` is kept); it keeps its
/// position. Every loop then refers to the kept point, drops the
/// resulting immediate repeats, and faces left with fewer than three
/// points are removed, as are unused points. Pairs are merged in the
/// given order; a pair whose points were already merged is skipped.
pub fn delete_edges(sketch: &Sketch, pairs: &[(u32, u32)]) -> Result<Sketch, SketchError> {
    validate_structure(sketch)?;
    for (a, b) in pairs {
        if !edge_exists(sketch, *a, *b) {
            return Err(SketchError::UnknownEdge(*a, *b));
        }
    }
    let mut out = sketch.clone();
    let mut merged_into: std::collections::BTreeMap<u32, u32> = std::collections::BTreeMap::new();
    let resolve = |merged_into: &std::collections::BTreeMap<u32, u32>, mut id: u32| {
        while let Some(next) = merged_into.get(&id) {
            id = *next;
        }
        id
    };
    for (a, b) in pairs {
        let (a, b) = (resolve(&merged_into, *a), resolve(&merged_into, *b));
        if a == b {
            continue;
        }
        let mut faces: Vec<&SketchFace> = out
            .faces
            .iter()
            .filter(|f| loop_has_edge(&f.points, a, b))
            .collect();
        faces.sort_by_key(|f| f.id);
        let kept = faces
            .first()
            .and_then(|face| {
                loop_edges(&face.points).find_map(|(p, q)| {
                    if p == a && q == b {
                        Some(a)
                    } else if p == b && q == a {
                        Some(b)
                    } else {
                        None
                    }
                })
            })
            .unwrap_or(a);
        let removed = if kept == a { b } else { a };
        for face in &mut out.faces {
            for id in &mut face.points {
                if *id == removed {
                    *id = kept;
                }
            }
            dedup_loop(&mut face.points);
        }
        merged_into.insert(removed, kept);
    }
    prune(&mut out);
    Ok(out)
}

/// Delete points. Each loop that used a deleted point reconnects its
/// neighbours (the edges at that point are removed); faces left with
/// fewer than three points are removed, then unused points.
pub fn delete_points(sketch: &Sketch, ids: &[u32]) -> Result<Sketch, SketchError> {
    validate_structure(sketch)?;
    for id in ids {
        sketch.uv(*id)?;
    }
    let mut out = sketch.clone();
    for face in &mut out.faces {
        face.points.retain(|p| !ids.contains(p));
    }
    out.points.retain(|p| !ids.contains(&p.id));
    prune(&mut out);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::super::{edges, validate, validate_face};
    use super::*;

    const SOLID: SketchFaceKind = SketchFaceKind::Solid { thickness: 0.3 };

    fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> Vec<P2> {
        vec![[x0, y0], [x1, y0], [x1, y1], [x0, y1]]
    }

    fn one_square() -> Sketch {
        add_face(&Sketch::default(), &rect(0.0, 0.0, 2.0, 2.0), SOLID).unwrap_or_default()
    }

    /// Two unit squares side by side sharing the edge x = 1.
    fn two_squares() -> Sketch {
        let s = add_face(&Sketch::default(), &rect(0.0, 0.0, 1.0, 1.0), SOLID).unwrap_or_default();
        add_face(&s, &rect(1.0, 0.0, 2.0, 1.0), SOLID).unwrap_or_default()
    }

    fn id_at(sketch: &Sketch, uv: P2) -> u32 {
        sketch
            .points
            .iter()
            .find(|p| geom::dist(p.uv, uv) < 1e-9)
            .map(|p| p.id)
            .unwrap_or(u32::MAX)
    }

    fn area(sketch: &Sketch, face: u32) -> f64 {
        geom::signed_area(&face_polygon(sketch, face).unwrap_or_default())
    }

    #[test]
    fn add_face_normalizes_ccw_and_reuses_coincident_points() {
        // Clockwise input with a closing duplicate.
        let cw = vec![[0.0, 0.0], [0.0, 1.0], [1.0, 1.0], [1.0, 0.0], [0.0, 0.0]];
        let s = add_face(&Sketch::default(), &cw, SOLID).unwrap_or_default();
        assert_eq!(s.points.len(), 4);
        assert_eq!(s.faces.len(), 1);
        assert!(area(&s, 0) > 0.0, "normalized counter-clockwise");

        let two = two_squares();
        assert_eq!(two.points.len(), 6, "the shared corners are reused");
        let shared: Vec<_> = edges(&two).into_iter().filter(|e| e.faces.len() == 2).collect();
        assert_eq!(shared.len(), 1, "one shared edge");
        assert_eq!(validate(&two), Ok(()));

        assert_eq!(
            add_face(&Sketch::default(), &[[0.0, 0.0], [1.0, 0.0]], SOLID).err(),
            Some(SketchError::TooFewPoints { face: 0 })
        );
        assert_eq!(
            add_face(
                &Sketch::default(),
                &rect(0.0, 0.0, 1.0, 1.0),
                SketchFaceKind::Solid { thickness: -1.0 }
            )
            .err(),
            Some(SketchError::InvalidThickness { face: 0 })
        );
        assert_eq!(
            add_face(&Sketch::default(), &[[0.0, 0.0], [f64::NAN, 1.0], [1.0, 1.0]], SOLID).err(),
            Some(SketchError::NonFinite)
        );
    }

    #[test]
    fn moves_follow_planar_graph_semantics() {
        let two = two_squares();
        let a = id_at(&two, [1.0, 0.0]);
        let b = id_at(&two, [1.0, 1.0]);

        let moved = move_points(&two, &[a], [0.25, 0.0]).unwrap_or_default();
        assert_eq!(moved.uv(a), Ok([1.25, 0.0]));

        let set = set_point(&two, a, [1.5, -0.5]).unwrap_or_default();
        assert_eq!(set.uv(a), Ok([1.5, -0.5]));

        // Moving the shared edge moves both faces' boundary.
        let edge = move_edges(&two, &[(b, a)], [0.5, 0.0]).unwrap_or_default();
        assert_eq!(edge.uv(a), Ok([1.5, 0.0]));
        assert_eq!(edge.uv(b), Ok([1.5, 1.0]));
        assert!((area(&edge, 0) - 1.5).abs() < 1e-12);
        assert!((area(&edge, 1) - 0.5).abs() < 1e-12);

        // Moving face 0 drags the shared points: face 1 stretches.
        let face = move_faces(&two, &[0], [0.0, 1.0]).unwrap_or_default();
        assert_eq!(face.uv(a), Ok([1.0, 1.0]));
        assert_eq!(face.uv(id_at(&two, [2.0, 0.0])), Ok([2.0, 0.0]));

        assert_eq!(move_points(&two, &[99], [1.0, 0.0]).err(), Some(SketchError::UnknownPoint(99)));
        let far = id_at(&two, [0.0, 0.0]);
        assert_eq!(
            move_edges(&two, &[(far, id_at(&two, [2.0, 1.0]))], [1.0, 0.0]).err(),
            Some(SketchError::UnknownEdge(far, id_at(&two, [2.0, 1.0])))
        );
        assert_eq!(move_faces(&two, &[5], [1.0, 0.0]).err(), Some(SketchError::UnknownFace(5)));
        assert_eq!(
            move_points(&two, &[a], [f64::INFINITY, 0.0]).err(),
            Some(SketchError::NonFinite)
        );
    }

    #[test]
    fn insert_point_on_a_shared_edge_goes_into_both_loops() {
        let two = two_squares();
        let a = id_at(&two, [1.0, 0.0]);
        let b = id_at(&two, [1.0, 1.0]);
        let new_id = next_point_id(&two);
        let s = insert_point_on_edge(&two, b, a, 0.25).unwrap_or_default();
        assert_eq!(s.uv(new_id), Ok([1.0, 0.75]));
        for face in &s.faces {
            assert!(face.points.contains(&new_id), "face {} got the point", face.id);
            assert!(loop_has_edge(&face.points, a, new_id));
            assert!(loop_has_edge(&face.points, new_id, b));
            assert!(!loop_has_edge(&face.points, a, b), "the old edge is gone");
        }
        assert_eq!(validate(&s), Ok(()));
        assert_eq!(
            insert_point_on_edge(&two, a, b, 1.0).err(),
            Some(SketchError::InvalidParameter)
        );
        let c = id_at(&two, [0.0, 0.0]);
        assert_eq!(
            insert_point_on_edge(&two, c, b, 0.5).err(),
            Some(SketchError::UnknownEdge(c, b))
        );
    }

    #[test]
    fn split_through_a_convex_face_keeps_left_part_id() {
        let square = one_square();
        let next_face = next_face_id(&square);
        // Drawn upward through x = 0.5, both ends outside.
        let s = split_faces(&square, [0.5, -1.0], [0.5, 3.0]).unwrap_or_default();
        assert_eq!(s.faces.len(), 2);
        assert_eq!(s.points.len(), 6, "two crossing points inserted");
        // Face 0 (kept id) is the part LEFT of the upward direction.
        let left = face_polygon(&s, 0).unwrap_or_default();
        assert!(left.iter().all(|p| p[0] <= 0.5 + 1e-12), "{left:?}");
        assert!((area(&s, 0) - 1.0).abs() < 1e-12);
        assert!((area(&s, next_face) - 3.0).abs() < 1e-12);
        assert_eq!(s.face(next_face).map(|f| f.kind), Some(SOLID), "kind inherited");
        assert_eq!(validate(&s), Ok(()));
        // The two pieces share the new edge.
        let shared: Vec<_> = edges(&s).into_iter().filter(|e| e.faces.len() == 2).collect();
        assert_eq!(shared.len(), 1);

        // Drawn downward: the kept id is again the left part (now x > 0.5).
        let s = split_faces(&square, [0.5, 3.0], [0.5, -1.0]).unwrap_or_default();
        assert!((area(&s, 0) - 3.0).abs() < 1e-12);

        // A segment that misses, or runs along the boundary, splits nothing.
        assert_eq!(
            split_faces(&square, [3.0, 0.0], [3.0, 2.0]).err(),
            Some(SketchError::NothingToSplit)
        );
        assert_eq!(
            split_faces(&square, [-1.0, 0.0], [3.0, 0.0]).err(),
            Some(SketchError::NothingToSplit)
        );
        // A segment ending inside the face enters without leaving.
        assert_eq!(
            split_faces(&square, [0.5, -1.0], [0.5, 1.0]).err(),
            Some(SketchError::NothingToSplit)
        );
    }

    #[test]
    fn split_next_to_a_neighbour_keeps_the_shared_boundary_consistent() {
        // Split both squares with one horizontal line at y = 0.5: the
        // crossing on the shared edge x = 1 is inserted once and used by
        // all four resulting faces.
        let two = two_squares();
        let s = split_faces(&two, [-1.0, 0.5], [3.0, 0.5]).unwrap_or_default();
        assert_eq!(s.faces.len(), 4);
        let mid = id_at(&s, [1.0, 0.5]);
        assert_ne!(mid, u32::MAX, "the shared-edge crossing exists once");
        let users = s.faces.iter().filter(|f| f.points.contains(&mid)).count();
        assert_eq!(users, 4);
        assert_eq!(s.points.len(), 9);
        for face in &s.faces {
            assert!((area(&s, face.id) - 0.5).abs() < 1e-12);
            assert_eq!(validate_face(&s, face.id), Ok(()));
        }

        // Splitting only the right square inserts its crossing on the
        // shared edge into the LEFT square's loop too.
        let s = split_faces(&two, [1.5, -1.0], [1.5, 2.0]).unwrap_or_default();
        assert_eq!(s.faces.len(), 3);
        assert_eq!(validate(&s), Ok(()));
        let s = split_faces(&two, [0.5, 0.5], [3.0, 0.5]).unwrap_or_default();
        let on_shared = id_at(&s, [1.0, 0.5]);
        let left_square = s.face(0).map(|f| f.points.clone()).unwrap_or_default();
        assert!(left_square.contains(&on_shared), "neighbour loop stays consistent");
        assert_eq!(validate(&s), Ok(()));
    }

    #[test]
    fn split_of_a_non_convex_face_cuts_every_stretch() {
        // A U shape: the horizontal line y = 1.5 enters and leaves twice.
        let u = vec![
            [0.0, 0.0],
            [3.0, 0.0],
            [3.0, 2.0],
            [2.0, 2.0],
            [2.0, 1.0],
            [1.0, 1.0],
            [1.0, 2.0],
            [0.0, 2.0],
        ];
        let s = add_face(&Sketch::default(), &u, SOLID).unwrap_or_default();
        let total = area(&s, 0);
        let split = split_faces(&s, [-1.0, 1.5], [4.0, 1.5]).unwrap_or_default();
        assert_eq!(split.faces.len(), 3, "base + two arm tips");
        let sum: f64 = split.faces.iter().map(|f| area(&split, f.id)).sum();
        assert!((sum - total).abs() < 1e-12);
        assert_eq!(validate(&split), Ok(()));
    }

    #[test]
    fn delete_faces_drops_unused_points_only() {
        let two = two_squares();
        let s = delete_faces(&two, &[1]).unwrap_or_default();
        assert_eq!(s.faces.len(), 1);
        assert_eq!(s.points.len(), 4, "shared corners survive, private ones go");
        assert_eq!(delete_faces(&two, &[7]).err(), Some(SketchError::UnknownFace(7)));
    }

    #[test]
    fn delete_edge_merges_into_the_first_point_in_loop_order() {
        let square = one_square();
        // Loop is counter-clockwise from (0,0): 0 -> (2,0) -> (2,2) -> (0,2).
        let a = id_at(&square, [2.0, 0.0]);
        let b = id_at(&square, [2.0, 2.0]);
        // Given as (b, a), the kept point is still a (first in loop order).
        let s = delete_edges(&square, &[(b, a)]).unwrap_or_default();
        assert_eq!(s.faces.len(), 1);
        assert_eq!(s.points.len(), 3);
        assert_eq!(s.uv(a), Ok([2.0, 0.0]), "kept point keeps its position");
        assert!(s.point(b).is_none());

        // Through the wrap-around: edge (last, first) keeps the last.
        let first = id_at(&square, [0.0, 0.0]);
        let last = id_at(&square, [0.0, 2.0]);
        let s = delete_edges(&square, &[(first, last)]).unwrap_or_default();
        assert!(s.point(last).is_some());
        assert!(s.point(first).is_none());

        // Collapsing a triangle removes the face and its points.
        let tri = add_face(&Sketch::default(), &[[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]], SOLID)
            .unwrap_or_default();
        let (p, q) = (id_at(&tri, [0.0, 0.0]), id_at(&tri, [1.0, 0.0]));
        let s = delete_edges(&tri, &[(p, q)]).unwrap_or_default();
        assert!(s.faces.is_empty());
        assert!(s.points.is_empty());

        // A shared edge merges in both loops.
        let two = two_squares();
        let (m, n) = (id_at(&two, [1.0, 0.0]), id_at(&two, [1.0, 1.0]));
        let s = delete_edges(&two, &[(m, n)]).unwrap_or_default();
        assert_eq!(s.faces.len(), 2, "both squares become triangles");
        assert_eq!(s.points.len(), 5);
        assert_eq!(validate(&s), Ok(()));

        assert_eq!(
            delete_edges(&square, &[(first, b)]).err(),
            Some(SketchError::UnknownEdge(first, b))
        );
    }

    #[test]
    fn delete_points_reconnects_neighbours() {
        let square = one_square();
        let corner = id_at(&square, [2.0, 2.0]);
        let s = delete_points(&square, &[corner]).unwrap_or_default();
        assert_eq!(s.faces.len(), 1);
        assert_eq!(s.faces.first().map(|f| f.points.len()), Some(3));
        assert!((area(&s, 0) - 2.0).abs() < 1e-12, "a triangle remains");

        // Two corners of a square leave two points: the face goes.
        let other = id_at(&square, [0.0, 0.0]);
        let s = delete_points(&square, &[corner, other]).unwrap_or_default();
        assert!(s.faces.is_empty());
        assert!(s.points.is_empty());

        assert_eq!(delete_points(&square, &[42]).err(), Some(SketchError::UnknownPoint(42)));
    }

    #[test]
    fn operations_reject_structurally_invalid_input() {
        let mut bad = one_square();
        bad.points.push(SketchPoint { id: 0, uv: [9.0, 9.0] });
        assert_eq!(
            move_points(&bad, &[0], [1.0, 0.0]).err(),
            Some(SketchError::DuplicatePointId(0))
        );
        let mut exhausted = one_square();
        if let Some(p) = exhausted.points.last_mut() {
            p.id = u32::MAX;
        }
        if let Some(f) = exhausted.faces.first_mut()
            && let Some(last) = f.points.last_mut()
        {
            *last = u32::MAX;
        }
        assert_eq!(
            add_face(&exhausted, &rect(5.0, 5.0, 6.0, 6.0), SOLID).err(),
            Some(SketchError::IdSpaceExhausted)
        );
    }
}

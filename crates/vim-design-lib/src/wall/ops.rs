//! Editing operations on a wall profile that keep the top anchors
//! consistent.
//!
//! Each operation takes the STORED profile, its top-anchored ids, and
//! the wall's current top reference height, applies the matching
//! [`crate::sketch::ops`] operation in EFFECTIVE coordinates (what the
//! user sees), and returns the stored profile and anchors to put back
//! with one `UpdateWall`.
//!
//! Anchor rules:
//! - a surviving point keeps its anchor, whether or not it moved;
//! - a removed point loses its anchor (a merged edge keeps the kept
//!   point's anchor);
//! - a point an operation creates is top-anchored exactly when it lies
//!   on an edge between two top-anchored points (a point inserted on the
//!   top edge follows the top).
//!
//! A point whose effective position an operation does not change keeps
//! its stored coordinates bit for bit.

use crate::sketch::geom::{P2, point_segment_distance};
use crate::sketch::{self, POINT_TOLERANCE, Sketch, SketchError, SketchFaceKind};

use super::{effective_profile, stored_profile};

/// The stored profile and its top-anchored ids.
pub type AnchoredProfile = (Sketch, Vec<u32>);

fn finite(height: f64) -> Result<(), SketchError> {
    if height.is_finite() {
        Ok(())
    } else {
        Err(SketchError::NonFinite)
    }
}

/// Run a sketch operation in effective coordinates and store back.
fn edit(
    profile: &Sketch,
    top_points: &[u32],
    height: f64,
    op: impl FnOnce(&Sketch) -> Result<Sketch, SketchError>,
) -> Result<AnchoredProfile, SketchError> {
    finite(height)?;
    let before = effective_profile(profile, top_points, height);
    let after = op(&before)?;

    // Edges between two anchored points, for anchoring new points.
    let anchored_edges: Vec<(P2, P2)> = sketch::edges(&before)
        .into_iter()
        .filter(|e| top_points.contains(&e.a) && top_points.contains(&e.b))
        .filter_map(|e| Some((before.uv(e.a).ok()?, before.uv(e.b).ok()?)))
        .collect();
    let mut anchors: Vec<u32> = Vec::new();
    for point in &after.points {
        let anchored = match before.point(point.id) {
            Some(_) => top_points.contains(&point.id),
            None => anchored_edges
                .iter()
                .any(|(a, b)| point_segment_distance(point.uv, *a, *b) <= POINT_TOLERANCE),
        };
        if anchored {
            anchors.push(point.id);
        }
    }
    anchors.sort_unstable();
    anchors.dedup();

    let mut stored = stored_profile(&after, &anchors, height);
    for point in &mut stored.points {
        let unchanged = before
            .point(point.id)
            .zip(after.point(point.id))
            .is_some_and(|(b, a)| b.uv == a.uv);
        if unchanged && let Some(original) = profile.point(point.id) {
            point.uv = original.uv;
        }
    }
    Ok((stored, anchors))
}

/// Add a face drawn in effective coordinates; see
/// [`sketch::ops::add_face`].
pub fn add_face(
    profile: &Sketch,
    top_points: &[u32],
    height: f64,
    uv_loop: &[P2],
    kind: SketchFaceKind,
) -> Result<AnchoredProfile, SketchError> {
    edit(profile, top_points, height, |s| sketch::ops::add_face(s, uv_loop, kind))
}

/// Move points; see [`sketch::ops::move_points`].
pub fn move_points(
    profile: &Sketch,
    top_points: &[u32],
    height: f64,
    ids: &[u32],
    delta: P2,
) -> Result<AnchoredProfile, SketchError> {
    edit(profile, top_points, height, |s| sketch::ops::move_points(s, ids, delta))
}

/// Move a point to an effective position; see [`sketch::ops::set_point`].
pub fn set_point(
    profile: &Sketch,
    top_points: &[u32],
    height: f64,
    id: u32,
    uv: P2,
) -> Result<AnchoredProfile, SketchError> {
    edit(profile, top_points, height, |s| sketch::ops::set_point(s, id, uv))
}

/// Move edges; see [`sketch::ops::move_edges`].
pub fn move_edges(
    profile: &Sketch,
    top_points: &[u32],
    height: f64,
    pairs: &[(u32, u32)],
    delta: P2,
) -> Result<AnchoredProfile, SketchError> {
    edit(profile, top_points, height, |s| sketch::ops::move_edges(s, pairs, delta))
}

/// Move faces; see [`sketch::ops::move_faces`].
pub fn move_faces(
    profile: &Sketch,
    top_points: &[u32],
    height: f64,
    face_ids: &[u32],
    delta: P2,
) -> Result<AnchoredProfile, SketchError> {
    edit(profile, top_points, height, |s| sketch::ops::move_faces(s, face_ids, delta))
}

/// Insert a point on an edge; see [`sketch::ops::insert_point_on_edge`].
/// The point is top-anchored when both edge ends are.
pub fn insert_point_on_edge(
    profile: &Sketch,
    top_points: &[u32],
    height: f64,
    a: u32,
    b: u32,
    t: f64,
) -> Result<AnchoredProfile, SketchError> {
    edit(profile, top_points, height, |s| sketch::ops::insert_point_on_edge(s, a, b, t))
}

/// Split faces with a segment in effective coordinates; see
/// [`sketch::ops::split_faces`].
pub fn split_faces(
    profile: &Sketch,
    top_points: &[u32],
    height: f64,
    seg_start: P2,
    seg_end: P2,
) -> Result<AnchoredProfile, SketchError> {
    edit(profile, top_points, height, |s| sketch::ops::split_faces(s, seg_start, seg_end))
}

/// Delete faces; see [`sketch::ops::delete_faces`].
pub fn delete_faces(
    profile: &Sketch,
    top_points: &[u32],
    height: f64,
    ids: &[u32],
) -> Result<AnchoredProfile, SketchError> {
    edit(profile, top_points, height, |s| sketch::ops::delete_faces(s, ids))
}

/// Delete edges by merging their points; see
/// [`sketch::ops::delete_edges`]. The kept point keeps its anchor.
pub fn delete_edges(
    profile: &Sketch,
    top_points: &[u32],
    height: f64,
    pairs: &[(u32, u32)],
) -> Result<AnchoredProfile, SketchError> {
    edit(profile, top_points, height, |s| sketch::ops::delete_edges(s, pairs))
}

/// Delete points; see [`sketch::ops::delete_points`].
pub fn delete_points(
    profile: &Sketch,
    top_points: &[u32],
    height: f64,
    ids: &[u32],
) -> Result<AnchoredProfile, SketchError> {
    edit(profile, top_points, height, |s| sketch::ops::delete_points(s, ids))
}

/// Anchor points to the top reference (`top = true`) or to the base
/// (`top = false`), converting their stored v so their effective position
/// does not move.
pub fn set_anchor(
    profile: &Sketch,
    top_points: &[u32],
    height: f64,
    ids: &[u32],
    top: bool,
) -> Result<AnchoredProfile, SketchError> {
    finite(height)?;
    for id in ids {
        profile.uv(*id)?;
    }
    let effective = effective_profile(profile, top_points, height);
    let mut anchors: Vec<u32> = top_points.to_vec();
    if top {
        anchors.extend_from_slice(ids);
    } else {
        anchors.retain(|id| !ids.contains(id));
    }
    anchors.sort_unstable();
    anchors.dedup();
    let mut stored = stored_profile(&effective, &anchors, height);
    for point in &mut stored.points {
        let changed = top_points.contains(&point.id) != anchors.contains(&point.id);
        if !changed && let Some(original) = profile.point(point.id) {
            point.uv = original.uv;
        }
    }
    Ok((stored, anchors))
}

#[cfg(test)]
mod tests {
    use super::super::default_profile;
    use super::*;

    const H: f64 = 2.7;

    fn uv(profile: &Sketch, top: &[u32], id: u32) -> P2 {
        effective_profile(profile, top, H)
            .uv(id)
            .unwrap_or([f64::NAN, f64::NAN])
    }

    #[test]
    fn inserting_on_the_top_edge_anchors_the_new_point() {
        let (profile, top) = default_profile(4.0, 0.2);
        let new_id = sketch::next_point_id(&profile);
        // Top edge 2-3 (both anchored): the new point follows the top.
        let (p, t) = insert_point_on_edge(&profile, &top, H, 2, 3, 0.5).unwrap_or_default();
        assert!(t.contains(&new_id));
        assert_eq!(p.uv(new_id), Ok([2.0, 0.0]), "stored v measured from the top");
        assert_eq!(uv(&p, &t, new_id), [2.0, H]);
        // Bottom edge 0-1 (not anchored): the new point stays on the base.
        let (p, t) = insert_point_on_edge(&profile, &top, H, 0, 1, 0.5).unwrap_or_default();
        assert!(!t.contains(&new_id));
        assert_eq!(p.uv(new_id), Ok([2.0, 0.0]));
        // A side edge 1-2 (one anchored end): not anchored.
        let (_, t) = insert_point_on_edge(&profile, &top, H, 1, 2, 0.5).unwrap_or_default();
        assert!(!t.contains(&new_id));
    }

    #[test]
    fn moved_points_keep_their_anchor_and_untouched_points_their_bits() {
        let (profile, top) = default_profile(4.0, 0.2);
        // Raise the top-left corner by 1 m: a gable-like slope.
        let (p, t) = move_points(&profile, &top, H, &[3], [0.0, 1.0]).unwrap_or_default();
        assert_eq!(t, top, "anchors unchanged");
        assert_eq!(p.uv(3), Ok([0.0, 1.0]), "stored relative to the top");
        assert_eq!(uv(&p, &t, 3), [0.0, H + 1.0]);
        for id in [0, 1, 2] {
            assert_eq!(p.uv(id), profile.uv(id), "point {id} untouched");
        }
    }

    #[test]
    fn a_window_keeps_its_sill_when_drawn() {
        let (profile, top) = default_profile(4.0, 0.2);
        let window = [[1.0, 0.9], [2.2, 0.9], [2.2, 2.1], [1.0, 2.1]];
        let (p, t) =
            add_face(&profile, &top, H, &window, SketchFaceKind::Void { depth: None })
                .unwrap_or_default();
        assert_eq!(t, top, "window corners are not anchored");
        assert_eq!(p.faces.len(), 2);
        // A different top height leaves the window where it is.
        let taller = effective_profile(&p, &t, 3.5);
        assert_eq!(sketch::face_polygon(&taller, 1).ok().and_then(|poly| poly.first().copied()), Some([1.0, 0.9]));
    }

    #[test]
    fn deleting_and_merging_drop_or_keep_anchors() {
        let (profile, top) = default_profile(4.0, 0.2);
        let (p, t) = insert_point_on_edge(&profile, &top, H, 2, 3, 0.5).unwrap_or_default();
        let apex = sketch::next_point_id(&profile);
        let (_, t2) = delete_points(&p, &t, H, &[apex]).unwrap_or_default();
        assert_eq!(t2, top, "deleted point loses its anchor");
        // Merge edge 1-2 (right side): the kept point is 1 (base).
        let (merged, t3) = delete_edges(&profile, &top, H, &[(1, 2)]).unwrap_or_default();
        assert!(merged.point(2).is_none());
        assert_eq!(t3, vec![3]);
    }

    #[test]
    fn split_through_the_top_edge_anchors_the_crossing() {
        let (profile, top) = default_profile(4.0, 0.2);
        let (p, t) = split_faces(&profile, &top, H, [2.0, -1.0], [2.0, 5.0]).unwrap_or_default();
        assert_eq!(p.faces.len(), 2);
        let effective = effective_profile(&p, &t, H);
        let top_crossing = effective
            .points
            .iter()
            .find(|q| (q.uv[0] - 2.0).abs() < 1e-9 && (q.uv[1] - H).abs() < 1e-9)
            .map(|q| q.id);
        let bottom_crossing = effective
            .points
            .iter()
            .find(|q| (q.uv[0] - 2.0).abs() < 1e-9 && q.uv[1].abs() < 1e-9)
            .map(|q| q.id);
        assert!(top_crossing.is_some_and(|id| t.contains(&id)));
        assert!(bottom_crossing.is_some_and(|id| !t.contains(&id)));
    }

    #[test]
    fn set_anchor_converts_without_a_jump() {
        let (profile, top) = default_profile(4.0, 0.2);
        let (p, t) = set_anchor(&profile, &top, H, &[3], false).unwrap_or_default();
        assert_eq!(t, vec![2]);
        assert_eq!(p.uv(3), Ok([0.0, H]), "now stored from the base");
        assert_eq!(uv(&p, &t, 3), [0.0, H]);
        let (back, t) = set_anchor(&p, &t, H, &[3], true).unwrap_or_default();
        assert_eq!(t, top);
        assert_eq!(back.uv(3), Ok([0.0, 0.0]));
        assert_eq!(
            set_anchor(&profile, &top, H, &[42], true).err(),
            Some(SketchError::UnknownPoint(42))
        );
        assert_eq!(
            move_points(&profile, &top, f64::NAN, &[0], [1.0, 0.0]).err(),
            Some(SketchError::NonFinite)
        );
    }
}

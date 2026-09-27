//! Editing operations on wall-run data.
//!
//! Each operation takes a [`WallRunData`], returns the edited copy to
//! put back with one `UpdateWallRun`, and checks the result with
//! [`validate`]: an edit that would leave an invalid run (an opening in
//! a join zone, a self-crossing line) is an error, and the input is not
//! changed. Values an operation does not change are copied bit for bit.
//!
//! Openings and custom profiles belong to segments, and a segment is
//! named by its start point id:
//! - a segment whose start and end points both survive, consecutively,
//!   keeps its openings and its profile;
//! - where points are removed and segments merge, each opening keeps its
//!   plan position: it moves to the merged segment that contains it
//!   (same direction) or the edit is an error; the custom profiles of
//!   merged segments are dropped;
//! - the openings and profile of a deleted edge go with it.

use crate::sketch::{POINT_TOLERANCE, Sketch};

use super::{
    Opening, OpeningKind, P2, RunPoint, SegmentProfile, WallRunData, WallRunError, add, dist, dot,
    scale, segment_clear_span, sub, unit, validate,
};

fn finite(values: &[f64]) -> Result<(), WallRunError> {
    if values.iter().all(|v| v.is_finite()) {
        Ok(())
    } else {
        Err(WallRunError::NonFinite)
    }
}

fn checked(run: WallRunData) -> Result<WallRunData, WallRunError> {
    validate(&run)?;
    Ok(run)
}

fn point_index(run: &WallRunData, id: u32) -> Result<usize, WallRunError> {
    run.points
        .iter()
        .position(|p| p.id == id)
        .ok_or(WallRunError::UnknownPoint(id))
}

fn segment_index(run: &WallRunData, segment: u32) -> Result<usize, WallRunError> {
    run.segment_index(segment).ok_or(WallRunError::UnknownSegment(segment))
}

/// (start id, end id) of every segment.
fn segment_pairs(run: &WallRunData) -> Vec<(u32, u32)> {
    let n = run.points.len();
    (0..run.segment_count())
        .filter_map(|i| Some((run.points.get(i)?.id, run.points.get((i + 1) % n)?.id)))
        .collect()
}

/// Re-assign openings and profiles after the point list changed (see the
/// module rules). `dropped` segments lose their openings and profile.
fn resegment(
    old: &WallRunData,
    points: Vec<RunPoint>,
    closed: bool,
    dropped: &[u32],
) -> Result<WallRunData, WallRunError> {
    let mut new = WallRunData {
        points,
        closed,
        ..old.clone()
    };
    let min_points = if closed { 3 } else { 2 };
    if new.points.len() < min_points {
        return Err(WallRunError::TooFewPoints);
    }
    let old_pairs = segment_pairs(old);
    let new_pairs = segment_pairs(&new);
    let survives = |segment: u32| -> bool {
        old_pairs
            .iter()
            .find(|(s, _)| *s == segment)
            .is_some_and(|pair| new_pairs.contains(pair))
    };
    let mut openings = Vec::with_capacity(old.openings.len());
    for opening in &old.openings {
        if dropped.contains(&opening.segment) {
            continue;
        }
        if survives(opening.segment) {
            openings.push(*opening);
            continue;
        }
        let index = segment_index(old, opening.segment)?;
        let (a, b) = old
            .segment_ends(index)
            .ok_or(WallRunError::UnknownSegment(opening.segment))?;
        let dir = unit(a, b).ok_or(WallRunError::ZeroLengthSegment(opening.segment))?;
        let p = add(a, scale(dir, opening.offset_m));
        let q = add(a, scale(dir, opening.offset_m + opening.width_m));
        let mut placed = None;
        for (k, (start, _)) in new_pairs.iter().enumerate() {
            let Some((na, nb)) = new.segment_ends(k) else { continue };
            let Some(nd) = unit(na, nb) else { continue };
            let on = |x: P2| {
                crate::sketch::geom::point_segment_distance(x, na, nb) <= POINT_TOLERANCE
            };
            if dot(dir, nd) > 1.0 - 1e-9 && on(p) && on(q) {
                placed = Some((*start, dot(sub(p, na), nd)));
                break;
            }
        }
        let (segment, offset_m) = placed.ok_or(WallRunError::OpeningDisplaced(opening.id))?;
        openings.push(Opening {
            segment,
            offset_m,
            ..*opening
        });
    }
    new.openings = openings;
    new.profiles = old
        .profiles
        .iter()
        .filter(|p| !dropped.contains(&p.segment) && survives(p.segment))
        .cloned()
        .collect();
    checked(new)
}

// ---------------------------------------------------------------------
// Points and edges.
// ---------------------------------------------------------------------

/// Move points by `delta` (base-plane meters). Openings keep their
/// offsets along their segments.
pub fn move_points(run: &WallRunData, ids: &[u32], delta: P2) -> Result<WallRunData, WallRunError> {
    finite(&delta)?;
    let mut out = run.clone();
    for id in ids {
        point_index(run, *id)?;
    }
    for point in &mut out.points {
        if ids.contains(&point.id) {
            point.uv = add(point.uv, delta);
        }
    }
    checked(out)
}

/// Put one point at `uv`.
pub fn set_point(run: &WallRunData, id: u32, uv: P2) -> Result<WallRunData, WallRunError> {
    finite(&uv)?;
    let index = point_index(run, id)?;
    let mut out = run.clone();
    if let Some(point) = out.points.get_mut(index) {
        point.uv = uv;
    }
    checked(out)
}

/// Move segments (both end points of each) by `delta`.
pub fn move_edges(run: &WallRunData, segments: &[u32], delta: P2) -> Result<WallRunData, WallRunError> {
    let pairs = segment_pairs(run);
    let mut ids = Vec::new();
    for segment in segments {
        let (start, end) = pairs
            .iter()
            .find(|(s, _)| s == segment)
            .copied()
            .ok_or(WallRunError::UnknownSegment(*segment))?;
        ids.push(start);
        ids.push(end);
    }
    ids.sort_unstable();
    ids.dedup();
    move_points(run, &ids, delta)
}

/// Insert a point on segment `segment`, `offset_m` along it from its
/// start. Returns the run and the new point id (the next free id).
///
/// Openings of the split segment go to the part they lie in (the second
/// part is the new segment, named by the new point; offsets become
/// relative to it); an opening across the new point is an error. A
/// custom profile is split at the new point in effective coordinates
/// for top reference `height` (the current one, as for
/// [`crate::wall::ops`]): each face goes to the part its points lie in,
/// with its top anchors; a part without faces gets no profile.
pub fn insert_point(
    run: &WallRunData,
    segment: u32,
    offset_m: f64,
    height: f64,
) -> Result<(WallRunData, u32), WallRunError> {
    finite(&[offset_m, height])?;
    let index = segment_index(run, segment)?;
    let (a, b) = run.segment_ends(index).ok_or(WallRunError::UnknownSegment(segment))?;
    let length = dist(a, b);
    if offset_m <= POINT_TOLERANCE || offset_m >= length - POINT_TOLERANCE {
        return Err(WallRunError::InvalidParameter);
    }
    let dir = unit(a, b).ok_or(WallRunError::ZeroLengthSegment(segment))?;
    let id = run.next_point_id();
    let mut out = run.clone();
    out.points.insert(
        index + 1,
        RunPoint {
            id,
            uv: add(a, scale(dir, offset_m)),
        },
    );
    for opening in &mut out.openings {
        if opening.segment != segment {
            continue;
        }
        if opening.offset_m + opening.width_m <= offset_m + POINT_TOLERANCE {
            continue;
        }
        if opening.offset_m >= offset_m - POINT_TOLERANCE {
            opening.segment = id;
            opening.offset_m -= offset_m;
            continue;
        }
        return Err(WallRunError::OpeningStraddlesSplit(opening.id));
    }
    if let Some(position) = out.profiles.iter().position(|p| p.segment == segment) {
        let custom = out.profiles.remove(position);
        let (first, second) = split_profile(&custom, offset_m, height)?;
        let mut insert_at = position;
        if let Some((profile, top_points)) = first {
            out.profiles.insert(insert_at, SegmentProfile { segment, profile, top_points });
            insert_at += 1;
        }
        if let Some((profile, top_points)) = second {
            out.profiles.insert(insert_at, SegmentProfile { segment: id, profile, top_points });
        }
    }
    Ok((checked(out)?, id))
}

type Anchored = (Sketch, Vec<u32>);

/// Split a segment profile at u = `at`: the part before (as is) and the
/// part after (shifted to start at u = 0).
fn split_profile(
    custom: &SegmentProfile,
    at: f64,
    height: f64,
) -> Result<(Option<Anchored>, Option<Anchored>), WallRunError> {
    let err = |error| WallRunError::Profile { segment: custom.segment, error };
    let effective = crate::wall::effective_profile(&custom.profile, &custom.top_points, height);
    let (lo, hi) = effective.points.iter().fold((0.0_f64, 0.0_f64), |(lo, hi), p| {
        (lo.min(p.uv[1]), hi.max(p.uv[1]))
    });
    let (profile, anchors) = match crate::wall::ops::split_faces(
        &custom.profile,
        &custom.top_points,
        height,
        [at, lo - 1.0],
        [at, hi + 1.0],
    ) {
        Ok(split) => split,
        Err(crate::sketch::SketchError::NothingToSplit) => {
            (custom.profile.clone(), custom.top_points.clone())
        }
        Err(error) => return Err(err(error)),
    };
    let effective = crate::wall::effective_profile(&profile, &anchors, height);
    let mut before = Vec::new();
    let mut after = Vec::new();
    for face in &effective.faces {
        let polygon = crate::sketch::face_polygon(&effective, face.id).map_err(err)?;
        let mean = polygon.iter().map(|p| p[0]).sum::<f64>() / polygon.len().max(1) as f64;
        if mean < at {
            before.push(face.id);
        } else {
            after.push(face.id);
        }
    }
    let part = |remove: &[u32], keep: &[u32]| -> Result<Option<Anchored>, WallRunError> {
        if keep.is_empty() {
            return Ok(None);
        }
        let kept = if remove.is_empty() {
            (profile.clone(), anchors.clone())
        } else {
            crate::wall::ops::delete_faces(&profile, &anchors, height, remove).map_err(err)?
        };
        Ok(Some(kept))
    };
    let first = part(&after, &before)?;
    let second = match part(&before, &after)? {
        Some((sketch, top)) => {
            let ids: Vec<u32> = sketch.points.iter().map(|p| p.id).collect();
            Some(crate::wall::ops::move_points(&sketch, &top, height, &ids, [-at, 0.0]).map_err(err)?)
        }
        None => None,
    };
    Ok((first, second))
}

/// Delete points. Their neighbouring segments merge (the merged segment
/// is named by the surviving start point); deleting an end point of an
/// open run shortens it.
pub fn delete_points(run: &WallRunData, ids: &[u32]) -> Result<WallRunData, WallRunError> {
    for id in ids {
        point_index(run, *id)?;
    }
    let points = run.points.iter().filter(|p| !ids.contains(&p.id)).copied().collect();
    resegment(run, points, run.closed, &[])
}

/// Delete segments by merging each one's end point into its FIRST point
/// (its start, which keeps its position). The deleted segments' openings
/// and profiles go with them.
pub fn delete_edges(run: &WallRunData, segments: &[u32]) -> Result<WallRunData, WallRunError> {
    let pairs = segment_pairs(run);
    let mut remove = Vec::new();
    for segment in segments {
        let (_, end) = pairs
            .iter()
            .find(|(s, _)| s == segment)
            .copied()
            .ok_or(WallRunError::UnknownSegment(*segment))?;
        remove.push(end);
    }
    // A chain of deleted edges collapses into its first point.
    let points: Vec<RunPoint> = run.points.iter().filter(|p| !remove.contains(&p.id)).copied().collect();
    let dropped: Vec<u32> = segments.to_vec();
    resegment(run, points, run.closed, &dropped)
}

/// Which end of an open run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunEnd {
    Start,
    End,
}

/// Add a point at one end of an open run. Returns the run and the new
/// point id (the next free id). At the start, the new point starts the
/// new first segment; at the end, the old last point starts the new
/// last segment.
pub fn extend(run: &WallRunData, end: RunEnd, uv: P2) -> Result<(WallRunData, u32), WallRunError> {
    finite(&uv)?;
    if run.closed {
        return Err(WallRunError::WrongRunKind);
    }
    let id = run.next_point_id();
    let mut out = run.clone();
    let point = RunPoint { id, uv };
    match end {
        RunEnd::Start => out.points.insert(0, point),
        RunEnd::End => out.points.push(point),
    }
    Ok((checked(out)?, id))
}

/// Close an open run (a segment from the last point back to the first)
/// or open a closed one (the closing segment, with its openings and
/// profile, goes).
pub fn set_closed(run: &WallRunData, closed: bool) -> Result<WallRunData, WallRunError> {
    if run.closed == closed {
        return checked(run.clone());
    }
    if closed {
        let mut out = run.clone();
        out.closed = true;
        return checked(out);
    }
    let closing = run.points.last().map(|p| p.id).ok_or(WallRunError::TooFewPoints)?;
    resegment(run, run.points.clone(), false, &[closing])
}

// ---------------------------------------------------------------------
// Openings.
// ---------------------------------------------------------------------

/// Add an opening; its id is replaced by the next free opening id,
/// which is returned.
pub fn add_opening(run: &WallRunData, opening: Opening) -> Result<(WallRunData, u32), WallRunError> {
    let id = run.next_opening_id();
    let mut out = run.clone();
    out.openings.push(Opening { id, ..opening });
    Ok((checked(out)?, id))
}

/// Move an opening by `delta`: along its segment (clamped so the
/// opening stays inside the segment's clear span) and up (a window's
/// sill, clamped at the base; a door ignores it).
pub fn move_opening(run: &WallRunData, id: u32, delta: P2) -> Result<WallRunData, WallRunError> {
    finite(&delta)?;
    let mut out = run.clone();
    let opening = out
        .openings
        .iter_mut()
        .find(|o| o.id == id)
        .ok_or(WallRunError::UnknownOpening(id))?;
    let (lo, hi) = segment_clear_span(run, opening.segment)?;
    if hi - lo < opening.width_m - POINT_TOLERANCE {
        return Err(WallRunError::OpeningOutsideClearSpan(id));
    }
    let max = (hi - opening.width_m).max(lo);
    opening.offset_m = (opening.offset_m + delta[0]).clamp(lo, max);
    if opening.kind == OpeningKind::Window {
        opening.sill_m = (opening.sill_m + delta[1]).max(0.0);
    }
    checked(out)
}

/// Replace the opening with `opening.id`.
pub fn set_opening(run: &WallRunData, opening: Opening) -> Result<WallRunData, WallRunError> {
    let mut out = run.clone();
    let slot = out
        .openings
        .iter_mut()
        .find(|o| o.id == opening.id)
        .ok_or(WallRunError::UnknownOpening(opening.id))?;
    *slot = opening;
    checked(out)
}

/// Delete an opening.
pub fn delete_opening(run: &WallRunData, id: u32) -> Result<WallRunData, WallRunError> {
    if !run.openings.iter().any(|o| o.id == id) {
        return Err(WallRunError::UnknownOpening(id));
    }
    let mut out = run.clone();
    out.openings.retain(|o| o.id != id);
    checked(out)
}

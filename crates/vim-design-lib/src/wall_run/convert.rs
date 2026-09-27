//! Conversion of connected `Wall` entities into one wall run.

use crate::document::Document;
use crate::entity::{Params, slot};
use crate::id::EntityId;
use crate::sketch::{POINT_TOLERANCE, Sketch, SketchFaceKind};

use super::{Opening, OpeningKind, RunPoint, SegmentProfile, WallRunData, WallRunError, dist, validate};

/// Why a set of walls does not convert into a wall run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FromWallsError {
    /// The list is empty.
    NoWalls,
    /// This id is not a wall.
    NotAWall(EntityId),
    /// This wall has another base or top plane than the first wall.
    MixedPlanes(EntityId),
    /// This wall does not start where the previous one ends.
    NotConnected(EntityId),
    /// This wall's solid faces have another thickness than the first
    /// wall's.
    MixedThickness(EntityId),
    /// The converted run is invalid.
    Run(WallRunError),
}

impl std::fmt::Display for FromWallsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FromWallsError::NoWalls => write!(f, "no walls to convert"),
            FromWallsError::NotAWall(id) => write!(f, "{id:?} is not a wall"),
            FromWallsError::MixedPlanes(id) => {
                write!(f, "wall {id:?} has another base or top plane")
            }
            FromWallsError::NotConnected(id) => {
                write!(f, "wall {id:?} does not start where the previous wall ends")
            }
            FromWallsError::MixedThickness(id) => write!(f, "wall {id:?} has another thickness"),
            FromWallsError::Run(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for FromWallsError {}

struct WallData<'a> {
    id: EntityId,
    start: [f64; 2],
    end: [f64; 2],
    height_m: f64,
    top_offset_m: f64,
    profile: &'a Sketch,
    top_points: &'a [u32],
    base: Option<EntityId>,
    top: Option<EntityId>,
}

/// The wall run equivalent to a chain of walls, in order: each wall
/// must start where the previous one ends (the run is closed when the
/// last ends where the first starts), share the first wall's base and
/// top planes, and have one solid thickness. Returns the run data (put
/// it in `CreateWallRun` or `Params` with [`WallRunData::into_params`])
/// and the base and top planes.
///
/// The run takes the first wall's height and top offset. Rectangular
/// voids become openings (a void that reaches the base is a door); a
/// wall whose remaining profile is not the plain rectangle up to the
/// run's top reference keeps it as a segment profile, exact at the
/// current heights.
pub fn from_walls(
    doc: &Document,
    walls: &[EntityId],
) -> Result<(WallRunData, EntityId, Option<EntityId>), FromWallsError> {
    let mut data = Vec::with_capacity(walls.len());
    for id in walls {
        let record = doc.entity(*id).ok_or(FromWallsError::NotAWall(*id))?;
        let Params::Wall {
            start,
            end,
            height_m,
            top_offset_m,
            profile,
            top_points,
        } = &record.params
        else {
            return Err(FromWallsError::NotAWall(*id));
        };
        let plane = |index: usize| record.inputs.get(index).and_then(|s| s.referenced().next());
        data.push(WallData {
            id: *id,
            start: *start,
            end: *end,
            height_m: *height_m,
            top_offset_m: *top_offset_m,
            profile,
            top_points,
            base: plane(slot::WALL_BASE),
            top: plane(slot::WALL_TOP),
        });
    }
    let first = data.first().ok_or(FromWallsError::NoWalls)?;
    let base = first.base.ok_or(FromWallsError::NotAWall(first.id))?;
    let top = first.top;
    let thickness = thickness_of(first).ok_or(FromWallsError::MixedThickness(first.id))?;
    let run_height = crate::wall::wall_top_height(doc, first.id).ok_or(FromWallsError::NotAWall(first.id))?;

    let mut points: Vec<RunPoint> = Vec::with_capacity(data.len() + 1);
    let mut openings: Vec<Opening> = Vec::new();
    let mut profiles: Vec<SegmentProfile> = Vec::new();
    for (index, wall) in data.iter().enumerate() {
        if wall.base != first.base || wall.top != first.top {
            return Err(FromWallsError::MixedPlanes(wall.id));
        }
        if thickness_of(wall).is_none_or(|t| (t - thickness).abs() > 1e-9) {
            return Err(FromWallsError::MixedThickness(wall.id));
        }
        if let Some(previous) = points.last()
            && dist(previous.uv, wall.start) > POINT_TOLERANCE
        {
            return Err(FromWallsError::NotConnected(wall.id));
        }
        let segment = index as u32;
        if points.is_empty() {
            points.push(RunPoint { id: 0, uv: wall.start });
        }
        points.push(RunPoint { id: segment + 1, uv: wall.end });

        let height = crate::wall::wall_top_height(doc, wall.id).ok_or(FromWallsError::NotAWall(wall.id))?;
        let effective = crate::wall::effective_profile(wall.profile, wall.top_points, height);
        let mut rest = effective.clone();
        for face in &effective.faces {
            let SketchFaceKind::Void { depth } = face.kind else { continue };
            let Some((u0, v0, u1, v1)) = rectangle(&effective, face.id) else { continue };
            let door = v0 <= POINT_TOLERANCE;
            openings.push(Opening {
                id: openings.len() as u32,
                segment,
                offset_m: u0,
                sill_m: if door { 0.0 } else { v0 },
                width_m: u1 - u0,
                height_m: if door { v1 } else { v1 - v0 },
                kind: if door { OpeningKind::Door } else { OpeningKind::Window },
                depth_m: depth,
            });
            rest = crate::sketch::ops::delete_faces(&rest, &[face.id])
                .map_err(|error| FromWallsError::Run(WallRunError::Profile { segment, error }))?;
        }
        let length = dist(wall.start, wall.end);
        let plain = matches!(rest.faces.as_slice(), [face]
            if matches!(face.kind, SketchFaceKind::Solid { .. })
                && rectangle(&rest, face.id).is_some_and(|(u0, v0, u1, v1)| {
                    u0.abs() <= POINT_TOLERANCE
                        && v0.abs() <= POINT_TOLERANCE
                        && (u1 - length).abs() <= POINT_TOLERANCE
                        && (v1 - run_height).abs() <= POINT_TOLERANCE
                })
                && face.points.iter().all(|id| {
                    rest.uv(*id).is_ok_and(|uv| uv[1] <= POINT_TOLERANCE || wall.top_points.contains(id))
                }));
        let same_reference = (height - run_height).abs() <= POINT_TOLERANCE
            && wall.height_m == first.height_m
            && wall.top_offset_m == first.top_offset_m;
        if !(plain && same_reference) {
            let anchors: Vec<u32> = wall
                .top_points
                .iter()
                .copied()
                .filter(|id| rest.point(*id).is_some())
                .collect();
            profiles.push(SegmentProfile {
                segment,
                profile: crate::wall::stored_profile(&rest, &anchors, run_height),
                top_points: anchors,
            });
        }
    }
    let closed = data.len() >= 3
        && points.len() >= 2
        && matches!((points.first(), points.last()), (Some(a), Some(b)) if dist(a.uv, b.uv) <= POINT_TOLERANCE);
    if closed {
        points.pop();
        // The closing segment starts at the last point: renumber its
        // openings and profile.
        let last = points.last().map_or(0, |p| p.id);
        let closing = (data.len() - 1) as u32;
        for opening in openings.iter_mut().filter(|o| o.segment == closing) {
            opening.segment = last;
        }
        for profile in profiles.iter_mut().filter(|p| p.segment == closing) {
            profile.segment = last;
        }
    }
    let run = WallRunData {
        points,
        closed,
        thickness_m: thickness,
        height_m: first.height_m,
        top_offset_m: first.top_offset_m,
        openings,
        profiles,
    };
    validate(&run).map_err(FromWallsError::Run)?;
    Ok((run, base, top))
}

/// The one thickness of a wall's solid faces.
fn thickness_of(wall: &WallData<'_>) -> Option<f64> {
    let mut found: Option<f64> = None;
    for face in &wall.profile.faces {
        if let SketchFaceKind::Solid { thickness } = face.kind {
            match found {
                None => found = Some(thickness),
                Some(t) if (t - thickness).abs() <= 1e-9 => {}
                Some(_) => return None,
            }
        }
    }
    found
}

/// (u0, v0, u1, v1) of an axis-aligned rectangular face.
fn rectangle(sketch: &Sketch, face: u32) -> Option<(f64, f64, f64, f64)> {
    let polygon = crate::sketch::face_polygon(sketch, face).ok()?;
    if polygon.len() != 4 {
        return None;
    }
    let us: Vec<f64> = polygon.iter().map(|p| p[0]).collect();
    let vs: Vec<f64> = polygon.iter().map(|p| p[1]).collect();
    let (u0, u1) = (us.iter().copied().fold(f64::INFINITY, f64::min), us.iter().copied().fold(f64::NEG_INFINITY, f64::max));
    let (v0, v1) = (vs.iter().copied().fold(f64::INFINITY, f64::min), vs.iter().copied().fold(f64::NEG_INFINITY, f64::max));
    let on_box = polygon.iter().all(|p| {
        ((p[0] - u0).abs() <= POINT_TOLERANCE || (p[0] - u1).abs() <= POINT_TOLERANCE)
            && ((p[1] - v0).abs() <= POINT_TOLERANCE || (p[1] - v1).abs() <= POINT_TOLERANCE)
    });
    let distinct = (0..4).all(|i| {
        let (Some(a), Some(b)) = (polygon.get(i), polygon.get((i + 1) % 4)) else { return false };
        (a[0] - b[0]).abs() <= POINT_TOLERANCE || (a[1] - b[1]).abs() <= POINT_TOLERANCE
    });
    (on_box && distinct && u1 - u0 > POINT_TOLERANCE && v1 - v0 > POINT_TOLERANCE).then_some((u0, v0, u1, v1))
}

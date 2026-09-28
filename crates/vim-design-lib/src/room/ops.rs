//! Editing operations on a room boundary.
//!
//! Each operation takes a [`RoomData`], returns the edited copy to put
//! back with one `UpdateRoom`, and checks the result with [`validate`]
//! (an edit that would leave a self-crossing or clockwise boundary is an
//! error). Values an operation does not change are copied bit for bit.
//!
//! Hidden edges follow their start point: an edge whose start point is
//! deleted loses its hidden flag (the merged edge keeps the flag of the
//! surviving start point), and the new edge of a split keeps the flag of
//! the edge it was split from. `UpdateRoom` re-anchors the layout
//! openings of edges that an edit changes.

use crate::sketch::POINT_TOLERANCE;
use crate::sketch::geom::{self, P2};
use crate::wall_run::RunPoint;

use super::{RoomData, RoomError, canonical, validate};

fn finite(values: &[f64]) -> Result<(), RoomError> {
    if values.iter().all(|v| v.is_finite()) {
        Ok(())
    } else {
        Err(RoomError::NonFinite)
    }
}

fn checked(mut room: RoomData) -> Result<RoomData, RoomError> {
    let ids: Vec<u32> = room.edges();
    room.hidden_edges.retain(|e| ids.contains(e));
    room.hidden_edges = canonical(&room.hidden_edges);
    validate(&room)?;
    Ok(room)
}

fn point_index(room: &RoomData, id: u32) -> Result<usize, RoomError> {
    room.boundary.iter().position(|p| p.id == id).ok_or(RoomError::UnknownPoint(id))
}

/// Move points by `delta`.
pub fn move_points(room: &RoomData, ids: &[u32], delta: P2) -> Result<RoomData, RoomError> {
    finite(&delta)?;
    for id in ids {
        point_index(room, *id)?;
    }
    let mut out = room.clone();
    for point in &mut out.boundary {
        if ids.contains(&point.id) {
            point.uv = geom::add(point.uv, delta);
        }
    }
    checked(out)
}

/// Put one point at `uv`.
pub fn set_point(room: &RoomData, id: u32, uv: P2) -> Result<RoomData, RoomError> {
    finite(&uv)?;
    let index = point_index(room, id)?;
    let mut out = room.clone();
    if let Some(point) = out.boundary.get_mut(index) {
        point.uv = uv;
    }
    checked(out)
}

/// Move edges (both end points of each) by `delta`.
pub fn move_edges(room: &RoomData, edges: &[u32], delta: P2) -> Result<RoomData, RoomError> {
    let mut ids = Vec::new();
    for edge in edges {
        ids.push(*edge);
        ids.push(room.edge_end_id(*edge).ok_or(RoomError::UnknownEdge(*edge))?);
    }
    ids.sort_unstable();
    ids.dedup();
    move_points(room, &ids, delta)
}

/// Insert a point on edge `edge`, `offset_m` along it from its start.
/// Returns the room and the new point id (the next free id).
pub fn insert_point(room: &RoomData, edge: u32, offset_m: f64) -> Result<(RoomData, u32), RoomError> {
    finite(&[offset_m])?;
    let index = room.edge_index(edge).ok_or(RoomError::UnknownEdge(edge))?;
    let (a, b) = room.edge_ends(edge).ok_or(RoomError::UnknownEdge(edge))?;
    let length = geom::dist(a, b);
    if offset_m <= POINT_TOLERANCE || offset_m >= length - POINT_TOLERANCE {
        return Err(RoomError::InvalidParameter);
    }
    let id = room.next_point_id();
    let mut out = room.clone();
    out.boundary.insert(index + 1, RunPoint { id, uv: geom::lerp(a, b, offset_m / length) });
    if room.is_hidden(edge) {
        out.hidden_edges.push(id);
    }
    Ok((checked(out)?, id))
}

/// Delete points; their neighbouring edges merge.
pub fn delete_points(room: &RoomData, ids: &[u32]) -> Result<RoomData, RoomError> {
    for id in ids {
        point_index(room, *id)?;
    }
    let mut out = room.clone();
    out.boundary.retain(|p| !ids.contains(&p.id));
    if out.boundary.len() < 3 {
        return Err(RoomError::TooFewPoints);
    }
    checked(out)
}

/// Delete edges by merging each edge's end point into its FIRST point
/// (its start, which keeps its position).
pub fn delete_edges(room: &RoomData, edges: &[u32]) -> Result<RoomData, RoomError> {
    let mut remove = Vec::new();
    for edge in edges {
        remove.push(room.edge_end_id(*edge).ok_or(RoomError::UnknownEdge(*edge))?);
    }
    delete_points(room, &remove)
}

/// Mark edges as hidden (no wall) or visible.
pub fn set_hidden(room: &RoomData, edges: &[u32], hidden: bool) -> Result<RoomData, RoomError> {
    for edge in edges {
        room.edge_index(*edge).ok_or(RoomError::UnknownEdge(*edge))?;
    }
    let mut out = room.clone();
    if hidden {
        out.hidden_edges.extend_from_slice(edges);
    } else {
        out.hidden_edges.retain(|e| !edges.contains(e));
    }
    checked(out)
}

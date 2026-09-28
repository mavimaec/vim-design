//! Rooms: named closed boundaries on a construction plane.
//!
//! A room is data. Its boundary is a closed, counter-clockwise loop of
//! run points (stable ids, (u, v) on the plane); edge `k` runs from
//! point `k` to point `k + 1` (the last back to the first) and is named
//! by its START point id. The room interior is on the LEFT of every
//! edge. `hidden_edges` lists edges that generate no wall (a conceptual
//! division, such as a dining nook open to the kitchen).
//!
//! Rooms generate walls only through a [`crate::room_layout`]: the
//! layout owns the arrangement of its rooms (precedence cuts, shared
//! walls, junctions). `precedence` orders overlapping rooms: a higher
//! room cuts into a lower one.

use serde::{Deserialize, Serialize};

use crate::document::Document;
use crate::entity::Params;
use crate::sketch::POINT_TOLERANCE;
use crate::sketch::geom::{self, P2};
use crate::wall_run::RunPoint;

pub mod ops;

/// The data of a room, mirroring `Params::Room`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoomData {
    pub name: String,
    pub precedence: i32,
    pub boundary: Vec<RunPoint>,
    pub hidden_edges: Vec<u32>,
}

impl RoomData {
    /// The room data of `Params::Room`.
    pub fn from_params(params: &Params) -> Option<RoomData> {
        match params {
            Params::Room {
                name,
                precedence,
                boundary,
                hidden_edges,
            } => Some(RoomData {
                name: name.clone(),
                precedence: *precedence,
                boundary: boundary.clone(),
                hidden_edges: hidden_edges.clone(),
            }),
            _ => None,
        }
    }

    /// `Params::Room` with this data (hidden edges sorted, deduplicated).
    pub fn into_params(self) -> Params {
        Params::Room {
            name: self.name,
            precedence: self.precedence,
            boundary: self.boundary,
            hidden_edges: canonical(&self.hidden_edges),
        }
    }

    /// Edge ids (start point ids) in loop order.
    pub fn edges(&self) -> Vec<u32> {
        self.boundary.iter().map(|p| p.id).collect()
    }

    /// Index of the edge starting at point `edge`.
    pub fn edge_index(&self, edge: u32) -> Option<usize> {
        self.boundary.iter().position(|p| p.id == edge)
    }

    /// Start and end of edge `edge`.
    pub fn edge_ends(&self, edge: u32) -> Option<(P2, P2)> {
        let index = self.edge_index(edge)?;
        let n = self.boundary.len();
        let a = self.boundary.get(index)?.uv;
        let b = self.boundary.get((index + 1) % n.max(1))?.uv;
        Some((a, b))
    }

    /// The id of the point that edge `edge` ends at.
    pub fn edge_end_id(&self, edge: u32) -> Option<u32> {
        let index = self.edge_index(edge)?;
        let n = self.boundary.len();
        self.boundary.get((index + 1) % n.max(1)).map(|p| p.id)
    }

    /// True when edge `edge` generates no wall.
    pub fn is_hidden(&self, edge: u32) -> bool {
        self.hidden_edges.contains(&edge)
    }

    /// The boundary polygon.
    pub fn polygon(&self) -> Vec<P2> {
        self.boundary.iter().map(|p| p.uv).collect()
    }

    /// Signed area (positive for a counter-clockwise boundary).
    pub fn signed_area(&self) -> f64 {
        geom::signed_area(&self.polygon())
    }

    /// The next free point id (largest + 1).
    pub fn next_point_id(&self) -> u32 {
        self.boundary.iter().map(|p| p.id).max().map_or(0, |m| m.saturating_add(1))
    }
}

pub(crate) fn canonical(ids: &[u32]) -> Vec<u32> {
    let mut out = ids.to_vec();
    out.sort_unstable();
    out.dedup();
    out
}

/// Typed failure of a room check or operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RoomError {
    /// Fewer than three boundary points.
    TooFewPoints,
    /// Two points share this id.
    DuplicatePointId(u32),
    /// No point has this id.
    UnknownPoint(u32),
    /// No edge starts at this point id.
    UnknownEdge(u32),
    /// A coordinate is NaN or infinite.
    NonFinite,
    /// The edge starting at this point has no length.
    ZeroLengthEdge(u32),
    /// The boundary crosses itself.
    SelfIntersecting,
    /// The boundary encloses no area.
    ZeroArea,
    /// The boundary runs clockwise.
    Clockwise,
    /// An operation parameter is out of range.
    InvalidParameter,
}

impl std::fmt::Display for RoomError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RoomError::TooFewPoints => write!(f, "a room needs three boundary points"),
            RoomError::DuplicatePointId(id) => write!(f, "point id {id} is used twice"),
            RoomError::UnknownPoint(id) => write!(f, "no point {id}"),
            RoomError::UnknownEdge(id) => write!(f, "no edge starts at point {id}"),
            RoomError::NonFinite => write!(f, "a coordinate is not finite"),
            RoomError::ZeroLengthEdge(id) => write!(f, "edge {id} has no length"),
            RoomError::SelfIntersecting => write!(f, "the room boundary crosses itself"),
            RoomError::ZeroArea => write!(f, "the room has no area"),
            RoomError::Clockwise => write!(f, "the room boundary runs clockwise"),
            RoomError::InvalidParameter => write!(f, "a parameter is out of range"),
        }
    }
}

impl std::error::Error for RoomError {}

/// Structural validity, as the room commands require it: three or more
/// points with unique ids and finite coordinates, and hidden edges that
/// are boundary edges.
pub fn validate_structure(room: &RoomData) -> Result<(), RoomError> {
    if room.boundary.len() < 3 {
        return Err(RoomError::TooFewPoints);
    }
    let mut ids = std::collections::BTreeSet::new();
    for point in &room.boundary {
        if !point.uv.iter().all(|c| c.is_finite()) {
            return Err(RoomError::NonFinite);
        }
        if !ids.insert(point.id) {
            return Err(RoomError::DuplicatePointId(point.id));
        }
    }
    for edge in &room.hidden_edges {
        if !ids.contains(edge) {
            return Err(RoomError::UnknownEdge(*edge));
        }
    }
    Ok(())
}

/// Full validity: structure, no zero-length edge, no self-crossing, a
/// positive (counter-clockwise) area.
pub fn validate(room: &RoomData) -> Result<(), RoomError> {
    validate_structure(room)?;
    for edge in room.edges() {
        let (a, b) = room.edge_ends(edge).ok_or(RoomError::UnknownEdge(edge))?;
        if geom::dist(a, b) <= POINT_TOLERANCE {
            return Err(RoomError::ZeroLengthEdge(edge));
        }
    }
    if geom::self_intersects(&room.polygon(), POINT_TOLERANCE) {
        return Err(RoomError::SelfIntersecting);
    }
    let area = room.signed_area();
    if area.abs() <= POINT_TOLERANCE * POINT_TOLERANCE * 1.0e3 {
        return Err(RoomError::ZeroArea);
    }
    if area < 0.0 {
        return Err(RoomError::Clockwise);
    }
    Ok(())
}

/// A room from a polygon drawn in any direction: repeated points are
/// dropped, the loop is made counter-clockwise, and points get ids
/// 0, 1, 2, ... in loop order.
pub fn from_polygon(name: &str, precedence: i32, polygon: &[P2]) -> Result<RoomData, RoomError> {
    let mut points: Vec<P2> = Vec::with_capacity(polygon.len());
    for p in polygon {
        if !p.iter().all(|c| c.is_finite()) {
            return Err(RoomError::NonFinite);
        }
        if points.last().is_none_or(|q| geom::dist(*q, *p) > POINT_TOLERANCE) {
            points.push(*p);
        }
    }
    while points.len() > 1
        && matches!((points.first(), points.last()), (Some(a), Some(b)) if geom::dist(*a, *b) <= POINT_TOLERANCE)
    {
        points.pop();
    }
    if geom::signed_area(&points) < 0.0 {
        points.reverse();
    }
    let room = RoomData {
        name: name.to_owned(),
        precedence,
        boundary: points
            .into_iter()
            .enumerate()
            .map(|(i, uv)| RunPoint { id: i as u32, uv })
            .collect(),
        hidden_edges: vec![],
    };
    validate(&room)?;
    Ok(room)
}

/// An axis-aligned rectangle room between two opposite corners.
pub fn from_rectangle(name: &str, precedence: i32, a: P2, b: P2) -> Result<RoomData, RoomError> {
    let (x0, x1) = (a[0].min(b[0]), a[0].max(b[0]));
    let (y0, y1) = (a[1].min(b[1]), a[1].max(b[1]));
    from_polygon(name, precedence, &[[x0, y0], [x1, y0], [x1, y1], [x0, y1]])
}

/// The default name of the next room: "Room NNN", one above the largest
/// number among the document's rooms named that way ("Room 001" first).
pub fn default_name(doc: &Document) -> String {
    let largest = doc
        .entities()
        .filter_map(|(_, record)| match &record.params {
            Params::Room { name, .. } => name.strip_prefix("Room ")?.parse::<u32>().ok(),
            _ => None,
        })
        .max()
        .unwrap_or(0);
    format!("Room {:03}", largest.saturating_add(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn polygons_become_counter_clockwise_rooms() {
        let room = from_polygon("R", 0, &[[0.0, 0.0], [0.0, 3.0], [4.0, 3.0], [4.0, 0.0]]);
        let room = room.unwrap_or_else(|_| RoomData {
            name: String::new(),
            precedence: 0,
            boundary: vec![],
            hidden_edges: vec![],
        });
        assert!(room.signed_area() > 0.0);
        assert_eq!(room.edges(), vec![0, 1, 2, 3]);
        assert_eq!(
            from_polygon("R", 0, &[[0.0, 0.0], [4.0, 4.0], [4.0, 0.0], [0.0, 4.0]]).err(),
            Some(RoomError::SelfIntersecting)
        );
        assert_eq!(
            from_polygon("R", 0, &[[0.0, 0.0], [4.0, 0.0], [8.0, 0.0]]).err(),
            Some(RoomError::SelfIntersecting)
        );
    }
}

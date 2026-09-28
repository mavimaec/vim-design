//! Rooms in the app: the pieces between the library's `Room` /
//! `RoomLayout` (docs/AUTHORING.md §12) and the page.
//!
//! - [`RoomProfile`]: a room's boundary in Edit Mode — points, and edges
//!   as two-point faces (a hidden edge is a void face, drawn dashed);
//!   every edit is one `room::ops` call.
//! - [`label_point`]: where a region's name + area label goes.
//! - [`cover_at`]: the room edge under a plan point, from the layout's
//!   wall graph (openings anchor to it).
//! - [`room_error`] / [`layout_error`]: short user messages.

use vim_design_lib::EntityId;
use vim_design_lib::room::{RoomData, RoomError, ops as room_ops};
use vim_design_lib::room_layout::{LayoutError, WallSegment};

use super::edit::{Edit, EditError, EdgeKey, FaceKind, ProfileFace, ProfileModel, ProfilePoint, ProfileView};
use super::geom::{Invalid, P2, dist, point_in_polygon, point_segment_distance, signed_area};

/// Samples per axis when placing a room label.
const LABEL_SAMPLES: usize = 16;

/// A room error as a short message for a toast.
pub fn room_error(e: RoomError) -> EditError {
    match e {
        RoomError::TooFewPoints => EditError::Invalid(Invalid::TooFewPoints),
        RoomError::SelfIntersecting => EditError::Invalid(Invalid::SelfIntersecting),
        RoomError::ZeroArea => EditError::Invalid(Invalid::ZeroArea),
        RoomError::ZeroLengthEdge(_) => EditError::NoEffect("Two points of the room would be on top of each other"),
        RoomError::Clockwise => EditError::NoEffect("The room would turn inside out"),
        RoomError::UnknownPoint(_) | RoomError::UnknownEdge(_) => EditError::Unknown,
        RoomError::DuplicatePointId(_) | RoomError::NonFinite | RoomError::InvalidParameter => {
            EditError::NoEffect("That change is not possible for this room")
        }
    }
}

/// A layout error as a short message for a toast.
pub fn layout_error(e: LayoutError) -> String {
    match e {
        LayoutError::OpeningDoesNotFit(_) => "The opening does not fit this wall (between its junctions)".to_owned(),
        LayoutError::InvalidThickness => "The wall thickness must be positive".to_owned(),
        LayoutError::InvalidHeight | LayoutError::TopBelowBase => "The wall top must be above its base".to_owned(),
        LayoutError::InvalidOpening(_) => "The opening needs a positive size and a sill at or above the base".to_owned(),
        other => {
            let s = other.to_string();
            let mut c = s.chars();
            c.next().map_or(s.clone(), |f| f.to_uppercase().collect::<String>() + c.as_str())
        }
    }
}

/// A room's boundary in Edit Mode.
#[derive(Debug, Clone, PartialEq)]
pub struct RoomProfile {
    pub data: RoomData,
}

impl RoomProfile {
    /// The edge (start point id) between two neighbouring points.
    fn edge_of(&self, e: EdgeKey) -> Option<u32> {
        let b = &self.data.boundary;
        let n = b.len();
        (0..n).find_map(|i| (EdgeKey::new(b[i].id, b[(i + 1) % n].id) == e).then_some(b[i].id))
    }

    fn edges_of(&self, keys: &[EdgeKey]) -> Result<Vec<u32>, EditError> {
        keys.iter().map(|k| self.edge_of(*k).ok_or(EditError::Unknown)).collect()
    }

    /// The selected edges (start point ids) of an Edit Mode selection.
    pub fn selected_edges(&self, keys: impl Iterator<Item = EdgeKey>) -> Vec<u32> {
        keys.filter_map(|k| self.edge_of(k)).collect()
    }
}

impl ProfileModel for RoomProfile {
    fn view(&self) -> ProfileView {
        let b = &self.data.boundary;
        let n = b.len();
        ProfileView {
            points: b.iter().map(|p| ProfilePoint { id: p.id, uv: p.uv }).collect(),
            faces: (0..n)
                .map(|i| {
                    let kind = if self.data.is_hidden(b[i].id) { FaceKind::Void { depth: None } } else { FaceKind::Solid { thickness: 0.0 } };
                    ProfileFace { id: i as u32, points: vec![b[i].id, b[(i + 1) % n].id], kind }
                })
                .collect(),
        }
    }

    fn apply(&self, edit: &Edit) -> Result<Self, EditError> {
        let data = &self.data;
        let next = match edit {
            Edit::MovePoints { ids, delta } => room_ops::move_points(data, ids, *delta),
            Edit::MoveEdges { edges, delta } => room_ops::move_edges(data, &self.edges_of(edges)?, *delta),
            Edit::MoveFaces { faces, delta } => {
                let starts: Vec<u32> = faces.iter().filter_map(|f| data.boundary.get(*f as usize).map(|p| p.id)).collect();
                room_ops::move_edges(data, &starts, *delta)
            }
            Edit::InsertPoint { edge, uv } => {
                let start = self.edge_of(*edge).ok_or(EditError::Unknown)?;
                let (a, b) = data.edge_ends(start).ok_or(EditError::Unknown)?;
                let len = dist(a, b).max(1e-12);
                let along = ((uv[0] - a[0]) * (b[0] - a[0]) + (uv[1] - a[1]) * (b[1] - a[1])) / len;
                room_ops::insert_point(data, start, along).map(|(r, _)| r)
            }
            Edit::DeletePoints(ids) => room_ops::delete_points(data, ids),
            Edit::DeleteEdges(keys) => room_ops::delete_edges(data, &self.edges_of(keys)?),
            _ => return Err(EditError::NoEffect("That edit does not apply to a room")),
        };
        next.map(|data| RoomProfile { data }).map_err(room_error)
    }
}

/// Where a region's label goes: of the largest piece, the sampled
/// interior point farthest from its outline (a label at the bounding-box
/// centre can fall outside an L-shaped room, or into a notch).
pub fn label_point(shapes: &[Vec<Vec<P2>>]) -> Option<P2> {
    let shape = shapes
        .iter()
        .filter(|s| !s.is_empty())
        .max_by(|a, b| signed_area(&a[0]).abs().total_cmp(&signed_area(&b[0]).abs()))?;
    let outer = &shape[0];
    let (mut lo, mut hi) = ([f64::INFINITY; 2], [f64::NEG_INFINITY; 2]);
    for p in outer {
        for k in 0..2 {
            lo[k] = lo[k].min(p[k]);
            hi[k] = hi[k].max(p[k]);
        }
    }
    let clearance = |p: P2| {
        shape
            .iter()
            .flat_map(|ring| (0..ring.len()).map(move |i| (ring[i], ring[(i + 1) % ring.len()])))
            .map(|(a, b)| point_segment_distance(p, a, b))
            .fold(f64::INFINITY, f64::min)
    };
    let mut best: Option<(f64, P2)> = None;
    for i in 0..LABEL_SAMPLES {
        for j in 0..LABEL_SAMPLES {
            let p = [
                lo[0] + (hi[0] - lo[0]) * (i as f64 + 0.5) / LABEL_SAMPLES as f64,
                lo[1] + (hi[1] - lo[1]) * (j as f64 + 0.5) / LABEL_SAMPLES as f64,
            ];
            if !point_in_polygon(p, outer) || shape[1..].iter().any(|h| point_in_polygon(p, h)) {
                continue;
            }
            let c = clearance(p);
            if best.is_none_or(|(bc, _)| c > bc) {
                best = Some((c, p));
            }
        }
    }
    best.map(|(_, p)| p).or_else(|| Some([(lo[0] + hi[0]) / 2.0, (lo[1] + hi[1]) / 2.0]))
}

/// The room edge under a plan point: the visible wall segment within
/// `tol` meters (the nearest), and its first covering room edge (in rank
/// order: where a higher room cuts into a lower one, the higher room's
/// edge). (room, edge, the point projected on the segment).
pub fn cover_at(segments: &[WallSegment], p: P2, tol: f64) -> Option<(EntityId, u32, P2)> {
    let (_, s) = segments
        .iter()
        .filter(|s| !s.hidden)
        .map(|s| (point_segment_distance(p, s.a, s.b), s))
        .filter(|(d, _)| *d <= tol)
        .min_by(|a, b| a.0.total_cmp(&b.0))?;
    let cover = s.covers.iter().find(|c| !c.hidden)?;
    let len = dist(s.a, s.b).max(1e-12);
    let t = (((p[0] - s.a[0]) * (s.b[0] - s.a[0]) + (p[1] - s.a[1]) * (s.b[1] - s.a[1])) / (len * len)).clamp(0.0, 1.0);
    Some((cover.room, cover.edge, [s.a[0] + (s.b[0] - s.a[0]) * t, s.a[1] + (s.b[1] - s.a[1]) * t]))
}

/// Distance along a room edge (from its start) of a plan point's
/// projection.
pub fn along_edge(room: &RoomData, edge: u32, p: P2) -> Option<f64> {
    let (a, b) = room.edge_ends(edge)?;
    let len = dist(a, b).max(1e-12);
    Some(((p[0] - a[0]) * (b[0] - a[0]) + (p[1] - a[1]) * (b[1] - a[1])) / len)
}

#[cfg(test)]
mod tests {
    use super::*;
    use vim_design_lib::room;
    use vim_design_lib::room_layout::{LayoutInput, RoomLayoutData, arrange};

    #[test]
    fn the_room_profile_edits_through_room_ops() {
        let mut data = room::from_rectangle("Room 001", 0, [0.0, 0.0], [4.0, 3.0]).expect("room");
        let south = data.boundary[0].id;
        data = room_ops::set_hidden(&data, &[south], true).expect("hide");
        let p = RoomProfile { data };
        assert_eq!(p.view().faces.iter().filter(|f| f.kind.is_void()).count(), 1);
        let (a, b) = (p.data.boundary[0].id, p.data.boundary[1].id);
        let p = p.apply(&Edit::InsertPoint { edge: EdgeKey::new(a, b), uv: [2.0, 0.0] }).expect("insert");
        assert_eq!(p.data.boundary.len(), 5);
        assert_eq!(p.data.hidden_edges.len(), 2, "both halves of the hidden edge stay hidden");
        let new = p.data.boundary[1].id;
        let p = p.apply(&Edit::MovePoints { ids: vec![new], delta: [0.0, -1.0] }).expect("move");
        assert!((p.data.signed_area() - 14.0).abs() < 1e-9);
        assert!(p.apply(&Edit::MovePoints { ids: vec![new], delta: [0.0, 5.0] }).is_err(), "a fold is refused");
    }

    #[test]
    fn labels_sit_inside_l_shapes() {
        let l = vec![vec![[0.0, 0.0], [4.0, 0.0], [4.0, 2.0], [2.0, 2.0], [2.0, 4.0], [0.0, 4.0]]];
        let p = label_point(std::slice::from_ref(&l)).expect("label");
        assert!(point_in_polygon(p, &l[0]) && !(p[0] > 2.0 && p[1] > 2.0), "{p:?}");
    }

    #[test]
    fn a_plan_point_finds_the_covering_room_edge() {
        let a = room::from_rectangle("A", 0, [0.0, 0.0], [4.0, 3.0]).expect("a");
        let b = room::from_rectangle("B", 1, [4.0, 0.0], [7.0, 3.0]).expect("b");
        let input = LayoutInput {
            layout: RoomLayoutData { thickness_m: 0.114, height_m: 2.7, top_offset_m: 0.0, openings: vec![] },
            rooms: vec![(EntityId(10), a), (EntityId(11), b)],
        };
        let arr = arrange(&input).expect("arrange");
        let (room, edge, at) = cover_at(&arr.segments, [4.03, 1.5], 0.1).expect("the shared wall");
        // B ranks first (higher precedence): its west edge covers the shared wall.
        assert_eq!(room, EntityId(11));
        let data = input.room(room).expect("room");
        assert!((along_edge(data, edge, at).expect("along") - 1.5).abs() < 1e-9);
        assert!(cover_at(&arr.segments, [2.0, 1.5], 0.1).is_none(), "inside a room: no wall");
    }
}

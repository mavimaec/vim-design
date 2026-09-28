//! Rooms (preview behind `?rooms`): a TEMPORARY app-side stand-in for
//! the library's `Room` / `RoomLayout` entities, so the page's room tools
//! can be built before the library lands them. It follows the library
//! contract, so the switch replaces only this module and its wasm glue:
//!
//! - A room: a name, a precedence, a closed counter-clockwise boundary of
//!   run points (stable ids; edge `k` is named by its START point id),
//!   and the hidden edges (no wall: a conceptual division).
//! - A layout per plane: the wall thickness and height of the rooms'
//!   walls, and the openings, each anchored to a room edge.
//! - Effective regions: rooms ranked by precedence (higher first, ties
//!   by id); a room's region is its boundary minus the rooms above it.
//! - The wall network: the room edges minus the parts inside rooms
//!   ranked above, split where they overlap and deduplicated (a boundary
//!   shared by two rooms is one wall). A piece is hidden when ANY room
//!   edge covering it is hidden.
//!
//! What the preview does NOT do (the library does): wall solids with
//! exact joins, and persistence in the document. The page draws the
//! walls as plan bands.

use i_overlay::core::fill_rule::FillRule;
use i_overlay::core::overlay_rule::OverlayRule;
use i_overlay::core::solver::Solver;
use i_overlay::float::overlay::{FloatOverlay, OverlayOptions};
use vim_design_lib::EntityId;
use vim_design_lib::wall_run::{OpeningKind, RunPoint};

use super::edit::{Edit, EditError, EdgeKey, FaceKind, ProfileFace, ProfileModel, ProfilePoint, ProfileView};
use super::geom::{self, EPS, Invalid, P2, dist, point_in_polygon, point_segment_distance, signed_area};
use super::walls::PARTITION_THICKNESS_M;

/// Default room names: "Room 001", "Room 002", ... (the number after the
/// highest one used).
pub const ROOM_NAME_PREFIX: &str = "Room";
const ROOM_NUMBER_DIGITS: usize = 3;
/// Default height of room walls (meters).
pub const DEFAULT_ROOM_WALL_HEIGHT_M: f64 = 2.7;
/// Pieces of wall shorter than this are dropped (meters).
const MIN_PIECE_M: f64 = 1e-6;

/// A room (see the module docs).
#[derive(Debug, Clone, PartialEq)]
pub struct Room {
    pub id: u32,
    pub plane: EntityId,
    pub name: String,
    pub precedence: i32,
    pub boundary: Vec<RunPoint>,
    pub hidden_edges: Vec<u32>,
}

impl Room {
    /// A room from a drawn outline: deduplicated, simple, made
    /// counter-clockwise, point ids 1..n.
    pub fn from_outline(id: u32, plane: EntityId, name: &str, precedence: i32, outline: &[P2]) -> Result<Room, Invalid> {
        let pts = geom::dedup_closed(outline);
        geom::validate_outline(&pts)?;
        let pts = geom::normalized_ccw(&pts);
        Ok(Room {
            id,
            plane,
            name: name.to_owned(),
            precedence,
            boundary: pts.iter().zip(1u32..).map(|(uv, id)| RunPoint { id, uv: *uv }).collect(),
            hidden_edges: Vec::new(),
        })
    }

    pub fn polygon(&self) -> Vec<P2> {
        self.boundary.iter().map(|p| p.uv).collect()
    }

    /// Edge ids (start point ids) in loop order.
    pub fn edges(&self) -> Vec<u32> {
        self.boundary.iter().map(|p| p.id).collect()
    }

    pub fn edge_ends(&self, edge: u32) -> Option<(P2, P2)> {
        let i = self.boundary.iter().position(|p| p.id == edge)?;
        let n = self.boundary.len();
        Some((self.boundary[i].uv, self.boundary[(i + 1) % n].uv))
    }

    pub fn is_hidden(&self, edge: u32) -> bool {
        self.hidden_edges.contains(&edge)
    }

    /// Hide or show edges (a hidden edge generates no wall).
    pub fn set_hidden(&mut self, edges: &[u32], hidden: bool) {
        self.hidden_edges.retain(|e| !edges.contains(e));
        if hidden {
            self.hidden_edges.extend(edges.iter().copied().filter(|e| self.boundary.iter().any(|p| p.id == *e)));
        }
        self.hidden_edges.sort_unstable();
        self.hidden_edges.dedup();
    }

    /// A new boundary (an edit): kept counter-clockwise, hidden edges of
    /// vanished points dropped.
    pub fn with_boundary(&self, boundary: Vec<RunPoint>) -> Result<Room, Invalid> {
        let pts: Vec<P2> = boundary.iter().map(|p| p.uv).collect();
        geom::validate_outline(&pts)?;
        let mut boundary = boundary;
        if signed_area(&pts) < 0.0 {
            // Reversed: edge k (start point k) now runs the other way.
            boundary.reverse();
        }
        let mut room = Room { boundary, ..self.clone() };
        room.hidden_edges.retain(|e| room.boundary.iter().any(|p| p.id == *e));
        Ok(room)
    }
}

/// The next default room name: "Room NNN" after the highest used.
pub fn default_name<'a>(names: impl Iterator<Item = &'a str>) -> String {
    let max = names
        .filter_map(|n| n.strip_prefix(ROOM_NAME_PREFIX)?.strip_prefix(' ')?.parse::<u64>().ok())
        .max()
        .unwrap_or(0);
    format!("{ROOM_NAME_PREFIX} {:0width$}", max + 1, width = ROOM_NUMBER_DIGITS)
}

/// An opening in a room wall, anchored to a room edge: `offset_m` along
/// the edge from its start to the opening's left edge.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RoomOpening {
    pub id: u32,
    pub room: u32,
    pub edge: u32,
    pub offset_m: f64,
    pub sill_m: f64,
    pub width_m: f64,
    pub height_m: f64,
    pub kind: OpeningKind,
    pub depth_m: Option<f64>,
}

/// The wall settings of the rooms of one plane.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Layout {
    pub plane: EntityId,
    pub thickness_m: f64,
    /// Fixed height (used when `top` is `None`).
    pub height_m: f64,
    pub top: Option<EntityId>,
    pub top_offset_m: f64,
}

impl Layout {
    pub fn new(plane: EntityId) -> Self {
        Layout { plane, thickness_m: PARTITION_THICKNESS_M, height_m: DEFAULT_ROOM_WALL_HEIGHT_M, top: None, top_offset_m: 0.0 }
    }
}

/// One piece of the wall network: its ends, the room edges covering it,
/// and whether it is hidden (any covering edge hidden).
#[derive(Debug, Clone, PartialEq)]
pub struct WallPiece {
    pub a: P2,
    pub b: P2,
    /// (room id, edge id) of every room edge along the piece.
    pub covers: Vec<(u32, u32)>,
    pub hidden: bool,
}

/// Every room and layout (session state in the preview).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Rooms {
    pub rooms: Vec<Room>,
    pub layouts: Vec<Layout>,
    pub openings: Vec<RoomOpening>,
}

impl Rooms {
    pub fn room(&self, id: u32) -> Option<&Room> {
        self.rooms.iter().find(|r| r.id == id)
    }

    pub fn room_mut(&mut self, id: u32) -> Option<&mut Room> {
        self.rooms.iter_mut().find(|r| r.id == id)
    }

    pub fn next_id(&self) -> u32 {
        self.rooms.iter().map(|r| r.id).max().unwrap_or(0) + 1
    }

    pub fn next_opening_id(&self) -> u32 {
        self.openings.iter().map(|o| o.id).max().unwrap_or(0) + 1
    }

    pub fn layout(&self, plane: EntityId) -> Layout {
        self.layouts.iter().find(|l| l.plane == plane).copied().unwrap_or_else(|| Layout::new(plane))
    }

    pub fn set_layout(&mut self, layout: Layout) {
        self.layouts.retain(|l| l.plane != layout.plane);
        self.layouts.push(layout);
    }

    /// Add a room drawn on `plane`: named by default, on top of the
    /// others (it cuts into the rooms it overlaps).
    pub fn add(&mut self, plane: EntityId, outline: &[P2]) -> Result<u32, Invalid> {
        let id = self.next_id();
        let name = default_name(self.rooms.iter().map(|r| r.name.as_str()));
        let top = self.rooms.iter().filter(|r| r.plane == plane).map(|r| r.precedence).max().map_or(0, |p| p + 1);
        self.rooms.push(Room::from_outline(id, plane, &name, top, outline)?);
        Ok(id)
    }

    /// Delete a room and its openings.
    pub fn remove(&mut self, id: u32) {
        self.rooms.retain(|r| r.id != id);
        self.openings.retain(|o| o.room != id);
    }

    /// The rooms of a plane, highest first (ties: lower id first).
    pub fn ranked(&self, plane: EntityId) -> Vec<&Room> {
        let mut out: Vec<&Room> = self.rooms.iter().filter(|r| r.plane == plane).collect();
        out.sort_by(|a, b| b.precedence.cmp(&a.precedence).then(a.id.cmp(&b.id)));
        out
    }

    /// Move a room one place up (`up`) or down the ranking: it swaps
    /// precedence with its neighbour. False at the end.
    pub fn restack(&mut self, id: u32, up: bool) -> bool {
        let Some(plane) = self.room(id).map(|r| r.plane) else { return false };
        let order: Vec<u32> = self.ranked(plane).iter().map(|r| r.id).collect();
        let Some(i) = order.iter().position(|r| *r == id) else { return false };
        let j = if up { i.checked_sub(1) } else { Some(i + 1).filter(|j| *j < order.len()) };
        let Some(j) = j else { return false };
        // Distinct precedences in ranking order first, so a swap always
        // changes the order.
        let n = order.len() as i32;
        for (k, rid) in order.iter().enumerate() {
            if let Some(r) = self.room_mut(*rid) {
                r.precedence = n - k as i32;
            }
        }
        let (pi, pj) = (n - i as i32, n - j as i32);
        if let Some(r) = self.room_mut(order[i]) {
            r.precedence = pj;
        }
        if let Some(r) = self.room_mut(order[j]) {
            r.precedence = pi;
        }
        true
    }

    /// Effective regions of a plane's rooms: each room's boundary minus
    /// the rooms ranked above it. (room id, shapes: outer ring then holes).
    pub fn regions(&self, plane: EntityId) -> Vec<(u32, Vec<Vec<Vec<P2>>>)> {
        let ranked = self.ranked(plane);
        let mut out = Vec::new();
        for (i, r) in ranked.iter().enumerate() {
            let above: Vec<Vec<P2>> = ranked[..i].iter().map(|a| a.polygon()).collect();
            let shapes = if above.is_empty() { vec![vec![r.polygon()]] } else { difference(&[r.polygon()], &above) };
            out.push((r.id, shapes));
        }
        out
    }

    /// The effective area of a room (m²).
    pub fn region_area(&self, id: u32) -> f64 {
        let Some(plane) = self.room(id).map(|r| r.plane) else { return 0.0 };
        self.regions(plane)
            .into_iter()
            .find(|(r, _)| *r == id)
            .map_or(0.0, |(_, shapes)| shapes.iter().flat_map(|s| s.iter()).map(|ring| signed_area(ring)).sum::<f64>().abs())
    }

    /// The wall network of a plane (see the module docs).
    pub fn walls(&self, plane: EntityId) -> Vec<WallPiece> {
        let ranked = self.ranked(plane);
        // 1. Every room edge minus its parts inside the rooms above.
        let mut raw: Vec<(P2, P2, (u32, u32), bool)> = Vec::new();
        for (i, r) in ranked.iter().enumerate() {
            let above: Vec<Vec<P2>> = ranked[..i].iter().map(|a| a.polygon()).collect();
            for edge in r.edges() {
                let Some((a, b)) = r.edge_ends(edge) else { continue };
                for (s, t) in outside_parts(a, b, &above) {
                    raw.push((s, t, (r.id, edge), r.is_hidden(edge)));
                }
            }
        }
        // 2. Split at the ends of collinear overlapping pieces; merge the
        //    identical sub-pieces.
        let mut pieces: Vec<WallPiece> = Vec::new();
        for (k, &(a, b, cover, _)) in raw.iter().enumerate() {
            let len = dist(a, b);
            let mut cuts = vec![0.0, len];
            for (m, &(c, d, ..)) in raw.iter().enumerate() {
                if m != k && collinear_overlap(a, b, c, d) {
                    for p in [c, d] {
                        let t = along(a, b, p);
                        if t > MIN_PIECE_M && t < len - MIN_PIECE_M {
                            cuts.push(t);
                        }
                    }
                }
            }
            cuts.sort_by(f64::total_cmp);
            cuts.dedup_by(|x, y| (*x - *y).abs() <= MIN_PIECE_M);
            let dir = [(b[0] - a[0]) / len, (b[1] - a[1]) / len];
            for w in cuts.windows(2) {
                let s = [a[0] + dir[0] * w[0], a[1] + dir[1] * w[0]];
                let t = [a[0] + dir[0] * w[1], a[1] + dir[1] * w[1]];
                if let Some(p) = pieces.iter_mut().find(|p| same_segment(p.a, p.b, s, t)) {
                    if !p.covers.contains(&cover) {
                        p.covers.push(cover);
                    }
                } else {
                    pieces.push(WallPiece { a: s, b: t, covers: vec![cover], hidden: false });
                }
            }
        }
        for p in &mut pieces {
            p.hidden = p.covers.iter().any(|(room, edge)| self.room(*room).is_some_and(|r| r.is_hidden(*edge)));
        }
        pieces
    }

    /// The room edge a plan point is on (within `tol` meters of a visible
    /// wall piece): (room, edge, offset along the edge), the highest
    /// ranked covering room first.
    pub fn edge_at(&self, plane: EntityId, p: P2, tol: f64) -> Option<(u32, u32, f64)> {
        let mut best: Option<(f64, &WallPiece)> = None;
        let pieces = self.walls(plane);
        for piece in pieces.iter().filter(|w| !w.hidden) {
            let d = point_segment_distance(p, piece.a, piece.b);
            if d <= tol && best.is_none_or(|(bd, _)| d < bd) {
                best = Some((d, piece));
            }
        }
        let (_, piece) = best?;
        let (room, edge) = *piece.covers.first()?;
        let (a, b) = self.room(room)?.edge_ends(edge)?;
        Some((room, edge, along(a, b, p)))
    }

    /// The visible spans of a room edge (offsets along it), for fitting
    /// an opening.
    pub fn edge_spans(&self, room: u32, edge: u32) -> Vec<(f64, f64)> {
        let Some(r) = self.room(room) else { return Vec::new() };
        let Some((a, b)) = r.edge_ends(edge) else { return Vec::new() };
        let mut spans: Vec<(f64, f64)> = self
            .walls(r.plane)
            .iter()
            .filter(|w| !w.hidden && w.covers.contains(&(room, edge)))
            .map(|w| {
                let (s, t) = (along(a, b, w.a), along(a, b, w.b));
                (s.min(t), s.max(t))
            })
            .collect();
        spans.sort_by(|x, y| x.0.total_cmp(&y.0));
        // Merge touching spans.
        let mut out: Vec<(f64, f64)> = Vec::new();
        for s in spans {
            match out.last_mut() {
                Some(last) if s.0 <= last.1 + MIN_PIECE_M => last.1 = last.1.max(s.1),
                _ => out.push(s),
            }
        }
        out
    }

    /// An opening placed on a room edge: kept inside the visible span it
    /// is in (half the wall thickness clear of the span ends). Refused
    /// when it does not fit.
    pub fn fit_opening(&self, o: RoomOpening) -> Result<RoomOpening, String> {
        let plane = self.room(o.room).map(|r| r.plane).ok_or("no such room")?;
        let clear = self.layout(plane).thickness_m / 2.0;
        let centre = o.offset_m + o.width_m / 2.0;
        let spans = self.edge_spans(o.room, o.edge);
        let span = spans
            .iter()
            .find(|(s, t)| centre >= *s - EPS && centre <= *t + EPS)
            .or_else(|| spans.first())
            .ok_or("This room edge has no visible wall")?;
        let (lo, hi) = (span.0 + clear, span.1 - clear);
        if o.width_m > hi - lo + 1e-9 {
            return Err("This wall is too short for the opening".to_owned());
        }
        Ok(RoomOpening { offset_m: o.offset_m.clamp(lo, hi - o.width_m), ..o })
    }
}

/// A room's boundary in Edit Mode: its points, and its edges as
/// two-point faces (a hidden edge is a void face, so the page draws it
/// dashed). Points and edges move, insert, and delete; the room stays a
/// simple counter-clockwise loop.
#[derive(Debug, Clone, PartialEq)]
pub struct RoomProfile {
    pub room: Room,
}

impl RoomProfile {
    /// The edge (start point id) between two neighbouring points.
    fn edge_of(&self, e: EdgeKey) -> Option<u32> {
        let n = self.room.boundary.len();
        (0..n).find_map(|i| {
            let (a, b) = (self.room.boundary[i].id, self.room.boundary[(i + 1) % n].id);
            (EdgeKey::new(a, b) == e).then_some(a)
        })
    }

    /// The point ids of some edges (both ends).
    fn edge_points(&self, edges: &[u32]) -> Vec<u32> {
        let n = self.room.boundary.len();
        let mut ids = Vec::new();
        for e in edges {
            if let Some(i) = self.room.boundary.iter().position(|p| p.id == *e) {
                ids.push(self.room.boundary[i].id);
                ids.push(self.room.boundary[(i + 1) % n].id);
            }
        }
        ids.sort_unstable();
        ids.dedup();
        ids
    }

    fn moved(&self, ids: &[u32], delta: P2) -> Result<Room, EditError> {
        if ids.iter().any(|id| !self.room.boundary.iter().any(|p| p.id == *id)) {
            return Err(EditError::Unknown);
        }
        let boundary = self
            .room
            .boundary
            .iter()
            .map(|p| if ids.contains(&p.id) { RunPoint { id: p.id, uv: [p.uv[0] + delta[0], p.uv[1] + delta[1]] } } else { *p })
            .collect();
        self.room.with_boundary(boundary).map_err(EditError::Invalid)
    }

    fn without(&self, ids: &[u32]) -> Result<Room, EditError> {
        let boundary: Vec<RunPoint> = self.room.boundary.iter().filter(|p| !ids.contains(&p.id)).copied().collect();
        if boundary.len() < 3 {
            return Err(EditError::Invalid(Invalid::TooFewPoints));
        }
        self.room.with_boundary(boundary).map_err(EditError::Invalid)
    }
}

impl ProfileModel for RoomProfile {
    fn view(&self) -> ProfileView {
        let n = self.room.boundary.len();
        ProfileView {
            points: self.room.boundary.iter().map(|p| ProfilePoint { id: p.id, uv: p.uv }).collect(),
            faces: (0..n)
                .map(|i| {
                    let (a, b) = (self.room.boundary[i].id, self.room.boundary[(i + 1) % n].id);
                    let kind = if self.room.is_hidden(a) { FaceKind::Void { depth: None } } else { FaceKind::Solid { thickness: 0.0 } };
                    ProfileFace { id: i as u32, points: vec![a, b], kind }
                })
                .collect(),
        }
    }

    fn apply(&self, edit: &Edit) -> Result<Self, EditError> {
        let edges = |keys: &[EdgeKey]| -> Result<Vec<u32>, EditError> {
            keys.iter().map(|k| self.edge_of(*k).ok_or(EditError::Unknown)).collect()
        };
        let room = match edit {
            Edit::MovePoints { ids, delta } => self.moved(ids, *delta)?,
            Edit::MoveEdges { edges: keys, delta } => self.moved(&self.edge_points(&edges(keys)?), *delta)?,
            Edit::MoveFaces { faces, delta } => {
                let starts: Vec<u32> = faces.iter().filter_map(|f| self.room.boundary.get(*f as usize).map(|p| p.id)).collect();
                self.moved(&self.edge_points(&starts), *delta)?
            }
            Edit::InsertPoint { edge, uv } => {
                let start = self.edge_of(*edge).ok_or(EditError::Unknown)?;
                let (a, b) = self.room.edge_ends(start).ok_or(EditError::Unknown)?;
                let t = (along(a, b, *uv) / dist(a, b).max(EPS)).clamp(0.0, 1.0);
                let at = [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t];
                let id = self.room.boundary.iter().map(|p| p.id).max().unwrap_or(0) + 1;
                let i = self.room.boundary.iter().position(|p| p.id == start).ok_or(EditError::Unknown)?;
                let mut boundary = self.room.boundary.clone();
                boundary.insert(i + 1, RunPoint { id, uv: at });
                let mut room = self.room.with_boundary(boundary).map_err(EditError::Invalid)?;
                // Both halves of a hidden edge stay hidden.
                if self.room.is_hidden(start) {
                    room.set_hidden(&[id], true);
                }
                room
            }
            Edit::DeletePoints(ids) => self.without(ids)?,
            // Each edge's end point merges into its start.
            Edit::DeleteEdges(keys) => {
                let n = self.room.boundary.len();
                let ends: Vec<u32> = edges(keys)?
                    .iter()
                    .filter_map(|e| self.room.boundary.iter().position(|p| p.id == *e))
                    .map(|i| self.room.boundary[(i + 1) % n].id)
                    .collect();
                self.without(&ends)?
            }
            _ => return Err(EditError::NoEffect("That edit does not apply to a room")),
        };
        Ok(RoomProfile { room })
    }
}

/// Samples per axis when placing a room label.
const LABEL_SAMPLES: usize = 16;

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

/// Distance along a→b of the projection of `p`.
fn along(a: P2, b: P2, p: P2) -> f64 {
    let len = dist(a, b);
    if len <= 0.0 {
        return 0.0;
    }
    ((p[0] - a[0]) * (b[0] - a[0]) + (p[1] - a[1]) * (b[1] - a[1])) / len
}

/// c–d lies on the line of a–b and overlaps it by more than a point.
fn collinear_overlap(a: P2, b: P2, c: P2, d: P2) -> bool {
    let on = |p: P2| {
        let len = dist(a, b).max(EPS);
        (((b[0] - a[0]) * (p[1] - a[1]) - (b[1] - a[1]) * (p[0] - a[0])) / len).abs() <= EPS
    };
    if !on(c) || !on(d) {
        return false;
    }
    let len = dist(a, b);
    let (s, t) = (along(a, b, c), along(a, b, d));
    s.max(t).min(len) - s.min(t).max(0.0) > MIN_PIECE_M
}

fn same_segment(a: P2, b: P2, c: P2, d: P2) -> bool {
    let near = |p: P2, q: P2| dist(p, q) <= 1e-6;
    (near(a, c) && near(b, d)) || (near(a, d) && near(b, c))
}

/// The parts of a–b that are not strictly inside any of `polys`.
fn outside_parts(a: P2, b: P2, polys: &[Vec<P2>]) -> Vec<(P2, P2)> {
    let len = dist(a, b);
    if len <= MIN_PIECE_M {
        return Vec::new();
    }
    let mut ts = vec![0.0, 1.0];
    for poly in polys {
        let n = poly.len();
        for i in 0..n {
            if let Some(t) = segment_param(a, b, poly[i], poly[(i + 1) % n]) {
                ts.push(t);
            }
            // Polygon vertices on the segment split it too.
            let t = along(a, b, poly[i]) / len;
            if (0.0..=1.0).contains(&t) && point_segment_distance(poly[i], a, b) <= EPS {
                ts.push(t);
            }
        }
    }
    ts.sort_by(f64::total_cmp);
    ts.dedup_by(|x, y| (*x - *y).abs() * len <= MIN_PIECE_M);
    let at = |t: f64| [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t];
    let mut out: Vec<(P2, P2)> = Vec::new();
    for w in ts.windows(2) {
        let mid = at((w[0] + w[1]) / 2.0);
        if polys.iter().any(|p| point_in_polygon(mid, p)) {
            continue;
        }
        match out.last_mut() {
            Some(last) if dist(last.1, at(w[0])) <= MIN_PIECE_M => last.1 = at(w[1]),
            _ => out.push((at(w[0]), at(w[1]))),
        }
    }
    out
}

/// Where a–b crosses c–d, as a parameter along a–b (proper crossings).
fn segment_param(a: P2, b: P2, c: P2, d: P2) -> Option<f64> {
    let r = [b[0] - a[0], b[1] - a[1]];
    let s = [d[0] - c[0], d[1] - c[1]];
    let den = r[0] * s[1] - r[1] * s[0];
    if den.abs() <= 1e-12 {
        return None;
    }
    let q = [c[0] - a[0], c[1] - a[1]];
    let t = (q[0] * s[1] - q[1] * s[0]) / den;
    let u = (q[0] * r[1] - q[1] * r[0]) / den;
    ((0.0..=1.0).contains(&t) && (0.0..=1.0).contains(&u)).then_some(t)
}

/// `subject` minus `clip` (non-zero fill): shapes as an outer ring then
/// holes.
fn difference(subject: &[Vec<P2>], clip: &[Vec<P2>]) -> Vec<Vec<Vec<P2>>> {
    let mut options: OverlayOptions<f64, i64> = OverlayOptions::default();
    options.preserve_output_collinear = true;
    options.min_output_area = EPS * EPS;
    let mut engine = FloatOverlay::<P2, i64>::from_subj_and_clip_custom(&subject.to_vec(), &clip.to_vec(), options, Solver::default());
    engine.overlay(OverlayRule::Difference, FillRule::NonZero)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLANE: EntityId = EntityId(7);

    fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> Vec<P2> {
        geom::rectangle([x0, y0], [x1, y1])
    }

    #[test]
    fn default_names_count_up_with_three_digits() {
        assert_eq!(default_name([].iter().copied()), "Room 001");
        assert_eq!(default_name(["Room 001", "Kitchen", "Room 009"].iter().copied()), "Room 010");
        let mut rooms = Rooms::default();
        rooms.add(PLANE, &rect(0.0, 0.0, 4.0, 3.0)).expect("a");
        rooms.add(PLANE, &rect(4.0, 0.0, 7.0, 3.0)).expect("b");
        let names: Vec<&str> = rooms.rooms.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["Room 001", "Room 002"]);
    }

    #[test]
    fn a_shared_boundary_is_one_wall() {
        let mut rooms = Rooms::default();
        rooms.add(PLANE, &rect(0.0, 0.0, 4.0, 3.0)).expect("kitchen");
        rooms.add(PLANE, &rect(4.0, 0.0, 7.0, 3.0)).expect("dining");
        let walls = rooms.walls(PLANE);
        // 4 + 4 edges, one shared: 7 pieces.
        assert_eq!(walls.len(), 7, "{walls:?}");
        let shared = walls.iter().find(|w| w.covers.len() == 2).expect("shared");
        assert!((shared.a[0] - 4.0).abs() < 1e-9 && (shared.b[0] - 4.0).abs() < 1e-9);
    }

    #[test]
    fn a_hidden_edge_hides_the_shared_wall() {
        let mut rooms = Rooms::default();
        let a = rooms.add(PLANE, &rect(0.0, 0.0, 4.0, 3.0)).expect("kitchen");
        rooms.add(PLANE, &rect(4.0, 0.0, 7.0, 3.0)).expect("nook");
        // The kitchen's east edge: from (4, 0) (point 2) to (4, 3).
        let east = rooms.room(a).and_then(|r| r.boundary.iter().find(|p| p.uv == [4.0, 0.0]).map(|p| p.id)).expect("east");
        rooms.room_mut(a).expect("room").set_hidden(&[east], true);
        let walls = rooms.walls(PLANE);
        let shared = walls.iter().find(|w| w.covers.len() == 2).expect("shared");
        assert!(shared.hidden);
        assert_eq!(walls.iter().filter(|w| w.hidden).count(), 1);
    }

    #[test]
    fn a_higher_room_cuts_into_a_lower_one() {
        let mut rooms = Rooms::default();
        let big = rooms.add(PLANE, &rect(0.0, 0.0, 6.0, 4.0)).expect("big");
        let closet = rooms.add(PLANE, &rect(4.0, 2.0, 8.0, 5.0)).expect("closet");
        assert!((rooms.region_area(big) - (24.0 - 4.0)).abs() < 1e-6);
        assert!((rooms.region_area(closet) - 12.0).abs() < 1e-6);
        // Sent below, the big room takes the overlap back.
        assert!(rooms.restack(closet, false));
        assert!((rooms.region_area(big) - 24.0).abs() < 1e-6);
        assert!((rooms.region_area(closet) - 8.0).abs() < 1e-6);
        // No wall piece runs inside the room above it.
        for w in rooms.walls(PLANE) {
            let mid = [(w.a[0] + w.b[0]) / 2.0, (w.a[1] + w.b[1]) / 2.0];
            assert!(!point_in_polygon(mid, &rect(0.0, 0.0, 6.0, 4.0)), "{w:?}");
        }
    }

    #[test]
    fn the_room_profile_edits_its_boundary() {
        let mut rooms = Rooms::default();
        let id = rooms.add(PLANE, &rect(0.0, 0.0, 4.0, 3.0)).expect("room");
        let mut room = rooms.room(id).cloned().expect("room");
        room.set_hidden(&[1], true); // the south edge (point 1 -> 2)
        let p = RoomProfile { room };
        assert_eq!(p.view().faces.iter().filter(|f| f.kind.is_void()).count(), 1);
        // A point on the hidden edge: both halves stay hidden.
        let p = p.apply(&Edit::InsertPoint { edge: EdgeKey::new(1, 2), uv: [2.0, 0.0] }).expect("insert");
        assert_eq!(p.room.boundary.len(), 5);
        assert_eq!(p.room.hidden_edges, vec![1, 5]);
        // Moving it out makes a pentagon; a fold is refused.
        let p = p.apply(&Edit::MovePoints { ids: vec![5], delta: [0.0, -1.0] }).expect("move");
        assert!((signed_area(&p.room.polygon()) - 14.0).abs() < 1e-9);
        assert!(p.apply(&Edit::MovePoints { ids: vec![5], delta: [0.0, 5.0] }).is_err());
        // Deleting it prunes its hidden edge; a triangle is the minimum.
        let p = p.apply(&Edit::DeletePoints(vec![5])).expect("delete");
        assert_eq!(p.room.hidden_edges, vec![1]);
        let p = p.apply(&Edit::DeletePoints(vec![4])).expect("triangle");
        assert!(p.apply(&Edit::DeletePoints(vec![3])).is_err());
    }

    #[test]
    fn a_label_sits_inside_an_l_shaped_region() {
        let mut rooms = Rooms::default();
        let l = rooms.add(PLANE, &rect(0.0, 0.0, 4.0, 4.0)).expect("big");
        rooms.add(PLANE, &rect(2.0, 2.0, 5.0, 5.0)).expect("corner");
        let (_, shapes) = rooms.regions(PLANE).into_iter().find(|(id, _)| *id == l).expect("region");
        let p = label_point(&shapes).expect("label");
        assert!(point_in_polygon(p, &shapes[0][0]), "{p:?}");
        assert!(!(p[0] > 2.0 && p[1] > 2.0), "not in the cut corner: {p:?}");
    }

    #[test]
    fn openings_fit_inside_the_visible_span() {
        let mut rooms = Rooms::default();
        let a = rooms.add(PLANE, &rect(0.0, 0.0, 4.0, 3.0)).expect("room");
        let south = rooms.room(a).map(|r| r.boundary[0].id).expect("south");
        let (room, edge, u) = rooms.edge_at(PLANE, [2.0, 0.02], 0.1).expect("on the south wall");
        assert_eq!((room, edge), (a, south));
        assert!((u - 2.0).abs() < 1e-9);
        let o = RoomOpening {
            id: 1, room, edge, offset_m: 3.6, sill_m: 0.0, width_m: 0.9, height_m: 2.1, kind: OpeningKind::Door, depth_m: None,
        };
        let fitted = rooms.fit_opening(o).expect("fits");
        assert!((fitted.offset_m - (4.0 - 0.057 - 0.9)).abs() < 1e-9, "{fitted:?}");
        assert!(rooms.fit_opening(RoomOpening { width_m: 4.0, ..o }).is_err());
    }
}

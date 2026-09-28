//! Rooms preview (`?rooms`): the page's room tools over the app-side
//! adapter (`authoring::rooms`) — the Rooms tool, the plan overlays
//! (region fill, name + area label, the wall network as bands), the
//! Rooms group of the Model tree (rename, restack, delete), Room Edit
//! Mode (points, edges, hidden walls), room wall openings, and the layout
//! wall settings.
//!
//! The adapter's state is session data, persisted by the page beside
//! its session. Its history joins the one Undo / Redo: each room change
//! is one step, recorded with the document's undo depth at the time, and
//! Undo takes the room step while no document step came after it (see
//! [`AuthorApp::rooms_undo`]). Phase B moves all of it into the library
//! (document entities, real wall solids) and this module becomes a thin
//! caller.

use vim_design_lib::EntityId;
use vim_design_lib::wall_run::{OpeningKind, RunPoint};
use wasm_bindgen::prelude::*;

use super::camera::ViewMode;
use super::edit::{EditProfile, EditTarget};
use super::{AuthorApp, MAX_WALL_HEIGHT_M, MAX_WALL_THICKNESS_M, MIN_WALL_HEIGHT_M, MIN_WALL_THICKNESS_M, SketchFrame, eid};
use crate::authoring::edit::interact::SelectMode;
use crate::authoring::edit::session::EditSession;
use crate::authoring::geom::{P2, point_in_polygon};
use crate::authoring::rooms::{Layout, Room, RoomOpening, RoomProfile, Rooms};
use glam::Vec3;

/// Room change history: (document undo depth when recorded, the rooms
/// before the change) per undo step; the same for redo.
#[derive(Debug, Clone, Default)]
pub struct RoomHistory {
    undo: Vec<(usize, Rooms)>,
    redo: Vec<(usize, Rooms)>,
}

/// A session over rooms (Room Edit Mode or Openings mode): the history
/// length and the rooms at its entry (✗ restores them).
#[derive(Debug, Clone)]
pub struct RoomSession {
    floor: usize,
    entry: Rooms,
}

fn kind_name(k: OpeningKind) -> &'static str {
    match k {
        OpeningKind::Window => "window",
        OpeningKind::Door => "door",
    }
}

fn opening_kind(name: &str) -> Option<OpeningKind> {
    match name {
        "window" => Some(OpeningKind::Window),
        "door" => Some(OpeningKind::Door),
        _ => None,
    }
}

#[wasm_bindgen]
impl AuthorApp {
    /// The page turns the rooms preview on (`?rooms`).
    pub fn set_rooms_preview(&mut self, on: bool) {
        self.rooms_preview = on;
    }

    pub fn rooms_preview(&self) -> bool {
        self.rooms_preview
    }

    /// Bumped on every room change (the page persists and repaints).
    pub fn rooms_revision(&self) -> f64 {
        self.rooms_revision as f64
    }

    /// Every room (the Model tree): id, name, plane, level, precedence,
    /// rank on its plane (1 = top), effective area, hidden edge count.
    pub fn rooms_json(&self) -> String {
        let items: Vec<serde_json::Value> = self
            .rooms
            .rooms
            .iter()
            .map(|r| {
                let rank = self.rooms.ranked(r.plane).iter().position(|x| x.id == r.id).map_or(0, |i| i + 1);
                let count = self.rooms.rooms.iter().filter(|x| x.plane == r.plane).count();
                serde_json::json!({
                    "id": r.id, "name": r.name, "plane": r.plane.0 as f64,
                    "level": self.root_level(r.plane).map(|l| l.0 as f64),
                    "precedence": r.precedence, "rank": rank, "ofRank": count,
                    "area": self.rooms.region_area(r.id), "hiddenEdges": r.hidden_edges.len(),
                    "edges": r.boundary.len(),
                    "selected": self.room_selection == Some(r.id),
                })
            })
            .collect();
        serde_json::Value::Array(items).to_string()
    }

    /// The plan overlays of the active plane's rooms, in device pixels:
    /// regions (outer ring + holes), labels (name, area, at the region's
    /// centre), the wall network (a band per piece; hidden pieces as a
    /// dashed centre line), and the openings.
    pub fn rooms_hud_json(&self) -> String {
        let off = r#"{"active":false}"#.to_owned();
        if !self.rooms_preview || self.camera.mode != ViewMode::Plan {
            return off;
        }
        let Some(plane) = self.plane() else { return off };
        let frame = self.plane_frame(plane);
        let (w, h) = self.size_f();
        let proj = |p: P2| self.camera.project(frame.world(p), w, h).map(|(x, y)| [x, y]);
        let ring = |r: &[P2]| -> Vec<[f32; 2]> { r.iter().filter_map(|p| proj(*p)).collect() };
        let editing = self.room_edit;
        let mut regions = Vec::new();
        let mut labels = Vec::new();
        for (id, shapes) in self.rooms.regions(plane) {
            let Some(room) = self.rooms.room(id) else { continue };
            let rank = self.rooms.ranked(plane).iter().position(|x| x.id == id).unwrap_or(0);
            let rings: Vec<Vec<[f32; 2]>> = shapes.iter().flat_map(|s| s.iter().map(|r| ring(r))).collect();
            let area = self.rooms.region_area(id);
            let at = crate::authoring::rooms::label_point(&shapes).and_then(proj);
            regions.push(serde_json::json!({
                "id": id, "rings": rings, "sel": self.room_selection == Some(id), "editing": editing == Some(id), "color": rank,
            }));
            if let Some([x, y]) = at {
                labels.push(serde_json::json!({ "id": id, "x": x, "y": y, "name": room.name, "area": area }));
            }
        }
        let layout = self.rooms.layout(plane);
        let half = layout.thickness_m / 2.0;
        let band = |a: P2, b: P2, half: f64| -> Vec<[f32; 2]> {
            let len = crate::authoring::geom::dist(a, b).max(1e-9);
            let n = [-(b[1] - a[1]) / len * half, (b[0] - a[0]) / len * half];
            ring(&[[a[0] + n[0], a[1] + n[1]], [b[0] + n[0], b[1] + n[1]], [b[0] - n[0], b[1] - n[1]], [a[0] - n[0], a[1] - n[1]]])
        };
        let walls: Vec<serde_json::Value> = self
            .rooms
            .walls(plane)
            .iter()
            .map(|p| {
                let line = [proj(p.a), proj(p.b)];
                serde_json::json!({
                    "pts": if p.hidden { Vec::new() } else { band(p.a, p.b, half) },
                    "line": line.iter().flatten().flat_map(|q| q.iter().copied()).collect::<Vec<f32>>(),
                    "hidden": p.hidden,
                })
            })
            .collect();
        let selected = self.openings.as_ref().and_then(|o| o.room_selected);
        let openings: Vec<serde_json::Value> = self
            .rooms
            .openings
            .iter()
            .filter_map(|o| {
                let room = self.rooms.room(o.room).filter(|r| r.plane == plane)?;
                let (a, b) = room.edge_ends(o.edge)?;
                let len = crate::authoring::geom::dist(a, b).max(1e-9);
                let d = [(b[0] - a[0]) / len, (b[1] - a[1]) / len];
                let s = [a[0] + d[0] * o.offset_m, a[1] + d[1] * o.offset_m];
                let t = [s[0] + d[0] * o.width_m, s[1] + d[1] * o.width_m];
                Some(serde_json::json!({
                    "id": o.id, "kind": kind_name(o.kind), "pts": band(s, t, half * 1.2),
                    "niche": o.depth_m.is_some(), "sel": selected == Some(o.id),
                }))
            })
            .collect();
        serde_json::json!({
            "active": true, "regions": regions, "labels": labels, "walls": walls, "openings": openings,
        })
        .to_string()
    }

    /// Select a room (clears the element selection); -1 clears.
    pub fn room_select(&mut self, id: f64) {
        let id = (id >= 0.0).then_some(id as u32).filter(|i| self.rooms.room(*i).is_some());
        self.room_selection = id;
        if id.is_some() {
            self.selection = None;
            self.refresh_styles();
        }
    }

    pub fn room_selected(&self) -> f64 {
        self.room_selection.map_or(-1.0, f64::from)
    }

    /// The room under a canvas point (its effective region), or -1.
    pub fn room_at(&self, px: f32, py: f32) -> f64 {
        if !self.rooms_preview || self.camera.mode != ViewMode::Plan {
            return -1.0;
        }
        let Some(plane) = self.plane() else { return -1.0 };
        let Some(uv) = self.plane_uv_at(plane, px, py) else { return -1.0 };
        for (id, shapes) in self.rooms.regions(plane) {
            for shape in &shapes {
                let Some(outer) = shape.first() else { continue };
                if point_in_polygon(uv, outer) && !shape[1..].iter().any(|hole| point_in_polygon(uv, hole)) {
                    return f64::from(id);
                }
            }
        }
        -1.0
    }

    /// Rename a room (one undo step). False when the name is empty.
    pub fn room_rename(&mut self, id: f64, name: &str) -> bool {
        let name = name.trim().to_owned();
        if name.is_empty() {
            return false;
        }
        let id = id as u32;
        if self.rooms.room(id).is_none_or(|r| r.name == name) {
            return false;
        }
        self.rooms_change(|r| {
            r.room_mut(id).ok_or("no such room")?.name = name;
            Ok(())
        })
        .is_ok()
    }

    /// Bring a room forward (`up`: it cuts into the room it passes) or
    /// send it backward. False at the end of the order.
    pub fn room_restack(&mut self, id: f64, up: bool) -> bool {
        let id = id as u32;
        self.rooms_change(|r| if r.restack(id, up) { Ok(()) } else { Err("at the end".to_owned()) }).is_ok()
    }

    pub fn room_delete(&mut self, id: f64) -> bool {
        let id = id as u32;
        if self.rooms.room(id).is_none() {
            return false;
        }
        let ok = self.rooms_change(|r| {
            r.remove(id);
            Ok(())
        });
        if self.room_selection == Some(id) {
            self.room_selection = None;
        }
        ok.is_ok()
    }

    /// The room walls' settings of the active plane: thickness, height
    /// mode ("fixed" / "upto"), top plane, offset, fixed height.
    pub fn room_layout_json(&self) -> String {
        let Some(plane) = self.plane() else { return "null".to_owned() };
        let l = self.rooms.layout(plane);
        serde_json::json!({
            "plane": plane.0 as f64, "thickness": l.thickness_m, "mode": if l.top.is_some() { "upto" } else { "fixed" },
            "topPlane": l.top.map(|p| p.0 as f64), "topOffset": l.top_offset_m, "height": l.height_m,
            "rooms": self.rooms.rooms.iter().filter(|r| r.plane == plane).count(),
        })
        .to_string()
    }

    /// Change the active plane's room wall settings (NaN keeps a value;
    /// `mode` "fixed" / "upto" / "" keeps it). One undo step.
    pub fn set_room_layout(&mut self, thickness: f64, mode: &str, plane: f64, offset: f64, height: f64) -> bool {
        let Some(base) = self.plane() else { return false };
        let mut l = self.rooms.layout(base);
        if thickness.is_finite() {
            l.thickness_m = thickness.clamp(MIN_WALL_THICKNESS_M, MAX_WALL_THICKNESS_M);
            // New room walls start with the last thickness chosen.
            self.remembered.room_wall_thickness = l.thickness_m;
        }
        if height.is_finite() {
            l.height_m = height.clamp(MIN_WALL_HEIGHT_M, MAX_WALL_HEIGHT_M);
        }
        match mode {
            "fixed" => l.top = None,
            "upto" if plane >= 0.0 && self.root_level(eid(plane)).is_some() => l.top = Some(eid(plane)),
            _ => {}
        }
        if offset.is_finite() {
            l.top_offset_m = offset.clamp(-MAX_WALL_HEIGHT_M, MAX_WALL_HEIGHT_M);
        }
        if l == self.rooms.layout(base) {
            return false;
        }
        self.rooms_change(|r| {
            r.set_layout(l);
            Ok(())
        })
        .is_ok()
    }

    /// Enter Room Edit Mode on a room: its points and edges, in plan on
    /// its plane.
    pub fn room_edit_begin(&mut self, id: f64) -> bool {
        let id = id as u32;
        if self.edit.is_some() || self.openings.is_some() {
            return false;
        }
        let Some(room) = self.rooms.room(id).cloned() else { return false };
        if self.prev_camera.is_none() {
            self.prev_camera = Some(self.camera.clone());
        }
        self.set_active_plane(room.plane.0 as f64);
        self.camera.set_mode(ViewMode::Plan);
        self.room_session = Some(RoomSession { floor: self.room_history.undo.len(), entry: self.rooms.clone() });
        self.room_edit = Some(id);
        self.room_selection = Some(id);
        self.edit_target = EditTarget::Room;
        let session = EditSession::new(EditProfile::Room(RoomProfile { room: room.clone() }), room.plane, None, room.name.clone());
        self.edit_sketch = None;
        self.edit_entry_element = None;
        self.start_edit(session);
        if let Some(s) = self.edit.as_mut() {
            s.set_mode(SelectMode::Edges);
        }
        self.room_selection = Some(id);
        true
    }

    /// Room Edit Mode: hide (no wall) or show the walls of the selected
    /// edges. One undo step.
    pub fn room_set_hidden(&mut self, hidden: bool) -> String {
        let Some(id) = self.room_edit else { return r#"{"result":"none"}"#.to_owned() };
        let Some(s) = self.edit.as_ref() else { return r#"{"result":"none"}"#.to_owned() };
        let EditProfile::Room(p) = &s.model else { return r#"{"result":"none"}"#.to_owned() };
        let n = p.room.boundary.len();
        let edges: Vec<u32> = s
            .selection
            .edges
            .iter()
            .filter_map(|e| {
                (0..n).find_map(|i| {
                    let (a, b) = (p.room.boundary[i].id, p.room.boundary[(i + 1) % n].id);
                    (crate::authoring::edit::EdgeKey::new(a, b) == *e).then_some(a)
                })
            })
            .collect();
        if edges.is_empty() {
            return serde_json::json!({ "result": "rejected", "reason": "Select the edges whose wall to hide" }).to_string();
        }
        let ok = self.rooms_change(|r| {
            r.room_mut(id).ok_or("no such room")?.set_hidden(&edges, hidden);
            Ok(())
        });
        self.reload_room_edit();
        match ok {
            Ok(()) => r#"{"result":"changed"}"#.to_owned(),
            Err(e) => serde_json::json!({ "result": "rejected", "reason": e }).to_string(),
        }
    }

    /// The rooms preview state for the page to persist (JSON).
    pub fn rooms_state_json(&self) -> String {
        let rooms: Vec<serde_json::Value> = self
            .rooms
            .rooms
            .iter()
            .map(|r| {
                serde_json::json!({
                    "id": r.id, "plane": r.plane.0 as f64, "name": r.name, "precedence": r.precedence,
                    "boundary": r.boundary.iter().map(|p| [f64::from(p.id), p.uv[0], p.uv[1]]).collect::<Vec<_>>(),
                    "hidden": r.hidden_edges,
                })
            })
            .collect();
        let layouts: Vec<serde_json::Value> = self
            .rooms
            .layouts
            .iter()
            .map(|l| {
                serde_json::json!({
                    "plane": l.plane.0 as f64, "thickness": l.thickness_m, "height": l.height_m,
                    "top": l.top.map(|t| t.0 as f64), "topOffset": l.top_offset_m,
                })
            })
            .collect();
        let openings: Vec<serde_json::Value> = self
            .rooms
            .openings
            .iter()
            .map(|o| {
                serde_json::json!({
                    "id": o.id, "room": o.room, "edge": o.edge, "offset": o.offset_m, "sill": o.sill_m,
                    "width": o.width_m, "height": o.height_m, "kind": kind_name(o.kind), "depth": o.depth_m,
                })
            })
            .collect();
        serde_json::json!({ "rooms": rooms, "layouts": layouts, "openings": openings, "revision": self.rooms_revision }).to_string()
    }

    /// Restore persisted rooms (rooms of planes that no longer exist are
    /// dropped). Not an undo step.
    pub fn load_rooms_json(&mut self, json: &str) -> bool {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else { return false };
        let arr = |k: &str| v.get(k).and_then(serde_json::Value::as_array).cloned().unwrap_or_default();
        let num = |x: &serde_json::Value, k: &str| x.get(k).and_then(serde_json::Value::as_f64);
        let mut rooms = Rooms::default();
        for r in arr("rooms") {
            let plane = eid(num(&r, "plane").unwrap_or(-1.0));
            if self.root_level(plane).is_none() {
                continue;
            }
            let boundary: Vec<RunPoint> = r
                .get("boundary")
                .and_then(serde_json::Value::as_array)
                .map(|pts| {
                    pts.iter()
                        .filter_map(|p| {
                            let p = p.as_array()?;
                            Some(RunPoint { id: p.first()?.as_f64()? as u32, uv: [p.get(1)?.as_f64()?, p.get(2)?.as_f64()?] })
                        })
                        .collect()
                })
                .unwrap_or_default();
            let hidden: Vec<u32> = r
                .get("hidden")
                .and_then(serde_json::Value::as_array)
                .map(|h| h.iter().filter_map(|x| x.as_u64().map(|x| x as u32)).collect())
                .unwrap_or_default();
            let room = Room {
                id: num(&r, "id").unwrap_or(0.0) as u32,
                plane,
                name: r.get("name").and_then(serde_json::Value::as_str).unwrap_or("Room").to_owned(),
                precedence: num(&r, "precedence").unwrap_or(0.0) as i32,
                boundary,
                hidden_edges: hidden,
            };
            if room.id > 0 && room.with_boundary(room.boundary.clone()).is_ok() && rooms.room(room.id).is_none() {
                rooms.rooms.push(room);
            }
        }
        for l in arr("layouts") {
            let plane = eid(num(&l, "plane").unwrap_or(-1.0));
            if self.root_level(plane).is_none() {
                continue;
            }
            let mut layout = Layout::new(plane);
            layout.thickness_m = num(&l, "thickness").unwrap_or(layout.thickness_m).clamp(MIN_WALL_THICKNESS_M, MAX_WALL_THICKNESS_M);
            layout.height_m = num(&l, "height").unwrap_or(layout.height_m).clamp(MIN_WALL_HEIGHT_M, MAX_WALL_HEIGHT_M);
            layout.top = num(&l, "top").map(eid).filter(|t| self.root_level(*t).is_some());
            layout.top_offset_m = num(&l, "topOffset").unwrap_or(0.0);
            rooms.set_layout(layout);
        }
        for o in arr("openings") {
            let Some(kind) = o.get("kind").and_then(serde_json::Value::as_str).and_then(opening_kind) else { continue };
            let opening = RoomOpening {
                id: num(&o, "id").unwrap_or(0.0) as u32,
                room: num(&o, "room").unwrap_or(0.0) as u32,
                edge: num(&o, "edge").unwrap_or(0.0) as u32,
                offset_m: num(&o, "offset").unwrap_or(0.0),
                sill_m: num(&o, "sill").unwrap_or(0.0),
                width_m: num(&o, "width").unwrap_or(0.9),
                height_m: num(&o, "height").unwrap_or(2.1),
                kind,
                depth_m: num(&o, "depth"),
            };
            if rooms.room(opening.room).is_some() {
                rooms.openings.push(opening);
            }
        }
        self.rooms = rooms;
        self.room_history = RoomHistory::default();
        self.room_selection = None;
        true
    }
}

impl AuthorApp {
    /// One room change: applied to a copy, kept when `f` succeeds (one
    /// undo step), dropped otherwise.
    pub(super) fn rooms_change<T>(&mut self, f: impl FnOnce(&mut Rooms) -> Result<T, String>) -> Result<T, String> {
        let before = self.rooms.clone();
        let mut next = before.clone();
        let out = f(&mut next)?;
        if next != before {
            self.room_history.undo.push((self.doc.undo_depth(), before));
            self.room_history.redo.clear();
            self.rooms = next;
            self.rooms_revision += 1;
        }
        Ok(out)
    }

    fn room_floor(&self) -> usize {
        self.room_session.as_ref().map_or(0, |s| s.floor)
    }

    /// Undo the last room change, when no document step came after it
    /// (and not past a session's entry).
    pub(super) fn rooms_can_undo(&self) -> bool {
        self.room_history.undo.len() > self.room_floor()
            && self.room_history.undo.last().is_some_and(|(depth, _)| *depth == self.doc.undo_depth())
    }

    pub(super) fn rooms_can_redo(&self) -> bool {
        self.room_history.redo.last().is_some_and(|(depth, _)| *depth == self.doc.undo_depth())
    }

    pub(super) fn rooms_undo(&mut self) -> bool {
        if !self.rooms_can_undo() {
            return false;
        }
        let Some((depth, before)) = self.room_history.undo.pop() else { return false };
        let now = std::mem::replace(&mut self.rooms, before);
        self.room_history.redo.push((depth, now));
        self.rooms_revision += 1;
        self.after_room_history();
        true
    }

    pub(super) fn rooms_redo(&mut self) -> bool {
        if !self.rooms_can_redo() {
            return false;
        }
        let Some((depth, after)) = self.room_history.redo.pop() else { return false };
        let now = std::mem::replace(&mut self.rooms, after);
        self.room_history.undo.push((depth, now));
        self.rooms_revision += 1;
        self.after_room_history();
        true
    }

    fn after_room_history(&mut self) {
        if self.room_selection.is_some_and(|id| self.rooms.room(id).is_none()) {
            self.room_selection = None;
        }
        self.reload_room_edit();
    }

    /// Room Edit Mode: show the stored room again (after a change, undo,
    /// or redo).
    pub(super) fn reload_room_edit(&mut self) {
        let Some(room) = self.room_edit.and_then(|id| self.rooms.room(id)).cloned() else { return };
        if let Some(s) = self.edit.as_mut() {
            s.name = room.name.clone();
            s.set_model(EditProfile::Room(RoomProfile { room }));
        }
    }

    /// Room Edit Mode: store an accepted boundary edit (one step).
    pub(super) fn store_room_edit(&mut self, room: Room, ok: &str) -> String {
        let id = room.id;
        let res = self.rooms_change(|r| {
            let slot = r.room_mut(id).ok_or("no such room")?;
            slot.boundary = room.boundary;
            slot.hidden_edges = room.hidden_edges;
            // Openings on vanished edges go with them.
            let edges = slot.edges();
            r.openings.retain(|o| o.room != id || edges.contains(&o.edge));
            Ok(())
        });
        self.reload_room_edit();
        match res {
            Ok(()) => serde_json::json!({ "result": ok }).to_string(),
            Err(e) => serde_json::json!({ "result": "rejected", "reason": e }).to_string(),
        }
    }

    /// Leave a room session: ✓ keeps its steps, ✗ restores the entry.
    pub(super) fn end_room_session(&mut self, keep: bool) -> bool {
        let Some(s) = self.room_session.take() else { return false };
        let changed = self.room_history.undo.len() > s.floor;
        if !keep {
            self.rooms = s.entry;
            self.room_history.undo.truncate(s.floor);
            self.room_history.redo.clear();
            self.rooms_revision += 1;
        }
        self.room_edit = None;
        changed
    }

    /// Start a room session for Openings mode (✗ restores the rooms).
    pub(super) fn begin_room_session(&mut self) {
        self.room_session = Some(RoomSession { floor: self.room_history.undo.len(), entry: self.rooms.clone() });
    }

    /// Room Edit Mode state for the page: the room, and the selected
    /// edges' walls (hidden or not) for the "Hidden wall" toggle.
    pub(super) fn room_edit_state_json(&self) -> serde_json::Value {
        let Some(id) = self.room_edit else { return serde_json::Value::Null };
        let (Some(room), Some(s)) = (self.rooms.room(id), self.edit.as_ref()) else { return serde_json::Value::Null };
        let n = room.boundary.len();
        let selected: Vec<u32> = (0..n)
            .filter(|i| {
                let (a, b) = (room.boundary[*i].id, room.boundary[(i + 1) % n].id);
                s.selection.edges.contains(&crate::authoring::edit::EdgeKey::new(a, b))
            })
            .map(|i| room.boundary[i].id)
            .collect();
        serde_json::json!({
            "id": id, "name": room.name, "edges": n, "hidden": room.hidden_edges.len(),
            "selectedEdges": selected.len(),
            "selectedHidden": !selected.is_empty() && selected.iter().all(|e| room.is_hidden(*e)),
            "area": self.rooms.region_area(id),
        })
    }

    /// The bounds of the room in Room Edit Mode, or of the selected room
    /// (on its plane, as a flat box): what Fit frames.
    pub(super) fn room_focus_bbox(&self) -> Option<([f64; 3], [f64; 3])> {
        let room = self.rooms.room(self.room_edit.or(self.room_selection)?)?;
        let z = self.plane_elevation(room.plane);
        let pts = room.polygon();
        let (mut lo, mut hi) = ([f64::INFINITY, f64::INFINITY, z], [f64::NEG_INFINITY, f64::NEG_INFINITY, z]);
        for p in &pts {
            for k in 0..2 {
                lo[k] = lo[k].min(p[k]);
                hi[k] = hi[k].max(p[k]);
            }
        }
        lo[0].is_finite().then_some((lo, hi))
    }

    /// The frame of a construction plane (world = origin + u·x + v·y).
    fn plane_frame(&self, plane: EntityId) -> SketchFrame {
        SketchFrame { origin: Vec3::new(0.0, 0.0, self.plane_elevation(plane) as f32), u: Vec3::X, v: Vec3::Y }
    }

    /// Where a canvas point is on a plane (plane coordinates).
    pub(super) fn plane_uv_at(&self, plane: EntityId, px: f32, py: f32) -> Option<P2> {
        let (w, h) = self.size_f();
        let z = self.plane_elevation(plane) as f32;
        let hit = self.camera.unproject_to_plane(px, py, w, h, z)?;
        Some([f64::from(hit.x), f64::from(hit.y)])
    }

    /// Meters per device pixel on a plane around a canvas point.
    pub(super) fn plane_m_per_px(&self, plane: EntityId, uv: P2) -> f64 {
        let frame = self.plane_frame(plane);
        1.0 / f64::from(self.px_per_m_at(frame.world(uv), &frame))
    }

    /// A new room from a finished sketch (one step): selected.
    pub(super) fn commit_room(&mut self, plane: EntityId, outline: &[P2]) -> Result<(u32, String), String> {
        let before = self.rooms.layouts.iter().any(|l| l.plane == plane);
        let thickness = self.remembered.room_wall_thickness;
        let id = self.rooms_change(|r| {
            let id = r.add(plane, outline).map_err(|e| e.message().to_owned())?;
            if !before {
                // The plane's first room: its walls take the remembered
                // thickness.
                let mut l = Layout::new(plane);
                l.thickness_m = thickness;
                r.set_layout(l);
            }
            Ok(id)
        })?;
        self.room_selection = Some(id);
        let name = self.rooms.room(id).map(|r| r.name.clone()).unwrap_or_default();
        Ok((id, name))
    }

    /// Openings mode on room walls: the room opening under a canvas
    /// point (plan).
    pub(super) fn room_opening_at(&self, px: f32, py: f32, tol_px: f32) -> Option<u32> {
        if !self.rooms_preview || self.camera.mode != ViewMode::Plan {
            return None;
        }
        let plane = self.plane()?;
        let uv = self.plane_uv_at(plane, px, py)?;
        let tol = f64::from(tol_px) * self.plane_m_per_px(plane, uv);
        let half = self.rooms.layout(plane).thickness_m / 2.0 + tol;
        self.rooms.openings.iter().find_map(|o| {
            let room = self.rooms.room(o.room).filter(|r| r.plane == plane)?;
            let (a, b) = room.edge_ends(o.edge)?;
            let len = crate::authoring::geom::dist(a, b).max(1e-9);
            let d = [(b[0] - a[0]) / len, (b[1] - a[1]) / len];
            let rel = [uv[0] - a[0], uv[1] - a[1]];
            let u = rel[0] * d[0] + rel[1] * d[1];
            let v = -rel[0] * d[1] + rel[1] * d[0];
            (u >= o.offset_m - tol && u <= o.offset_m + o.width_m + tol && v.abs() <= half).then_some(o.id)
        })
    }

    /// Openings mode: place the preset on the room wall under a canvas
    /// point, anchored to the room edge covering it. `None`: no room wall
    /// there.
    pub(super) fn place_room_opening(&mut self, kind: OpeningKind, px: f32, py: f32, tol_px: f32) -> Option<Result<u32, String>> {
        if !self.rooms_preview || self.camera.mode != ViewMode::Plan {
            return None;
        }
        let plane = self.plane()?;
        let uv = self.plane_uv_at(plane, px, py)?;
        let tol = f64::from(tol_px) * self.plane_m_per_px(plane, uv) + self.rooms.layout(plane).thickness_m / 2.0;
        let (room, edge, u) = self.rooms.edge_at(plane, uv, tol)?;
        let size = self.remembered.opening(kind);
        let o = RoomOpening {
            id: self.rooms.next_opening_id(),
            room,
            edge,
            offset_m: u - size.width / 2.0,
            sill_m: size.sill,
            width_m: size.width,
            height_m: size.height,
            kind,
            depth_m: size.depth,
        };
        Some(self.rooms_change(|r| {
            let o = r.fit_opening(o)?;
            r.openings.push(o);
            Ok(o.id)
        }))
    }

    /// Openings mode: change a room opening (NaN keeps a value; see
    /// `openings_set`).
    pub(super) fn set_room_opening(&mut self, id: u32, width: f64, height: f64, sill: f64, depth: f64) -> Result<RoomOpening, String> {
        let current = *self.rooms.openings.iter().find(|o| o.id == id).ok_or("no such opening")?;
        let mut next = current;
        if width.is_finite() {
            next.offset_m = current.offset_m + (current.width_m - width) / 2.0;
            next.width_m = width.max(crate::authoring::openings::MIN_OPENING_SIZE_M);
        }
        if height.is_finite() {
            next.height_m = height.max(crate::authoring::openings::MIN_OPENING_SIZE_M);
        }
        if sill.is_finite() && next.kind == OpeningKind::Window {
            next.sill_m = sill.max(0.0);
        }
        if depth.is_finite() {
            next.depth_m = (depth > 0.0).then_some(depth);
        }
        self.rooms_change(|r| {
            let o = r.fit_opening(next)?;
            if let Some(slot) = r.openings.iter_mut().find(|x| x.id == id) {
                *slot = o;
            }
            Ok(o)
        })
    }

    pub(super) fn delete_room_opening(&mut self, id: u32) -> bool {
        self.rooms_change(|r| {
            r.openings.retain(|o| o.id != id);
            Ok(())
        })
        .is_ok()
    }

    /// The selected room opening for the Openings panel.
    pub(super) fn room_opening_json(&self, id: u32) -> Option<serde_json::Value> {
        let o = self.rooms.openings.iter().find(|o| o.id == id)?;
        let room = self.rooms.room(o.room)?;
        let layout = self.rooms.layout(room.plane);
        Some(serde_json::json!({
            "wall": -1.0, "roomOpening": o.id, "wallName": format!("{} wall", room.name), "id": o.id, "segment": o.edge,
            "kind": kind_name(o.kind), "offset": o.offset_m, "sill": o.sill_m, "width": o.width_m, "height": o.height_m,
            "depth": o.depth_m, "wallHeight": layout.height_m, "wallThickness": layout.thickness_m,
        }))
    }
}

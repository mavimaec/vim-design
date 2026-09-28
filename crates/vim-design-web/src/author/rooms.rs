//! Rooms: the Rooms tool, the plan overlays (region fill, name + area
//! label with the region's status, hidden walls dashed), the room page
//! (rename, order, delete), Room Edit Mode, the room walls' settings,
//! and openings in room walls — over the library's `Room` and
//! `RoomLayout` (docs/AUTHORING.md §12).
//!
//! Document shape (see `ops::create_room`): a plane's first room creates
//! its `RoomLayout` and a "Room walls" element that owns it (the wall
//! mesh), associated with the plane's root level. Rooms are NOT
//! elements: they are data of the layout, listed in the Model tree under
//! their plane's root level. Every change is library ops and one
//! command (drags and typing coalesce), so the document's undo and
//! persistence cover rooms like everything else.

use glam::Vec3;
use vim_design_lib::room::{self, RoomData, ops as room_ops};
use vim_design_lib::room_layout::{self, LayoutInput, RegionStatus, RoomOpening, WallSegment, ops as layout_ops};
use vim_design_lib::wall_run::OpeningKind;
use vim_design_lib::{Command, EntityId};
use wasm_bindgen::prelude::*;

use super::camera::ViewMode;
use super::edit::{EditProfile, EditTarget};
use super::{AuthorApp, MAX_WALL_HEIGHT_M, MAX_WALL_THICKNESS_M, MIN_WALL_HEIGHT_M, MIN_WALL_THICKNESS_M, SketchFrame, eid};
use crate::authoring::edit::interact::SelectMode;
use crate::authoring::edit::session::EditSession;
use crate::authoring::geom::{P2, dist, point_in_polygon};
use crate::authoring::model::{self, ElementModel, RoomModel, RoomWallsModel, WallLine};
use crate::authoring::ops;
use crate::authoring::rooms::{RoomProfile, along_edge, cover_at, label_point, layout_error, room_error};

/// A layout's wall graph (from `room_layout::arrange`), kept for picking
/// room walls and drawing hidden ones (derived; never authoritative).
#[derive(Debug, Clone)]
pub struct LayoutGraph {
    pub layout: EntityId,
    pub thickness: f64,
    pub segments: Vec<WallSegment>,
}

fn none() -> String {
    r#"{"result":"none"}"#.to_owned()
}

fn rejected(reason: &str) -> String {
    serde_json::json!({ "result": "rejected", "reason": reason }).to_string()
}

fn status_name(s: &RegionStatus) -> &'static str {
    match s {
        RegionStatus::Whole => "whole",
        RegionStatus::Pieces(_) => "pieces",
        RegionStatus::Empty => "empty",
        RegionStatus::Invalid(_) => "invalid",
    }
}

#[wasm_bindgen]
impl AuthorApp {
    /// Every room (the Model tree, the room page): id, name, plane,
    /// level, layout, its walls element, precedence, rank on its layout
    /// (1 = top), effective area and region status (with a short note:
    /// "in 2 pieces", "hidden behind Room 001", "invalid: ..."), edge and
    /// hidden edge counts.
    pub fn rooms_json(&self) -> String {
        let items: Vec<serde_json::Value> = self.rooms.iter().map(|r| self.room_value(r)).collect();
        serde_json::Value::Array(items).to_string()
    }

    /// The plan overlays of the active plane's rooms, in device pixels:
    /// regions (outer ring + holes), labels (name, area, status note) and
    /// the hidden walls (dashed lines). The walls themselves are the
    /// layout's mesh.
    pub fn rooms_hud_json(&self) -> String {
        let off = r#"{"active":false}"#.to_owned();
        if self.camera.mode != ViewMode::Plan {
            return off;
        }
        let Some(plane) = self.plane() else { return off };
        let Some(layout) = ops::layout_on(&self.doc, plane) else { return off };
        let frame = self.plane_frame(plane);
        let (w, h) = self.size_f();
        let proj = |p: P2| self.camera.project(frame.world(p), w, h).map(|(x, y)| [x, y]);
        let ring = |r: &[P2]| -> Vec<[f32; 2]> { r.iter().filter_map(|p| proj(*p)).collect() };
        let ranking = self.layout_input(layout).map(|i| layout_ops::ranking(&i)).unwrap_or_default();
        let mut regions = Vec::new();
        let mut labels = Vec::new();
        for region in self.engine.room_regions(layout).unwrap_or_default() {
            let Some(r) = self.room_model(region.room) else { continue };
            let rank = ranking.iter().position(|x| *x == region.room).unwrap_or(0);
            let rings: Vec<Vec<[f32; 2]>> = region.polygons.iter().flat_map(|s| s.iter().map(|r| ring(r))).collect();
            regions.push(serde_json::json!({
                "id": region.room.0 as f64, "rings": rings, "color": rank,
                "sel": self.room_selection == Some(region.room), "editing": self.room_edit == Some(region.room),
            }));
            // An empty or invalid region labels the drawn boundary.
            let at = label_point(&region.polygons).or_else(|| label_point(&[vec![r.data.polygon()]])).and_then(proj);
            if let Some([x, y]) = at {
                labels.push(serde_json::json!({
                    "id": region.room.0 as f64, "x": x, "y": y, "name": r.data.name, "area": region.area_m2,
                    "status": status_name(&region.status), "note": self.region_note(r, &region.status),
                }));
            }
        }
        let hidden: Vec<Vec<f32>> = self
            .room_graph(layout)
            .map(|g| {
                g.segments
                    .iter()
                    .filter(|s| s.hidden)
                    .filter_map(|s| Some([proj(s.a)?, proj(s.b)?].concat()))
                    .collect()
            })
            .unwrap_or_default();
        let issues: Vec<String> =
            self.engine.room_layout_issues(layout).unwrap_or_default().iter().map(|i| self.issue_text(layout, i)).collect();
        serde_json::json!({ "active": true, "regions": regions, "labels": labels, "hidden": hidden, "issues": issues }).to_string()
    }

    /// What a Select tap picks: a wall first (room walls included); in
    /// plan, a room before the floor plate under it (a plate stays
    /// selectable outside rooms and in the Model tree); in 3D the element
    /// hit, else a room. JSON `{"element": id}` or `{"room": id}`.
    pub fn pick_select(&self, px: f32, py: f32, wall_tol: f32) -> String {
        let mut id = self.pick(px, py);
        if id < 0.0 {
            id = self.pick_wall(px, py, wall_tol);
        }
        let wall = id >= 0.0 && self.model.iter().any(|e| e.element() == eid(id) && e.is_wall());
        let room = if !wall && (id < 0.0 || self.camera.mode == ViewMode::Plan) { self.room_at(px, py) } else { -1.0 };
        if room >= 0.0 {
            serde_json::json!({ "room": room }).to_string()
        } else {
            serde_json::json!({ "element": id }).to_string()
        }
    }

    /// Select a room (clears the element selection); -1 clears.
    pub fn room_select(&mut self, id: f64) {
        let id = (id >= 0.0).then(|| eid(id)).filter(|i| self.room_model(*i).is_some());
        self.room_selection = id;
        if id.is_some() {
            self.selection = None;
        }
        self.refresh_styles();
    }

    pub fn room_selected(&self) -> f64 {
        self.room_selection.map_or(-1.0, |r| r.0 as f64)
    }

    /// The room whose region is under a canvas point on the active plane,
    /// or -1.
    pub fn room_at(&self, px: f32, py: f32) -> f64 {
        let Some(plane) = self.plane() else { return -1.0 };
        let Some(layout) = ops::layout_on(&self.doc, plane) else { return -1.0 };
        let Some(uv) = self.plane_uv_at(plane, px, py) else { return -1.0 };
        for region in self.engine.room_regions(layout).unwrap_or_default() {
            for shape in &region.polygons {
                let Some(outer) = shape.first() else { continue };
                if point_in_polygon(uv, outer) && !shape[1..].iter().any(|hole| point_in_polygon(uv, hole)) {
                    return region.room.0 as f64;
                }
            }
        }
        -1.0
    }

    /// Rename a room (one undo step; typing coalesces until
    /// `end_gesture`). False when the name is empty or unchanged.
    pub fn room_rename(&mut self, id: f64, name: &str) -> bool {
        let name = name.trim().to_owned();
        let id = eid(id);
        if name.is_empty() || self.room_model(id).is_none_or(|r| r.data.name == name) {
            return false;
        }
        self.submit_room(id, RoomUpdate { name: Some(name), ..RoomUpdate::default() }, Some(&format!("room_name_{}", id.0)))
    }

    /// Bring a room forward (it cuts into the room it passes) or send it
    /// backward: exactly one place in its layout's order (the library's
    /// ranking). The rooms get distinct precedences in the new order
    /// (`UpdateRoom`s only where one changes): one undo step. False at the
    /// end.
    pub fn room_restack(&mut self, id: f64, up: bool) -> bool {
        let id = eid(id);
        let Some(input) = self.room_model(id).and_then(|r| r.layout).and_then(|l| self.layout_input(l)) else { return false };
        let mut order = layout_ops::ranking(&input);
        let Some(i) = order.iter().position(|r| *r == id) else { return false };
        let j = if up { i.checked_sub(1) } else { Some(i + 1).filter(|j| *j < order.len()) };
        let Some(j) = j else { return false };
        order.swap(i, j);
        // Highest first: n-1 .. 0, keeping the layout's lowest precedence.
        let base = input.rooms.iter().map(|(_, r)| r.precedence).min().unwrap_or(0);
        let n = order.len() as i32;
        let updates: Vec<(EntityId, RoomUpdate)> = order
            .iter()
            .enumerate()
            .filter_map(|(k, rid)| {
                let p = base + n - 1 - k as i32;
                let now = input.room(*rid)?.precedence;
                (now != p).then(|| (*rid, RoomUpdate { precedence: Some(p), ..RoomUpdate::default() }))
            })
            .collect();
        !updates.is_empty() && self.submit_rooms(updates, None)
    }

    /// Delete a room (its openings go; the plane's last room takes the
    /// room walls with it). One undo step.
    pub fn room_delete(&mut self, id: f64) -> bool {
        let id = eid(id);
        if self.room_model(id).is_none() {
            return false;
        }
        let depth = self.doc.undo_depth();
        match ops::delete_room(&mut self.doc, id) {
            Ok(()) => {
                self.gestures.one_shot(depth);
                if self.room_selection == Some(id) {
                    self.room_selection = None;
                }
                self.sync("delete room");
                true
            }
            Err(e) => {
                ops::rollback_to(&mut self.doc, depth);
                self.gestures.invalidate_redo();
                self.sync("delete room (failed)");
                web_sys::console::error_1(&JsValue::from_str(&e));
                false
            }
        }
    }

    /// A room layout's wall settings: its element, thickness, height mode
    /// ("fixed" / "upto"), top plane, offset, fixed height, the resulting
    /// height, rooms and openings. `layout` < 0: the active plane's.
    pub fn room_layout_json(&self, layout: f64) -> String {
        let Some(w) = self.layout_walls(layout) else { return "null".to_owned() };
        self.room_walls_value(w).to_string()
    }

    /// Change a layout's wall settings (NaN keeps a value; `mode`
    /// "fixed" / "upto" / "" keeps it). One undo step; typing coalesces
    /// until `end_gesture`. The thickness and height are remembered for
    /// the next plane's rooms. Returns "" or a refusal reason.
    pub fn set_room_layout(&mut self, layout: f64, thickness: f64, mode: &str, plane: f64, offset: f64, height: f64) -> String {
        let Some(w) = self.layout_walls(layout).cloned() else { return "No room walls here".to_owned() };
        let mut thickness_m = None;
        let mut height_m = None;
        let mut top = None;
        let mut top_offset_m = None;
        if thickness.is_finite() {
            let t = thickness.clamp(MIN_WALL_THICKNESS_M, MAX_WALL_THICKNESS_M);
            thickness_m = Some(t);
            self.remembered.room_wall_thickness = t;
        }
        match mode {
            "fixed" => {
                if w.top.is_some() {
                    top = Some(None);
                    height_m = Some(w.top_height.clamp(MIN_WALL_HEIGHT_M, MAX_WALL_HEIGHT_M)); // keep the height it has
                }
            }
            "upto" => {
                let p = eid(plane);
                if plane < 0.0 || self.root_level(p).is_none() {
                    return "Pick the plane the walls go up to".to_owned();
                }
                let off = if offset.is_finite() { offset } else { w.data.top_offset_m };
                if self.plane_elevation(p) + off - self.plane_elevation(w.plane) < MIN_WALL_HEIGHT_M {
                    return "The wall top must be above its base: pick a higher plane".to_owned();
                }
                top = Some(Some(p));
            }
            _ => {}
        }
        if offset.is_finite() {
            top_offset_m = Some(offset.clamp(-MAX_WALL_HEIGHT_M, MAX_WALL_HEIGHT_M));
        }
        if height.is_finite() {
            let h = height.clamp(MIN_WALL_HEIGHT_M, MAX_WALL_HEIGHT_M);
            height_m = Some(h);
            self.remembered.room_wall_height = h;
            if w.top.is_some() {
                top = Some(None); // a height is a fixed height
            }
        }
        let coalesce = self.gestures.begin_continuing(&self.doc, &format!("room_layout_{}", w.layout.0));
        let cmd = Command::UpdateRoomLayout {
            id: w.layout,
            plane: None,
            top,
            rooms: None,
            thickness_m,
            height_m,
            top_offset_m,
            openings: None,
            coalesce,
        };
        match self.doc.submit(cmd) {
            Ok(_) => {
                self.sync("room walls");
                String::new()
            }
            Err(st) => {
                self.sync("room walls (refused)");
                format!("The room walls refused this ({st:?})")
            }
        }
    }

    /// Enter Room Edit Mode on a room: its points and edges, in plan on
    /// its plane (a gestures session: ✓ keeps the steps, ✗ undoes them).
    pub fn room_edit_begin(&mut self, id: f64) -> bool {
        let id = eid(id);
        if self.edit.is_some() || self.openings.is_some() {
            return false;
        }
        let Some(r) = self.room_model(id).cloned() else { return false };
        self.gestures.begin_session(&self.doc);
        if self.prev_camera.is_none() {
            self.prev_camera = Some(self.camera.clone());
        }
        self.set_active_plane(r.plane.0 as f64);
        self.camera.set_mode(ViewMode::Plan);
        self.edit_target = EditTarget::Room;
        let session = EditSession::new(EditProfile::Room(RoomProfile { data: r.data.clone() }), r.plane, None, r.data.name.clone());
        self.edit_sketch = None;
        self.edit_entry_element = None;
        self.start_edit(session);
        self.room_edit = Some(id);
        self.room_selection = Some(id);
        if let Some(s) = self.edit.as_mut() {
            s.set_mode(SelectMode::Edges);
        }
        true
    }

    /// Room Edit Mode: hide (no wall) or show the walls of the selected
    /// edges. One undo step.
    pub fn room_set_hidden(&mut self, hidden: bool) -> String {
        let Some(id) = self.room_edit else { return none() };
        let Some(s) = self.edit.as_ref() else { return none() };
        let EditProfile::Room(p) = &s.model else { return none() };
        let edges = p.selected_edges(s.selection.edges.iter().copied());
        if edges.is_empty() {
            return rejected("Select the edges whose wall to hide");
        }
        let data = match room_ops::set_hidden(&p.data, &edges, hidden) {
            Ok(d) => d,
            Err(e) => return rejected(room_error(e).message()),
        };
        let ok = self.submit_room(id, RoomUpdate { hidden_edges: Some(data.hidden_edges), ..RoomUpdate::default() }, None);
        self.reload_room_edit();
        if ok { r#"{"result":"changed"}"#.to_owned() } else { rejected("The room refused the change") }
    }
}

/// The optional fields of an `UpdateRoom`.
#[derive(Debug, Clone, Default)]
pub(super) struct RoomUpdate {
    pub name: Option<String>,
    pub precedence: Option<i32>,
    pub boundary: Option<Vec<vim_design_lib::wall_run::RunPoint>>,
    pub hidden_edges: Option<Vec<u32>>,
}

impl AuthorApp {
    /// Rooms and wall graphs from the document (after every change).
    pub(super) fn derive_rooms(&mut self) {
        self.rooms = model::rooms(&self.doc);
        self.room_graphs = self
            .model
            .iter()
            .filter_map(|e| match e {
                ElementModel::RoomWalls(w) => Some(w),
                _ => None,
            })
            .filter_map(|w| {
                let input = room_layout::inputs(&self.doc, w.layout)?;
                let arr = room_layout::arrange(&input).ok()?;
                Some(LayoutGraph { layout: w.layout, thickness: w.data.thickness_m, segments: arr.segments })
            })
            .collect();
        if self.room_selection.is_some_and(|r| self.room_model(r).is_none()) {
            self.room_selection = None;
        }
    }

    pub(super) fn room_model(&self, id: EntityId) -> Option<&RoomModel> {
        self.rooms.iter().find(|r| r.room == id)
    }

    fn room_graph(&self, layout: EntityId) -> Option<&LayoutGraph> {
        self.room_graphs.iter().find(|g| g.layout == layout)
    }

    pub(super) fn layout_input(&self, layout: EntityId) -> Option<LayoutInput> {
        room_layout::inputs(&self.doc, layout)
    }

    /// The room walls element of a layout id, or of the active plane
    /// (`layout` < 0).
    pub(super) fn layout_walls(&self, layout: f64) -> Option<&RoomWallsModel> {
        let layout = if layout >= 0.0 { eid(layout) } else { ops::layout_on(&self.doc, self.plane()?)? };
        self.model.iter().find_map(|e| match e {
            ElementModel::RoomWalls(w) if w.layout == layout => Some(w),
            _ => None,
        })
    }

    /// The room walls element by its element id.
    pub(super) fn room_walls(&self, element: EntityId) -> Option<&RoomWallsModel> {
        self.model.iter().find_map(|e| match e {
            ElementModel::RoomWalls(w) if w.element == element => Some(w),
            _ => None,
        })
    }

    pub(super) fn room_walls_value(&self, w: &RoomWallsModel) -> serde_json::Value {
        let base = self.plane_elevation(w.plane);
        serde_json::json!({
            "element": w.element.0 as f64, "layout": w.layout.0 as f64, "name": w.name, "plane": w.plane.0 as f64,
            "thickness": w.data.thickness_m, "mode": if w.top.is_some() { "upto" } else { "fixed" },
            "topPlane": w.top.map(|p| p.0 as f64), "topOffset": w.data.top_offset_m, "height": w.data.height_m,
            "effectiveHeight": w.top_height, "rooms": w.rooms.len(), "openings": w.data.openings.len(),
            "baseElevation": base,
            "issues": self.engine.room_layout_issues(w.layout).unwrap_or_default().iter().map(|i| self.issue_text(w.layout, i)).collect::<Vec<_>>(),
        })
    }

    fn room_value(&self, r: &RoomModel) -> serde_json::Value {
        let input = r.layout.and_then(|l| self.layout_input(l));
        let ranking = input.as_ref().map(layout_ops::ranking).unwrap_or_default();
        let rank = ranking.iter().position(|x| *x == r.room).map_or(0, |i| i + 1);
        let region = r.layout.and_then(|l| self.engine.room_regions(l)).and_then(|rs| rs.iter().find(|x| x.room == r.room));
        let walls = r.layout.and_then(|l| self.layout_walls(l.0 as f64));
        serde_json::json!({
            "id": r.room.0 as f64, "name": r.data.name, "plane": r.plane.0 as f64,
            "level": r.level.map(|l| l.0 as f64), "layout": r.layout.map(|l| l.0 as f64),
            "walls": walls.map(|w| w.element.0 as f64),
            "precedence": r.data.precedence, "rank": rank, "ofRank": ranking.len(),
            "area": region.map_or(0.0, |x| x.area_m2),
            "status": region.map_or("invalid", |x| status_name(&x.status)),
            "note": region.map_or(String::new(), |x| self.region_note(r, &x.status)),
            "edges": r.data.boundary.len(), "hiddenEdges": r.data.hidden_edges.len(),
            "boundary": r.data.polygon(),
            "selected": self.room_selection == Some(r.room),
        })
    }

    /// A short note on a region: "in 2 pieces", "hidden behind Room 001"
    /// (the room ranked above it that covers it most), "invalid: ...".
    fn region_note(&self, r: &RoomModel, status: &RegionStatus) -> String {
        match status {
            RegionStatus::Whole => String::new(),
            RegionStatus::Pieces(n) => format!("in {n} pieces"),
            RegionStatus::Invalid(e) => format!("invalid: {e}"),
            RegionStatus::Empty => {
                let input = r.layout.and_then(|l| self.layout_input(l));
                let ranking = input.as_ref().map(layout_ops::ranking).unwrap_or_default();
                let mine = r.data.polygon();
                let above = ranking
                    .iter()
                    .take_while(|id| **id != r.room)
                    .filter_map(|id| self.room_model(*id))
                    .find(|o| mine.iter().all(|p| point_in_polygon(*p, &o.data.polygon()) || on_outline(*p, &o.data.polygon())));
                match above {
                    Some(o) => format!("hidden behind {}", o.data.name),
                    None => "hidden behind the rooms above it".to_owned(),
                }
            }
        }
    }

    /// A layout issue as page text (room and opening named).
    fn issue_text(&self, _layout: EntityId, i: &room_layout::LayoutIssue) -> String {
        match i {
            room_layout::LayoutIssue::RoomInvalid { room, error } => {
                let name = self.room_model(*room).map_or_else(|| "A room".to_owned(), |r| r.data.name.clone());
                format!("{name} is invalid ({error}) and makes no walls")
            }
            room_layout::LayoutIssue::OpeningDoesNotFit { .. } => "An opening no longer fits its wall and is left out".to_owned(),
        }
    }

    /// The Model tree rows of a level's rooms (rank order per layout).
    pub(super) fn room_tree_rows(&self, level: EntityId) -> Vec<serde_json::Value> {
        let mut rows: Vec<(i64, usize, serde_json::Value)> = self
            .rooms
            .iter()
            .filter(|r| r.level == Some(level))
            .map(|r| {
                let v = self.room_value(r);
                let rank = v["rank"].as_u64().unwrap_or(0) as usize;
                let area = v["area"].as_f64().unwrap_or(0.0);
                let note = v["note"].as_str().unwrap_or("").to_owned();
                let meta = if note.is_empty() { format!("{area:.1} m²") } else { format!("{area:.1} m² · {note}") };
                let row = serde_json::json!({
                    "id": r.room.0 as f64, "name": r.data.name, "kind": "room", "meta": meta, "editable": true,
                    "selected": v["selected"], "rank": v["rank"], "ofRank": v["ofRank"], "status": v["status"],
                });
                (r.layout.map_or(0, |l| l.0 as i64), rank, row)
            })
            .collect();
        rows.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
        rows.into_iter().map(|(_, _, v)| v).collect()
    }

    /// The united plan footprint area of a layout's walls (m²).
    pub(super) fn layout_footprint_area(&self, layout: EntityId) -> f64 {
        let Some(input) = self.layout_input(layout) else { return 0.0 };
        let Ok(fp) = room_layout::arrange(&input).and_then(|a| a.footprint()) else { return 0.0 };
        fp.iter().flat_map(|s| s.iter()).map(|ring| crate::authoring::geom::signed_area(ring)).sum::<f64>().abs()
    }

    /// A layout's openings for the Properties list.
    pub(super) fn room_openings_json(&self, w: &RoomWallsModel) -> Vec<serde_json::Value> {
        w.data
            .openings
            .iter()
            .map(|o| {
                serde_json::json!({
                    "id": o.id, "room": o.room.0 as f64, "roomName": self.room_model(o.room).map(|r| r.data.name.clone()),
                    "edge": o.edge, "kind": if o.kind == OpeningKind::Door { "door" } else { "window" },
                    "offset": o.offset_m, "sill": o.sill_m, "width": o.width_m, "height": o.height_m, "depth": o.depth_m,
                })
            })
            .collect()
    }

    /// Room Edit Mode: show the stored room again (after a change, undo,
    /// or redo).
    pub(super) fn reload_room_edit(&mut self) {
        let Some(r) = self.room_edit.and_then(|id| self.room_model(id)).cloned() else { return };
        if let Some(s) = self.edit.as_mut() {
            s.name = r.data.name.clone();
            s.set_model(EditProfile::Room(RoomProfile { data: r.data }));
        }
    }

    /// One `UpdateRoom` (coalesced under `key` until the gesture ends),
    /// one undo step.
    pub(super) fn submit_room(&mut self, id: EntityId, u: RoomUpdate, key: Option<&str>) -> bool {
        self.submit_rooms(vec![(id, u)], key)
    }

    /// Several `UpdateRoom`s as ONE undo step (or part of the open
    /// gesture `key`); all or nothing.
    pub(super) fn submit_rooms(&mut self, updates: Vec<(EntityId, RoomUpdate)>, key: Option<&str>) -> bool {
        let depth = self.doc.undo_depth();
        let coalesce = key.is_some_and(|k| self.gestures.begin_continuing(&self.doc, k));
        for (id, u) in updates {
            let cmd = Command::UpdateRoom {
                id,
                plane: None,
                name: u.name,
                precedence: u.precedence,
                boundary: u.boundary,
                hidden_edges: u.hidden_edges,
                coalesce,
            };
            if let Err(st) = self.doc.submit(cmd) {
                web_sys::console::error_1(&JsValue::from_str(&format!("UpdateRoom refused: {st:?}")));
                ops::rollback_to(&mut self.doc, depth);
                self.sync("room (refused)");
                return false;
            }
        }
        if key.is_none() {
            self.gestures.one_shot(depth);
        }
        self.sync("room");
        true
    }

    /// Room Edit Mode: store an accepted boundary edit (one step, or part
    /// of the open gesture `key`). A corner shared with another room of
    /// the layout (the same position) moves with it, so the rooms stay
    /// joined and their shared wall follows.
    pub(super) fn store_room_edit(&mut self, p: &RoomProfile, key: Option<&str>, ok: &str) -> String {
        let Some(id) = self.room_edit else { return none() };
        let Some(before) = self.room_model(id).cloned() else { return none() };
        let moved: Vec<(P2, P2)> = before
            .data
            .boundary
            .iter()
            .filter_map(|old| {
                let new = p.data.boundary.iter().find(|q| q.id == old.id)?;
                (dist(old.uv, new.uv) > 1e-9).then_some((old.uv, new.uv))
            })
            .collect();
        let mut updates = vec![(
            id,
            RoomUpdate { boundary: Some(p.data.boundary.clone()), hidden_edges: Some(p.data.hidden_edges.clone()), ..RoomUpdate::default() },
        )];
        if !moved.is_empty() {
            for other in self.rooms.iter().filter(|r| r.room != id && r.layout.is_some() && r.layout == before.layout) {
                let mut data = other.data.clone();
                let mut changed = false;
                for q in &mut data.boundary {
                    if let Some((_, to)) = moved.iter().find(|(from, _)| dist(*from, q.uv) <= 1e-6) {
                        q.uv = *to;
                        changed = true;
                    }
                }
                // A neighbour the move would break keeps its shape.
                if changed && room::validate(&data).is_ok() {
                    updates.push((other.room, RoomUpdate { boundary: Some(data.boundary), ..RoomUpdate::default() }));
                }
            }
        }
        let key = key.map(|k| format!("edit_{k}"));
        let done = self.submit_rooms(updates, key.as_deref());
        self.reload_room_edit();
        if done { serde_json::json!({ "result": ok }).to_string() } else { rejected("The room refused the change") }
    }

    /// Room Edit Mode state for the page: the room, and the selected
    /// edges' walls (hidden or not) for the "Hidden wall" toggle.
    pub(super) fn room_edit_state_json(&self) -> serde_json::Value {
        let Some(id) = self.room_edit else { return serde_json::Value::Null };
        let (Some(r), Some(s)) = (self.room_model(id), self.edit.as_ref()) else { return serde_json::Value::Null };
        let EditProfile::Room(p) = &s.model else { return serde_json::Value::Null };
        let selected = p.selected_edges(s.selection.edges.iter().copied());
        let mut v = self.room_value(r);
        v["selectedEdges"] = selected.len().into();
        v["selectedHidden"] = (!selected.is_empty() && selected.iter().all(|e| p.data.is_hidden(*e))).into();
        v["hidden"] = p.data.hidden_edges.len().into();
        v
    }

    /// The bounds of the room in Room Edit Mode, or of the selected room
    /// (on its plane, up to its walls' top): what Fit frames.
    pub(super) fn room_focus_bbox(&self) -> Option<([f64; 3], [f64; 3])> {
        let r = self.room_model(self.room_edit.or(self.room_selection)?)?;
        let z = self.plane_elevation(r.plane);
        // Up to the top of its walls (a room seen in 3D is its walls).
        let top = r.layout.and_then(|l| room_layout::layout_top_height(&self.doc, l)).unwrap_or(0.0);
        let (mut lo, mut hi) = ([f64::INFINITY, f64::INFINITY, z], [f64::NEG_INFINITY, f64::NEG_INFINITY, z + top]);
        for p in r.data.polygon() {
            for k in 0..2 {
                lo[k] = lo[k].min(p[k]);
                hi[k] = hi[k].max(p[k]);
            }
        }
        lo[0].is_finite().then_some((lo, hi))
    }

    /// The frame of a construction plane (world = origin + u·x + v·y).
    pub(super) fn plane_frame(&self, plane: EntityId) -> SketchFrame {
        SketchFrame { origin: Vec3::new(0.0, 0.0, self.plane_elevation(plane) as f32), u: Vec3::X, v: Vec3::Y }
    }

    /// Where a canvas point is on a plane (plane coordinates).
    pub(super) fn plane_uv_at(&self, plane: EntityId, px: f32, py: f32) -> Option<P2> {
        let (w, h) = self.size_f();
        let z = self.plane_elevation(plane) as f32;
        let hit = self.camera.unproject_to_plane(px, py, w, h, z)?;
        Some([f64::from(hit.x), f64::from(hit.y)])
    }

    /// Meters per device pixel on a plane around a plan point.
    pub(super) fn plane_m_per_px(&self, plane: EntityId, uv: P2) -> f64 {
        let frame = self.plane_frame(plane);
        1.0 / f64::from(self.px_per_m_at(frame.world(uv), &frame))
    }

    /// A new room from a finished sketch (one gesture): on top of the
    /// plane's rooms, named "Room NNN"; selected.
    pub(super) fn commit_room(&mut self, plane: EntityId, outline: &[P2]) -> Result<(EntityId, String), String> {
        let name = room::default_name(&self.doc);
        let top = self.rooms.iter().filter(|r| r.plane == plane).map(|r| r.data.precedence).max().map_or(0, |p| p + 1);
        let data = room::from_polygon(&name, top, outline).map_err(|e| room_error(e).message().to_owned())?;
        let id = self.create_room_gesture(plane, &data)?;
        self.room_selection = Some(id);
        self.selection = None;
        Ok((id, name))
    }

    /// Create a room on `plane` (the plane's first room also makes the
    /// layout and its "Room walls" element): one undo step.
    pub(super) fn create_room_gesture(&mut self, plane: EntityId, data: &RoomData) -> Result<EntityId, String> {
        let level = self.root_level(plane).unwrap_or(plane);
        let settings = ops::LayoutSettings {
            thickness_m: self.remembered.room_wall_thickness,
            height_m: self.remembered.room_wall_height,
            top: None,
            top_offset_m: 0.0,
        };
        let depth = self.doc.undo_depth();
        match ops::create_room(&mut self.doc, plane, level, data, settings) {
            Ok(id) => {
                self.gestures.one_shot(depth);
                self.sync("room");
                Ok(id)
            }
            Err(e) => {
                ops::rollback_to(&mut self.doc, depth);
                self.gestures.invalidate_redo();
                self.sync("room (failed)");
                Err(e)
            }
        }
    }

    // -- Room wall openings -----------------------------------------------

    /// A room edge as a wall line: centered on the edge, the reference
    /// face on the side away from the room (the normal points into the
    /// room), `element` the room walls element.
    pub(super) fn room_edge_line(&self, w: &RoomWallsModel, room: &RoomData, edge: u32) -> Option<WallLine> {
        let (a, b) = room.edge_ends(edge)?;
        let len = dist(a, b).max(1e-12);
        let d = [(b[0] - a[0]) / len, (b[1] - a[1]) / len];
        let left = [-d[1], d[0]];
        let h = w.data.thickness_m / 2.0;
        Some(WallLine {
            element: w.element,
            segment: Some(edge),
            plane: w.plane,
            start: [a[0] - left[0] * h, a[1] - left[1] * h],
            end: [b[0] - left[0] * h, b[1] - left[1] * h],
            base_w: 0.0,
            height: w.top_height,
            thickness: w.data.thickness_m,
            normal: left,
        })
    }

    /// Every room wall opening as the Openings mode shows it: its wall
    /// line, visible rectangle (along the edge; a door from the base),
    /// kind, niche, id (keyed with the room walls element).
    pub(super) fn room_openings_shown(&self) -> Vec<super::openings::ShownOpening> {
        let mut out = Vec::new();
        for e in &self.model {
            let ElementModel::RoomWalls(w) = e else { continue };
            for o in &w.data.openings {
                let Some(room) = self.room_model(o.room) else { continue };
                let Some(line) = self.room_edge_line(w, &room.data, o.edge) else { continue };
                let v0 = if o.kind == OpeningKind::Door { 0.0 } else { o.sill_m };
                out.push((line, [[o.offset_m, v0], [o.offset_m + o.width_m, v0 + o.height_m]], o.kind, o.depth_m.is_some(), Some(o.id)));
            }
        }
        out
    }

    /// The room walls and the room edge under a canvas point: in plan on
    /// the active plane; in 3D where the pointer ray meets a room walls
    /// mesh. (walls element, room, edge, along the edge).
    pub(super) fn room_wall_at(&self, px: f32, py: f32, tol_px: f32) -> Option<(EntityId, EntityId, u32, f64)> {
        let (vw, vh) = self.size_f();
        let (walls, uv) = if self.camera.mode == ViewMode::Plan {
            let plane = self.plane()?;
            let w = self.layout_walls(ops::layout_on(&self.doc, plane)?.0 as f64)?;
            (w, self.plane_uv_at(plane, px, py)?)
        } else {
            let (origin, dir) = self.camera.ray(px, py, vw, vh)?;
            let (element, d) = self.pick.pick_all(origin, dir, &|p| self.pickable_z(p.z)).into_iter().find(|(id, _)| self.room_walls(*id).is_some())?;
            let hit = origin + dir * d;
            (self.room_walls(element)?, [f64::from(hit.x), f64::from(hit.y)])
        };
        let graph = self.room_graph(walls.layout)?;
        let tol = f64::from(tol_px) * self.plane_m_per_px(walls.plane, uv) + graph.thickness / 2.0;
        let (room, edge, at) = cover_at(&graph.segments, uv, tol)?;
        let along = along_edge(&self.room_model(room)?.data, edge, at)?;
        Some((walls.element, room, edge, along))
    }

    /// Room wall openings laid out around `along` on a room edge, each
    /// snapped and moved into the nearest span that holds it, then added
    /// (a refusal names why). Returns the new layout data and ids.
    pub(super) fn room_openings_added(
        &self,
        walls: EntityId,
        room: EntityId,
        edge: u32,
        along: f64,
        copies: &[vim_design_lib::wall_run::Opening],
    ) -> Result<(EntityId, room_layout::RoomLayoutData, Vec<u32>), String> {
        let w = self.room_walls(walls).ok_or("No room walls here")?;
        let mut input = self.layout_input(w.layout).ok_or("No room walls here")?;
        let spans = room_layout::opening_span(&input, room, edge).map_err(layout_error)?;
        let mut ids = Vec::new();
        for o in crate::authoring::clipboard::laid_out(copies, 0, along) {
            let width = snap(o.width_m.max(crate::authoring::openings::MIN_OPENING_SIZE_M));
            let centre = o.offset_m + width / 2.0;
            let span = spans
                .iter()
                .filter(|(lo, hi)| hi - lo >= width - 1e-9)
                .min_by(|a, b| (centre - centre.clamp(a.0, a.1)).abs().total_cmp(&(centre - centre.clamp(b.0, b.1)).abs()))
                .ok_or("This wall is too short for the opening (between its junctions)")?;
            let offset = snap(o.offset_m).clamp(span.0, span.1 - width);
            let top = w.top_height - crate::authoring::walls::WINDOW_MARGIN_M;
            let height = snap(o.height_m.max(crate::authoring::openings::MIN_OPENING_SIZE_M));
            let sill = if o.kind == OpeningKind::Door { 0.0 } else { snap(o.sill_m).max(0.0).min((top - height).max(0.0)) };
            if sill + height > top + 1e-9 {
                return Err("The wall is not tall enough for this opening".to_owned());
            }
            let ro = RoomOpening {
                id: 0,
                room,
                edge,
                offset_m: offset,
                sill_m: sill,
                width_m: width,
                height_m: height,
                kind: o.kind,
                depth_m: o.depth_m,
            };
            let (data, id) = layout_ops::add_opening(&input, ro).map_err(layout_error)?;
            input.layout = data;
            ids.push(id);
        }
        Ok((w.layout, input.layout, ids))
    }

    /// Openings mode: place the preset (its remembered size) on a room
    /// edge hit, centred at the tap: one step. Returns the (walls element,
    /// opening id).
    pub(super) fn place_room_opening(&mut self, kind: OpeningKind, hit: (EntityId, EntityId, u32, f64)) -> Result<(EntityId, u32), String> {
        let (walls, room, edge, along) = hit;
        let size = self.remembered.opening(kind);
        let copy = vim_design_lib::wall_run::Opening {
            id: 0,
            segment: 0,
            offset_m: along - size.width / 2.0,
            sill_m: size.sill,
            width_m: size.width,
            height_m: size.height,
            kind,
            depth_m: size.depth,
        };
        let (layout, data, ids) = self.room_openings_added(walls, room, edge, along, &[copy])?;
        if !self.store_layout_openings(layout, data.openings, None) {
            return Err("The room walls refused the opening".to_owned());
        }
        ids.first().map(|id| (walls, *id)).ok_or_else(|| "No opening was placed".to_owned())
    }

    /// Store a layout's openings: one `UpdateRoomLayout` (or part of the
    /// open gesture `key`).
    pub(super) fn store_layout_openings(&mut self, layout: EntityId, openings: Vec<RoomOpening>, key: Option<&str>) -> bool {
        let depth = self.doc.undo_depth();
        let coalesce = key.is_some_and(|k| self.gestures.begin_continuing(&self.doc, k));
        match self.doc.submit(ops::update_layout_openings(layout, openings, coalesce)) {
            Ok(_) => {
                if key.is_none() {
                    self.gestures.one_shot(depth);
                }
                self.sync("room opening");
                true
            }
            Err(st) => {
                self.sync("room opening (refused)");
                web_sys::console::error_1(&JsValue::from_str(&format!("UpdateRoomLayout refused: {st:?}")));
                false
            }
        }
    }

    /// A room wall opening by walls element and id.
    pub(super) fn room_opening(&self, walls: EntityId, id: u32) -> Option<(RoomWallsModel, RoomOpening)> {
        let w = self.room_walls(walls)?.clone();
        let o = *w.data.openings.iter().find(|o| o.id == id)?;
        Some((w, o))
    }

    /// Change a room wall opening: `[width, height, sill, depth]` as in
    /// `openings_set` (NaN keeps a value; a size keeps its centre; snapped
    /// and kept in its span).
    pub(super) fn set_room_opening(&mut self, walls: EntityId, id: u32, values: [f64; 4], key: &str) -> Result<RoomOpening, String> {
        let [width, height, sill, depth] = values;
        let (w, cur) = self.room_opening(walls, id).ok_or("No such opening")?;
        let input = self.layout_input(w.layout).ok_or("No room walls here")?;
        let mut next = cur;
        if width.is_finite() {
            let width = snap(width.max(crate::authoring::openings::MIN_OPENING_SIZE_M));
            next.offset_m = cur.offset_m + (cur.width_m - width) / 2.0;
            next.width_m = width;
        }
        if height.is_finite() {
            next.height_m = snap(height.max(crate::authoring::openings::MIN_OPENING_SIZE_M));
        }
        if sill.is_finite() && next.kind == OpeningKind::Window {
            next.sill_m = snap(sill).max(0.0);
        }
        if depth.is_finite() {
            next.depth_m = (depth > 0.0).then_some(depth);
        }
        // Keep it inside the span it is in.
        let spans = room_layout::opening_span(&input, cur.room, cur.edge).map_err(layout_error)?;
        let centre = next.offset_m + next.width_m / 2.0;
        if let Some(s) = spans.iter().find(|(lo, hi)| centre >= *lo - 1e-9 && centre <= *hi + 1e-9) {
            if next.width_m > s.1 - s.0 + 1e-9 {
                return Err("This wall is too short for the opening (between its junctions)".to_owned());
            }
            next.offset_m = next.offset_m.clamp(s.0, s.1 - next.width_m);
        }
        let top = w.top_height - crate::authoring::walls::WINDOW_MARGIN_M;
        if next.kind == OpeningKind::Window && next.sill_m + next.height_m > top {
            next.sill_m = (top - next.height_m).max(0.0);
        }
        if (if next.kind == OpeningKind::Door { 0.0 } else { next.sill_m }) + next.height_m > top + 1e-9 {
            return Err("The wall is not tall enough for this opening".to_owned());
        }
        let data = layout_ops::set_opening(&input, next).map_err(layout_error)?;
        if self.store_layout_openings(w.layout, data.openings, Some(key)) { Ok(next) } else { Err("The room walls refused the opening".to_owned()) }
    }

    /// Move a room wall opening along its edge to `offset` (clamped into
    /// the nearest span by `move_opening`); coalesced under `key`.
    pub(super) fn move_room_opening(&mut self, walls: EntityId, id: u32, offset: f64, key: &str) -> Result<bool, String> {
        let (w, cur) = self.room_opening(walls, id).ok_or("No such opening")?;
        let input = self.layout_input(w.layout).ok_or("No room walls here")?;
        let delta = snap(offset) - cur.offset_m;
        if delta.abs() < 1e-9 {
            return Ok(false);
        }
        let data = layout_ops::move_opening(&input, id, [delta, 0.0]).map_err(layout_error)?;
        if data == input.layout {
            return Ok(false);
        }
        Ok(self.store_layout_openings(w.layout, data.openings, Some(key)))
    }

    pub(super) fn delete_room_opening(&mut self, walls: EntityId, id: u32) -> bool {
        let Some((w, _)) = self.room_opening(walls, id) else { return false };
        let Some(input) = self.layout_input(w.layout) else { return false };
        match layout_ops::delete_opening(&input, id) {
            Ok(data) => self.store_layout_openings(w.layout, data.openings, None),
            Err(_) => false,
        }
    }

    /// The selected room wall opening for the Openings panel.
    pub(super) fn room_opening_json(&self, walls: EntityId, id: u32) -> Option<serde_json::Value> {
        let (w, o) = self.room_opening(walls, id)?;
        let room = self.room_model(o.room)?;
        let input = self.layout_input(w.layout)?;
        let span = room_layout::opening_span(&input, o.room, o.edge).ok().and_then(|s| {
            let c = o.offset_m + o.width_m / 2.0;
            s.into_iter().find(|(lo, hi)| c >= *lo - 1e-9 && c <= *hi + 1e-9)
        });
        Some(serde_json::json!({
            "wall": w.element.0 as f64, "roomWalls": true, "room": o.room.0 as f64, "wallName": format!("{} wall", room.data.name),
            "id": o.id, "segment": o.edge, "kind": if o.kind == OpeningKind::Door { "door" } else { "window" },
            "offset": o.offset_m, "sill": o.sill_m, "width": o.width_m, "height": o.height_m, "depth": o.depth_m,
            "wallHeight": w.top_height, "wallThickness": w.data.thickness_m, "span": span.map(|(a, b)| [a, b]),
        }))
    }

    /// Room wall openings as run openings (for the clipboard).
    pub(super) fn room_opening_as_run(o: &RoomOpening) -> vim_design_lib::wall_run::Opening {
        vim_design_lib::wall_run::Opening {
            id: o.id,
            segment: 0,
            offset_m: o.offset_m,
            sill_m: o.sill_m,
            width_m: o.width_m,
            height_m: o.height_m,
            kind: o.kind,
            depth_m: o.depth_m,
        }
    }
}

/// Snap to the openings grid (0.1 m).
fn snap(v: f64) -> f64 {
    (v * crate::authoring::openings::OPENING_SNAP_PER_M).round() / crate::authoring::openings::OPENING_SNAP_PER_M
}

fn on_outline(p: P2, poly: &[P2]) -> bool {
    let n = poly.len();
    (0..n).any(|i| crate::authoring::geom::point_segment_distance(p, poly[i], poly[(i + 1) % n]) <= 1e-6)
}

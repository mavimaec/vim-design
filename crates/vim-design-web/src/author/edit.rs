//! Edit Mode of the authoring app: a modal session editing one
//! element's 2D profile in plan — a floor plate's `Sketch` (faces of
//! shared points, solid with their own thickness or void), or a wall
//! run's polyline (its points and segments; the footprint is the
//! thickened line with mitered joins).
//!
//! Lifecycle (the session is session state; the model lives in the
//! document):
//! - enter: [`AuthorApp::edit_begin`] (pencil or Hole tool on a floor
//!   plate), [`AuthorApp::edit_begin_new`] (the Floor tool), or
//!   [`AuthorApp::edit_begin_wall`] (a wall's pencil: its whole run); a
//!   gestures session opens at the document's undo depth. A legacy
//!   extrusion plate or wall is first converted INSIDE the session, below
//!   its undo floor (✗ reverts it; undo inside the session does not);
//! - edit: each accepted edit is one ordinary undo step of the ONE
//!   history — an `UpdateSketch`, or the run's walls rewritten; slider and
//!   typing gestures coalesce; the first face of a new plate creates the
//!   sketch and its element. Undo inside the session stops at its entry;
//! - leave: [`AuthorApp::edit_confirm`] keeps the steps as they are (a
//!   plate left without faces is deleted; a new one leaves no trace);
//!   [`AuthorApp::edit_cancel`] undoes back to the entry and drops the
//!   steps.

use glam::Vec3;
use wasm_bindgen::prelude::*;

use super::camera::ViewMode;
use super::{AuthorApp, SketchFrame, Tool, eid};
use vim_design_lib::{Command, EntityId};
use vim_design_lib::sketch::Sketch as Profile;

use crate::authoring::edit::interact::{self, Hit, SelectMode};
use crate::authoring::edit::presets;
use crate::authoring::edit::session::{EditSession, Marquee};
use crate::authoring::edit::{Edit, EditError, FaceKind, ProfileModel, ProfileView};
use crate::authoring::geom::P2;
use crate::authoring::model::{self, ElementModel, WallModel, WallRunModel};
use crate::authoring::ops;
use crate::authoring::rooms::RoomProfile;
use crate::authoring::runs::{RunModel, reversed, run_error};
use crate::authoring::sketch::{Shape, Sketch, SketchTool};
use crate::authoring::snap::SnapResult;

/// What Edit Mode works on: a floor plate's sketch or a wall run.
#[derive(Debug, Clone, PartialEq)]
pub enum EditProfile {
    Sketch(Profile),
    Run(RunModel),
    /// Rooms preview: a room's boundary.
    Room(RoomProfile),
}

impl Default for EditProfile {
    fn default() -> Self {
        EditProfile::Sketch(Profile::default())
    }
}

impl EditProfile {
    pub(super) fn run(&self) -> Option<&RunModel> {
        match self {
            EditProfile::Run(r) => Some(r),
            EditProfile::Sketch(_) | EditProfile::Room(_) => None,
        }
    }

    fn is_empty(&self) -> bool {
        match self {
            EditProfile::Sketch(s) => s.faces.is_empty(),
            EditProfile::Run(r) => r.data.points.is_empty(),
            EditProfile::Room(r) => r.room.boundary.is_empty(),
        }
    }
}

impl ProfileModel for EditProfile {
    fn view(&self) -> ProfileView {
        match self {
            EditProfile::Sketch(s) => s.view(),
            EditProfile::Run(r) => r.view(),
            EditProfile::Room(r) => r.view(),
        }
    }

    fn apply(&self, edit: &Edit) -> Result<Self, EditError> {
        match self {
            EditProfile::Sketch(s) => s.apply(edit).map(EditProfile::Sketch),
            EditProfile::Run(r) => r.apply(edit).map(EditProfile::Run),
            EditProfile::Room(r) => r.apply(edit).map(EditProfile::Room),
        }
    }
}

/// A wall run's Edit Mode: its element and `WallRun` entity (the run's
/// data lives in the document; the session reloads it after every edit,
/// undo, and redo).
#[derive(Debug, Clone, Copy)]
pub struct RunEdit {
    pub element: EntityId,
    pub run: EntityId,
}

/// Limits for face thickness and void depth (meters).
pub(super) const MIN_FACE_THICKNESS_M: f64 = 0.01;
pub(super) const MAX_FACE_THICKNESS_M: f64 = 5.0;

/// What a pointer gesture in Edit Mode is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EditTool {
    /// Select, move, marquee, long-press insert.
    #[default]
    Select,
    Solid,
    Void,
    Split,
    /// Wall runs: tap to add a point after the nearest end.
    Extend,
}

impl EditTool {
    fn name(self) -> &'static str {
        match self {
            EditTool::Select => "select",
            EditTool::Solid => "solid",
            EditTool::Void => "void",
            EditTool::Split => "split",
            EditTool::Extend => "extend",
        }
    }
}

/// What Edit Mode is editing, which fixes its frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EditTarget {
    /// A floor plate: the profile lies on its construction plane.
    #[default]
    Plane,
    /// A wall run: its polyline on the base plane, in plan.
    Run,
    /// Rooms preview: a room's boundary on its plane, in plan.
    Room,
}

/// Pointer state of a move drag: where it was pressed on the plane, and
/// the latest snap (drawn by the HUD).
#[derive(Debug, Clone, Default)]
pub struct EditPointer {
    pub press_uv: Option<P2>,
    pub snap: Option<SnapResult>,
}

fn json_err(e: &EditError) -> String {
    serde_json::json!({ "result": "rejected", "reason": e.message() }).to_string()
}

#[wasm_bindgen]
impl AuthorApp {
    /// Enter Edit Mode on a floor plate (the pencil button, or the Hole
    /// tool). A legacy extrusion plate is converted to a sketch plate
    /// first, inside the session's transaction.
    pub fn edit_begin(&mut self, element: f64) -> bool {
        let id = eid(element);
        if self.edit.is_some() {
            return false;
        }
        let legacy = match self.model.iter().find(|e| e.element() == id) {
            Some(ElementModel::Plate(p)) => Some(p.clone()),
            Some(ElementModel::SketchPlate(_)) => None,
            _ => return false,
        };
        self.gestures.begin_session(&self.doc);
        if let Some(plate) = legacy {
            let depth = self.doc.undo_depth();
            match ops::convert_legacy_plate(&mut self.doc, &plate) {
                Ok(_) => self.gestures.one_shot(depth),
                Err(e) => {
                    self.gestures.cancel_session(&mut self.doc);
                    self.sync("convert failed");
                    web_sys::console::error_1(&JsValue::from_str(&e));
                    return false;
                }
            }
            self.sync("convert plate");
            self.gestures.set_session_floor(&self.doc);
        }
        let Some(ElementModel::SketchPlate(p)) = self.model.iter().find(|e| e.element() == id).cloned() else {
            self.gestures.cancel_session(&mut self.doc);
            self.sync("edit failed");
            return false;
        };
        self.edit_sketch = Some(p.sketch_entity);
        self.edit_entry_element = Some(id);
        let session = self.floor_session(EditProfile::Sketch(p.sketch.clone()), p.plane_level, Some(id), p.name.clone());
        self.start_edit(session);
        true
    }

    /// Enter Edit Mode for a NEW floor plate on the active level (the
    /// Floor tool): an empty profile with the solid-face tool armed. The
    /// plate is created with its first face.
    pub fn edit_begin_new(&mut self) -> bool {
        let Some(plane) = self.plane().filter(|_| self.can_author() && self.edit.is_none()) else {
            return false;
        };
        let name = ops::next_element_name(&self.doc, "Floor plate");
        self.gestures.begin_session(&self.doc);
        self.edit_sketch = None;
        self.edit_entry_element = None;
        let session = self.floor_session(EditProfile::default(), plane, None, name);
        self.start_edit(session);
        self.edit_set_tool("solid");
        true
    }

    /// Enter Edit Mode on a wall run, in plan on its base plane: its
    /// points and segments. A wall of the earlier tools is converted
    /// first, inside the session (legacy walls to `Wall`s, then their
    /// whole chain to one run).
    pub fn edit_begin_wall(&mut self, wall: f64) -> bool {
        let id = eid(wall);
        if self.edit.is_some() || self.openings.is_some() {
            return false;
        }
        self.gestures.begin_session(&self.doc);
        let was_run = self.is_run(id);
        let element = match self.ensure_run(id) {
            Ok(e) => e,
            Err(e) => {
                self.gestures.cancel_session(&mut self.doc);
                self.sync("convert failed");
                web_sys::console::error_1(&JsValue::from_str(&e));
                return false;
            }
        };
        if !was_run {
            // The conversion is part of the session, not an edit to undo.
            self.gestures.set_session_floor(&self.doc);
        }
        let Some(r) = self.run_model(element).cloned() else {
            self.gestures.cancel_session(&mut self.doc);
            self.sync("edit failed");
            return false;
        };
        self.run_edit = Some(RunEdit { element, run: r.run });
        // Plan, on the run's base plane.
        if self.prev_camera.is_none() {
            self.prev_camera = Some(self.camera.clone());
        }
        self.set_active_plane(r.base.0 as f64);
        self.camera.set_mode(ViewMode::Plan);
        self.edit_target = EditTarget::Run;
        let model = RunModel { data: r.data.clone(), height: r.top_height };
        let mut session = EditSession::new(EditProfile::Run(model), r.base, Some(element), r.name.clone());
        session.new_thickness = r.data.thickness_m;
        self.edit_sketch = None;
        self.edit_entry_element = Some(element);
        self.start_edit(session);
        if let Some(s) = self.edit.as_mut() {
            s.set_mode(SelectMode::Points);
        }
        self.zoom_fit();
        true
    }

    /// Extend tool: a new point at the cursor after the run's nearest end.
    pub fn edit_extend(&mut self, px: f32, py: f32) -> String {
        let none = || r#"{"result":"none"}"#.to_owned();
        let Some(uv) = self.edit_cursor_uv(px, py) else { return none() };
        let Some(run) = self.edit.as_ref().and_then(|s| s.model.run()) else { return none() };
        let step = self.snap_step;
        let uv = if self.snap_enabled { [(uv[0] / step).round() * step, (uv[1] / step).round() * step] } else { uv };
        let pts: Vec<P2> = run.data.points.iter().map(|p| p.uv).collect();
        let (Some(first), Some(last)) = (pts.first(), pts.last()) else { return none() };
        let at_end = crate::authoring::geom::dist(uv, *last) <= crate::authoring::geom::dist(uv, *first);
        self.edit_apply(&Edit::Extend { at_end, uv }, None, "extended")
    }

    /// Wall run settings — one undo step each (a slider or field
    /// gesture coalesces until [`AuthorApp::edit_end_gesture`]):
    /// thickness (NaN: unchanged), thickness side and closed (-1:
    /// unchanged, 0 / 1), and the height mode: "fixed" with `height`
    /// (NaN: the current one), "upto" `plane` (-1: unchanged) plus
    /// `offset` (NaN: unchanged), or "" (unchanged).
    #[allow(clippy::too_many_arguments)]
    pub fn edit_run_settings(
        &mut self,
        thickness: f64,
        flip: i32,
        closed: i32,
        mode: &str,
        plane: f64,
        offset: f64,
        height: f64,
    ) -> String {
        let (Some(run), Some(edit)) = (self.edit.as_ref().and_then(|s| s.model.run()).cloned(), self.run_edit) else {
            return r#"{"result":"none"}"#.to_owned();
        };
        let Some(current) = self.run_model(edit.element).cloned() else { return r#"{"result":"none"}"#.to_owned() };
        let mut data = run.data.clone();
        if thickness.is_finite() {
            data.thickness_m = thickness.clamp(super::MIN_WALL_THICKNESS_M, super::MAX_WALL_THICKNESS_M);
        }
        if flip >= 0 {
            // The material to the other side: the run reversed.
            data = reversed(&data);
        }
        if closed >= 0 && (closed == 1) != data.closed {
            match vim_design_lib::wall_run::ops::set_closed(&data, closed == 1) {
                Ok(d) => data = d,
                Err(e) => return json_err(&run_error(e)),
            }
        }
        if let Err(e) = vim_design_lib::wall_run::validate(&data) {
            return json_err(&run_error(e));
        }
        // Height mode: (top, top offset, fixed height).
        let (mut top, mut top_offset, mut height_m) = (current.top, current.data.top_offset_m, current.data.height_m);
        match mode {
            "fixed" => {
                if top.is_some() {
                    // Keep the height the walls have now.
                    height_m = current.top_height;
                }
                top = None;
                if height.is_finite() {
                    height_m = height.clamp(super::MIN_WALL_HEIGHT_M, super::MAX_WALL_HEIGHT_M);
                }
            }
            "upto" => {
                if plane >= 0.0 && self.root_level(eid(plane)).is_some() {
                    top = Some(eid(plane));
                }
                if offset.is_finite() {
                    top_offset = offset.clamp(-super::MAX_WALL_HEIGHT_M, super::MAX_WALL_HEIGHT_M);
                }
                let Some(t) = top else {
                    return serde_json::json!({ "result": "rejected", "reason": "Pick the plane the walls go up to" }).to_string();
                };
                let reach = self.plane_elevation(t) + top_offset - self.plane_elevation(current.base);
                if reach < super::MIN_WALL_HEIGHT_M {
                    return serde_json::json!({
                        "result": "rejected", "reason": "The wall top must be above its base: pick a higher plane",
                    })
                    .to_string();
                }
                // The fixed height is the fallback if the top is removed.
                height_m = reach.min(super::MAX_WALL_HEIGHT_M);
            }
            _ => {}
        }
        let continuous = thickness.is_finite() || height.is_finite() || offset.is_finite();
        let key = continuous.then_some("run_settings");
        let mut cmd = ops::update_run(edit.run, &data, false);
        if let Command::UpdateWallRun { top: t, top_offset_m, height_m: h, .. } = &mut cmd {
            *t = Some(top);
            *top_offset_m = Some(top_offset);
            *h = Some(height_m);
        }
        self.submit_run_edit(cmd, key, "changed")
    }

    /// The opening presets (meters), for the page's hints.
    pub fn presets_json(&self) -> String {
        serde_json::json!({
            "window": { "width": presets::WINDOW_WIDTH_M, "height": presets::WINDOW_HEIGHT_M, "sill": presets::WINDOW_SILL_M },
            "door": { "width": presets::DOOR_WIDTH_M, "height": presets::DOOR_HEIGHT_M },
        })
        .to_string()
    }

    pub fn edit_active(&self) -> bool {
        self.edit.is_some()
    }

    /// Leave Edit Mode keeping the changes: the whole session becomes ONE
    /// undo step. A plate left without faces is deleted (a new one leaves
    /// no trace). Returns JSON `{"result": "confirmed", "changed": bool,
    /// "name": ..., "deleted": bool}`.
    pub fn edit_confirm(&mut self) -> String {
        let Some(session) = self.edit.take() else {
            return r#"{"result":"none"}"#.to_owned();
        };
        if self.edit_target == EditTarget::Room {
            let changed = self.end_room_session(true);
            self.leave_edit();
            return serde_json::json!({ "result": "confirmed", "changed": changed, "name": session.name }).to_string();
        }
        let empty = session.model.is_empty();
        let mut deleted = false;
        if empty && self.edit_entry_element.is_none() {
            // A new plate whose faces were all removed: nothing happened.
            self.gestures.cancel_session(&mut self.doc);
            self.leave_edit();
            self.sync("edit (nothing kept)");
            return serde_json::json!({ "result": "confirmed", "changed": false, "name": session.name }).to_string();
        }
        if empty
            && let Some(element) = session.element.filter(|e| self.doc.entity(*e).is_some()) {
                let depth = self.doc.undo_depth();
                match ops::delete_element(&mut self.doc, element) {
                    Ok(()) => {
                        self.gestures.one_shot(depth);
                        deleted = true;
                    }
                    Err(e) => {
                        ops::rollback_to(&mut self.doc, depth);
                        web_sys::console::error_1(&JsValue::from_str(&e));
                    }
                }
            }
        let changed = self.gestures.end_session(&self.doc);
        self.leave_edit();
        self.sync("edit");
        serde_json::json!({
            "result": "confirmed", "changed": changed, "name": session.name, "deleted": deleted,
        })
        .to_string()
    }

    /// Leave Edit Mode discarding the changes: the document returns to
    /// its state at entry.
    pub fn edit_cancel(&mut self) {
        if self.edit_target == EditTarget::Room && self.edit.take().is_some() {
            self.end_room_session(false);
            self.leave_edit();
            return;
        }
        if self.edit.take().is_some() {
            self.gestures.cancel_session(&mut self.doc);
            self.leave_edit();
            self.sync("edit cancelled");
        }
    }

    /// Selection mode: "points", "edges", or "faces".
    pub fn edit_set_mode(&mut self, mode: &str) {
        let mode = match mode {
            "points" => SelectMode::Points,
            "edges" => SelectMode::Edges,
            _ => SelectMode::Faces,
        };
        if let Some(s) = self.edit.as_mut() {
            s.set_mode(mode);
        }
    }

    /// Edit tool: "select", or for floor plates "solid", "void", "split"
    /// (they draw with the shared sketch tools), for wall runs "extend".
    pub fn edit_set_tool(&mut self, tool: &str) -> bool {
        let Some(level) = self.edit.as_ref().map(|s| s.level) else {
            return false;
        };
        let run = self.edit_target != EditTarget::Plane;
        let room = self.edit_target == EditTarget::Room;
        let tool = if room { "select" } else { tool };
        self.edit_tool = match tool {
            "solid" if !run => EditTool::Solid,
            "void" if !run => EditTool::Void,
            "split" if !run => EditTool::Split,
            "extend" if run => EditTool::Extend,
            _ => EditTool::Select,
        };
        self.sketch = match self.edit_tool {
            EditTool::Select | EditTool::Extend => None,
            EditTool::Solid | EditTool::Void => Some(Sketch::new(SketchTool::Profile, self.shape, level)),
            EditTool::Split => Some(Sketch::new(SketchTool::Split, Shape::Polygon, level)),
        };
        if let Some(s) = self.edit.as_mut() {
            s.drag = None;
            s.marquee = None;
        }
        true
    }

    /// Hover (mouse/pen): highlight the item under the pointer.
    pub fn edit_hover(&mut self, px: f32, py: f32, tol: f32) {
        let hit = self.edit_hit(px, py, tol);
        if let Some(s) = self.edit.as_mut() {
            s.hover = hit;
        }
    }

    /// Tap: select the item under the pointer (`additive` toggles it);
    /// empty space clears the selection.
    pub fn edit_tap(&mut self, px: f32, py: f32, tol: f32, additive: bool) {
        let hit = self.edit_hit(px, py, tol);
        if let Some(s) = self.edit.as_mut() {
            s.tap(hit, additive);
        }
    }

    /// A press became a drag. On a selectable item it starts moving the
    /// selection ("move"); on empty space the page draws a marquee
    /// ("marquee").
    pub fn edit_drag_begin(&mut self, px: f32, py: f32, tol: f32, additive: bool) -> String {
        let hit = self.edit_hit(px, py, tol);
        let cursor = self.edit_cursor_uv(px, py);
        let Some(s) = self.edit.as_mut() else { return "none".to_owned() };
        match (hit, cursor) {
            (Some(h), Some(uv)) => {
                let view = s.model.view();
                let mut sel = s.selection.clone();
                if !sel.contains(&h) {
                    if !additive {
                        sel = Default::default();
                    }
                    sel.insert(&h);
                }
                let moving = interact::moving_points(&view, &sel);
                let anchor = interact::drag_anchor(&view, &h, &moving, uv).unwrap_or(uv);
                s.begin_drag(&h, anchor, additive);
                self.edit_pointer = EditPointer { press_uv: Some(uv), snap: None };
                "move".to_owned()
            }
            _ => {
                let base = if additive { s.selection.clone() } else { Default::default() };
                s.marquee = Some(Marquee { rect: [px, py, px, py], additive, base: base.clone() });
                s.selection = base;
                "marquee".to_owned()
            }
        }
    }

    /// Drag update (device pixels): moves snap to other points, to
    /// horizontal/vertical alignment with the start, and to the grid.
    pub fn edit_drag_move(&mut self, px: f32, py: f32, tol: f32) {
        let Some(uv) = self.edit_cursor_uv(px, py) else { return };
        let Some(press) = self.edit_pointer.press_uv else { return };
        let ppm = self.edit_px_per_m(uv);
        let (enabled, step) = (self.snap_enabled, self.edit_snap_step());
        let Some(s) = self.edit.as_mut() else { return };
        let Some(d) = &s.drag else { return };
        let raw = [d.anchor[0] + uv[0] - press[0], d.anchor[1] + uv[1] - press[1]];
        let snap = interact::snap_move(
            &s.model.view(),
            &d.moving,
            d.anchor,
            raw,
            enabled,
            step,
            f64::from(tol / ppm.max(1e-3)),
        );
        let delta = [snap.point[0] - d.anchor[0], snap.point[1] - d.anchor[1]];
        s.update_drag(delta);
        self.edit_pointer.snap = Some(snap);
    }

    /// Release a move: apply it, or snap back if the result is invalid.
    /// Returns JSON `{"result": "moved"|"none"|"rejected", "reason"?}`.
    pub fn edit_drag_end(&mut self) -> String {
        self.edit_pointer = EditPointer::default();
        let Some(s) = self.edit.as_mut() else { return r#"{"result":"none"}"#.to_owned() };
        match s.end_drag() {
            Ok(Some(edit)) => self.edit_apply(&edit, None, "moved"),
            Ok(None) => r#"{"result":"none"}"#.to_owned(),
            Err(e) => json_err(&e),
        }
    }

    /// Marquee update: the rectangle from the press to the pointer
    /// (device pixels); items fully inside are selected live.
    pub fn edit_marquee(&mut self, x0: f32, y0: f32, x1: f32, y1: f32) {
        let frame = self.edit_frame();
        let (w, h) = self.size_f();
        let camera = self.camera.clone();
        let Some(s) = self.edit.as_mut() else { return };
        let Some(m) = s.marquee.as_mut() else { return };
        m.rect = [x0, y0, x1, y1];
        let (base, rect) = (m.base.clone(), m.rect);
        let view = s.model.view();
        let proj = |p: P2| camera.project(frame.world(p), w, h).map(|(x, y)| [x, y]);
        let mut sel = base;
        sel.extend(&interact::marquee(&view, s.mode, &proj, rect));
        s.selection = sel;
    }

    pub fn edit_marquee_end(&mut self) {
        if let Some(s) = self.edit.as_mut() {
            s.marquee = None;
        }
    }

    /// Abandon a drag or marquee (a second finger came down).
    pub fn edit_gesture_cancel(&mut self) {
        self.edit_pointer = EditPointer::default();
        if let Some(s) = self.edit.as_mut() {
            s.drag = None;
            if let Some(m) = s.marquee.take() {
                s.selection = m.base;
            }
        }
    }

    /// Long press in Points mode on an edge: insert a point there (it
    /// becomes the selection). Returns JSON with its canvas position.
    pub fn edit_long_press(&mut self, px: f32, py: f32, tol: f32) -> String {
        let cursor = self.edit_cursor_uv(px, py);
        let frame = self.edit_frame();
        let (w, h) = self.size_f();
        let Some(s) = self.edit.as_ref() else { return r#"{"result":"none"}"#.to_owned() };
        if s.mode != SelectMode::Points {
            return r#"{"result":"none"}"#.to_owned();
        }
        let view = s.model.view();
        let proj = |p: P2| self.camera.project(frame.world(p), w, h).map(|(x, y)| [x, y]);
        let Some((edge, uv)) = interact::hit_edge(&view, &proj, [px, py], tol, cursor) else {
            return r#"{"result":"none"}"#.to_owned();
        };
        let before: std::collections::BTreeSet<u32> = view.points.iter().map(|p| p.id).collect();
        let out = self.edit_apply(&Edit::InsertPoint { edge, uv }, None, "inserted");
        if let Some(s) = self.edit.as_mut() {
            let added = s.model.view().points.iter().map(|p| p.id).find(|id| !before.contains(id));
            if let Some(id) = added {
                s.selection = Default::default();
                s.selection.points.insert(id);
            }
        }
        match self.camera.project(frame.world(uv), w, h) {
            Some((x, y)) if out.contains("inserted") => {
                serde_json::json!({ "result": "inserted", "x": x, "y": y }).to_string()
            }
            _ => out,
        }
    }

    /// Delete the selection (points reconnect their neighbours, edges
    /// merge into their first point, faces drop their private points).
    pub fn edit_delete(&mut self) -> String {
        let Some(edit) = self.edit.as_ref().and_then(|s| s.delete_edit()) else {
            return r#"{"result":"none"}"#.to_owned();
        };
        self.edit_apply(&edit, None, "deleted")
    }

    pub fn edit_undo(&mut self) -> bool {
        if self.edit_target == EditTarget::Room {
            return self.edit.is_some() && self.rooms_undo();
        }
        if self.edit.is_none() || !self.gestures.undo(&mut self.doc) {
            return false;
        }
        self.sync("edit undo");
        self.reload_edit_model();
        true
    }

    pub fn edit_redo(&mut self) -> bool {
        if self.edit_target == EditTarget::Room {
            return self.edit.is_some() && self.rooms_redo();
        }
        if self.edit.is_none() || !self.gestures.redo(&mut self.doc) {
            return false;
        }
        self.sync("edit redo");
        self.reload_edit_model();
        true
    }

    /// Thickness of the selected solid faces, or of new solid faces when
    /// none are selected. Coalesced until [`AuthorApp::edit_end_gesture`].
    pub fn edit_set_thickness(&mut self, t: f64) -> String {
        if !t.is_finite() {
            return r#"{"result":"none"}"#.to_owned();
        }
        let t = t.clamp(MIN_FACE_THICKNESS_M, MAX_FACE_THICKNESS_M);
        let Some(s) = self.edit.as_mut() else { return r#"{"result":"none"}"#.to_owned() };
        let solids = selected_faces(s, false);
        let fresh = s.is_fresh();
        if solids.is_empty() || fresh {
            // The default for new faces; the just-drawn faces follow it.
            s.new_thickness = t;
            if self.edit_target == EditTarget::Plane {
                self.plate_thickness = t; // remembered for the next new plate
            }
        }
        if solids.is_empty() {
            return r#"{"result":"default"}"#.to_owned();
        }
        self.edit_apply(&Edit::SetKind { faces: solids, kind: FaceKind::Solid { thickness: t } }, Some("thickness"), "changed")
    }

    /// Depth of the selected void faces (or new voids); a depth turns
    /// "through" off.
    pub fn edit_set_depth(&mut self, depth: f64) -> String {
        if !depth.is_finite() {
            return r#"{"result":"none"}"#.to_owned();
        }
        let d = depth.clamp(MIN_FACE_THICKNESS_M, MAX_FACE_THICKNESS_M);
        let Some(s) = self.edit.as_mut() else { return r#"{"result":"none"}"#.to_owned() };
        let voids = selected_faces(s, true);
        if voids.is_empty() || s.is_fresh() {
            s.new_void_depth = d;
            s.new_void_through = false;
            self.remembered.void_depth = d;
            self.remembered.void_through = false;
        }
        if voids.is_empty() {
            return r#"{"result":"default"}"#.to_owned();
        }
        self.edit_apply(&Edit::SetKind { faces: voids, kind: FaceKind::Void { depth: Some(d) } }, Some("depth"), "changed")
    }

    /// Voids cut all the way through (`true`) or to their depth.
    pub fn edit_set_through(&mut self, through: bool) -> String {
        let Some(s) = self.edit.as_mut() else { return r#"{"result":"none"}"#.to_owned() };
        let voids = selected_faces(s, true);
        if voids.is_empty() || s.is_fresh() {
            s.new_void_through = through;
            self.remembered.void_through = through;
        }
        if voids.is_empty() {
            return r#"{"result":"default"}"#.to_owned();
        }
        let depth = s
            .selected_kinds()
            .iter()
            .find_map(|k| match k {
                FaceKind::Void { depth: Some(d) } => Some(*d),
                _ => None,
            })
            .unwrap_or(s.new_void_depth);
        let kind = FaceKind::Void { depth: (!through).then_some(depth) };
        self.edit_apply(&Edit::SetKind { faces: voids, kind }, None, "changed")
    }

    /// Close a coalescing gesture (the slider or field was released).
    pub fn edit_end_gesture(&mut self) {
        self.gestures.end();
    }

    /// Edit Mode state for the page: name, mode, tool, selection size,
    /// undo availability, and the thickness panel.
    pub fn edit_state_json(&self) -> String {
        let Some(s) = &self.edit else {
            return r#"{"active":false}"#.to_owned();
        };
        let kinds = if s.mode == SelectMode::Faces { s.selected_kinds() } else { Vec::new() };
        let solids: Vec<f64> = kinds
            .iter()
            .filter_map(|k| match k {
                FaceKind::Solid { thickness } => Some(*thickness),
                _ => None,
            })
            .collect();
        let voids: Vec<Option<f64>> = kinds
            .iter()
            .filter_map(|k| match k {
                FaceKind::Void { depth } => Some(*depth),
                _ => None,
            })
            .collect();
        let target = if kinds.is_empty() {
            "new"
        } else if s.is_fresh() {
            "fresh" // the just-drawn faces: their values are also the defaults
        } else {
            "selection"
        };
        let solid = if kinds.is_empty() {
            Some(serde_json::json!({ "count": 0, "thickness": s.new_thickness, "mixed": false }))
        } else {
            solids.first().map(|t| {
                serde_json::json!({
                    "count": solids.len(),
                    "thickness": t,
                    "mixed": solids.iter().any(|x| (x - t).abs() > 1e-9),
                })
            })
        };
        let void = if kinds.is_empty() {
            Some(serde_json::json!({
                "count": 0, "depth": s.new_void_depth, "through": s.new_void_through, "mixed": false,
            }))
        } else {
            voids.first().map(|d| {
                serde_json::json!({
                    "count": voids.len(),
                    "depth": d.unwrap_or(s.new_void_depth),
                    "through": d.is_none(),
                    "mixed": voids.iter().any(|x| x != d),
                })
            })
        };
        serde_json::json!({
            "active": true,
            "name": s.name,
            "isNew": s.element.is_none(),
            "mode": s.mode.name(),
            "tool": self.edit_tool.name(),
            "selection": s.selection.len(),
            "canDelete": !s.selection.is_empty(),
            "canUndo": self.edit_can_undo(),
            "canRedo": self.edit_can_redo(),
            "target": match self.edit_target {
                EditTarget::Run => "run",
                EditTarget::Room => "room",
                EditTarget::Plane => "floor",
            },
            "room": self.room_edit_state_json(),
            "run": self.run_state_json(),
            "faces": s.model.view().faces.len(),
            "panel": { "target": target, "solid": solid, "void": void },
        })
        .to_string()
    }

    /// The profile being edited, in plane coordinates (test hook).
    pub fn edit_profile_json(&self) -> String {
        let Some(s) = &self.edit else { return "null".to_owned() };
        profile_json(&s.model.view())
    }

    /// Screen-space overlay of the profile (device pixels): faces (solid
    /// filled, void dotted), edges, points, hover/selection, marquee,
    /// drag validity and snap.
    pub fn edit_hud_json(&self) -> String {
        let Some(s) = &self.edit else { return r#"{"active":false}"#.to_owned() };
        let frame = self.edit_frame();
        let (w, h) = self.size_f();
        let proj = |p: P2| self.camera.project(frame.world(p), w, h).map(|(x, y)| [x, y]);
        let view = s.view();
        let hover = s.hover;
        let mut faces: Vec<&crate::authoring::edit::ProfileFace> = view.faces.iter().collect();
        faces.sort_by_key(|f| f.kind.is_void()); // voids drawn on top
        let faces_json: Vec<serde_json::Value> = faces
            .iter()
            .map(|f| {
                let pts: Vec<[f32; 2]> = view.polygon(f).iter().filter_map(|p| proj(*p)).collect();
                serde_json::json!({
                    "id": f.id,
                    "kind": if f.kind.is_void() { "void" } else { "solid" },
                    "pts": pts,
                    "sel": s.selection.faces.contains(&f.id),
                    "hover": hover == Some(Hit::Face(f.id)),
                })
            })
            .collect();
        let edges_json: Vec<serde_json::Value> = view
            .edges()
            .iter()
            .filter_map(|e| {
                let (a, b) = (proj(view.point(e.0)?)?, proj(view.point(e.1)?)?);
                let void_only = view
                    .faces
                    .iter()
                    .filter(|f| {
                        let n = f.points.len();
                        (0..n).any(|i| crate::authoring::edit::EdgeKey::new(f.points[i], f.points[(i + 1) % n]) == *e)
                    })
                    .all(|f| f.kind.is_void());
                Some(serde_json::json!({
                    "p": [a[0], a[1], b[0], b[1]],
                    "sel": s.selection.edges.contains(e),
                    "hover": matches!(hover, Some(Hit::Edge { edge, .. }) if edge == *e),
                    "void": void_only,
                }))
            })
            .collect();
        let points_json: Vec<serde_json::Value> = view
            .points
            .iter()
            .filter_map(|p| {
                let xy = proj(p.uv)?;
                Some(serde_json::json!({
                    "id": p.id,
                    "p": xy,
                    "sel": s.selection.points.contains(&p.id),
                    "hover": hover == Some(Hit::Point(p.id)),
                }))
            })
            .collect();
        let snap = self.edit_pointer.snap.as_ref().and_then(|sn| {
            proj(sn.point).map(|[x, y]| serde_json::json!({ "x": x, "y": y, "kind": sn.kind.name() }))
        });
        let guides: Vec<[f32; 4]> = self
            .edit_pointer
            .snap
            .as_ref()
            .map(|sn| {
                sn.guides
                    .iter()
                    .filter_map(|(a, b)| Some([proj(*a)?[0], proj(*a)?[1], proj(*b)?[0], proj(*b)?[1]]))
                    .collect()
            })
            .unwrap_or_default();
        // A wall run: its thickened footprint (the dragged run while a
        // move is shown), rings in screen space.
        let run = match &s.drag {
            Some(crate::authoring::edit::session::Drag { preview: Ok(p), .. }) => p.run(),
            _ => s.model.run(),
        };
        let mut footprint: Vec<Vec<[f32; 2]>> = run
            .map(|r| crate::authoring::runs::footprint(&r.data).iter().map(|ring| ring.iter().filter_map(|p| proj(*p)).collect()).collect())
            .unwrap_or_default();
        // A room: its boundary as the filled outline (edges and points on top).
        let room = match (&s.drag, &s.model) {
            (Some(crate::authoring::edit::session::Drag { preview: Ok(EditProfile::Room(p)), .. }), _) | (_, EditProfile::Room(p)) => Some(p),
            _ => None,
        };
        if let Some(p) = room {
            footprint = vec![p.room.polygon().iter().filter_map(|q| proj(*q)).collect()];
        }
        serde_json::json!({
            "active": true,
            "mode": s.mode.name(),
            "footprint": footprint,
            "closed": run.is_some_and(|r| r.data.closed) || room.is_some(),
            "room": room.is_some(),
            "faces": if run.is_some() || room.is_some() { Vec::new() } else { faces_json },
            "edges": edges_json,
            "points": points_json,
            "marquee": s.marquee.as_ref().map(|m| m.rect),
            "dragging": s.drag.is_some(),
            "invalid": s.drag.as_ref().is_some_and(|d| d.preview.is_err()),
            "invalidReason": s.drag.as_ref().and_then(|d| d.preview.as_ref().err().map(|e| e.message())),
            "snap": snap,
            "guides": guides,
        })
        .to_string()
    }
}

fn selected_faces(s: &EditSession<EditProfile>, voids: bool) -> Vec<u32> {
    if s.mode != SelectMode::Faces {
        return Vec::new();
    }
    s.model
        .view()
        .faces
        .iter()
        .filter(|f| s.selection.faces.contains(&f.id) && f.kind.is_void() == voids)
        .map(|f| f.id)
        .collect()
}

fn profile_json(view: &ProfileView) -> String {
    serde_json::json!({
        "points": view.points.iter().map(|p| serde_json::json!({ "id": p.id, "uv": p.uv })).collect::<Vec<_>>(),
        "faces": view.faces.iter().map(|f| serde_json::json!({
            "id": f.id,
            "points": f.points,
            "outline": view.polygon(f),
            "kind": match f.kind {
                FaceKind::Solid { thickness } => serde_json::json!({ "solid": thickness }),
                FaceKind::Void { depth } => serde_json::json!({ "void": depth }),
            },
        })).collect::<Vec<_>>(),
    })
    .to_string()
}

impl AuthorApp {
    /// The run session's state for the page (`null` outside one).
    fn run_state_json(&self) -> serde_json::Value {
        let (Some(run), Some(r)) = (
            self.edit.as_ref().and_then(|s| s.model.run()),
            self.run_edit.and_then(|e| self.run_model(e.element)),
        ) else {
            return serde_json::Value::Null;
        };
        serde_json::json!({
            "points": run.data.points.len(),
            "segments": run.data.segment_count(),
            "closed": run.data.closed,
            "thickness": run.data.thickness_m,
            "openings": run.data.openings.len(),
            "mode": if r.top.is_some() { "upto" } else { "fixed" },
            "topPlane": r.top.map(|t| t.0 as f64),
            "topOffset": r.data.top_offset_m,
            "height": r.top_height,
            "element": r.element.0 as f64,
            "base": r.base.0 as f64,
            "footprintArea": crate::authoring::runs::footprint_area(&run.data),
        })
    }

    /// A wall run of the model, by element.
    pub(super) fn run_model(&self, element: EntityId) -> Option<&WallRunModel> {
        self.model.iter().find_map(|e| match e {
            ElementModel::Run(r) if r.element == element => Some(r),
            _ => None,
        })
    }

    fn is_run(&self, element: EntityId) -> bool {
        self.run_model(element).is_some()
    }

    /// The wall run `element` is (or becomes): walls of the earlier tools
    /// are converted — legacy walls on its level to `Wall`s, then the
    /// connected chain to one run element (each conversion one undo
    /// step). Returns the run's element.
    pub(super) fn ensure_run(&mut self, element: EntityId) -> Result<EntityId, String> {
        if self.is_run(element) {
            return Ok(element);
        }
        if let Some(ElementModel::LegacyWall(w)) = self.model.iter().find(|e| e.element() == element) {
            let level = w.plane_level;
            self.convert_legacy_walls(level)?;
        }
        if !matches!(self.model.iter().find(|e| e.element() == element), Some(ElementModel::Wall(_))) {
            return Err("not a wall".to_owned());
        }
        let walls: Vec<WallModel> = self
            .model
            .iter()
            .filter_map(|e| if let ElementModel::Wall(w) = e { Some(w.clone()) } else { None })
            .collect();
        let depth = self.doc.undo_depth();
        match ops::convert_chain_to_run(&mut self.doc, &walls, element) {
            Ok(run_element) => {
                self.gestures.one_shot(depth);
                self.sync("convert to wall run");
                Ok(run_element)
            }
            Err(e) => {
                ops::rollback_to(&mut self.doc, depth);
                self.gestures.invalidate_redo();
                self.sync("convert failed");
                Err(e)
            }
        }
    }

    /// Convert every legacy extrusion wall on `level` to a `Wall` — one
    /// undo step.
    pub(super) fn convert_legacy_walls(&mut self, level: EntityId) -> Result<(), String> {
        let legacy: Vec<_> = self
            .model
            .iter()
            .filter_map(|e| match e {
                ElementModel::LegacyWall(w) if w.plane_level == level => Some(w.clone()),
                _ => None,
            })
            .collect();
        let depth = self.doc.undo_depth();
        if let Err(e) = legacy.iter().try_for_each(|w| ops::convert_legacy_wall(&mut self.doc, w).map(|_| ())) {
            ops::rollback_to(&mut self.doc, depth);
            return Err(e);
        }
        self.gestures.one_shot(depth);
        self.sync("convert walls");
        Ok(())
    }

    /// Store a run edit: one `UpdateWallRun` — one undo step, or part of
    /// the open gesture `key` (coalesced).
    fn submit_run_edit(&mut self, cmd: Command, key: Option<&str>, ok: &str) -> String {
        let depth = self.doc.undo_depth();
        let coalesce = match key {
            Some(k) => self.gestures.begin_continuing(&self.doc, &format!("edit_{k}")),
            None => false,
        };
        let cmd = match cmd {
            Command::UpdateWallRun { id, base, top, points, closed, thickness_m, height_m, top_offset_m, openings, profiles, .. } => {
                Command::UpdateWallRun { id, base, top, points, closed, thickness_m, height_m, top_offset_m, openings, profiles, coalesce }
            }
            other => other,
        };
        match self.doc.submit(cmd) {
            Ok(_) => {
                if key.is_none() {
                    self.gestures.one_shot(depth);
                }
                self.sync("edit wall run");
                self.reload_edit_model();
                serde_json::json!({ "result": ok }).to_string()
            }
            Err(st) => {
                ops::rollback_to(&mut self.doc, depth);
                self.sync("edit failed");
                serde_json::json!({ "result": "rejected", "reason": format!("the wall was refused ({st:?})") }).to_string()
            }
        }
    }

    pub(super) fn edit_can_undo(&self) -> bool {
        if self.edit_target == EditTarget::Room { self.rooms_can_undo() } else { self.gestures.can_undo() }
    }

    pub(super) fn edit_can_redo(&self) -> bool {
        if self.edit_target == EditTarget::Room { self.rooms_can_redo() } else { self.gestures.can_redo() }
    }

    pub(super) fn start_edit(&mut self, session: EditSession<EditProfile>) {
        self.selection = None;
        self.paste_armed = false; // a paste belongs to the mode it was armed in
        self.tool = Tool::Select;
        self.sketch = None;
        self.edit_tool = EditTool::Select;
        self.edit_pointer = EditPointer::default();
        self.edit = Some(session);
        self.refresh_styles();
    }

    fn leave_edit(&mut self) {
        self.paste_armed = false;
        if matches!(self.edit_target, EditTarget::Run | EditTarget::Room) {
            if let Some(c) = self.prev_camera.take() {
                self.camera = c;
            }
            self.camera.plane_z = self.active_elevation as f32;
            self.grid_key = None;
        }
        self.run_edit = None;
        self.edit_target = EditTarget::Plane;
        self.sketch = None;
        self.edit_tool = EditTool::Select;
        self.edit_pointer = EditPointer::default();
        self.edit_sketch = None;
        self.edit_entry_element = None;
        self.refresh_styles();
    }

    /// Read the session's profile back from the document (after an edit,
    /// undo, or redo). A sketch undone away leaves an empty profile; redo
    /// brings the same entity back.
    fn reload_edit_model(&mut self) {
        if self.edit_target == EditTarget::Room {
            self.reload_room_edit();
            return;
        }
        if self.edit_target == EditTarget::Run {
            let run = self.run_edit.and_then(|e| self.run_model(e.element)).map(|r| RunModel { data: r.data.clone(), height: r.top_height });
            if let (Some(run), Some(s)) = (run, self.edit.as_mut()) {
                s.set_model(EditProfile::Run(run));
            }
            self.refresh_styles();
            return;
        }
        let stored = self.edit_sketch.and_then(|id| model::sketch_params(&self.doc, id));
        let element = self.edit_sketch.and_then(|sk| {
            self.model.iter().find_map(|e| match e {
                ElementModel::SketchPlate(p) if p.sketch_entity == sk => Some(p.element),
                _ => None,
            })
        });
        if let Some(s) = self.edit.as_mut() {
            s.element = element.or(self.edit_entry_element);
            s.set_model(EditProfile::Sketch(stored.map(|(sketch, _)| sketch).unwrap_or_default()));
        }
        self.refresh_styles();
    }

    /// The plane the edited profile lives on.
    pub(super) fn edit_frame(&self) -> SketchFrame {
        let plane = self.edit.as_ref().map_or(vim_design_lib::EntityId::INVALID, |s| s.level);
        SketchFrame {
            origin: Vec3::new(0.0, 0.0, self.plane_elevation(plane) as f32),
            u: Vec3::X,
            v: Vec3::Y,
        }
    }

    fn edit_cursor_uv(&self, px: f32, py: f32) -> Option<P2> {
        let frame = self.edit_frame();
        let (w, h) = self.size_f();
        self.camera
            .unproject_to(px, py, w, h, frame.origin, frame.normal())
            .map(|hit| frame.local(hit))
    }

    fn edit_px_per_m(&self, uv: P2) -> f32 {
        let frame = self.edit_frame();
        self.px_per_m_at(frame.world(uv), &frame)
    }

    fn edit_hit(&self, px: f32, py: f32, tol: f32) -> Option<Hit> {
        let s = self.edit.as_ref()?;
        let frame = self.edit_frame();
        let (w, h) = self.size_f();
        let proj = |p: P2| self.camera.project(frame.world(p), w, h).map(|(x, y)| [x, y]);
        interact::hit(&s.model.view(), s.mode, &proj, [px, py], tol, self.edit_cursor_uv(px, py))
    }

    /// Accept an edit: compute it with the library's ops (validated live),
    /// store it with one `UpdateSketch` (coalesced under `key`), or create
    /// the plate with its first face. Returns page JSON.
    pub(super) fn edit_apply(&mut self, edit: &Edit, key: Option<&str>, ok: &str) -> String {
        let Some(s) = self.edit.as_ref() else { return r#"{"result":"none"}"#.to_owned() };
        let next = match s.model.apply(edit) {
            Ok(next) => next,
            Err(e) => return json_err(&e),
        };
        if let EditProfile::Run(run) = next {
            let Some(edit) = self.run_edit else { return r#"{"result":"none"}"#.to_owned() };
            return self.submit_run_edit(ops::update_run(edit.run, &run.data, false), key, ok);
        }
        if let EditProfile::Room(p) = next {
            return self.store_room_edit(p.room, ok);
        }
        let EditProfile::Sketch(next) = next else { return r#"{"result":"none"}"#.to_owned() };
        let (plane, name) = (s.level, s.name.clone());
        let level = self.root_level(plane).unwrap_or(plane);
        let depth = self.doc.undo_depth();
        let stored = self.edit_sketch.filter(|id| self.doc.entity(*id).is_some());
        let result = match stored {
            Some(id) => {
                let coalesce = match key {
                    Some(k) => self.gestures.begin_continuing(&self.doc, &format!("edit_{k}")),
                    None => false,
                };
                self.doc
                    .submit(Command::UpdateSketch { id, sketch: next.clone(), coalesce })
                    .map(|_| ())
                    .map_err(|st| format!("the sketch was refused ({st:?})"))
            }
            None if next.faces.is_empty() => {
                // Nothing to store yet (a new plate without faces).
                if let Some(s) = self.edit.as_mut() {
                    s.set_model(EditProfile::Sketch(next));
                }
                return serde_json::json!({ "result": ok }).to_string();
            }
            None => ops::create_sketch_element(&mut self.doc, plane, level, &next, &name).map(|(element, sketch)| {
                self.edit_sketch = Some(sketch);
                if let Some(s) = self.edit.as_mut() {
                    s.element = Some(element);
                }
            }),
        };
        match result {
            Ok(()) => {
                if key.is_none() {
                    self.gestures.one_shot(depth);
                }
                self.sync("edit");
                self.reload_edit_model();
                serde_json::json!({ "result": ok }).to_string()
            }
            Err(e) => {
                ops::rollback_to(&mut self.doc, depth);
                self.sync("edit failed");
                serde_json::json!({ "result": "rejected", "reason": e }).to_string()
            }
        }
    }

    /// A finished sketch in Edit Mode: a new face, or a split line.
    pub(super) fn finish_edit_sketch(&mut self, tool: SketchTool, outline: Vec<P2>) -> String {
        let Some(s) = self.edit.as_ref() else { return r#"{"result":"none"}"#.to_owned() };
        let edit = match (tool, outline.as_slice()) {
            (SketchTool::Split, [a, b, ..]) => Edit::SplitFaces { a: *a, b: *b },
            _ => {
                let kind = if self.edit_tool == EditTool::Void { s.new_void_kind() } else { s.new_solid_kind() };
                Edit::AddFace { outline, kind }
            }
        };
        let adding = matches!(edit, Edit::AddFace { .. });
        let before: std::collections::BTreeSet<u32> = s.model.view().faces.iter().map(|f| f.id).collect();
        let out = self.edit_apply(&edit, None, "committed");
        if let Some(sk) = self.sketch.as_mut() {
            sk.points.clear();
        }
        if adding && out.contains(r#""result":"committed""#) {
            self.select_new_faces(&before);
        }
        out
    }

    /// After a face was added: the faces not in `before` become the
    /// selection (the panel then targets them — "fresh").
    pub(super) fn select_new_faces(&mut self, before: &std::collections::BTreeSet<u32>) {
        let Some(s) = self.edit.as_mut() else { return };
        let fresh: std::collections::BTreeSet<u32> =
            s.model.view().faces.iter().map(|f| f.id).filter(|id| !before.contains(id)).collect();
        if !fresh.is_empty() {
            s.select_fresh(fresh);
        }
    }

    /// A floor plate session with the remembered defaults for new faces.
    fn floor_session(
        &self,
        profile: EditProfile,
        plane: EntityId,
        element: Option<EntityId>,
        name: String,
    ) -> EditSession<EditProfile> {
        let mut session = EditSession::new(profile, plane, element, name);
        session.new_thickness = self.plate_thickness;
        session.new_void_depth = self.remembered.void_depth;
        session.new_void_through = self.remembered.void_through;
        session
    }
}

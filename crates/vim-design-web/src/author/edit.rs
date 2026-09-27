//! Edit Mode of the authoring app: a modal session editing one
//! element's 2D profile — the library's `Sketch` (faces of shared
//! points, solid with their own thickness or void).
//!
//! Lifecycle (the session is session state; the profile lives in the
//! document):
//! - enter: [`AuthorApp::edit_begin`] (pencil or Hole tool on a floor
//!   plate) or [`AuthorApp::edit_begin_new`] (the Floor tool); a gestures
//!   transaction opens at the document's undo depth. A legacy extrusion
//!   plate is first converted to a sketch plate INSIDE the transaction,
//!   below its undo floor (✗ reverts it; step-by-step undo does not);
//! - edit: each accepted edit is one `UpdateSketch` (slider and typing
//!   gestures coalesce); the first face of a new plate creates the
//!   sketch and its element. Undo/redo inside the session are the
//!   document's, bounded by the transaction;
//! - leave: [`AuthorApp::edit_confirm`] collapses the session into ONE
//!   undo step of the main history (a plate left without faces is
//!   deleted; a new one leaves no trace); [`AuthorApp::edit_cancel`]
//!   reverts the document to the entry state and drops the session's
//!   redo.

use glam::Vec3;
use wasm_bindgen::prelude::*;

use super::{AuthorApp, SketchFrame, Tool, eid};
use vim_design_lib::Command;
use vim_design_lib::sketch::Sketch as Profile;

use crate::authoring::edit::interact::{self, Hit, SelectMode};
use crate::authoring::edit::presets::{Opening, preset_outline};
use crate::authoring::edit::session::{EditSession, Marquee};
use crate::authoring::edit::{Edit, EditError, FaceKind, ProfileModel, ProfileView};
use crate::authoring::geom::P2;
use crate::authoring::model::{self, ElementModel};
use crate::authoring::ops;
use crate::authoring::sketch::{Shape, Sketch, SketchTool};
use crate::authoring::snap::SnapResult;

/// The profile Edit Mode works on: the library's sketch.
pub type EditProfile = Profile;

/// Limits for face thickness and void depth (meters).
const MIN_FACE_THICKNESS_M: f64 = 0.01;
const MAX_FACE_THICKNESS_M: f64 = 5.0;
/// Wall Edit Mode on a landscape screen: the edit panel covers the top
/// right, so the elevation zooms out by this factor and the wall moves
/// left by this fraction of its length.
const WALL_EDIT_LANDSCAPE_ZOOM_OUT: f32 = 1.2;
const WALL_EDIT_LANDSCAPE_SHIFT: f32 = 0.12;

/// What a pointer gesture in Edit Mode is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EditTool {
    /// Select, move, marquee, long-press insert.
    #[default]
    Select,
    Solid,
    Void,
    Split,
    /// Walls: tap to place a window / door preset void.
    Window,
    Door,
}

impl EditTool {
    fn name(self) -> &'static str {
        match self {
            EditTool::Select => "select",
            EditTool::Solid => "solid",
            EditTool::Void => "void",
            EditTool::Split => "split",
            EditTool::Window => "window",
            EditTool::Door => "door",
        }
    }
}

/// What Edit Mode is editing, which fixes its frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EditTarget {
    /// A floor plate: the profile lies on its construction plane.
    #[default]
    Plane,
    /// A wall: the profile is the wall's elevation (u along the wall from
    /// its start, v up from its base), seen in an elevation view.
    Wall(vim_design_lib::EntityId),
}

/// A session whose edits live only in memory (history kept here), until
/// the profile can be stored in the document.
#[derive(Debug, Clone, Default)]
pub struct MemoryHistory {
    undo: Vec<EditProfile>,
    redo: Vec<EditProfile>,
    last_key: Option<String>,
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
        self.gestures.begin_transaction(&self.doc);
        if let Some(plate) = legacy {
            let depth = self.doc.undo_depth();
            match ops::convert_legacy_plate(&mut self.doc, &plate) {
                Ok(_) => self.gestures.one_shot(depth),
                Err(e) => {
                    self.gestures.rollback_transaction(&mut self.doc);
                    self.sync("convert failed");
                    web_sys::console::error_1(&JsValue::from_str(&e));
                    return false;
                }
            }
            self.sync("convert plate");
            self.gestures.set_transaction_floor(&self.doc);
        }
        let Some(ElementModel::SketchPlate(p)) = self.model.iter().find(|e| e.element() == id).cloned() else {
            self.gestures.rollback_transaction(&mut self.doc);
            self.sync("edit failed");
            return false;
        };
        self.edit_sketch = Some(p.sketch_entity);
        self.edit_entry_element = Some(id);
        self.start_edit(EditSession::new(p.sketch.clone(), p.plane_level, Some(id), p.name.clone()));
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
        self.gestures.begin_transaction(&self.doc);
        self.edit_sketch = None;
        self.edit_entry_element = None;
        let mut session = EditSession::new(Profile::default(), plane, None, name);
        session.new_thickness = self.plate_thickness;
        self.start_edit(session);
        self.edit_set_tool("solid");
        true
    }

    /// Enter Edit Mode on a wall: an orthographic elevation facing it and
    /// its profile (u along the wall, v up) — the wall body a solid face,
    /// its windows through voids, the top corners anchored to the top.
    /// The session is kept in memory: the edits are not written to the
    /// wall until walls are profile-based in the document.
    pub fn edit_begin_wall(&mut self, wall: f64) -> bool {
        let id = eid(wall);
        if self.edit.is_some() {
            return false;
        }
        let Some(w) = self.wall(id).cloned() else { return false };
        let mut faces = vec![(w.profile.clone(), FaceKind::Solid { thickness: w.thickness })];
        faces.extend(
            w.windows.iter().filter(|h| h.outline.len() >= 3).map(|h| (h.outline.clone(), FaceKind::Void { depth: None })),
        );
        let Ok(profile) = crate::authoring::edit::profile::sketch_from_faces(&faces) else {
            return false;
        };
        let top: std::collections::BTreeSet<u32> = profile
            .points
            .iter()
            .filter(|p| (p.uv[1] - w.height).abs() < 1e-6)
            .map(|p| p.id)
            .collect();
        if self.prev_camera.is_none() {
            self.prev_camera = Some(self.camera.clone());
        }
        let (frame, length, height) = self.elevation_frame(&w);
        let (vw, vh) = self.size_f();
        self.camera.enter_elevation(frame, length, height, vw / vh);
        if vw > vh {
            self.camera.elevation_half_h *= WALL_EDIT_LANDSCAPE_ZOOM_OUT;
            self.camera.target += frame.u * (length * WALL_EDIT_LANDSCAPE_SHIFT);
        }
        self.edit_target = EditTarget::Wall(id);
        self.edit_memory = Some(MemoryHistory::default());
        let mut session = EditSession::new(profile, w.plane_level, Some(id), w.name.clone());
        session.top_points = top;
        session.new_thickness = w.thickness;
        self.edit_sketch = None;
        self.edit_entry_element = Some(id);
        self.start_edit(session);
        self.grid_key = None;
        true
    }

    /// Window / Door tool: place the preset opening centred where the
    /// wall was tapped.
    pub fn edit_place_preset(&mut self, px: f32, py: f32) -> String {
        let kind = match self.edit_tool {
            EditTool::Window => Opening::Window,
            EditTool::Door => Opening::Door,
            _ => return r#"{"result":"none"}"#.to_owned(),
        };
        let Some(uv) = self.edit_cursor_uv(px, py) else { return r#"{"result":"none"}"#.to_owned() };
        let edit = Edit::AddFace { outline: preset_outline(kind, uv[0]), kind: FaceKind::Void { depth: None } };
        self.edit_apply(&edit, None, "committed")
    }

    /// Anchor the selected points to the wall's top (they follow its
    /// height) or bottom. Returns how many points changed.
    pub fn edit_set_anchor(&mut self, top: bool) -> u32 {
        if !matches!(self.edit_target, EditTarget::Wall(_)) {
            return 0;
        }
        self.edit.as_mut().map_or(0, |s| s.set_anchor(top) as u32)
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
        if let Some(mem) = self.edit_memory.take() {
            // In-memory session: nothing is written to the document.
            self.leave_edit();
            return serde_json::json!({
                "result": "confirmed", "changed": !mem.undo.is_empty(), "name": session.name, "memory": true,
            })
            .to_string();
        }
        let empty = session.model.faces.is_empty();
        let mut deleted = false;
        if empty && self.edit_entry_element.is_none() {
            // A new plate whose faces were all removed: nothing happened.
            self.gestures.rollback_transaction(&mut self.doc);
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
        let changed = self.gestures.commit_transaction(&self.doc);
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
        if self.edit.is_some() && self.edit_memory.take().is_some() {
            self.edit = None;
            self.leave_edit();
            return;
        }
        if self.edit.take().is_some() {
            self.gestures.rollback_transaction(&mut self.doc);
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

    /// Edit tool: "select", "solid", "void", or "split" (the last three
    /// draw with the shared sketch tools).
    pub fn edit_set_tool(&mut self, tool: &str) -> bool {
        let Some(level) = self.edit.as_ref().map(|s| s.level) else {
            return false;
        };
        let wall = matches!(self.edit_target, EditTarget::Wall(_));
        self.edit_tool = match tool {
            "solid" => EditTool::Solid,
            "void" => EditTool::Void,
            "split" => EditTool::Split,
            "window" if wall => EditTool::Window,
            "door" if wall => EditTool::Door,
            _ => EditTool::Select,
        };
        self.sketch = match self.edit_tool {
            EditTool::Select => None,
            EditTool::Solid | EditTool::Void => Some(Sketch::new(SketchTool::Profile, self.shape, level)),
            EditTool::Split => Some(Sketch::new(SketchTool::Split, Shape::Polygon, level)),
            EditTool::Window | EditTool::Door => None,
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
        let (enabled, step) = (self.snap_enabled, self.snap_step);
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
        if let (Some(mem), Some(s)) = (self.edit_memory.as_mut(), self.edit.as_mut()) {
            let Some(prev) = mem.undo.pop() else { return false };
            mem.redo.push(std::mem::replace(&mut s.model, prev.clone()));
            mem.last_key = None;
            s.set_model(prev);
            return true;
        }
        if self.edit.is_none() || !self.gestures.undo(&mut self.doc) {
            return false;
        }
        self.sync("edit undo");
        self.reload_edit_model();
        true
    }

    pub fn edit_redo(&mut self) -> bool {
        if let (Some(mem), Some(s)) = (self.edit_memory.as_mut(), self.edit.as_mut()) {
            let Some(next) = mem.redo.pop() else { return false };
            mem.undo.push(std::mem::replace(&mut s.model, next.clone()));
            mem.last_key = None;
            s.set_model(next);
            return true;
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
        if solids.is_empty() {
            s.new_thickness = t;
            self.plate_thickness = t; // remembered for the next new plate
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
        if voids.is_empty() {
            s.new_void_depth = d;
            s.new_void_through = false;
            return r#"{"result":"default"}"#.to_owned();
        }
        self.edit_apply(&Edit::SetKind { faces: voids, kind: FaceKind::Void { depth: Some(d) } }, Some("depth"), "changed")
    }

    /// Voids cut all the way through (`true`) or to their depth.
    pub fn edit_set_through(&mut self, through: bool) -> String {
        let Some(s) = self.edit.as_mut() else { return r#"{"result":"none"}"#.to_owned() };
        let voids = selected_faces(s, true);
        if voids.is_empty() {
            s.new_void_through = through;
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
        let target = if kinds.is_empty() { "new" } else { "selection" };
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
            "target": if matches!(self.edit_target, EditTarget::Wall(_)) { "wall" } else { "floor" },
            "memory": self.edit_memory.is_some(),
            "anchor": {
                "selected": s.selected_points().len(),
                "top": s.selected_points().iter().filter(|id| s.top_points.contains(id)).count(),
            },
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
                    "top": s.top_points.contains(&p.id),
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
        serde_json::json!({
            "active": true,
            "mode": s.mode.name(),
            "faces": faces_json,
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
    pub(super) fn edit_can_undo(&self) -> bool {
        match &self.edit_memory {
            Some(m) => !m.undo.is_empty(),
            None => self.gestures.can_undo(),
        }
    }

    pub(super) fn edit_can_redo(&self) -> bool {
        match &self.edit_memory {
            Some(m) => !m.redo.is_empty(),
            None => self.gestures.can_redo(),
        }
    }

    fn start_edit(&mut self, session: EditSession<EditProfile>) {
        if self.window_host.is_some() {
            self.end_window();
        }
        self.selection = None;
        self.tool = Tool::Select;
        self.sketch = None;
        self.edit_tool = EditTool::Select;
        self.edit_pointer = EditPointer::default();
        self.edit = Some(session);
        self.refresh_styles();
    }

    fn leave_edit(&mut self) {
        if matches!(self.edit_target, EditTarget::Wall(_)) {
            if let Some(c) = self.prev_camera.take() {
                self.camera = c;
            }
            self.camera.elevation = None;
            self.camera.plane_z = self.active_elevation as f32;
            self.grid_key = None;
        }
        self.edit_target = EditTarget::Plane;
        self.edit_memory = None;
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
        let stored = self.edit_sketch.and_then(|id| model::sketch_params(&self.doc, id));
        let element = self.edit_sketch.and_then(|sk| {
            self.model.iter().find_map(|e| match e {
                ElementModel::SketchPlate(p) if p.sketch_entity == sk => Some(p.element),
                _ => None,
            })
        });
        if let Some(s) = self.edit.as_mut() {
            s.element = element.or(self.edit_entry_element);
            s.set_model(stored.map(|(sketch, _)| sketch).unwrap_or_default());
        }
        self.refresh_styles();
    }

    /// The plane the edited profile lives on.
    pub(super) fn edit_frame(&self) -> SketchFrame {
        if let Some((f, _, _)) = self.edit_wall_frame() {
            return SketchFrame { origin: f.origin, u: f.u, v: Vec3::Z };
        }
        let plane = self.edit.as_ref().map_or(vim_design_lib::EntityId::INVALID, |s| s.level);
        SketchFrame {
            origin: Vec3::new(0.0, 0.0, self.plane_elevation(plane) as f32),
            u: Vec3::X,
            v: Vec3::Y,
        }
    }

    /// The elevation frame of the wall being edited (wall sessions only).
    pub(super) fn edit_wall_frame(&self) -> Option<(super::ElevationFrame, f32, f32)> {
        let EditTarget::Wall(id) = self.edit_target else { return None };
        self.wall(id).map(|w| self.elevation_frame(w))
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
        if let Some(mem) = self.edit_memory.as_mut() {
            let continuing = key.is_some() && key == mem.last_key.as_deref();
            if !continuing {
                mem.undo.push(s.model.clone());
            }
            mem.redo.clear();
            mem.last_key = key.map(str::to_owned);
            if let Some(s) = self.edit.as_mut() {
                s.set_model(next);
            }
            return serde_json::json!({ "result": ok }).to_string();
        }
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
                    s.set_model(next);
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
        let out = self.edit_apply(&edit, None, "committed");
        if let Some(sk) = self.sketch.as_mut() {
            sk.points.clear();
        }
        out
    }
}

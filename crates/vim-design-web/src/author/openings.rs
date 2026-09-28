//! Openings mode: place, move, resize, and delete windows and doors on
//! any wall without editing the wall itself. Entered from the Window /
//! Door tool; ✓ / ✗ like every Edit Mode (a gestures session: each
//! change is one ordinary undo step, ✗ undoes back to the entry).
//!
//! Openings are the structured openings of the library's `WallRun`: tap
//! a wall (plan or 3D) to place the armed preset centred where it was
//! tapped along that segment; tap an opening to select it; drag it along
//! its segment (a window also up and down) on a 0.1 m grid, inside the
//! segment's clear span (`wall_run::segment_clear_span`: clear of the
//! corner joins). Each change is one `wall_run::ops` call and one
//! `UpdateWallRun` (a drag coalesces). A wall of the earlier tools is
//! converted to a run when it is first worked on.

use glam::Vec3;
use vim_design_lib::EntityId;
use vim_design_lib::wall_run::{Opening, OpeningKind, WallRunData, ops as run_ops, segment_clear_span};
use wasm_bindgen::prelude::*;

use super::camera::ViewMode;
use super::{AuthorApp, ElevationFrame, eid};
use crate::authoring::geom::{P2, point_in_polygon, point_segment_distance};
use crate::authoring::model::{ElementModel, WallLine, WallRunModel};
use crate::authoring::openings::{MIN_OPENING_SIZE_M, OPENING_SNAP_PER_M, openings_of};
use crate::authoring::ops;
use crate::authoring::runs::run_error;
use crate::authoring::walls::WINDOW_MARGIN_M;

/// An opening as the mode shows it: its segment line, visible rectangle,
/// kind, whether it is a niche, and its run opening id (`None`: a void
/// of a `Wall` not converted yet).
type ShownOpening = (WallLine, [P2; 2], OpeningKind, bool, Option<u32>);

/// A paste of openings on a run: the run, its new data, and the new
/// openings with their ids.
type PastedOpenings = (WallRunModel, WallRunData, Vec<(u32, Opening)>);

/// Openings mode state (session state).
#[derive(Debug, Clone)]
pub struct OpeningsSession {
    pub preset: OpeningKind,
    /// The selected opening: its run's element and its opening id.
    pub selected: Option<(EntityId, u32)>,
    pub hover: Option<(EntityId, u32)>,
    pub drag: Option<OpeningDrag>,
    /// The wall segment an elevation view faces (the last one worked on).
    pub faced: Option<(EntityId, u32)>,
    /// The opening just placed: while it is the selection, its size is
    /// also the remembered size of its kind.
    pub fresh: Option<(EntityId, u32)>,
    /// Rooms preview: the selected room wall opening (its id).
    pub room_selected: Option<u32>,
}

impl OpeningsSession {
    pub fn is_fresh(&self) -> bool {
        self.fresh.is_some() && self.selected == self.fresh
    }
}

/// A move of an opening: where the pointer grabbed it (segment-local u
/// along, v up) and the opening as it was.
#[derive(Debug, Clone, Copy)]
pub struct OpeningDrag {
    pub element: EntityId,
    pub grab: P2,
    pub orig: Opening,
}

fn none() -> String {
    r#"{"result":"none"}"#.to_owned()
}

fn rejected(reason: &str) -> String {
    serde_json::json!({ "result": "rejected", "reason": reason }).to_string()
}

fn snap(v: f64) -> f64 {
    (v * OPENING_SNAP_PER_M).round() / OPENING_SNAP_PER_M
}

fn kind_name(k: OpeningKind) -> &'static str {
    match k {
        OpeningKind::Window => "window",
        OpeningKind::Door => "door",
    }
}

/// An opening's visible rectangle, segment-local: (u0, v0), (u1, v1) — a
/// door from the base up.
fn visible(o: &Opening) -> [P2; 2] {
    let v0 = match o.kind {
        OpeningKind::Window => o.sill_m,
        OpeningKind::Door => 0.0,
    };
    [[o.offset_m, v0], [o.offset_m + o.width_m, v0 + o.height_m]]
}

/// Convex hull of screen points (monotone chain), counter-clockwise.
fn hull(mut pts: Vec<P2>) -> Vec<P2> {
    pts.sort_by(|a, b| a[0].total_cmp(&b[0]).then(a[1].total_cmp(&b[1])));
    pts.dedup();
    if pts.len() < 3 {
        return pts;
    }
    let cross = |o: P2, a: P2, b: P2| (a[0] - o[0]) * (b[1] - o[1]) - (a[1] - o[1]) * (b[0] - o[0]);
    let mut lower: Vec<P2> = Vec::new();
    for p in &pts {
        while lower.len() >= 2 && cross(lower[lower.len() - 2], lower[lower.len() - 1], *p) <= 0.0 {
            lower.pop();
        }
        lower.push(*p);
    }
    let mut upper: Vec<P2> = Vec::new();
    for p in pts.iter().rev() {
        while upper.len() >= 2 && cross(upper[upper.len() - 2], upper[upper.len() - 1], *p) <= 0.0 {
            upper.pop();
        }
        upper.push(*p);
    }
    lower.pop();
    upper.pop();
    lower.extend(upper);
    lower
}

#[wasm_bindgen]
impl AuthorApp {
    /// Enter Openings mode (the Window / Door tool): `preset` is "window"
    /// or "door".
    pub fn openings_begin(&mut self, preset: &str) -> bool {
        if self.edit.is_some() || self.openings.is_some() || !self.can_author() {
            return false;
        }
        self.gestures.begin_session(&self.doc);
        self.sketch = None;
        self.selection = None;
        self.paste_armed = false; // a paste belongs to the mode it was armed in
        self.openings = Some(OpeningsSession {
            preset: if preset == "door" { OpeningKind::Door } else { OpeningKind::Window },
            selected: None,
            hover: None,
            drag: None,
            faced: None,
            fresh: None,
            room_selected: None,
        });
        self.begin_room_session();
        self.refresh_styles();
        true
    }

    pub fn openings_active(&self) -> bool {
        self.openings.is_some()
    }

    /// The preset a tap on a wall places: "window" or "door".
    pub fn openings_set_preset(&mut self, preset: &str) {
        if let Some(o) = self.openings.as_mut() {
            o.preset = if preset == "door" { OpeningKind::Door } else { OpeningKind::Window };
        }
    }

    /// ✓: keep the session's changes (each stays one undo step).
    pub fn openings_confirm(&mut self) -> bool {
        if self.openings.is_none() {
            return false;
        }
        let changed = self.gestures.end_session(&self.doc);
        let rooms_changed = self.end_room_session(true);
        self.leave_openings();
        self.sync("openings");
        changed || rooms_changed
    }

    /// ✗: undo back to the entry.
    pub fn openings_cancel(&mut self) {
        if self.openings.is_some() {
            self.gestures.cancel_session(&mut self.doc);
            self.end_room_session(false);
            self.leave_openings();
            self.sync("openings cancelled");
        }
    }

    /// Hover (mouse): highlight the opening under the pointer.
    pub fn openings_hover(&mut self, px: f32, py: f32, tol: f32) {
        let hit = self.opening_at(px, py, tol);
        if let Some(o) = self.openings.as_mut() {
            o.hover = hit;
        }
    }

    /// Tap: select the opening under the pointer, or place the preset on
    /// the wall under it (centred on the tapped point along the segment).
    /// A wall of the earlier tools is converted to a run first. JSON
    /// `{"result": "selected" | "placed" | "cleared" | "rejected", ...}`.
    pub fn openings_tap(&mut self, px: f32, py: f32, tol: f32, wall_tol: f32) -> String {
        if self.openings.is_none() {
            return none();
        }
        if let Some(hit) = self.opening_at(px, py, tol) {
            self.select_opening(hit);
            return r#"{"result":"selected"}"#.to_owned();
        }
        // Rooms preview: an opening in a room wall, or a room wall.
        if let Some(id) = self.room_opening_at(px, py, tol) {
            self.select_room_opening(Some(id));
            return r#"{"result":"selected"}"#.to_owned();
        }
        let wall = self.pick_wall(px, py, wall_tol);
        if wall < 0.0 {
            let preset = self.openings.as_ref().map_or(OpeningKind::Window, |o| o.preset);
            match self.place_room_opening(preset, px, py, wall_tol) {
                Some(Ok(id)) => {
                    self.select_room_opening(Some(id));
                    return serde_json::json!({ "result": "placed", "kind": kind_name(preset), "room": true }).to_string();
                }
                Some(Err(e)) => return rejected(&e),
                None => {}
            }
            if let Some(o) = self.openings.as_mut() {
                o.selected = None;
                o.room_selected = None;
            }
            return r#"{"result":"cleared"}"#.to_owned();
        }
        let element = match self.ensure_run(eid(wall)) {
            Ok(e) => e,
            Err(e) => return rejected(&e),
        };
        // An opening of a converted wall may be what was tapped.
        if let Some(hit) = self.opening_at(px, py, tol) {
            self.select_opening(hit);
            return r#"{"result":"selected"}"#.to_owned();
        }
        let Some((segment, uv)) = self.segment_at(element, px, py) else { return none() };
        let Some(preset) = self.openings.as_ref().map(|o| o.preset) else { return none() };
        // The remembered size of the kind (the last one the user chose).
        let size = self.remembered.opening(preset);
        let opening = Opening {
            id: 0,
            segment,
            offset_m: uv[0] - size.width / 2.0,
            sill_m: size.sill,
            width_m: size.width,
            height_m: size.height,
            kind: preset,
            depth_m: size.depth,
        };
        let Some(r) = self.run_model(element).cloned() else { return none() };
        let opening = match fitted(&r, opening) {
            Ok(o) => o,
            Err(e) => return rejected(&e),
        };
        match run_ops::add_opening(&r.data, opening) {
            Ok((data, id)) => {
                self.store_run(&r, &data, None);
                self.select_opening((element, id));
                if let Some(o) = self.openings.as_mut() {
                    o.fresh = Some((element, id));
                }
                serde_json::json!({ "result": "placed", "kind": kind_name(preset) }).to_string()
            }
            Err(e) => rejected(run_error(e).message()),
        }
    }

    /// A press became a drag: on an opening it starts moving it (true);
    /// elsewhere the page navigates (false).
    pub fn openings_drag_begin(&mut self, px: f32, py: f32, tol: f32) -> bool {
        let Some((element, id)) = self.opening_at(px, py, tol) else { return false };
        let Some(orig) = self.run_opening(element, id) else { return false };
        let Some(grab) = self.segment_uv_at(element, orig.segment, px, py) else { return false };
        self.select_opening((element, id));
        if let Some(o) = self.openings.as_mut() {
            o.drag = Some(OpeningDrag { element, grab, orig });
        }
        true
    }

    /// Drag update: the opening follows the pointer along its segment (a
    /// window also vertically), snapped and kept in the clear span.
    pub fn openings_drag_move(&mut self, px: f32, py: f32) -> String {
        let Some(d) = self.openings.as_ref().and_then(|o| o.drag) else { return none() };
        let Some(uv) = self.segment_uv_at(d.element, d.orig.segment, px, py) else { return none() };
        let Some(r) = self.run_model(d.element).cloned() else { return none() };
        let plan = self.camera.mode == ViewMode::Plan;
        let mut next = d.orig;
        next.offset_m = d.orig.offset_m + uv[0] - d.grab[0];
        if !plan {
            next.sill_m = d.orig.sill_m + uv[1] - d.grab[1];
        }
        let next = match fitted(&r, next) {
            Ok(o) => o,
            Err(e) => return rejected(&e),
        };
        if r.data.openings.contains(&next) {
            return r#"{"result":"none"}"#.to_owned(); // snapped to where it is
        }
        let key = format!("opening_move_{}_{}", d.element.0, d.orig.id);
        match run_ops::set_opening(&r.data, next) {
            Ok(data) => {
                self.store_run(&r, &data, Some(&key));
                r#"{"result":"moved"}"#.to_owned()
            }
            Err(e) => rejected(run_error(e).message()),
        }
    }

    pub fn openings_drag_end(&mut self) {
        if let Some(o) = self.openings.as_mut() {
            o.drag = None;
        }
        self.gestures.end();
    }

    /// Change the selected opening: width (about its centre), height,
    /// sill (NaN keeps a value), depth (NaN keeps it, a negative value
    /// makes it go through, a positive one a niche that deep). Typing
    /// coalesces until [`AuthorApp::end_gesture`].
    pub fn openings_set(&mut self, width: f64, height: f64, sill: f64, depth: f64) -> String {
        if let Some(id) = self.openings.as_ref().and_then(|o| o.room_selected) {
            return match self.set_room_opening(id, width, height, sill, depth) {
                Ok(_) => r#"{"result":"changed"}"#.to_owned(),
                Err(e) => rejected(&e),
            };
        }
        let Some((element, id)) = self.openings.as_ref().and_then(|o| o.selected) else { return none() };
        let (Some(r), Some(current)) = (self.run_model(element).cloned(), self.run_opening(element, id)) else {
            return none();
        };
        let mut next = current;
        if width.is_finite() {
            next.offset_m = current.offset_m + (current.width_m - width) / 2.0;
            next.width_m = width;
        }
        if height.is_finite() {
            next.height_m = height;
        }
        if sill.is_finite() {
            next.sill_m = sill;
        }
        if depth.is_finite() {
            next.depth_m = (depth > 0.0).then_some(depth);
        }
        let next = match fitted(&r, next) {
            Ok(o) => o,
            Err(e) => return rejected(&e),
        };
        let key = format!("opening_set_{}_{}", element.0, id);
        match run_ops::set_opening(&r.data, next) {
            Ok(data) => {
                if self.openings.as_ref().is_some_and(OpeningsSession::is_fresh) {
                    self.remembered.remember_opening(&next);
                }
                if let Some(d) = next.depth_m {
                    self.remembered.niche_depth = d; // any niche depth chosen
                }
                self.store_run(&r, &data, Some(&key));
                r#"{"result":"changed"}"#.to_owned()
            }
            Err(e) => rejected(run_error(e).message()),
        }
    }

    /// Delete the selected opening (one undo step).
    pub fn openings_delete(&mut self) -> bool {
        if let Some(id) = self.openings.as_ref().and_then(|o| o.room_selected) {
            let ok = self.delete_room_opening(id);
            self.select_room_opening(None);
            return ok;
        }
        let Some((element, id)) = self.openings.as_ref().and_then(|o| o.selected) else { return false };
        let Some(r) = self.run_model(element).cloned() else { return false };
        let Ok(data) = run_ops::delete_opening(&r.data, id) else { return false };
        self.store_run(&r, &data, None);
        if let Some(o) = self.openings.as_mut() {
            o.selected = None;
        }
        true
    }

    /// Openings mode state for the page.
    pub fn openings_state_json(&self) -> String {
        let Some(o) = &self.openings else { return r#"{"active":false}"#.to_owned() };
        let room_selected = o.room_selected.and_then(|id| self.room_opening_json(id));
        let selected = room_selected.or_else(|| o.selected.and_then(|(element, id)| {
            let r = self.run_model(element)?;
            let op = self.run_opening(element, id)?;
            let span = segment_clear_span(&r.data, op.segment).ok();
            Some(serde_json::json!({
                "wall": element.0 as f64, "wallName": r.name, "id": id, "segment": op.segment, "kind": kind_name(op.kind),
                "offset": op.offset_m, "sill": op.sill_m, "width": op.width_m, "height": op.height_m,
                "depth": op.depth_m, "wallHeight": r.top_height, "wallThickness": r.data.thickness_m,
                "span": span.map(|(a, b)| [a, b]),
            }))
        }));
        serde_json::json!({
            "active": true,
            "preset": kind_name(o.preset),
            "selected": selected,
            "count": self.opening_count() + self.rooms.openings.len(),
            "canUndo": self.gestures.can_undo() || self.rooms_can_undo(),
            "canRedo": self.gestures.can_redo() || self.rooms_can_redo(),
            "faced": o.faced.map(|(f, _)| f.0 as f64),
            "fresh": o.is_fresh(),
            "nicheDepth": self.remembered.niche_depth,
            "presetSize": {
                "window": self.remembered.opening(OpeningKind::Window).to_json(),
                "door": self.remembered.opening(OpeningKind::Door).to_json(),
            },
        })
        .to_string()
    }

    /// Every opening on every wall, outlined in screen space (device
    /// pixels), with selection and hover. Head-on to a segment, only its
    /// openings (the others would be edge-on slivers).
    pub fn openings_hud_json(&self) -> String {
        let Some(o) = &self.openings else { return r#"{"active":false}"#.to_owned() };
        let only = (self.camera.mode == ViewMode::Elevation).then_some(o.faced).flatten();
        let mut items = Vec::new();
        for (line, rect, kind, niche, id) in self.all_openings() {
            if only.is_some_and(|(e, s)| Some((e, s)) != line.segment.map(|seg| (line.element, seg))) {
                continue;
            }
            let Some(pts) = self.opening_screen(&line, rect) else { continue };
            let key = id.map(|i| (line.element, i));
            items.push(serde_json::json!({
                "wall": line.element.0 as f64, "id": id, "kind": kind_name(kind), "pts": pts,
                "sel": key.is_some() && o.selected == key, "hover": key.is_some() && o.hover == key, "niche": niche,
            }));
        }
        serde_json::json!({ "active": true, "items": items }).to_string()
    }

    /// Every wall run opening (test hook): wall, id, segment, kind, and
    /// its rectangle.
    pub fn openings_json(&self) -> String {
        let mut out = Vec::new();
        for e in &self.model {
            let ElementModel::Run(r) = e else { continue };
            for o in &r.data.openings {
                out.push(serde_json::json!({
                    "wall": r.element.0 as f64, "id": o.id, "segment": o.segment, "kind": kind_name(o.kind),
                    "offset": o.offset_m, "sill": o.sill_m, "width": o.width_m, "height": o.height_m, "depth": o.depth_m,
                    "span": segment_clear_span(&r.data, o.segment).ok().map(|(a, b)| [a, b]),
                }));
            }
        }
        serde_json::Value::Array(out).to_string()
    }
}

/// An opening snapped to the grid and kept on its segment: inside the
/// clear span, a window above the base and under the top (by the window
/// margin), a door on the base. Refused when it cannot fit.
fn fitted(r: &WallRunModel, o: Opening) -> Result<Opening, String> {
    let (lo, hi) = segment_clear_span(&r.data, o.segment).map_err(|e| run_error(e).message().to_owned())?;
    let mut o = o;
    o.width_m = snap(o.width_m.max(MIN_OPENING_SIZE_M));
    o.height_m = snap(o.height_m.max(MIN_OPENING_SIZE_M));
    if o.width_m > hi - lo + 1e-9 {
        return Err("This wall segment is too short for the opening (between its corner joins)".to_owned());
    }
    // Snapped on the grid, then kept inside the span (its ends may be off
    // the grid).
    o.offset_m = snap(o.offset_m).clamp(lo, hi - o.width_m);
    let top = r.top_height - WINDOW_MARGIN_M;
    match o.kind {
        OpeningKind::Door => o.sill_m = 0.0,
        OpeningKind::Window => {
            o.sill_m = snap(o.sill_m).max(0.0);
            if o.sill_m + o.height_m > top {
                o.sill_m = (top - o.height_m).max(0.0);
            }
        }
    }
    if o.sill_m + o.height_m > top + 1e-9 {
        return Err("The wall is not tall enough for this opening".to_owned());
    }
    Ok(o)
}

impl AuthorApp {
    /// Paste openings on the wall under a canvas point, laid out around
    /// the point along the segment (each kept inside the clear span): one
    /// undo step (a wall of the earlier tools is converted first); the
    /// first is selected.
    pub(super) fn paste_openings(&mut self, copies: &[Opening], px: f32, py: f32, wall_tol: f32) -> Result<(), String> {
        let wall = self.pick_wall(px, py, wall_tol);
        if wall < 0.0 {
            return Err("Tap a wall to paste the openings".to_owned());
        }
        let element = self.ensure_run(eid(wall))?;
        let (r, data, placed) = self.pasted_openings(copies, element, px, py)?;
        self.store_run(&r, &data, None);
        if let Some((id, _)) = placed.first() {
            self.select_opening((element, *id));
        }
        Ok(())
    }

    /// The paste preview on the wall run under a canvas point: the
    /// openings' outlines on screen (`None`: no run there, or they do not
    /// fit).
    pub(super) fn openings_preview(&self, copies: &[Opening], px: f32, py: f32, wall_tol: f32) -> Option<Vec<Vec<[f32; 2]>>> {
        let element = eid(self.pick_wall(px, py, wall_tol));
        let (r, _, placed) = self.pasted_openings(copies, element, px, py).ok()?;
        let segment = placed.first()?.1.segment;
        let line = self.segment_line(r.element, segment)?;
        Some(placed.iter().filter_map(|(_, o)| self.opening_screen(&line, visible(o))).collect())
    }

    /// A run with pasted openings added: (the run, its new data, the new
    /// openings with their ids).
    fn pasted_openings(
        &self,
        copies: &[Opening],
        element: EntityId,
        px: f32,
        py: f32,
    ) -> Result<PastedOpenings, String> {
        let tap = "Tap a wall to paste the openings";
        let r = self.run_model(element).cloned().ok_or(tap)?;
        let (segment, uv) = self.segment_at(element, px, py).ok_or(tap)?;
        let mut data = r.data.clone();
        let mut placed = Vec::new();
        for o in crate::authoring::clipboard::laid_out(copies, segment, uv[0]) {
            let o = fitted(&r, o)?;
            let (next, id) = run_ops::add_opening(&data, o).map_err(|e| run_error(e).message().to_owned())?;
            data = next;
            placed.push((id, o));
        }
        Ok((r, data, placed))
    }

    fn leave_openings(&mut self) {
        self.openings = None;
        self.paste_armed = false;
        if self.camera.mode == ViewMode::Elevation {
            if let Some(c) = self.prev_camera.take() {
                self.camera = c;
            } else {
                self.camera.mode = ViewMode::Plan;
            }
            self.camera.elevation = None;
            self.grid_key = None;
        }
        self.refresh_styles();
    }

    /// Select a room wall opening (the run opening selection clears).
    fn select_room_opening(&mut self, id: Option<u32>) {
        if let Some(o) = self.openings.as_mut() {
            o.room_selected = id;
            o.selected = None;
        }
    }

    fn select_opening(&mut self, hit: (EntityId, u32)) {
        let segment = self.run_opening(hit.0, hit.1).map(|o| o.segment);
        if let Some(o) = self.openings.as_mut() {
            o.room_selected = None;
            o.selected = Some(hit);
            if let Some(s) = segment {
                o.faced = Some((hit.0, s));
            }
        }
    }

    fn run_opening(&self, element: EntityId, id: u32) -> Option<Opening> {
        self.run_model(element)?.data.openings.iter().find(|o| o.id == id).copied()
    }

    fn opening_count(&self) -> usize {
        self.all_openings().len()
    }

    /// Store a run's new data: one `UpdateWallRun`, one undo step (or
    /// part of the open gesture `key`).
    fn store_run(&mut self, r: &WallRunModel, data: &WallRunData, key: Option<&str>) {
        let depth = self.doc.undo_depth();
        if let Some(k) = key {
            self.gestures.begin_continuing(&self.doc, k);
        }
        self.submit(ops::update_run(r.run, data, false));
        if key.is_none() {
            self.gestures.one_shot(depth);
        }
        self.sync("opening");
    }

    /// The segment line of a run.
    fn segment_line(&self, element: EntityId, segment: u32) -> Option<WallLine> {
        self.run_model(element)?.lines().into_iter().find(|l| l.segment == Some(segment))
    }

    /// Every opening shown in the mode: the runs' openings (with ids) and,
    /// read-only until converted, the rectangular voids of `Wall`s.
    fn all_openings(&self) -> Vec<ShownOpening> {
        let mut out = Vec::new();
        for e in &self.model {
            match e {
                ElementModel::Run(r) => {
                    for o in &r.data.openings {
                        if let Some(line) = self.segment_line(r.element, o.segment) {
                            out.push((line, visible(o), o.kind, o.depth_m.is_some(), Some(o.id)));
                        }
                    }
                }
                ElementModel::Wall(w) => {
                    let Some(line) = e.wall_line() else { continue };
                    for o in openings_of(&w.effective()) {
                        let kind = match o.kind {
                            crate::authoring::openings::OpeningKind::Door => OpeningKind::Door,
                            crate::authoring::openings::OpeningKind::Window => OpeningKind::Window,
                        };
                        out.push((line, o.visible(), kind, o.depth.is_some(), None));
                    }
                }
                _ => {}
            }
        }
        out
    }

    /// The elevation frame facing the segment Openings mode works on.
    pub(super) fn openings_faced_frame(&self) -> Option<(ElevationFrame, f32, f32)> {
        let (element, segment) = self.openings.as_ref()?.faced?;
        self.segment_line(element, segment).map(|w| self.elevation_frame(&w))
    }

    /// World point of a segment-local (u, v) on the reference face, `into`
    /// meters into the wall.
    fn line_world(&self, w: &WallLine, u: f64, v: f64, into: f64) -> Vec3 {
        let d = w.dir();
        let z = self.plane_elevation(w.plane) + w.base_w + v;
        Vec3::new(
            (w.start[0] + d[0] * u + w.normal[0] * into) as f32,
            (w.start[1] + d[1] * u + w.normal[1] * into) as f32,
            z as f32,
        )
    }

    /// An opening's outline on screen: the convex hull of its box through
    /// the wall (a line in plan, a rectangle head-on).
    fn opening_screen(&self, w: &WallLine, rect: [P2; 2]) -> Option<Vec<[f32; 2]>> {
        let (vw, vh) = self.size_f();
        let [[u0, v0], [u1, v1]] = rect;
        let mut pts = Vec::with_capacity(8);
        for (u, v) in [(u0, v0), (u1, v0), (u1, v1), (u0, v1)] {
            for into in [0.0, w.thickness] {
                let (x, y) = self.camera.project(self.line_world(w, u, v, into), vw, vh)?;
                pts.push([f64::from(x), f64::from(y)]);
            }
        }
        Some(hull(pts).iter().map(|p| [p[0] as f32, p[1] as f32]).collect())
    }

    /// The opening under a canvas point (within `tol` device pixels); a
    /// `Wall`'s void counts too, so tapping it converts the wall.
    fn opening_at(&self, px: f32, py: f32, tol: f32) -> Option<(EntityId, u32)> {
        let p = [f64::from(px), f64::from(py)];
        let only = self.openings.as_ref().and_then(|o| o.faced).filter(|_| self.camera.mode == ViewMode::Elevation);
        let mut best: Option<(f64, (EntityId, u32))> = None;
        for (line, rect, _, _, id) in self.all_openings() {
            let Some(id) = id else { continue };
            if only.is_some_and(|(e, s)| Some((e, s)) != line.segment.map(|seg| (line.element, seg))) {
                continue;
            }
            let Some(poly) = self.opening_screen(&line, rect) else { continue };
            let poly: Vec<P2> = poly.iter().map(|q| [f64::from(q[0]), f64::from(q[1])]).collect();
            let n = poly.len();
            let d = if n >= 3 && point_in_polygon(p, &poly) {
                0.0
            } else {
                (0..n).map(|i| point_segment_distance(p, poly[i], poly[(i + 1) % n])).fold(f64::INFINITY, f64::min)
            };
            if d <= f64::from(tol) && best.is_none_or(|(bd, _)| d < bd) {
                best = Some((d, (line.element, id)));
            }
        }
        best.map(|(_, hit)| hit)
    }

    /// Where a canvas point is on a segment, segment-local (u along it, v
    /// up): on its vertical plane when the view looks at it, else (plan)
    /// on its base plane, projected onto its line (v = 0).
    fn segment_uv_at(&self, element: EntityId, segment: u32, px: f32, py: f32) -> Option<P2> {
        let w = self.segment_line(element, segment)?;
        self.line_uv_at(&w, px, py)
    }

    fn line_uv_at(&self, w: &WallLine, px: f32, py: f32) -> Option<P2> {
        let (vw, vh) = self.size_f();
        let origin = self.line_world(w, 0.0, 0.0, 0.0);
        let d = w.dir();
        let u_axis = Vec3::new(d[0] as f32, d[1] as f32, 0.0);
        let normal = Vec3::Z.cross(u_axis);
        let local = |hit: Vec3| {
            let r = hit - origin;
            [f64::from(r.dot(u_axis)), f64::from(r.z)]
        };
        if self.camera.mode != ViewMode::Plan
            && let Some(hit) = self.camera.unproject_to(px, py, vw, vh, origin, normal)
        {
            return Some(local(hit));
        }
        let hit = self.camera.unproject_to(px, py, vw, vh, origin, Vec3::Z)?;
        Some([local(hit)[0], 0.0])
    }

    /// The run segment nearest a canvas point, and the point on it.
    fn segment_at(&self, element: EntityId, px: f32, py: f32) -> Option<(u32, P2)> {
        let r = self.run_model(element)?;
        let (vw, vh) = self.size_f();
        let p = [f64::from(px), f64::from(py)];
        let mut best: Option<(f64, u32, P2)> = None;
        for line in r.lines() {
            let Some(uv) = self.line_uv_at(&line, px, py) else { continue };
            let u = uv[0].clamp(0.0, line.length());
            // Screen distance from the pointer to the segment's base line.
            let (Some(a), Some(b)) = (
                self.camera.project(self.line_world(&line, 0.0, 0.0, 0.0), vw, vh),
                self.camera.project(self.line_world(&line, line.length(), 0.0, 0.0), vw, vh),
            ) else {
                continue;
            };
            let da = point_segment_distance(p, [f64::from(a.0), f64::from(a.1)], [f64::from(b.0), f64::from(b.1)]);
            let (Some(c), Some(e)) = (
                self.camera.project(self.line_world(&line, 0.0, line.height, 0.0), vw, vh),
                self.camera.project(self.line_world(&line, line.length(), line.height, 0.0), vw, vh),
            ) else {
                continue;
            };
            let dt = point_segment_distance(p, [f64::from(c.0), f64::from(c.1)], [f64::from(e.0), f64::from(e.1)]);
            // Seen from the side, a point inside the face counts as 0; in
            // plan the base line is the wall.
            let inside = self.camera.mode != ViewMode::Plan
                && (0.0..=line.height).contains(&uv[1])
                && (0.0..=line.length()).contains(&uv[0]);
            let d = if inside { 0.0 } else if self.camera.mode == ViewMode::Plan { da } else { da.min(dt) };
            if let Some(seg) = line.segment
                && best.is_none_or(|(bd, ..)| d < bd)
            {
                best = Some((d, seg, [u, uv[1]]));
            }
        }
        best.map(|(_, s, uv)| (s, uv))
    }
}

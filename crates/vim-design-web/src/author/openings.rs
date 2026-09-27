//! Openings mode: place, move, resize, and delete windows and doors on
//! any wall without editing the wall itself. Entered from the Window /
//! Door tool; ✓ / ✗ like every Edit Mode (a gestures session: each
//! change is one ordinary undo step, ✗ undoes back to the entry).
//!
//! Tap a wall (plan or 3D) to place the armed preset centred where it
//! was tapped; tap an opening to select it; drag it along the wall
//! (windows also up and down) on a 0.1 m grid, clear of the wall's ends.
//! Openings are the adapter's rectangles (`authoring::openings`), stored
//! as void faces of the wall's profile: each change is one `UpdateWall`.
//! A legacy wall is converted when an opening is first placed in it.

use glam::Vec3;
use vim_design_lib::sketch::SketchFaceKind;
use vim_design_lib::{Command, EntityId};
use wasm_bindgen::prelude::*;

use super::camera::ViewMode;
use super::walls::update_wall;
use super::{AuthorApp, ElevationFrame, eid};
use crate::authoring::geom::{P2, point_in_polygon, point_segment_distance};
use crate::authoring::model::{ElementModel, WallModel};
use crate::authoring::openings::{OpeningKind, OpeningRect, WallSpan, fit, openings_of};

/// Openings mode state (session state).
#[derive(Debug, Clone)]
pub struct OpeningsSession {
    pub preset: OpeningKind,
    /// The selected opening: its wall's element and its void face.
    pub selected: Option<(EntityId, u32)>,
    pub hover: Option<(EntityId, u32)>,
    pub drag: Option<OpeningDrag>,
    /// The wall an elevation view faces (the last one worked on).
    pub faced: Option<EntityId>,
}

/// A move of an opening: where the pointer grabbed it (wall-local) and
/// the opening as it was.
#[derive(Debug, Clone, Copy)]
pub struct OpeningDrag {
    pub element: EntityId,
    pub grab: P2,
    pub orig: OpeningRect,
}

fn none() -> String {
    r#"{"result":"none"}"#.to_owned()
}

fn rejected(reason: &str) -> String {
    serde_json::json!({ "result": "rejected", "reason": reason }).to_string()
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
        self.openings = Some(OpeningsSession {
            preset: if preset == "door" { OpeningKind::Door } else { OpeningKind::Window },
            selected: None,
            hover: None,
            drag: None,
            faced: None,
        });
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
        self.leave_openings();
        self.sync("openings");
        changed
    }

    /// ✗: undo back to the entry.
    pub fn openings_cancel(&mut self) {
        if self.openings.is_some() {
            self.gestures.cancel_session(&mut self.doc);
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
    /// the wall under it (centred on the tapped point along the wall).
    /// JSON `{"result": "selected" | "placed" | "cleared" | "rejected", ...}`.
    pub fn openings_tap(&mut self, px: f32, py: f32, tol: f32, wall_tol: f32) -> String {
        if self.openings.is_none() {
            return none();
        }
        if let Some(hit) = self.opening_at(px, py, tol) {
            if let Some(o) = self.openings.as_mut() {
                o.selected = Some(hit);
                o.faced = Some(hit.0);
            }
            return r#"{"result":"selected"}"#.to_owned();
        }
        let wall = self.pick_wall(px, py, wall_tol);
        if wall < 0.0 {
            if let Some(o) = self.openings.as_mut() {
                o.selected = None;
            }
            return r#"{"result":"cleared"}"#.to_owned();
        }
        let element = eid(wall);
        if let Some(ElementModel::LegacyWall(w)) = self.model.iter().find(|e| e.element() == element) {
            let level = w.plane_level;
            if let Err(e) = self.convert_legacy_walls(level) {
                return rejected(&e);
            }
        }
        let Some(u) = self.wall_u_at(element, px, py) else { return none() };
        let Some(preset) = self.openings.as_ref().map(|o| o.preset) else { return none() };
        match self.write_opening(element, None, Some(OpeningRect::preset(preset, u)), None) {
            Ok(face) => {
                if let Some(o) = self.openings.as_mut() {
                    o.selected = face.map(|f| (element, f));
                    o.faced = Some(element);
                }
                serde_json::json!({ "result": "placed", "kind": preset.name() }).to_string()
            }
            Err(e) => rejected(&e),
        }
    }

    /// A press became a drag: on an opening it starts moving it (true);
    /// elsewhere the page navigates (false).
    pub fn openings_drag_begin(&mut self, px: f32, py: f32, tol: f32) -> bool {
        let Some((element, face)) = self.opening_at(px, py, tol) else { return false };
        let (Some(grab), Some(orig)) = (self.wall_uv_at(element, px, py), self.opening(element, face)) else {
            return false;
        };
        if let Some(o) = self.openings.as_mut() {
            o.selected = Some((element, face));
            o.faced = Some(element);
            o.drag = Some(OpeningDrag { element, grab, orig });
        }
        true
    }

    /// Drag update: the opening follows the pointer along its wall (a
    /// window also vertically), snapped and kept on the wall.
    pub fn openings_drag_move(&mut self, px: f32, py: f32) -> String {
        let Some(d) = self.openings.as_ref().and_then(|o| o.drag) else { return none() };
        let Some(uv) = self.wall_uv_at(d.element, px, py) else { return none() };
        let Some(current) = self.opening(d.element, d.orig.face) else { return none() };
        let plan = self.camera.mode == ViewMode::Plan;
        let mut next = d.orig;
        next.offset = d.orig.offset + uv[0] - d.grab[0];
        if !plan {
            next.sill = d.orig.sill + uv[1] - d.grab[1];
        }
        let key = format!("opening_move_{}_{}", d.element.0, d.orig.face);
        match self.write_opening(d.element, Some(current), Some(next), Some(&key)) {
            Ok(_) => r#"{"result":"moved"}"#.to_owned(),
            Err(e) => rejected(&e),
        }
    }

    pub fn openings_drag_end(&mut self) {
        if let Some(o) = self.openings.as_mut() {
            o.drag = None;
        }
        self.gestures.end();
    }

    /// Change the selected opening: width, height, sill (NaN keeps a
    /// value), depth (NaN keeps it, a negative value makes it go through,
    /// a positive one a niche that deep). Typing coalesces until
    /// [`AuthorApp::end_gesture`].
    pub fn openings_set(&mut self, width: f64, height: f64, sill: f64, depth: f64) -> String {
        let Some((element, face)) = self.openings.as_ref().and_then(|o| o.selected) else { return none() };
        let Some(current) = self.opening(element, face) else { return none() };
        let mut next = current;
        if width.is_finite() {
            // Resized about its centre.
            next.offset = current.offset + (current.width - width) / 2.0;
            next.width = width;
        }
        if height.is_finite() {
            next.height = height;
        }
        if sill.is_finite() {
            next.sill = sill;
        }
        if depth.is_finite() {
            next.depth = (depth > 0.0).then_some(depth);
        }
        let key = format!("opening_set_{}_{}", element.0, face);
        match self.write_opening(element, Some(current), Some(next), Some(&key)) {
            Ok(_) => r#"{"result":"changed"}"#.to_owned(),
            Err(e) => rejected(&e),
        }
    }

    /// Delete the selected opening (one undo step).
    pub fn openings_delete(&mut self) -> bool {
        let Some((element, face)) = self.openings.as_ref().and_then(|o| o.selected) else { return false };
        let Some(current) = self.opening(element, face) else { return false };
        let ok = self.write_opening(element, Some(current), None, None).is_ok();
        if ok && let Some(o) = self.openings.as_mut() {
            o.selected = None;
        }
        ok
    }

    /// Openings mode state for the page.
    pub fn openings_state_json(&self) -> String {
        let Some(o) = &self.openings else { return r#"{"active":false}"#.to_owned() };
        let selected = o.selected.and_then(|(element, face)| {
            let w = self.wall(element)?;
            let r = self.opening(element, face)?;
            Some(serde_json::json!({
                "wall": element.0 as f64, "wallName": w.name, "face": face, "kind": r.kind.name(),
                "offset": r.offset, "sill": r.sill, "width": r.width, "height": r.height,
                "depth": r.depth, "wallHeight": w.top_height, "wallThickness": w.thickness(), "wallLength": w.length(),
            }))
        });
        let count: usize = self.model.iter().map(|e| match e {
            ElementModel::Wall(w) => openings_of(&w.effective()).len(),
            _ => 0,
        }).sum();
        serde_json::json!({
            "active": true,
            "preset": o.preset.name(),
            "selected": selected,
            "count": count,
            "canUndo": self.gestures.can_undo(),
            "canRedo": self.gestures.can_redo(),
            "faced": o.faced.map(|f| f.0 as f64),
        })
        .to_string()
    }

    /// Every opening on every wall, outlined in screen space (device
    /// pixels), with selection and hover.
    pub fn openings_hud_json(&self) -> String {
        let Some(o) = &self.openings else { return r#"{"active":false}"#.to_owned() };
        let mut items = Vec::new();
        // Head-on to a wall, the others' openings would be edge-on slivers.
        let only = (self.camera.mode == ViewMode::Elevation).then_some(o.faced).flatten();
        for e in &self.model {
            let ElementModel::Wall(w) = e else { continue };
            if only.is_some_and(|f| f != w.element) {
                continue;
            }
            for r in openings_of(&w.effective()) {
                let Some(outline) = self.opening_screen(w, &r) else { continue };
                let id = (w.element, r.face);
                items.push(serde_json::json!({
                    "wall": w.element.0 as f64, "face": r.face, "kind": r.kind.name(),
                    "pts": outline, "sel": o.selected == Some(id), "hover": o.hover == Some(id),
                    "niche": r.depth.is_some(),
                }));
            }
        }
        serde_json::json!({ "active": true, "items": items }).to_string()
    }

    /// Every opening (test hook): wall, face, kind, and its rectangle.
    pub fn openings_json(&self) -> String {
        let mut out = Vec::new();
        for e in &self.model {
            let ElementModel::Wall(w) = e else { continue };
            for r in openings_of(&w.effective()) {
                out.push(serde_json::json!({
                    "wall": w.element.0 as f64, "face": r.face, "kind": r.kind.name(), "offset": r.offset,
                    "sill": r.sill, "width": r.width, "height": r.height, "depth": r.depth,
                }));
            }
        }
        serde_json::Value::Array(out).to_string()
    }
}

impl AuthorApp {
    fn leave_openings(&mut self) {
        self.openings = None;
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

    /// The elevation frame facing the wall Openings mode works on.
    pub(super) fn openings_faced_frame(&self) -> Option<(ElevationFrame, f32, f32)> {
        let faced = self.openings.as_ref()?.faced?;
        self.wall_line(faced).map(|w| self.elevation_frame(&w))
    }

    /// One opening of a wall, by face.
    fn opening(&self, element: EntityId, face: u32) -> Option<OpeningRect> {
        openings_of(&self.wall(element)?.effective()).into_iter().find(|r| r.face == face)
    }

    /// World point of a wall-local (u, v) on the reference face, pushed
    /// `w` into the wall (along its material normal).
    fn wall_world(&self, w: &WallModel, u: f64, v: f64, into: f64) -> Vec3 {
        let (d, n) = (w.dir(), w.normal());
        let z = self.plane_elevation(w.base) + v;
        Vec3::new(
            (w.start[0] + d[0] * u + n[0] * into) as f32,
            (w.start[1] + d[1] * u + n[1] * into) as f32,
            z as f32,
        )
    }

    /// An opening's outline on screen: the convex hull of its box through
    /// the wall (a line in plan, a rectangle head-on).
    fn opening_screen(&self, w: &WallModel, r: &OpeningRect) -> Option<Vec<[f32; 2]>> {
        let (vw, vh) = self.size_f();
        let [[u0, v0], [u1, v1]] = r.visible();
        let t = w.thickness();
        let mut pts = Vec::with_capacity(8);
        for (u, v) in [(u0, v0), (u1, v0), (u1, v1), (u0, v1)] {
            for into in [0.0, t] {
                let (x, y) = self.camera.project(self.wall_world(w, u, v, into), vw, vh)?;
                pts.push([f64::from(x), f64::from(y)]);
            }
        }
        Some(hull(pts).iter().map(|p| [p[0] as f32, p[1] as f32]).collect())
    }

    /// The opening under a canvas point (within `tol` device pixels).
    fn opening_at(&self, px: f32, py: f32, tol: f32) -> Option<(EntityId, u32)> {
        let p = [f64::from(px), f64::from(py)];
        let mut best: Option<(f64, (EntityId, u32))> = None;
        let faced = self.openings.as_ref().and_then(|o| o.faced);
        let only = (self.camera.mode == ViewMode::Elevation).then_some(faced).flatten();
        for e in &self.model {
            let ElementModel::Wall(w) = e else { continue };
            if only.is_some_and(|f| f != w.element) {
                continue;
            }
            for r in openings_of(&w.effective()) {
                let Some(poly) = self.opening_screen(w, &r) else { continue };
                let poly: Vec<P2> = poly.iter().map(|q| [f64::from(q[0]), f64::from(q[1])]).collect();
                let n = poly.len();
                let d = if n >= 3 && point_in_polygon(p, &poly) {
                    0.0
                } else {
                    (0..n).map(|i| point_segment_distance(p, poly[i], poly[(i + 1) % n])).fold(f64::INFINITY, f64::min)
                };
                if d <= f64::from(tol) && best.is_none_or(|(bd, _)| d < bd) {
                    best = Some((d, (w.element, r.face)));
                }
            }
        }
        best.map(|(_, hit)| hit)
    }

    /// Where a canvas point is on a wall, wall-local (u along it, v up):
    /// on its vertical plane when the view looks at it, else (plan) on
    /// its base plane, projected onto its line (v = 0).
    fn wall_uv_at(&self, element: EntityId, px: f32, py: f32) -> Option<P2> {
        let w = self.wall(element)?;
        let (vw, vh) = self.size_f();
        let origin = self.wall_world(w, 0.0, 0.0, 0.0);
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

    fn wall_u_at(&self, element: EntityId, px: f32, py: f32) -> Option<f64> {
        self.wall_uv_at(element, px, py).map(|uv| uv[0])
    }

    /// Store an opening change on its wall — `old` replaced by `new`
    /// (`None` / `None`: add / delete) — with one `UpdateWall` (part of
    /// the open gesture `key`, else one undo step). The new opening is
    /// snapped and kept on the wall first. Returns its face.
    fn write_opening(
        &mut self,
        element: EntityId,
        old: Option<OpeningRect>,
        new: Option<OpeningRect>,
        key: Option<&str>,
    ) -> Result<Option<u32>, String> {
        let w = self.wall(element).cloned().ok_or("The wall no longer exists")?;
        let span = WallSpan { length: w.length(), height: w.top_height, thickness: w.thickness() };
        let new = match new {
            Some(r) => Some(fit(r, span).map_err(str::to_owned)?),
            None => None,
        };
        if let (Some(a), Some(b)) = (old, new)
            && a.offset == b.offset
            && a.sill == b.sill
            && a.width == b.width
            && a.height == b.height
            && a.depth == b.depth
        {
            return Ok(Some(a.face)); // nothing moved (snapped to the same place)
        }
        let (h, top) = (w.top_height, w.top_points.clone());
        let map_err = |e: vim_design_lib::sketch::SketchError| crate::authoring::edit::profile::edit_error(e).message().to_owned();
        // Replace = delete the old void, add the new one (a fresh face id).
        let (mut profile, mut anchors) = (w.profile.clone(), top);
        if let Some(a) = old {
            (profile, anchors) = vim_design_lib::wall::ops::delete_faces(&profile, &anchors, h, &[a.face]).map_err(map_err)?;
        }
        let mut face = None;
        if let Some(b) = new {
            let before: Vec<u32> = profile.faces.iter().map(|f| f.id).collect();
            let kind = SketchFaceKind::Void { depth: b.depth };
            (profile, anchors) = vim_design_lib::wall::ops::add_face(&profile, &anchors, h, &b.outline(), kind).map_err(map_err)?;
            face = profile.faces.iter().map(|f| f.id).find(|id| !before.contains(id));
        }
        let depth = self.doc.undo_depth();
        if let Some(k) = key {
            self.gestures.begin_continuing(&self.doc, k);
        }
        let mut cmd = update_wall(w.wall, false);
        if let Command::UpdateWall { profile: p, top_points: tp, .. } = &mut cmd {
            *p = Some(profile);
            *tp = Some(anchors);
        }
        self.doc.submit(cmd).map_err(|st| format!("the wall was refused ({st:?})"))?;
        if key.is_none() {
            self.gestures.one_shot(depth);
        }
        // A replaced opening keeps its selection and drag under the new id.
        if let (Some(a), Some(f)) = (old, face)
            && let Some(o) = self.openings.as_mut()
        {
            if o.selected == Some((element, a.face)) {
                o.selected = Some((element, f));
            }
            if let Some(d) = o.drag.as_mut().filter(|d| d.element == element && d.orig.face == a.face) {
                d.orig.face = f;
            }
        }
        self.sync("opening");
        Ok(face)
    }
}

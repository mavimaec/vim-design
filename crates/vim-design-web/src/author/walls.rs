//! Wall operations of the authoring app: settings for new walls (their
//! height mode: a fixed height, or up to a construction plane plus an
//! offset), wall property edits, and picking a wall (with a generous
//! screen tolerance — walls are thin in plan).
//!
//! Walls are the library's `Wall` entity: a reference line on a plane
//! and an elevation profile (openings are void faces, edited in the
//! wall's Edit Mode). Legacy extrusion walls from before it still show,
//! select, and delete; their height and thickness stay editable, and the
//! pencil converts one in place.

use glam::Vec3;
use vim_design_lib::sketch::SketchFaceKind;
use vim_design_lib::{Command, EntityId};
use wasm_bindgen::prelude::*;

use super::camera::ViewMode;
use super::{
    AuthorApp, MAX_WALL_HEIGHT_M, MAX_WALL_THICKNESS_M, MIN_WALL_HEIGHT_M, MIN_WALL_THICKNESS_M, eid,
};
use crate::authoring::model::{LegacyWallModel, WallModel, WallRunModel};
use crate::authoring::ops::{self, WallHeight};
use crate::authoring::walls::WINDOW_MARGIN_M;

/// Screen distance from a point to a segment (pixels).
fn seg_dist(p: (f32, f32), a: (f32, f32), b: (f32, f32)) -> f32 {
    let (abx, aby) = (b.0 - a.0, b.1 - a.1);
    let len2 = abx * abx + aby * aby;
    let t = if len2 <= 1e-9 {
        0.0
    } else {
        (((p.0 - a.0) * abx + (p.1 - a.1) * aby) / len2).clamp(0.0, 1.0)
    };
    (p.0 - a.0 - abx * t).hypot(p.1 - a.1 - aby * t)
}

/// An `UpdateWall` that changes only the given fields.
pub(super) fn update_wall(id: EntityId, coalesce: bool) -> Command {
    Command::UpdateWall {
        id,
        base: None,
        top: None,
        start: None,
        end: None,
        height_m: None,
        top_offset_m: None,
        profile: None,
        top_points: None,
        coalesce,
    }
}

#[wasm_bindgen]
impl AuthorApp {
    /// Settings for NEW walls (meters; `flip` puts the thickness on the
    /// right of the drawing direction).
    pub fn set_wall_settings(&mut self, height: f64, thickness: f64, flip: bool) {
        if height.is_finite() {
            self.wall_height = height.clamp(MIN_WALL_HEIGHT_M, MAX_WALL_HEIGHT_M);
        }
        if thickness.is_finite() {
            self.wall_thickness = thickness.clamp(MIN_WALL_THICKNESS_M, MAX_WALL_THICKNESS_M);
        }
        self.wall_flip = flip;
    }

    pub fn wall_settings_json(&self) -> String {
        let base = self.plane();
        serde_json::json!({
            "height": self.wall_height,
            "thickness": self.wall_thickness,
            "flip": self.wall_flip,
            "mode": if self.wall_top.is_some() { "upto" } else { "fixed" },
            "topPlane": self.wall_top.map(|p| p.0 as f64),
            "topOffset": self.wall_top_offset,
            "effectiveHeight": base.and_then(|b| self.new_wall_height_mode(b).ok().map(|m| self.height_of(b, &m))),
            "planes": self.top_plane_candidates(),
        })
        .to_string()
    }

    /// Height mode for new walls: "fixed" (the height setting) or "upto"
    /// (up to `plane` plus `offset`).
    pub fn set_wall_height_mode(&mut self, mode: &str, plane: f64, offset: f64) {
        self.wall_top = (mode == "upto").then(|| eid(plane)).filter(|p| self.root_level(*p).is_some());
        if offset.is_finite() {
            self.wall_top_offset = offset.clamp(-MAX_WALL_HEIGHT_M, MAX_WALL_HEIGHT_M);
        }
    }

    /// A wall's height mode: up to `plane` plus `offset` (the height then
    /// follows the plane), or fixed at its current height when `plane` is
    /// negative. One undo step; offset drags coalesce until
    /// [`AuthorApp::end_gesture`]. Returns the new height, or -1 when
    /// refused (the top would not clear the openings or the base).
    pub fn set_wall_top(&mut self, id: f64, plane: f64, offset: f64) -> f64 {
        if let Some(r) = self.run_model(eid(id)).cloned() {
            return self.set_run_top(&r, plane, offset);
        }
        let Some(wall) = self.wall(eid(id)).cloned() else { return -1.0 };
        let top = (plane >= 0.0).then(|| eid(plane));
        let offset = if offset.is_finite() { offset.clamp(-MAX_WALL_HEIGHT_M, MAX_WALL_HEIGHT_M) } else { 0.0 };
        let height = match top {
            Some(t) if self.root_level(t).is_none() => return -1.0,
            Some(t) => self.plane_elevation(t) + offset - self.plane_elevation(wall.base),
            None => wall.top_height,
        };
        if height < self.lib_wall_min_height(&wall) - 1e-9 {
            return -1.0;
        }
        let coalesce = self.gestures.begin_continuing(&self.doc, &format!("wall_top_{}", wall.wall.0));
        let mut cmd = update_wall(wall.wall, coalesce);
        if let Command::UpdateWall { top: t, top_offset_m, height_m, .. } = &mut cmd {
            *t = Some(top);
            match top {
                Some(_) => *top_offset_m = Some(offset),
                // Fixed: keep the height the wall has now.
                None => *height_m = Some(height.clamp(MIN_WALL_HEIGHT_M, MAX_WALL_HEIGHT_M)),
            }
        }
        self.submit(cmd);
        self.sync("wall top");
        height
    }

    /// The wall under (or near) a canvas point: a ray hit on a wall, or
    /// else the wall whose base or top line passes within `tol_px`
    /// device pixels. In plan, only walls on the active level count.
    /// Returns the element id or -1.
    pub fn pick_wall(&self, px: f32, py: f32, tol_px: f32) -> f64 {
        let hit = self.pick(px, py);
        if hit >= 0.0 && self.is_wall(eid(hit)) {
            return hit;
        }
        let (w, h) = self.size_f();
        let plan = self.camera.mode == ViewMode::Plan;
        let mut best: Option<(f32, EntityId)> = None;
        for (e, wall) in self.model.iter().flat_map(|e| e.wall_lines().into_iter().map(move |w| (e, w))) {
            if plan && e.level() != self.active_level {
                continue;
            }
            let z = self.plane_elevation(wall.plane) + wall.base_w;
            let world = |p: [f64; 2], dz: f64| Vec3::new(p[0] as f32, p[1] as f32, (z + dz) as f32);
            for dz in [0.0, wall.height * 0.5, wall.height] {
                let (Some(a), Some(b)) = (
                    self.camera.project(world(wall.start, dz), w, h),
                    self.camera.project(world(wall.end, dz), w, h),
                ) else {
                    continue;
                };
                let d = seg_dist((px, py), a, b);
                if d <= tol_px && best.is_none_or(|(bd, _)| d < bd) {
                    best = Some((d, wall.element));
                }
            }
        }
        best.map_or(-1.0, |(_, id)| id.0 as f64)
    }

    /// Edit a wall's fixed height (coalesced within one gesture); a wall
    /// that was up to a plane becomes fixed. Clamped so the wall stays
    /// above its openings.
    pub fn set_wall_height(&mut self, id: f64, height: f64) {
        let id = eid(id);
        if !height.is_finite() {
            return;
        }
        if let Some(r) = self.run_model(id).cloned() {
            let h = height.clamp(self.run_min_height(&r), MAX_WALL_HEIGHT_M);
            let coalesce = self.gestures.begin_continuing(&self.doc, &format!("wall_height_{}", id.0));
            self.submit(run_update(r.run, coalesce, |top, _offset, height_m, _t| {
                *height_m = Some(h);
                if r.top.is_some() {
                    *top = Some(None);
                }
            }));
            self.sync("wall height");
            return;
        }
        if let Some(wall) = self.wall(id).cloned() {
            let h = height.clamp(self.lib_wall_min_height(&wall), MAX_WALL_HEIGHT_M);
            let coalesce = self.gestures.begin_continuing(&self.doc, &format!("wall_height_{}", id.0));
            let mut cmd = update_wall(wall.wall, coalesce);
            if let Command::UpdateWall { top, height_m, .. } = &mut cmd {
                *height_m = Some(h);
                if wall.top.is_some() {
                    *top = Some(None);
                }
            }
            self.submit(cmd);
            self.sync("wall height");
            return;
        }
        let Some(wall) = self.legacy_wall(id).cloned() else { return };
        let h = height.clamp(self.min_wall_height(&wall), MAX_WALL_HEIGHT_M);
        let coalesce = self.gestures.begin_continuing(&self.doc, &format!("wall_height_{}", id.0));
        for cp in &wall.top_cps {
            let Some((_, p)) = crate::authoring::model::control_point(&self.doc, *cp) else {
                continue;
            };
            self.submit(Command::UpdateControlPoint {
                id: *cp,
                position: [p[0], p[1], wall.base_w + h],
                coalesce,
            });
        }
        self.sync("wall height");
    }

    /// Edit a wall's thickness: every solid face of its profile (a legacy
    /// wall: its extrusion path's end point, so the profile face and its
    /// windows stay put).
    pub fn set_wall_thickness(&mut self, id: f64, thickness: f64) {
        let id = eid(id);
        if !thickness.is_finite() {
            return;
        }
        let t = thickness.clamp(MIN_WALL_THICKNESS_M, MAX_WALL_THICKNESS_M);
        if let Some(r) = self.run_model(id).cloned() {
            let mut data = r.data.clone();
            data.thickness_m = t;
            if let Err(e) = vim_design_lib::wall_run::validate(&data) {
                self.notice = Some(crate::authoring::runs::run_error(e).message().to_owned());
                return;
            }
            let coalesce = self.gestures.begin_continuing(&self.doc, &format!("wall_thickness_{}", id.0));
            self.submit(run_update(r.run, coalesce, |_top, _offset, _height, th| *th = Some(t)));
            self.sync("wall thickness");
            return;
        }
        let coalesce = self.gestures.begin_continuing(&self.doc, &format!("wall_thickness_{}", id.0));
        if let Some(wall) = self.wall(id).cloned() {
            let mut profile = wall.profile.clone();
            for f in &mut profile.faces {
                if let SketchFaceKind::Solid { thickness } = &mut f.kind {
                    *thickness = t;
                }
            }
            let mut cmd = update_wall(wall.wall, coalesce);
            if let Command::UpdateWall { profile: p, .. } = &mut cmd {
                *p = Some(profile);
            }
            self.submit(cmd);
            self.sync("wall thickness");
            return;
        }
        let Some(wall) = self.legacy_wall(id).cloned() else { return };
        let Some((_, s)) = crate::authoring::model::control_point(&self.doc, wall.path_start) else {
            return;
        };
        let end = [s[0] + wall.normal[0] * t, s[1] + wall.normal[1] * t, s[2]];
        self.submit(Command::UpdateControlPoint { id: wall.path_end, position: end, coalesce });
        self.sync("wall thickness");
    }

    /// Remove a window from a legacy wall and sweep its construction
    /// geometry.
    pub fn delete_window(&mut self, wall: f64, wire: f64) -> bool {
        self.delete_hole(wall, wire)
    }

    /// Remove an opening — one undo step: a wall run's opening (by id),
    /// or a void face of a `Wall`'s profile.
    pub fn delete_opening(&mut self, wall: f64, face: f64) -> bool {
        if let Some(r) = self.run_model(eid(wall)).cloned() {
            let Ok(data) = vim_design_lib::wall_run::ops::delete_opening(&r.data, face as u32) else { return false };
            let depth = self.doc.undo_depth();
            self.submit(ops::update_run(r.run, &data, false));
            self.gestures.one_shot(depth);
            self.sync("delete opening");
            return true;
        }
        let Some(w) = self.wall(eid(wall)).cloned() else { return false };
        let face = face as u32;
        if !w.profile.faces.iter().any(|f| f.id == face && matches!(f.kind, SketchFaceKind::Void { .. })) {
            return false;
        }
        match vim_design_lib::wall::ops::delete_faces(&w.profile, &w.top_points, w.top_height, &[face]) {
            Ok((profile, top_points)) => {
                let depth = self.doc.undo_depth();
                let mut cmd = update_wall(w.wall, false);
                if let Command::UpdateWall { profile: p, top_points: tp, .. } = &mut cmd {
                    *p = Some(profile);
                    *tp = Some(top_points);
                }
                self.submit(cmd);
                self.gestures.one_shot(depth);
                self.sync("delete opening");
                true
            }
            Err(_) => false,
        }
    }
}

/// An `UpdateWallRun` changing only the height fields and the thickness
/// set by `f(top, top_offset, height, thickness)`.
fn run_update(
    id: EntityId,
    coalesce: bool,
    f: impl FnOnce(&mut Option<Option<EntityId>>, &mut Option<f64>, &mut Option<f64>, &mut Option<f64>),
) -> Command {
    let (mut top, mut offset, mut height, mut thickness) = (None, None, None, None);
    f(&mut top, &mut offset, &mut height, &mut thickness);
    Command::UpdateWallRun {
        id,
        base: None,
        top,
        points: None,
        closed: None,
        thickness_m: thickness,
        height_m: height,
        top_offset_m: offset,
        openings: None,
        profiles: None,
        coalesce,
    }
}

impl AuthorApp {
    /// A wall run's height mode (see [`AuthorApp::set_wall_top`]).
    fn set_run_top(&mut self, r: &WallRunModel, plane: f64, offset: f64) -> f64 {
        let top = (plane >= 0.0).then(|| eid(plane));
        let offset = if offset.is_finite() { offset.clamp(-MAX_WALL_HEIGHT_M, MAX_WALL_HEIGHT_M) } else { 0.0 };
        let height = match top {
            Some(t) if self.root_level(t).is_none() => return -1.0,
            Some(t) => self.plane_elevation(t) + offset - self.plane_elevation(r.base),
            None => r.top_height,
        };
        if height < self.run_min_height(r) - 1e-9 {
            return -1.0;
        }
        let coalesce = self.gestures.begin_continuing(&self.doc, &format!("wall_top_{}", r.run.0));
        self.submit(run_update(r.run, coalesce, |t, o, h, _| {
            *t = Some(top);
            match top {
                Some(_) => {
                    *o = Some(offset);
                    // The fixed height is the fallback if the top goes.
                    *h = Some(height.clamp(MIN_WALL_HEIGHT_M, MAX_WALL_HEIGHT_M));
                }
                None => *h = Some(height.clamp(MIN_WALL_HEIGHT_M, MAX_WALL_HEIGHT_M)),
            }
        }));
        self.sync("wall top");
        height
    }

    /// Lowest top reference a wall run may take: above its openings by
    /// the window margin.
    pub(super) fn run_min_height(&self, r: &WallRunModel) -> f64 {
        r.data
            .openings
            .iter()
            .map(|o| match o.kind {
                vim_design_lib::wall_run::OpeningKind::Door => o.height_m,
                vim_design_lib::wall_run::OpeningKind::Window => o.sill_m + o.height_m,
            })
            .fold(MIN_WALL_HEIGHT_M - WINDOW_MARGIN_M, f64::max)
            + WINDOW_MARGIN_M
    }

    /// Planes a wall top can reach: every construction plane (levels and
    /// workplanes), highest first, with its path and elevation.
    fn top_plane_candidates(&self) -> Vec<serde_json::Value> {
        let mut planes: Vec<EntityId> = ops::levels_sorted(&self.doc).iter().map(|l| l.id).collect();
        planes.extend(ops::workplanes(&self.doc).iter().map(|w| w.id));
        let mut rows: Vec<(f64, serde_json::Value)> =
            planes.into_iter().map(|p| (self.plane_elevation(p), self.plane_json(p))).collect();
        rows.sort_by(|a, b| b.0.total_cmp(&a.0));
        rows.into_iter().map(|(_, v)| v).collect()
    }

    /// The height a wall of `mode` gets on `base`.
    fn height_of(&self, base: EntityId, mode: &WallHeight) -> f64 {
        match mode.top {
            Some(top) => self.plane_elevation(top) + mode.top_offset_m - self.plane_elevation(base),
            None => mode.height_m,
        }
    }

    /// The height mode of a new wall on `base` from the tool bar: fixed,
    /// or up to the chosen plane (refused when that is not above the
    /// base).
    pub(super) fn new_wall_height_mode(&self, base: EntityId) -> Result<WallHeight, String> {
        let Some(top) = self.wall_top else {
            return Ok(WallHeight { height_m: self.wall_height, top: None, top_offset_m: 0.0 });
        };
        let mode = WallHeight { height_m: self.wall_height, top: Some(top), top_offset_m: self.wall_top_offset };
        let h = self.height_of(base, &mode);
        if h < MIN_WALL_HEIGHT_M {
            return Err("The wall top must be above its base: pick a higher plane".to_owned());
        }
        // The stored height is the fallback if the top is later unwired.
        Ok(WallHeight { height_m: h.min(MAX_WALL_HEIGHT_M), ..mode })
    }

    /// Lowest height a legacy wall may take: above its highest window by
    /// the window margin.
    pub(super) fn min_wall_height(&self, wall: &LegacyWallModel) -> f64 {
        (wall.highest_window_top() + WINDOW_MARGIN_M).max(MIN_WALL_HEIGHT_M)
    }

    /// Lowest top reference a wall may take: its base-anchored points
    /// (window heads, a door top) stay below the top, and its
    /// top-anchored points stay above the base, by the window margin.
    pub(super) fn lib_wall_min_height(&self, wall: &WallModel) -> f64 {
        wall.profile
            .points
            .iter()
            .map(|p| {
                if wall.top_points.contains(&p.id) { -p.uv[1] } else { p.uv[1] }
            })
            .fold(0.0, f64::max)
            .max(MIN_WALL_HEIGHT_M - WINDOW_MARGIN_M)
            + WINDOW_MARGIN_M
    }
}

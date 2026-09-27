//! Wall and window operations of the authoring app: wall settings and
//! edits, picking a wall (with a generous screen tolerance — walls are
//! thin in plan), and the window flow: tap a wall -> orthographic
//! elevation view facing it -> draw windows -> Done returns to the
//! previous view.

use glam::Vec3;
use vim_design_lib::{Command, EntityId};
use wasm_bindgen::prelude::*;

use super::camera::ViewMode;
use super::{
    AuthorApp, MAX_WALL_HEIGHT_M, MAX_WALL_THICKNESS_M, MIN_WALL_HEIGHT_M,
    MIN_WALL_THICKNESS_M, Tool, eid,
};
use crate::authoring::model::{ElementModel, WallModel};
use crate::authoring::sketch::{Sketch, SketchTool};
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
            "effectiveHeight": base.and_then(|b| self.new_wall_height(b).ok()),
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

    /// Up-to height for an existing wall: its height becomes the distance
    /// from its base to `plane` plus `offset`. Returns the new height, or
    /// -1 when the plane is not above the base.
    pub fn set_wall_height_up_to(&mut self, id: f64, plane: f64, offset: f64) -> f64 {
        let Some(wall) = self.wall(eid(id)).cloned() else { return -1.0 };
        match self.height_up_to(wall.plane_level, eid(plane), offset) {
            Ok(h) => {
                self.set_wall_height(id, h);
                self.gestures.end();
                h
            }
            Err(_) => -1.0,
        }
    }
}

#[wasm_bindgen]
impl AuthorApp {
    /// The wall under (or near) a canvas point: a ray hit on a wall, or
    /// else the wall whose base or top line passes within `tol_px`
    /// device pixels. In plan, only walls on the active level count.
    /// Returns the element id or -1.
    pub fn pick_wall(&self, px: f32, py: f32, tol_px: f32) -> f64 {
        let hit = self.pick(px, py);
        if hit >= 0.0 && self.wall(eid(hit)).is_some() {
            return hit;
        }
        let (w, h) = self.size_f();
        let plan = self.camera.mode == ViewMode::Plan;
        let mut best: Option<(f32, EntityId)> = None;
        for e in &self.model {
            let ElementModel::Wall(wall) = e else { continue };
            if plan && Some(wall.plane_level) != self.active_level {
                continue;
            }
            let z = self.level_elevation(wall.plane_level) + wall.base_w;
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

    /// Start drawing windows on a wall: switch to the window tool and an
    /// orthographic elevation view facing the wall's profile face (the
    /// current view is restored by [`AuthorApp::end_window`]).
    pub fn begin_window(&mut self, wall: f64) -> bool {
        let id = eid(wall);
        let Some(model) = self.wall(id).cloned() else {
            return false;
        };
        if self.prev_camera.is_none() {
            self.prev_camera = Some(self.camera.clone());
        }
        let (frame, length, height) = self.elevation_frame(&model);
        let (w, h) = self.size_f();
        self.camera.enter_elevation(frame, length, height, w / h);
        self.tool = Tool::Window;
        self.window_host = Some(id);
        self.selection = None;
        self.sketch = Some(Sketch::new(SketchTool::Window, self.window_shape, model.plane_level));
        self.grid_key = None;
        self.refresh_styles();
        true
    }

    /// Done with the wall: back to the view and camera from before the
    /// elevation (the window tool stays active to pick another wall).
    pub fn end_window(&mut self) {
        self.window_host = None;
        if self.tool == Tool::Window {
            self.sketch = None;
        }
        if let Some(c) = self.prev_camera.take() {
            self.camera = c;
        } else if self.camera.mode == ViewMode::Elevation {
            self.camera.mode = ViewMode::Plan;
        }
        self.camera.elevation = None;
        self.camera.plane_z = self.active_elevation as f32;
        self.grid_key = None;
        self.refresh_styles();
    }

    /// The wall being drawn on (-1 when none).
    pub fn window_host(&self) -> f64 {
        self.window_host.map_or(-1.0, |id| id.0 as f64)
    }

    /// Edit a wall's height (coalesced within one gesture). Clamped so the
    /// wall stays above its highest window plus the window margin.
    pub fn set_wall_height(&mut self, id: f64, height: f64) {
        let id = eid(id);
        let Some(wall) = self.wall(id).cloned() else { return };
        if !height.is_finite() {
            return;
        }
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

    /// Edit a wall's thickness: moves only the extrusion path's end point
    /// (the profile face and its windows stay put).
    pub fn set_wall_thickness(&mut self, id: f64, thickness: f64) {
        let id = eid(id);
        let Some(wall) = self.wall(id).cloned() else { return };
        if !thickness.is_finite() {
            return;
        }
        let t = thickness.clamp(MIN_WALL_THICKNESS_M, MAX_WALL_THICKNESS_M);
        let Some((_, s)) = crate::authoring::model::control_point(&self.doc, wall.path_start) else {
            return;
        };
        let end = [s[0] + wall.normal[0] * t, s[1] + wall.normal[1] * t, s[2]];
        let coalesce = self.gestures.begin_continuing(&self.doc, &format!("wall_thickness_{}", id.0));
        self.submit(Command::UpdateControlPoint { id: wall.path_end, position: end, coalesce });
        self.sync("wall thickness");
    }

    /// Remove a window from a wall and sweep its construction geometry.
    pub fn delete_window(&mut self, wall: f64, wire: f64) -> bool {
        self.delete_hole(wall, wire)
    }
}

impl AuthorApp {
    /// Planes a wall top can reach: every construction plane, grouped by
    /// story level (top first), with its path and elevation.
    fn top_plane_candidates(&self) -> Vec<serde_json::Value> {
        let mut levels = crate::authoring::ops::levels_sorted(&self.doc);
        levels.reverse();
        levels.iter().map(|l| self.plane_json(l.id)).collect()
    }

    /// Height from a wall's base plane up to `top` plus `offset`.
    fn height_up_to(&self, base: EntityId, top: EntityId, offset: f64) -> Result<f64, String> {
        let h = self.plane_elevation(top) + offset - self.plane_elevation(base);
        if h < MIN_WALL_HEIGHT_M {
            return Err("The wall top must be above its base: pick a higher plane".to_owned());
        }
        Ok(h.min(MAX_WALL_HEIGHT_M))
    }

    /// The height a new wall on `base` gets from the height mode.
    pub(super) fn new_wall_height(&self, base: EntityId) -> Result<f64, String> {
        match self.wall_top {
            Some(top) => self.height_up_to(base, top, self.wall_top_offset),
            None => Ok(self.wall_height),
        }
    }

    /// Lowest height a wall may take: above its highest window by the
    /// window margin.
    pub(super) fn min_wall_height(&self, wall: &WallModel) -> f64 {
        (wall.highest_window_top() + WINDOW_MARGIN_M).max(MIN_WALL_HEIGHT_M)
    }
}

//! The authoring app's camera: a top-down orthographic PLAN view onto
//! the active level, a perspective 3D ORBIT view, and an orthographic
//! ELEVATION view facing one wall (window drawing). Right-handed, Z-up.
//! Camera state is session state: never in the document, never
//! undoable.

use glam::{Mat4, Vec3, Vec4};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewMode {
    Plan,
    Orbit,
    Elevation,
}

impl ViewMode {
    pub fn name(self) -> &'static str {
        match self {
            ViewMode::Plan => "plan",
            ViewMode::Orbit => "3d",
            ViewMode::Elevation => "elevation",
        }
    }
}

/// Vertical field of view of the 3D view.
const FOV_Y: f32 = 45.0 * std::f32::consts::PI / 180.0;
/// Plan cut height above the active plane when the level's plan span
/// gives none above it (the architectural "cut plane"): geometry above
/// it is clipped by the near plane, so upper floors never hide the level
/// being drawn on.
pub const PLAN_CUT_M: f32 = 1.2;
/// Depth range below the plan cut.
const PLAN_DEPTH_M: f32 = 400.0;
const MIN_DISTANCE: f32 = 1.0;
const MAX_DISTANCE: f32 = 600.0;
const MIN_HALF_H: f32 = 0.5;
const MAX_HALF_H: f32 = 400.0;
/// Navigation stays near the model: the visible half-height (or the 3D
/// distance's equivalent) is at most `VIEW_RANGE_FACTOR` times the
/// model's radius plus `MIN_VIEW_RANGE_M`, and the view centre stays
/// within the model's radius times `PAN_MARGIN_FACTOR` plus
/// `MIN_PAN_MARGIN_M` of its centre.
pub const VIEW_RANGE_FACTOR: f32 = 3.0;
pub const MIN_VIEW_RANGE_M: f32 = 12.0;
pub const PAN_MARGIN_FACTOR: f32 = 1.5;
pub const MIN_PAN_MARGIN_M: f32 = 8.0;
/// Elevation view: the eye sits this far in front of the wall face it
/// looks at, so everything nearer the viewer is clipped away.
const ELEVATION_EYE_GAP_M: f32 = 0.02;
/// Elevation view: depth kept behind the faced wall's back face.
/// Everything farther is clipped, so window openings show the
/// background instead of the walls behind them.
const ELEVATION_BACK_M: f32 = 0.05;

/// The wall an elevation view faces. The basis comes from stable inputs
/// only: `u` = the wall's drawn direction, `v` = world up, and the view
/// direction `n = up × u` (so `u` always points to the screen's right).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ElevationFrame {
    /// World point of the wall's base-line start (u = 0, v = 0).
    pub origin: Vec3,
    pub u: Vec3,
    /// View direction (horizontal).
    pub n: Vec3,
    /// Offset along `n` from the reference face to the wall face nearest
    /// the viewer (≤ 0).
    pub near_offset: f32,
    /// Wall thickness (the depth of the view volume behind the face).
    pub depth: f32,
}

#[derive(Debug, Clone)]
pub struct Camera {
    pub mode: ViewMode,
    /// Orbit target / plan center (x, y). `z` follows the active plane.
    pub target: Vec3,
    pub yaw: f32,
    pub pitch: f32,
    pub distance: f32,
    /// Plan view: half the visible height in meters.
    pub plan_half_h: f32,
    /// Elevation of the active construction plane.
    pub plane_z: f32,
    /// The plan cut in world z (the active level's plan span); used when
    /// above the active plane (a workplane above the cut gets
    /// `PLAN_CUT_M` over it instead).
    pub cut_z: Option<f32>,
    /// Elevation view: the faced wall, and half the visible height.
    pub elevation: Option<ElevationFrame>,
    pub elevation_half_h: f32,
}

impl Default for Camera {
    fn default() -> Self {
        Self {
            mode: ViewMode::Plan,
            target: Vec3::ZERO,
            yaw: -120f32.to_radians(),
            pitch: 32f32.to_radians(),
            distance: 28.0,
            plan_half_h: 9.0,
            plane_z: 0.0,
            cut_z: None,
            elevation: None,
            elevation_half_h: 3.0,
        }
    }
}

impl Camera {
    fn orbit_dir(&self) -> Vec3 {
        let (sy, cy) = self.yaw.sin_cos();
        let (sp, cp) = self.pitch.sin_cos();
        Vec3::new(cp * cy, cp * sy, sp)
    }

    fn orbit_target(&self) -> Vec3 {
        Vec3::new(self.target.x, self.target.y, self.plane_z)
    }

    /// The 3D view's eye position.
    pub fn eye(&self) -> Vec3 {
        self.orbit_target() + self.distance * self.orbit_dir()
    }

    pub fn view_proj(&self, aspect: f32) -> Mat4 {
        if let (ViewMode::Elevation, Some(f)) = (self.mode, self.elevation) {
            let eye = self.target + f.n * (f.near_offset - ELEVATION_EYE_GAP_M);
            let view = glam::camera::rh::view::look_at_mat4(eye, eye + f.n, Vec3::Z);
            let hh = self.elevation_half_h;
            let hw = hh * aspect;
            let far = ELEVATION_EYE_GAP_M + f.depth + ELEVATION_BACK_M;
            return glam::camera::rh::proj::directx::orthographic(-hw, hw, -hh, hh, 0.0, far)
                * view;
        }
        match self.mode {
            ViewMode::Orbit => {
                let target = self.orbit_target();
                let eye = self.eye();
                // Looking straight down makes Z-up degenerate as the up
                // vector; the pitch clamp keeps us away from it.
                let view = glam::camera::rh::view::look_at_mat4(eye, target, Vec3::Z);
                let near = (self.distance * 0.01).max(0.05);
                let far = self.distance * 40.0 + 200.0;
                glam::camera::rh::proj::directx::perspective(FOV_Y, aspect, near, far) * view
            }
            ViewMode::Plan | ViewMode::Elevation => {
                let cut = self.cut_z.filter(|z| *z > self.plane_z).unwrap_or(self.plane_z + PLAN_CUT_M);
                let eye = Vec3::new(self.target.x, self.target.y, cut);
                let view = glam::camera::rh::view::look_at_mat4(eye, eye - Vec3::Z, Vec3::Y);
                let hh = self.plan_half_h;
                let hw = hh * aspect;
                glam::camera::rh::proj::directx::orthographic(-hw, hw, -hh, hh, 0.0, PLAN_DEPTH_M)
                    * view
            }
        }
    }

    /// Screen pixels per meter at the target (for grid density).
    pub fn px_per_m(&self, height_px: f32) -> f32 {
        let half_h = match self.mode {
            ViewMode::Elevation if self.elevation.is_some() => self.elevation_half_h,
            ViewMode::Plan | ViewMode::Elevation => self.plan_half_h,
            ViewMode::Orbit => self.distance * (FOV_Y / 2.0).tan(),
        };
        height_px / (2.0 * half_h.max(1e-3))
    }

    /// Feature-edge nudge anchor and fraction (see the renderer).
    pub fn edge_nudge(&self) -> ([f32; 3], f32) {
        if let (ViewMode::Elevation, Some(f)) = (self.mode, self.elevation) {
            // A far point behind the viewer: 1000 m * 5e-6 = 5 mm pull.
            return ((self.target - f.n * 1000.0).to_array(), 5e-6);
        }
        match self.mode {
            ViewMode::Orbit => (self.eye().to_array(), 0.0015),
            // A far point straight above: 1000 m * 5e-6 = 5 mm pull.
            ViewMode::Plan | ViewMode::Elevation => (
                [self.target.x, self.target.y, self.plane_z + 1000.0],
                5e-6,
            ),
        }
    }

    pub fn orbit(&mut self, dx: f32, dy: f32) {
        if self.mode != ViewMode::Orbit {
            return;
        }
        self.yaw -= dx * 0.0065;
        self.pitch = (self.pitch + dy * 0.0065).clamp(-0.35, 1.53);
    }

    /// Zoom by `factor` (< 1 = in) keeping the plane point `anchor`
    /// fixed on screen (zoom toward the cursor).
    pub fn zoom(&mut self, factor: f32, anchor: Option<Vec3>) {
        if self.mode == ViewMode::Elevation && self.elevation.is_some() {
            let old = self.elevation_half_h;
            let new = (old * factor).clamp(MIN_HALF_H, MAX_HALF_H);
            if let Some(a) = anchor {
                self.target = a + (self.target - a) * (new / old);
            }
            self.elevation_half_h = new;
            return;
        }
        match self.mode {
            ViewMode::Plan | ViewMode::Elevation => {
                let old = self.plan_half_h;
                let new = (old * factor).clamp(MIN_HALF_H, MAX_HALF_H);
                if let Some(a) = anchor {
                    let k = new / old;
                    self.target.x = a.x + (self.target.x - a.x) * k;
                    self.target.y = a.y + (self.target.y - a.y) * k;
                }
                self.plan_half_h = new;
            }
            ViewMode::Orbit => {
                let old = self.distance;
                let new = (old * factor).clamp(MIN_DISTANCE, MAX_DISTANCE);
                if let Some(a) = anchor {
                    let k = new / old;
                    self.target.x = a.x + (self.target.x - a.x) * k;
                    self.target.y = a.y + (self.target.y - a.y) * k;
                }
                self.distance = new;
            }
        }
    }

    /// Translate the view so that the plane point under the previous
    /// pointer lands under the current pointer.
    pub fn pan_plane(&mut self, from: Vec3, to: Vec3) {
        let d = from - to;
        self.target.x += d.x;
        self.target.y += d.y;
        if self.mode == ViewMode::Elevation {
            self.target.z += d.z; // the elevation plane is vertical
        }
    }

    /// Screen-space pan fallback (the pointer is off the plane).
    pub fn pan_pixels(&mut self, dx: f32, dy: f32, height_px: f32) {
        let m_per_px = 1.0 / self.px_per_m(height_px);
        let (right, up) = match self.mode {
            ViewMode::Elevation => (self.elevation.map_or(Vec3::X, |f| f.u), Vec3::Z),
            ViewMode::Plan => (Vec3::X, Vec3::Y),
            ViewMode::Orbit => {
                let fwd = -self.orbit_dir();
                let right = fwd.cross(Vec3::Z).normalize_or_zero();
                let up = Vec3::new(fwd.x, fwd.y, 0.0).normalize_or_zero();
                (right, up)
            }
        };
        self.target -= right * dx * m_per_px;
        self.target += up * dy * m_per_px;
    }

    /// Frame the world AABB.
    pub fn fit(&mut self, min: Vec3, max: Vec3, aspect: f32) {
        let center = (min + max) * 0.5;
        self.target.x = center.x;
        self.target.y = center.y;
        let size = max - min;
        let half_h = (size.y * 0.5).max(size.x * 0.5 / aspect.max(0.1)).max(1.0) * 1.25;
        self.plan_half_h = half_h.clamp(MIN_HALF_H, MAX_HALF_H);
        let radius = (size.length() * 0.5).max(2.0);
        let fov = if aspect < 1.0 {
            2.0 * ((FOV_Y / 2.0).tan() * aspect).atan()
        } else {
            FOV_Y
        };
        self.distance = (radius / (fov / 2.0).sin() * 1.05).clamp(MIN_DISTANCE, MAX_DISTANCE);
    }

    /// Keep the view near a model of `radius` around `center`: cap the
    /// zoom-out and pull the view centre back within the pan margin.
    pub fn clamp_to(&mut self, center: Vec3, radius: f32) {
        let range = radius.max(0.0) * VIEW_RANGE_FACTOR + MIN_VIEW_RANGE_M;
        self.plan_half_h = self.plan_half_h.min(range).max(MIN_HALF_H);
        self.elevation_half_h = self.elevation_half_h.min(range).max(MIN_HALF_H);
        self.distance = self.distance.min(range / (FOV_Y / 2.0).tan()).max(MIN_DISTANCE);
        let margin = radius.max(0.0) * PAN_MARGIN_FACTOR + MIN_PAN_MARGIN_M;
        let off = glam::Vec2::new(self.target.x - center.x, self.target.y - center.y);
        if off.length() > margin {
            let o = off.normalize() * margin;
            self.target.x = center.x + o.x;
            self.target.y = center.y + o.y;
        }
    }

    /// Switch views keeping the framing roughly continuous.
    pub fn set_mode(&mut self, mode: ViewMode) {
        if self.mode == mode {
            return;
        }
        match mode {
            ViewMode::Orbit => {
                self.distance = (self.plan_half_h / (FOV_Y / 2.0).tan() * 1.1)
                    .clamp(MIN_DISTANCE, MAX_DISTANCE);
            }
            ViewMode::Plan => {
                self.plan_half_h =
                    (self.distance * (FOV_Y / 2.0).tan()).clamp(MIN_HALF_H, MAX_HALF_H);
            }
            ViewMode::Elevation => {}
        }
        self.mode = mode;
    }

    /// Face a wall: orthographic view along `frame.n`, framing the
    /// `length` × `height` face with some margin.
    pub fn enter_elevation(&mut self, frame: ElevationFrame, length: f32, height: f32, aspect: f32) {
        self.mode = ViewMode::Elevation;
        self.elevation = Some(frame);
        self.target = frame.origin + frame.u * (length / 2.0) + Vec3::Z * (height / 2.0);
        let half_h = (height / 2.0).max(length / 2.0 / aspect.max(0.1));
        self.elevation_half_h = (half_h * 1.3 + 0.3).clamp(MIN_HALF_H, MAX_HALF_H);
    }

    /// Ray intersection with an arbitrary plane (point + normal).
    pub fn unproject_to(
        &self,
        px: f32,
        py: f32,
        w: f32,
        h: f32,
        origin: Vec3,
        normal: Vec3,
    ) -> Option<Vec3> {
        let (o, d) = self.ray(px, py, w, h)?;
        let denom = d.dot(normal);
        if denom.abs() <= 1e-6 {
            return None;
        }
        let t = (origin - o).dot(normal) / denom;
        let hit = o + d * t;
        (t >= 0.0 && (hit - o).length() < 5_000.0).then_some(hit)
    }

    /// Canvas pixel -> world ray (origin, unit direction).
    pub fn ray(&self, px: f32, py: f32, w: f32, h: f32) -> Option<(Vec3, Vec3)> {
        let ndc_x = px / w * 2.0 - 1.0;
        let ndc_y = 1.0 - py / h * 2.0;
        let inv = self.view_proj(w / h).inverse();
        let near = inv * Vec4::new(ndc_x, ndc_y, 0.0, 1.0);
        let far = inv * Vec4::new(ndc_x, ndc_y, 1.0, 1.0);
        if near.w.abs() <= 1e-12 || far.w.abs() <= 1e-12 {
            return None;
        }
        let a = near.truncate() / near.w;
        let b = far.truncate() / far.w;
        let dir = (b - a).normalize_or_zero();
        (dir != Vec3::ZERO).then_some((a, dir))
    }

    /// Canvas pixel -> intersection with the horizontal plane z = `z`.
    pub fn unproject_to_plane(&self, px: f32, py: f32, w: f32, h: f32, z: f32) -> Option<Vec3> {
        let (o, d) = self.ray(px, py, w, h)?;
        if d.z.abs() <= 1e-6 {
            return None;
        }
        let t = (z - o.z) / d.z;
        if t < 0.0 {
            return None;
        }
        let hit = o + d * t;
        // Reject grazing hits far beyond the horizon.
        ((hit - o).length() < 5_000.0).then_some(hit)
    }

    /// World -> canvas pixels (None behind the camera).
    pub fn project(&self, world: Vec3, w: f32, h: f32) -> Option<(f32, f32)> {
        let clip = self.view_proj(w / h) * Vec4::new(world.x, world.y, world.z, 1.0);
        if clip.w <= 1e-6 {
            return None;
        }
        let ndc = clip / clip.w;
        Some(((ndc.x * 0.5 + 0.5) * w, (0.5 - ndc.y * 0.5) * h))
    }
}

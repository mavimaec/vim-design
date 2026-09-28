//! The authoring app (wasm only) behind `www/app.html` — the GitHub
//! Pages single-page application.
//!
//! Owns the `Document` + `Engine` pair per the facade contract
//! (eval::mod.rs), the shared wgpu renderer, the plan/3D camera, CPU
//! picking, and the sketch tools. JS owns the DOM and input decoding:
//! it forwards canvas positions in DEVICE pixels, and repaints its
//! panels when [`AuthorApp::take_params_dirty`] fires.
//!
//! Rules this module follows:
//! - The document is the single source of truth. The element list is
//!   DERIVED from it (`authoring::model::derive`) after every poll whose
//!   `params_changed` is non-empty — undo, redo, reload, and import need
//!   no special cases.
//! - Every model-changing user action is ONE gesture group = one undo
//!   step (`crate::gestures`).
//! - Active level, tool, sketch, snap settings, camera, and selection
//!   are SESSION state: never in the document, never undoable.
//! - Interest filter: none (`set_params_watch(None)`). The derived
//!   element list depends on arbitrary construction entities (a hole's
//!   control points, a path end point), so the honest watch set is the
//!   whole document; re-derivation is O(entities) and cheap at app
//!   scale.

mod camera;
mod clipboard;
mod span;
mod defaults;
mod edit;
mod pick;
mod openings;
mod planes;
mod rooms;
mod walls;

use std::collections::{BTreeMap, HashMap};

use glam::Vec3;
use vim_design_lib::eval::Engine;
use vim_design_lib::{Command, Document, EntityId, EntityKind, Params, VimStatus};
use wasm_bindgen::prelude::*;

use crate::authoring::geom::{Invalid, P2, dist, signed_area};
use crate::authoring::model::{self, ElementModel, LegacyWallModel, PlateModel, WallLine, WallModel};
use crate::authoring::ops;
use crate::authoring::sketch::{
    self, PlaceOutcome, PlateOutline, Shape, Sketch, SketchContext, SketchTool,
};
use crate::authoring::snap::{self, SnapInput};
use crate::gestures::Gestures;
use crate::render::{LineLayer, MeshStyle, Renderer, RendererOptions};

use camera::{Camera, ElevationFrame, ViewMode};
use edit::{EditPointer, EditProfile, EditTarget, EditTool};
use crate::authoring::edit::ProfileModel;
use crate::authoring::edit::session::EditSession;
use pick::PickScene;

/// sRGB hex component -> linear.
fn lin(c: u8) -> f32 {
    let c = f32::from(c) / 255.0;
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

fn rgb(hex: u32) -> [f32; 3] {
    [lin((hex >> 16) as u8), lin((hex >> 8) as u8), lin(hex as u8)]
}

fn rgba(hex: u32, a: f32) -> [f32; 4] {
    let [r, g, b] = rgb(hex);
    [r, g, b, a]
}

// Palette (sRGB hex; linearized at use). Calm light theme.
const CLEAR: u32 = 0xeef0f3;
const CONCRETE: u32 = 0xcfc9bf;
/// Walls: warm off-white, lighter and warmer than the concrete plates.
const WALL: u32 = 0xfbf7f0;
/// Walls cut by the plan view: a flat, dark "poché" fill.
const POCHE: u32 = 0x5b6270;
const OTHER_ELEMENT: u32 = 0xc2c8d0;
const EDGE: u32 = 0x2b3340;
const ACCENT: u32 = 0x2f6fed;
const GRID: u32 = 0x1e293b;
const AXIS_X: u32 = 0xe5484d;
const AXIS_Y: u32 = 0x2fa36b;

/// The wireframe alone: a strong blue-slate (linear), nearly opaque.
const WIRE_ONLY_COLOR: [f32; 4] = [0.05, 0.11, 0.32, 0.85];
/// Grid lines float this far above the active plane so they stay
/// visible on plate tops (which sit exactly on the plane).
const GRID_LIFT_M: f32 = 0.002;
const GRID_STEPS: [f64; 11] = [0.1, 0.25, 0.5, 1.0, 2.0, 5.0, 10.0, 20.0, 50.0, 100.0, 200.0];
/// Grid step on a wall face in an elevation view (openings are placed
/// more finely than plans).
const ELEVATION_GRID_STEP_M: f64 = 0.1;
/// Plates are pushed back this much (clip depth) in orthographic views,
/// where the plan cut exposes wall bottoms coplanar with plate tops.
const PLATE_DEPTH_BIAS: f32 = 2e-5;
/// A tie between pick hits within this distance (meters) goes to the
/// wall (walls stand on plates: their bottoms coincide with plate tops).
const PICK_TIE_M: f32 = 0.005;
/// Default wall height, thickness, and limits (meters).
const DEFAULT_WALL_HEIGHT_M: f64 = 2.7;
/// New walls: an interior partition (see `PARTITION_THICKNESS_M`).
const DEFAULT_WALL_THICKNESS_M: f64 = crate::authoring::walls::PARTITION_THICKNESS_M;
const MIN_WALL_HEIGHT_M: f64 = 0.1;
const MAX_WALL_HEIGHT_M: f64 = 50.0;
const MIN_WALL_THICKNESS_M: f64 = 0.02;
const MAX_WALL_THICKNESS_M: f64 = 2.0;

/// Cache key of the generated grid: (minor bits, major bits, x0, x1,
/// y0, y1 in major units, plane z bits, plan flag).
type GridKey = (u64, u64, i64, i64, i64, i64, u32, u8);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tool {
    Select,
    Plate,
    Hole,
    Wall,
    /// Rooms (preview, `?rooms`): a room boundary.
    Room,
}

impl Tool {
    fn name(self) -> &'static str {
        match self {
            Tool::Select => "select",
            Tool::Plate => "plate",
            Tool::Hole => "hole",
            Tool::Wall => "wall",
            Tool::Room => "room",
        }
    }
}

/// The plane a sketch lives on: world = origin + u·p[0] + v·p[1].
#[derive(Debug, Clone, Copy)]
struct SketchFrame {
    origin: Vec3,
    u: Vec3,
    v: Vec3,
}

impl SketchFrame {
    fn world(&self, p: P2) -> Vec3 {
        self.origin + self.u * p[0] as f32 + self.v * p[1] as f32
    }

    fn local(&self, w: Vec3) -> P2 {
        let d = w - self.origin;
        [f64::from(d.dot(self.u)), f64::from(d.dot(self.v))]
    }

    fn normal(&self) -> Vec3 {
        self.u.cross(self.v)
    }
}

/// What a sketch snaps to (see `snap::snap`).
#[derive(Default)]
struct SnapSources {
    vertices: Vec<P2>,
    edges: Vec<(P2, P2)>,
    align: Vec<P2>,
    step: f64,
}

fn now_ms() -> f64 {
    web_sys::window()
        .and_then(|w| w.performance())
        .map(|p| p.now())
        .unwrap_or(0.0)
}

fn fmt_m(v: f64) -> String {
    let s = format!("{v:.2}");
    if s == "-0.00" { "0.00".to_owned() } else { s.replace('-', "\u{2212}") }
}

fn eid(id: f64) -> EntityId {
    EntityId(if id.is_finite() && id > 0.0 { id as u64 } else { 0 })
}

#[wasm_bindgen]
pub struct AuthorApp {
    doc: Document,
    engine: Engine,
    renderer: Renderer,
    camera: Camera,
    gestures: Gestures,
    pick: PickScene,
    errors: BTreeMap<EntityId, String>,
    /// Derived element model (never authoritative).
    model: Vec<ElementModel>,
    params_dirty: bool,
    /// Session state: never in the document, never undoable.
    active_level: Option<EntityId>,
    active_elevation: f64,
    /// The active construction plane when it is not the active level
    /// itself (a workplane); `None` means "draw on the active level".
    active_plane: Option<EntityId>,
    selection: Option<EntityId>,
    tool: Tool,
    /// Outline shape per drawing tool family.
    shape: Shape,
    wall_shape: Shape,
    room_shape: Shape,
    sketch: Option<Sketch>,
    /// Settings for NEW walls.
    wall_height: f64,
    wall_thickness: f64,
    wall_flip: bool,
    /// Height mode of new walls: `None` = a fixed height; `Some(plane)` =
    /// up to that plane, plus `wall_top_offset`.
    wall_top: Option<EntityId>,
    wall_top_offset: f64,
    /// The camera to return to after a wall's Edit Mode (elevation).
    prev_camera: Option<Camera>,
    /// Edit Mode: the profile session, the active edit tool, and the
    /// pointer state of a move drag.
    edit: Option<EditSession<EditProfile>>,
    edit_tool: EditTool,
    edit_pointer: EditPointer,
    /// The sketch entity being edited (kept when undone away, so redo
    /// finds it again), and the element the session started on (`None`
    /// for a new plate).
    edit_sketch: Option<EntityId>,
    edit_entry_element: Option<EntityId>,
    /// Floor plane or wall elevation.
    edit_target: EditTarget,
    /// Canvas margins covered by the page's panels (device px: left,
    /// top, right, bottom).
    view_insets: [f32; 4],
    /// A wall run's Edit Mode (its walls and undo snapshots).
    run_edit: Option<edit::RunEdit>,
    /// Openings mode (placing windows and doors).
    openings: Option<openings::OpeningsSession>,
    snap_enabled: bool,
    snap_step: f64,
    plate_thickness: f64,
    /// Copy / paste: the copy, and whether a paste is armed (session
    /// state; see `clipboard`).
    clipboard: Option<crate::authoring::clipboard::Clip>,
    paste_armed: bool,
    /// Rooms (see `rooms`): the derived rooms and the wall graph of each
    /// layout (never authoritative), the selected room (session state; a
    /// room is not an element), the room in Room Edit Mode.
    rooms: Vec<crate::authoring::model::RoomModel>,
    room_graphs: Vec<rooms::LayoutGraph>,
    room_selection: Option<EntityId>,
    room_edit: Option<EntityId>,
    /// The see-through bands of the active level's plan span (session
    /// toggle; see `span`).
    plan_span_on: bool,
    /// The wall run just drawn (see `walls::fresh_run`).
    fresh_run: Option<EntityId>,
    /// Remembered settings for new items (see `defaults`).
    remembered: defaults::Remembered,
    /// Bumped on every committed document change and on replacement —
    /// the page persists when it changes.
    revision: u64,
    last_committed: u64,
    grid_key: Option<GridKey>,
    last_op: String,
    last_latency_ms: f64,
    last_mesh_upserts: usize,
    last_base_transforms: usize,
    committed: u64,
    evaluated: u64,
    pending: usize,
    notice: Option<String>,
}

#[wasm_bindgen]
impl AuthorApp {
    /// Initialize wgpu on the canvas and start a new project (Site =
    /// Montreal, levels Ground + Level 2). The page then loads the
    /// persisted document, if any, via [`AuthorApp::load_document`].
    pub async fn create(canvas_id: String) -> Result<AuthorApp, JsValue> {
        // A panic reaches the console with its message (wasm otherwise
        // reports only "unreachable").
        std::panic::set_hook(Box::new(|info| web_sys::console::error_1(&JsValue::from_str(&format!("panic: {info}")))));
        let document = web_sys::window()
            .and_then(|w| w.document())
            .ok_or_else(|| JsValue::from_str("no DOM document"))?;
        let canvas = document
            .get_element_by_id(&canvas_id)
            .ok_or_else(|| JsValue::from_str("canvas not found"))?
            .dyn_into::<web_sys::HtmlCanvasElement>()?;
        let mut renderer = Renderer::with_options(canvas, RendererOptions { msaa: true })
            .await
            .map_err(|e| JsValue::from_str(&e))?;
        renderer.wireframe = false;
        let [r, g, b] = rgb(CLEAR);
        renderer.set_clear_color([f64::from(r), f64::from(g), f64::from(b)]);
        let mut app = AuthorApp {
            doc: Document::new(),
            engine: Engine::new(),
            renderer,
            camera: Camera::default(),
            gestures: Gestures::default(),
            pick: PickScene::default(),
            errors: BTreeMap::new(),
            model: Vec::new(),
            params_dirty: true,
            active_level: None,
            active_elevation: 0.0,
            active_plane: None,
            selection: None,
            tool: Tool::Select,
            shape: Shape::Polygon,
            wall_shape: Shape::Polygon,
            room_shape: Shape::Rect,
            sketch: None,
            wall_height: DEFAULT_WALL_HEIGHT_M,
            wall_thickness: DEFAULT_WALL_THICKNESS_M,
            wall_flip: false,
            wall_top: None,
            wall_top_offset: 0.0,
            prev_camera: None,
            edit: None,
            edit_tool: EditTool::Select,
            edit_pointer: EditPointer::default(),
            edit_sketch: None,
            edit_entry_element: None,
            edit_target: EditTarget::Plane,
            run_edit: None,
            openings: None,
            view_insets: [0.0; 4],
            snap_enabled: true,
            snap_step: 0.25,
            plate_thickness: crate::authoring::edit::session::DEFAULT_SOLID_THICKNESS_M,
            remembered: defaults::Remembered::default(),
            fresh_run: None,
            plan_span_on: true,
            clipboard: None,
            paste_armed: false,
            rooms: Vec::new(),
            room_graphs: Vec::new(),
            room_selection: None,
            room_edit: None,
            revision: 0,
            last_committed: 0,
            grid_key: None,
            last_op: String::new(),
            last_latency_ms: 0.0,
            last_mesh_upserts: 0,
            last_base_transforms: 0,
            committed: 0,
            evaluated: 0,
            pending: 0,
            notice: None,
        };
        app.new_project();
        Ok(app)
    }

    // -- Document lifecycle ---------------------------------------------

    /// Replace the document with a fresh project (Site + two levels).
    pub fn new_project(&mut self) {
        let mut doc = Document::new();
        if let Err(e) = ops::seed_new_project(&mut doc) {
            web_sys::console::error_1(&JsValue::from_str(&e));
        }
        self.replace_document(doc, "new project");
        self.camera = Camera::default();
    }

    /// Validate bytes as a document without touching the current one.
    /// Returns "" when loadable, else a short reason.
    pub fn check_document(&self, bytes: &[u8]) -> String {
        match Document::load(bytes) {
            Ok(_) => String::new(),
            Err(status) => describe_load_error(status),
        }
    }

    /// Replace the current document with the given VIMD bytes. Returns
    /// "" on success, else a short reason (the current document is kept).
    pub fn load_document(&mut self, bytes: &[u8]) -> String {
        match Document::load(bytes) {
            Ok(doc) => {
                self.replace_document(doc, "load");
                String::new()
            }
            Err(status) => describe_load_error(status),
        }
    }

    /// The document as VIMD bytes (Uint8Array on the JS side).
    pub fn save_document(&self) -> Result<Vec<u8>, JsValue> {
        self.doc
            .save()
            .map_err(|s| JsValue::from_str(&format!("save failed: {s:?}")))
    }

    /// Changes whenever the document content may have changed.
    pub fn revision(&self) -> f64 {
        self.revision as f64
    }

    // -- Frame loop -------------------------------------------------------

    pub fn render(&mut self) -> Result<(), JsValue> {
        self.camera.plane_z = self.active_elevation as f32;
        self.apply_span();
        self.update_grid();
        let (eye, fraction) = self.camera.edge_nudge();
        self.renderer.set_edge_nudge(eye, fraction);
        let view_proj = self.camera.view_proj(self.renderer.aspect());
        self.renderer.render(view_proj).map_err(|e| JsValue::from_str(&e))
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        self.renderer.resize(width, height);
    }

    // -- View ---------------------------------------------------------------

    /// "plan", "3d", or (in Openings mode) "elevation": head-on to the
    /// wall being worked on.
    pub fn set_view_mode(&mut self, mode: &str) {
        if mode == "elevation" {
            if self.faced_frame().is_some() {
                if self.camera.mode != ViewMode::Elevation && self.prev_camera.is_none() {
                    self.prev_camera = Some(self.camera.clone());
                }
                self.face_wall();
                self.refresh_styles();
            }
            return;
        }
        let mode = if mode == "3d" { ViewMode::Orbit } else { ViewMode::Plan };
        if self.camera.mode == ViewMode::Elevation {
            // Back from an elevation: the view from before it.
            if let Some(c) = self.prev_camera.take() {
                self.camera = c;
            }
            self.camera.elevation = None;
            self.grid_key = None;
        }
        self.camera.set_mode(mode);
        self.refresh_styles();
    }

    /// "plan", "3d", or "elevation".
    pub fn view_mode(&self) -> String {
        self.camera.mode.name().to_owned()
    }

    pub fn orbit(&mut self, dx: f32, dy: f32) {
        self.camera.orbit(dx, dy);
    }

    /// Pan so the view-plane point under (x0, y0) moves under (x1, y1).
    pub fn pan(&mut self, x0: f32, y0: f32, x1: f32, y1: f32) {
        let (_, h) = self.size_f();
        match (self.view_plane_hit(x0, y0), self.view_plane_hit(x1, y1)) {
            (Some(a), Some(b)) if (a - b).length() < 1_000.0 => self.camera.pan_plane(a, b),
            _ => self.camera.pan_pixels(x1 - x0, y1 - y0, h),
        }
        self.clamp_camera();
    }

    /// Zoom by `factor` (< 1 zooms in) toward the canvas point.
    pub fn zoom_at(&mut self, factor: f32, px: f32, py: f32) {
        let anchor = self.view_plane_hit(px, py);
        self.camera.zoom(factor.clamp(0.2, 5.0), anchor);
        self.clamp_camera();
    }

    /// Frame the focus: the element being edited, else the selection,
    /// else all geometry (the active plane's square when empty); in an
    /// elevation view, the faced wall.
    pub fn zoom_fit(&mut self) {
        let (w, h) = self.size_f();
        let aspect = w / h;
        if self.camera.mode == ViewMode::Elevation && self.faced_frame().is_some() {
            self.face_wall();
            return;
        }
        let _ = aspect;
        match self.focus_bbox().or_else(|| self.renderer.scene_bbox()) {
            Some((min, max)) => {
                let v = |a: [f64; 3]| Vec3::new(a[0] as f32, a[1] as f32, a[2] as f32);
                self.fit_box(v(min), v(max));
            }
            None => {
                let z = self.active_elevation as f32;
                self.fit_box(Vec3::new(-6.0, -6.0, z), Vec3::new(6.0, 6.0, z));
            }
        }
    }

    /// The canvas margins (device pixels) covered by the page's panels:
    /// fitting frames into the rest.
    pub fn set_view_insets(&mut self, left: f32, top: f32, right: f32, bottom: f32) {
        let clean = |v: f32| if v.is_finite() { v.max(0.0) } else { 0.0 };
        self.view_insets = [clean(left), clean(top), clean(right), clean(bottom)];
    }

    /// Camera state for session persistence (JSON).
    pub fn camera_json(&self) -> String {
        // During window drawing, persist the view to return to.
        let c = self.prev_camera.as_ref().unwrap_or(&self.camera);
        serde_json::json!({
            "mode": c.mode.name(),
            "tx": c.target.x, "ty": c.target.y,
            "yaw": c.yaw, "pitch": c.pitch,
            "distance": c.distance, "halfH": c.plan_half_h,
        })
        .to_string()
    }

    /// Restore [`AuthorApp::camera_json`] output; ignores bad input.
    pub fn set_camera_json(&mut self, json: &str) {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else {
            return;
        };
        let f = |k: &str| v.get(k).and_then(|x| x.as_f64()).filter(|x| x.is_finite());
        let c = &mut self.camera;
        if let (Some(tx), Some(ty)) = (f("tx"), f("ty")) {
            c.target.x = tx as f32;
            c.target.y = ty as f32;
        }
        if let Some(yaw) = f("yaw") {
            c.yaw = yaw as f32;
        }
        if let Some(pitch) = f("pitch") {
            c.pitch = (pitch as f32).clamp(-0.35, 1.53);
        }
        if let Some(d) = f("distance") {
            c.distance = (d as f32).clamp(1.0, 600.0);
        }
        if let Some(hh) = f("halfH") {
            c.plan_half_h = (hh as f32).clamp(0.5, 400.0);
        }
        if let Some(mode) = v.get("mode").and_then(|m| m.as_str()) {
            c.mode = if mode == "3d" { ViewMode::Orbit } else { ViewMode::Plan };
        }
        self.clamp_camera();
        self.refresh_styles();
    }

    // -- Display ------------------------------------------------------------

    /// Render mode (session state): "shaded", "shaded-wire" (the
    /// triangle edges over the shading), or "wire" (the triangle edges
    /// alone). The wireframe shows the real mesh topology.
    pub fn set_render_mode(&mut self, mode: &str) {
        let (shaded, wire) = match mode {
            "shaded-wire" => (true, true),
            "wire" => (false, true),
            _ => (true, false),
        };
        self.renderer.shaded = shaded;
        self.renderer.wireframe = wire;
        // Alone, the wireframe is stronger and the feature edges would
        // only hide it.
        self.renderer.feature_edges = shaded;
        self.renderer.wire_color = if shaded { crate::render::DEFAULT_WIRE_COLOR } else { WIRE_ONLY_COLOR };
    }

    pub fn render_mode(&self) -> String {
        match (self.renderer.shaded, self.renderer.wireframe) {
            (true, true) => "shaded-wire",
            (false, _) => "wire",
            _ => "shaded",
        }
        .to_owned()
    }

    /// Old session flag: the wireframe over the shading.
    pub fn set_wireframe(&mut self, enabled: bool) {
        self.set_render_mode(if enabled { "shaded-wire" } else { "shaded" });
    }

    pub fn wireframe(&self) -> bool {
        self.renderer.wireframe
    }

    // -- Tools --------------------------------------------------------------

    /// Activate a tool: "select", "plate", "hole", or "wall". Drawing
    /// tools need an active level (`can_author`). (Windows and doors have
    /// their own mode: [`AuthorApp::openings_begin`].) Returns false when
    /// refused.
    pub fn set_tool(&mut self, tool: &str) -> bool {
        let tool = match tool {
            "plate" => Tool::Plate,
            "hole" => Tool::Hole,
            "wall" => Tool::Wall,
            "room" => Tool::Room,
            _ => Tool::Select,
        };
        if tool != Tool::Select && !self.can_author() {
            return false;
        }
        self.tool = tool;
        self.paste_armed = false; // another tool ends a paste
        // The wall just drawn stays selected only while its tool is armed.
        if self.fresh_run.take().is_some_and(|f| self.selection == Some(f)) {
            self.selection = None;
        }
        self.sketch = match (tool, self.plane()) {
            (Tool::Plate, Some(level)) => Some(Sketch::new(SketchTool::Plate, self.shape, level)),
            (Tool::Wall, Some(level)) => Some(Sketch::new(SketchTool::Wall, self.wall_shape, level)),
            (Tool::Room, Some(level)) => Some(Sketch::new(SketchTool::Room, self.room_shape, level)),
            _ => None,
        };
        if tool != Tool::Select {
            self.selection = None;
        }
        self.refresh_styles();
        true
    }

    pub fn tool(&self) -> String {
        self.tool.name().to_owned()
    }

    /// Outline shape of the current tool: "polygon" or "rect" (clears
    /// placed points). Walls: polyline or rectangular room.
    pub fn set_shape(&mut self, shape: &str) {
        let shape = if shape == "rect" { Shape::Rect } else { Shape::Polygon };
        match self.tool {
            Tool::Wall => self.wall_shape = shape,
            Tool::Room => self.room_shape = shape,
            _ => self.shape = shape,
        }
        if let Some(s) = self.sketch.as_mut() {
            s.set_shape(shape);
        }
    }

    pub fn shape(&self) -> String {
        self.current_shape().name().to_owned()
    }

    pub fn set_snap(&mut self, enabled: bool, step: f64) {
        self.snap_enabled = enabled;
        if step.is_finite() && step > 0.0 {
            self.snap_step = step.clamp(0.01, 10.0);
        }
        self.grid_key = None;
    }

    /// Thickness for NEW floor plates (meters).
    pub fn set_plate_thickness_setting(&mut self, t: f64) {
        if t.is_finite() {
            self.plate_thickness = t.clamp(0.02, 5.0);
        }
    }

    pub fn plate_thickness_setting(&self) -> f64 {
        self.plate_thickness
    }

    /// Move the sketch cursor to a canvas point (device pixels) and snap
    /// it. `tol_px` is the capture radius in device pixels (the page
    /// passes a larger one for touch).
    pub fn sketch_hover(&mut self, px: f32, py: f32, tol_px: f32) {
        let Some(sketch) = &self.sketch else { return };
        let Some(frame) = self.sketch_frame() else { return };
        let (w, h) = self.size_f();
        let Some(hit) = self.camera.unproject_to(px, py, w, h, frame.origin, frame.normal()) else {
            if let Some(s) = self.sketch.as_mut() {
                s.cursor = None;
            }
            return;
        };
        let raw = frame.local(hit);
        let ppm = self.px_per_m_at(hit, &frame);
        let sources = self.snap_sources(sketch);
        // Axis anchors apply to polygons only: aligning a rectangle's
        // second corner with its first would collapse it to zero area.
        let polygon = sketch.shape == Shape::Polygon;
        let first = sketch.points.first().copied().filter(|_| polygon);
        let prev = sketch.points.last().copied().filter(|_| polygon);
        let result = snap::snap(&SnapInput {
            raw,
            enabled: self.snap_enabled,
            step: sources.step,
            tolerance: f64::from(tol_px / ppm.max(1e-3)),
            close_target: sketch.close_target(),
            prev,
            first,
            vertices: &sources.vertices,
            edges: &sources.edges,
            align: &sources.align,
        });
        if let Some(s) = self.sketch.as_mut() {
            s.cursor = Some(result);
        }
    }

    /// The pointer left the canvas: hide the cursor.
    pub fn sketch_leave(&mut self) {
        if let Some(s) = self.sketch.as_mut() {
            s.cursor = None;
        }
    }

    /// Place a vertex at the current cursor. Returns JSON
    /// `{"result": "added"|"duplicate"|"none"|"committed"|"rejected",
    ///   "reason": ..., "count": n, "name": ...}`. Tapping the first
    /// vertex closes the outline (a wall run becomes a closed loop).
    pub fn sketch_place(&mut self) -> String {
        let Some(sketch) = self.sketch.as_mut() else {
            return r#"{"result":"none","count":0}"#.to_owned();
        };
        let outcome = sketch.place();
        match outcome {
            PlaceOutcome::Added if sketch.tool == SketchTool::Split && sketch.points.len() >= 2 => {
                self.finish_sketch(false)
            }
            PlaceOutcome::Added => {
                let status = self.sketch_status();
                let reason = status.and_then(|s| s.reason).map(|r| r.message());
                serde_json::json!({
                    "result": "added",
                    "count": self.sketch_count(),
                    "reason": reason,
                })
                .to_string()
            }
            PlaceOutcome::Duplicate => {
                serde_json::json!({"result": "duplicate", "count": self.sketch_count()}).to_string()
            }
            PlaceOutcome::NoCursor => {
                serde_json::json!({"result": "none", "count": self.sketch_count()}).to_string()
            }
            PlaceOutcome::CloseRequested => self.finish_sketch(true),
            PlaceOutcome::RectComplete => {
                let out = self.finish_sketch(true);
                // A rejected rectangle keeps its first corner so the user
                // can simply try the second corner again.
                if let Some(s) = self.sketch.as_mut() {
                    s.points.truncate(1);
                }
                out
            }
        }
    }

    pub fn sketch_undo_point(&mut self) -> bool {
        self.sketch.as_mut().is_some_and(|s| s.undo_point())
    }

    /// Commit the outline if valid (Finish button / Enter). A wall
    /// polyline is committed as an OPEN run. Returns JSON like
    /// `sketch_place`.
    pub fn sketch_finish(&mut self) -> String {
        self.finish_sketch(false)
    }

    /// Discard the in-progress outline (the tool stays active). Returns
    /// the number of points that were discarded.
    pub fn sketch_cancel(&mut self) -> u32 {
        self.sketch.as_mut().map_or(0, |s| {
            let n = s.points.len() as u32;
            s.points.clear();
            n
        })
    }

    /// Screen-space sketch overlay for the page's 2D HUD canvas (device
    /// pixels): placed points, preview outline, cursor + snap kind,
    /// guides, live segment length, wall footprints, validity.
    pub fn hud_json(&self) -> String {
        let (Some(sk), Some(frame)) = (&self.sketch, self.sketch_frame()) else {
            return r#"{"active":false}"#.to_owned();
        };
        let (w, h) = self.size_f();
        let proj = |p: P2| -> Option<[f32; 2]> {
            self.camera.project(frame.world(p), w, h).map(|(x, y)| [x, y])
        };
        let points: Vec<[f32; 2]> = sk.points.iter().filter_map(|p| proj(*p)).collect();
        let preview_uv = sk.preview();
        let preview: Vec<[f32; 2]> = preview_uv.iter().filter_map(|p| proj(*p)).collect();
        // On a wall face: wall-local u along, v up.
        let facing = self.camera.mode == ViewMode::Elevation;
        let (lu, lv) = if facing { ("u", "v") } else { ("x", "y") };
        let cursor = sk.cursor.as_ref().and_then(|c| {
            proj(c.point).map(|[x, y]| {
                serde_json::json!({
                    "x": x, "y": y, "u": c.point[0], "v": c.point[1],
                    "kind": c.kind.name(),
                    "label": format!("{lu} {} · {lv} {}", fmt_m(c.point[0]), fmt_m(c.point[1])),
                })
            })
        });
        let guides: Vec<[f32; 4]> = sk
            .cursor
            .as_ref()
            .map(|c| {
                c.guides
                    .iter()
                    .filter_map(|(a, b)| match (proj(*a), proj(*b)) {
                        (Some(a), Some(b)) => Some([a[0], a[1], b[0], b[1]]),
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default();
        let seg = match (sk.shape, sk.points.as_slice(), sk.cursor.as_ref()) {
            (Shape::Polygon, [.., last], Some(c)) if dist(*last, c.point) > 1e-6 => {
                let mid = [(last[0] + c.point[0]) / 2.0, (last[1] + c.point[1]) / 2.0];
                proj(mid).map(|[x, y]| {
                    serde_json::json!({"x": x, "y": y,
                        "text": format!("{} m", fmt_m(dist(*last, c.point)))})
                })
            }
            (Shape::Rect, [a], Some(c)) => {
                let mid = [(a[0] + c.point[0]) / 2.0, (a[1] + c.point[1]) / 2.0];
                proj(mid).map(|[x, y]| {
                    serde_json::json!({"x": x, "y": y, "text": format!("{} × {} m",
                        fmt_m((c.point[0] - a[0]).abs()), fmt_m((c.point[1] - a[1]).abs()))})
                })
            }
            _ => None,
        };
        // Wall footprints of the preview run: shows which side the
        // thickness grows on.
        let bands: Vec<Vec<[f32; 2]>> = if sk.tool == SketchTool::Wall {
            let closing = sk.cursor.as_ref().is_some_and(|c| c.kind == snap::SnapKind::First);
            let (run, closed) = match sk.shape {
                Shape::Rect => (preview_uv.clone(), true),
                Shape::Polygon if closing => (sk.points.clone(), true),
                Shape::Polygon => (preview_uv.clone(), false),
            };
            // The run as the library will build it: mitered.
            let pts = crate::authoring::geom::dedup_closed(&run);
            if pts.len() >= 2 {
                let data = crate::authoring::runs::new_run(&pts, closed, self.wall_flip, self.wall_thickness, 1.0, 0.0);
                crate::authoring::runs::footprint(&data)
                    .iter()
                    .map(|ring| ring.iter().filter_map(|p| proj(*p)).collect())
                    .collect()
            } else {
                Vec::new()
            }
        } else {
            Vec::new()
        };
        let status = self.sketch_status();
        let first = sk.close_target().and_then(proj);
        serde_json::json!({
            "active": true,
            "tool": sk.tool.name(),
            "shape": sk.shape.name(),
            "count": sk.points.len(),
            "points": points,
            "preview": preview,
            "closed": sk.shape == Shape::Rect || preview_uv.len() >= 3,
            "fill": sk.tool != SketchTool::Wall,
            "closeHint": sk.tool != SketchTool::Wall,
            "bands": bands,
            "cursor": cursor,
            "guides": guides,
            "seg": seg,
            "first": first,
            "canFinish": status.as_ref().is_some_and(|s| s.can_finish),
            "previewOk": status.as_ref().is_none_or(|s| s.preview_ok),
            "reason": status.as_ref().and_then(|s| s.reason).map(|r| r.message()),
            "reasonCode": status.as_ref().and_then(|s| s.reason).map(|r| r.code()),
        })
        .to_string()
    }

    /// Sketch state for tests: placed points in plane coordinates.
    pub fn sketch_json(&self) -> String {
        match &self.sketch {
            Some(s) => serde_json::json!({
                "active": true,
                "tool": s.tool.name(),
                "shape": s.shape.name(),
                "points": s.points,
                "cursor": s.cursor.as_ref().map(|c| serde_json::json!({
                    "u": c.point[0], "v": c.point[1], "kind": c.kind.name()})),
            })
            .to_string(),
            None => r#"{"active":false}"#.to_owned(),
        }
    }

    // -- Selection ----------------------------------------------------------

    /// Pick the element under a canvas point (device pixels): its id, or
    /// -1 for empty space.
    pub fn pick(&self, px: f32, py: f32) -> f64 {
        let (w, h) = self.size_f();
        let Some((origin, dir)) = self.camera.ray(px, py, w, h) else {
            return -1.0;
        };
        let hits = self.pick.pick_all(origin, dir, &|p| self.pickable_z(p.z));
        let Some(&(nearest, d0)) = hits.first() else {
            return -1.0;
        };
        hits.iter()
            .take_while(|(_, d)| *d <= d0 + PICK_TIE_M)
            .find(|(id, _)| self.is_wall(*id))
            .map_or(nearest, |(id, _)| *id)
            .0 as f64
    }

    /// The Model tree: story levels, top first, each with its nested
    /// construction planes and its elements grouped by category (Floors,
    /// Walls, Other). Rows carry what the page shows (name, meta) and
    /// whether the element opens in Edit Mode.
    pub fn tree_json(&self) -> String {
        let mut levels = ops::levels_sorted(&self.doc);
        levels.reverse();
        let workplanes = ops::workplanes(&self.doc);
        let groups_of = |level: EntityId| -> Vec<serde_json::Value> {
            let mut floors = Vec::new();
            let mut walls = Vec::new();
            let mut rooms = Vec::new();
            let mut other = Vec::new();
            for e in self.model.iter().filter(|e| e.level() == Some(level)) {
                let (bucket, meta, edit) = match e {
                    ElementModel::SketchPlate(p) => {
                        let area = self.pick.owner_stats(p.element).map_or(0.0, |s| s.top_area);
                        let faces = p.sketch.faces.len();
                        let meta = format!("{:.1} m² · {} face{}", area, faces, if faces == 1 { "" } else { "s" });
                        (&mut floors, meta, true)
                    }
                    ElementModel::Plate(p) => (&mut floors, format!("{:.1} m² · {:.2} m", p.area, p.thickness), true),
                    ElementModel::Run(r) => {
                        let n = r.data.segment_count();
                        let meta = format!("{} segment{} · h {:.2} m", n, if n == 1 { "" } else { "s" }, r.top_height);
                        (&mut walls, meta, true)
                    }
                    ElementModel::Wall(w) => {
                        let meta = format!("{:.2} m · h {:.2} m", w.length(), w.top_height);
                        (&mut walls, meta, true)
                    }
                    ElementModel::LegacyWall(w) => {
                        let meta = format!("{:.2} m · h {:.2} m", w.length(), w.height);
                        (&mut walls, meta, true)
                    }
                    ElementModel::RoomWalls(w) => {
                        let meta = format!("{:.3} m · h {:.2} m", w.data.thickness_m, w.top_height);
                        (&mut walls, meta, false)
                    }
                    ElementModel::Other(_) => (&mut other, String::new(), false),
                };
                bucket.push(serde_json::json!({
                    "id": e.element().0 as f64,
                    "name": e.name(),
                    "kind": e.kind_name(),
                    "meta": meta,
                    "editable": edit,
                    "selected": self.selection == Some(e.element()),
                }));
            }
            // Rooms (not elements) under their plane's root level, top of
            // each layout's order first, after the "Room walls" rows.
            rooms.extend(self.room_tree_rows(level));
            [("floors", "Floors", floors), ("walls", "Walls", walls), ("rooms", "Rooms", rooms), ("other", "Other", other)]
                .into_iter()
                .filter(|(_, _, items)| !items.is_empty())
                .map(|(key, label, items)| serde_json::json!({ "key": key, "label": label, "items": items }))
                .collect()
        };
        let rows: Vec<serde_json::Value> = levels
            .iter()
            .map(|l| {
                serde_json::json!({
                    "id": l.id.0 as f64,
                    "name": l.name,
                    "elevation": l.elevation_m,
                    "isStory": l.is_building_story,
                    "color": l.color,
                    "active": self.plane() == Some(l.id),
                    // Nested construction planes (workplanes), recursive.
                    "planes": self.plane_rows(&workplanes, l.id),
                    "groups": groups_of(l.id),
                })
            })
            .collect();
        serde_json::json!({
            "activeLevel": self.active_level.map(|id| id.0 as f64),
            "activePlane": self.plane().map(|id| id.0 as f64),
            "levels": rows,
        })
        .to_string()
    }

    /// Reveal an element: make its level active and frame its geometry.
    pub fn frame_element(&mut self, id: f64) -> bool {
        let id = eid(id);
        let Some(level) = self.model.iter().find(|e| e.element() == id).and_then(|e| e.level()) else {
            return false;
        };
        if self.plane().and_then(|p| self.root_level(p)) != Some(level) {
            self.set_active_plane(level.0 as f64);
        }
        let Some(stats) = self.pick.owner_stats(id) else { return false };
        let [min, max] = stats.bbox;
        self.fit_box(Vec3::from_array(min), Vec3::from_array(max));
        true
    }

    /// Select an element (-1 clears). Session state: no document change.
    pub fn select(&mut self, id: f64) {
        let id = eid(id);
        self.selection = self.model.iter().any(|e| e.element() == id).then_some(id);
        if self.selection.is_some() {
            self.room_selection = None;
        }
        self.refresh_styles();
    }

    pub fn selection(&self) -> f64 {
        self.selection.map_or(-1.0, |id| id.0 as f64)
    }

    /// Properties of the selected element (JSON, `null` when none).
    pub fn selected_json(&self) -> String {
        let Some(sel) = self.selection else {
            return "null".to_owned();
        };
        self.model
            .iter()
            .find(|e| e.element() == sel)
            .map_or_else(|| "null".to_owned(), |e| self.element_value(e).to_string())
    }

    /// The derived element list (JSON array).
    pub fn elements_json(&self) -> String {
        serde_json::Value::Array(self.model.iter().map(|e| self.element_value(e)).collect())
            .to_string()
    }

    // -- Element edits (one gesture each) -----------------------------------

    pub fn set_element_name(&mut self, id: f64, name: String) {
        let id = eid(id);
        if !self.model.iter().any(|e| e.element() == id) {
            return;
        }
        let coalesce = self.gestures.begin_continuing(&self.doc, &format!("name_{}", id.0));
        self.submit(Command::UpdateElement { id, name: Some(name), members: None, coalesce });
        self.sync("rename");
    }

    /// Edit a floor plate's thickness (coalesced within one gesture;
    /// the page calls [`AuthorApp::end_gesture`] on commit).
    pub fn set_plate_thickness(&mut self, id: f64, thickness: f64) {
        let id = eid(id);
        if !thickness.is_finite() {
            return;
        }
        let t = thickness.clamp(0.02, 5.0);
        let Some(plate) = self.plate(id).cloned() else { return };
        let Some((_, s)) = model::control_point(&self.doc, plate.path_start) else { return };
        let end = if plate.downward { [s[0], s[1], s[2] - t] } else { [s[0], s[1], s[2] + t] };
        let coalesce = self.gestures.begin_continuing(&self.doc, &format!("thickness_{}", id.0));
        self.submit(Command::UpdateControlPoint { id: plate.path_end, position: end, coalesce });
        self.sync("plate thickness");
    }

    /// Close the open continuous edit gesture.
    pub fn end_gesture(&mut self) {
        self.gestures.end();
    }

    /// Remove a hole from a plate and sweep its construction geometry.
    pub fn delete_hole(&mut self, element: f64, wire: f64) -> bool {
        let element = eid(element);
        let face = match (self.plate(element), self.legacy_wall(element)) {
            (Some(p), _) => p.face,
            (None, Some(w)) => w.face,
            (None, None) => return false,
        };
        let depth = self.doc.undo_depth();
        match ops::delete_hole(&mut self.doc, face, eid(wire)) {
            Ok(()) => {
                self.gestures.one_shot(depth);
                self.sync("delete hole");
                true
            }
            Err(e) => {
                ops::rollback_to(&mut self.doc, depth);
                self.gestures.invalidate_redo();
                web_sys::console::error_1(&JsValue::from_str(&e));
                self.sync("delete hole (failed)");
                false
            }
        }
    }

    /// Delete an element with the orphan sweep (one undo step).
    pub fn delete_element(&mut self, id: f64) -> bool {
        let id = eid(id);
        if self.room_walls(id).is_some() {
            // The walls come from the rooms: delete or hide those instead.
            self.notice = Some("Room walls come from their rooms: delete a room, or hide a wall in its Edit Mode".to_owned());
            return false;
        }
        let depth = self.doc.undo_depth();
        match ops::delete_element(&mut self.doc, id) {
            Ok(()) => {
                self.gestures.one_shot(depth);
                if self.selection == Some(id) {
                    self.selection = None;
                }
                self.sync("delete element");
                true
            }
            Err(e) => {
                ops::rollback_to(&mut self.doc, depth);
                self.gestures.invalidate_redo();
                web_sys::console::error_1(&JsValue::from_str(&format!("delete element: {e}")));
                self.sync("delete element (failed)");
                false
            }
        }
    }

    // -- Site + levels ---------------------------------------------------------

    pub fn site_json(&self) -> String {
        match ops::site_params(&self.doc) {
            Some((id, lat, lon, elev, north)) => serde_json::json!({
                "id": id.0 as f64, "latitude": lat, "longitude": lon,
                "elevation": elev, "trueNorth": north,
            })
            .to_string(),
            None => "null".to_owned(),
        }
    }

    pub fn set_site(&mut self, latitude: f64, longitude: f64, elevation: f64) {
        let Some((id, ..)) = ops::site_params(&self.doc) else { return };
        if ![latitude, longitude, elevation].iter().all(|v| v.is_finite()) {
            return;
        }
        let coalesce = self.gestures.begin_continuing(&self.doc, "site");
        self.submit(Command::UpdateSite {
            id,
            latitude_deg: Some(latitude.clamp(-90.0, 90.0)),
            longitude_deg: Some(longitude.clamp(-180.0, 180.0)),
            elevation_m: Some(elevation),
            true_north_deg: None,
            coalesce,
        });
        self.sync("site");
    }

    /// Levels (ascending elevation) plus the session's active level.
    pub fn levels_json(&self) -> String {
        let levels: Vec<serde_json::Value> = ops::levels_sorted(&self.doc)
            .iter()
            .map(|l| {
                let count = self.model.iter().filter(|e| e.level() == Some(l.id)).count();
                serde_json::json!({
                    "id": l.id.0 as f64, "name": l.name, "elevation": l.elevation_m,
                    "isStory": l.is_building_story, "color": l.color, "extent": l.extent_m,
                    "elements": count,
                })
            })
            .collect();
        serde_json::json!({
            "activeId": self.active_level.map(|id| id.0 as f64),
            "activePlane": self.plane().map(|p| self.plane_json(p)),
            "levels": levels,
        })
        .to_string()
    }

    /// Select the active level — SESSION state only (no document change,
    /// no undo step). A sketch in progress moves to the new plane.
    pub fn set_active_level(&mut self, id: f64) -> bool {
        self.set_active_plane(id)
    }

    /// Make a construction plane active — SESSION state only. A story
    /// level becomes the active level itself; the active level is always
    /// the plane's root level (the association target of new elements).
    pub fn set_active_plane(&mut self, id: f64) -> bool {
        let id = eid(id);
        let Some(level) = self.root_level(id) else {
            return false;
        };
        self.active_level = Some(level);
        self.active_plane = (id != level).then_some(id);
        self.refresh_session_and_overlays();
        self.apply_span();
        true
    }

    /// Add a level above the top one (3 m up, the next color, "Level N"):
    /// one undo step. Returns its id, or -1.
    pub fn add_level(&mut self) -> f64 {
        let levels = ops::levels_sorted(&self.doc);
        let top = levels.last().map_or(0.0, |l| l.elevation_m);
        let elevation = if levels.is_empty() { 0.0 } else { top + 3.0 };
        let name = if levels.is_empty() {
            "Ground".to_owned()
        } else {
            format!("Level {}", levels.len() + 1)
        };
        let color = ops::APP_LEVEL_COLORS[levels.len() % ops::APP_LEVEL_COLORS.len()];
        let depth = self.doc.undo_depth();
        let id = match self.doc.submit(Command::CreateLevel {
            name,
            elevation_m: elevation,
            is_building_story: true,
            color,
            extent_m: ops::LEVEL_EXTENT_M,
        }) {
            Ok(out) => {
                self.gestures.one_shot(depth);
                out.created_ids.first().map_or(-1.0, |id| id.0 as f64)
            }
            Err(status) => {
                web_sys::console::error_1(&JsValue::from_str(&format!("CreateLevel rejected: {status:?}")));
                -1.0
            }
        };
        self.sync("add level");
        id
    }

    pub fn update_level_name(&mut self, id: f64, name: String) {
        self.submit_level_update(id, "level name", Some(name), None, None, None);
    }

    pub fn update_level_elevation(&mut self, id: f64, elevation: f64) {
        if elevation.is_finite() {
            self.submit_level_update(id, "level elevation", None, Some(elevation), None, None);
        }
    }

    pub fn update_level_story(&mut self, id: f64, is_story: bool) {
        self.submit_level_update(id, "level story", None, None, Some(is_story), None);
    }

    /// Update a level's color, preserving its stored alpha.
    pub fn update_level_color(&mut self, id: f64, r: f32, g: f32, b: f32) {
        let alpha = ops::levels_sorted(&self.doc)
            .iter()
            .find(|l| l.id == eid(id))
            .map_or(0.3, |l| l.color[3]);
        self.submit_level_update(id, "level color", None, None, None, Some([r, g, b, alpha]));
    }

    /// Non-cascade delete attempt: "deleted", "has_dependents" (the page
    /// shows the cascade confirmation), or an error string.
    pub fn delete_level(&mut self, id: f64) -> String {
        let depth = self.doc.undo_depth();
        match self.doc.submit(Command::DeleteLevel { id: eid(id), cascade: false }) {
            Ok(_) => {
                self.gestures.one_shot(depth);
                self.sync("delete level");
                "deleted".to_owned()
            }
            // Held only by metadata (its plan span): nothing to confirm —
            // delete it with them, one undo step.
            Err(VimStatus::HasDependents) if self.only_metadata_depends_on(eid(id)) => {
                if self.delete_level_cascade(id) { "deleted".to_owned() } else { "error: cascade".to_owned() }
            }
            Err(VimStatus::HasDependents) => "has_dependents".to_owned(),
            Err(status) => format!("error: {status:?}"),
        }
    }

    /// Confirmed cascade delete: the level plus everything associated
    /// with / attached to it — ONE undo step.
    pub fn delete_level_cascade(&mut self, id: f64) -> bool {
        let depth = self.doc.undo_depth();
        match self.doc.submit(Command::DeleteLevel { id: eid(id), cascade: true }) {
            Ok(_) => {
                self.gestures.one_shot(depth);
                self.sync("delete level (cascade)");
                true
            }
            Err(status) => {
                web_sys::console::error_1(&JsValue::from_str(&format!(
                    "DeleteLevel cascade rejected: {status:?}"
                )));
                false
            }
        }
    }

    /// True when drawing is allowed: a level exists and is active
    /// (every element is associated with the active level).
    pub fn can_author(&self) -> bool {
        self.active_level.is_some()
    }

    // -- Undo / redo --------------------------------------------------------

    pub fn undo(&mut self) -> bool {
        if self.edit.is_some() {
            return self.edit_undo();
        }
        if self.gestures.undo(&mut self.doc) {
            self.sync("undo");
            true
        } else {
            false
        }
    }

    pub fn redo(&mut self) -> bool {
        if self.edit.is_some() {
            return self.edit_redo();
        }
        if self.gestures.redo(&mut self.doc) {
            self.sync("redo");
            true
        } else {
            false
        }
    }

    /// Inside Edit Mode these are bounded by the session's transaction.
    pub fn can_undo(&self) -> bool {
        if self.edit.is_some() { self.edit_can_undo() } else { self.gestures.can_undo() }
    }

    pub fn can_redo(&self) -> bool {
        if self.edit.is_some() { self.edit_can_redo() } else { self.gestures.can_redo() }
    }

    // -- Status ---------------------------------------------------------------

    /// True (drained on read) when a poll reported parametric changes
    /// since the last call — the page's single trigger to repaint its
    /// document-bound panels.
    pub fn take_params_dirty(&mut self) -> bool {
        std::mem::take(&mut self.params_dirty)
    }

    /// One-shot user notice ("Floor plate 2 created"), drained on read.
    pub fn take_notice(&mut self) -> String {
        self.notice.take().unwrap_or_default()
    }

    pub fn stats_json(&self) -> String {
        let errors: Vec<String> = self
            .errors
            .iter()
            .map(|(id, msg)| format!("#{}: {}", id.0, msg))
            .collect();
        let plates_on_level = self
            .model
            .iter()
            .filter(|e| matches!(e, ElementModel::Plate(_) | ElementModel::SketchPlate(_)) && e.level() == self.active_level)
            .count();
        let focus: Vec<usize> = self.focus_ids().iter().filter_map(|id| self.pick.owner_stats(*id)).map(|s| s.triangles).collect();
        let focus_triangles = (!focus.is_empty()).then(|| focus.iter().sum::<usize>());
        serde_json::json!({
            "backend": self.renderer.backend_name(),
            "msaa": self.renderer.sample_count(),
            "committed": self.committed,
            "evaluated": self.evaluated,
            "pending": self.pending,
            "settled": self.committed == self.evaluated && self.pending == 0,
            "triangles": self.renderer.drawn_triangle_count(),
            "lastOp": self.last_op,
            "lastLatencyMs": self.last_latency_ms,
            "lastMeshUpserts": self.last_mesh_upserts,
            "lastBaseTransforms": self.last_base_transforms,
            "canAuthor": self.can_author(),
            "canUndo": self.can_undo(),
            "canRedo": self.can_redo(),
            "wireframe": self.renderer.wireframe,
            "renderMode": self.render_mode(),
            // Triangles of the selection (or the element being edited).
            "focusTriangles": focus_triangles,
            "tool": self.tool.name(),
            "shape": self.current_shape().name(),
            "view": self.camera.mode.name(),
            "selection": self.selection.map(|id| id.0 as f64),
            "revision": self.revision,
            "elements": self.model.len(),
            "plates": self.model.iter().filter(|e| matches!(e, ElementModel::Plate(_))).count(),
            "platesOnLevel": plates_on_level,
            "walls": self.model.iter().filter(|e| e.is_wall()).count(),
            "wallsOnLevel": self.model.iter().filter(|e| e.is_wall() && e.level() == self.active_level).count(),
            "editing": self.edit.is_some(),
            "rooms": self.rooms.len(),
            // The see-through bands drawn now (the active level's span).
            "span": self.renderer.span.map(|b| serde_json::json!({
                "topZ": b.top_z, "bottomZ": b.bottom_z, "above": b.above_opacity, "below": b.below_opacity,
            })),
            "roomSelected": self.room_selection.map(|r| r.0 as f64),
            "openings": self.openings.is_some(),
            "wall": {
                "height": self.wall_height,
                "thickness": self.wall_thickness,
                "flip": self.wall_flip,
                // The run just drawn: the drawbar also edits it.
                "fresh": self.fresh_run().and_then(|id| self.run_model(id)).map(|r| serde_json::json!({
                    "id": r.element.0 as f64, "name": r.name,
                })),
            },
            "snap": { "enabled": self.snap_enabled, "step": self.snap_step },
            "errors": errors,
        })
        .to_string()
    }

    /// Diagnostics for "Copy debug info": entity counts by kind, errors,
    /// generations.
    pub fn debug_json(&self) -> String {
        let mut counts: BTreeMap<String, usize> = BTreeMap::new();
        for (_, record) in self.doc.entities() {
            *counts.entry(format!("{:?}", record.kind())).or_default() += 1;
        }
        serde_json::json!({
            "lib": vim_design_lib::version(),
            "entities": self.doc.entity_count(),
            "byKind": counts,
            "elements": self.model.len(),
            "committed": self.committed,
            "evaluated": self.evaluated,
            "undoDepth": self.doc.undo_depth(),
            "evalErrors": self.errors.values().collect::<Vec<_>>(),
        })
        .to_string()
    }

    /// World -> canvas device pixels: `[x, y]` JSON or `null`.
    pub fn world_to_screen(&self, x: f64, y: f64, z: f64) -> String {
        let (w, h) = self.size_f();
        match self
            .camera
            .project(Vec3::new(x as f32, y as f32, z as f32), w, h)
        {
            Some((px, py)) => format!("[{px},{py}]"),
            None => "null".to_owned(),
        }
    }

    pub fn scene_bbox_json(&self) -> String {
        match self.renderer.scene_bbox() {
            Some((min, max)) => serde_json::json!({ "min": min, "max": max }).to_string(),
            None => "null".to_owned(),
        }
    }
}

fn describe_load_error(status: VimStatus) -> String {
    match status {
        VimStatus::UnsupportedVersion => "not a VIM Design file (or a newer format)".to_owned(),
        VimStatus::MalformedData => "the file is damaged or incomplete".to_owned(),
        other => format!("could not read the file ({other:?})"),
    }
}

impl AuthorApp {
    /// The element the view focuses on: the one being edited, else the
    /// selection.
    fn focus_element(&self) -> Option<EntityId> {
        let opening = self.openings.as_ref().and_then(|o| o.selected).map(|(wall, _)| wall);
        self.edit.as_ref().and_then(|s| s.element).or(opening).or(self.selection)
    }

    /// The elements in focus: a wall run being edited (all its walls),
    /// else the focus element.
    fn focus_ids(&self) -> Vec<EntityId> {
        self.focus_element().into_iter().collect()
    }

    /// The box Fit frames: the focus elements' bounds.
    fn focus_bbox(&self) -> Option<([f64; 3], [f64; 3])> {
        let ids = self.focus_ids();
        if ids.is_empty()
            && let Some(b) = self.room_focus_bbox()
        {
            return Some(b); // rooms preview: the room edited or selected
        }
        let mut out: Option<([f64; 3], [f64; 3])> = None;
        for s in ids.iter().filter_map(|id| self.pick.owner_stats(*id)) {
            let [min, max] = s.bbox;
            let (min, max) = (min.map(f64::from), max.map(f64::from));
            out = Some(match out {
                None => (min, max),
                Some((a, b)) => (
                    [a[0].min(min[0]), a[1].min(min[1]), a[2].min(min[2])],
                    [b[0].max(max[0]), b[1].max(max[1]), b[2].max(max[2])],
                ),
            });
        }
        out
    }

    /// Keep the camera near the model (see `camera::VIEW_RANGE_FACTOR`):
    /// the model's bounds, or the active plane's square when empty.
    fn clamp_camera(&mut self) {
        if self.camera.mode == ViewMode::Elevation {
            return;
        }
        let (center, radius) = match self.renderer.scene_bbox() {
            Some((min, max)) => {
                let v = |a: [f64; 3]| Vec3::new(a[0] as f32, a[1] as f32, a[2] as f32);
                let (min, max) = (v(min), v(max));
                ((min + max) * 0.5, (max - min).length() * 0.5)
            }
            None => {
                let extent = ops::levels_sorted(&self.doc)
                    .iter()
                    .find(|l| Some(l.id) == self.active_level)
                    .map_or(ops::LEVEL_EXTENT_M, |l| l.extent_m);
                (Vec3::new(0.0, 0.0, self.active_elevation as f32), extent as f32)
            }
        };
        self.camera.clamp_to(center, radius);
    }

    /// Frame a world box in the part of the canvas the panels leave free
    /// (see [`AuthorApp::set_view_insets`]).
    fn fit_box(&mut self, min: Vec3, max: Vec3) {
        let (w, h) = self.size_f();
        let [l, t, r, b] = self.view_insets;
        // Keep at least half the canvas each way, whatever the panels.
        let (fw, fh) = ((w - l - r).max(w * 0.5), (h - t - b).max(h * 0.5));
        self.camera.fit(min, max, fw / fh);
        let k = h / fh;
        self.camera.plan_half_h *= k;
        self.camera.distance *= k;
        // The box centre (now at the canvas centre) moves to the free
        // area's centre.
        let (cx, cy) = (w / 2.0, h / 2.0);
        let (dx, dy) = ((l - r) / 2.0, (t - b) / 2.0);
        if dx.abs() > 0.5 || dy.abs() > 0.5 {
            match (self.view_plane_hit(cx, cy), self.view_plane_hit(cx + dx, cy + dy)) {
                (Some(a), Some(p)) => self.camera.pan_plane(a, p),
                _ => self.camera.pan_pixels(dx, dy, h),
            }
        }
    }

    fn size_f(&self) -> (f32, f32) {
        let (w, h) = self.renderer.size();
        (w as f32, h as f32)
    }

    /// Screen pixels per meter around a point of a sketch plane.
    fn px_per_m_at(&self, p: Vec3, frame: &SketchFrame) -> f32 {
        let (w, h) = self.size_f();
        let len = |axis: Vec3| match (
            self.camera.project(p - axis * 0.5, w, h),
            self.camera.project(p + axis * 0.5, w, h),
        ) {
            (Some(a), Some(b)) => (a.0 - b.0).hypot(a.1 - b.1),
            _ => 0.0,
        };
        len(frame.u).max(len(frame.v)).max(1e-3)
    }

    fn current_shape(&self) -> Shape {
        match self.tool {
            Tool::Wall => self.wall_shape,
            Tool::Room => self.room_shape,
            _ => self.shape,
        }
    }

    /// The active construction plane: a workplane when one is active,
    /// otherwise the active level.
    fn plane(&self) -> Option<EntityId> {
        self.active_plane.or(self.active_level)
    }

    /// Height of a construction plane (a level or a workplane) above the
    /// scene origin, exactly as evaluation places it (0 when unknown).
    fn plane_elevation(&self, plane: EntityId) -> f64 {
        vim_design_lib::workplane::plane_elevation(&self.doc, plane).unwrap_or(0.0)
    }

    /// The story level a construction plane belongs to (a level is its
    /// own root). Elements drawn on the plane are associated with it.
    fn root_level(&self, plane: EntityId) -> Option<EntityId> {
        vim_design_lib::workplane::root_level(&self.doc, plane)
    }

    /// Name of a level or a workplane.
    fn plane_name(&self, plane: EntityId) -> Option<String> {
        match self.doc.entity(plane).map(|e| &e.params) {
            Some(Params::Level { name, .. } | Params::Workplane { name, .. }) => Some(name.clone()),
            _ => None,
        }
    }

    /// Display data of a plane: name, "Level › Plane" path (every
    /// workplane of the chain), elevation.
    fn plane_json(&self, plane: EntityId) -> serde_json::Value {
        let name = self.plane_name(plane).unwrap_or_default();
        let mut chain = vec![name.clone()];
        let mut current = plane;
        while let Some(parent) = planes::parent(&self.doc, current) {
            chain.push(self.plane_name(parent).unwrap_or_default());
            current = parent;
        }
        chain.reverse();
        let path = chain.join(" › ");
        serde_json::json!({
            "id": plane.0 as f64,
            "name": name,
            "path": path,
            "elevation": self.plane_elevation(plane),
            "isLevel": self.root_level(plane) == Some(plane),
            // A workplane's offset from its parent (levels: none).
            "offset": match self.doc.entity(plane).map(|e| &e.params) {
                Some(Params::Workplane { offset_m, .. }) => Some(*offset_m),
                _ => None,
            },
        })
    }

    /// A wall of the library's `Wall` entity, by element.
    fn wall(&self, element: EntityId) -> Option<&WallModel> {
        self.model.iter().find_map(|e| match e {
            ElementModel::Wall(w) if w.element == element => Some(w),
            _ => None,
        })
    }

    /// A wall from before the `Wall` entity (extrusion-based), by element.
    /// Only metadata (a plan span, the site) depends on `id`.
    fn only_metadata_depends_on(&self, id: EntityId) -> bool {
        self.doc
            .dependents(id)
            .is_ok_and(|d| d.iter().all(|x| self.doc.entity(*x).is_some_and(|e| e.kind().is_metadata())))
    }

    fn legacy_wall(&self, element: EntityId) -> Option<&LegacyWallModel> {
        self.model.iter().find_map(|e| match e {
            ElementModel::LegacyWall(w) if w.element == element => Some(w),
            _ => None,
        })
    }

    fn is_wall(&self, element: EntityId) -> bool {
        self.model.iter().any(|e| e.element() == element && e.is_wall())
    }

    /// The elevation frame facing a wall, with its length and height.
    fn elevation_frame(&self, wall: &WallLine) -> (ElevationFrame, f32, f32) {
        let d = wall.dir();
        let u = Vec3::new(d[0] as f32, d[1] as f32, 0.0);
        // n = up × u: looking along n puts u on the screen's right.
        let n = Vec3::Z.cross(u);
        let side = wall.normal[0] * f64::from(n.x) + wall.normal[1] * f64::from(n.y);
        let z = self.plane_elevation(wall.plane) + wall.base_w;
        let frame = ElevationFrame {
            origin: Vec3::new(wall.start[0] as f32, wall.start[1] as f32, z as f32),
            u,
            n,
            near_offset: (wall.thickness * side).min(0.0) as f32,
            depth: wall.thickness as f32,
        };
        (frame, wall.length() as f32, wall.height as f32)
    }

    /// The wall an elevation view faces: the wall being edited.
    fn faced_frame(&self) -> Option<(ElevationFrame, f32, f32)> {
        self.openings_faced_frame()
    }

    /// Face the faced wall head-on, the whole wall in view.
    fn face_wall(&mut self) {
        let Some((frame, length, height)) = self.faced_frame() else { return };
        let (w, h) = self.size_f();
        self.camera.enter_elevation(frame, length, height, w / h);
        self.grid_key = None;
    }

    /// The plane the current sketch lives on.
    fn sketch_frame(&self) -> Option<SketchFrame> {
        let sk = self.sketch.as_ref()?;
        if matches!(sk.tool, SketchTool::Profile | SketchTool::Split) {
            return Some(self.edit_frame());
        }
        Some(SketchFrame {
            origin: Vec3::new(0.0, 0.0, self.plane_elevation(sk.level) as f32),
            u: Vec3::X,
            v: Vec3::Y,
        })
    }

    /// The point under a canvas pixel on the plane the view navigates:
    /// the faced wall in an elevation view, the active level otherwise.
    fn view_plane_hit(&self, px: f32, py: f32) -> Option<Vec3> {
        let (w, h) = self.size_f();
        match (self.camera.mode, self.camera.elevation) {
            (ViewMode::Elevation, Some(f)) => {
                self.camera.unproject_to(px, py, w, h, self.camera.target, f.n)
            }
            _ => self.camera.unproject_to_plane(px, py, w, h, self.active_elevation as f32),
        }
    }

    /// Snap targets for a sketch: what to capture and at which grid step.
    fn snap_sources(&self, sk: &Sketch) -> SnapSources {
        match sk.tool {
            // Snapping onto a plate or hole edge would make a hole touch
            // it (invalid), so holes snap to corners only.
            SketchTool::Hole => SnapSources {
                vertices: self.level_vertices(sk.level),
                step: self.snap_step,
                ..SnapSources::default()
            },
            SketchTool::Profile | SketchTool::Split => {
                let view = self.edit.as_ref().map(|s| s.model.view()).unwrap_or_default();
                let edges = view
                    .edges()
                    .iter()
                    .filter_map(|e| Some((view.point(e.0)?, view.point(e.1)?)))
                    .collect();
                SnapSources {
                    vertices: view.points.iter().map(|p| p.uv).collect(),
                    edges,
                    align: Vec::new(),
                    step: self.edit_snap_step(),
                }
            }
            SketchTool::Plate | SketchTool::Wall => SnapSources {
                vertices: self.level_vertices(sk.level),
                edges: self.level_edges(sk.level),
                align: Vec::new(),
                step: self.snap_step,
            },
            // Rooms snap to the plane's geometry and to the other rooms.
            SketchTool::Room => {
                let mut vertices = self.level_vertices(sk.level);
                let mut edges = self.level_edges(sk.level);
                for r in self.rooms.iter().filter(|r| r.plane == sk.level) {
                    let poly = r.data.polygon();
                    vertices.extend(poly.iter().copied());
                    edges.extend((0..poly.len()).map(|i| (poly[i], poly[(i + 1) % poly.len()])));
                }
                SnapSources { vertices, edges, align: Vec::new(), step: self.snap_step }
            }
        }
    }

    /// Snap step in Edit Mode (the plan's).
    fn edit_snap_step(&self) -> f64 {
        self.snap_step
    }

    fn sketch_count(&self) -> usize {
        self.sketch.as_ref().map_or(0, |s| s.points.len())
    }

    fn plate(&self, element: EntityId) -> Option<&PlateModel> {
        self.model.iter().find_map(|e| match e {
            ElementModel::Plate(p) if p.element == element => Some(p),
            _ => None,
        })
    }

    fn plate_by_face(&self, face: EntityId) -> Option<&PlateModel> {
        self.model.iter().find_map(|e| match e {
            ElementModel::Plate(p) if p.face == face => Some(p),
            _ => None,
        })
    }

    /// Plates whose top face lies on `level`'s plane (w = 0).
    fn plate_outlines(&self, level: EntityId) -> Vec<PlateOutline<'_>> {
        self.model
            .iter()
            .filter_map(|e| match e {
                ElementModel::Plate(p) if p.plane_level == level && p.top_w.abs() < 1e-9 => {
                    Some(PlateOutline {
                        face: p.face,
                        outline: &p.outline,
                        holes: p.holes.iter().map(|h| h.outline.as_slice()).collect(),
                    })
                }
                _ => None,
            })
            .collect()
    }

    /// Snap vertices on `level`'s plane: plate + hole corners and wall
    /// base-line ends.
    fn level_vertices(&self, level: EntityId) -> Vec<P2> {
        let mut out = Vec::new();
        for e in &self.model {
            match e {
                ElementModel::Plate(p) if p.plane_level == level && p.top_w.abs() < 1e-9 => {
                    out.extend_from_slice(&p.outline);
                    for h in &p.holes {
                        out.extend_from_slice(&h.outline);
                    }
                }
                ElementModel::Run(_) | ElementModel::Wall(_) | ElementModel::LegacyWall(_) => {
                    for w in e.wall_lines().into_iter().filter(|w| w.plane == level && w.base_w.abs() < 1e-9) {
                        out.push(w.start);
                        out.push(w.end);
                    }
                }
                ElementModel::SketchPlate(p) if p.plane_level == level && !self.is_edited(p.element) => {
                    out.extend(p.sketch.points.iter().map(|q| q.uv));
                }
                _ => {}
            }
        }
        out
    }

    /// The element Edit Mode is working on (its own points snap through
    /// the session, not as "other" geometry).
    fn is_edited(&self, element: EntityId) -> bool {
        self.edit.as_ref().and_then(|s| s.element) == Some(element)
            || self.run_edit.is_some_and(|r| r.element == element)
    }

    fn is_plate(&self, element: EntityId) -> bool {
        self.model.iter().any(|e| {
            e.element() == element && matches!(e, ElementModel::Plate(_) | ElementModel::SketchPlate(_))
        })
    }

    /// Snap edges on `level`'s plane: plate outlines and wall base lines
    /// (tracing a plate edge is how a wall is laid onto a plate).
    fn level_edges(&self, level: EntityId) -> Vec<(P2, P2)> {
        let mut out = Vec::new();
        for e in &self.model {
            match e {
                ElementModel::Plate(p) if p.plane_level == level && p.top_w.abs() < 1e-9 => {
                    let n = p.outline.len();
                    out.extend((0..n).map(|i| (p.outline[i], p.outline[(i + 1) % n])));
                }
                ElementModel::Run(_) | ElementModel::Wall(_) | ElementModel::LegacyWall(_) => {
                    for w in e.wall_lines().into_iter().filter(|w| w.plane == level && w.base_w.abs() < 1e-9) {
                        out.push((w.start, w.end));
                    }
                }
                ElementModel::SketchPlate(p) if p.plane_level == level && !self.is_edited(p.element) => {
                    for e in vim_design_lib::sketch::edges(&p.sketch) {
                        if let (Ok(a), Ok(b)) = (p.sketch.uv(e.a), p.sketch.uv(e.b)) {
                            out.push((a, b));
                        }
                    }
                }
                _ => {}
            }
        }
        out
    }

    fn sketch_status(&self) -> Option<sketch::SketchStatus> {
        let sk = self.sketch.as_ref()?;
        Some(match sk.tool {
            SketchTool::Plate | SketchTool::Profile | SketchTool::Split | SketchTool::Room => {
                sketch::status(sk, &SketchContext::Plate)
            }
            SketchTool::Hole => {
                let plates = self.plate_outlines(sk.level);
                sketch::status(sk, &SketchContext::Hole(&plates))
            }
            SketchTool::Wall => sketch::status(
                sk,
                &SketchContext::Wall { thickness: self.wall_thickness, flip: self.wall_flip },
            ),
        })
    }

    /// Validate and commit the sketch as ONE gesture. `closing`: the
    /// first vertex was tapped (a wall polyline becomes a closed loop).
    fn finish_sketch(&mut self, closing: bool) -> String {
        let Some(sketch) = self.sketch.clone() else {
            return r#"{"result":"none","count":0}"#.to_owned();
        };
        let rejected = |reason: Invalid, count: usize| {
            serde_json::json!({
                "result": "rejected",
                "reason": reason.message(),
                "code": reason.code(),
                "count": count,
            })
            .to_string()
        };
        let run = if sketch.tool == SketchTool::Wall {
            let (run, closed) = sketch::wall_run(&sketch, closing);
            if let Err(e) = sketch::validate_run(&run, closed, self.wall_thickness, self.wall_flip) {
                return rejected(e, sketch.points.len());
            }
            Some((crate::authoring::geom::dedup_closed(&run), closed))
        } else {
            if let Some(st) = self.sketch_status().filter(|s| !s.can_finish) {
                return rejected(st.reason.unwrap_or(Invalid::TooFewPoints), sketch.points.len());
            }
            None
        };
        if matches!(sketch.tool, SketchTool::Profile | SketchTool::Split) {
            return self.finish_edit_sketch(sketch.tool, sketch.outline());
        }
        let outline = sketch.outline();
        let depth = self.doc.undo_depth();
        let result: Result<(String, Option<EntityId>, &str), String> = match sketch.tool {
            SketchTool::Plate => {
                let name = ops::next_element_name(&self.doc, "Floor plate");
                ops::commit_plate(&mut self.doc, sketch.level, &outline, self.plate_thickness, &name)
                    .map(|ids| (format!("{name} created"), Some(ids.element), "draw plate"))
            }
            SketchTool::Hole => {
                let plates = self.plate_outlines(sketch.level);
                match sketch::validate_hole(&outline, &plates) {
                    Ok(index) => {
                        let face = plates[index].face;
                        let (element, name) = self
                            .plate_by_face(face)
                            .map_or((None, String::new()), |p| (Some(p.element), p.name.clone()));
                        ops::commit_hole(&mut self.doc, sketch.level, face, &outline)
                            .map(|_| (format!("Hole in {name} created"), element, "add hole"))
                    }
                    Err(invalid) => Err(invalid.message().to_owned()),
                }
            }
            SketchTool::Wall => {
                let level = self.root_level(sketch.level).unwrap_or(sketch.level);
                let (points, closed) = run.clone().unwrap_or_default();
                match self.new_wall_height_mode(sketch.level) {
                    Ok(height) => ops::commit_run(
                        &mut self.doc,
                        sketch.level,
                        level,
                        &points,
                        closed,
                        self.wall_flip,
                        self.wall_thickness,
                        height,
                    ),
                    Err(e) => Err(e),
                }
            }
            .map(|element| {
                let segs = run.as_ref().map_or(0, |(p, c)| if *c { p.len() } else { p.len().saturating_sub(1) });
                let msg = if segs == 1 { "Wall created".to_owned() } else { format!("Wall created: {segs} segments") };
                (msg, Some(element), "draw walls")
            }),
            SketchTool::Room => self
                .commit_room(sketch.level, &outline)
                .map(|(_, name)| (format!("{name} created"), None, "draw room")),
            SketchTool::Profile | SketchTool::Split => Err("not a document sketch".to_owned()),
        };
        match result {
            Ok((notice, element, op)) => {
                if sketch.tool != SketchTool::Room {
                    self.gestures.one_shot(depth); // a room is a room step (preview)
                }
                if let Some(s) = self.sketch.as_mut() {
                    s.points.clear();
                }
                self.sync(op);
                if sketch.tool == SketchTool::Wall {
                    // The new run is the selection: the drawbar's settings
                    // apply to it until the next point.
                    self.selection = element;
                    self.fresh_run = element;
                    self.refresh_styles();
                }
                self.notice = Some(notice.clone());
                serde_json::json!({
                    "result": "committed",
                    "name": notice,
                    "element": element.map(|e| e.0 as f64),
                    "count": 0,
                })
                .to_string()
            }
            Err(e) => {
                ops::rollback_to(&mut self.doc, depth);
                self.gestures.invalidate_redo();
                self.sync("commit failed");
                serde_json::json!({"result": "rejected", "reason": e, "count": sketch.points.len()})
                    .to_string()
            }
        }
    }

    fn level_name(&self, id: Option<EntityId>) -> Option<String> {
        let id = id?;
        match self.doc.entity(id).map(|e| &e.params) {
            Some(Params::Level { name, .. }) => Some(name.clone()),
            _ => None,
        }
    }

    fn element_value(&self, e: &ElementModel) -> serde_json::Value {
        let base = serde_json::json!({
            "id": e.element().0 as f64,
            "kind": e.kind_name(),
            "name": e.name(),
            "levelId": e.level().map(|l| l.0 as f64),
            "levelName": self.level_name(e.level()),
        });
        match e {
            ElementModel::Plate(p) => {
                let mut v = base;
                let holes: Vec<serde_json::Value> = p
                    .holes
                    .iter()
                    .enumerate()
                    .map(|(i, h)| {
                        serde_json::json!({
                            "wire": h.wire.0 as f64,
                            "index": i + 1,
                            "area": signed_area(&h.outline).abs(),
                            "outline": h.outline,
                        })
                    })
                    .collect();
                if let Some(obj) = v.as_object_mut() {
                    obj.insert("thickness".into(), p.thickness.into());
                    obj.insert("area".into(), p.area.into());
                    obj.insert("outline".into(), serde_json::json!(p.outline));
                    obj.insert("holes".into(), holes.into());
                }
                v
            }
            ElementModel::SketchPlate(p) => {
                let mut v = base;
                let stats = self.pick.owner_stats(p.element);
                let faces: Vec<serde_json::Value> = p
                    .sketch
                    .faces
                    .iter()
                    .map(|f| {
                        let outline = vim_design_lib::sketch::face_polygon(&p.sketch, f.id).unwrap_or_default();
                        match f.kind {
                            vim_design_lib::sketch::SketchFaceKind::Solid { thickness } => serde_json::json!({
                                "id": f.id, "kind": "solid", "thickness": thickness, "outline": outline,
                            }),
                            vim_design_lib::sketch::SketchFaceKind::Void { depth } => serde_json::json!({
                                "id": f.id, "kind": "void", "depth": depth, "outline": outline,
                            }),
                        }
                    })
                    .collect();
                if let Some(obj) = v.as_object_mut() {
                    obj.insert("sketch".into(), true.into());
                    obj.insert("faces".into(), faces.into());
                    obj.insert("faceCount".into(), p.sketch.faces.len().into());
                    obj.insert("area".into(), stats.map_or(0.0, |s| s.top_area).into());
                    obj.insert("volume".into(), stats.map_or(0.0, |s| s.volume).into());
                    obj.insert("triangles".into(), stats.map_or(0, |s| s.triangles).into());
                    obj.insert("bbox".into(), stats.map_or(serde_json::Value::Null, |s| serde_json::json!(s.bbox)));
                }
                v
            }
            ElementModel::Run(r) => {
                let mut v = base;
                let stats = self.pick.owner_stats(r.element);
                let openings: Vec<serde_json::Value> = r
                    .data
                    .openings
                    .iter()
                    .map(|o| {
                        serde_json::json!({
                            "id": o.id, "segment": o.segment, "kind": match o.kind {
                                vim_design_lib::wall_run::OpeningKind::Window => "window",
                                vim_design_lib::wall_run::OpeningKind::Door => "door",
                            },
                            "offset": o.offset_m, "sill": o.sill_m, "width": o.width_m, "height": o.height_m, "depth": o.depth_m,
                        })
                    })
                    .collect();
                if let Some(obj) = v.as_object_mut() {
                    obj.insert("run".into(), true.into());
                    obj.insert("legacy".into(), false.into());
                    obj.insert("points".into(), serde_json::json!(r.data.points.iter().map(|p| p.uv).collect::<Vec<_>>()));
                    obj.insert("closed".into(), r.data.closed.into());
                    obj.insert("segments".into(), r.data.segment_count().into());
                    obj.insert("height".into(), r.top_height.into());
                    obj.insert("heightM".into(), r.data.height_m.into());
                    obj.insert("mode".into(), if r.top.is_some() { "upto" } else { "fixed" }.into());
                    obj.insert("topPlane".into(), r.top.map(|t| t.0 as f64).into());
                    obj.insert("topOffset".into(), r.data.top_offset_m.into());
                    obj.insert("basePlane".into(), (r.base.0 as f64).into());
                    obj.insert("basePlaneName".into(), self.plane_name(r.base).into());
                    obj.insert("baseElevation".into(), self.plane_elevation(r.base).into());
                    obj.insert("thickness".into(), r.data.thickness_m.into());
                    obj.insert("length".into(), r.length().into());
                    obj.insert("footprintArea".into(), crate::authoring::runs::footprint_area(&r.data).into());
                    obj.insert("minHeight".into(), self.run_min_height(r).into());
                    obj.insert("openings".into(), openings.len().into());
                    obj.insert("openingList".into(), openings.into());
                    obj.insert("faces".into(), serde_json::json!([]));
                    obj.insert("windows".into(), serde_json::json!([]));
                    obj.insert("volume".into(), stats.map_or(0.0, |s| s.volume).into());
                    obj.insert("triangles".into(), stats.map_or(0, |s| s.triangles).into());
                    obj.insert("bbox".into(), stats.map_or(serde_json::Value::Null, |s| serde_json::json!(s.bbox)));
                }
                v
            }
            ElementModel::RoomWalls(w) => {
                let mut v = base;
                let stats = self.pick.owner_stats(w.element);
                if let (Some(obj), serde_json::Value::Object(walls)) = (v.as_object_mut(), self.room_walls_value(w)) {
                    obj.extend(walls);
                    obj.insert("footprintArea".into(), self.layout_footprint_area(w.layout).into());
                    obj.insert("openingList".into(), self.room_openings_json(w).into());
                    obj.insert("roomList".into(), serde_json::json!(w.rooms.iter().map(|r| r.0 as f64).collect::<Vec<_>>()));
                    obj.insert("volume".into(), stats.map_or(0.0, |s| s.volume).into());
                    obj.insert("triangles".into(), stats.map_or(0, |s| s.triangles).into());
                    obj.insert("bbox".into(), stats.map_or(serde_json::Value::Null, |s| serde_json::json!(s.bbox)));
                }
                v
            }
            ElementModel::Wall(w) => {
                let mut v = base;
                let stats = self.pick.owner_stats(w.element);
                let effective = w.effective();
                let faces: Vec<serde_json::Value> = effective
                    .faces
                    .iter()
                    .map(|f| {
                        let outline = vim_design_lib::sketch::face_polygon(&effective, f.id).unwrap_or_default();
                        match f.kind {
                            vim_design_lib::sketch::SketchFaceKind::Solid { thickness } => serde_json::json!({
                                "id": f.id, "kind": "solid", "thickness": thickness, "outline": outline,
                            }),
                            vim_design_lib::sketch::SketchFaceKind::Void { depth } => serde_json::json!({
                                "id": f.id, "kind": "void", "depth": depth, "outline": outline,
                            }),
                        }
                    })
                    .collect();
                let openings = faces.iter().filter(|f| f["kind"] == "void").count();
                if let Some(obj) = v.as_object_mut() {
                    obj.insert("height".into(), w.top_height.into());
                    obj.insert("heightM".into(), w.height_m.into());
                    obj.insert("mode".into(), if w.top.is_some() { "upto" } else { "fixed" }.into());
                    obj.insert("topPlane".into(), w.top.map(|t| t.0 as f64).into());
                    obj.insert("topOffset".into(), w.top_offset_m.into());
                    obj.insert("basePlane".into(), (w.base.0 as f64).into());
                    obj.insert("basePlaneName".into(), self.plane_name(w.base).into());
                    obj.insert("baseElevation".into(), self.plane_elevation(w.base).into());
                    obj.insert("thickness".into(), w.thickness().into());
                    obj.insert("length".into(), w.length().into());
                    obj.insert("start".into(), serde_json::json!(w.start));
                    obj.insert("end".into(), serde_json::json!(w.end));
                    obj.insert("normal".into(), serde_json::json!(w.normal()));
                    obj.insert("baseW".into(), 0.0.into());
                    obj.insert("minHeight".into(), self.lib_wall_min_height(w).into());
                    obj.insert("topPoints".into(), serde_json::json!(w.top_points));
                    obj.insert("faces".into(), faces.into());
                    obj.insert("openings".into(), openings.into());
                    obj.insert("windows".into(), serde_json::json!([]));
                    obj.insert("legacy".into(), false.into());
                    obj.insert("volume".into(), stats.map_or(0.0, |s| s.volume).into());
                    obj.insert("triangles".into(), stats.map_or(0, |s| s.triangles).into());
                    obj.insert("bbox".into(), stats.map_or(serde_json::Value::Null, |s| serde_json::json!(s.bbox)));
                }
                v
            }
            ElementModel::LegacyWall(w) => {
                let mut v = base;
                let windows: Vec<serde_json::Value> = w
                    .windows
                    .iter()
                    .enumerate()
                    .map(|(i, h)| {
                        serde_json::json!({
                            "wire": h.wire.0 as f64,
                            "index": i + 1,
                            "area": signed_area(&h.outline).abs(),
                            "outline": h.outline,
                        })
                    })
                    .collect();
                if let Some(obj) = v.as_object_mut() {
                    obj.insert("height".into(), w.height.into());
                    obj.insert("thickness".into(), w.thickness.into());
                    obj.insert("length".into(), w.length().into());
                    obj.insert("start".into(), serde_json::json!(w.start));
                    obj.insert("end".into(), serde_json::json!(w.end));
                    obj.insert("normal".into(), serde_json::json!(w.normal));
                    obj.insert("baseW".into(), w.base_w.into());
                    obj.insert("profile".into(), serde_json::json!(w.profile));
                    obj.insert("minHeight".into(), self.min_wall_height(w).into());
                    obj.insert("windows".into(), windows.into());
                    obj.insert("legacy".into(), true.into());
                    obj.insert("mode".into(), "fixed".into());
                    obj.insert("basePlane".into(), (w.plane_level.0 as f64).into());
                }
                v
            }
            ElementModel::Other(_) => base,
        }
    }

    /// Swap in a new document (new project, load, import): fresh engine,
    /// cleared GPU/pick state and history; session state revalidated.
    fn replace_document(&mut self, doc: Document, op: &str) {
        self.doc = doc;
        self.engine = Engine::new();
        // This renderer composes `world = instance ∘ base`: opt into
        // translation factoring (level elevation edits become
        // transform-only).
        self.engine.set_translation_factoring(true);
        self.engine.set_params_watch(None);
        self.renderer.clear_scene();
        self.pick.clear();
        self.gestures = Gestures::default();
        self.errors.clear();
        self.selection = None;
        self.room_selection = None;
        self.room_edit = None;
        self.sketch = None;
        self.tool = Tool::Select;
        self.edit = None;
        self.edit_target = EditTarget::Plane;
        self.run_edit = None;
        self.openings = None;
        self.edit_tool = EditTool::Select;
        self.edit_sketch = None;
        self.edit_entry_element = None;
        if let Some(c) = self.prev_camera.take() {
            self.camera = c;
        }
        self.active_level = None;
        self.active_plane = None;
        self.active_elevation = 0.0;
        self.last_committed = 0;
        // A loaded document reports no params_changed (nothing was
        // "touched"), so the replacement itself triggers the derivation
        // (before the first sync, which colors meshes by element kind).
        self.model = model::derive(&self.doc);
        self.derive_rooms();
        self.params_dirty = true;
        self.sync(op);
        self.revision += 1;
        self.refresh_session_and_overlays();
    }

    fn submit_level_update(
        &mut self,
        id: f64,
        op: &str,
        name: Option<String>,
        elevation_m: Option<f64>,
        is_building_story: Option<bool>,
        color: Option<[f32; 4]>,
    ) {
        let id = eid(id);
        if !matches!(self.doc.entity(id).map(|e| e.kind()), Some(EntityKind::Level)) {
            return;
        }
        let coalesce = self.gestures.begin_continuing(&self.doc, &format!("{op}_{}", id.0));
        self.submit(Command::UpdateLevel {
            id,
            name,
            elevation_m,
            is_building_story,
            color,
            extent_m: None,
            coalesce,
        });
        self.sync(op);
    }

    fn submit(&mut self, cmd: Command) {
        let label = cmd.label();
        if let Err(status) = self.doc.submit(cmd) {
            web_sys::console::error_1(&JsValue::from_str(&format!(
                "{label} rejected: {status:?}"
            )));
        }
    }

    fn material_color(&self, material: Option<EntityId>, fallback: [f32; 3]) -> [f32; 3] {
        match material.and_then(|id| self.doc.entity(id)).map(|e| &e.params) {
            Some(Params::Material { color, .. }) => {
                [color[0] as f32, color[1] as f32, color[2] as f32]
            }
            _ => fallback,
        }
    }

    /// The facade drive cycle: evaluate, poll, apply to GPU + pick scene
    /// (removals before upserts), re-derive the model on parametric
    /// changes.
    fn sync(&mut self, op: &str) {
        let t0 = now_ms();
        self.engine.evaluate_pending(&mut self.doc);
        let updates = self.engine.poll_updates(&self.doc);
        // The plan span follows its level (and edits of it).
        self.apply_span();
        if !updates.params_changed.is_empty() {
            self.model = model::derive(&self.doc);
            self.derive_rooms();
            self.params_dirty = true;
        }

        for id in &updates.meshes_removed {
            self.renderer.remove_mesh(*id);
            self.pick.remove_mesh(*id);
        }
        for id in &updates.instances_removed {
            self.renderer.remove_instance(*id);
            self.pick.remove_instance(*id);
        }
        for mu in &updates.meshes {
            let fallback = if self.is_plate(mu.id) {
                rgb(CONCRETE)
            } else if self.is_wall(mu.id) {
                rgb(WALL)
            } else {
                rgb(OTHER_ELEMENT)
            };
            let colors: Vec<[f32; 3]> = mu
                .mesh
                .submeshes
                .iter()
                .map(|sub| self.material_color(sub.material, fallback))
                .collect();
            self.renderer
                .upsert_mesh(mu.id, &mu.mesh, &colors, &mu.base_transform);
            self.pick.upsert_mesh(mu.id, &mu.mesh, &mu.base_transform);
        }
        for bt in &updates.base_transforms {
            self.renderer.set_base_transform(bt.id, &bt.transform);
            self.pick.set_base(bt.id, &bt.transform);
        }
        for iu in &updates.instances {
            self.renderer.upsert_instance(iu.id, iu.element_id, &iu.transform);
            self.pick.upsert_instance(iu.id, iu.element_id, &iu.transform);
        }
        for (id, diag) in &updates.errors {
            self.errors.insert(*id, diag.to_string());
        }
        for id in &updates.errors_cleared {
            self.errors.remove(id);
        }
        self.errors.retain(|id, _| self.doc.entity(*id).is_some());

        if updates.committed_generation != self.last_committed {
            self.last_committed = updates.committed_generation;
            self.revision += 1;
        }
        self.committed = updates.committed_generation;
        self.evaluated = updates.evaluated_generation;
        self.pending = updates.pending_count;
        self.last_mesh_upserts = updates.meshes.len();
        self.last_base_transforms = updates.base_transforms.len();

        if self
            .selection
            .is_some_and(|s| !self.model.iter().any(|e| e.element() == s))
        {
            self.selection = None;
        }
        self.refresh_session_and_overlays();

        self.last_latency_ms = now_ms() - t0;
        self.last_op = op.to_owned();
    }

    /// Revalidate the active level (fallback: nearest by elevation),
    /// keep the sketch on it, rebuild level outlines and styles.
    fn refresh_session_and_overlays(&mut self) {
        let levels = ops::levels_sorted(&self.doc);
        let alive = self
            .active_level
            .filter(|id| levels.iter().any(|l| l.id == *id));
        self.active_level = alive.or_else(|| {
            levels
                .iter()
                .min_by(|a, b| {
                    let da = (a.elevation_m - self.active_elevation).abs();
                    let db = (b.elevation_m - self.active_elevation).abs();
                    da.total_cmp(&db)
                })
                .map(|l| l.id)
        });
        // A plane that vanished falls back to its level.
        if self.active_plane.is_some_and(|p| self.doc.entity(p).is_none()) {
            self.active_plane = None;
        }
        if let Some(plane) = self.plane() {
            self.active_elevation = self.plane_elevation(plane);
        }
        // The camera orbits / looks down onto the active plane.
        self.camera.plane_z = self.active_elevation as f32;
        // A plan sketch lives on the active plane (Edit Mode sketches live
        // on the edited profile).
        let plane_sketch = self.sketch.as_ref().filter(|s| !matches!(s.tool, SketchTool::Profile | SketchTool::Split));
        match (self.plane(), plane_sketch.map(|s| s.level)) {
            (Some(active), Some(level)) if active != level => {
                if let Some(s) = self.sketch.as_mut() {
                    s.level = active;
                    s.points.clear();
                    s.cursor = None;
                }
            }
            (None, Some(_)) => {
                self.sketch = None;
                self.tool = Tool::Select;
            }
            _ => {}
        }

        // Other planes (levels and workplanes): faint outline squares (a
        // full translucent square would obscure the plan view). The active
        // plane shows its grid.
        let mut outlines: Vec<(f64, f64, [f32; 4])> = levels
            .iter()
            .filter(|l| Some(l.id) != self.plane())
            .map(|l| (l.extent_m, l.elevation_m, l.color))
            .collect();
        outlines.extend(
            ops::workplanes(&self.doc)
                .iter()
                .filter(|w| Some(w.id) != self.plane())
                .map(|w| (w.extent_m, self.plane_elevation(w.id), w.color)),
        );
        let mut lines: Vec<f32> = Vec::new();
        for (extent, z, color) in outlines {
            let (e, z) = (extent as f32, z as f32);
            let c = [color[0], color[1], color[2], 0.55];
            let corners = [[-e, -e], [e, -e], [e, e], [-e, e]];
            for i in 0..4 {
                let (a, b) = (corners[i], corners[(i + 1) % 4]);
                lines.extend_from_slice(&[a[0], a[1], z]);
                lines.extend_from_slice(&c);
                lines.extend_from_slice(&[b[0], b[1], z]);
                lines.extend_from_slice(&c);
            }
        }
        self.renderer.set_lines(LineLayer::Plain, &lines);
        self.grid_key = None;
        self.refresh_styles();
    }

    /// Per-owner styles: selection accent, plan-view dimming of other
    /// levels, the faced wall outlined in an elevation view, crisp
    /// feature edges everywhere.
    fn refresh_styles(&mut self) {
        let edge = rgba(EDGE, 0.5);
        self.renderer.set_default_style(MeshStyle { tint: [0.0; 4], edge, depth_bias: 0.0, unlit: 0.0 });
        let mut styles: HashMap<EntityId, MeshStyle> = HashMap::new();
        let [cr, cg, cb] = rgb(CLEAR);
        let ortho = self.camera.mode != ViewMode::Orbit;
        for id in self.pick.owner_ids().collect::<Vec<_>>() {
            let element = self.model.iter().find(|e| e.element() == id);
            let level = element.and_then(|e| e.level());
            let is_plate = matches!(element, Some(ElementModel::Plate(_) | ElementModel::SketchPlate(_)));
            let is_wall = element.is_some_and(|e| e.is_wall());
            let depth_bias = if ortho && is_plate { PLATE_DEPTH_BIAS } else { 0.0 };
            // The plan cut exposes the inside of a wall: fill it flat.
            let cut_wall = is_wall && self.camera.mode == ViewMode::Plan;
            let other_level = self.camera.mode == ViewMode::Plan
                && level.is_some()
                && level != self.active_level;
            let edited = self.is_edited(id);
            let (tint, edge, unlit) = if edited {
                // The profile overlay is the focus while editing; the
                // live mesh stays readable under it.
                ([cr, cg, cb, 0.3], rgba(EDGE, 0.3), 0.0)
            } else if Some(id) == self.selection {
                let a = if cut_wall { 0.85 } else { 0.34 };
                (rgba(ACCENT, a), rgba(ACCENT, 1.0), if cut_wall { 1.0 } else { 0.0 })
            } else if other_level {
                ([cr, cg, cb, 0.6], rgba(EDGE, 0.15), 0.0)
            } else if cut_wall {
                (rgba(POCHE, 0.8), edge, 1.0)
            } else {
                ([0.0; 4], edge, 0.0)
            };
            styles.insert(id, MeshStyle { tint, edge, depth_bias, unlit });
        }
        self.renderer.set_styles(styles);
    }

    /// The plane the grid is drawn on: the active level (lifted so it
    /// shows on plate tops), or the faced wall in an elevation view
    /// (just in front of the face, wall-local u along / v up).
    fn grid_frame(&self) -> SketchFrame {
        match (self.camera.mode, self.faced_frame()) {
            (ViewMode::Elevation, Some((f, _, _))) => SketchFrame {
                origin: f.origin + f.n * (f.near_offset - GRID_LIFT_M * 2.0),
                u: f.u,
                v: Vec3::Z,
            },
            _ => SketchFrame {
                origin: Vec3::new(0.0, 0.0, self.active_elevation as f32 + GRID_LIFT_M),
                u: Vec3::X,
                v: Vec3::Y,
            },
        }
    }

    /// Rebuild the grid when the view changes enough (density follows
    /// zoom; extent covers the view).
    fn update_grid(&mut self) {
        let (w, h) = self.size_f();
        let ppm = f64::from(self.camera.px_per_m(h));
        let mode = self.camera.mode;
        // Sparser in 3D: perspective compresses distant lines.
        let min_px = if mode == ViewMode::Orbit { 16.0 } else { 9.0 };
        let mut minor = if mode == ViewMode::Elevation { ELEVATION_GRID_STEP_M } else { self.snap_step };
        for s in GRID_STEPS {
            if minor * ppm >= min_px {
                break;
            }
            if s > minor {
                minor = s;
            }
        }
        let major = if minor < 1.0 { 1.0 } else { minor * 5.0 };
        let frame = self.grid_frame();
        let center = frame.local(self.camera.target);
        let (half_x, half_y, fade) = match mode {
            ViewMode::Plan => {
                let hh = f64::from(self.camera.plan_half_h) * 1.15;
                (hh * f64::from(w / h), hh, 0.0f32)
            }
            ViewMode::Elevation => {
                let hh = f64::from(self.camera.elevation_half_h) * 1.15;
                (hh * f64::from(w / h), hh, 0.0f32)
            }
            ViewMode::Orbit => {
                let r = (f64::from(self.camera.distance) * 1.3).clamp(12.0, 900.0);
                (r, r, r as f32)
            }
        };
        // Cap the line count (very wide views at fine steps).
        while (2.0 * half_x.max(half_y) / minor) > 700.0 {
            minor = if minor < 1.0 { 1.0 } else { minor * 2.0 };
        }
        let major = major.max(minor);
        let snapq = |v: f64| (v / major).floor() as i64;
        let key = (
            minor.to_bits(),
            major.to_bits(),
            snapq(center[0] - half_x),
            snapq(center[0] + half_x) + 1,
            snapq(center[1] - half_y),
            snapq(center[1] + half_y) + 1,
            (frame.origin.x + frame.origin.y * 7.0 + frame.origin.z * 13.0).to_bits(),
            match mode {
                ViewMode::Plan => 0,
                ViewMode::Orbit => 1,
                ViewMode::Elevation => 2,
            },
        );
        let target = self.camera.target;
        self.renderer.set_fade([target.x, target.y, frame.origin.z], fade);
        if self.grid_key == Some(key) {
            return;
        }
        self.grid_key = Some(key);
        let (x0, x1) = (key.2 as f64 * major, key.3 as f64 * major);
        let (y0, y1) = (key.4 as f64 * major, key.5 as f64 * major);
        let minor_c = rgba(GRID, 0.07);
        let major_c = rgba(GRID, 0.17);
        let axis_x = rgba(AXIS_X, 0.75);
        let axis_y = rgba(AXIS_Y, 0.75);
        let mut verts: Vec<f32> = Vec::new();
        let mut line = |a: P2, b: P2, c: [f32; 4]| {
            verts.extend_from_slice(&frame.world(a).to_array());
            verts.extend_from_slice(&c);
            verts.extend_from_slice(&frame.world(b).to_array());
            verts.extend_from_slice(&c);
        };
        let is_multiple = |v: f64, step: f64| ((v / step).round() * step - v).abs() < 1e-6;
        let (i0, i1) = ((x0 / minor).ceil() as i64, (x1 / minor).floor() as i64);
        for i in i0..=i1 {
            let x = i as f64 * minor;
            if x.abs() < 1e-9 {
                continue; // the axis is drawn below, on top
            }
            let c = if is_multiple(x, major) { major_c } else { minor_c };
            line([x, y0], [x, y1], c);
        }
        let (j0, j1) = ((y0 / minor).ceil() as i64, (y1 / minor).floor() as i64);
        for j in j0..=j1 {
            let y = j as f64 * minor;
            if y.abs() < 1e-9 {
                continue;
            }
            let c = if is_multiple(y, major) { major_c } else { minor_c };
            line([x0, y], [x1, y], c);
        }
        if x0 <= 0.0 && 0.0 <= x1 {
            line([0.0, y0], [0.0, y1], axis_y);
        }
        if y0 <= 0.0 && 0.0 <= y1 {
            line([x0, 0.0], [x1, 0.0], axis_x);
        }
        self.renderer.set_lines(LineLayer::Grid, &verts);
    }
}

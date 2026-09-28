//! Remembered settings for NEW items: the last value the user chose for
//! each creation control (floor thickness, void depth, wall thickness and
//! height mode, opening sizes per kind, outline shape per tool). Session
//! state — never in the document. The page persists it with its session
//! (`session_defaults_json` / `set_session_defaults`); editing the just
//! created item also updates the value remembered here.

use serde_json::{Value, json};
use vim_design_lib::wall_run::{Opening, OpeningKind};
use wasm_bindgen::prelude::*;

use super::{AuthorApp, MAX_WALL_HEIGHT_M, MAX_WALL_THICKNESS_M, MIN_WALL_HEIGHT_M, MIN_WALL_THICKNESS_M};
use crate::authoring::edit::presets::{DOOR_HEIGHT_M, DOOR_WIDTH_M, WINDOW_HEIGHT_M, WINDOW_SILL_M, WINDOW_WIDTH_M};
use crate::authoring::edit::session::DEFAULT_VOID_DEPTH_M;
use super::edit::{MAX_FACE_THICKNESS_M, MIN_FACE_THICKNESS_M};
use crate::authoring::openings::MIN_OPENING_SIZE_M;
use crate::authoring::sketch::Shape;

/// Depth a niche starts with when an opening is made a niche (m).
pub const DEFAULT_NICHE_DEPTH_M: f64 = 0.1;
/// Largest opening dimension the remembered sizes accept (m).
const MAX_OPENING_SIZE_M: f64 = 20.0;

/// The size a new opening of one kind is placed with.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OpeningSize {
    pub width: f64,
    pub height: f64,
    pub sill: f64,
    /// `None`: through the wall; `Some(d)`: a niche `d` deep.
    pub depth: Option<f64>,
}

impl OpeningSize {
    pub const WINDOW: Self = Self { width: WINDOW_WIDTH_M, height: WINDOW_HEIGHT_M, sill: WINDOW_SILL_M, depth: None };
    pub const DOOR: Self = Self { width: DOOR_WIDTH_M, height: DOOR_HEIGHT_M, sill: 0.0, depth: None };

    pub fn of(o: &Opening) -> Self {
        Self { width: o.width_m, height: o.height_m, sill: o.sill_m, depth: o.depth_m }
    }

    pub fn to_json(self) -> Value {
        json!({ "width": self.width, "height": self.height, "sill": self.sill, "depth": self.depth })
    }

    /// Read a persisted size; missing or bad values keep `fallback`'s.
    fn from_json(v: &Value, fallback: Self) -> Self {
        let num = |k: &str| v.get(k).and_then(Value::as_f64).unwrap_or(f64::NAN);
        let depth = v.get("depth").and_then(Value::as_f64);
        Self { width: num("width"), height: num("height"), sill: num("sill"), depth }.clean(fallback)
    }

    fn clean(self, fallback: Self) -> Self {
        let size = |v: f64, d: f64| if v.is_finite() { v.clamp(MIN_OPENING_SIZE_M, MAX_OPENING_SIZE_M) } else { d };
        Self {
            width: size(self.width, fallback.width),
            height: size(self.height, fallback.height),
            sill: if self.sill.is_finite() { self.sill.clamp(0.0, MAX_OPENING_SIZE_M) } else { fallback.sill },
            depth: self.depth.filter(|d| d.is_finite() && *d > 0.0),
        }
    }
}

/// Remembered values that have no older home among the app's fields.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Remembered {
    pub void_depth: f64,
    pub void_through: bool,
    pub window: OpeningSize,
    pub door: OpeningSize,
    /// The depth a niche starts with (the last niche depth chosen).
    pub niche_depth: f64,
    /// Room walls: the thickness and fixed height of a plane's first
    /// room's layout (the last chosen).
    pub room_wall_thickness: f64,
    pub room_wall_height: f64,
}

impl Default for Remembered {
    fn default() -> Self {
        Self {
            void_depth: DEFAULT_VOID_DEPTH_M,
            void_through: true,
            window: OpeningSize::WINDOW,
            door: OpeningSize::DOOR,
            niche_depth: DEFAULT_NICHE_DEPTH_M,
            room_wall_thickness: crate::authoring::walls::PARTITION_THICKNESS_M,
            room_wall_height: super::DEFAULT_WALL_HEIGHT_M,
        }
    }
}

impl Remembered {
    pub fn opening(&self, kind: OpeningKind) -> OpeningSize {
        match kind {
            OpeningKind::Window => self.window,
            OpeningKind::Door => self.door,
        }
    }

    /// Remember the size of an opening the user just set.
    pub fn remember_opening(&mut self, o: &Opening) {
        let size = OpeningSize::of(o);
        match o.kind {
            OpeningKind::Window => self.window = size,
            OpeningKind::Door => self.door = OpeningSize { sill: 0.0, ..size },
        }
        if let Some(d) = o.depth_m {
            self.niche_depth = d;
        }
    }
}

fn shape_of(name: Option<&str>, fallback: Shape) -> Shape {
    match name {
        Some("rect") => Shape::Rect,
        Some("polygon") => Shape::Polygon,
        _ => fallback,
    }
}

#[wasm_bindgen]
impl AuthorApp {
    /// Every remembered setting for new items, as JSON (the page stores
    /// it with its session).
    pub fn session_defaults_json(&self) -> String {
        let r = &self.remembered;
        json!({
            "floorThickness": self.plate_thickness,
            "voidDepth": r.void_depth,
            "voidThrough": r.void_through,
            "wallThickness": self.wall_thickness,
            "wallHeight": self.wall_height,
            "wallFlip": self.wall_flip,
            "wallMode": if self.wall_top.is_some() { "upto" } else { "fixed" },
            "wallTop": self.wall_top.map(|p| p.0 as f64),
            "wallTopOffset": self.wall_top_offset,
            "window": r.window.to_json(),
            "door": r.door.to_json(),
            "nicheDepth": r.niche_depth,
            "floorShape": self.shape.name(),
            "wallShape": self.wall_shape.name(),
            "roomShape": self.room_shape.name(),
            "roomWallThickness": r.room_wall_thickness,
            "roomWallHeight": r.room_wall_height,
        })
        .to_string()
    }

    /// Restore remembered settings (after the document loaded, so a
    /// remembered top plane is found). Unknown or bad values keep the
    /// current ones.
    pub fn set_session_defaults(&mut self, json: &str) {
        let Ok(j) = serde_json::from_str::<Value>(json) else { return };
        let num = |k: &str| j.get(k).and_then(Value::as_f64).filter(|x| x.is_finite());
        let flag = |k: &str| j.get(k).and_then(Value::as_bool);
        let text = |k: &str| j.get(k).and_then(Value::as_str);
        if let Some(t) = num("floorThickness") {
            self.plate_thickness = t.clamp(MIN_FACE_THICKNESS_M, MAX_FACE_THICKNESS_M);
        }
        if let Some(d) = num("voidDepth") {
            self.remembered.void_depth = d.clamp(MIN_FACE_THICKNESS_M, MAX_FACE_THICKNESS_M);
        }
        if let Some(t) = flag("voidThrough") {
            self.remembered.void_through = t;
        }
        if let Some(t) = num("wallThickness") {
            self.wall_thickness = t.clamp(MIN_WALL_THICKNESS_M, MAX_WALL_THICKNESS_M);
        }
        if let Some(h) = num("wallHeight") {
            self.wall_height = h.clamp(MIN_WALL_HEIGHT_M, MAX_WALL_HEIGHT_M);
        }
        if let Some(f) = flag("wallFlip") {
            self.wall_flip = f;
        }
        if let Some(mode) = text("wallMode") {
            // A remembered top plane that is gone falls back to a fixed
            // height (set_wall_height_mode checks the plane).
            let plane = num("wallTop").unwrap_or(-1.0);
            self.set_wall_height_mode(mode, plane, num("wallTopOffset").unwrap_or(f64::NAN));
        }
        if let Some(w) = j.get("window") {
            self.remembered.window = OpeningSize::from_json(w, OpeningSize::WINDOW);
        }
        if let Some(d) = j.get("door") {
            self.remembered.door = OpeningSize { sill: 0.0, ..OpeningSize::from_json(d, OpeningSize::DOOR) };
        }
        if let Some(d) = num("nicheDepth").filter(|d| *d > 0.0) {
            self.remembered.niche_depth = d.min(MAX_WALL_THICKNESS_M);
        }
        self.shape = shape_of(text("floorShape"), self.shape);
        self.wall_shape = shape_of(text("wallShape"), self.wall_shape);
        self.room_shape = shape_of(text("roomShape"), self.room_shape);
        if let Some(t) = num("roomWallThickness") {
            self.remembered.room_wall_thickness = t.clamp(MIN_WALL_THICKNESS_M, MAX_WALL_THICKNESS_M);
        }
        if let Some(h) = num("roomWallHeight") {
            self.remembered.room_wall_height = h.clamp(MIN_WALL_HEIGHT_M, MAX_WALL_HEIGHT_M);
        }
    }
}

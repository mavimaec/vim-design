//! Plan span: the active level's view range (the library's `PlanSpan`,
//! document data per level: saved, exported, undoable). The part of any
//! element above the span's top is drawn see-through with the level's
//! "above" opacity, the part below its bottom with the "below" opacity;
//! the plan view cuts at the span's cut height. A session toggle turns
//! the see-through bands off (every element drawn normally; the plan
//! still cuts at the cut height).
//!
//! Picking follows what is drawn: a hit in a band fainter than
//! [`PICK_MIN_OPACITY`] is passed through, so the work inside the span
//! stays pickable under see-through floors.

use vim_design_lib::plan_span::{self, ResolvedSpan, SpanTop};
use vim_design_lib::{Command, EntityId};
use wasm_bindgen::prelude::*;

use super::{AuthorApp, eid};
use crate::render::SpanBands;

/// A hit in a see-through band at or under this opacity is picked
/// through (the default "above" band, 0.25, is).
pub const PICK_MIN_OPACITY: f32 = 0.3;
/// Offsets of a span edit, relative to its level (meters).
const MAX_SPAN_OFFSET_M: f64 = 100.0;

fn top_json(top: SpanTop) -> serde_json::Value {
    match top {
        SpanTop::NextStory => serde_json::json!({ "mode": "next", "offset": null }),
        SpanTop::Offset(o) => serde_json::json!({ "mode": "offset", "offset": o }),
    }
}

#[wasm_bindgen]
impl AuthorApp {
    /// The see-through bands on (session state; the page persists it).
    pub fn set_plan_span_enabled(&mut self, on: bool) {
        self.plan_span_on = on;
        self.apply_span();
    }

    pub fn plan_span_enabled(&self) -> bool {
        self.plan_span_on
    }

    /// A level's plan span: its data (or the defaults), whether it has its
    /// own `PlanSpan`, and the resolved heights (world z). `level` < 0:
    /// the active plane's level.
    pub fn plan_span_json(&self, level: f64) -> String {
        let Some(level) = self.span_level(level) else { return "null".to_owned() };
        let data = plan_span::data_of_level(&self.doc, level);
        let r = plan_span::resolve(&self.doc, level);
        serde_json::json!({
            "level": level.0 as f64,
            "custom": plan_span::of_level(&self.doc, level).is_some(),
            "top": top_json(data.top),
            "cut": data.cut_offset_m, "bottom": data.bottom_offset_m,
            "above": data.above_opacity, "below": data.below_opacity,
            "topZ": r.map(|r| r.top_z), "cutZ": r.map(|r| r.cut_z), "bottomZ": r.map(|r| r.bottom_z),
            "topRaised": r.is_some_and(|r| r.top_raised),
            "enabled": self.plan_span_on,
            "active": self.span_level(-1.0) == Some(level),
        })
        .to_string()
    }

    /// Edit a level's plan span (NaN keeps a value; `top` "next" /
    /// "offset" / "" keeps it): the first edit creates the level's
    /// `PlanSpan`, later ones update it — one undo step per gesture
    /// (drags coalesce until `end_gesture`). Returns "" or why it was
    /// refused.
    #[allow(clippy::too_many_arguments)]
    pub fn set_plan_span(
        &mut self,
        level: f64,
        top: &str,
        top_offset: f64,
        cut: f64,
        bottom: f64,
        above: f32,
        below: f32,
    ) -> String {
        let Some(level) = self.span_level(level) else { return "Not a level".to_owned() };
        let mut data = plan_span::data_of_level(&self.doc, level);
        let clamp = |v: f64| v.clamp(-MAX_SPAN_OFFSET_M, MAX_SPAN_OFFSET_M);
        match top {
            "next" => data.top = SpanTop::NextStory,
            "offset" => {
                let current = match data.top {
                    SpanTop::Offset(o) => o,
                    SpanTop::NextStory => plan_span::resolve(&self.doc, level).map_or(plan_span::DEFAULT_STORY_HEIGHT_M, |r| {
                        r.top_z - self.plane_elevation(level)
                    }),
                };
                data.top = SpanTop::Offset(if top_offset.is_finite() { clamp(top_offset) } else { current });
            }
            _ => {
                if top_offset.is_finite() {
                    data.top = SpanTop::Offset(clamp(top_offset));
                }
            }
        }
        if cut.is_finite() {
            data.cut_offset_m = clamp(cut);
        }
        if bottom.is_finite() {
            data.bottom_offset_m = clamp(bottom);
        }
        if above.is_finite() {
            data.above_opacity = above.clamp(0.0, 1.0);
        }
        if below.is_finite() {
            data.below_opacity = below.clamp(0.0, 1.0);
        }
        if let Err(e) = plan_span::validate(&data) {
            let s = e.to_string();
            let mut c = s.chars();
            return c.next().map_or(s.clone(), |f| f.to_uppercase().collect::<String>() + c.as_str());
        }
        let key = format!("plan_span_{}", level.0);
        let cmd = match plan_span::of_level(&self.doc, level) {
            Some(id) => {
                let coalesce = self.gestures.begin_continuing(&self.doc, &key);
                Command::UpdatePlanSpan {
                    id,
                    top: Some(data.top),
                    cut_offset_m: Some(data.cut_offset_m),
                    bottom_offset_m: Some(data.bottom_offset_m),
                    above_opacity: Some(data.above_opacity),
                    below_opacity: Some(data.below_opacity),
                    coalesce,
                }
            }
            None => {
                // A create cannot coalesce: it opens the gesture.
                self.gestures.begin_continuing(&self.doc, &key);
                Command::CreatePlanSpan {
                    level,
                    top: data.top,
                    cut_offset_m: data.cut_offset_m,
                    bottom_offset_m: data.bottom_offset_m,
                    above_opacity: data.above_opacity,
                    below_opacity: data.below_opacity,
                }
            }
        };
        match self.doc.submit(cmd) {
            Ok(_) => {
                self.sync("plan span");
                String::new()
            }
            Err(st) => {
                self.sync("plan span (refused)");
                format!("The plan span was refused ({st:?})")
            }
        }
    }

    /// Back to the defaults: the level's `PlanSpan` is deleted (one undo
    /// step). False when it had none.
    pub fn reset_plan_span(&mut self, level: f64) -> bool {
        let Some(id) = self.span_level(level).and_then(|l| plan_span::of_level(&self.doc, l)) else { return false };
        let depth = self.doc.undo_depth();
        match self.doc.submit(Command::DeletePlanSpan { id }) {
            Ok(_) => {
                self.gestures.one_shot(depth);
                self.sync("plan span reset");
                true
            }
            Err(_) => false,
        }
    }
}

impl AuthorApp {
    /// A level id, or (`id` < 0) the active plane's root level.
    fn span_level(&self, id: f64) -> Option<EntityId> {
        let level = if id >= 0.0 { eid(id) } else { self.root_level(self.plane()?)? };
        (self.root_level(level) == Some(level)).then_some(level)
    }

    /// The active level's resolved span.
    pub(super) fn active_span(&self) -> Option<ResolvedSpan> {
        plan_span::resolve(&self.doc, self.span_level(-1.0)?)
    }

    /// Before a frame: the plan cut and the see-through bands.
    pub(super) fn apply_span(&mut self) {
        let span = self.active_span();
        self.camera.cut_z = span.map(|s| s.cut_z as f32);
        self.renderer.span = span.filter(|_| self.plan_span_on).map(|s| SpanBands {
            top_z: s.top_z as f32,
            bottom_z: s.bottom_z as f32,
            above_opacity: s.above_opacity,
            below_opacity: s.below_opacity,
        });
    }

    /// A pick hit at world height `z` counts (it is not in a band fainter
    /// than [`PICK_MIN_OPACITY`]).
    pub(super) fn pickable_z(&self, z: f32) -> bool {
        let Some(s) = self.renderer.span else { return true };
        let eps = 1e-3;
        if z > s.top_z + eps {
            s.above_opacity > PICK_MIN_OPACITY
        } else if z < s.bottom_z - eps {
            s.below_opacity > PICK_MIN_OPACITY
        } else {
            true
        }
    }
}

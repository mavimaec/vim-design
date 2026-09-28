//! Plan spans: the per-level view range.
//!
//! A `PlanSpan` belongs to one level and defines how the views treat
//! geometry by height around that level: the band between `bottom` and
//! `top` is shown normally, the part of every element above `top` is
//! drawn see-through with `above_opacity`, the part below `bottom` with
//! `below_opacity` (0 = invisible, 1 = normal), and plan views cut at
//! `cut`. It applies to plan and 3D views.
//!
//! This is the one deliberate exception to "view state never lives in
//! the document": the user chose the span as per-level project data (like
//! the view range of a floor plan), so it is saved, exported, and undoable.
//! It is metadata: it generates no geometry and never counts as an
//! element.
//!
//! A level without a `PlanSpan` uses the defaults ([`PlanSpanData::default`]).

use serde::{Deserialize, Serialize};

use crate::document::Document;
use crate::entity::{EntityKind, Params, slot};
use crate::id::EntityId;

/// Default plan cut height above the level (meters).
pub const DEFAULT_CUT_OFFSET_M: f64 = 1.2;
/// Default bottom of the span relative to the level (meters).
pub const DEFAULT_BOTTOM_OFFSET_M: f64 = 0.0;
/// Default opacity of what lies above the span.
pub const DEFAULT_ABOVE_OPACITY: f32 = 0.25;
/// Default opacity of what lies below the span.
pub const DEFAULT_BELOW_OPACITY: f32 = 0.35;
/// The span top of the highest story (no story level above it): this
/// far above the level (meters).
pub const DEFAULT_STORY_HEIGHT_M: f64 = 3.0;

/// Where the span ends above the level.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum SpanTop {
    /// At the elevation of the next building-story level above
    /// ([`DEFAULT_STORY_HEIGHT_M`] above the level when there is none).
    NextStory,
    /// This far above the level (meters).
    Offset(f64),
}

/// The data of a plan span, mirroring `Params::PlanSpan`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PlanSpanData {
    pub top: SpanTop,
    pub cut_offset_m: f64,
    pub bottom_offset_m: f64,
    pub above_opacity: f32,
    pub below_opacity: f32,
}

impl Default for PlanSpanData {
    fn default() -> Self {
        PlanSpanData {
            top: SpanTop::NextStory,
            cut_offset_m: DEFAULT_CUT_OFFSET_M,
            bottom_offset_m: DEFAULT_BOTTOM_OFFSET_M,
            above_opacity: DEFAULT_ABOVE_OPACITY,
            below_opacity: DEFAULT_BELOW_OPACITY,
        }
    }
}

impl PlanSpanData {
    /// The plan span data of `Params::PlanSpan`.
    pub fn from_params(params: &Params) -> Option<PlanSpanData> {
        match params {
            Params::PlanSpan {
                top,
                cut_offset_m,
                bottom_offset_m,
                above_opacity,
                below_opacity,
            } => Some(PlanSpanData {
                top: *top,
                cut_offset_m: *cut_offset_m,
                bottom_offset_m: *bottom_offset_m,
                above_opacity: *above_opacity,
                below_opacity: *below_opacity,
            }),
            _ => None,
        }
    }

    /// `Params::PlanSpan` with this data.
    pub fn into_params(self) -> Params {
        Params::PlanSpan {
            top: self.top,
            cut_offset_m: self.cut_offset_m,
            bottom_offset_m: self.bottom_offset_m,
            above_opacity: self.above_opacity,
            below_opacity: self.below_opacity,
        }
    }
}

/// Typed failure of a plan span check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanSpanError {
    /// An offset or opacity is NaN or infinite.
    NonFinite,
    /// An opacity is outside 0..=1.
    OpacityOutOfRange,
    /// The bottom is not below the cut.
    BottomNotBelowCut,
    /// The cut is not below a fixed top.
    CutNotBelowTop,
}

impl std::fmt::Display for PlanSpanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PlanSpanError::NonFinite => write!(f, "an offset or opacity is not finite"),
            PlanSpanError::OpacityOutOfRange => write!(f, "an opacity must be between 0 and 1"),
            PlanSpanError::BottomNotBelowCut => write!(f, "the span bottom must be below the cut"),
            PlanSpanError::CutNotBelowTop => write!(f, "the cut must be below the span top"),
        }
    }
}

impl std::error::Error for PlanSpanError {}

/// Structural validity, as the plan span commands require it: finite
/// offsets and opacities, opacities in 0..=1, bottom < cut, and cut <
/// top for a fixed top. For `NextStory` the top is only known at
/// resolution ([`resolve`] raises it to the cut when the next story is
/// lower).
pub fn validate(span: &PlanSpanData) -> Result<(), PlanSpanError> {
    let top = match span.top {
        SpanTop::Offset(top) => Some(top),
        SpanTop::NextStory => None,
    };
    let finite = [span.cut_offset_m, span.bottom_offset_m].iter().chain(top.iter()).all(|v| v.is_finite())
        && span.above_opacity.is_finite()
        && span.below_opacity.is_finite();
    if !finite {
        return Err(PlanSpanError::NonFinite);
    }
    if !(0.0..=1.0).contains(&span.above_opacity) || !(0.0..=1.0).contains(&span.below_opacity) {
        return Err(PlanSpanError::OpacityOutOfRange);
    }
    if span.bottom_offset_m >= span.cut_offset_m {
        return Err(PlanSpanError::BottomNotBelowCut);
    }
    if top.is_some_and(|top| span.cut_offset_m >= top) {
        return Err(PlanSpanError::CutNotBelowTop);
    }
    Ok(())
}

/// A plan span in world z (meters).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ResolvedSpan {
    pub top_z: f64,
    pub cut_z: f64,
    pub bottom_z: f64,
    pub above_opacity: f32,
    pub below_opacity: f32,
    /// The next story is at or below the cut, so the top was raised to
    /// the cut (nothing between cut and top is shown normally).
    pub top_raised: bool,
}

/// The `PlanSpan` entity of level `level`, if it has one.
pub fn of_level(doc: &Document, level: EntityId) -> Option<EntityId> {
    let graph = doc.graph_ref();
    graph.dependents(level).into_iter().find(|id| {
        graph.get(*id).is_some_and(|r| {
            r.kind() == EntityKind::PlanSpan
                && r.inputs.get(slot::PLAN_SPAN_LEVEL).and_then(|s| s.referenced().next()) == Some(level)
        })
    })
}

/// The span data of level `level`: its `PlanSpan`, or the defaults.
pub fn data_of_level(doc: &Document, level: EntityId) -> PlanSpanData {
    of_level(doc, level)
        .and_then(|id| doc.entity(id))
        .and_then(|r| PlanSpanData::from_params(&r.params))
        .unwrap_or_default()
}

/// The elevation of the next building-story level above `elevation`.
fn next_story(doc: &Document, level: EntityId, elevation: f64) -> Option<f64> {
    doc.entities()
        .filter(|(id, _)| **id != level)
        .filter_map(|(_, r)| match &r.params {
            Params::Level { elevation_m, is_building_story: true, .. } if *elevation_m > elevation => {
                Some(*elevation_m)
            }
            _ => None,
        })
        .min_by(f64::total_cmp)
}

/// The resolved span of level `level` in world z, from params only (the
/// level's `PlanSpan` or the defaults). `None` when `level` is not a
/// level.
pub fn resolve(doc: &Document, level: EntityId) -> Option<ResolvedSpan> {
    let elevation = match &doc.entity(level)?.params {
        Params::Level { elevation_m, .. } => *elevation_m,
        _ => return None,
    };
    let span = data_of_level(doc, level);
    let cut_z = elevation + span.cut_offset_m;
    let wanted_top = match span.top {
        SpanTop::Offset(top) => elevation + top,
        SpanTop::NextStory => next_story(doc, level, elevation).unwrap_or(elevation + DEFAULT_STORY_HEIGHT_M),
    };
    let top_raised = wanted_top < cut_z;
    Some(ResolvedSpan {
        top_z: wanted_top.max(cut_z),
        cut_z,
        bottom_z: elevation + span.bottom_offset_m,
        above_opacity: span.above_opacity,
        below_opacity: span.below_opacity,
        top_raised,
    })
}

/// The resolved span for geometry on construction plane `plane`: the
/// span of the plane's root level (workplane nesting does not matter).
pub fn resolve_for_plane(doc: &Document, plane: EntityId) -> Option<ResolvedSpan> {
    resolve(doc, crate::workplane::root_level(doc, plane)?)
}

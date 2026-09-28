//! Plan spans: per-level view ranges stored in the document.

use proptest::prelude::*;
use vim_design_lib::eval::{Engine, Evaluated};
use vim_design_lib::plan_span::{
    self, DEFAULT_ABOVE_OPACITY, DEFAULT_BELOW_OPACITY, DEFAULT_CUT_OFFSET_M, DEFAULT_STORY_HEIGHT_M,
    PlanSpanData, PlanSpanError, ResolvedSpan, SpanTop,
};
use vim_design_lib::{Command, Document, EntityId, EntityKind, VimStatus};
use vim_design_test::{one, ok, save};

fn level(doc: &mut Document, name: &str, elevation_m: f64, is_building_story: bool) -> EntityId {
    one(
        doc,
        Command::CreateLevel {
            name: name.to_owned(),
            elevation_m,
            is_building_story,
            color: [0.2, 0.5, 0.9, 0.3],
            extent_m: 10.0,
        },
    )
}

fn set_elevation(doc: &mut Document, id: EntityId, elevation: f64) {
    ok(
        doc,
        Command::UpdateLevel {
            id,
            name: None,
            elevation_m: Some(elevation),
            is_building_story: None,
            color: None,
            extent_m: None,
            coalesce: false,
        },
    );
}

fn create(level: EntityId, span: PlanSpanData) -> Command {
    Command::CreatePlanSpan {
        level,
        top: span.top,
        cut_offset_m: span.cut_offset_m,
        bottom_offset_m: span.bottom_offset_m,
        above_opacity: span.above_opacity,
        below_opacity: span.below_opacity,
    }
}

fn update(id: EntityId, above: Option<f32>, top: Option<SpanTop>, coalesce: bool) -> Command {
    Command::UpdatePlanSpan {
        id,
        top,
        cut_offset_m: None,
        bottom_offset_m: None,
        above_opacity: above,
        below_opacity: None,
        coalesce,
    }
}

fn near(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-12
}

#[test]
fn a_level_without_a_plan_span_uses_the_defaults() {
    let mut doc = Document::new();
    let ground = level(&mut doc, "Ground", 0.5, true);
    let _roof = level(&mut doc, "Roof", 9.0, false);
    // The highest story: the default story height above the level.
    assert_eq!(
        plan_span::resolve(&doc, ground),
        Some(ResolvedSpan {
            top_z: 0.5 + DEFAULT_STORY_HEIGHT_M,
            cut_z: 0.5 + DEFAULT_CUT_OFFSET_M,
            bottom_z: 0.5,
            above_opacity: DEFAULT_ABOVE_OPACITY,
            below_opacity: DEFAULT_BELOW_OPACITY,
            top_raised: false,
        })
    );
    assert_eq!(plan_span::of_level(&doc, ground), None);
    assert_eq!(plan_span::data_of_level(&doc, ground), PlanSpanData::default());
    // Not a level: no span.
    let workplane = one(
        &mut doc,
        Command::CreateWorkplane {
            parent: ground,
            name: "Ceiling".to_owned(),
            offset_m: 2.6,
            color: [0.5; 4],
            extent_m: 5.0,
        },
    );
    assert_eq!(plan_span::resolve(&doc, workplane), None);
    // Geometry on a workplane uses its root level's span.
    assert_eq!(plan_span::resolve_for_plane(&doc, workplane), plan_span::resolve(&doc, ground));
}

#[test]
fn next_story_tracks_the_story_level_above() {
    let mut doc = Document::new();
    let ground = level(&mut doc, "Ground", 0.0, true);
    let _mezzanine = level(&mut doc, "Mezzanine", 1.5, false);
    let second = level(&mut doc, "Level 2", 3.2, true);
    let _third = level(&mut doc, "Level 3", 6.4, true);
    let top = |doc: &Document| plan_span::resolve(doc, ground).map(|s| s.top_z);
    // Non-story levels do not count.
    assert_eq!(top(&doc), Some(3.2));
    set_elevation(&mut doc, second, 3.6);
    assert_eq!(top(&doc), Some(3.6));
    // A story below the cut raises the top to the cut.
    set_elevation(&mut doc, second, 1.0);
    let span = plan_span::resolve(&doc, ground).expect("span");
    assert!(span.top_raised && near(span.top_z, span.cut_z));
    // The upper story's own span reaches the next story up.
    assert_eq!(plan_span::resolve(&doc, second).map(|s| s.top_z), Some(6.4));
}

#[test]
fn offsets_resolve_in_world_z() {
    let mut doc = Document::new();
    let ground = level(&mut doc, "Level 2", 3.0, true);
    let span = PlanSpanData {
        top: SpanTop::Offset(2.4),
        cut_offset_m: 1.0,
        bottom_offset_m: -0.5,
        above_opacity: 0.0,
        below_opacity: 1.0,
    };
    let id = one(&mut doc, create(ground, span));
    assert_eq!(plan_span::of_level(&doc, ground), Some(id));
    assert_eq!(
        plan_span::resolve(&doc, ground),
        Some(ResolvedSpan {
            top_z: 5.4,
            cut_z: 4.0,
            bottom_z: 2.5,
            above_opacity: 0.0,
            below_opacity: 1.0,
            top_raised: false,
        })
    );
    // The span moves with its level.
    set_elevation(&mut doc, ground, 4.0);
    assert_eq!(plan_span::resolve(&doc, ground).map(|s| s.cut_z), Some(5.0));
    // It evaluates to plain data and has no mesh.
    let mut engine = Engine::new();
    engine.evaluate_pending(&mut doc);
    let updates = engine.poll_updates(&doc);
    assert!(updates.errors.is_empty(), "{:?}", updates.errors);
    assert!(matches!(engine.value(id), Some(Evaluated::PlanSpan(data)) if *data == span));
    assert!(engine.mesh(id).is_none());
    assert!(updates.meshes.is_empty());
    assert!(EntityKind::PlanSpan.is_metadata() && EntityKind::Site.is_metadata());
    assert!(!EntityKind::Element.is_metadata());
}

#[test]
fn one_plan_span_per_level_and_invalid_spans_reject() {
    let mut doc = Document::new();
    let ground = level(&mut doc, "Ground", 0.0, true);
    let second = level(&mut doc, "Level 2", 3.0, true);
    one(&mut doc, create(ground, PlanSpanData::default()));
    let bytes = save(&doc);
    assert_eq!(doc.submit(create(ground, PlanSpanData::default())).err(), Some(VimStatus::SingletonExists));
    one(&mut doc, create(second, PlanSpanData::default()));
    doc.undo().expect("undo");
    assert_eq!(save(&doc), bytes);
    let bad = |span: PlanSpanData| create(second, span);
    let base = PlanSpanData::default();
    for (span, error) in [
        (PlanSpanData { above_opacity: 1.5, ..base }, PlanSpanError::OpacityOutOfRange),
        (PlanSpanData { below_opacity: -0.1, ..base }, PlanSpanError::OpacityOutOfRange),
        (PlanSpanData { cut_offset_m: f64::NAN, ..base }, PlanSpanError::NonFinite),
        (PlanSpanData { bottom_offset_m: 1.2, ..base }, PlanSpanError::BottomNotBelowCut),
        (PlanSpanData { top: SpanTop::Offset(1.0), ..base }, PlanSpanError::CutNotBelowTop),
        (PlanSpanData { top: SpanTop::Offset(f64::INFINITY), ..base }, PlanSpanError::NonFinite),
    ] {
        assert_eq!(plan_span::validate(&span), Err(error));
        assert_eq!(doc.submit(bad(span)).err(), Some(VimStatus::InvalidPlanSpan));
    }
    assert_eq!(save(&doc), bytes, "rejections change nothing");
    // Only a level takes a plan span.
    let workplane = one(
        &mut doc,
        Command::CreateWorkplane {
            parent: ground,
            name: "Ceiling".to_owned(),
            offset_m: 2.6,
            color: [0.5; 4],
            extent_m: 5.0,
        },
    );
    assert_eq!(
        doc.submit(create(workplane, PlanSpanData::default())).err(),
        Some(VimStatus::SlotKindMismatch)
    );
}

#[test]
fn the_level_cascade_deletes_the_plan_span_and_updates_undo_byte_exactly() {
    let mut doc = Document::new();
    let ground = level(&mut doc, "Ground", 0.0, true);
    let id = one(&mut doc, create(ground, PlanSpanData::default()));
    let before = save(&doc);
    // A slider drag of 10 coalesced updates is one undo step.
    let depth = doc.undo_depth();
    for step in 1..=10 {
        ok(&mut doc, update(id, Some(0.05 * step as f32), None, true));
    }
    assert_eq!(doc.undo_depth(), depth + 1);
    assert_eq!(plan_span::resolve(&doc, ground).map(|s| s.above_opacity), Some(0.5));
    ok(&mut doc, update(id, None, Some(SpanTop::Offset(2.0)), false));
    let after = save(&doc);
    doc.undo().expect("undo");
    doc.undo().expect("undo");
    assert_eq!(save(&doc), before, "byte-exact undo");
    doc.redo().expect("redo");
    doc.redo().expect("redo");
    assert_eq!(save(&doc), after, "byte-exact redo");
    assert_eq!(doc.submit(update(id, None, Some(SpanTop::Offset(0.5)), false)).err(), Some(VimStatus::InvalidPlanSpan));

    // A plain level delete is rejected; the cascade takes the span.
    assert_eq!(doc.submit(Command::DeleteLevel { id: ground, cascade: false }).err(), Some(VimStatus::HasDependents));
    ok(&mut doc, Command::DeleteLevel { id: ground, cascade: true });
    assert!(doc.entity(id).is_none());
    doc.undo().expect("undo");
    assert_eq!(save(&doc), after);
    ok(&mut doc, Command::DeletePlanSpan { id });
    assert_eq!(plan_span::of_level(&doc, ground), None);
    doc.undo().expect("undo");
    assert_eq!(save(&doc), after);
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// Every valid span resolves with bottom < cut <= top in world z.
    #[test]
    fn valid_spans_resolve_in_order(
        elevation in -10i16..10,
        cut in 1i16..40,
        bottom in -40i16..40,
        top in prop::option::of(1i16..60),
        next in prop::option::of(-5i16..60),
        above in 0u8..=100,
        below in 0u8..=100,
    ) {
        let mut doc = Document::new();
        let e = f64::from(elevation) * 0.1;
        let lvl = level(&mut doc, "L", e, true);
        if let Some(next) = next {
            level(&mut doc, "N", e + f64::from(next) * 0.1, true);
        }
        let span = PlanSpanData {
            top: top.map_or(SpanTop::NextStory, |t| SpanTop::Offset(f64::from(t) * 0.1)),
            cut_offset_m: f64::from(cut) * 0.1,
            bottom_offset_m: f64::from(bottom) * 0.1,
            above_opacity: f32::from(above) / 100.0,
            below_opacity: f32::from(below) / 100.0,
        };
        let accepted = doc.submit(create(lvl, span)).is_ok();
        prop_assert_eq!(accepted, plan_span::validate(&span).is_ok());
        let resolved = plan_span::resolve(&doc, lvl).expect("resolved");
        prop_assert!(resolved.bottom_z < resolved.cut_z);
        prop_assert!(resolved.cut_z <= resolved.top_z);
        prop_assert!((0.0..=1.0).contains(&resolved.above_opacity));
    }
}

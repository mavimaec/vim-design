//! The dirty pump's parametric changed-set (docs/ARCHITECTURE.md §6.3):
//! `Updates::params_changed` reports delta TARGETS (not the downstream
//! closure), through one commit-gate code path shared by submit, undo,
//! and redo — plus the interest filter and the deleted-id rule.

use std::collections::BTreeSet;

use vim_design_lib::eval::Engine;
use vim_design_lib::{Command, Document, EntityId, VimStatus};
use vim_design_test::{build_cube, one, ok};

/// Evaluate + poll, returning the params_changed vector.
fn pump(doc: &mut Document, engine: &mut Engine) -> Vec<EntityId> {
    engine.evaluate_pending(doc);
    engine.poll_updates(doc).params_changed
}

#[test]
fn update_reports_the_target_not_the_downstream_closure() {
    let mut doc = Document::new();
    let mut engine = Engine::new();
    let cube = build_cube(&mut doc, [0.0, 0.0, 0.0], 1.0, 1.0);
    let _ = pump(&mut doc, &mut engine); // drain creation dirt

    let corner = cube.base.cps[1];
    ok(
        &mut doc,
        Command::UpdateControlPoint {
            id: corner,
            position: [1.2, 0.0, 0.0],
            coalesce: false,
        },
    );
    engine.evaluate_pending(&mut doc);
    let updates = engine.poll_updates(&doc);

    // Exactly the touched control point — even though the whole cube
    // chain re-evaluated and the mesh was re-delivered.
    assert_eq!(updates.params_changed, vec![corner]);
    assert_eq!(updates.meshes.len(), 1, "geometry still repumps normally");

    // Second poll with no edits: empty and settled.
    let second = engine.poll_updates(&doc);
    assert!(second.params_changed.is_empty());
    assert!(second.is_settled_and_empty(), "{second:?}");
}

#[test]
fn undo_and_redo_report_through_the_same_path() {
    let mut doc = Document::new();
    let mut engine = Engine::new();
    let cube = build_cube(&mut doc, [0.0, 0.0, 0.0], 1.0, 1.0);
    let corner = cube.base.cps[2];
    ok(
        &mut doc,
        Command::UpdateControlPoint {
            id: corner,
            position: [1.5, 1.5, 0.0],
            coalesce: false,
        },
    );
    let _ = pump(&mut doc, &mut engine);

    // Undo is not special: the inverted delta targets the same id.
    doc.undo().expect("undo");
    assert_eq!(pump(&mut doc, &mut engine), vec![corner]);

    // Redo likewise.
    doc.redo().expect("redo");
    assert_eq!(pump(&mut doc, &mut engine), vec![corner]);
}

#[test]
fn composites_report_every_constituent_and_undo_of_the_group_repeats_them() {
    let mut doc = Document::new();
    let mut engine = Engine::new();
    let created = ok(
        &mut doc,
        Command::CreateCylinder {
            center: [0.0, 0.0, 0.0],
            radius: 0.5,
            height: 2.0,
        },
    )
    .created_ids;
    let mut expected = created.clone();
    expected.sort();

    assert_eq!(pump(&mut doc, &mut engine), expected, "all 8 constituents");

    doc.undo().expect("undo cylinder group");
    assert_eq!(
        pump(&mut doc, &mut engine),
        expected,
        "undo of the group reports all constituents again (now deleted)"
    );
    doc.redo().expect("redo cylinder group");
    assert_eq!(pump(&mut doc, &mut engine), expected);
}

#[test]
fn five_hundred_updates_between_polls_report_once() {
    let mut doc = Document::new();
    let mut engine = Engine::new();
    let cube = build_cube(&mut doc, [0.0, 0.0, 0.0], 1.0, 1.0);
    let _ = pump(&mut doc, &mut engine);

    let top = cube.path_cps[1];
    for i in 1..=500 {
        ok(
            &mut doc,
            Command::UpdateControlPoint {
                id: top,
                position: [0.0, 0.0, 1.0 + f64::from(i) * 0.001],
                coalesce: true,
            },
        );
    }
    assert_eq!(pump(&mut doc, &mut engine), vec![top], "sets, not queues");
}

#[test]
fn watch_set_filters_params_only_and_at_drain_time() {
    let mut doc = Document::new();
    let mut engine = Engine::new();
    let cube = build_cube(&mut doc, [0.0, 0.0, 0.0], 1.0, 1.0);
    let _ = pump(&mut doc, &mut engine);

    let watched = cube.path_cps[1];
    let unwatched = cube.base.cps[0];
    engine.set_params_watch(Some(BTreeSet::from([watched])));

    ok(
        &mut doc,
        Command::UpdateControlPoint {
            id: watched,
            position: [0.0, 0.0, 1.4],
            coalesce: false,
        },
    );
    ok(
        &mut doc,
        Command::UpdateControlPoint {
            id: unwatched,
            position: [-0.1, -0.1, 0.0],
            coalesce: false,
        },
    );
    engine.evaluate_pending(&mut doc);
    let updates = engine.poll_updates(&doc);
    assert_eq!(updates.params_changed, vec![watched], "filter trims to the watch set");
    assert!(
        !updates.meshes.is_empty(),
        "mesh reporting is never filtered by the watch set"
    );

    // Documented drain rule: the unwatched id's dirt was DISCARDED by
    // the drain, not retained — unsetting the watch does not resurrect it.
    engine.set_params_watch(None);
    let after = engine.poll_updates(&doc);
    assert!(after.params_changed.is_empty(), "{after:?}");

    // With the watch unset, fresh edits to both ids report both.
    ok(
        &mut doc,
        Command::UpdateControlPoint {
            id: watched,
            position: [0.0, 0.0, 1.5],
            coalesce: false,
        },
    );
    ok(
        &mut doc,
        Command::UpdateControlPoint {
            id: unwatched,
            position: [-0.2, -0.2, 0.0],
            coalesce: false,
        },
    );
    let mut expected = vec![watched, unwatched];
    expected.sort();
    assert_eq!(pump(&mut doc, &mut engine), expected);
}

#[test]
fn deleted_ids_are_reported_once_and_delete_plus_undo_coalesces_alive() {
    let mut doc = Document::new();
    let mut engine = Engine::new();
    let lone = one(
        &mut doc,
        Command::CreateControlPoint {
            position: [7.0, 7.0, 7.0],
        },
    );
    let _ = pump(&mut doc, &mut engine);

    // Delete: the Remove delta targets the id, so the id is reported
    // even though the entity no longer exists at poll time — the bound
    // widget needs to hear it went away. Reported once, then silence.
    ok(&mut doc, Command::DeleteControlPoint { id: lone });
    assert_eq!(pump(&mut doc, &mut engine), vec![lone]);
    assert!(doc.entity(lone).is_none(), "gone at poll time, still reported");
    assert!(engine.poll_updates(&doc).params_changed.is_empty());

    // Undo of the delete re-inserts: reported again, now alive.
    doc.undo().expect("undo delete");
    assert_eq!(pump(&mut doc, &mut engine), vec![lone]);
    assert!(doc.entity(lone).is_some());

    // Delete + undo BETWEEN polls: one report, entity alive.
    ok(&mut doc, Command::DeleteControlPoint { id: lone });
    doc.undo().expect("undo delete again");
    assert_eq!(pump(&mut doc, &mut engine), vec![lone]);
    assert!(doc.entity(lone).is_some());
}

#[test]
fn rejected_commands_leave_the_pump_empty() {
    let mut doc = Document::new();
    let mut engine = Engine::new();
    let cube = build_cube(&mut doc, [0.0, 0.0, 0.0], 1.0, 1.0);
    let _ = pump(&mut doc, &mut engine);

    // Rejected mid-composite: speculative deltas rolled back — the
    // commit gate never sees them, so nothing is reported.
    assert_eq!(
        doc.submit(Command::DeleteFace { id: cube.face }).err(),
        Some(VimStatus::HasDependents)
    );
    let updates = {
        engine.evaluate_pending(&mut doc);
        engine.poll_updates(&doc)
    };
    assert!(updates.params_changed.is_empty(), "{updates:?}");
    assert!(updates.is_settled_and_empty(), "{updates:?}");
}

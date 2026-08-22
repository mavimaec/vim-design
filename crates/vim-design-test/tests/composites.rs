//! Test plan (d): composite commands.
//!
//! CreateCylinder is one undo step; a poisoned composite (a constituent
//! failing after earlier constituents already applied) rolls back fully,
//! leaving byte-identical state and nothing on the undo stack.

use vim_design_lib::{Command, Document, EntityKind, VimStatus};
use vim_design_test::{assert_save_load_roundtrip, one, ok, save};

fn create_cylinder(doc: &mut Document) -> Vec<vim_design_lib::EntityId> {
    ok(
        doc,
        Command::CreateCylinder {
            center: [1.0, 2.0, 0.0],
            radius: 0.5,
            height: 3.0,
        },
    )
    .created_ids
}

#[test]
fn create_cylinder_is_one_undo_step() {
    let mut doc = Document::new();
    let created = create_cylinder(&mut doc);

    // 8 constituents: center cp, top cp, circle, edge, wire, face, line,
    // extrusion.
    assert_eq!(created.len(), 8);
    assert_eq!(doc.entity_count(), 8);
    assert_eq!(doc.undo_depth(), 1, "composite commits as one group");
    assert_eq!(doc.undo_label(), Some("CreateCylinder"));

    let kinds: Vec<EntityKind> = created
        .iter()
        .filter_map(|id| doc.entity(*id).map(|r| r.kind()))
        .collect();
    assert_eq!(
        kinds,
        vec![
            EntityKind::ControlPoint,
            EntityKind::ControlPoint,
            EntityKind::Circle,
            EntityKind::Edge,
            EntityKind::Wire,
            EntityKind::Face,
            EntityKind::Line,
            EntityKind::Extrusion,
        ]
    );

    // One undo removes the whole cylinder; one redo restores it.
    doc.undo().expect("undo cylinder");
    assert_eq!(doc.entity_count(), 0);
    doc.redo().expect("redo cylinder");
    assert_eq!(doc.entity_count(), 8);
    for id in &created {
        assert!(doc.entity(*id).is_some(), "same ids after redo");
    }
    doc.debug_validate().expect("invariants after redo");

    assert_save_load_roundtrip(&doc);
}

#[test]
fn update_and_delete_cylinder_round_trip() {
    let mut doc = Document::new();
    let created = create_cylinder(&mut doc);
    let extrusion = *created.last().expect("extrusion is last");

    // Update radius/height/center as one step.
    ok(
        &mut doc,
        Command::UpdateCylinder {
            extrusion,
            center: Some([0.0, 0.0, 1.0]),
            radius: Some(1.25),
            height: Some(5.0),
            coalesce: false,
        },
    );
    assert_eq!(doc.undo_depth(), 2);
    let top_cp = created[1];
    assert_eq!(
        doc.entity(top_cp).map(|r| r.params.clone()),
        Some(vim_design_lib::Params::ControlPoint {
            position: [0.0, 0.0, 6.0] // center.z + height
        })
    );

    // Delete removes the whole subgraph as one step.
    ok(&mut doc, Command::DeleteCylinder { extrusion });
    assert_eq!(doc.entity_count(), 0);
    assert_eq!(doc.undo_depth(), 3);

    // Undo the delete: everything back, same ids, same wiring.
    doc.undo().expect("undo delete cylinder");
    assert_eq!(doc.entity_count(), 8);
    for id in &created {
        assert!(doc.entity(*id).is_some());
    }
    doc.debug_validate().expect("wiring restored intact");

    assert_save_load_roundtrip(&doc);
}

#[test]
fn coalesced_cylinder_drag_is_one_undo_step() {
    let mut doc = Document::new();
    let created = create_cylinder(&mut doc);
    let extrusion = *created.last().expect("extrusion is last");

    for i in 1..=50 {
        ok(
            &mut doc,
            Command::UpdateCylinder {
                extrusion,
                center: None,
                radius: Some(0.5 + f64::from(i) * 0.01),
                height: None,
                coalesce: true,
            },
        );
    }
    assert_eq!(doc.undo_depth(), 2, "create + one coalesced drag");
    doc.undo().expect("undo drag");
    let circle = created[2];
    assert_eq!(
        doc.entity(circle).map(|r| r.params.clone()),
        Some(vim_design_lib::Params::Circle { radius: 0.5 })
    );
}

#[test]
fn poisoned_composite_rolls_back_fully() {
    let mut doc = Document::new();
    let created = create_cylinder(&mut doc);
    let extrusion = *created.last().expect("extrusion is last");
    let face = created[5];

    // Poison: an external extrusion reuses the cylinder's face, so
    // DeleteCylinder succeeds for its first constituent (the cylinder's
    // own extrusion) and then fails on the face (HasDependents).
    let s = one(&mut doc, Command::CreateControlPoint { position: [0.0; 3] });
    let e = one(
        &mut doc,
        Command::CreateControlPoint {
            position: [0.0, 0.0, 9.0],
        },
    );
    let path = one(&mut doc, Command::CreateLine { start: s, end: e });
    let external = one(
        &mut doc,
        Command::CreateExtrusion {
            profile: face,
            path,
        },
    );

    let bytes_before = save(&doc);
    let undo_before = doc.undo_depth();
    doc.take_dirty();

    assert_eq!(
        doc.submit(Command::DeleteCylinder { extrusion }).err(),
        Some(VimStatus::HasDependents)
    );

    // Full rollback: byte-identical, nothing on the undo stack, no dirt.
    assert_eq!(save(&doc), bytes_before, "byte-identical after poisoned composite");
    assert_eq!(doc.undo_depth(), undo_before, "no undo step recorded");
    assert!(doc.dirty_set().is_empty(), "no dirty entries from the failed attempt");
    assert!(doc.entity(extrusion).is_some(), "cylinder extrusion restored");
    assert_eq!(doc.dependents(face), Ok(vec![extrusion, external]));
    doc.debug_validate().expect("invariants after rollback");

    assert_save_load_roundtrip(&doc);
}

#[test]
fn composite_on_non_cylinder_shape_is_rejected_cleanly() {
    let mut doc = Document::new();
    let created = create_cylinder(&mut doc);
    let extrusion = *created.last().expect("extrusion is last");

    // Rewire the path to a spline: the subgraph is no longer a cylinder.
    let cp_ids: Vec<_> = (0..4)
        .map(|i| {
            one(
                &mut doc,
                Command::CreateControlPoint {
                    position: [f64::from(i), 0.0, 0.0],
                },
            )
        })
        .collect();
    let spline = one(
        &mut doc,
        Command::CreateSpline {
            control_points: cp_ids,
            degree: None,
            knots: None,
        },
    );
    ok(
        &mut doc,
        Command::UpdateExtrusion {
            id: extrusion,
            profile: None,
            path: Some(spline),
            coalesce: false,
        },
    );

    let bytes_before = save(&doc);
    assert_eq!(
        doc.submit(Command::UpdateCylinder {
            extrusion,
            center: None,
            radius: Some(2.0),
            height: None,
            coalesce: false,
        })
        .err(),
        Some(VimStatus::InvalidCommand)
    );
    assert_eq!(save(&doc), bytes_before);
}

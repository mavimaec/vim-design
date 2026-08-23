//! Test plan (e), directed part: undo/redo per command class, including
//! delete -> undo restoring the same id and wiring, and redo-stack
//! clearing on new commands.

use vim_design_lib::{Command, Document, EntityId, Params, SlotValue, VimStatus};
use vim_design_test::{assert_save_load_roundtrip, build_chain, one, ok, save};

/// Run `cmd` on `doc`, then undo and redo it, asserting byte-identity
/// with the states captured before and after.
fn assert_undo_redo_round_trip(doc: &mut Document, cmd: Command) {
    let before = save(doc);
    ok(doc, cmd);
    let after = save(doc);

    doc.undo().expect("undo");
    assert_eq!(save(doc), before, "undo restores the pre-command bytes");
    doc.redo().expect("redo");
    assert_eq!(save(doc), after, "redo restores the post-command bytes");
    doc.debug_validate().expect("invariants after undo/redo");
}

#[test]
fn every_command_class_undoes_and_redoes() {
    let mut doc = Document::new();
    let chain = build_chain(&mut doc);

    // Params updates.
    assert_undo_redo_round_trip(
        &mut doc,
        Command::UpdateControlPoint {
            id: chain.cps[0],
            position: [9.0, 9.0, 9.0],
            coalesce: false,
        },
    );
    assert_undo_redo_round_trip(
        &mut doc,
        Command::UpdateMaterial {
            id: chain.material,
            name: Some("brass".to_owned()),
            color: None,
            roughness: Some(0.1),
            coalesce: false,
        },
    );
    assert_undo_redo_round_trip(
        &mut doc,
        Command::UpdateSpline {
            id: chain.spline,
            control_points: None,
            degree: Some(Some(2)),
            knots: Some(Some(vec![0.0, 0.0, 0.5, 1.0, 1.0])),
            coalesce: false,
        },
    );
    assert_undo_redo_round_trip(
        &mut doc,
        Command::UpdateElement {
            id: chain.element,
            name: Some("beam".to_owned()),
            members: None,
            coalesce: false,
        },
    );
    assert_undo_redo_round_trip(
        &mut doc,
        Command::UpdateInstance {
            id: chain.instances[0],
            transform: Some(vim_design_test::translation(0.0, 7.0, 0.0)),
            element: None,
            coalesce: false,
        },
    );

    // Rewires (single and multi slots).
    assert_undo_redo_round_trip(
        &mut doc,
        Command::UpdateExtrusion {
            id: chain.extrusion,
            profile: None,
            path: Some(chain.spline),
            coalesce: false,
        },
    );
    assert_undo_redo_round_trip(
        &mut doc,
        Command::UpdateFaceMaterial {
            face: chain.face,
            material: None, // clear the optional slot
        },
    );
    assert_undo_redo_round_trip(
        &mut doc,
        Command::UpdateLine {
            id: chain.line,
            start: Some(chain.cps[0]),
            end: None,
            coalesce: false,
        },
    );
    // Wire rewire (multi slot) round-trips like every other class.
    let edge2 = one(&mut doc, Command::CreateEdge { curve: chain.line });
    assert_undo_redo_round_trip(
        &mut doc,
        Command::UpdateWire {
            id: chain.wire,
            edges: vec![chain.edge, edge2],
            coalesce: false,
        },
    );
    // Multi-slot rewire: reverse the spline's control point order.
    let mut reversed = chain.cps.to_vec();
    reversed.reverse();
    assert_undo_redo_round_trip(
        &mut doc,
        Command::UpdateSpline {
            id: chain.spline,
            control_points: Some(reversed),
            degree: None,
            knots: None,
            coalesce: false,
        },
    );

    // Creates.
    assert_undo_redo_round_trip(&mut doc, Command::CreatePlane {
        origin: [0.0, 0.0, 3.0],
        normal: [0.0, 0.0, 1.0],
    });
    assert_undo_redo_round_trip(&mut doc, Command::CreateSectionBox {
        min: [-5.0, -5.0, 0.0],
        max: [5.0, 5.0, 10.0],
    });
    assert_undo_redo_round_trip(&mut doc, Command::CreateSolid {
        faces: vec![chain.face],
    });

    assert_save_load_roundtrip(&doc);
}

#[test]
fn delete_undo_restores_same_id_and_wiring() {
    let mut doc = Document::new();
    let chain = build_chain(&mut doc);

    // Deleting an instance (a leaf) succeeds.
    let victim = chain.instances[1];
    let record_before = doc.entity(victim).expect("instance exists").clone();
    ok(&mut doc, Command::DeleteInstance { id: victim });
    assert!(doc.entity(victim).is_none());

    doc.undo().expect("undo delete");
    let restored = doc.entity(victim).expect("restored under the SAME id");
    assert_eq!(restored, &record_before, "identical params and wiring");
    assert_eq!(
        restored.inputs.first(),
        Some(&SlotValue::One(Some(chain.element))),
        "rewired to the same element"
    );
    assert_eq!(
        doc.dependents(chain.element),
        Ok(chain.instances.to_vec()),
        "reverse index restored"
    );

    // Delete a whole leaf-first chain, then undo in reverse order.
    ok(&mut doc, Command::DeleteInstance { id: chain.instances[1] });
    ok(&mut doc, Command::DeleteInstance { id: chain.instances[0] });
    // sweep_orphans: false — this test exercises MANUAL leaf-first
    // deletion; the sweep variant is covered in orphan_sweep.rs.
    ok(
        &mut doc,
        Command::DeleteElement {
            id: chain.element,
            sweep_orphans: false,
        },
    );
    ok(&mut doc, Command::DeleteExtrusion { id: chain.extrusion });
    assert!(doc.entity(chain.extrusion).is_none());
    for _ in 0..4 {
        doc.undo().expect("unwind deletes");
    }
    assert!(doc.entity(chain.extrusion).is_some());
    assert!(doc.entity(chain.element).is_some());
    doc.debug_validate().expect("full wiring restored");

    assert_save_load_roundtrip(&doc);
}

#[test]
fn new_command_after_undo_clears_redo_stack() {
    let mut doc = Document::new();
    let cp = one(&mut doc, Command::CreateControlPoint { position: [0.0; 3] });
    ok(
        &mut doc,
        Command::UpdateControlPoint {
            id: cp,
            position: [1.0, 0.0, 0.0],
            coalesce: false,
        },
    );

    doc.undo().expect("undo update");
    assert!(doc.can_redo());

    ok(
        &mut doc,
        Command::UpdateControlPoint {
            id: cp,
            position: [2.0, 0.0, 0.0],
            coalesce: false,
        },
    );
    assert!(!doc.can_redo(), "new command cleared the redo stack");
    assert_eq!(doc.redo().err(), Some(VimStatus::NothingToRedo));
    assert_eq!(
        doc.entity(cp).map(|r| r.params.clone()),
        Some(Params::ControlPoint {
            position: [2.0, 0.0, 0.0]
        })
    );
}

#[test]
fn undo_redo_on_empty_stacks_reject() {
    let mut doc = Document::new();
    assert!(!doc.can_undo());
    assert!(!doc.can_redo());
    assert_eq!(doc.undo().err(), Some(VimStatus::NothingToUndo));
    assert_eq!(doc.redo().err(), Some(VimStatus::NothingToRedo));
}

#[test]
fn undone_creates_never_recycle_ids() {
    let mut doc = Document::new();
    let first = one(&mut doc, Command::CreateControlPoint { position: [0.0; 3] });
    doc.undo().expect("undo create");
    let second = one(&mut doc, Command::CreateControlPoint { position: [0.0; 3] });
    assert!(
        second > first,
        "the allocator is not rolled back by undo: {first:?} vs {second:?}"
    );
    assert_ne!(first, EntityId::INVALID);
}

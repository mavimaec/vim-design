//! Structural coverage for the `Revolve` entity (promoted 2026-08-22 —
//! docs/ARCHITECTURE.md §3.1, §15.13), matching the per-kind coverage
//! style of the other geometry commands: create with defaults, update
//! (params + rewires, coalescing), delete, rejections, undo/redo, and
//! save/load round-trips.

use vim_design_lib::{Command, Document, EntityKind, Params, SlotValue, VimStatus};
use vim_design_test::{assert_save_load_roundtrip, build_cone, one, ok, save};

#[test]
fn create_revolve_defaults_to_full_turn() {
    let mut doc = Document::new();
    let cone = build_cone(&mut doc, [0.0, 0.0, 0.0], 1.5, 2.0);

    let record = doc.entity(cone.revolve).expect("revolve exists");
    assert_eq!(record.kind(), EntityKind::Revolve);
    assert_eq!(
        record.params,
        Params::Revolve {
            angle_radians: std::f64::consts::TAU,
            face_materials: vec![]
        }
    );
    assert_eq!(
        record.inputs,
        vec![
            SlotValue::One(Some(cone.face)),
            SlotValue::One(Some(cone.axis)),
            SlotValue::Many(vec![]), // face_materials (sub-face paints)
        ]
    );
    // The revolve is a real dependent of both inputs.
    assert_eq!(doc.dependents(cone.face), Ok(vec![cone.revolve]));
    assert_eq!(doc.dependents(cone.axis), Ok(vec![cone.revolve]));

    assert_save_load_roundtrip(&doc);
}

#[test]
fn update_revolve_params_and_rewires() {
    let mut doc = Document::new();
    let cone = build_cone(&mut doc, [0.0, 0.0, 0.0], 1.5, 2.0);

    // Angle-only update.
    ok(
        &mut doc,
        Command::UpdateRevolve {
            id: cone.revolve,
            profile: None,
            axis: None,
            angle_radians: Some(std::f64::consts::PI),
            coalesce: false,
        },
    );
    assert_eq!(
        doc.entity(cone.revolve).map(|r| r.params.clone()),
        Some(Params::Revolve {
            angle_radians: std::f64::consts::PI,
            face_materials: vec![]
        })
    );

    // Rewire the axis to another line.
    let other_axis = one(
        &mut doc,
        Command::CreateLine {
            start: cone.base_cp,
            end: cone.apex_cp,
        },
    );
    ok(
        &mut doc,
        Command::UpdateRevolve {
            id: cone.revolve,
            profile: None,
            axis: Some(other_axis),
            angle_radians: None,
            coalesce: false,
        },
    );
    assert_eq!(doc.dependents(other_axis), Ok(vec![cone.revolve]));
    assert_eq!(
        doc.dependents(cone.axis).map(|d| d.contains(&cone.revolve)),
        Ok(false)
    );

    // Coalesced angle drag: one undo step for the whole drag.
    let depth_before = doc.undo_depth();
    for i in 1..=20 {
        ok(
            &mut doc,
            Command::UpdateRevolve {
                id: cone.revolve,
                profile: None,
                axis: None,
                angle_radians: Some(f64::from(i) * 0.1),
                coalesce: true,
            },
        );
    }
    assert_eq!(doc.undo_depth(), depth_before + 1, "drag coalesced");
    doc.undo().expect("undo drag");
    assert_eq!(
        doc.entity(cone.revolve).map(|r| r.params.clone()),
        Some(Params::Revolve {
            angle_radians: std::f64::consts::PI,
            face_materials: vec![]
        })
    );

    assert_save_load_roundtrip(&doc);
}

#[test]
fn revolve_rejections() {
    let mut doc = Document::new();
    let cone = build_cone(&mut doc, [0.0, 0.0, 0.0], 1.5, 2.0);
    let bytes_before = save(&doc);

    // Axis slot accepts Line only (not Spline, unlike the extrusion path).
    let spline = {
        let cps = vec![cone.base_cp, cone.rim_cp, cone.apex_cp];
        one(
            &mut doc,
            Command::CreateSpline {
                control_points: cps,
                degree: None,
                knots: None,
            },
        )
    };
    assert_eq!(
        doc.submit(Command::UpdateRevolve {
            id: cone.revolve,
            profile: None,
            axis: Some(spline),
            angle_radians: None,
            coalesce: false,
        })
        .err(),
        Some(VimStatus::SlotKindMismatch)
    );
    assert_eq!(
        doc.submit(Command::CreateRevolve {
            profile: cone.wire, // not a Face
            axis: cone.axis,
            angle_radians: None,
        })
        .err(),
        Some(VimStatus::SlotKindMismatch)
    );
    assert_eq!(
        doc.submit(Command::UpdateRevolve {
            id: cone.face, // wrong kind behind a valid id
            profile: None,
            axis: None,
            angle_radians: Some(1.0),
            coalesce: false,
        })
        .err(),
        Some(VimStatus::WrongEntityKind)
    );

    // The profile face cannot be deleted while the revolve depends on it.
    assert_eq!(
        doc.submit(Command::DeleteFace { id: cone.face }).err(),
        Some(VimStatus::HasDependents)
    );

    // Cleanup: drop the probe spline; document is back to the baseline.
    ok(&mut doc, Command::DeleteSpline { id: spline });
    assert_eq!(save(&doc), bytes_before);
}

#[test]
fn delete_revolve_and_undo_redo() {
    let mut doc = Document::new();
    let cone = build_cone(&mut doc, [0.0, 0.0, 0.0], 1.5, 2.0);

    // Revolve is a valid element member (docs/ARCHITECTURE.md §3.1).
    let element = one(
        &mut doc,
        Command::CreateElement {
            name: "cone".to_owned(),
            members: vec![cone.revolve],
        },
    );
    assert_eq!(
        doc.submit(Command::DeleteRevolve { id: cone.revolve }).err(),
        Some(VimStatus::HasDependents)
    );
    ok(&mut doc, Command::DeleteElement { id: element });

    let bytes_with_revolve = save(&doc);
    ok(&mut doc, Command::DeleteRevolve { id: cone.revolve });
    assert!(doc.entity(cone.revolve).is_none());

    doc.undo().expect("undo delete revolve");
    assert_eq!(save(&doc), bytes_with_revolve, "same id and wiring restored");
    doc.redo().expect("redo delete revolve");
    assert!(doc.entity(cone.revolve).is_none());
    doc.undo().expect("undo again");

    assert_save_load_roundtrip(&doc);
}

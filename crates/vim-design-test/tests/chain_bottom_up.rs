//! Test plan (a): bottom-up construction chain.
//!
//! 4 control points -> spline -> edge -> face; 2 control points -> line;
//! extrusion(face, line); material + UpdateFaceMaterial; element +
//! instances. Asserts monotonic ids, slot kind-checks, and that the
//! reverse index matches a from-scratch rebuild after every command.

use vim_design_lib::{Command, Document, EntityId, EntityKind, SlotValue, VimStatus};
use vim_design_test::{IDENTITY_XFORM, assert_save_load_roundtrip, ok, translation};

/// Submit + full invariant check (rebuilds the reverse index from scratch
/// and compares — Document::debug_validate).
fn checked(doc: &mut Document, cmd: Command) -> Vec<EntityId> {
    let out = ok(doc, cmd);
    doc.debug_validate()
        .expect("invariants must hold after every command");
    out.created_ids
}

fn checked_one(doc: &mut Document, cmd: Command) -> EntityId {
    let ids = checked(doc, cmd);
    assert_eq!(ids.len(), 1);
    ids[0]
}

#[test]
fn bottom_up_chain_builds_with_monotonic_ids_and_valid_index() {
    let mut doc = Document::new();
    let mut all_ids: Vec<EntityId> = Vec::new();

    // 4 control points -> spline.
    let cps: Vec<EntityId> = (0..4)
        .map(|i| {
            checked_one(
                &mut doc,
                Command::CreateControlPoint {
                    position: [f64::from(i), 0.0, 0.0],
                },
            )
        })
        .collect();
    all_ids.extend(&cps);

    let spline = checked_one(
        &mut doc,
        Command::CreateSpline {
            control_points: cps.clone(),
            degree: Some(3),
            knots: None,
        },
    );
    all_ids.push(spline);

    // Spline -> edge -> wire -> face.
    let edge = checked_one(&mut doc, Command::CreateEdge { curve: spline });
    let wire = checked_one(&mut doc, Command::CreateWire { edges: vec![edge] });
    let face = checked_one(
        &mut doc,
        Command::CreateFace {
            outer: wire,
            holes: vec![],
            plane: None, // no explicit surface: structurally valid
        },
    );
    all_ids.extend([edge, wire, face]);

    // 2 control points -> line.
    let start = checked_one(&mut doc, Command::CreateControlPoint { position: [0.0; 3] });
    let end = checked_one(
        &mut doc,
        Command::CreateControlPoint {
            position: [0.0, 0.0, 3.0],
        },
    );
    let line = checked_one(&mut doc, Command::CreateLine { start, end });
    all_ids.extend([start, end, line]);

    // Extrusion(face, line).
    let extrusion = checked_one(&mut doc, Command::CreateExtrusion { profile: face, path: line });
    all_ids.push(extrusion);

    // Material + UpdateFaceMaterial.
    let material = checked_one(
        &mut doc,
        Command::CreateMaterial {
            name: "steel".to_owned(),
            color: [0.6, 0.6, 0.7],
            roughness: 0.3,
        },
    );
    all_ids.push(material);
    checked(
        &mut doc,
        Command::UpdateFaceMaterial {
            face,
            material: Some(material),
        },
    );
    let face_record = doc.entity(face).expect("face exists");
    assert_eq!(
        face_record.inputs.get(2),
        Some(&SlotValue::One(Some(material))),
        "material wired into the face's material slot"
    );

    // Element + instances.
    let element = checked_one(
        &mut doc,
        Command::CreateElement {
            name: "column".to_owned(),
            members: vec![extrusion],
        },
    );
    let inst_a = checked_one(
        &mut doc,
        Command::CreateInstance {
            element,
            transform: IDENTITY_XFORM,
        },
    );
    let inst_b = checked_one(
        &mut doc,
        Command::CreateInstance {
            element,
            transform: translation(5.0, 0.0, 0.0),
        },
    );
    all_ids.extend([element, inst_a, inst_b]);

    // Monotonic, never-reused ids in creation order.
    for pair in all_ids.windows(2) {
        assert!(pair[0] < pair[1], "ids must be strictly increasing");
    }
    assert!(all_ids.iter().all(|id| *id != EntityId::INVALID));

    // Kinds landed as declared.
    let kind = |id: EntityId| doc.entity(id).map(|r| r.kind());
    assert_eq!(kind(spline), Some(EntityKind::Spline));
    assert_eq!(kind(edge), Some(EntityKind::Edge));
    assert_eq!(kind(wire), Some(EntityKind::Wire));
    assert_eq!(kind(face), Some(EntityKind::Face));
    assert_eq!(kind(extrusion), Some(EntityKind::Extrusion));
    assert_eq!(kind(element), Some(EntityKind::Element));
    assert_eq!(kind(inst_b), Some(EntityKind::Instance));

    // Reverse index content spot-checks (beyond debug_validate).
    assert_eq!(doc.dependents(spline), Ok(vec![edge]));
    assert_eq!(doc.dependents(edge), Ok(vec![wire]));
    assert_eq!(doc.dependents(wire), Ok(vec![face]));
    assert_eq!(doc.dependents(face), Ok(vec![extrusion]));
    assert_eq!(doc.dependents(material), Ok(vec![face]));
    assert_eq!(doc.dependents(element), Ok(vec![inst_a, inst_b]));

    // Slot kind-checks: every mis-typed wiring is rejected.
    let cp0 = cps[0];
    assert_eq!(
        doc.submit(Command::CreateEdge { curve: cp0 }).err(),
        Some(VimStatus::SlotKindMismatch),
        "an edge's curve slot rejects a control point"
    );
    assert_eq!(
        doc.submit(Command::CreateWire { edges: vec![spline] }).err(),
        Some(VimStatus::SlotKindMismatch),
        "a wire's edges slot rejects a spline"
    );
    assert_eq!(
        doc.submit(Command::CreateFace { outer: edge, holes: vec![], plane: None })
            .err(),
        Some(VimStatus::SlotKindMismatch),
        "a face's outer slot rejects a bare edge (wires only)"
    );
    assert_eq!(
        doc.submit(Command::CreateFace {
            outer: wire,
            holes: vec![],
            plane: Some(edge), // not a Plane
        })
        .err(),
        Some(VimStatus::SlotKindMismatch),
        "a face's plane slot rejects a non-plane"
    );
    assert_eq!(
        doc.submit(Command::CreateExtrusion { profile: face, path: face }).err(),
        Some(VimStatus::SlotKindMismatch),
        "an extrusion's path slot rejects a face"
    );
    assert_eq!(
        doc.submit(Command::CreateInstance { element: extrusion, transform: IDENTITY_XFORM })
            .err(),
        Some(VimStatus::SlotKindMismatch),
        "an instance's element slot rejects an extrusion"
    );
    doc.debug_validate().expect("rejections leave invariants intact");

    // Test plan (g): scenario end -> save/load/save byte-identity.
    assert_save_load_roundtrip(&doc);
}

#[test]
fn face_supports_inner_hole_wires() {
    let mut doc = Document::new();

    // Two independent loops: an outer wire and a hole wire.
    let mut wires = Vec::new();
    for offset in [0.0, 10.0] {
        let cps: Vec<_> = (0..4)
            .map(|i| {
                checked_one(
                    &mut doc,
                    Command::CreateControlPoint {
                        position: [offset + f64::from(i), 0.0, 0.0],
                    },
                )
            })
            .collect();
        let spline = checked_one(
            &mut doc,
            Command::CreateSpline {
                control_points: cps,
                degree: None,
                knots: None,
            },
        );
        let edge = checked_one(&mut doc, Command::CreateEdge { curve: spline });
        wires.push(checked_one(&mut doc, Command::CreateWire { edges: vec![edge] }));
    }
    let (outer, hole) = (wires[0], wires[1]);

    let face = checked_one(
        &mut doc,
        Command::CreateFace {
            outer,
            holes: vec![hole],
            plane: None,
        },
    );
    let record = doc.entity(face).expect("face exists");
    assert_eq!(record.inputs.first(), Some(&SlotValue::One(Some(outer))));
    assert_eq!(record.inputs.get(1), Some(&SlotValue::Many(vec![hole])));
    assert_eq!(doc.dependents(hole), Ok(vec![face]));

    // The hole wire cannot be deleted while the face references it.
    assert_eq!(
        doc.submit(Command::DeleteWire { id: hole }).err(),
        Some(VimStatus::HasDependents)
    );
    // Unwire the hole (multi-slot rewire to empty is fine: holes are
    // optional), then deletion succeeds and undo restores the wiring.
    checked(
        &mut doc,
        Command::UpdateFace {
            id: face,
            outer: None,
            holes: Some(vec![]),
            plane: None,
            coalesce: false,
        },
    );
    checked(&mut doc, Command::DeleteWire { id: hole });
    doc.undo().expect("undo delete hole wire");
    doc.undo().expect("undo unwire hole");
    assert_eq!(
        doc.entity(face).and_then(|r| r.inputs.get(1).cloned()),
        Some(SlotValue::Many(vec![hole]))
    );

    assert_save_load_roundtrip(&doc);
}

#[test]
fn face_supports_optional_surface_plane() {
    let mut doc = Document::new();

    // A loop to bound the face.
    let cps: Vec<EntityId> = (0..4)
        .map(|i| {
            checked_one(
                &mut doc,
                Command::CreateControlPoint {
                    position: [f64::from(i), 0.0, 3.0],
                },
            )
        })
        .collect();
    let spline = checked_one(
        &mut doc,
        Command::CreateSpline {
            control_points: cps,
            degree: None,
            knots: None,
        },
    );
    let edge = checked_one(&mut doc, Command::CreateEdge { curve: spline });
    let wire = checked_one(&mut doc, Command::CreateWire { edges: vec![edge] });
    let plane = checked_one(
        &mut doc,
        Command::CreatePlane {
            origin: [0.0, 0.0, 3.0],
            normal: [0.0, 0.0, 1.0],
        },
    );

    // Face with an explicit surface plane wired at creation.
    let face = checked_one(
        &mut doc,
        Command::CreateFace {
            outer: wire,
            holes: vec![],
            plane: Some(plane),
        },
    );
    let record = doc.entity(face).expect("face exists");
    assert_eq!(record.inputs.get(3), Some(&SlotValue::One(Some(plane))));

    // The plane is a real graph dependency: delete-rejection + query.
    assert_eq!(doc.dependents(plane), Ok(vec![face]));
    assert_eq!(
        doc.submit(Command::DeletePlane { id: plane }).err(),
        Some(VimStatus::HasDependents)
    );

    // Rewiring the plane off and on inverts cleanly under undo/redo.
    let bytes_with_plane = doc.save().expect("save with plane");
    checked(
        &mut doc,
        Command::UpdateFace {
            id: face,
            outer: None,
            holes: None,
            plane: Some(None), // clear the surface (evaluator will infer)
            coalesce: false,
        },
    );
    assert_eq!(
        doc.entity(face).and_then(|r| r.inputs.get(3).cloned()),
        Some(SlotValue::One(None)),
        "face without an explicit plane stays structurally valid"
    );
    assert_eq!(doc.dependents(plane), Ok(vec![]));
    let bytes_without_plane = doc.save().expect("save without plane");

    doc.undo().expect("undo clear plane");
    assert_eq!(doc.save().expect("save"), bytes_with_plane);
    assert_eq!(doc.dependents(plane), Ok(vec![face]));
    doc.redo().expect("redo clear plane");
    assert_eq!(doc.save().expect("save"), bytes_without_plane);

    // Now the unreferenced plane can be deleted; undo restores the wiring
    // state exactly.
    checked(&mut doc, Command::DeletePlane { id: plane });
    doc.undo().expect("undo delete plane");
    assert_eq!(doc.save().expect("save"), bytes_without_plane);

    assert_save_load_roundtrip(&doc);
}

#[test]
fn required_slots_reject_empty_wiring() {
    let mut doc = Document::new();
    assert_eq!(
        doc.submit(Command::CreateWire { edges: vec![] }).err(),
        Some(VimStatus::MissingRequiredSlot)
    );
    assert_eq!(
        doc.submit(Command::CreateSpline {
            control_points: vec![],
            degree: None,
            knots: None,
        })
        .err(),
        Some(VimStatus::MissingRequiredSlot)
    );
    assert_save_load_roundtrip(&doc);
}

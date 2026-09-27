//! Sketch entities: layered evaluation golden volumes, provenance of the
//! generated faces, transform-only level edits, undo/redo, validation
//! tiers, and deletion.

use vim_design_lib::eval::{Engine, EvalErrorKind, SubRefResolution};
use vim_design_lib::sketch::{self, Sketch, SketchDirection, SketchFaceKind, ops};
use vim_design_lib::{
    Command, Document, EntityId, EntityKind, Mesh, ProvenancePath, SubRef, VimStatus,
};
use vim_design_test::{assert_bbox_near, mesh_bbox, mesh_volume, one, ok, save};

type P2 = [f64; 2];

fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> Vec<P2> {
    vec![[x0, y0], [x1, y0], [x1, y1], [x0, y1]]
}

fn solid(thickness: f64) -> SketchFaceKind {
    SketchFaceKind::Solid { thickness }
}

fn void(depth: Option<f64>) -> SketchFaceKind {
    SketchFaceKind::Void { depth }
}

fn build(faces: &[(Vec<P2>, SketchFaceKind)]) -> Sketch {
    let mut s = Sketch::default();
    for (outline, kind) in faces {
        s = ops::add_face(&s, outline, *kind).expect("add_face");
    }
    s
}

fn level(doc: &mut Document, elevation_m: f64) -> EntityId {
    one(
        doc,
        Command::CreateLevel {
            name: "Ground".to_owned(),
            elevation_m,
            is_building_story: true,
            color: [0.2, 0.5, 0.9, 0.3],
            extent_m: 10.0,
        },
    )
}

/// A sketch wrapped in an element associated with its level.
struct Placed {
    level: EntityId,
    sketch: EntityId,
    element: EntityId,
}

fn place(doc: &mut Document, s: Sketch, direction: SketchDirection) -> Placed {
    let level = level(doc, 0.0);
    place_on(doc, level, s, direction)
}

fn place_on(doc: &mut Document, level: EntityId, s: Sketch, direction: SketchDirection) -> Placed {
    let sketch = one(
        doc,
        Command::CreateSketch {
            plane: level,
            sketch: s,
            direction,
        },
    );
    let element = one(
        doc,
        Command::CreateElement {
            name: "Floor plate 1".to_owned(),
            members: vec![sketch],
            level,
        },
    );
    Placed {
        level,
        sketch,
        element,
    }
}

/// Evaluate and return the element's mesh (asserting a clean poll).
fn element_mesh(doc: &mut Document, engine: &mut Engine, element: EntityId) -> Mesh {
    engine.evaluate_pending(doc);
    let updates = engine.poll_updates(doc);
    assert!(updates.errors.is_empty(), "{:?}", updates.errors);
    engine.mesh(element).expect("element meshed").clone()
}

fn volume_of(faces: &[(Vec<P2>, SketchFaceKind)]) -> f64 {
    let mut doc = Document::new();
    let mut engine = Engine::new();
    let placed = place(&mut doc, build(faces), SketchDirection::Below);
    mesh_volume(&element_mesh(&mut doc, &mut engine, placed.element))
}

fn assert_near(actual: f64, expected: f64) {
    assert!(
        (actual - expected).abs() < 1e-6,
        "expected {expected}, got {actual}"
    );
}

// ---------------------------------------------------------------------
// Layered evaluation golden volumes.
// ---------------------------------------------------------------------

#[test]
fn single_solid_face_hangs_below_its_level() {
    let mut doc = Document::new();
    let mut engine = Engine::new();
    let placed = place(
        &mut doc,
        build(&[(rect(0.0, 0.0, 4.0, 3.0), solid(0.3))]),
        SketchDirection::Below,
    );
    let mesh = element_mesh(&mut doc, &mut engine, placed.element);
    assert_near(mesh_volume(&mesh), 4.0 * 3.0 * 0.3);
    assert_bbox_near(&mesh, [0.0, 0.0, -0.3], [4.0, 3.0, 0.0], 1e-6);
    vim_design_test::assert_watertight(&mesh);
}

#[test]
fn above_direction_grows_up_from_the_plane() {
    let mut doc = Document::new();
    let mut engine = Engine::new();
    let placed = place(
        &mut doc,
        build(&[(rect(0.0, 0.0, 1.0, 1.0), solid(2.5))]),
        SketchDirection::Above,
    );
    let mesh = element_mesh(&mut doc, &mut engine, placed.element);
    assert_bbox_near(&mesh, [0.0, 0.0, 0.0], [1.0, 1.0, 2.5], 1e-6);
    assert_near(mesh_volume(&mesh), 2.5);
}

#[test]
fn two_disjoint_faces_make_one_element_mesh() {
    let faces = [
        (rect(0.0, 0.0, 2.0, 2.0), solid(0.2)),
        (rect(5.0, 0.0, 6.0, 3.0), solid(0.4)),
    ];
    let mut doc = Document::new();
    let mut engine = Engine::new();
    let placed = place(&mut doc, build(&faces), SketchDirection::Below);
    engine.evaluate_pending(&mut doc);
    let updates = engine.poll_updates(&doc);
    assert_eq!(updates.meshes.len(), 1, "one element, one mesh");
    assert_eq!(updates.meshes.first().map(|m| m.id), Some(placed.element));
    assert_near(volume_of(&faces), 4.0 * 0.2 + 3.0 * 0.4);
}

#[test]
fn overlapping_solids_union_with_their_own_thickness() {
    // A = [0,2]^2 at 0.2 m, B = [1,3]^2 at 0.5 m; union area 7.
    let v = volume_of(&[
        (rect(0.0, 0.0, 2.0, 2.0), solid(0.2)),
        (rect(1.0, 1.0, 3.0, 3.0), solid(0.5)),
    ]);
    assert_near(v, 7.0 * 0.2 + 4.0 * 0.3);
}

#[test]
fn through_void_is_a_hole() {
    let v = volume_of(&[
        (rect(0.0, 0.0, 4.0, 4.0), solid(0.3)),
        (rect(1.0, 1.0, 2.0, 2.0), void(None)),
    ]);
    assert_near(v, 15.0 * 0.3);
}

#[test]
fn pocket_void_removes_only_its_depth() {
    let v = volume_of(&[
        (rect(0.0, 0.0, 4.0, 4.0), solid(0.3)),
        (rect(1.0, 1.0, 2.0, 2.0), void(Some(0.1))),
    ]);
    assert_near(v, 16.0 * 0.3 - 1.0 * 0.1);
}

#[test]
fn void_partly_outside_shapes_the_boundary() {
    let mut doc = Document::new();
    let mut engine = Engine::new();
    let placed = place(
        &mut doc,
        build(&[
            (rect(0.0, 0.0, 4.0, 4.0), solid(0.3)),
            (rect(3.0, 1.0, 5.0, 2.0), void(None)),
        ]),
        SketchDirection::Below,
    );
    let mesh = element_mesh(&mut doc, &mut engine, placed.element);
    assert_near(mesh_volume(&mesh), 15.0 * 0.3);
    assert_bbox_near(&mesh, [0.0, 0.0, -0.3], [4.0, 4.0, 0.0], 1e-6);
}

#[test]
fn void_fully_outside_is_a_no_op() {
    let plain = [(rect(0.0, 0.0, 4.0, 4.0), solid(0.3))];
    let with_far_void = [
        (rect(0.0, 0.0, 4.0, 4.0), solid(0.3)),
        (rect(6.0, 6.0, 7.0, 7.0), void(Some(0.2))),
    ];
    let mut doc = Document::new();
    let mut engine = Engine::new();
    let a = place(&mut doc, build(&plain), SketchDirection::Below);
    let mesh_a = element_mesh(&mut doc, &mut engine, a.element);
    let mut doc = Document::new();
    let mut engine = Engine::new();
    let b = place(&mut doc, build(&with_far_void), SketchDirection::Below);
    let mesh_b = element_mesh(&mut doc, &mut engine, b.element);
    assert_eq!(mesh_a.positions, mesh_b.positions);
    assert_eq!(mesh_a.indices, mesh_b.indices);
}

#[test]
fn nested_voids_take_the_deepest_removal() {
    // Plate 6x6 at 0.4; pocket A = [1,5]^2 at 0.1; deeper pocket B =
    // [2,4]^2 at 0.3 inside A.
    let v = volume_of(&[
        (rect(0.0, 0.0, 6.0, 6.0), solid(0.4)),
        (rect(1.0, 1.0, 5.0, 5.0), void(Some(0.1))),
        (rect(2.0, 2.0, 4.0, 4.0), void(Some(0.3))),
    ]);
    assert_near(v, 36.0 * 0.4 - (12.0 * 0.1 + 4.0 * 0.3));
    // A through void nested inside a pocket.
    let v = volume_of(&[
        (rect(0.0, 0.0, 6.0, 6.0), solid(0.4)),
        (rect(1.0, 1.0, 5.0, 5.0), void(Some(0.1))),
        (rect(2.0, 2.0, 4.0, 4.0), void(None)),
    ]);
    assert_near(v, 36.0 * 0.4 - (12.0 * 0.1 + 4.0 * 0.4));
}

#[test]
fn voids_deeper_than_the_material_cut_through() {
    let v = volume_of(&[
        (rect(0.0, 0.0, 4.0, 4.0), solid(0.3)),
        (rect(1.0, 1.0, 2.0, 2.0), void(Some(5.0))),
    ]);
    assert_near(v, 15.0 * 0.3);
}

// ---------------------------------------------------------------------
// Provenance.
// ---------------------------------------------------------------------

#[test]
fn generated_faces_are_named_from_sketch_ids() {
    let mut doc = Document::new();
    let mut engine = Engine::new();
    let s = build(&[
        (rect(0.0, 0.0, 4.0, 4.0), solid(0.3)),
        (rect(1.0, 1.0, 2.0, 2.0), void(Some(0.1))),
    ]);
    let (a, b) = {
        let face = s.face(0).expect("face 0");
        (face.points[0], face.points[1])
    };
    let placed = place(&mut doc, s.clone(), SketchDirection::Below);
    engine.evaluate_pending(&mut doc);
    let sub = |path: ProvenancePath| SubRef {
        owner: placed.sketch,
        path,
    };
    // A plate side: face 0's edge a-b (points sorted).
    let side = ProvenancePath::SketchSide {
        face: 0,
        a: a.min(b),
        b: a.max(b),
    };
    assert!(matches!(
        engine.resolve_subref(&sub(side)),
        Ok(SubRefResolution::Faces(n)) if n >= 1
    ));
    // The top face at depth 0, facing the level.
    assert!(matches!(
        engine.resolve_subref(&sub(ProvenancePath::SketchCap {
            depth_um: 0,
            toward_plane: true
        })),
        Ok(SubRefResolution::Faces(n)) if n >= 1
    ));
    // The pocket walls are named by the void face (id 1).
    let void_face = s.face(1).expect("face 1");
    let (p, q) = (void_face.points[0], void_face.points[1]);
    assert!(matches!(
        engine.resolve_subref(&sub(ProvenancePath::SketchSide {
            face: 1,
            a: p.min(q),
            b: p.max(q)
        })),
        Ok(SubRefResolution::Faces(1))
    ));
    // The bottom at 0.3 m, facing away.
    assert!(matches!(
        engine.resolve_subref(&sub(ProvenancePath::SketchCap {
            depth_um: 300_000,
            toward_plane: false
        })),
        Ok(SubRefResolution::Faces(_))
    ));
    // Names are stable: a point moved elsewhere keeps the side's name.
    let moved = ops::move_points(&s, &[a], [-0.5, 0.0]).expect("move");
    ok(
        &mut doc,
        Command::UpdateSketch {
            id: placed.sketch,
            sketch: moved,
            coalesce: false,
        },
    );
    engine.evaluate_pending(&mut doc);
    assert!(matches!(
        engine.resolve_subref(&sub(ProvenancePath::SketchSide {
            face: 0,
            a: a.min(b),
            b: a.max(b)
        })),
        Ok(SubRefResolution::Faces(_))
    ));
    // A name that does not exist fails typed.
    assert_eq!(
        engine
            .resolve_subref(&sub(ProvenancePath::SketchSide { face: 9, a: 0, b: 1 }))
            .err()
            .map(|d| d.kind),
        Some(EvalErrorKind::UnresolvedSubRef)
    );
}

// ---------------------------------------------------------------------
// Translation factoring.
// ---------------------------------------------------------------------

#[test]
fn level_elevation_edit_is_transform_only_for_a_sketch_element() {
    let mut doc = Document::new();
    let mut engine = Engine::new();
    engine.set_translation_factoring(true);
    let ground = level(&mut doc, 0.5);
    let placed = place_on(
        &mut doc,
        ground,
        build(&[
            (rect(0.0, 0.0, 4.0, 3.0), solid(0.3)),
            (rect(1.0, 1.0, 2.0, 2.0), void(None)),
        ]),
        SketchDirection::Below,
    );
    engine.evaluate_pending(&mut doc);
    let updates = engine.poll_updates(&doc);
    assert!(updates.errors.is_empty(), "{:?}", updates.errors);
    let first = updates.meshes.first().expect("element mesh");
    assert_eq!(first.id, placed.element);
    assert_eq!(
        [first.base_transform[3], first.base_transform[7], first.base_transform[11]],
        [0.0, 0.0, 0.5]
    );
    let (min, max) = mesh_bbox(&first.mesh);
    assert!((min[2] + 0.3).abs() < 1e-6 && max[2].abs() < 1e-6, "level-local mesh (f32 positions)");
    let tess = engine.tessellation_count(placed.element);
    let sketch_evals = engine.eval_count(placed.sketch);

    ok(
        &mut doc,
        Command::UpdateLevel {
            id: ground,
            name: None,
            elevation_m: Some(3.0),
            is_building_story: None,
            color: None,
            extent_m: None,
            coalesce: true,
        },
    );
    engine.evaluate_pending(&mut doc);
    let updates = engine.poll_updates(&doc);
    assert!(updates.meshes.is_empty(), "no mesh upserts on an elevation edit");
    assert_eq!(updates.base_transforms.len(), 1);
    let moved = updates.base_transforms.first().expect("transform");
    assert_eq!(moved.id, placed.element);
    assert_eq!(
        [moved.transform[3], moved.transform[7], moved.transform[11]],
        [0.0, 0.0, 3.0]
    );
    assert_eq!(engine.tessellation_count(placed.element), tess);
    assert_eq!(engine.eval_count(placed.sketch), sketch_evals, "sketch not re-evaluated");
    let _ = placed.level;
}

// ---------------------------------------------------------------------
// Commands, undo/redo, validation tiers.
// ---------------------------------------------------------------------

#[test]
fn update_sketch_undo_redo_is_byte_exact_and_drags_coalesce() {
    let mut doc = Document::new();
    let mut engine = Engine::new();
    let s = build(&[(rect(0.0, 0.0, 4.0, 4.0), solid(0.3))]);
    let placed = place(&mut doc, s.clone(), SketchDirection::Below);
    let mesh_before = element_mesh(&mut doc, &mut engine, placed.element);
    let bytes_before = save(&doc);

    let split = ops::split_faces(&s, [2.0, -1.0], [2.0, 5.0]).expect("split");
    let split = ops::delete_faces(&split, &[0]).expect("delete left half");
    ok(
        &mut doc,
        Command::UpdateSketch {
            id: placed.sketch,
            sketch: split,
            coalesce: false,
        },
    );
    let mesh_half = element_mesh(&mut doc, &mut engine, placed.element);
    assert_near(mesh_volume(&mesh_half), 8.0 * 0.3);
    let bytes_after = save(&doc);

    doc.undo().expect("undo");
    assert_eq!(save(&doc), bytes_before, "undo restores the bytes");
    let mesh_undone = element_mesh(&mut doc, &mut engine, placed.element);
    assert_eq!(mesh_undone.indices, mesh_before.indices);
    assert_eq!(mesh_undone.positions, mesh_before.positions);
    doc.redo().expect("redo");
    assert_eq!(save(&doc), bytes_after, "redo restores the bytes");

    // A coalesced drag of one corner: one undo step for 20 updates.
    let corner = s.face(0).expect("face").points[2];
    let depth = doc.undo_depth();
    let mut current = doc
        .entity(placed.sketch)
        .and_then(|r| match &r.params {
            vim_design_lib::Params::Sketch { sketch, .. } => Some(sketch.clone()),
            _ => None,
        })
        .expect("sketch params");
    let corner_now = current.points.iter().find(|p| p.id == corner).map(|p| p.id);
    if let Some(corner) = corner_now {
        for step in 1..=20 {
            current = ops::set_point(&current, corner, [4.0 + f64::from(step) * 0.05, 4.0])
                .expect("set_point");
            ok(
                &mut doc,
                Command::UpdateSketch {
                    id: placed.sketch,
                    sketch: current.clone(),
                    coalesce: true,
                },
            );
        }
        assert_eq!(doc.undo_depth(), depth + 1, "the drag is one undo step");
        doc.undo().expect("undo drag");
        assert_eq!(save(&doc), bytes_after);
    }
}

#[test]
fn structural_problems_reject_the_command() {
    let mut doc = Document::new();
    let ground = level(&mut doc, 0.0);
    let bytes = save(&doc);
    let mut bad = build(&[(rect(0.0, 0.0, 1.0, 1.0), solid(0.3))]);
    if let Some(face) = bad.faces.first_mut() {
        face.points.push(99);
    }
    assert_eq!(
        doc.submit(Command::CreateSketch {
            plane: ground,
            sketch: bad.clone(),
            direction: SketchDirection::Below,
        })
        .err(),
        Some(VimStatus::InvalidSketch)
    );
    assert_eq!(save(&doc), bytes, "rejection leaves the document untouched");
    assert_eq!(
        sketch::validate_structure(&bad),
        Err(sketch::SketchError::MissingPoint { face: 0, point: 99 })
    );

    let good = build(&[(rect(0.0, 0.0, 1.0, 1.0), solid(0.3))]);
    let id = one(
        &mut doc,
        Command::CreateSketch {
            plane: ground,
            sketch: good,
            direction: SketchDirection::Below,
        },
    );
    let mut thin = build(&[(rect(0.0, 0.0, 1.0, 1.0), solid(0.3))]);
    if let Some(face) = thin.faces.first_mut() {
        face.kind = SketchFaceKind::Solid { thickness: 0.0 };
    }
    assert_eq!(
        doc.submit(Command::UpdateSketch {
            id,
            sketch: thin,
            coalesce: false,
        })
        .err(),
        Some(VimStatus::InvalidSketch)
    );
    // The plane slot accepts construction planes only.
    let cp = one(&mut doc, Command::CreateControlPoint { position: [0.0; 3] });
    assert_eq!(
        doc.submit(Command::CreateSketch {
            plane: cp,
            sketch: Sketch::default(),
            direction: SketchDirection::Below,
        })
        .err(),
        Some(VimStatus::SlotKindMismatch)
    );
}

#[test]
fn a_self_crossing_face_is_an_evaluation_error_with_stale_mesh() {
    let mut doc = Document::new();
    let mut engine = Engine::new();
    let s = build(&[(rect(0.0, 0.0, 2.0, 2.0), solid(0.3))]);
    let placed = place(&mut doc, s.clone(), SketchDirection::Below);
    let healthy = element_mesh(&mut doc, &mut engine, placed.element);

    // Drag a corner across the opposite edge: the loop crosses itself.
    let corner = s.face(0).expect("face").points[1];
    let crossed = ops::set_point(&s, corner, [-1.0, 1.5]).expect("set_point");
    assert!(matches!(
        sketch::validate(&crossed),
        Err(sketch::SketchError::SelfIntersecting { face: 0 })
    ));
    ok(
        &mut doc,
        Command::UpdateSketch {
            id: placed.sketch,
            sketch: crossed,
            coalesce: false,
        },
    );
    engine.evaluate_pending(&mut doc);
    let updates = engine.poll_updates(&doc);
    let kind = updates
        .errors
        .iter()
        .find(|(id, _)| *id == placed.sketch)
        .map(|(_, d)| d.kind);
    assert_eq!(kind, Some(EvalErrorKind::Degenerate));
    assert_eq!(updates.committed_generation, updates.evaluated_generation);
    let stale = engine.mesh(placed.element).expect("stale mesh retained");
    assert_eq!(stale.indices, healthy.indices);

    // Fix: the error clears.
    ok(
        &mut doc,
        Command::UpdateSketch {
            id: placed.sketch,
            sketch: s,
            coalesce: false,
        },
    );
    engine.evaluate_pending(&mut doc);
    let updates = engine.poll_updates(&doc);
    assert!(updates.errors.is_empty());
    assert!(updates.errors_cleared.contains(&placed.sketch));
}

#[test]
fn a_sketch_without_material_has_no_mesh_and_no_error() {
    let mut doc = Document::new();
    let mut engine = Engine::new();
    let s = build(&[(rect(0.0, 0.0, 2.0, 2.0), solid(0.3))]);
    let placed = place(&mut doc, s.clone(), SketchDirection::Below);
    element_mesh(&mut doc, &mut engine, placed.element);

    // A through void over the whole plate removes everything.
    let covered = ops::add_face(&s, &rect(-1.0, -1.0, 3.0, 3.0), void(None)).expect("add");
    ok(
        &mut doc,
        Command::UpdateSketch {
            id: placed.sketch,
            sketch: covered,
            coalesce: false,
        },
    );
    engine.evaluate_pending(&mut doc);
    let updates = engine.poll_updates(&doc);
    assert!(updates.errors.is_empty(), "{:?}", updates.errors);
    assert_eq!(updates.meshes_removed, vec![placed.element], "tombstoned");
    assert!(engine.mesh(placed.element).is_none());

    // Back: the mesh reappears.
    ok(
        &mut doc,
        Command::UpdateSketch {
            id: placed.sketch,
            sketch: s,
            coalesce: false,
        },
    );
    engine.evaluate_pending(&mut doc);
    let updates = engine.poll_updates(&doc);
    assert_eq!(updates.meshes.len(), 1);
}

#[test]
fn update_sketch_is_reported_by_the_pump() {
    let mut doc = Document::new();
    let mut engine = Engine::new();
    let s = build(&[(rect(0.0, 0.0, 2.0, 2.0), solid(0.3))]);
    let placed = place(&mut doc, s.clone(), SketchDirection::Below);
    engine.evaluate_pending(&mut doc);
    let _ = engine.poll_updates(&doc);
    let moved = ops::move_faces(&s, &[0], [1.0, 0.0]).expect("move");
    ok(
        &mut doc,
        Command::UpdateSketch {
            id: placed.sketch,
            sketch: moved,
            coalesce: false,
        },
    );
    engine.evaluate_pending(&mut doc);
    let updates = engine.poll_updates(&doc);
    assert_eq!(updates.params_changed, vec![placed.sketch]);
    assert_eq!(updates.meshes.len(), 1);
}

#[test]
fn a_standalone_sketch_is_its_own_mesh_owner() {
    let mut doc = Document::new();
    let mut engine = Engine::new();
    let ground = level(&mut doc, 0.0);
    let sketch = one(
        &mut doc,
        Command::CreateSketch {
            plane: ground,
            sketch: build(&[(rect(0.0, 0.0, 1.0, 1.0), solid(0.2))]),
            direction: SketchDirection::Below,
        },
    );
    engine.evaluate_pending(&mut doc);
    let updates = engine.poll_updates(&doc);
    assert_eq!(updates.meshes.first().map(|m| m.id), Some(sketch));
}

// ---------------------------------------------------------------------
// Deletion.
// ---------------------------------------------------------------------

#[test]
fn deleting_the_element_sweeps_its_sketch_and_the_level_cascade_takes_it() {
    let mut doc = Document::new();
    let placed = place(
        &mut doc,
        build(&[(rect(0.0, 0.0, 1.0, 1.0), solid(0.2))]),
        SketchDirection::Below,
    );
    let before = save(&doc);
    ok(
        &mut doc,
        Command::DeleteElement {
            id: placed.element,
            sweep_orphans: true,
        },
    );
    assert!(doc.entity(placed.sketch).is_none(), "swept");
    assert!(doc.entity(placed.level).is_some(), "levels are never swept");
    doc.undo().expect("undo");
    assert_eq!(save(&doc), before);

    // Keep-geometry escape hatch.
    ok(
        &mut doc,
        Command::DeleteElement {
            id: placed.element,
            sweep_orphans: false,
        },
    );
    assert!(doc.entity(placed.sketch).is_some());
    doc.undo().expect("undo");

    // The level cascade removes the element and its sketch in one step.
    ok(
        &mut doc,
        Command::DeleteLevel {
            id: placed.level,
            cascade: true,
        },
    );
    assert_eq!(doc.entity_count(), 0);
    doc.undo().expect("undo cascade");
    assert_eq!(save(&doc), before);
}

#[test]
fn a_document_with_sketches_round_trips() {
    let mut doc = Document::new();
    let placed = place(
        &mut doc,
        build(&[
            (rect(0.0, 0.0, 4.0, 4.0), solid(0.3)),
            (rect(1.0, 1.0, 2.0, 2.0), void(Some(0.1))),
            (rect(2.5, 2.5, 3.5, 3.5), void(None)),
        ]),
        SketchDirection::Below,
    );
    let bytes = save(&doc);
    let reloaded = Document::load(&bytes).expect("load");
    assert_eq!(save(&reloaded), bytes);
    assert_eq!(
        reloaded.entity(placed.sketch).map(|r| r.kind()),
        Some(EntityKind::Sketch)
    );
    vim_design_test::assert_save_load_roundtrip(&doc);
}

#[test]
fn without_factoring_a_sketch_bakes_its_level_elevation() {
    let mut doc = Document::new();
    let mut engine = Engine::new();
    let raised = level(&mut doc, 3.0);
    let placed = place_on(
        &mut doc,
        raised,
        build(&[(rect(0.0, 0.0, 2.0, 1.0), solid(0.3))]),
        SketchDirection::Below,
    );
    let mesh = element_mesh(&mut doc, &mut engine, placed.element);
    assert_bbox_near(&mesh, [0.0, 0.0, 2.7], [2.0, 1.0, 3.0], 1e-6);
    assert_near(mesh_volume(&mesh), 0.6);
}

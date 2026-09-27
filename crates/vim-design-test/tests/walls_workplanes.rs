//! Workplanes (nested construction planes) and walls (a reference line
//! plus an editable elevation profile): frames, nesting, cascade,
//! translation factoring, golden volumes, height constraints, anchors,
//! undo, validation tiers, and deletion.

use vim_design_lib::eval::{Engine, EvalErrorKind, Evaluated, SubRefResolution};
use vim_design_lib::sketch::{self, Sketch, SketchDirection, SketchFaceKind};
use vim_design_lib::wall::{self, ops as wall_ops};
use vim_design_lib::{
    Command, Document, EntityId, Mesh, ProvenancePath, SubRef, VimStatus, workplane,
};
use vim_design_test::{assert_bbox_near, mesh_bbox, mesh_volume, one, ok, save};

const THICKNESS: f64 = 0.2;

fn level(doc: &mut Document, name: &str, elevation_m: f64) -> EntityId {
    one(
        doc,
        Command::CreateLevel {
            name: name.to_owned(),
            elevation_m,
            is_building_story: true,
            color: [0.2, 0.5, 0.9, 0.3],
            extent_m: 10.0,
        },
    )
}

fn workplane_under(doc: &mut Document, parent: EntityId, offset_m: f64) -> EntityId {
    one(
        doc,
        Command::CreateWorkplane {
            parent,
            name: "Ceiling".to_owned(),
            offset_m,
            color: [0.5, 0.5, 0.5, 0.2],
            extent_m: 8.0,
        },
    )
}

fn frame(engine: &Engine, id: EntityId) -> ([f64; 3], [f64; 3]) {
    match engine.value(id) {
        Some(Evaluated::Frame {
            origin,
            level_offset,
            ..
        }) => (*origin, *level_offset),
        other => panic!("frame value: {other:?}"),
    }
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

/// A wall wrapped in an element associated with `association`.
struct PlacedWall {
    wall: EntityId,
    element: EntityId,
}

#[allow(clippy::too_many_arguments)]
fn place_wall(
    doc: &mut Document,
    association: EntityId,
    base: EntityId,
    top: Option<EntityId>,
    length: f64,
    height_m: f64,
    top_offset_m: f64,
    edit: impl FnOnce(Sketch, Vec<u32>) -> (Sketch, Vec<u32>),
) -> PlacedWall {
    let (profile, top_points) = wall::default_profile(length, THICKNESS);
    let (profile, top_points) = edit(profile, top_points);
    let wall = one(
        doc,
        Command::CreateWall {
            base,
            top,
            start: [0.0, 0.0],
            end: [length, 0.0],
            height_m,
            top_offset_m,
            profile,
            top_points,
        },
    );
    let element = one(
        doc,
        Command::CreateElement {
            name: "Wall 1".to_owned(),
            members: vec![wall],
            level: association,
        },
    );
    PlacedWall { wall, element }
}

fn unchanged(p: Sketch, t: Vec<u32>) -> (Sketch, Vec<u32>) {
    (p, t)
}

fn element_mesh(doc: &mut Document, engine: &mut Engine, element: EntityId) -> Mesh {
    engine.evaluate_pending(doc);
    let updates = engine.poll_updates(doc);
    assert!(updates.errors.is_empty(), "{:?}", updates.errors);
    engine.mesh(element).expect("element meshed").clone()
}

fn assert_near(actual: f64, expected: f64) {
    assert!(
        (actual - expected).abs() < 1e-6,
        "expected {expected}, got {actual}"
    );
}

fn has_vertex_at_z(mesh: &Mesh, z: f64) -> bool {
    mesh.positions
        .iter()
        .any(|p| (f64::from(p[2]) - z).abs() < 1e-6)
}

// ---------------------------------------------------------------------
// Workplanes.
// ---------------------------------------------------------------------

#[test]
fn workplanes_nest_and_follow_their_parent() {
    let mut doc = Document::new();
    let mut engine = Engine::new();
    let story = level(&mut doc, "Level 2", 3.0);
    let ceiling = workplane_under(&mut doc, story, 2.4);
    let above = workplane_under(&mut doc, ceiling, 0.1);
    engine.evaluate_pending(&mut doc);
    let updates = engine.poll_updates(&doc);
    assert!(updates.errors.is_empty(), "{:?}", updates.errors);
    assert!(updates.meshes.is_empty(), "planes never own meshes");

    assert_eq!(frame(&engine, ceiling), ([0.0, 0.0, 5.4], [0.0, 0.0, 2.4]));
    assert_eq!(frame(&engine, above).1, [0.0, 0.0, 2.4 + 0.1]);
    assert_eq!(workplane::root_level(&doc, above), Some(story));
    assert_eq!(workplane::root_level(&doc, story), Some(story));
    assert_eq!(
        workplane::plane_elevation(&doc, above),
        Some(frame(&engine, above).0[2]),
        "params-only elevation matches the evaluated frame exactly"
    );

    // Moving the level moves both workplanes; their level offsets stay.
    set_elevation(&mut doc, story, 3.5);
    engine.evaluate_pending(&mut doc);
    assert_eq!(frame(&engine, ceiling), ([0.0, 0.0, 3.5 + 2.4], [0.0, 0.0, 2.4]));

    // A parent inside its own subtree is a cycle.
    assert_eq!(
        doc.submit(Command::UpdateWorkplane {
            id: ceiling,
            parent: Some(above),
            name: None,
            offset_m: None,
            color: None,
            extent_m: None,
            coalesce: false,
        })
        .err(),
        Some(VimStatus::WouldCreateCycle)
    );
    assert_eq!(
        doc.submit(Command::CreateWorkplane {
            parent: story,
            name: "bad".to_owned(),
            offset_m: f64::NAN,
            color: [0.0; 4],
            extent_m: 1.0,
        })
        .err(),
        Some(VimStatus::InvalidCommand)
    );
    // Attachment accepts workplanes: a control point on the ceiling.
    let cp = one(&mut doc, Command::CreateControlPoint { position: [1.0, 1.0, 0.0] });
    ok(
        &mut doc,
        Command::UpdateControlPointPlane {
            id: cp,
            plane: Some(ceiling),
            position: None,
        },
    );
    engine.evaluate_pending(&mut doc);
    assert!(matches!(
        engine.value(cp),
        Some(Evaluated::Point(p)) if *p == [1.0, 1.0, 3.5 + 2.4]
    ));
}

#[test]
fn deleting_a_level_with_workplanes_cascades_as_one_step() {
    let mut doc = Document::new();
    let story = level(&mut doc, "Ground", 0.0);
    let ceiling = workplane_under(&mut doc, story, 2.4);
    let mut plate = Sketch::default();
    plate = sketch::ops::add_face(
        &plate,
        &[[0.0, 0.0], [2.0, 0.0], [2.0, 2.0], [0.0, 2.0]],
        SketchFaceKind::Solid { thickness: 0.05 },
    )
    .expect("add_face");
    let ceiling_sketch = one(
        &mut doc,
        Command::CreateSketch {
            plane: ceiling,
            sketch: plate,
            direction: SketchDirection::Below,
        },
    );
    one(
        &mut doc,
        Command::CreateElement {
            name: "Ceiling 1".to_owned(),
            members: vec![ceiling_sketch],
            level: story,
        },
    );
    assert_eq!(
        doc.submit(Command::DeleteLevel { id: story, cascade: false }).err(),
        Some(VimStatus::HasDependents)
    );
    assert_eq!(
        doc.submit(Command::DeleteWorkplane { id: ceiling }).err(),
        Some(VimStatus::HasDependents)
    );
    let before = save(&doc);
    let depth = doc.undo_depth();
    ok(&mut doc, Command::DeleteLevel { id: story, cascade: true });
    assert_eq!(doc.undo_depth(), depth + 1);
    assert_eq!(doc.entity_count(), 0, "level, workplane, sketch, element all gone");
    doc.undo().expect("undo cascade");
    assert_eq!(save(&doc), before, "byte-exact restore");
}

#[test]
fn a_sketch_on_a_workplane_is_level_local_and_root_drags_are_transform_only() {
    let mut doc = Document::new();
    let mut engine = Engine::new();
    engine.set_translation_factoring(true);
    let story = level(&mut doc, "Ground", 0.25);
    let ceiling = workplane_under(&mut doc, story, 2.4);
    let plate = sketch::ops::add_face(
        &Sketch::default(),
        &[[0.0, 0.0], [3.0, 0.0], [3.0, 2.0], [0.0, 2.0]],
        SketchFaceKind::Solid { thickness: 0.05 },
    )
    .expect("add_face");
    let s = one(
        &mut doc,
        Command::CreateSketch {
            plane: ceiling,
            sketch: plate,
            direction: SketchDirection::Below,
        },
    );
    let element = one(
        &mut doc,
        Command::CreateElement {
            name: "Ceiling 1".to_owned(),
            members: vec![s],
            level: story,
        },
    );
    engine.evaluate_pending(&mut doc);
    let updates = engine.poll_updates(&doc);
    assert!(updates.errors.is_empty(), "{:?}", updates.errors);
    let first = updates.meshes.first().expect("mesh");
    assert_eq!(first.id, element);
    assert_eq!(first.base_transform[11], 0.25, "root elevation as base");
    let (min, max) = mesh_bbox(&first.mesh);
    assert!((min[2] - 2.35).abs() < 1e-6 && (max[2] - 2.4).abs() < 1e-6, "offset baked locally");
    let evals = engine.eval_count(s);
    let tess = engine.tessellation_count(element);

    // Root drag: transform-only.
    set_elevation(&mut doc, story, 3.0);
    engine.evaluate_pending(&mut doc);
    let updates = engine.poll_updates(&doc);
    assert!(updates.meshes.is_empty(), "no mesh upserts on a root drag");
    assert_eq!(updates.base_transforms.len(), 1);
    assert_eq!(updates.base_transforms.first().map(|t| t.transform[11]), Some(3.0));
    assert_eq!(engine.eval_count(s), evals);
    assert_eq!(engine.tessellation_count(element), tess);

    // Workplane offset edit: the sketch re-evaluates.
    ok(
        &mut doc,
        Command::UpdateWorkplane {
            id: ceiling,
            parent: None,
            name: None,
            offset_m: Some(2.7),
            color: None,
            extent_m: None,
            coalesce: false,
        },
    );
    engine.evaluate_pending(&mut doc);
    let updates = engine.poll_updates(&doc);
    assert_eq!(updates.meshes.len(), 1, "offset edit re-meshes");
    let (_, max) = mesh_bbox(&updates.meshes.first().expect("mesh").mesh);
    assert!((max[2] - 2.7).abs() < 1e-6);
}

// ---------------------------------------------------------------------
// Walls: golden volumes.
// ---------------------------------------------------------------------

#[test]
fn a_fixed_height_wall() {
    let mut doc = Document::new();
    let mut engine = Engine::new();
    let ground = level(&mut doc, "Ground", 0.0);
    let placed = place_wall(&mut doc, ground, ground, None, 4.0, 2.7, 0.0, unchanged);
    let mesh = element_mesh(&mut doc, &mut engine, placed.element);
    assert_near(mesh_volume(&mesh), 4.0 * 2.7 * THICKNESS);
    // Material grows to the LEFT of start -> end (+y for a wall along +x).
    assert_bbox_near(&mesh, [0.0, 0.0, 0.0], [4.0, THICKNESS, 2.7], 1e-6);
    assert_eq!(wall::wall_top_height(&doc, placed.wall), Some(2.7));
}

#[test]
fn a_top_constrained_wall_reaches_its_top_plane() {
    let mut doc = Document::new();
    let mut engine = Engine::new();
    let ground = level(&mut doc, "Ground", 0.0);
    let second = level(&mut doc, "Level 2", 3.0);
    // Height stops 0.3 m under Level 2 (a slab gap); height_m unused.
    let placed = place_wall(&mut doc, ground, ground, Some(second), 4.0, 9.9, -0.3, unchanged);
    let mesh = element_mesh(&mut doc, &mut engine, placed.element);
    assert_bbox_near(&mesh, [0.0, 0.0, 0.0], [4.0, THICKNESS, 2.7], 1e-6);
    assert_near(mesh_volume(&mesh), 4.0 * 2.7 * THICKNESS);
    assert_eq!(wall::wall_top_height(&doc, placed.wall), Some((3.0 - 0.0) + -0.3));
}

fn with_void(corners: [[f64; 2]; 4], depth: Option<f64>) -> impl FnOnce(Sketch, Vec<u32>) -> (Sketch, Vec<u32>) {
    move |p, t| {
        wall_ops::add_face(&p, &t, 2.7, &corners, SketchFaceKind::Void { depth }).expect("add_face")
    }
}

#[test]
fn a_window_goes_through_and_a_niche_does_not() {
    let window = [[1.0, 0.9], [2.2, 0.9], [2.2, 1.9], [1.0, 1.9]];
    let full = 4.0 * 2.7 * THICKNESS;

    let mut doc = Document::new();
    let mut engine = Engine::new();
    let ground = level(&mut doc, "Ground", 0.0);
    let placed = place_wall(&mut doc, ground, ground, None, 4.0, 2.7, 0.0, with_void(window, None));
    let mesh = element_mesh(&mut doc, &mut engine, placed.element);
    assert_near(mesh_volume(&mesh), full - 1.2 * 1.0 * THICKNESS);

    let mut doc = Document::new();
    let mut engine = Engine::new();
    let ground = level(&mut doc, "Ground", 0.0);
    let placed =
        place_wall(&mut doc, ground, ground, None, 4.0, 2.7, 0.0, with_void(window, Some(0.1)));
    let mesh = element_mesh(&mut doc, &mut engine, placed.element);
    assert_near(mesh_volume(&mesh), full - 1.2 * 1.0 * 0.1);
}

#[test]
fn a_door_is_a_void_that_crosses_the_bottom_edge() {
    let door = [[1.0, -0.5], [2.0, -0.5], [2.0, 2.1], [1.0, 2.1]];
    let mut doc = Document::new();
    let mut engine = Engine::new();
    let ground = level(&mut doc, "Ground", 0.0);
    let placed = place_wall(&mut doc, ground, ground, None, 4.0, 2.7, 0.0, with_void(door, None));
    let mesh = element_mesh(&mut doc, &mut engine, placed.element);
    assert_near(mesh_volume(&mesh), (4.0 * 2.7 - 1.0 * 2.1) * THICKNESS);
    assert_bbox_near(&mesh, [0.0, 0.0, 0.0], [4.0, THICKNESS, 2.7], 1e-6);
}

#[test]
fn a_gable_with_a_top_anchored_apex() {
    let mut doc = Document::new();
    let mut engine = Engine::new();
    let ground = level(&mut doc, "Ground", 0.0);
    let gable = |p: Sketch, t: Vec<u32>| {
        let apex = sketch::next_point_id(&p);
        let (p, t) = wall_ops::insert_point_on_edge(&p, &t, 2.7, 2, 3, 0.5).expect("insert");
        assert!(t.contains(&apex), "a point on the top edge is top-anchored");
        wall_ops::move_points(&p, &t, 2.7, &[apex], [0.0, 1.0]).expect("move")
    };
    let placed = place_wall(&mut doc, ground, ground, None, 4.0, 2.7, 0.0, gable);
    let mesh = element_mesh(&mut doc, &mut engine, placed.element);
    assert_near(mesh_volume(&mesh), (4.0 * 2.7 + 0.5 * 4.0 * 1.0) * THICKNESS);
    assert_bbox_near(&mesh, [0.0, 0.0, 0.0], [4.0, THICKNESS, 3.7], 1e-6);
}

#[test]
fn a_wall_on_a_workplane_starts_at_its_offset() {
    let mut doc = Document::new();
    let mut engine = Engine::new();
    let ground = level(&mut doc, "Ground", 0.0);
    let parapet_base = workplane_under(&mut doc, ground, 3.0);
    let placed = place_wall(&mut doc, ground, parapet_base, None, 4.0, 1.0, 0.0, unchanged);
    let mesh = element_mesh(&mut doc, &mut engine, placed.element);
    assert_bbox_near(&mesh, [0.0, 0.0, 3.0], [4.0, THICKNESS, 4.0], 1e-6);
}

// ---------------------------------------------------------------------
// Walls: height constraints and factoring.
// ---------------------------------------------------------------------

#[test]
fn dragging_the_top_level_changes_the_height_and_windows_keep_their_sill() {
    let mut doc = Document::new();
    let mut engine = Engine::new();
    engine.set_translation_factoring(true);
    let ground = level(&mut doc, "Ground", 0.0);
    let second = level(&mut doc, "Level 2", 3.0);
    let window = [[1.0, 0.9], [2.2, 0.9], [2.2, 1.9], [1.0, 1.9]];
    let placed = place_wall(&mut doc, ground, ground, Some(second), 4.0, 2.7, 0.0, |p, t| {
        wall_ops::add_face(&p, &t, 3.0, &window, SketchFaceKind::Void { depth: None })
            .expect("add_face")
    });
    let before = element_mesh(&mut doc, &mut engine, placed.element);
    assert_near(mesh_volume(&before), (4.0 * 3.0 - 1.2) * THICKNESS);
    assert!(has_vertex_at_z(&before, 0.9) && has_vertex_at_z(&before, 1.9));

    set_elevation(&mut doc, second, 3.5);
    engine.evaluate_pending(&mut doc);
    let updates = engine.poll_updates(&doc);
    assert!(updates.errors.is_empty(), "{:?}", updates.errors);
    assert_eq!(updates.meshes.len(), 1, "a top-constrained wall re-meshes");
    let after = engine.mesh(placed.element).expect("mesh").clone();
    assert_near(mesh_volume(&after), (4.0 * 3.5 - 1.2) * THICKNESS);
    let (_, max) = mesh_bbox(&after);
    assert!((max[2] - 3.5).abs() < 1e-6);
    assert!(has_vertex_at_z(&after, 0.9) && has_vertex_at_z(&after, 1.9), "sill kept");
    assert_eq!(wall::wall_top_height(&doc, placed.wall), Some(3.5));

    // A top at or below the base is an evaluation error, not a crash.
    set_elevation(&mut doc, second, -1.0);
    engine.evaluate_pending(&mut doc);
    let updates = engine.poll_updates(&doc);
    let kind = updates
        .errors
        .iter()
        .find(|(id, _)| *id == placed.wall)
        .map(|(_, d)| d.kind);
    assert_eq!(kind, Some(EvalErrorKind::Degenerate));
    assert!(engine.mesh(placed.element).is_some(), "stale mesh retained");
}

#[test]
fn dragging_the_base_level_of_a_fixed_height_wall_is_transform_only() {
    let mut doc = Document::new();
    let mut engine = Engine::new();
    engine.set_translation_factoring(true);
    let ground = level(&mut doc, "Ground", 0.0);
    let placed = place_wall(&mut doc, ground, ground, None, 4.0, 2.7, 0.0, unchanged);
    element_mesh(&mut doc, &mut engine, placed.element);
    let evals = engine.eval_count(placed.wall);
    let tess = engine.tessellation_count(placed.element);

    set_elevation(&mut doc, ground, 1.2);
    engine.evaluate_pending(&mut doc);
    let updates = engine.poll_updates(&doc);
    assert!(updates.meshes.is_empty());
    assert_eq!(updates.base_transforms.len(), 1);
    assert_eq!(updates.base_transforms.first().map(|t| t.transform[11]), Some(1.2));
    assert_eq!(engine.eval_count(placed.wall), evals);
    assert_eq!(engine.tessellation_count(placed.element), tess);
}

// ---------------------------------------------------------------------
// Walls: provenance, undo, validation, deletion.
// ---------------------------------------------------------------------

#[test]
fn wall_faces_are_named_from_profile_ids() {
    let mut doc = Document::new();
    let mut engine = Engine::new();
    let ground = level(&mut doc, "Ground", 0.0);
    let placed = place_wall(&mut doc, ground, ground, None, 4.0, 2.7, 0.0, unchanged);
    engine.evaluate_pending(&mut doc);
    let sub = |path| SubRef {
        owner: placed.wall,
        path,
    };
    // The top of the wall: the side swept by profile edge 2-3.
    assert_eq!(
        engine.resolve_subref(&sub(ProvenancePath::SketchSide { face: 0, a: 2, b: 3 })),
        Ok(SubRefResolution::Faces(1))
    );
    // The reference face (depth 0) and the far face (the thickness).
    assert_eq!(
        engine.resolve_subref(&sub(ProvenancePath::SketchCap {
            depth_um: 0,
            toward_plane: true
        })),
        Ok(SubRefResolution::Faces(1))
    );
    assert_eq!(
        engine.resolve_subref(&sub(ProvenancePath::SketchCap {
            depth_um: 200_000,
            toward_plane: false
        })),
        Ok(SubRefResolution::Faces(1))
    );
}

#[test]
fn update_wall_undo_redo_is_byte_exact_and_drags_coalesce() {
    let mut doc = Document::new();
    let mut engine = Engine::new();
    let ground = level(&mut doc, "Ground", 0.0);
    let placed = place_wall(&mut doc, ground, ground, None, 4.0, 2.7, 0.0, unchanged);
    let mesh_before = element_mesh(&mut doc, &mut engine, placed.element);
    let bytes_before = save(&doc);

    let (profile, top) = wall::default_profile(4.0, THICKNESS);
    let (profile, top) = wall_ops::add_face(
        &profile,
        &top,
        2.7,
        &[[1.0, 0.9], [2.0, 0.9], [2.0, 1.9], [1.0, 1.9]],
        SketchFaceKind::Void { depth: None },
    )
    .expect("add_face");
    ok(
        &mut doc,
        Command::UpdateWall {
            id: placed.wall,
            base: None,
            top: None,
            start: None,
            end: None,
            height_m: None,
            top_offset_m: None,
            profile: Some(profile),
            top_points: Some(top),
            coalesce: false,
        },
    );
    let bytes_after = save(&doc);
    doc.undo().expect("undo");
    assert_eq!(save(&doc), bytes_before);
    let mesh_undone = element_mesh(&mut doc, &mut engine, placed.element);
    assert_eq!(mesh_undone.indices, mesh_before.indices);
    doc.redo().expect("redo");
    assert_eq!(save(&doc), bytes_after);

    // A height drag of 10 coalesced updates is one undo step.
    let depth = doc.undo_depth();
    for step in 1..=10 {
        ok(
            &mut doc,
            Command::UpdateWall {
                id: placed.wall,
                base: None,
                top: None,
                start: None,
                end: None,
                height_m: Some(2.7 + f64::from(step) * 0.1),
                top_offset_m: None,
                profile: None,
                top_points: None,
                coalesce: true,
            },
        );
    }
    assert_eq!(doc.undo_depth(), depth + 1);
    doc.undo().expect("undo drag");
    assert_eq!(save(&doc), bytes_after);
}

#[test]
fn structural_problems_reject_and_geometric_ones_are_evaluation_errors() {
    let mut doc = Document::new();
    let mut engine = Engine::new();
    let ground = level(&mut doc, "Ground", 0.0);
    let bytes = save(&doc);
    let (profile, top) = wall::default_profile(4.0, THICKNESS);
    let create = |start: [f64; 2], end: [f64; 2], height: f64, top_points: Vec<u32>| {
        Command::CreateWall {
            base: ground,
            top: None,
            start,
            end,
            height_m: height,
            top_offset_m: 0.0,
            profile: profile.clone(),
            top_points,
        }
    };
    for bad in [
        create([1.0, 1.0], [1.0, 1.0], 2.7, top.clone()),
        create([0.0, 0.0], [4.0, 0.0], 0.0, top.clone()),
        create([0.0, 0.0], [4.0, 0.0], 2.7, vec![2, 3, 17]),
    ] {
        assert_eq!(doc.submit(bad).err(), Some(VimStatus::InvalidWall));
    }
    assert_eq!(save(&doc), bytes, "rejections leave the document untouched");
    assert_eq!(
        wall::validate_structure([0.0; 2], [4.0, 0.0], 2.7, 0.0, &profile, &[5]),
        Err(wall::WallError::UnknownTopPoint(5))
    );

    // A self-crossing effective profile: drag a base corner across the far side.
    let placed = place_wall(&mut doc, ground, ground, None, 4.0, 2.7, 0.0, unchanged);
    element_mesh(&mut doc, &mut engine, placed.element);
    let (crossed, crossed_top) =
        wall_ops::set_point(&profile, &top, 2.7, 1, [-1.0, 1.0]).expect("set_point");
    ok(
        &mut doc,
        Command::UpdateWall {
            id: placed.wall,
            base: None,
            top: None,
            start: None,
            end: None,
            height_m: None,
            top_offset_m: None,
            profile: Some(crossed),
            top_points: Some(crossed_top),
            coalesce: false,
        },
    );
    engine.evaluate_pending(&mut doc);
    let updates = engine.poll_updates(&doc);
    let kind = updates
        .errors
        .iter()
        .find(|(id, _)| *id == placed.wall)
        .map(|(_, d)| d.kind);
    assert_eq!(kind, Some(EvalErrorKind::Degenerate));
    assert!(engine.mesh(placed.element).is_some(), "stale mesh retained");
}

#[test]
fn deleting_the_element_sweeps_the_wall_and_cascades_take_it() {
    let mut doc = Document::new();
    let ground = level(&mut doc, "Ground", 0.0);
    let second = level(&mut doc, "Level 2", 3.0);
    let placed = place_wall(&mut doc, ground, ground, Some(second), 4.0, 2.7, 0.0, unchanged);
    let before = save(&doc);
    ok(
        &mut doc,
        Command::DeleteElement {
            id: placed.element,
            sweep_orphans: true,
        },
    );
    assert!(doc.entity(placed.wall).is_none(), "swept");
    doc.undo().expect("undo");
    assert_eq!(save(&doc), before);

    // The TOP level's cascade also takes the wall (it is a dependent).
    ok(&mut doc, Command::DeleteLevel { id: second, cascade: true });
    assert!(doc.entity(placed.wall).is_none());
    assert!(doc.entity(placed.element).is_none());
    assert!(doc.entity(ground).is_some());
    doc.undo().expect("undo cascade");
    assert_eq!(save(&doc), before);
    vim_design_test::assert_save_load_roundtrip(&doc);
}

//! Evaluation-performance milestone: early-cutoff memoization and
//! translation factoring (see eval/mod.rs "Evaluation performance").
//!
//! Headlines under test:
//! - a Level edit that does not move the frame (name/color/extent)
//!   causes ZERO downstream re-evaluation and re-tessellation;
//! - an elevation drag over a fully-attached scene is TRANSFORM-ONLY:
//!   no re-evaluation of solids, no re-tessellation, no mesh upserts —
//!   just base-transform updates in the poll;
//! - mixed (partially attached) owners keep the full re-eval path.

use vim_design_lib::eval::Engine;
use vim_design_lib::{
    Command, Document, EntityId, FaceTarget, ProvenancePath, SubRef,
};
use vim_design_test::{
    build_cone, build_cube, build_cylinder, build_plate, mesh_bbox, one, ok,
};

/// Engine with translation factoring enabled (the renderer-side
/// contract these tests exercise; default engines stay world-baked).
fn factored_engine() -> Engine {
    let mut engine = Engine::new();
    engine.set_translation_factoring(true);
    engine
}

fn create_level(doc: &mut Document, elevation_m: f64) -> EntityId {
    one(
        doc,
        Command::CreateLevel {
            name: "Ground".to_owned(),
            elevation_m,
            is_building_story: true,
            color: [0.2, 0.5, 0.9, 0.35],
            extent_m: 10.0,
        },
    )
}

fn attach_all(doc: &mut Document, level: EntityId, cps: &[EntityId]) {
    for cp in cps {
        ok(
            doc,
            Command::UpdateControlPointPlane {
                id: *cp,
                plane: Some(level),
                position: None,
            },
        );
    }
}

fn translation_of(transform: [f64; 12]) -> [f64; 3] {
    [transform[3], transform[7], transform[11]]
}

/// The four demo objects with EVERY construction control point attached
/// to `level` — all owners qualify for translation factoring.
struct AttachedScene {
    owners: Vec<EntityId>,
    cube_face: EntityId,
    cube_extrusion: EntityId,
}

fn build_fully_attached_scene(doc: &mut Document, level: EntityId) -> AttachedScene {
    let cube = build_cube(doc, [0.0, 0.0, 0.0], 1.0, 1.0);
    let mut cube_cps = cube.base.cps.clone();
    cube_cps.extend(cube.path_cps);
    attach_all(doc, level, &cube_cps);

    let plate = build_plate(doc, [3.0, 0.0, 0.0], 4.0, 3.0, 0.5, true);
    let mut plate_cps = plate.outer.cps.clone();
    if let Some(hole) = &plate.hole {
        plate_cps.extend(hole.cps.iter().copied());
    }
    plate_cps.extend(plate.path_cps);
    attach_all(doc, level, &plate_cps);

    let cylinder = build_cylinder(doc, [10.0, 0.0, 0.0], 0.5, 2.0);
    // Composite order: center cp, top cp, circle, edge, wire, face,
    // line, extrusion.
    attach_all(doc, level, &cylinder[0..2]);
    let cyl_extrusion = *cylinder.last().expect("extrusion last");

    let cone = build_cone(doc, [14.0, 0.0, 0.0], 1.5, 2.0);
    attach_all(doc, level, &[cone.base_cp, cone.rim_cp, cone.apex_cp]);

    AttachedScene {
        owners: vec![
            cube.extrusion,
            plate.extrusion,
            cyl_extrusion,
            cone.revolve,
        ],
        cube_face: cube.face,
        cube_extrusion: cube.extrusion,
    }
}

#[test]
fn cosmetic_level_edit_causes_zero_downstream_work() {
    let mut doc = Document::new();
    let mut engine = factored_engine();
    let level = create_level(&mut doc, 0.5);
    let scene = build_fully_attached_scene(&mut doc, level);
    engine.evaluate_pending(&mut doc);
    let updates = engine.poll_updates(&doc);
    assert!(updates.errors.is_empty(), "{:?}", updates.errors);
    let counts_before: Vec<(EntityId, u64)> = doc
        .entities()
        .map(|(id, _)| (*id, engine.eval_count(*id)))
        .collect();

    // Rename + recolor + resize the display square: the Frame is EQUAL,
    // so the early cutoff stops everything downstream.
    ok(
        &mut doc,
        Command::UpdateLevel {
            id: level,
            name: Some("Ground floor".to_owned()),
            elevation_m: None,
            is_building_story: Some(false),
            color: Some([0.9, 0.2, 0.2, 0.5]),
            extent_m: Some(25.0),
            coalesce: false,
        },
    );
    engine.evaluate_pending(&mut doc);
    let updates = engine.poll_updates(&doc);

    assert!(updates.meshes.is_empty(), "no re-tessellation: {updates:?}");
    assert!(updates.base_transforms.is_empty(), "frame did not move");
    assert!(updates.errors.is_empty());
    assert_eq!(
        updates.params_changed,
        vec![level],
        "the pump still reports the level edit"
    );
    assert_eq!(updates.committed_generation, updates.evaluated_generation);
    for (id, before) in counts_before {
        let expected = if id == level { before + 1 } else { before };
        assert_eq!(
            engine.eval_count(id),
            expected,
            "entity {id:?}: only the level itself re-evaluates"
        );
    }
    for owner in &scene.owners {
        assert_eq!(engine.tessellation_count(*owner), 1);
    }
}

#[test]
fn elevation_drag_over_attached_scene_is_transform_only() {
    let mut doc = Document::new();
    let mut engine = factored_engine();
    let level = create_level(&mut doc, 0.5);
    let scene = build_fully_attached_scene(&mut doc, level);
    engine.evaluate_pending(&mut doc);
    let updates = engine.poll_updates(&doc);
    assert!(updates.errors.is_empty(), "{:?}", updates.errors);
    assert_eq!(updates.meshes.len(), 4);
    // Qualified owners deliver LOCAL meshes + the elevation as a base
    // transform (composition: world = instance ∘ base).
    for update in &updates.meshes {
        assert_eq!(
            translation_of(update.base_transform),
            [0.0, 0.0, 0.5],
            "owner {:?} carries the level origin as its base",
            update.id
        );
    }
    let cube_mesh = engine.mesh(scene.cube_extrusion).expect("cube mesh").clone();
    let (min, max) = mesh_bbox(&cube_mesh);
    assert!(
        (min[2] - 0.0).abs() < 1e-9 && (max[2] - 1.0).abs() < 1e-9,
        "mesh is level-LOCAL (z from 0): {min:?}..{max:?}"
    );
    let tess_before: Vec<u64> = scene
        .owners
        .iter()
        .map(|o| engine.tessellation_count(*o))
        .collect();
    let solid_evals_before = engine.eval_count(scene.cube_extrusion);
    let face_evals_before = engine.eval_count(scene.cube_face);

    // The drag: 0.5 -> 2.0.
    ok(
        &mut doc,
        Command::UpdateLevel {
            id: level,
            name: None,
            elevation_m: Some(2.0),
            is_building_story: None,
            color: None,
            extent_m: None,
            coalesce: true,
        },
    );
    engine.evaluate_pending(&mut doc);
    let updates = engine.poll_updates(&doc);

    // NO mesh upserts, NO re-tessellation, NO solid/face re-evaluation —
    // only transform updates.
    assert!(updates.meshes.is_empty(), "transform-only poll: {:?}",
        updates.meshes.iter().map(|m| m.id).collect::<Vec<_>>());
    assert!(updates.errors.is_empty());
    let mut moved: Vec<EntityId> =
        updates.base_transforms.iter().map(|t| t.id).collect();
    moved.sort();
    let mut expected = scene.owners.clone();
    expected.sort();
    assert_eq!(moved, expected, "all four owners re-placed");
    for update in &updates.base_transforms {
        assert_eq!(translation_of(update.transform), [0.0, 0.0, 2.0]);
    }
    let tess_after: Vec<u64> = scene
        .owners
        .iter()
        .map(|o| engine.tessellation_count(*o))
        .collect();
    assert_eq!(tess_after, tess_before, "zero re-tessellation");
    assert_eq!(engine.eval_count(scene.cube_extrusion), solid_evals_before);
    assert_eq!(engine.eval_count(scene.cube_face), face_evals_before);
    assert_eq!(
        engine.mesh(scene.cube_extrusion).map(|m| m.indices.clone()),
        Some(cube_mesh.indices.clone()),
        "mesh bytes untouched"
    );
    assert_eq!(updates.committed_generation, updates.evaluated_generation);

    // Undo the drag: transform-only again, back to 0.5.
    doc.undo().expect("undo drag");
    engine.evaluate_pending(&mut doc);
    let updates = engine.poll_updates(&doc);
    assert!(updates.meshes.is_empty(), "undo is transform-only too");
    assert_eq!(updates.base_transforms.len(), 4);
    for update in &updates.base_transforms {
        assert_eq!(translation_of(update.transform), [0.0, 0.0, 0.5]);
    }
    assert_eq!(
        scene
            .owners
            .iter()
            .map(|o| engine.tessellation_count(*o))
            .collect::<Vec<_>>(),
        tess_before
    );
}

#[test]
fn paint_and_chamfer_survive_a_factored_drag() {
    let mut doc = Document::new();
    let mut engine = factored_engine();
    let level = create_level(&mut doc, 1.0);
    let cube = build_cube(&mut doc, [0.0, 0.0, 0.0], 1.0, 1.0);
    let mut cps = cube.base.cps.clone();
    cps.extend(cube.path_cps);
    attach_all(&mut doc, level, &cps);
    let red = one(
        &mut doc,
        Command::CreateMaterial {
            name: "red".to_owned(),
            color: [0.9, 0.1, 0.1],
            roughness: 0.5,
        },
    );
    let south = ProvenancePath::Side {
        source: cube.base.edges[0],
    };
    ok(
        &mut doc,
        Command::UpdateSubFaceMaterial {
            owner: cube.extrusion,
            target: FaceTarget::One(south),
            material: Some(red),
        },
    );
    let chamfer = one(
        &mut doc,
        Command::CreateChamfer {
            target: cube.extrusion,
            distance: 0.1,
            edges: vec![],
            sub_edges: vec![SubRef {
                owner: cube.extrusion,
                path: ProvenancePath::shared_edge(
                    ProvenancePath::CapEnd,
                    ProvenancePath::Side {
                        source: cube.base.edges[0],
                    },
                ),
            }],
        },
    );
    engine.evaluate_pending(&mut doc);
    let updates = engine.poll_updates(&doc);
    assert!(updates.errors.is_empty(), "{:?}", updates.errors);
    assert_eq!(updates.meshes.len(), 1);
    assert_eq!(updates.meshes[0].id, chamfer, "chamfer owns the mesh");
    assert_eq!(
        translation_of(updates.meshes[0].base_transform),
        [0.0, 0.0, 1.0],
        "chamfered owner still factored"
    );
    let mesh_before = updates.meshes[0].mesh.clone();
    assert_eq!(mesh_before.submeshes.len(), 2, "painted + default submeshes");
    let tess_before = engine.tessellation_count(chamfer);

    // Elevation drag: provenance, paint, and blend all live in local
    // space — nothing re-evaluates, the transform carries the move.
    ok(
        &mut doc,
        Command::UpdateLevel {
            id: level,
            name: None,
            elevation_m: Some(2.5),
            is_building_story: None,
            color: None,
            extent_m: None,
            coalesce: true,
        },
    );
    engine.evaluate_pending(&mut doc);
    let updates = engine.poll_updates(&doc);
    assert!(updates.meshes.is_empty());
    assert!(updates.errors.is_empty());
    assert_eq!(updates.base_transforms.len(), 1);
    assert_eq!(updates.base_transforms[0].id, chamfer);
    assert_eq!(
        translation_of(updates.base_transforms[0].transform),
        [0.0, 0.0, 2.5]
    );
    assert_eq!(engine.tessellation_count(chamfer), tess_before);
    let mesh_after = engine.mesh(chamfer).expect("mesh retained");
    assert_eq!(mesh_after.indices, mesh_before.indices);
    assert_eq!(mesh_after.submeshes, mesh_before.submeshes, "paint intact");
}

/// Wrap each producer in an Element ASSOCIATED WITH `level` (the demo's
/// exact shape: association target == the dragged attach level) plus one
/// instance each. Returns the element ids (the new mesh owners).
fn wrap_in_elements(
    doc: &mut Document,
    level: EntityId,
    producers: &[EntityId],
) -> Vec<EntityId> {
    producers
        .iter()
        .enumerate()
        .map(|(index, member)| {
            let element = one(
                doc,
                Command::CreateElement {
                    name: format!("e{index}"),
                    members: vec![*member],
                    level,
                },
            );
            one(
                doc,
                Command::CreateInstance {
                    element,
                    transform: vim_design_test::IDENTITY_XFORM,
                },
            );
            element
        })
        .collect()
}

#[test]
fn element_wrapped_attached_scene_drag_is_transform_only() {
    // THE demo shape: four fully-attached objects, each wrapped in an
    // element associated with the SAME level being dragged. The
    // data-only association edge must not break the transform-only path.
    let mut doc = Document::new();
    let mut engine = factored_engine();
    let level = create_level(&mut doc, 0.5);
    let scene = build_fully_attached_scene(&mut doc, level);
    let elements = wrap_in_elements(&mut doc, level, &scene.owners);
    engine.evaluate_pending(&mut doc);
    let updates = engine.poll_updates(&doc);
    assert!(updates.errors.is_empty(), "{:?}", updates.errors);
    assert_eq!(updates.meshes.len(), 4, "owners are the elements now");
    for update in &updates.meshes {
        assert!(elements.contains(&update.id));
        assert_eq!(translation_of(update.base_transform), [0.0, 0.0, 0.5]);
    }
    let tess_before: Vec<u64> = elements
        .iter()
        .map(|e| engine.tessellation_count(*e))
        .collect();
    let element_evals_before: Vec<u64> =
        elements.iter().map(|e| engine.eval_count(*e)).collect();

    ok(
        &mut doc,
        Command::UpdateLevel {
            id: level,
            name: None,
            elevation_m: Some(2.0),
            is_building_story: None,
            color: None,
            extent_m: None,
            coalesce: true,
        },
    );
    engine.evaluate_pending(&mut doc);
    let updates = engine.poll_updates(&doc);

    assert!(
        updates.meshes.is_empty(),
        "transform-only despite the association edge: {:?}",
        updates.meshes.iter().map(|m| m.id).collect::<Vec<_>>()
    );
    assert!(updates.errors.is_empty());
    let mut moved: Vec<EntityId> =
        updates.base_transforms.iter().map(|t| t.id).collect();
    moved.sort();
    let mut expected = elements.clone();
    expected.sort();
    assert_eq!(moved, expected, "exactly one base transform per element owner");
    for update in &updates.base_transforms {
        assert_eq!(translation_of(update.transform), [0.0, 0.0, 2.0]);
    }
    // Eval-count evidence: the elements were SKIPPED (the association
    // edge is exempt from value-change propagation), and nothing was
    // re-tessellated.
    assert_eq!(
        elements
            .iter()
            .map(|e| engine.eval_count(*e))
            .collect::<Vec<_>>(),
        element_evals_before,
        "elements did not re-evaluate through the association edge"
    );
    assert_eq!(
        elements
            .iter()
            .map(|e| engine.tessellation_count(*e))
            .collect::<Vec<_>>(),
        tess_before,
        "zero re-tessellation"
    );
    assert_eq!(updates.committed_generation, updates.evaluated_generation);
}

#[test]
fn association_rewire_still_redelivers_byte_identical() {
    // The mesh-inert contract survives the exemption: REWIRING the
    // association (a commit-gate root) still re-evaluates the element
    // and re-delivers a byte-identical mesh, and the pump reports it.
    let mut doc = Document::new();
    let mut engine = factored_engine();
    let level = create_level(&mut doc, 0.5);
    let other = one(
        &mut doc,
        Command::CreateLevel {
            name: "L2".to_owned(),
            elevation_m: 3.0,
            is_building_story: true,
            color: [0.2, 0.5, 0.9, 0.35],
            extent_m: 10.0,
        },
    );
    let cube = build_cube(&mut doc, [0.0, 0.0, 0.0], 1.0, 1.0);
    let mut cps = cube.base.cps.clone();
    cps.extend(cube.path_cps);
    attach_all(&mut doc, level, &cps);
    let element = one(
        &mut doc,
        Command::CreateElement {
            name: "cube".to_owned(),
            members: vec![cube.extrusion],
            level,
        },
    );
    engine.evaluate_pending(&mut doc);
    let updates = engine.poll_updates(&doc);
    let before = updates.meshes[0].mesh.clone();
    let base_before = updates.meshes[0].base_transform;

    ok(
        &mut doc,
        Command::UpdateElementLevel {
            element,
            level: other,
        },
    );
    engine.evaluate_pending(&mut doc);
    let updates = engine.poll_updates(&doc);
    assert!(updates.errors.is_empty());
    assert!(updates.params_changed.contains(&element), "pump reports the rewire");
    assert_eq!(updates.meshes.len(), 1, "root rewire re-delivers");
    assert_eq!(updates.meshes[0].mesh.positions, before.positions);
    assert_eq!(updates.meshes[0].mesh.indices, before.indices);
    assert_eq!(updates.meshes[0].mesh.submeshes, before.submeshes);
    // The base still follows the ATTACH level (association is data-only:
    // members are attached to `level`, not `other`).
    assert_eq!(updates.meshes[0].base_transform, base_before);
}

#[test]
fn mixed_owner_keeps_the_full_reeval_path() {
    let mut doc = Document::new();
    let mut engine = factored_engine();
    let level = create_level(&mut doc, 0.5);
    // Base square attached, extrusion path in WORLD coordinates: the
    // owner is disqualified (mixed closure) and must re-evaluate fully.
    let cube = build_cube(&mut doc, [0.0, 0.0, 0.0], 1.0, 1.0);
    attach_all(&mut doc, level, &cube.base.cps);
    engine.evaluate_pending(&mut doc);
    let updates = engine.poll_updates(&doc);
    assert!(updates.errors.is_empty(), "{:?}", updates.errors);
    assert_eq!(updates.meshes.len(), 1);
    assert_eq!(
        translation_of(updates.meshes[0].base_transform),
        [0.0, 0.0, 0.0],
        "world-space owner: identity base transform"
    );
    let (min, max) = mesh_bbox(&updates.meshes[0].mesh);
    assert!((min[2] - 0.5).abs() < 1e-9 && (max[2] - 1.5).abs() < 1e-9,
        "mixed owner bakes world coordinates: {min:?}..{max:?}");
    let tess_before = engine.tessellation_count(cube.extrusion);

    ok(
        &mut doc,
        Command::UpdateLevel {
            id: level,
            name: None,
            elevation_m: Some(1.5),
            is_building_story: None,
            color: None,
            extent_m: None,
            coalesce: true,
        },
    );
    engine.evaluate_pending(&mut doc);
    let updates = engine.poll_updates(&doc);
    assert!(updates.errors.is_empty(), "{:?}", updates.errors);
    assert_eq!(updates.meshes.len(), 1, "full re-eval path: mesh upsert");
    assert!(updates.base_transforms.is_empty());
    assert_eq!(engine.tessellation_count(cube.extrusion), tess_before + 1);
    let (min, max) = mesh_bbox(&updates.meshes[0].mesh);
    assert!((min[2] - 1.5).abs() < 1e-9 && (max[2] - 2.5).abs() < 1e-9);
}

#[test]
fn attached_scene_round_trips_through_save_load() {
    let mut doc = Document::new();
    let mut engine = factored_engine();
    let level = create_level(&mut doc, 0.75);
    let scene = build_fully_attached_scene(&mut doc, level);
    engine.evaluate_pending(&mut doc);
    let _ = engine.poll_updates(&doc);
    let original: Vec<(EntityId, Vec<u32>, [f64; 3])> = scene
        .owners
        .iter()
        .map(|o| {
            let mesh = engine.mesh(*o).expect("meshed");
            (*o, mesh.indices.clone(), [0.0, 0.0, 0.75])
        })
        .collect();

    let bytes = doc.save().expect("save");
    let mut loaded = Document::load(&bytes).expect("load");
    let mut fresh = factored_engine();
    fresh.evaluate_pending(&mut loaded);
    let updates = fresh.poll_updates(&loaded);
    assert!(updates.errors.is_empty(), "{:?}", updates.errors);
    assert_eq!(updates.meshes.len(), 4);
    for (owner, indices, translation) in original {
        let update = updates
            .meshes
            .iter()
            .find(|m| m.id == owner)
            .expect("owner re-meshed");
        assert_eq!(update.mesh.indices, indices, "identical local mesh");
        assert_eq!(translation_of(update.base_transform), translation);
    }
}

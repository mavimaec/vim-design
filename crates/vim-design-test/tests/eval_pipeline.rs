//! Pipeline behavior: incremental re-evaluation (only the edited chain
//! re-evaluates), settledness counters, the poll contract (changed-set
//! only, coalescing, tombstones), undo restoring meshes, and the
//! load-path seeding of a fresh engine.

use std::collections::BTreeMap;

use vim_design_lib::eval::Engine;
use vim_design_lib::{Command, Document, EntityId};
use vim_design_test::{
    build_cone, build_cube, build_cylinder, build_plate, mesh_volume, ok,
};

struct Scene {
    cube: vim_design_test::CubeFixture,
    plate: vim_design_test::PlateFixture,
    cylinder: Vec<EntityId>,
    cone: vim_design_test::ConeFixture,
}

fn build_scene(doc: &mut Document) -> Scene {
    Scene {
        cube: build_cube(doc, [0.0, 0.0, 0.0], 1.0, 1.0),
        plate: build_plate(doc, [3.0, 0.0, 0.0], 4.0, 3.0, 0.5, true),
        cylinder: build_cylinder(doc, [10.0, 0.0, 0.0], 0.5, 2.0),
        cone: build_cone(doc, [14.0, 0.0, 0.0], 1.5, 2.0),
    }
}

fn eval_counts(engine: &Engine, doc: &Document) -> BTreeMap<EntityId, u64> {
    doc.entities()
        .map(|(id, _)| (*id, engine.eval_count(*id)))
        .collect()
}

#[test]
fn editing_one_cube_corner_reevaluates_only_the_cube_chain() {
    let mut doc = Document::new();
    let mut engine = Engine::new();
    let scene = build_scene(&mut doc);

    engine.evaluate_pending(&mut doc);
    let _ = engine.poll_updates(&doc);
    let before = eval_counts(&engine, &doc);

    // Move one corner control point of the cube's base square.
    let corner = scene.cube.base.cps[1];
    ok(
        &mut doc,
        Command::UpdateControlPoint {
            id: corner,
            position: [1.2, 0.0, 0.0],
            coalesce: false,
        },
    );

    // Settledness: not settled until evaluated.
    assert!(doc.committed_generation() > engine.evaluated_generation());
    engine.evaluate_pending(&mut doc);
    assert_eq!(engine.evaluated_generation(), doc.committed_generation());

    let after = eval_counts(&engine, &doc);
    // Exactly the corner's downstream closure re-evaluated: the corner
    // itself, its two adjacent lines, their edges, wire, face, extrusion.
    let expected_dirty: Vec<EntityId> = {
        let base = &scene.cube.base;
        let mut ids = vec![corner];
        // lines touching cps[1]: line[0] (cps0->cps1) and line[1] (cps1->cps2)
        ids.push(base.lines[0]);
        ids.push(base.lines[1]);
        ids.push(base.edges[0]);
        ids.push(base.edges[1]);
        ids.push(base.wire);
        ids.push(scene.cube.face);
        ids.push(scene.cube.extrusion);
        ids
    };
    for (id, count_before) in &before {
        let count_after = after.get(id).copied().unwrap_or(0);
        if expected_dirty.contains(id) {
            assert_eq!(
                count_after,
                count_before + 1,
                "entity {id:?} should have re-evaluated exactly once"
            );
        } else {
            assert_eq!(
                count_after, *count_before,
                "entity {id:?} outside the cube chain must not re-evaluate"
            );
        }
    }

    // Only the cube's mesh was re-tessellated and re-delivered.
    let updates = engine.poll_updates(&doc);
    assert_eq!(updates.meshes.len(), 1);
    assert_eq!(updates.meshes[0].id, scene.cube.extrusion);
    assert!(updates.errors.is_empty());
    // The other owners were tessellated exactly once, ever.
    let cylinder_extrusion = *scene.cylinder.last().expect("extrusion last");
    assert_eq!(engine.tessellation_count(scene.plate.extrusion), 1);
    assert_eq!(engine.tessellation_count(cylinder_extrusion), 1);
    assert_eq!(engine.tessellation_count(scene.cone.revolve), 1);
    assert_eq!(engine.tessellation_count(scene.cube.extrusion), 2);
}

#[test]
fn poll_contract_changed_set_coalescing_and_tombstones() {
    let mut doc = Document::new();
    let mut engine = Engine::new();
    let cube = build_cube(&mut doc, [0.0, 0.0, 0.0], 1.0, 1.0);
    let cylinder = build_cylinder(&mut doc, [5.0, 0.0, 0.0], 0.5, 2.0);
    let cylinder_extrusion = *cylinder.last().expect("extrusion last");

    engine.evaluate_pending(&mut doc);
    let first = engine.poll_updates(&doc);
    assert_eq!(first.meshes.len(), 2);

    // Second poll with no edits: empty and settled.
    let second = engine.poll_updates(&doc);
    assert!(second.is_settled_and_empty(), "{second:?}");

    // Coalescing: two edits (evaluated separately) between polls arrive
    // as ONE mesh entry carrying the latest state.
    ok(
        &mut doc,
        Command::UpdateControlPoint {
            id: cube.path_cps[1],
            position: [0.0, 0.0, 2.0],
            coalesce: false,
        },
    );
    engine.evaluate_pending(&mut doc);
    ok(
        &mut doc,
        Command::UpdateControlPoint {
            id: cube.path_cps[1],
            position: [0.0, 0.0, 3.0],
            coalesce: false,
        },
    );
    engine.evaluate_pending(&mut doc);
    let updates = engine.poll_updates(&doc);
    assert_eq!(updates.meshes.len(), 1, "coalesced to one entry");
    assert_eq!(updates.meshes[0].id, cube.extrusion);
    let volume = mesh_volume(&updates.meshes[0].mesh);
    assert!(
        (volume - 3.0).abs() < 1e-9,
        "latest state wins (1x1x3 = 3 m^3): {volume}"
    );

    // Tombstones: deleting the cylinder composite removes its mesh.
    ok(
        &mut doc,
        Command::DeleteCylinder {
            extrusion: cylinder_extrusion,
        },
    );
    engine.evaluate_pending(&mut doc);
    let updates = engine.poll_updates(&doc);
    assert!(updates.meshes.is_empty());
    assert_eq!(updates.meshes_removed, vec![cylinder_extrusion]);

    // Undo of the delete brings the mesh back as a plain upsert.
    doc.undo().expect("undo delete cylinder");
    engine.evaluate_pending(&mut doc);
    let updates = engine.poll_updates(&doc);
    assert_eq!(updates.meshes.len(), 1);
    assert_eq!(updates.meshes[0].id, cylinder_extrusion);
    assert!(updates.meshes_removed.is_empty());
}

#[test]
fn delete_and_recreate_between_polls_is_a_plain_upsert() {
    let mut doc = Document::new();
    let mut engine = Engine::new();
    let cylinder = build_cylinder(&mut doc, [0.0, 0.0, 0.0], 0.5, 2.0);
    let extrusion = *cylinder.last().expect("extrusion last");
    engine.evaluate_pending(&mut doc);
    let _ = engine.poll_updates(&doc);

    // Delete, evaluate, undo, evaluate — all between two polls: the id
    // is live again, so no tombstone may surface (apply-order safety).
    ok(&mut doc, Command::DeleteCylinder { extrusion });
    engine.evaluate_pending(&mut doc);
    doc.undo().expect("undo");
    engine.evaluate_pending(&mut doc);

    let updates = engine.poll_updates(&doc);
    assert!(updates.meshes_removed.is_empty(), "live id never tombstoned");
    assert_eq!(updates.meshes.len(), 1);
    assert_eq!(updates.meshes[0].id, extrusion);
}

#[test]
fn undo_of_geometry_update_restores_the_previous_mesh() {
    let mut doc = Document::new();
    let mut engine = Engine::new();
    let cube = build_cube(&mut doc, [0.0, 0.0, 0.0], 1.0, 1.0);

    engine.evaluate_pending(&mut doc);
    let updates = engine.poll_updates(&doc);
    let original = updates.meshes[0].mesh.clone();

    ok(
        &mut doc,
        Command::UpdateControlPoint {
            id: cube.base.cps[2],
            position: [1.7, 1.3, 0.0],
            coalesce: false,
        },
    );
    engine.evaluate_pending(&mut doc);
    let updates = engine.poll_updates(&doc);
    let edited = updates.meshes[0].mesh.clone();
    assert!(
        (mesh_volume(&edited) - mesh_volume(&original)).abs() > 1e-6,
        "edit changed the geometry"
    );

    doc.undo().expect("undo update");
    engine.evaluate_pending(&mut doc);
    let updates = engine.poll_updates(&doc);
    let restored = updates.meshes[0].mesh.clone();

    // Deterministic evaluation (tenet 5): identical parametric state
    // yields the identical mesh.
    assert_eq!(restored.positions.len(), original.positions.len());
    assert_eq!(restored.indices, original.indices);
    assert!((mesh_volume(&restored) - mesh_volume(&original)).abs() < 1e-12);
}

#[test]
fn fresh_engine_on_loaded_document_seeds_everything() {
    let mut doc = Document::new();
    let mut engine = Engine::new();
    build_scene(&mut doc);
    engine.evaluate_pending(&mut doc);
    let baseline = engine.poll_updates(&doc);
    assert_eq!(baseline.meshes.len(), 4);

    // Save/load drops derived state and the dirty set; a fresh engine
    // must still evaluate the whole document on first contact.
    let bytes = doc.save().expect("save");
    let mut loaded = Document::load(&bytes).expect("load");
    let mut fresh = Engine::new();
    fresh.evaluate_pending(&mut loaded);
    let updates = fresh.poll_updates(&loaded);
    assert_eq!(updates.meshes.len(), 4, "all four objects re-meshed after load");
    assert_eq!(updates.evaluated_generation, loaded.committed_generation());
}

//! Golden-mesh assertions for the four acceptance objects (cube, floor
//! plate with hole, cylinder, cone): nonzero counts, watertightness,
//! bbox within tolerance of the analytic expectation, and volume sanity.
//! Also covers the mesh-ownership rule (standalone solids vs elements)
//! and submesh/material mapping.

use std::f64::consts::PI;

use vim_design_lib::eval::{Engine, Mesh};
use vim_design_lib::{Command, Document, EntityId};
use vim_design_test::{
    IDENTITY_XFORM, assert_bbox_near, assert_watertight, build_cone, build_cube,
    build_cylinder, build_plate, mesh_volume, one, ok, translation,
};

/// Evaluate everything and return the single mesh in the scene.
fn single_mesh(doc: &mut Document, engine: &mut Engine, owner: EntityId) -> Mesh {
    engine.evaluate_pending(doc);
    let updates = engine.poll_updates(doc);
    assert_eq!(updates.committed_generation, updates.evaluated_generation);
    assert_eq!(updates.pending_count, 0);
    assert!(updates.errors.is_empty(), "unexpected errors: {:?}", updates.errors);
    assert_eq!(updates.meshes.len(), 1, "expected exactly one mesh");
    let update = &updates.meshes[0];
    assert_eq!(update.id, owner, "mesh keyed by the standalone producer id");
    update.mesh.clone()
}

#[test]
fn cube_meshes_exactly() {
    let mut doc = Document::new();
    let mut engine = Engine::new();
    let cube = build_cube(&mut doc, [0.0, 0.0, 0.0], 1.0, 1.0);
    let mesh = single_mesh(&mut doc, &mut engine, cube.extrusion);

    assert!(mesh.vertex_count() > 0);
    assert_eq!(mesh.triangle_count(), 12, "a cube is 12 triangles");
    assert_eq!(mesh.positions.len(), mesh.normals.len());
    assert_bbox_near(&mesh, [0.0, 0.0, 0.0], [1.0, 1.0, 1.0], 1e-9);
    assert!((mesh_volume(&mesh) - 1.0).abs() < 1e-9, "unit cube volume");
    assert_watertight(&mesh);

    // No material wired: one default submesh covering the whole buffer.
    assert_eq!(mesh.submeshes.len(), 1);
    assert_eq!(mesh.submeshes[0].material, None);
    assert_eq!(mesh.submeshes[0].index_start, 0);
    assert_eq!(mesh.submeshes[0].index_count as usize, mesh.indices.len());
}

#[test]
fn plate_with_hole_proof() {
    // Same footprint with and without the hole: bbox identical, more
    // triangles with the hole, and exactly the hole's volume missing.
    let (width, depth, thickness) = (4.0, 3.0, 0.5);

    let mut doc_hole = Document::new();
    let mut engine_hole = Engine::new();
    let plate_hole = build_plate(&mut doc_hole, [0.0; 3], width, depth, thickness, true);
    let with_hole = single_mesh(&mut doc_hole, &mut engine_hole, plate_hole.extrusion);

    let mut doc_full = Document::new();
    let mut engine_full = Engine::new();
    let plate_full = build_plate(&mut doc_full, [0.0; 3], width, depth, thickness, false);
    let without_hole = single_mesh(&mut doc_full, &mut engine_full, plate_full.extrusion);

    assert!(
        with_hole.triangle_count() > without_hole.triangle_count(),
        "hole adds triangles: {} vs {}",
        with_hole.triangle_count(),
        without_hole.triangle_count()
    );
    assert_bbox_near(&with_hole, [0.0; 3], [width, depth, thickness], 1e-9);
    assert_bbox_near(&without_hole, [0.0; 3], [width, depth, thickness], 1e-9);

    let full_volume = width * depth * thickness;
    let hole_volume = 1.0 * 1.0 * thickness;
    assert!((mesh_volume(&without_hole) - full_volume).abs() < 1e-9);
    assert!(
        (mesh_volume(&with_hole) - (full_volume - hole_volume)).abs() < 1e-9,
        "hole removes exactly its volume: got {}",
        mesh_volume(&with_hole)
    );
    assert_watertight(&with_hole);
    assert_watertight(&without_hole);
}

#[test]
fn cylinder_meshes_within_tolerance() {
    let (radius, height) = (0.5, 2.0);
    let mut doc = Document::new();
    let mut engine = Engine::new();
    let created = build_cylinder(&mut doc, [0.0, 0.0, 0.0], radius, height);
    let extrusion = *created.last().expect("extrusion last");
    let mesh = single_mesh(&mut doc, &mut engine, extrusion);

    assert!(mesh.triangle_count() > 0);
    // Chordal deviation 1 mm: mesh vertices lie ON the cylinder, so the
    // bbox is within [exact - chordal, exact].
    let chordal = doc.settings().chordal_tolerance;
    assert_bbox_near(
        &mesh,
        [-radius, -radius, 0.0],
        [radius, radius, height],
        chordal + 1e-9,
    );
    let exact = PI * radius * radius * height;
    let volume = mesh_volume(&mesh);
    assert!(volume > 0.0, "outward orientation");
    assert!(
        (volume - exact).abs() / exact < 0.01,
        "cylinder volume within 1%: {volume} vs {exact}"
    );
    assert_watertight(&mesh);
}

#[test]
fn cone_meshes_within_tolerance() {
    let (radius, height) = (1.5, 2.0);
    let mut doc = Document::new();
    let mut engine = Engine::new();
    let cone = build_cone(&mut doc, [0.0, 0.0, 0.0], radius, height);
    let mesh = single_mesh(&mut doc, &mut engine, cone.revolve);

    assert!(mesh.triangle_count() > 0);
    let chordal = doc.settings().chordal_tolerance;
    assert_bbox_near(
        &mesh,
        [-radius, -radius, 0.0],
        [radius, radius, height],
        chordal + 1e-9,
    );
    let exact = PI * radius * radius * height / 3.0;
    let volume = mesh_volume(&mesh);
    assert!(volume > 0.0, "outward orientation");
    assert!(
        (volume - exact).abs() / exact < 0.01,
        "cone volume within 1%: {volume} vs {exact}"
    );
    assert_watertight(&mesh);
}

#[test]
fn four_object_scene_meshes_all_owners() {
    let mut doc = Document::new();
    let mut engine = Engine::new();
    let cube = build_cube(&mut doc, [0.0, 0.0, 0.0], 1.0, 1.0);
    let plate = build_plate(&mut doc, [3.0, 0.0, 0.0], 4.0, 3.0, 0.5, true);
    let cylinder = build_cylinder(&mut doc, [10.0, 0.0, 0.0], 0.5, 2.0);
    let cone = build_cone(&mut doc, [14.0, 0.0, 0.0], 1.5, 2.0);
    let cylinder_extrusion = *cylinder.last().expect("extrusion last");

    engine.evaluate_pending(&mut doc);
    let updates = engine.poll_updates(&doc);
    assert!(updates.errors.is_empty(), "{:?}", updates.errors);
    let mut owners: Vec<EntityId> = updates.meshes.iter().map(|m| m.id).collect();
    owners.sort();
    let mut expected = vec![
        cube.extrusion,
        plate.extrusion,
        cylinder_extrusion,
        cone.revolve,
    ];
    expected.sort();
    assert_eq!(owners, expected);
    for update in &updates.meshes {
        assert!(update.mesh.triangle_count() > 0);
        assert_watertight(&update.mesh);
    }
}

#[test]
fn element_absorbs_standalone_mesh_and_materials_map_to_submeshes() {
    let mut doc = Document::new();
    let mut engine = Engine::new();
    let cube = build_cube(&mut doc, [0.0, 0.0, 0.0], 1.0, 1.0);

    // Give the profile face a material: the solid inherits it.
    let material = one(
        &mut doc,
        Command::CreateMaterial {
            name: "steel".to_owned(),
            color: [0.6, 0.6, 0.7],
            roughness: 0.4,
        },
    );
    ok(
        &mut doc,
        Command::UpdateFaceMaterial {
            face: cube.face,
            material: Some(material),
        },
    );

    // First: standalone mesh keyed by the extrusion.
    engine.evaluate_pending(&mut doc);
    let updates = engine.poll_updates(&doc);
    assert_eq!(updates.meshes.len(), 1);
    assert_eq!(updates.meshes[0].id, cube.extrusion);
    assert_eq!(updates.meshes[0].mesh.submeshes[0].material, Some(material));

    // Wrap it into an element with two instances: the standalone mesh is
    // tombstoned and the geometry re-delivered under the element id.
    let level = one(
        &mut doc,
        Command::CreateLevel {
            name: "Ground".to_owned(),
            elevation_m: 0.0,
            is_building_story: true,
            color: [0.2, 0.5, 0.9, 0.35],
            extent_m: 10.0,
        },
    );
    let element = one(
        &mut doc,
        Command::CreateElement {
            name: "cube".to_owned(),
            members: vec![cube.extrusion],
            level,
        },
    );
    let instances = [
        one(
            &mut doc,
            Command::CreateInstance {
                element,
                transform: IDENTITY_XFORM,
            },
        ),
        one(
            &mut doc,
            Command::CreateInstance {
                element,
                transform: translation(2.0, 0.0, 0.0),
            },
        ),
    ];
    engine.evaluate_pending(&mut doc);
    let updates = engine.poll_updates(&doc);

    assert_eq!(updates.meshes_removed, vec![cube.extrusion], "standalone tombstoned");
    assert_eq!(updates.meshes.len(), 1);
    assert_eq!(updates.meshes[0].id, element);
    let mesh = &updates.meshes[0].mesh;
    assert_eq!(mesh.triangle_count(), 12);
    assert_eq!(mesh.submeshes.len(), 1, "one submesh per member");
    assert_eq!(mesh.submeshes[0].material, Some(material));

    let mut got: Vec<EntityId> = updates.instances.iter().map(|i| i.id).collect();
    got.sort();
    let mut want = instances.to_vec();
    want.sort();
    assert_eq!(got, want);
    for instance in &updates.instances {
        assert_eq!(instance.element_id, element);
    }

    // Deleting an instance arrives as an instance tombstone.
    ok(&mut doc, Command::DeleteInstance { id: instances[1] });
    engine.evaluate_pending(&mut doc);
    let updates = engine.poll_updates(&doc);
    assert_eq!(updates.instances_removed, vec![instances[1]]);
    assert!(updates.meshes.is_empty(), "mesh unchanged by instance delete");
}

/// Not an assertion — prints tessellation statistics at the default 1 mm
/// chordal tolerance for the four acceptance objects. Run explicitly:
/// `cargo test -p vim-design-test --test eval_meshes -- --ignored --nocapture`
#[test]
#[ignore = "diagnostic printout, not a regression test"]
fn print_mesh_stats_at_default_tolerance() {
    let mut doc = Document::new();
    let mut engine = Engine::new();
    let cube = build_cube(&mut doc, [0.0, 0.0, 0.0], 1.0, 1.0);
    let plate = build_plate(&mut doc, [3.0, 0.0, 0.0], 4.0, 3.0, 0.5, true);
    let cylinder = build_cylinder(&mut doc, [10.0, 0.0, 0.0], 0.5, 2.0);
    let cone = build_cone(&mut doc, [14.0, 0.0, 0.0], 1.5, 2.0);
    let cylinder_extrusion = *cylinder.last().expect("extrusion last");
    engine.evaluate_pending(&mut doc);
    for (name, id) in [
        ("cube 1x1x1", cube.extrusion),
        ("plate 4x3x0.5 + 1x1 hole", plate.extrusion),
        ("cylinder r=0.5 h=2", cylinder_extrusion),
        ("cone r=1.5 h=2", cone.revolve),
    ] {
        let mesh = engine.mesh(id).expect("meshed");
        println!(
            "{name}: vertices={} triangles={} volume={:.6}",
            mesh.vertex_count(),
            mesh.triangle_count(),
            mesh_volume(mesh)
        );
    }
}

//! SubRef resolution + per-face materials (docs/ARCHITECTURE.md §3.4).
//!
//! The load-bearing test here is the anti-topological-naming regression:
//! paint one lateral face of an extruded plate by its provenance name
//! (`Side { source: edge }`), then change the kernel's face ordering by
//! editing upstream — the SAME semantic face must stay painted.

use vim_design_lib::eval::{Engine, EvalErrorKind, SubRefResolution};
use vim_design_lib::{
    Command, Document, EntityId, FaceTarget, Mesh, ProvenancePath, SubRef,
};
use vim_design_test::{build_cone, build_cube, build_plate, one, ok};

fn red_material(doc: &mut Document) -> EntityId {
    one(
        doc,
        Command::CreateMaterial {
            name: "red".to_owned(),
            color: [0.9, 0.1, 0.1],
            roughness: 0.5,
        },
    )
}

/// Vertex positions covered by the submesh with the given material.
fn submesh_positions(mesh: &Mesh, material: Option<EntityId>) -> Vec<[f32; 3]> {
    let mut points = Vec::new();
    for submesh in &mesh.submeshes {
        if submesh.material != material {
            continue;
        }
        let start = submesh.index_start as usize;
        let end = start + submesh.index_count as usize;
        for index in &mesh.indices[start..end] {
            points.push(mesh.positions[*index as usize]);
        }
    }
    points
}

/// Assert the `material` submesh covers exactly the plate's x = `x` side:
/// all its vertices on that plane, spanning the full y/z extent.
fn assert_painted_side_at_x(mesh: &Mesh, material: EntityId, x: f32, depth: f32, thickness: f32) {
    let painted = submesh_positions(mesh, Some(material));
    assert!(
        !painted.is_empty(),
        "painted submesh must exist and be non-empty"
    );
    let (mut min_y, mut max_y, mut min_z, mut max_z) =
        (f32::INFINITY, f32::NEG_INFINITY, f32::INFINITY, f32::NEG_INFINITY);
    for p in &painted {
        assert!(
            (p[0] - x).abs() < 1e-5,
            "painted vertex off the x={x} plane: {p:?}"
        );
        min_y = min_y.min(p[1]);
        max_y = max_y.max(p[1]);
        min_z = min_z.min(p[2]);
        max_z = max_z.max(p[2]);
    }
    assert!((min_y - 0.0).abs() < 1e-5 && (max_y - depth).abs() < 1e-5);
    assert!((min_z - 0.0).abs() < 1e-5 && (max_z - thickness).abs() < 1e-5);
}

#[test]
fn painting_a_generated_cap_splits_the_submeshes() {
    let mut doc = Document::new();
    let mut engine = Engine::new();
    let cube = build_cube(&mut doc, [0.0, 0.0, 0.0], 1.0, 1.0);
    let red = red_material(&mut doc);

    ok(
        &mut doc,
        Command::UpdateSubFaceMaterial {
            owner: cube.extrusion,
            target: FaceTarget::One(ProvenancePath::CapEnd),
            material: Some(red),
        },
    );
    engine.evaluate_pending(&mut doc);
    let updates = engine.poll_updates(&doc);
    assert!(updates.errors.is_empty(), "{:?}", updates.errors);
    let mesh = &updates.meshes[0].mesh;

    assert_eq!(mesh.triangle_count(), 12, "cube still 12 triangles");
    assert_eq!(mesh.submeshes.len(), 2, "default + painted: {:?}", mesh.submeshes);
    let painted = submesh_positions(mesh, Some(red));
    assert_eq!(painted.len(), 6, "top cap = 2 triangles");
    assert!(
        painted.iter().all(|p| (p[2] - 1.0).abs() < 1e-6),
        "painted vertices all on the top cap plane"
    );
    // The material is a real graph edge now: deleting it is rejected.
    assert!(doc.submit(Command::DeleteMaterial { id: red }).is_err());

    // Clearing the assignment restores the single default submesh.
    ok(
        &mut doc,
        Command::UpdateSubFaceMaterial {
            owner: cube.extrusion,
            target: FaceTarget::One(ProvenancePath::CapEnd),
            material: None,
        },
    );
    engine.evaluate_pending(&mut doc);
    let updates = engine.poll_updates(&doc);
    assert_eq!(updates.meshes[0].mesh.submeshes.len(), 1);
    assert!(doc.submit(Command::DeleteMaterial { id: red }).is_ok());
}

#[test]
fn anti_tnp_paint_survives_kernel_face_reordering() {
    let (width, depth, thickness) = (4.0, 3.0, 0.5);
    let mut doc = Document::new();
    let mut engine = Engine::new();
    let plate = build_plate(&mut doc, [0.0; 3], width, depth, thickness, false);
    let red = red_material(&mut doc);

    // Paint the EAST side (x = 4): the face swept from outer edge #1.
    let east_edge = plate.outer.edges[1];
    ok(
        &mut doc,
        Command::UpdateSubFaceMaterial {
            owner: plate.extrusion,
            target: FaceTarget::One(ProvenancePath::Side { source: east_edge }),
            material: Some(red),
        },
    );
    engine.evaluate_pending(&mut doc);
    let updates = engine.poll_updates(&doc);
    assert!(updates.errors.is_empty(), "{:?}", updates.errors);
    assert_painted_side_at_x(
        &updates.meshes[0].mesh,
        red,
        width as f32,
        depth as f32,
        thickness as f32,
    );

    // Perturbation 1 — add detail upstream: split the SOUTH edge (which
    // precedes the east edge in wire order) into two edges. The profile
    // now has 5 sides, so every kernel face after the split point shifts
    // position in the kernel's output face list (probe-verified order:
    // bottom cap, sides in wire order, top cap). An index-based binding
    // would now paint the wrong face.
    let mid = one(
        &mut doc,
        Command::CreateControlPoint {
            position: [width / 2.0, 0.0, 0.0],
        },
    );
    let south_a = one(
        &mut doc,
        Command::CreateLine {
            start: plate.outer.cps[0],
            end: mid,
        },
    );
    let south_b = one(
        &mut doc,
        Command::CreateLine {
            start: mid,
            end: plate.outer.cps[1],
        },
    );
    let edge_a = one(&mut doc, Command::CreateEdge { curve: south_a });
    let edge_b = one(&mut doc, Command::CreateEdge { curve: south_b });
    ok(
        &mut doc,
        Command::UpdateWire {
            id: plate.outer.wire,
            edges: vec![
                edge_a,
                edge_b,
                plate.outer.edges[1],
                plate.outer.edges[2],
                plate.outer.edges[3],
            ],
            coalesce: false,
        },
    );
    engine.evaluate_pending(&mut doc);
    let updates = engine.poll_updates(&doc);
    assert!(updates.errors.is_empty(), "{:?}", updates.errors);
    let mesh = &updates.meshes[0].mesh;
    // The face list genuinely changed shape (5 sides + 2 caps)...
    match engine.value(plate.extrusion) {
        Some(vim_design_lib::Evaluated::Solid { solid, .. }) => {
            assert_eq!(solid.face_count(), 7, "5 sides + 2 caps after the split");
        }
        other => panic!("extrusion value: {other:?}"),
    }
    // ...and the SAME semantic face is still the painted one.
    assert_painted_side_at_x(mesh, red, width as f32, depth as f32, thickness as f32);

    // Perturbation 2 — reverse the wire's edge order entirely (same
    // geometry, chaining auto-flips): face order changes again; the
    // paint must not move.
    ok(
        &mut doc,
        Command::UpdateWire {
            id: plate.outer.wire,
            edges: vec![
                plate.outer.edges[3],
                plate.outer.edges[2],
                plate.outer.edges[1],
                edge_b,
                edge_a,
            ],
            coalesce: false,
        },
    );
    engine.evaluate_pending(&mut doc);
    let updates = engine.poll_updates(&doc);
    assert!(updates.errors.is_empty(), "{:?}", updates.errors);
    assert_painted_side_at_x(
        &updates.meshes[0].mesh,
        red,
        width as f32,
        depth as f32,
        thickness as f32,
    );

    // Rewind the perturbations (reversal + the 6 split commands): the
    // plate is back to 4 sides and STILL painted.
    for _ in 0..7 {
        doc.undo().expect("undo perturbation step");
    }
    engine.evaluate_pending(&mut doc);
    let updates = engine.poll_updates(&doc);
    assert_painted_side_at_x(
        &updates.meshes[0].mesh,
        red,
        width as f32,
        depth as f32,
        thickness as f32,
    );

    // One more undo removes the paint itself: single default submesh.
    doc.undo().expect("undo paint");
    engine.evaluate_pending(&mut doc);
    let updates = engine.poll_updates(&doc);
    let mesh = &updates.meshes[0].mesh;
    assert_eq!(mesh.submeshes.len(), 1, "paint undone");
    assert_eq!(mesh.submeshes[0].material, None);
    assert_eq!(mesh.triangle_count(), 12);
}

#[test]
fn resolve_subref_follows_topology_changes() {
    let mut doc = Document::new();
    let mut engine = Engine::new();
    // Partial revolve (π): has both caps.
    let cone = build_cone(&mut doc, [0.0, 0.0, 0.0], 1.5, 2.0);
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
    engine.evaluate_pending(&mut doc);

    let sub = |path: ProvenancePath| SubRef {
        owner: cone.revolve,
        path,
    };
    // Caps and sides resolve on the half cone.
    assert_eq!(
        engine.resolve_subref(&sub(ProvenancePath::CapStart)),
        Ok(SubRefResolution::Faces(1))
    );
    assert_eq!(
        engine.resolve_subref(&sub(ProvenancePath::CapEnd)),
        Ok(SubRefResolution::Faces(1))
    );
    let hypotenuse = ProvenancePath::Side {
        source: cone.profile_edges[1],
    };
    assert!(matches!(
        engine.resolve_subref(&sub(hypotenuse.clone())),
        Ok(SubRefResolution::Faces(_))
    ));
    // A generated edge: cap-start meets the hypotenuse surface.
    let rim = ProvenancePath::shared_edge(ProvenancePath::CapStart, hypotenuse.clone());
    assert!(matches!(
        engine.resolve_subref(&sub(rim.clone())),
        Ok(SubRefResolution::Edges(_))
    ));

    // Close the revolve (2π): the caps VANISH. The references must fail
    // typed — never silently re-bind (§3.4).
    ok(
        &mut doc,
        Command::UpdateRevolve {
            id: cone.revolve,
            profile: None,
            axis: None,
            angle_radians: Some(std::f64::consts::TAU),
            coalesce: false,
        },
    );
    engine.evaluate_pending(&mut doc);
    assert_eq!(
        engine
            .resolve_subref(&sub(ProvenancePath::CapEnd))
            .err()
            .map(|d| d.kind),
        Some(EvalErrorKind::UnresolvedSubRef)
    );
    assert_eq!(
        engine.resolve_subref(&sub(rim)).err().map(|d| d.kind),
        Some(EvalErrorKind::UnresolvedSubRef)
    );
    // The sides still resolve (several kernel faces share the name on a
    // full revolve).
    assert!(matches!(
        engine.resolve_subref(&sub(hypotenuse)),
        Ok(SubRefResolution::Faces(n)) if n >= 1
    ));
    // A source edge id that never existed.
    assert_eq!(
        engine
            .resolve_subref(&sub(ProvenancePath::Side {
                source: EntityId(999_999)
            }))
            .err()
            .map(|d| d.kind),
        Some(EvalErrorKind::UnresolvedSubRef)
    );
}

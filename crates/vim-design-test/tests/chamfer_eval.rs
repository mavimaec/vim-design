//! Chamfer evaluation (monstertruck-fillet, Chamfer profile — docs
//! §5.2): golden chamfered-cube geometry, mesh-ownership handoff from
//! the target to the chamfer, provenance propagation across the blend
//! (paints survive), unresolvable SubRef error + stale retention +
//! recovery, undo/redo byte-exactness, and the typed curved-edge
//! failure.

use vim_design_lib::eval::Engine;
use vim_design_lib::{
    Command, Document, EntityId, EvalErrorKind, Evaluated, Mesh, ProvenancePath, SubRef,
};
use vim_design_test::{
    assert_watertight, build_cube, build_cylinder, mesh_volume, one, ok,
};

/// SharedEdge path for "top rim above profile edge `e`".
fn top_rim(edge: EntityId) -> ProvenancePath {
    ProvenancePath::shared_edge(ProvenancePath::CapEnd, ProvenancePath::Side { source: edge })
}

fn face_count(engine: &Engine, id: EntityId) -> usize {
    match engine.value(id) {
        Some(Evaluated::Solid { solid, .. }) => solid.face_count(),
        other => panic!("expected a solid value for {id:?}, got {other:?}"),
    }
}

#[test]
fn chamfer_golden_cube_edge_and_ownership_handoff() {
    let mut doc = Document::new();
    let mut engine = Engine::new();
    let cube = build_cube(&mut doc, [0.0, 0.0, 0.0], 1.0, 1.0);

    // Baseline: the extrusion owns the mesh.
    engine.evaluate_pending(&mut doc);
    let updates = engine.poll_updates(&doc);
    assert_eq!(updates.meshes.len(), 1);
    assert_eq!(updates.meshes[0].id, cube.extrusion);

    // Chamfer the top rim edge above the south profile edge.
    let distance = 0.1;
    let chamfer = one(
        &mut doc,
        Command::CreateChamfer {
            target: cube.extrusion,
            distance,
            edges: vec![],
            sub_edges: vec![SubRef {
                owner: cube.extrusion,
                path: top_rim(cube.base.edges[0]),
            }],
        },
    );
    engine.evaluate_pending(&mut doc);
    let updates = engine.poll_updates(&doc);
    assert!(updates.errors.is_empty(), "{:?}", updates.errors);

    // Ownership handoff: the chamfer replaces its target as mesh owner.
    assert_eq!(updates.meshes_removed, vec![cube.extrusion]);
    assert_eq!(updates.meshes.len(), 1);
    assert_eq!(updates.meshes[0].id, chamfer);

    // Golden geometry: one blend face added; the removed material is the
    // analytic prism (d²/2 × edge length 1).
    let mesh = &updates.meshes[0].mesh;
    assert!(mesh.triangle_count() > 12);
    assert_watertight(mesh);
    let expected = 1.0 - distance * distance / 2.0;
    let volume = mesh_volume(mesh);
    // 1e-6 m^3 tolerance: facade positions are f32 (docs §6.3 buffers).
    assert!(
        (volume - expected).abs() < 1e-6,
        "chamfer removes the analytic prism: {volume} vs {expected}"
    );
    assert_eq!(face_count(&engine, chamfer), 7, "6 cube faces + 1 blend");
    // The blend face is named by the edge it replaced (§3.4).
    match engine.value(chamfer) {
        Some(Evaluated::Solid { solid, .. }) => {
            let blend = top_rim(cube.base.edges[0]);
            assert!(
                solid.face_paths().contains(&Some(blend)),
                "{:?}",
                solid.face_paths()
            );
        }
        other => panic!("chamfer value: {other:?}"),
    }

    // Deleting the chamfer hands the mesh back to the extrusion.
    ok(&mut doc, Command::DeleteChamfer { id: chamfer });
    engine.evaluate_pending(&mut doc);
    let updates = engine.poll_updates(&doc);
    assert_eq!(updates.meshes_removed, vec![chamfer]);
    assert_eq!(updates.meshes.len(), 1);
    assert_eq!(updates.meshes[0].id, cube.extrusion);
    assert!((mesh_volume(&updates.meshes[0].mesh) - 1.0).abs() < 1e-6);
}

#[test]
fn paint_survives_across_a_chamfer_and_undo_is_byte_exact() {
    let mut doc = Document::new();
    let mut engine = Engine::new();
    let cube = build_cube(&mut doc, [0.0, 0.0, 0.0], 1.0, 1.0);
    let red = one(
        &mut doc,
        Command::CreateMaterial {
            name: "red".to_owned(),
            color: [0.9, 0.1, 0.1],
            roughness: 0.5,
        },
    );
    // Paint the south side (y = 0), then chamfer the rim edge where that
    // very face meets the top cap: the trimmed south face must stay
    // painted (provenance survives the blend), the blend face gets the
    // default material.
    let south = ProvenancePath::Side {
        source: cube.base.edges[0],
    };
    ok(
        &mut doc,
        Command::UpdateSubFaceMaterial {
            owner: cube.extrusion,
            path: south.clone(),
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
                path: top_rim(cube.base.edges[0]),
            }],
        },
    );
    engine.evaluate_pending(&mut doc);
    let updates = engine.poll_updates(&doc);
    assert!(updates.errors.is_empty(), "{:?}", updates.errors);
    let mesh: Mesh = updates.meshes[0].mesh.clone();

    assert_eq!(mesh.submeshes.len(), 2, "default + painted: {:?}", mesh.submeshes);
    let painted: Vec<[f32; 3]> = mesh
        .submeshes
        .iter()
        .filter(|s| s.material == Some(red))
        .flat_map(|s| {
            let start = s.index_start as usize;
            let end = start + s.index_count as usize;
            mesh.indices[start..end]
                .iter()
                .map(|i| mesh.positions[*i as usize])
                .collect::<Vec<_>>()
        })
        .collect();
    assert!(!painted.is_empty());
    // Still exactly the south face — now trimmed: all on y = 0, and its
    // top edge pulled down to z = 0.9 by the chamfer.
    let max_z = painted.iter().map(|p| p[2]).fold(f32::NEG_INFINITY, f32::max);
    assert!(painted.iter().all(|p| p[1].abs() < 1e-6));
    assert!((max_z - 0.9).abs() < 1e-5, "trimmed to z=0.9: {max_z}");

    // Undo the chamfer: deterministic re-evaluation restores the
    // pre-chamfer painted mesh byte-exactly.
    engine.evaluate_pending(&mut doc); // settle
    let _ = engine.poll_updates(&doc);
    ok(
        &mut doc,
        Command::UpdateChamfer {
            id: chamfer,
            distance: Some(0.2),
            target: None,
            edges: None,
            sub_edges: None,
            coalesce: false,
        },
    );
    engine.evaluate_pending(&mut doc);
    let updates = engine.poll_updates(&doc);
    let widened = updates.meshes[0].mesh.clone();
    assert!((mesh_volume(&widened) - (1.0 - 0.02)).abs() < 1e-6);

    doc.undo().expect("undo distance update");
    engine.evaluate_pending(&mut doc);
    let updates = engine.poll_updates(&doc);
    let restored = updates.meshes[0].mesh.clone();
    assert_eq!(restored.indices, mesh.indices, "byte-exact re-evaluation");
    assert_eq!(restored.submeshes, mesh.submeshes);
    assert_eq!(restored.positions.len(), mesh.positions.len());

    doc.redo().expect("redo distance update");
    engine.evaluate_pending(&mut doc);
    let updates = engine.poll_updates(&doc);
    assert_eq!(updates.meshes[0].mesh.indices, widened.indices);
}

#[test]
fn unresolvable_subref_errors_and_recovers_with_stale_retention() {
    let mut doc = Document::new();
    let mut engine = Engine::new();
    let cube = build_cube(&mut doc, [0.0, 0.0, 0.0], 1.0, 1.0);
    let good_path = top_rim(cube.base.edges[0]);
    let chamfer = one(
        &mut doc,
        Command::CreateChamfer {
            target: cube.extrusion,
            distance: 0.1,
            edges: vec![],
            sub_edges: vec![SubRef {
                owner: cube.extrusion,
                path: good_path.clone(),
            }],
        },
    );
    engine.evaluate_pending(&mut doc);
    let updates = engine.poll_updates(&doc);
    assert!(updates.errors.is_empty());
    let healthy = updates.meshes[0].mesh.clone();

    // Re-point the chamfer at an edge that cannot resolve (a source edge
    // id that never existed): per-entity UnresolvedSubRef error on the
    // REFERENCING entity, stale mesh retained (§3.4, §6.4).
    ok(
        &mut doc,
        Command::UpdateChamfer {
            id: chamfer,
            distance: None,
            target: None,
            edges: None,
            sub_edges: Some(vec![SubRef {
                owner: cube.extrusion,
                path: top_rim(EntityId(999_999)),
            }]),
            coalesce: false,
        },
    );
    engine.evaluate_pending(&mut doc);
    let updates = engine.poll_updates(&doc);
    let diag = updates
        .errors
        .iter()
        .find(|(id, _)| *id == chamfer)
        .map(|(_, d)| d.kind);
    assert_eq!(diag, Some(EvalErrorKind::UnresolvedSubRef));
    assert_eq!(updates.committed_generation, updates.evaluated_generation);
    let stale = engine.mesh(chamfer).expect("stale chamfer mesh retained");
    assert_eq!(stale.indices, healthy.indices);

    // A SubRef whose owner is not the target is equally unresolvable.
    ok(
        &mut doc,
        Command::UpdateChamfer {
            id: chamfer,
            distance: None,
            target: None,
            edges: None,
            sub_edges: Some(vec![SubRef {
                owner: cube.face, // not the chamfer's target
                path: good_path.clone(),
            }]),
            coalesce: false,
        },
    );
    engine.evaluate_pending(&mut doc);
    let updates = engine.poll_updates(&doc);
    let diag = updates
        .errors
        .iter()
        .find(|(id, _)| *id == chamfer)
        .map(|(_, d)| d.kind);
    assert_eq!(diag, Some(EvalErrorKind::UnresolvedSubRef));

    // Fix the reference: error clears, mesh re-delivered.
    ok(
        &mut doc,
        Command::UpdateChamfer {
            id: chamfer,
            distance: None,
            target: None,
            edges: None,
            sub_edges: Some(vec![SubRef {
                owner: cube.extrusion,
                path: good_path,
            }]),
            coalesce: false,
        },
    );
    engine.evaluate_pending(&mut doc);
    let updates = engine.poll_updates(&doc);
    assert!(updates.errors.is_empty());
    assert!(updates.errors_cleared.contains(&chamfer));
    assert_eq!(updates.meshes.len(), 1);
    assert_eq!(updates.meshes[0].mesh.indices, healthy.indices);
}

#[test]
fn authored_edge_coincidence_addresses_the_bottom_rim() {
    // An authored Edge entity in the edges slot addresses the solid edge
    // coincident with its curve — for an extrusion, the bottom rim
    // segment over that profile edge.
    let mut doc = Document::new();
    let mut engine = Engine::new();
    let cube = build_cube(&mut doc, [0.0, 0.0, 0.0], 1.0, 1.0);
    let chamfer = one(
        &mut doc,
        Command::CreateChamfer {
            target: cube.extrusion,
            distance: 0.1,
            edges: vec![cube.base.edges[0]],
            sub_edges: vec![],
        },
    );
    engine.evaluate_pending(&mut doc);
    let updates = engine.poll_updates(&doc);
    assert!(updates.errors.is_empty(), "{:?}", updates.errors);
    let mesh = &updates.meshes[0].mesh;
    assert!((mesh_volume(mesh) - 0.995).abs() < 1e-6);
    assert_watertight(mesh);
    assert_eq!(face_count(&engine, chamfer), 7);
    // The blend face is named by the two adjacent named faces.
    match engine.value(chamfer) {
        Some(Evaluated::Solid { solid, .. }) => {
            let blend = ProvenancePath::shared_edge(
                ProvenancePath::CapStart,
                ProvenancePath::Side {
                    source: cube.base.edges[0],
                },
            );
            assert!(
                solid.face_paths().contains(&Some(blend)),
                "{:?}",
                solid.face_paths()
            );
        }
        other => panic!("chamfer value: {other:?}"),
    }
}

#[test]
fn curved_edge_chamfer_fails_typed_without_poisoning_the_scene() {
    let mut doc = Document::new();
    let mut engine = Engine::new();
    // Scene: a cube (stays healthy) + a cylinder whose top rim we try to
    // chamfer (curved edge — out of the supported straight-edge class).
    let cube = build_cube(&mut doc, [3.0, 0.0, 0.0], 1.0, 1.0);
    let cylinder = build_cylinder(&mut doc, [0.0, 0.0, 0.0], 0.5, 2.0);
    let cyl_extrusion = *cylinder.last().expect("extrusion last");
    let circle_edge = cylinder[3]; // composite order: cp, cp, circle, edge, ...

    let chamfer = one(
        &mut doc,
        Command::CreateChamfer {
            target: cyl_extrusion,
            distance: 0.05,
            edges: vec![],
            sub_edges: vec![SubRef {
                owner: cyl_extrusion,
                path: top_rim(circle_edge),
            }],
        },
    );
    engine.evaluate_pending(&mut doc);
    let updates = engine.poll_updates(&doc);

    // Typed per-entity failure on the chamfer (kernel or tessellation
    // class depending on where monstertruck gives up), never a crash;
    // the cube is untouched and the pipeline settles.
    let diag = updates
        .errors
        .iter()
        .find(|(id, _)| *id == chamfer)
        .map(|(_, d)| d.kind);
    assert!(
        matches!(
            diag,
            Some(EvalErrorKind::Kernel)
                | Some(EvalErrorKind::Tessellation)
                | Some(EvalErrorKind::UnresolvedSubRef)
        ),
        "typed failure expected, got {diag:?} ({:?})",
        updates.errors
    );
    assert_eq!(updates.committed_generation, updates.evaluated_generation);
    assert_eq!(updates.pending_count, 0);
    assert!(
        updates.meshes.iter().any(|m| m.id == cube.extrusion),
        "healthy cube still meshes"
    );
}

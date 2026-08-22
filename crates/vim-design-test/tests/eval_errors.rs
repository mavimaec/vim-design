//! Per-entity evaluation errors (docs/ARCHITECTURE.md §6.4): stale-mesh
//! retention through a degenerate edit and recovery on fix, wire-closure
//! and non-planar-face violations that never poison the rest of the
//! scene, the NotYetImplemented kinds, and the documented spline-path
//! limitation of extrusions.

use vim_design_lib::eval::{Engine, EvalErrorKind, EvalState};
use vim_design_lib::{Command, Document, EntityKind, PredicateAst, SelectionScope};
use vim_design_test::{build_cone, build_cube, build_cylinder, mesh_volume, one, ok};

#[test]
fn degenerate_cone_profile_retains_mesh_and_recovers_on_fix() {
    let mut doc = Document::new();
    let mut engine = Engine::new();
    let cone = build_cone(&mut doc, [0.0, 0.0, 0.0], 1.5, 2.0);

    engine.evaluate_pending(&mut doc);
    let updates = engine.poll_updates(&doc);
    assert!(updates.errors.is_empty());
    let healthy = updates.meshes[0].mesh.clone();
    let healthy_volume = mesh_volume(&healthy);

    // Degenerate: move the rim control point onto the axis — the profile
    // triangle collapses to a zero-area sliver (all three points on the
    // z-axis), which admits no plane.
    ok(
        &mut doc,
        Command::UpdateControlPoint {
            id: cone.rim_cp,
            position: [0.0, 0.0, 1.0],
            coalesce: false,
        },
    );
    engine.evaluate_pending(&mut doc);
    let updates = engine.poll_updates(&doc);

    // The failure surfaces on the face (the entity whose evaluation
    // failed), typed NotPlanar; the pipeline still settles.
    let error_ids: Vec<_> = updates.errors.iter().map(|(id, _)| *id).collect();
    assert!(
        error_ids.contains(&cone.face),
        "face must be in error: {:?}",
        updates.errors
    );
    let face_diag = updates
        .errors
        .iter()
        .find(|(id, _)| *id == cone.face)
        .map(|(_, diag)| diag.kind);
    assert_eq!(face_diag, Some(EvalErrorKind::NotPlanar));
    assert_eq!(updates.committed_generation, updates.evaluated_generation);

    // Stale retention: the face keeps its last successful value, so the
    // revolve (evaluating against the stale face) still has its mesh —
    // with the same geometry as before the bad edit.
    let mesh_now = engine.mesh(cone.revolve).expect("mesh retained");
    assert_eq!(mesh_now.indices.len(), healthy.indices.len());
    assert!((mesh_volume(mesh_now) - healthy_volume).abs() < 1e-9);
    assert!(matches!(
        engine.state(cone.face),
        Some(EvalState::Error {
            stale_generation: Some(_),
            ..
        })
    ));

    // Fix the parameter: the error clears and the mesh updates.
    ok(
        &mut doc,
        Command::UpdateControlPoint {
            id: cone.rim_cp,
            position: [2.0, 0.0, 0.0], // new radius 2.0
            coalesce: false,
        },
    );
    engine.evaluate_pending(&mut doc);
    let updates = engine.poll_updates(&doc);
    assert!(updates.errors.is_empty());
    assert!(updates.errors_cleared.contains(&cone.face), "{updates:?}");
    let recovered = updates
        .meshes
        .iter()
        .find(|m| m.id == cone.revolve)
        .expect("revolve re-meshed");
    let expected = std::f64::consts::PI * 2.0 * 2.0 * 2.0 / 3.0;
    let volume = mesh_volume(&recovered.mesh);
    assert!(
        (volume - expected).abs() / expected < 0.01,
        "recovered cone has the new radius: {volume} vs {expected}"
    );
}

#[test]
fn wire_closure_violation_is_per_entity_and_does_not_poison_the_scene() {
    let mut doc = Document::new();
    let mut engine = Engine::new();

    // A healthy cylinder shares the scene with a broken square: its
    // fourth edge is rewired to a line that leaves a gap.
    let cylinder = build_cylinder(&mut doc, [10.0, 0.0, 0.0], 0.5, 2.0);
    let cylinder_extrusion = *cylinder.last().expect("extrusion last");
    let cube = build_cube(&mut doc, [0.0, 0.0, 0.0], 1.0, 1.0);
    let far = one(
        &mut doc,
        Command::CreateControlPoint {
            position: [5.0, 5.0, 0.0],
        },
    );
    // Break the loop: last line now ends far away from the start.
    ok(
        &mut doc,
        Command::UpdateLine {
            id: cube.base.lines[3],
            start: None,
            end: Some(far),
            coalesce: false,
        },
    );

    engine.evaluate_pending(&mut doc);
    let updates = engine.poll_updates(&doc);

    // Wire fails typed; face/extrusion inherit as upstream errors (their
    // input never evaluated successfully, so there is nothing stale).
    let kind_of = |id| {
        updates
            .errors
            .iter()
            .find(|(e, _)| *e == id)
            .map(|(_, d)| d.kind)
    };
    assert_eq!(kind_of(cube.base.wire), Some(EvalErrorKind::WireNotClosed));
    assert_eq!(kind_of(cube.face), Some(EvalErrorKind::UpstreamError));
    assert_eq!(kind_of(cube.extrusion), Some(EvalErrorKind::UpstreamError));

    // The rest of the scene is untouched: the cylinder meshed fine and
    // the pipeline settled.
    assert_eq!(updates.meshes.len(), 1);
    assert_eq!(updates.meshes[0].id, cylinder_extrusion);
    assert_eq!(updates.committed_generation, updates.evaluated_generation);
    assert_eq!(updates.pending_count, 0);
}

#[test]
fn non_planar_face_is_a_typed_per_entity_error() {
    let mut doc = Document::new();
    let mut engine = Engine::new();
    let cube = build_cube(&mut doc, [0.0, 0.0, 0.0], 1.0, 1.0);

    // Lift one base corner out of plane: the wire still chains and
    // closes, but no plane fits the four corners.
    ok(
        &mut doc,
        Command::UpdateControlPoint {
            id: cube.base.cps[2],
            position: [1.0, 1.0, 0.4],
            coalesce: false,
        },
    );
    engine.evaluate_pending(&mut doc);
    let updates = engine.poll_updates(&doc);

    let face_diag = updates
        .errors
        .iter()
        .find(|(id, _)| *id == cube.face)
        .map(|(_, d)| d.kind);
    assert_eq!(face_diag, Some(EvalErrorKind::NotPlanar));
    // The wire itself is fine (closure is not planarity).
    assert!(matches!(
        engine.state(cube.base.wire),
        Some(EvalState::UpToDate { .. })
    ));
}

#[test]
fn not_yet_implemented_kinds_do_not_fail_the_pipeline() {
    let mut doc = Document::new();
    let mut engine = Engine::new();
    let cube = build_cube(&mut doc, [0.0, 0.0, 0.0], 1.0, 1.0);

    let selection = one(
        &mut doc,
        Command::CreateSelection {
            predicate: PredicateAst::KindIs(EntityKind::Edge),
            scope: SelectionScope::Global {
                kinds: vec![EntityKind::Edge],
            },
            frozen: false,
        },
    );
    let section_box = one(
        &mut doc,
        Command::CreateSectionBox {
            min: [-10.0; 3],
            max: [10.0; 3],
        },
    );
    // A chamfer fed by a Selection: the selection is NotYetImplemented,
    // so the chamfer inherits a clean upstream error (no crash, no
    // pipeline failure) — and the un-chamfered extrusion keeps its mesh
    // owner role only until the chamfer first succeeds (it never does
    // here, so the extrusion's mesh would move to the chamfer with no
    // geometry; the chamfer target keeps stale-retention semantics).
    let chamfer = one(
        &mut doc,
        Command::CreateChamfer {
            target: cube.extrusion,
            distance: 0.02,
            edges: vec![selection],
            sub_edges: vec![],
        },
    );

    engine.evaluate_pending(&mut doc);
    let updates = engine.poll_updates(&doc);

    for (id, expected) in [
        (selection, EvalErrorKind::NotYetImplemented),
        (section_box, EvalErrorKind::NotYetImplemented),
        (chamfer, EvalErrorKind::UpstreamError),
    ] {
        let diag = updates.errors.iter().find(|(e, _)| *e == id).map(|(_, d)| d.kind);
        assert_eq!(diag, Some(expected), "entity {id:?}");
    }
    // The pipeline settles despite the unevaluated kinds.
    assert_eq!(updates.committed_generation, updates.evaluated_generation);
    assert_eq!(updates.pending_count, 0);

    // And they stay quiet: the next poll is empty, not a repeating error.
    let second = engine.poll_updates(&doc);
    assert!(second.is_settled_and_empty(), "{second:?}");
}

#[test]
fn spline_extrusion_path_reports_not_yet_supported_and_retains_mesh() {
    let mut doc = Document::new();
    let mut engine = Engine::new();
    let cube = build_cube(&mut doc, [0.0, 0.0, 0.0], 1.0, 1.0);

    engine.evaluate_pending(&mut doc);
    let updates = engine.poll_updates(&doc);
    let healthy = updates.meshes[0].mesh.clone();

    // Rewire the extrusion path to a spline: structurally legal, cleanly
    // unsupported at evaluation time (documented v1 limitation).
    let spline_cps: Vec<_> = [[0.0, 0.0, 0.0], [0.5, 0.0, 1.0], [0.0, 0.0, 2.0]]
        .iter()
        .map(|p| one(&mut doc, Command::CreateControlPoint { position: *p }))
        .collect();
    let spline = one(
        &mut doc,
        Command::CreateSpline {
            control_points: spline_cps,
            degree: None,
            knots: None,
        },
    );
    ok(
        &mut doc,
        Command::UpdateExtrusion {
            id: cube.extrusion,
            profile: None,
            path: Some(spline),
            coalesce: false,
        },
    );
    engine.evaluate_pending(&mut doc);
    let updates = engine.poll_updates(&doc);

    let diag = updates
        .errors
        .iter()
        .find(|(id, _)| *id == cube.extrusion)
        .map(|(_, d)| d.kind);
    assert_eq!(diag, Some(EvalErrorKind::NotYetSupported));
    // Stale mesh retained (§6.4): the cube does not vanish.
    let mesh = engine.mesh(cube.extrusion).expect("stale mesh retained");
    assert_eq!(mesh.indices, healthy.indices);

    // Rewire back: the error clears and the mesh is re-delivered.
    ok(
        &mut doc,
        Command::UpdateExtrusion {
            id: cube.extrusion,
            profile: None,
            path: Some(cube.path),
            coalesce: false,
        },
    );
    engine.evaluate_pending(&mut doc);
    let updates = engine.poll_updates(&doc);
    assert!(updates.errors_cleared.contains(&cube.extrusion));
    assert_eq!(updates.meshes.len(), 1);
}

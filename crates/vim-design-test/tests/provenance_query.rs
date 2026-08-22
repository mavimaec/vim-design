//! ProvenanceQuery / SubRefSet — set-valued topological targeting in
//! provenance space (docs/ARCHITECTURE.md §3.5, tier 1).
//!
//! The point of the milestone is LIVENESS: a query re-expands against
//! the owner's current topology on every evaluation, so membership
//! follows upstream edits automatically (holes added later get painted
//! or chamfered without touching the consumer), and an empty expansion
//! is a valid no-op, never an error.

use vim_design_lib::eval::{Engine, QueryResolution};
use vim_design_lib::{
    CapId, Command, Document, EntityId, FaceTarget, Mesh, ProvenanceQuery, SubRefSet,
    WireFilter,
};
use vim_design_test::{
    RectLoop, assert_watertight, build_cone, build_cube, build_loop, build_plate,
    mesh_volume, one, ok,
};

fn set(owner: EntityId, query: ProvenanceQuery) -> SubRefSet {
    SubRefSet { owner, query }
}

fn rq(engine: &Engine, owner: EntityId, query: ProvenanceQuery) -> QueryResolution {
    engine
        .resolve_query(&set(owner, query))
        .expect("query must resolve structurally")
}

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

/// Vertex positions covered by submeshes with the given material.
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

/// Add a rectangular hole to a plate's face: 13 loop commands + the
/// UpdateFace rewire = 14 undo steps.
const ADD_HOLE_STEPS: usize = 14;
fn add_hole(
    doc: &mut Document,
    face: EntityId,
    existing_holes: &[EntityId],
    corners: &[[f64; 3]; 4],
) -> RectLoop {
    let hole = build_loop(doc, corners);
    let mut holes: Vec<EntityId> = existing_holes.to_vec();
    holes.push(hole.wire);
    ok(
        doc,
        Command::UpdateFace {
            id: face,
            outer: None,
            holes: Some(holes),
            plane: None,
            coalesce: false,
        },
    );
    hole
}

#[test]
fn expansion_counts_and_wire_role_discrimination() {
    let mut doc = Document::new();
    let mut engine = Engine::new();
    let cube = build_cube(&mut doc, [0.0, 0.0, 0.0], 1.0, 1.0);
    let plate = build_plate(&mut doc, [3.0, 0.0, 0.0], 4.0, 3.0, 0.5, true);
    let cone = build_cone(&mut doc, [12.0, 0.0, 0.0], 1.5, 2.0); // closed 2π
    engine.evaluate_pending(&mut doc);

    // Cube: 4 rim edges on each cap, 4 sides, 2 caps, 4 vertical edges.
    let e = cube.extrusion;
    let all = WireFilter::All;
    assert_eq!(
        rq(&engine, e, ProvenanceQuery::RimEdges { cap: CapId::End, wires: all }),
        QueryResolution { faces: 0, edges: 4 }
    );
    assert_eq!(
        rq(&engine, e, ProvenanceQuery::RimEdges { cap: CapId::Start, wires: all }),
        QueryResolution { faces: 0, edges: 4 }
    );
    // Wire-role discrimination: the cube has no holes.
    assert_eq!(
        rq(
            &engine,
            e,
            ProvenanceQuery::RimEdges { cap: CapId::End, wires: WireFilter::HolesOnly }
        ),
        QueryResolution { faces: 0, edges: 0 },
        "empty expansion is a VALID result, not an error"
    );
    assert_eq!(
        rq(&engine, e, ProvenanceQuery::SideFaces { wires: all }),
        QueryResolution { faces: 4, edges: 0 }
    );
    assert_eq!(
        rq(&engine, e, ProvenanceQuery::Caps),
        QueryResolution { faces: 2, edges: 0 }
    );
    assert_eq!(
        rq(
            &engine,
            e,
            ProvenanceQuery::VerticalEdges { wires: WireFilter::OuterOnly }
        ),
        QueryResolution { faces: 0, edges: 4 }
    );
    // Union deduplicates (both members expand to the same 4 rim edges)
    // and may mix face- and edge-valued members.
    assert_eq!(
        rq(
            &engine,
            e,
            ProvenanceQuery::Union(vec![
                ProvenanceQuery::RimEdges { cap: CapId::End, wires: all },
                ProvenanceQuery::RimEdges { cap: CapId::End, wires: WireFilter::OuterOnly },
                ProvenanceQuery::Caps,
            ])
        ),
        QueryResolution { faces: 2, edges: 4 }
    );

    // Plate with one hole: outer vs hole discrimination.
    let p = plate.extrusion;
    assert_eq!(
        rq(&engine, p, ProvenanceQuery::SideFaces { wires: WireFilter::HolesOnly }),
        QueryResolution { faces: 4, edges: 0 },
        "hole walls only"
    );
    assert_eq!(
        rq(&engine, p, ProvenanceQuery::SideFaces { wires: WireFilter::OuterOnly }),
        QueryResolution { faces: 4, edges: 0 }
    );
    assert_eq!(
        rq(&engine, p, ProvenanceQuery::SideFaces { wires: all }),
        QueryResolution { faces: 8, edges: 0 }
    );
    assert_eq!(
        rq(
            &engine,
            p,
            ProvenanceQuery::VerticalEdges { wires: WireFilter::HolesOnly }
        ),
        QueryResolution { faces: 0, edges: 4 }
    );
    assert_eq!(
        rq(
            &engine,
            p,
            ProvenanceQuery::RimEdges { cap: CapId::Start, wires: WireFilter::HolesOnly }
        ),
        QueryResolution { faces: 0, edges: 4 }
    );

    // Closed revolve: no caps, no rims — valid empty expansions.
    let c = cone.revolve;
    assert_eq!(rq(&engine, c, ProvenanceQuery::Caps), QueryResolution::default());
    assert_eq!(
        rq(&engine, c, ProvenanceQuery::RimEdges { cap: CapId::End, wires: all }),
        QueryResolution::default()
    );

    // Serde round-trip of a nested query (postcard, the wire format).
    let query = ProvenanceQuery::Union(vec![
        ProvenanceQuery::RimEdges { cap: CapId::End, wires: WireFilter::HolesOnly },
        ProvenanceQuery::VerticalEdges { wires: WireFilter::OuterOnly },
        ProvenanceQuery::Union(vec![ProvenanceQuery::Caps]),
    ]);
    let bytes = postcard::to_allocvec(&query).expect("serialize");
    let back: ProvenanceQuery = postcard::from_bytes(&bytes).expect("deserialize");
    assert_eq!(back, query);
    // Canonicalization sorts and dedups Union members.
    let messy = ProvenanceQuery::Union(vec![
        ProvenanceQuery::Caps,
        ProvenanceQuery::Caps,
        ProvenanceQuery::SideFaces { wires: all },
    ]);
    assert_eq!(
        messy.canonical(),
        ProvenanceQuery::Union(vec![
            ProvenanceQuery::SideFaces { wires: all },
            ProvenanceQuery::Caps,
        ])
        .canonical()
    );
}

#[test]
fn octagonal_prism_from_vertical_edges_set() {
    // Chamfer ALL four vertical edges of a cube in one set-driven step:
    // the edges are pairwise non-adjacent (no shared vertices), so the
    // kernel's corner-blending gap is not triggered.
    let mut doc = Document::new();
    let mut engine = Engine::new();
    let cube = build_cube(&mut doc, [0.0, 0.0, 0.0], 1.0, 1.0);
    let distance = 0.1;
    let chamfer = one(
        &mut doc,
        Command::CreateChamfer {
            target: cube.extrusion,
            distance,
            edges: vec![],
            sub_edges: vec![],
        },
    );
    ok(
        &mut doc,
        Command::UpdateChamferEdgeSets {
            id: chamfer,
            edge_sets: vec![set(
                cube.extrusion,
                ProvenanceQuery::VerticalEdges { wires: WireFilter::OuterOnly },
            )],
            coalesce: false,
        },
    );
    engine.evaluate_pending(&mut doc);
    let updates = engine.poll_updates(&doc);
    assert!(updates.errors.is_empty(), "{:?}", updates.errors);
    assert_eq!(updates.meshes.len(), 1);
    assert_eq!(updates.meshes[0].id, chamfer);
    let mesh = &updates.meshes[0].mesh;

    // Octagonal prism: 4 corner prisms of cross-section d²/2 removed.
    let expected = 1.0 - 4.0 * (distance * distance / 2.0);
    let volume = mesh_volume(mesh);
    assert!(
        (volume - expected).abs() < 1e-6,
        "octagonal prism volume: {volume} vs {expected}"
    );
    assert_watertight(mesh);
    // 4 trimmed sides + 4 blends + 2 octagonal caps.
    match engine.value(chamfer) {
        Some(vim_design_lib::Evaluated::Solid { solid, .. }) => {
            assert_eq!(solid.face_count(), 10);
            let blends = solid
                .face_paths()
                .iter()
                .filter(|p| {
                    matches!(p, Some(path) if path.is_edge()) // blend named by its SharedEdge
                })
                .count();
            assert_eq!(blends, 4, "{:?}", solid.face_paths());
        }
        other => panic!("chamfer value: {other:?}"),
    }
}

#[test]
fn rim_edges_set_on_a_cube_documents_the_corner_gap() {
    // RimEdges{End} on a cube expands to 4 edges that MEET AT CORNERS.
    // monstertruck-fillet has no corner blending; this test documents
    // the failure mode as typed, per-entity, non-poisoning (a custom
    // chamfer implementation to lift this is researched in parallel).
    let mut doc = Document::new();
    let mut engine = Engine::new();
    let cube = build_cube(&mut doc, [0.0, 0.0, 0.0], 1.0, 1.0);
    let healthy_cube = build_cube(&mut doc, [3.0, 0.0, 0.0], 1.0, 1.0);
    let chamfer = one(
        &mut doc,
        Command::CreateChamfer {
            target: cube.extrusion,
            distance: 0.1,
            edges: vec![],
            sub_edges: vec![],
        },
    );
    ok(
        &mut doc,
        Command::UpdateChamferEdgeSets {
            id: chamfer,
            edge_sets: vec![set(
                cube.extrusion,
                ProvenanceQuery::RimEdges { cap: CapId::End, wires: WireFilter::All },
            )],
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
    assert!(
        matches!(
            diag,
            Some(vim_design_lib::EvalErrorKind::Kernel)
                | Some(vim_design_lib::EvalErrorKind::Tessellation)
        ),
        "expected the typed corner-gap failure, got {diag:?} ({:?})",
        updates.errors
    );
    // Non-poisoning: the other cube meshes, the pipeline settles.
    assert!(updates.meshes.iter().any(|m| m.id == healthy_cube.extrusion));
    assert_eq!(updates.committed_generation, updates.evaluated_generation);
    assert_eq!(updates.pending_count, 0);
}

#[test]
fn liveness_hole_wall_paint_follows_holes() {
    let (width, depth, thickness) = (4.0, 3.0, 0.5);
    let mut doc = Document::new();
    let mut engine = Engine::new();
    let plate = build_plate(&mut doc, [0.0; 3], width, depth, thickness, true);
    let red = red_material(&mut doc);
    let hole1 = plate.hole.as_ref().expect("plate has a hole");

    // Paint "the hole walls" — as a live query, not concrete paths.
    ok(
        &mut doc,
        Command::UpdateSubFaceMaterial {
            owner: plate.extrusion,
            target: FaceTarget::Set(ProvenanceQuery::SideFaces {
                wires: WireFilter::HolesOnly,
            }),
            material: Some(red),
        },
    );
    engine.evaluate_pending(&mut doc);
    let updates = engine.poll_updates(&doc);
    assert!(updates.errors.is_empty(), "{:?}", updates.errors);
    let one_hole_mesh = updates.meshes[0].mesh.clone();
    let in_box = |p: &[f32; 3], min: [f32; 2], max: [f32; 2]| {
        p[0] >= min[0] - 1e-5
            && p[0] <= max[0] + 1e-5
            && p[1] >= min[1] - 1e-5
            && p[1] <= max[1] + 1e-5
    };
    let painted = submesh_positions(&one_hole_mesh, Some(red));
    assert!(!painted.is_empty());
    assert!(
        painted.iter().all(|p| in_box(p, [1.0, 1.0], [2.0, 2.0])),
        "red covers exactly hole 1's walls"
    );
    let one_hole_red_count = painted.len();
    assert_eq!(
        rq(
            &engine,
            plate.extrusion,
            ProvenanceQuery::SideFaces { wires: WireFilter::HolesOnly }
        ),
        QueryResolution { faces: 4, edges: 0 }
    );

    // LIVENESS: add a second hole through ordinary commands — nobody
    // touches the paint assignment, yet the new walls come out red.
    let _hole2 = add_hole(
        &mut doc,
        plate.face,
        &[hole1.wire],
        &[
            [2.6, 0.6, 0.0],
            [3.4, 0.6, 0.0],
            [3.4, 1.4, 0.0],
            [2.6, 1.4, 0.0],
        ],
    );
    engine.evaluate_pending(&mut doc);
    let updates = engine.poll_updates(&doc);
    assert!(updates.errors.is_empty(), "{:?}", updates.errors);
    let two_hole_mesh = updates.meshes[0].mesh.clone();
    let painted = submesh_positions(&two_hole_mesh, Some(red));
    assert!(
        painted.iter().all(|p| {
            in_box(p, [1.0, 1.0], [2.0, 2.0]) || in_box(p, [2.6, 0.6], [3.4, 1.4])
        }),
        "red covers exactly the two holes' walls"
    );
    assert!(
        painted.iter().any(|p| in_box(p, [2.6, 0.6], [3.4, 1.4])),
        "the NEW hole's walls are painted automatically"
    );
    assert!(painted.len() > one_hole_red_count);
    assert_eq!(
        rq(
            &engine,
            plate.extrusion,
            ProvenanceQuery::SideFaces { wires: WireFilter::HolesOnly }
        ),
        QueryResolution { faces: 8, edges: 0 }
    );
    // Expansion-level liveness for rim edges too.
    assert_eq!(
        rq(
            &engine,
            plate.extrusion,
            ProvenanceQuery::RimEdges { cap: CapId::End, wires: WireFilter::HolesOnly }
        ),
        QueryResolution { faces: 0, edges: 8 }
    );

    // Undo the whole add-hole chain: byte-exact revert of the mesh.
    for _ in 0..ADD_HOLE_STEPS {
        doc.undo().expect("undo add-hole step");
    }
    engine.evaluate_pending(&mut doc);
    let updates = engine.poll_updates(&doc);
    let reverted = &updates.meshes[0].mesh;
    assert_eq!(reverted.indices, one_hole_mesh.indices);
    assert_eq!(reverted.positions, one_hole_mesh.positions);
    assert_eq!(reverted.submeshes, one_hole_mesh.submeshes);
    assert_eq!(
        rq(
            &engine,
            plate.extrusion,
            ProvenanceQuery::RimEdges { cap: CapId::End, wires: WireFilter::HolesOnly }
        ),
        QueryResolution { faces: 0, edges: 4 }
    );

    // Redo the whole chain: byte-exact return of the two-hole state.
    for _ in 0..ADD_HOLE_STEPS {
        doc.redo().expect("redo add-hole step");
    }
    engine.evaluate_pending(&mut doc);
    let updates = engine.poll_updates(&doc);
    let redone = &updates.meshes[0].mesh;
    assert_eq!(redone.indices, two_hole_mesh.indices);
    assert_eq!(redone.positions, two_hole_mesh.positions);
    assert_eq!(redone.submeshes, two_hole_mesh.submeshes);
}

#[test]
fn empty_expansion_chamfer_is_a_pass_through_no_op() {
    let mut doc = Document::new();
    let mut engine = Engine::new();
    // Hole-less plate: RimEdges{HolesOnly} expands to nothing.
    let plate = build_plate(&mut doc, [0.0; 3], 4.0, 3.0, 0.5, false);
    engine.evaluate_pending(&mut doc);
    let updates = engine.poll_updates(&doc);
    let unchamfered = updates.meshes[0].mesh.clone();

    let chamfer = one(
        &mut doc,
        Command::CreateChamfer {
            target: plate.extrusion,
            distance: 0.05,
            edges: vec![],
            sub_edges: vec![],
        },
    );
    ok(
        &mut doc,
        Command::UpdateChamferEdgeSets {
            id: chamfer,
            edge_sets: vec![set(
                plate.extrusion,
                ProvenanceQuery::RimEdges { cap: CapId::End, wires: WireFilter::HolesOnly },
            )],
            coalesce: false,
        },
    );
    engine.evaluate_pending(&mut doc);
    let updates = engine.poll_updates(&doc);
    assert!(updates.errors.is_empty(), "empty expansion is a no-op: {:?}", updates.errors);
    // Ownership moved to the chamfer, but the geometry is IDENTICAL.
    assert_eq!(updates.meshes_removed, vec![plate.extrusion]);
    assert_eq!(updates.meshes.len(), 1);
    assert_eq!(updates.meshes[0].id, chamfer);
    assert_eq!(updates.meshes[0].mesh.indices, unchamfered.indices);
    assert_eq!(updates.meshes[0].mesh.positions, unchamfered.positions);

    // Add a hole: the expansion becomes non-empty (4 hole rim edges that
    // meet at corners) and the chamfer now actually attempts to blend.
    // The hole rim edges share vertices, so this runs into the same
    // documented kernel corner gap as `rim_edges_set_on_a_cube...`:
    // typed per-entity error, stale (pass-through) mesh retained,
    // pipeline settled. If a future kernel lifts the gap this assert
    // flips to a volume check.
    add_hole(
        &mut doc,
        plate.face,
        &[],
        &[
            [1.0, 1.0, 0.0],
            [2.0, 1.0, 0.0],
            [2.0, 2.0, 0.0],
            [1.0, 2.0, 0.0],
        ],
    );
    engine.evaluate_pending(&mut doc);
    let updates = engine.poll_updates(&doc);
    let diag = updates
        .errors
        .iter()
        .find(|(id, _)| *id == chamfer)
        .map(|(_, d)| d.kind);
    assert!(
        matches!(
            diag,
            Some(vim_design_lib::EvalErrorKind::Kernel)
                | Some(vim_design_lib::EvalErrorKind::Tessellation)
        ),
        "expected the documented corner-gap failure once the hole \
         appears, got {diag:?} ({:?})",
        updates.errors
    );
    assert_eq!(updates.committed_generation, updates.evaluated_generation);
    let stale = engine.mesh(chamfer).expect("stale pass-through mesh retained");
    assert_eq!(stale.indices, unchamfered.indices);
}

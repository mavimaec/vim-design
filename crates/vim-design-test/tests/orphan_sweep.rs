//! Element deletion orphan sweep + mandatory level association
//! (decision 2026-08-23, docs/AUTHORING.md §§2/4).
//!
//! The sweep is scoped reference-counting collection over the element's
//! construction-input closure: shared inputs survive by the ordinary
//! dependent rules (the crux test: two elements sharing corner points),
//! non-construction kinds (Site/Level/Material/Selection/Plane/Element/
//! Instance) are never collected, and everything happens inside the one
//! deletion command group.

use vim_design_lib::eval::Engine;
use vim_design_lib::{
    Command, Document, EntityId, EntityKind, PredicateAst, SelectionScope, VimStatus,
};
use vim_design_test::{build_cube, one, ok, save};

fn create_level(doc: &mut Document, name: &str, elevation_m: f64) -> EntityId {
    one(
        doc,
        Command::CreateLevel {
            name: name.to_owned(),
            elevation_m,
            is_building_story: true,
            color: [0.2, 0.5, 0.9, 0.35],
            extent_m: 10.0,
        },
    )
}

/// Two extruded boxes whose base squares SHARE two corner control
/// points (a party wall): everything else is exclusive per element.
struct SharedScene {
    shared_cps: [EntityId; 2],
    element_a: EntityId,
    element_b: EntityId,
    exclusive_a: Vec<EntityId>,
}

fn build_shared_scene(doc: &mut Document) -> SharedScene {
    let level = create_level(doc, "Ground", 0.0);
    // Shared edge x=1: corners (1,0) and (1,1) belong to both squares.
    let s0 = one(doc, Command::CreateControlPoint { position: [1.0, 0.0, 0.0] });
    let s1 = one(doc, Command::CreateControlPoint { position: [1.0, 1.0, 0.0] });
    let build_box = |doc: &mut Document, far_x: f64| -> (EntityId, Vec<EntityId>) {
        let f0 = one(doc, Command::CreateControlPoint { position: [far_x, 0.0, 0.0] });
        let f1 = one(doc, Command::CreateControlPoint { position: [far_x, 1.0, 0.0] });
        let corners = [s0, s1, f1, f0]; // shared edge + far edge, CCW-ish
        let lines: Vec<EntityId> = (0..4)
            .map(|i| {
                one(
                    doc,
                    Command::CreateLine {
                        start: corners[i],
                        end: corners[(i + 1) % 4],
                    },
                )
            })
            .collect();
        let edges: Vec<EntityId> = lines
            .iter()
            .map(|l| one(doc, Command::CreateEdge { curve: *l }))
            .collect();
        let wire = one(doc, Command::CreateWire { edges: edges.clone() });
        let face = one(
            doc,
            Command::CreateFace {
                outer: wire,
                holes: vec![],
                plane: None,
            },
        );
        let p0 = one(doc, Command::CreateControlPoint { position: [0.0, 0.0, 0.0] });
        let p1 = one(doc, Command::CreateControlPoint { position: [0.0, 0.0, 1.0] });
        let path = one(doc, Command::CreateLine { start: p0, end: p1 });
        let extrusion = one(doc, Command::CreateExtrusion { profile: face, path });
        let mut exclusive = vec![f0, f1, wire, face, p0, p1, path, extrusion];
        exclusive.extend(lines);
        exclusive.extend(edges);
        (extrusion, exclusive)
    };
    let (extrusion_a, exclusive_a) = build_box(doc, 0.0); // square x in [0,1]
    let (extrusion_b, _exclusive_b) = build_box(doc, 2.0); // square x in [1,2]
    let element_a = one(
        doc,
        Command::CreateElement {
            name: "A".to_owned(),
            members: vec![extrusion_a],
            level,
        },
    );
    let element_b = one(
        doc,
        Command::CreateElement {
            name: "B".to_owned(),
            members: vec![extrusion_b],
            level,
        },
    );
    let _ = level; // held alive by the elements' association edges
    SharedScene {
        shared_cps: [s0, s1],
        element_a,
        element_b,
        exclusive_a,
    }
}

#[test]
fn shared_input_refcounting_is_the_crux() {
    let mut doc = Document::new();
    let mut engine = Engine::new();
    let scene = build_shared_scene(&mut doc);
    engine.evaluate_pending(&mut doc);
    let updates = engine.poll_updates(&doc);
    assert!(updates.errors.is_empty(), "{:?}", updates.errors);
    let mesh_b_before = updates
        .meshes
        .iter()
        .find(|m| m.id == scene.element_b)
        .expect("element B meshes")
        .mesh
        .clone();

    // Delete A (sweep default): A's exclusive upstream is collected,
    // the shared party-wall corners survive (B's lines depend on them).
    let bytes_before_delete_a = save(&doc);
    let undo_before = doc.undo_depth();
    ok(
        &mut doc,
        Command::DeleteElement {
            id: scene.element_a,
            sweep_orphans: true,
        },
    );
    assert_eq!(doc.undo_depth(), undo_before + 1, "delete + sweep = ONE step");
    for id in &scene.exclusive_a {
        assert!(doc.entity(*id).is_none(), "exclusive input {id:?} swept");
    }
    for id in scene.shared_cps {
        assert!(doc.entity(id).is_some(), "shared corner {id:?} survives");
    }
    // B is untouched, its mesh byte-identical.
    engine.evaluate_pending(&mut doc);
    let updates = engine.poll_updates(&doc);
    assert!(updates.errors.is_empty(), "{:?}", updates.errors);
    let mesh_b_after = engine.mesh(scene.element_b).expect("B still meshed");
    assert_eq!(mesh_b_after.positions, mesh_b_before.positions);
    assert_eq!(mesh_b_after.indices, mesh_b_before.indices);

    // Undo restores A byte-exactly.
    doc.undo().expect("undo delete A");
    assert_eq!(save(&doc), bytes_before_delete_a);

    // Redo, then delete B too: the shared corners are now unreferenced
    // and get swept — zero geometry remains.
    doc.redo().expect("redo delete A");
    let bytes_before_delete_b = save(&doc);
    ok(
        &mut doc,
        Command::DeleteElement {
            id: scene.element_b,
            sweep_orphans: true,
        },
    );
    for id in scene.shared_cps {
        assert!(doc.entity(id).is_none(), "shared corner {id:?} swept with B");
    }
    let survivors: Vec<EntityKind> =
        doc.entities().map(|(_, r)| r.kind()).collect();
    assert_eq!(survivors, vec![EntityKind::Level], "only the level remains");
    doc.undo().expect("undo delete B");
    assert_eq!(save(&doc), bytes_before_delete_b);
    doc.debug_validate().expect("consistent after undo");
}

#[test]
fn sweep_opt_out_keeps_geometry_and_resurfaces_the_standalone_mesh() {
    let mut doc = Document::new();
    let mut engine = Engine::new();
    let level = create_level(&mut doc, "Ground", 0.0);
    let cube = build_cube(&mut doc, [0.0, 0.0, 0.0], 1.0, 1.0);
    let element = one(
        &mut doc,
        Command::CreateElement {
            name: "cube".to_owned(),
            members: vec![cube.extrusion],
            level,
        },
    );
    engine.evaluate_pending(&mut doc);
    let _ = engine.poll_updates(&doc);

    ok(
        &mut doc,
        Command::DeleteElement {
            id: element,
            sweep_orphans: false,
        },
    );
    assert!(doc.entity(cube.extrusion).is_some(), "geometry kept");
    engine.evaluate_pending(&mut doc);
    let updates = engine.poll_updates(&doc);
    assert!(updates.meshes_removed.contains(&element));
    assert!(
        updates.meshes.iter().any(|m| m.id == cube.extrusion),
        "standalone mesh resurfaces under the extrusion id"
    );
}

#[test]
fn non_construction_kinds_survive_the_sweep() {
    let mut doc = Document::new();
    let level = create_level(&mut doc, "Ground", 0.0);
    // Cube whose base corners are attached to the level, whose face has
    // a material and an explicit reference plane.
    let plane = one(
        &mut doc,
        Command::CreatePlane {
            origin: [0.0, 0.0, 0.0],
            normal: [0.0, 0.0, 1.0],
        },
    );
    let material = one(
        &mut doc,
        Command::CreateMaterial {
            name: "brick".to_owned(),
            color: [0.7, 0.3, 0.2],
            roughness: 0.8,
        },
    );
    let cube = build_cube(&mut doc, [0.0, 0.0, 0.0], 1.0, 1.0);
    for cp in &cube.base.cps {
        ok(
            &mut doc,
            Command::UpdateControlPointPlane {
                id: *cp,
                plane: Some(level),
                position: None,
            },
        );
    }
    ok(
        &mut doc,
        Command::UpdateFace {
            id: cube.face,
            outer: None,
            holes: None,
            plane: Some(Some(plane)),
            coalesce: false,
        },
    );
    ok(
        &mut doc,
        Command::UpdateFaceMaterial {
            face: cube.face,
            material: Some(material),
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

    ok(
        &mut doc,
        Command::DeleteElement {
            id: element,
            sweep_orphans: true,
        },
    );
    // The whole construction chain was their last geometric consumer —
    // and yet the excluded kinds survive.
    assert!(doc.entity(cube.face).is_none(), "face swept");
    assert!(doc.entity(cube.base.cps[0]).is_none(), "attached point swept");
    assert!(doc.entity(level).is_some(), "Level survives");
    assert!(doc.entity(plane).is_some(), "Plane (reference geometry) survives");
    assert!(doc.entity(material).is_some(), "Material survives");
    doc.debug_validate().expect("consistent after sweep");
}

#[test]
fn selection_scope_pins_geometry_through_a_sweep() {
    let mut doc = Document::new();
    let level = create_level(&mut doc, "Ground", 0.0);
    let cube = build_cube(&mut doc, [0.0, 0.0, 0.0], 1.0, 1.0);
    let pinned = cube.base.cps[0];
    // A selection scoped to one corner point: the scope is a real graph
    // edge (mirror slot), so the point is a dependent-pinned survivor —
    // by the ordinary rules, no sweep special case.
    let selection = one(
        &mut doc,
        Command::CreateSelection {
            predicate: PredicateAst::KindIs(EntityKind::ControlPoint),
            scope: SelectionScope::Entities(vec![pinned]),
            frozen: false,
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
    ok(
        &mut doc,
        Command::DeleteElement {
            id: element,
            sweep_orphans: true,
        },
    );
    assert!(doc.entity(pinned).is_some(), "selection scope pins the point");
    assert!(doc.entity(cube.base.cps[1]).is_none(), "unpinned corners swept");
    assert!(doc.entity(cube.face).is_none());
    assert_eq!(
        doc.dependents(pinned),
        Ok(vec![selection]),
        "the pin IS the dependent edge"
    );
}

#[test]
fn element_creation_requires_a_level() {
    let mut doc = Document::new();
    let cube = build_cube(&mut doc, [0.0, 0.0, 0.0], 1.0, 1.0);
    let bytes = save(&doc);

    // No Level exists: structurally impossible (there is no valid id to
    // pass; a bogus one fails existence).
    assert_eq!(
        doc.submit(Command::CreateElement {
            name: "wall".to_owned(),
            members: vec![cube.extrusion],
            level: EntityId(999_999),
        })
        .err(),
        Some(VimStatus::EntityNotFound)
    );
    // A non-level id fails the slot kind check.
    assert_eq!(
        doc.submit(Command::CreateElement {
            name: "wall".to_owned(),
            members: vec![cube.extrusion],
            level: cube.face,
        })
        .err(),
        Some(VimStatus::SlotKindMismatch)
    );
    assert_eq!(save(&doc), bytes, "rejections are byte-exact no-ops");

    // With a level: wired, and the pump reports the creation.
    let mut engine = Engine::new();
    engine.evaluate_pending(&mut doc);
    let _ = engine.poll_updates(&doc);
    let level = create_level(&mut doc, "Ground", 0.0);
    let element = one(
        &mut doc,
        Command::CreateElement {
            name: "wall".to_owned(),
            members: vec![cube.extrusion],
            level,
        },
    );
    assert_eq!(doc.dependents(level), Ok(vec![element]));
    engine.evaluate_pending(&mut doc);
    let updates = engine.poll_updates(&doc);
    assert!(updates.params_changed.contains(&element));
    assert!(updates.params_changed.contains(&level));
}

#[test]
fn level_cascade_leaves_zero_orphaned_geometry() {
    let mut doc = Document::new();
    let mut engine = Engine::new();
    // Site + two levels + a shared material: the survivors.
    let site = one(
        &mut doc,
        Command::CreateSite {
            latitude_deg: 45.5019,
            longitude_deg: -73.5674,
            elevation_m: 36.0,
            true_north_deg: 0.0,
        },
    );
    let ground = create_level(&mut doc, "Ground", 0.0);
    let level_2 = create_level(&mut doc, "Level 2", 3.0);
    let material = one(
        &mut doc,
        Command::CreateMaterial {
            name: "concrete".to_owned(),
            color: [0.7, 0.7, 0.65],
            roughness: 0.9,
        },
    );

    // Demo-like scene on Ground: four objects, all wrapped in elements
    // associated with Ground; the cube's corners attached to Ground; the
    // plate face painted.
    let plate = vim_design_test::build_plate(&mut doc, [0.0; 3], 4.0, 3.0, 0.5, true);
    ok(
        &mut doc,
        Command::UpdateFaceMaterial {
            face: plate.face,
            material: Some(material),
        },
    );
    let cube = build_cube(&mut doc, [6.0, 0.0, 0.0], 1.0, 1.0);
    for cp in &cube.base.cps {
        ok(
            &mut doc,
            Command::UpdateControlPointPlane {
                id: *cp,
                plane: Some(ground),
                position: None,
            },
        );
    }
    let cylinder = vim_design_test::build_cylinder(&mut doc, [9.0, 0.0, 0.0], 0.5, 2.0);
    let cyl_extrusion = *cylinder.last().expect("extrusion last");
    let cone = vim_design_test::build_cone(&mut doc, [12.0, 0.0, 0.0], 1.5, 2.0);
    let mut instances = Vec::new();
    for (name, member) in [
        ("plate", plate.extrusion),
        ("cube", cube.extrusion),
        ("cylinder", cyl_extrusion),
        ("cone", cone.revolve),
    ] {
        let element = one(
            &mut doc,
            Command::CreateElement {
                name: name.to_owned(),
                members: vec![member],
                level: ground,
            },
        );
        instances.push(one(
            &mut doc,
            Command::CreateInstance {
                element,
                transform: vim_design_test::IDENTITY_XFORM,
            },
        ));
    }
    engine.evaluate_pending(&mut doc);
    let updates = engine.poll_updates(&doc);
    assert!(updates.errors.is_empty(), "{:?}", updates.errors);
    assert_eq!(updates.meshes.len(), 4);

    // Cascade Ground: mandatory association pulls every element into the
    // dependent closure, and cascaded element deletions sweep — the
    // survivors are EXACTLY Site + Level 2 + Material.
    let bytes_before = save(&doc);
    let undo_before = doc.undo_depth();
    ok(&mut doc, Command::DeleteLevel { id: ground, cascade: true });
    assert_eq!(doc.undo_depth(), undo_before + 1, "one undo group");
    let mut survivors: Vec<(EntityId, EntityKind)> =
        doc.entities().map(|(id, r)| (*id, r.kind())).collect();
    survivors.sort();
    let mut expected = vec![
        (site, EntityKind::Site),
        (level_2, EntityKind::Level),
        (material, EntityKind::Material),
    ];
    expected.sort();
    assert_eq!(survivors, expected, "zero orphaned geometry");
    doc.debug_validate().expect("consistent after cascade");

    // One undo restores the whole scene byte-exactly; meshes come back.
    doc.undo().expect("undo cascade");
    assert_eq!(save(&doc), bytes_before);
    engine.evaluate_pending(&mut doc);
    let updates = engine.poll_updates(&doc);
    assert!(updates.errors.is_empty(), "{:?}", updates.errors);
    assert_eq!(updates.meshes.len(), 4, "all four objects re-meshed");
    for instance in &instances {
        assert!(updates.instances.iter().any(|i| i.id == *instance));
    }
}

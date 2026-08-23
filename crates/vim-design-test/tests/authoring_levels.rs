//! Phase A of the authoring tool (docs/AUTHORING.md §§1–5): Site
//! singleton, Level construction planes (Frame evaluation), control
//! point plane attachment with (u,v,w) interpretation, data-only element
//! association, and the DeleteLevel cascade as one undo group.

use std::collections::BTreeSet;

use vim_design_lib::eval::Engine;
use vim_design_lib::{
    Command, Document, EntityId, EntityKind, Evaluated, VimStatus,
};
use vim_design_test::{
    assert_save_load_roundtrip, build_cube, build_loop, mesh_volume, one, ok, save,
};

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

fn create_site(doc: &mut Document) -> Result<EntityId, VimStatus> {
    doc.submit(Command::CreateSite {
        latitude_deg: 45.5019,
        longitude_deg: -73.5674,
        elevation_m: 36.0,
        true_north_deg: 0.0,
    })
    .map(|out| out.created_ids.first().copied().unwrap_or(EntityId::INVALID))
}

/// A unit square profile whose four corners are ATTACHED to `level`
/// (coords are (u,v,0) in the level frame), extruded 1 m upward along an
/// unattached world-coordinate path line.
struct AttachedBox {
    corner_cps: Vec<EntityId>,
    face: EntityId,
    extrusion: EntityId,
    path_cps: [EntityId; 2],
}

fn build_attached_box(doc: &mut Document, level: EntityId) -> AttachedBox {
    let square = build_loop(
        doc,
        &[
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 1.0, 0.0],
            [0.0, 1.0, 0.0],
        ],
    );
    for cp in &square.cps {
        ok(
            doc,
            Command::UpdateControlPointPlane {
                id: *cp,
                plane: Some(level),
                position: None, // stored coords become (u, v, w) verbatim
            },
        );
    }
    let face = one(
        doc,
        Command::CreateFace {
            outer: square.wire,
            holes: vec![],
            plane: None,
        },
    );
    let path_cps = [
        one(doc, Command::CreateControlPoint { position: [0.0, 0.0, 0.0] }),
        one(doc, Command::CreateControlPoint { position: [0.0, 0.0, 1.0] }),
    ];
    let path = one(
        doc,
        Command::CreateLine {
            start: path_cps[0],
            end: path_cps[1],
        },
    );
    let extrusion = one(doc, Command::CreateExtrusion { profile: face, path });
    AttachedBox {
        corner_cps: square.cps,
        face,
        extrusion,
        path_cps,
    }
}

#[test]
fn site_is_a_singleton_enforced_at_the_delta_gate() {
    let mut doc = Document::new();
    let site = create_site(&mut doc).expect("first site");

    // Second create: rejected, byte-identical, nothing recorded.
    let bytes = save(&doc);
    let undo_depth = doc.undo_depth();
    let _ = doc.take_dirty(); // drain the first create's dirt
    let _ = doc.take_params_touched();
    assert_eq!(create_site(&mut doc).err(), Some(VimStatus::SingletonExists));
    assert_eq!(save(&doc), bytes, "rejection leaves the document untouched");
    assert_eq!(doc.undo_depth(), undo_depth);
    assert!(doc.dirty_set().is_empty());
    assert!(doc.params_touched().is_empty(), "pump untouched by rejection");

    // Update works and is metadata-only.
    ok(
        &mut doc,
        Command::UpdateSite {
            id: site,
            latitude_deg: None,
            longitude_deg: None,
            elevation_m: Some(40.0),
            true_north_deg: Some(12.5),
            coalesce: false,
        },
    );

    // Delete then create: fine (the singleton is per-live-document-state).
    ok(&mut doc, Command::DeleteSite { id: site });
    let second = create_site(&mut doc).expect("create after delete");
    assert_ne!(second, site, "ids are never reused");

    // Undoing back past the delete restores the ORIGINAL site; the redo
    // stack was invalidated by the new create, so no double-site state
    // is reachable.
    doc.undo().expect("undo create #2");
    doc.undo().expect("undo delete");
    assert_eq!(
        doc.entity(site).map(|r| r.kind()),
        Some(EntityKind::Site),
        "original site restored with its id"
    );
    assert_eq!(create_site(&mut doc).err(), Some(VimStatus::SingletonExists));

    assert_save_load_roundtrip(&doc);
}

#[test]
fn level_evaluates_to_a_frame_and_never_owns_a_mesh() {
    let mut doc = Document::new();
    let mut engine = Engine::new();
    let level = create_level(&mut doc, "Level 2", 3.0);
    let site = create_site(&mut doc).expect("site");
    engine.evaluate_pending(&mut doc);
    let updates = engine.poll_updates(&doc);

    assert!(updates.errors.is_empty(), "{:?}", updates.errors);
    assert!(updates.meshes.is_empty(), "levels/site never own meshes");
    match engine.value(level) {
        Some(Evaluated::Frame {
            origin,
            x_axis,
            y_axis,
            z_axis,
        }) => {
            assert_eq!(*origin, [0.0, 0.0, 3.0]);
            assert_eq!(*x_axis, [1.0, 0.0, 0.0]);
            assert_eq!(*y_axis, [0.0, 1.0, 0.0]);
            assert_eq!(*z_axis, [0.0, 0.0, 1.0]);
        }
        other => panic!("level value: {other:?}"),
    }
    assert!(matches!(engine.value(site), Some(Evaluated::Site)));
}

#[test]
fn moving_a_level_moves_exactly_the_attached_chain() {
    let mut doc = Document::new();
    let mut engine = Engine::new();
    let level = create_level(&mut doc, "Level 2", 3.0);
    let attached = build_attached_box(&mut doc, level);
    // A world-coordinate cube elsewhere: must not re-evaluate.
    let world_cube = build_cube(&mut doc, [10.0, 0.0, 0.0], 1.0, 1.0);

    engine.evaluate_pending(&mut doc);
    let updates = engine.poll_updates(&doc);
    assert!(updates.errors.is_empty(), "{:?}", updates.errors);
    let boxed = updates
        .meshes
        .iter()
        .find(|m| m.id == attached.extrusion)
        .expect("attached box meshes");
    let (min, max) = vim_design_test::mesh_bbox(&boxed.mesh);
    assert!((min[2] - 3.0).abs() < 1e-9 && (max[2] - 4.0).abs() < 1e-9);

    let counts_before: Vec<(EntityId, u64)> = doc
        .entities()
        .map(|(id, _)| (*id, engine.eval_count(*id)))
        .collect();

    // Drag the level from 3.0 to 4.5.
    ok(
        &mut doc,
        Command::UpdateLevel {
            id: level,
            name: None,
            elevation_m: Some(4.5),
            is_building_story: None,
            color: None,
            extent_m: None,
            coalesce: true,
        },
    );
    engine.evaluate_pending(&mut doc);
    let updates = engine.poll_updates(&doc);
    assert!(updates.errors.is_empty(), "{:?}", updates.errors);

    // The attached box rode along by exactly +1.5 m.
    assert_eq!(updates.meshes.len(), 1, "only the attached chain re-meshed");
    assert_eq!(updates.meshes[0].id, attached.extrusion);
    let (min, max) = vim_design_test::mesh_bbox(&updates.meshes[0].mesh);
    assert!((min[2] - 4.5).abs() < 1e-9 && (max[2] - 5.5).abs() < 1e-9);
    assert!((mesh_volume(&updates.meshes[0].mesh) - 1.0).abs() < 1e-9);

    // Eval-count instrumentation: the level's dependent closure
    // re-evaluated; the world cube's chain did not move at all, and the
    // attached box's UNattached path control points (inputs of the
    // extrusion, not dependents of the level) did not re-evaluate either.
    for (id, before) in counts_before {
        if attached.path_cps.contains(&id) {
            assert_eq!(
                engine.eval_count(id),
                before,
                "unattached path point {id:?} is not a dependent of the level"
            );
        }
        let after = engine.eval_count(id);
        let is_world_cube_part = world_cube.all_ids().contains(&id);
        if is_world_cube_part {
            assert_eq!(after, before, "world-coordinate entity {id:?} must not re-evaluate");
        }
    }
    assert_eq!(engine.tessellation_count(world_cube.extrusion), 1);
    assert_eq!(engine.tessellation_count(attached.extrusion), 2);
    assert_eq!(engine.eval_count(attached.face), 2, "attached chain re-evaluated once");
}

#[test]
fn attached_and_unattached_points_mix_in_one_wire() {
    let mut doc = Document::new();
    let mut engine = Engine::new();
    let level = create_level(&mut doc, "L1", 1.0);

    // Two corners attached ((u,v,0) on the level at z=1), two authored
    // directly in world coordinates at z=1: the wire chains and the face
    // is planar.
    let a = one(&mut doc, Command::CreateControlPoint { position: [0.0, 0.0, 0.0] });
    let b = one(&mut doc, Command::CreateControlPoint { position: [1.0, 0.0, 0.0] });
    for cp in [a, b] {
        ok(
            &mut doc,
            Command::UpdateControlPointPlane {
                id: cp,
                plane: Some(level),
                position: None,
            },
        );
    }
    let c = one(&mut doc, Command::CreateControlPoint { position: [1.0, 1.0, 1.0] });
    let d = one(&mut doc, Command::CreateControlPoint { position: [0.0, 1.0, 1.0] });
    let corners = [a, b, c, d];
    let lines: Vec<EntityId> = (0..4)
        .map(|i| {
            one(
                &mut doc,
                Command::CreateLine {
                    start: corners[i],
                    end: corners[(i + 1) % 4],
                },
            )
        })
        .collect();
    let edges: Vec<EntityId> = lines
        .iter()
        .map(|l| one(&mut doc, Command::CreateEdge { curve: *l }))
        .collect();
    let wire = one(&mut doc, Command::CreateWire { edges });
    let face = one(
        &mut doc,
        Command::CreateFace {
            outer: wire,
            holes: vec![],
            plane: None,
        },
    );
    engine.evaluate_pending(&mut doc);
    assert!(
        matches!(
            engine.state(face),
            Some(vim_design_lib::EvalState::UpToDate { .. })
        ),
        "mixed wire face evaluates: {:?}",
        engine.state(face)
    );
    match engine.value(a) {
        Some(Evaluated::Point(p)) => assert_eq!(*p, [0.0, 0.0, 1.0]),
        other => panic!("attached point value: {other:?}"),
    }
}

#[test]
fn params_pump_reports_level_edits_under_a_watch_set() {
    let mut doc = Document::new();
    let mut engine = Engine::new();
    let level = create_level(&mut doc, "L1", 2.0);
    let noise = one(&mut doc, Command::CreateControlPoint { position: [0.0; 3] });
    engine.evaluate_pending(&mut doc);
    let _ = engine.poll_updates(&doc);

    engine.set_params_watch(Some(BTreeSet::from([level])));
    ok(
        &mut doc,
        Command::UpdateLevel {
            id: level,
            name: Some("L1 renamed".to_owned()),
            elevation_m: Some(2.5),
            is_building_story: None,
            color: None,
            extent_m: None,
            coalesce: true,
        },
    );
    ok(
        &mut doc,
        Command::UpdateControlPoint {
            id: noise,
            position: [1.0; 3],
            coalesce: false,
        },
    );
    engine.evaluate_pending(&mut doc);
    let updates = engine.poll_updates(&doc);
    assert_eq!(updates.params_changed, vec![level]);
}

#[test]
fn element_level_association_is_mesh_inert() {
    let mut doc = Document::new();
    let mut engine = Engine::new();
    let level = create_level(&mut doc, "L1", 0.0);
    let cube = build_cube(&mut doc, [0.0, 0.0, 0.0], 1.0, 1.0);
    let element = one(
        &mut doc,
        Command::CreateElement {
            name: "wall".to_owned(),
            members: vec![cube.extrusion],
        },
    );
    engine.evaluate_pending(&mut doc);
    let updates = engine.poll_updates(&doc);
    let before = updates
        .meshes
        .iter()
        .find(|m| m.id == element)
        .expect("element mesh")
        .mesh
        .clone();

    // Associate: data-only. The element re-evaluates (the rewire dirties
    // it — params_changed reports it, correctly), but its output is
    // independent of the level slot, so the re-tessellated mesh is
    // byte-identical. That's the honest contract asserted here.
    ok(
        &mut doc,
        Command::UpdateElementLevel {
            element,
            level: Some(level),
        },
    );
    engine.evaluate_pending(&mut doc);
    let updates = engine.poll_updates(&doc);
    assert!(updates.errors.is_empty());
    assert!(updates.params_changed.contains(&element));
    let after = updates
        .meshes
        .iter()
        .find(|m| m.id == element)
        .expect("element re-delivered")
        .mesh
        .clone();
    assert_eq!(after.positions, before.positions, "association is mesh-inert");
    assert_eq!(after.indices, before.indices);
    assert_eq!(after.submeshes, before.submeshes);

    // Dissociate: byte-identical again.
    ok(
        &mut doc,
        Command::UpdateElementLevel {
            element,
            level: None,
        },
    );
    engine.evaluate_pending(&mut doc);
    let updates = engine.poll_updates(&doc);
    let cleared = updates
        .meshes
        .iter()
        .find(|m| m.id == element)
        .expect("element re-delivered")
        .mesh
        .clone();
    assert_eq!(cleared.indices, before.indices);
    assert_eq!(cleared.positions, before.positions);
}

#[test]
fn delete_level_cascade_is_one_undo_group_and_restores_byte_exact() {
    let mut doc = Document::new();
    let mut engine = Engine::new();
    let level = create_level(&mut doc, "L2", 3.0);
    let attached = build_attached_box(&mut doc, level);
    // Associate the box's element with the level, and instance it: both
    // are dependents and must cascade.
    let element = one(
        &mut doc,
        Command::CreateElement {
            name: "box".to_owned(),
            members: vec![attached.extrusion],
        },
    );
    ok(
        &mut doc,
        Command::UpdateElementLevel {
            element,
            level: Some(level),
        },
    );
    let instance = one(
        &mut doc,
        Command::CreateInstance {
            element,
            transform: vim_design_test::IDENTITY_XFORM,
        },
    );
    // A bystander that must survive (not a dependent of the level).
    let bystander = build_cube(&mut doc, [10.0, 0.0, 0.0], 1.0, 1.0);
    engine.evaluate_pending(&mut doc);
    let _ = engine.poll_updates(&doc);

    // Non-cascade: standard reject-if-dependents.
    assert_eq!(
        doc.submit(Command::DeleteLevel { id: level, cascade: false }).err(),
        Some(VimStatus::HasDependents)
    );

    // Cascade: the full transitive dependent closure + the level, one
    // undo group.
    let bytes_before = save(&doc);
    let undo_before = doc.undo_depth();
    ok(&mut doc, Command::DeleteLevel { id: level, cascade: true });
    assert_eq!(doc.undo_depth(), undo_before + 1, "ONE undo group");
    assert!(doc.entity(level).is_none());
    for id in &attached.corner_cps {
        assert!(doc.entity(*id).is_none(), "attached point cascaded");
    }
    assert!(doc.entity(attached.face).is_none());
    assert!(doc.entity(attached.extrusion).is_none());
    assert!(doc.entity(element).is_none(), "associated element cascaded");
    assert!(doc.entity(instance).is_none(), "instance cascaded");
    // Non-dependents survive: the unattached path control points and the
    // bystander cube.
    assert!(doc.entity(attached.path_cps[0]).is_some());
    assert!(doc.entity(attached.path_cps[1]).is_some());
    assert!(doc.entity(bystander.extrusion).is_some());
    doc.debug_validate().expect("consistent after cascade");

    // Poll: tombstones for the vanished owners.
    engine.evaluate_pending(&mut doc);
    let updates = engine.poll_updates(&doc);
    assert!(updates.meshes_removed.contains(&element));
    assert!(updates.instances_removed.contains(&instance));

    // ONE undo restores level + attachments + element + instance,
    // byte-exact; re-evaluation restores the mesh.
    doc.undo().expect("undo cascade");
    assert_eq!(save(&doc), bytes_before, "byte-exact restoration");
    engine.evaluate_pending(&mut doc);
    let updates = engine.poll_updates(&doc);
    assert!(updates.errors.is_empty(), "{:?}", updates.errors);
    assert!(updates.meshes.iter().any(|m| m.id == element));
    assert!(updates.instances.iter().any(|i| i.id == instance));

    assert_save_load_roundtrip(&doc);
}

#[test]
fn attach_detach_rewires_under_undo_redo() {
    let mut doc = Document::new();
    let mut engine = Engine::new();
    let level = create_level(&mut doc, "L1", 2.0);
    let cp = one(&mut doc, Command::CreateControlPoint { position: [1.0, 1.0, 0.5] });
    engine.evaluate_pending(&mut doc);

    let world_of = |engine: &Engine| match engine.value(cp) {
        Some(Evaluated::Point(p)) => *p,
        other => panic!("point value: {other:?}"),
    };
    assert_eq!(world_of(&engine), [1.0, 1.0, 0.5], "unattached = world");

    // Attach WITH the intent-layer conversion: same world position, so
    // the point does not jump ((u,v,w) = (1, 1, -1.5) on a z=2 level).
    ok(
        &mut doc,
        Command::UpdateControlPointPlane {
            id: cp,
            plane: Some(level),
            position: Some([1.0, 1.0, -1.5]),
        },
    );
    engine.evaluate_pending(&mut doc);
    assert_eq!(world_of(&engine), [1.0, 1.0, 0.5], "attach did not jump");

    // The attachment is live: the level's elevation now drives the point.
    ok(
        &mut doc,
        Command::UpdateLevel {
            id: level,
            name: None,
            elevation_m: Some(3.0),
            is_building_story: None,
            color: None,
            extent_m: None,
            coalesce: false,
        },
    );
    engine.evaluate_pending(&mut doc);
    assert_eq!(world_of(&engine), [1.0, 1.0, 1.5]);

    // Detach with conversion back to world coordinates.
    ok(
        &mut doc,
        Command::UpdateControlPointPlane {
            id: cp,
            plane: None,
            position: Some([1.0, 1.0, 1.5]),
        },
    );
    engine.evaluate_pending(&mut doc);
    assert_eq!(world_of(&engine), [1.0, 1.0, 1.5], "detach did not jump");

    // Undo/redo walk the whole history coherently (attach and detach are
    // single undoable commands: rewire + rewrite together).
    doc.undo().expect("undo detach");
    engine.evaluate_pending(&mut doc);
    assert_eq!(world_of(&engine), [1.0, 1.0, 1.5], "attached to z=3 level");
    doc.undo().expect("undo level move");
    engine.evaluate_pending(&mut doc);
    assert_eq!(world_of(&engine), [1.0, 1.0, 0.5]);
    doc.undo().expect("undo attach");
    engine.evaluate_pending(&mut doc);
    assert_eq!(world_of(&engine), [1.0, 1.0, 0.5], "back to world coords");
    doc.redo().expect("redo attach");
    doc.redo().expect("redo level move");
    engine.evaluate_pending(&mut doc);
    assert_eq!(world_of(&engine), [1.0, 1.0, 1.5]);

    // Kind safety: the plane slot accepts construction planes only.
    let stray = one(&mut doc, Command::CreateControlPoint { position: [0.0; 3] });
    assert_eq!(
        doc.submit(Command::UpdateControlPointPlane {
            id: cp,
            plane: Some(stray),
            position: None,
        })
        .err(),
        Some(VimStatus::SlotKindMismatch)
    );
    assert_save_load_roundtrip(&doc);
}

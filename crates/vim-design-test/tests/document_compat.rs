//! Saved-document compatibility.
//!
//! `fixtures/authoring_project.vimd` was saved by the library as it was
//! before the `Sketch` entity kind existed. Its content is what the web
//! authoring app builds: the Site, the levels "Ground" and "Level 2", a
//! floor plate with two holes, a closed run of four walls on the plate
//! edge, and two windows (hole wires in wall profile faces). Users keep
//! documents like this in browser storage, so every later library must
//! load it, evaluate it, and save it back to the SAME bytes (new enum
//! variants are appended, so existing postcard variant indices stay put).
//!
//! `write_fixture` is the generator. It only writes when the environment
//! variable `VIMD_WRITE_COMPAT_FIXTURE` is set; regenerate the fixture
//! only on an intentional, announced format break.

use vim_design_lib::entity::slot;
use vim_design_lib::eval::Engine;
use vim_design_lib::{Command, Document, EntityId, EntityKind, Params};

const FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/fixtures/authoring_project.vimd"
);

fn one(doc: &mut Document, cmd: Command) -> EntityId {
    let label = cmd.label();
    match doc.submit(cmd) {
        Ok(out) => match out.created_ids.as_slice() {
            [id] => *id,
            other => panic!("{label}: expected one created id, got {}", other.len()),
        },
        Err(status) => panic!("{label} rejected: {status:?}"),
    }
}

fn ok(doc: &mut Document, cmd: Command) {
    let label = cmd.label();
    if let Err(status) = doc.submit(cmd) {
        panic!("{label} rejected: {status:?}");
    }
}

/// A level-attached closed loop (control points -> lines -> edges ->
/// wire) from level-frame (u, v, w) points, as the app builds it.
fn attached_loop(doc: &mut Document, level: EntityId, points: &[[f64; 3]]) -> EntityId {
    let cps: Vec<EntityId> = points
        .iter()
        .map(|p| {
            let cp = one(doc, Command::CreateControlPoint { position: *p });
            ok(
                doc,
                Command::UpdateControlPointPlane {
                    id: cp,
                    plane: Some(level),
                    position: None,
                },
            );
            cp
        })
        .collect();
    let edges: Vec<EntityId> = (0..cps.len())
        .map(|i| {
            let line = one(
                doc,
                Command::CreateLine {
                    start: cps[i],
                    end: cps[(i + 1) % cps.len()],
                },
            );
            one(doc, Command::CreateEdge { curve: line })
        })
        .collect();
    one(doc, Command::CreateWire { edges })
}

/// An attached extrusion path from (u, v, w) `from` to `to`.
fn attached_path(doc: &mut Document, level: EntityId, from: [f64; 3], to: [f64; 3]) -> EntityId {
    let start = one(doc, Command::CreateControlPoint { position: from });
    let end = one(doc, Command::CreateControlPoint { position: to });
    let line = one(doc, Command::CreateLine { start, end });
    for cp in [start, end] {
        ok(
            doc,
            Command::UpdateControlPointPlane {
                id: cp,
                plane: Some(level),
                position: None,
            },
        );
    }
    line
}

fn place_element(doc: &mut Document, name: &str, member: EntityId, level: EntityId) -> EntityId {
    let element = one(
        doc,
        Command::CreateElement {
            name: name.to_owned(),
            members: vec![member],
            level,
        },
    );
    one(
        doc,
        Command::CreateInstance {
            element,
            transform: [
                1.0, 0.0, 0.0, 0.0, //
                0.0, 1.0, 0.0, 0.0, //
                0.0, 0.0, 1.0, 0.0,
            ],
        },
    );
    element
}

/// The authoring project, built with the commands the web app submits.
fn build_project() -> Document {
    let mut doc = Document::new();
    one(
        &mut doc,
        Command::CreateSite {
            latitude_deg: 45.5019,
            longitude_deg: -73.5674,
            elevation_m: 36.0,
            true_north_deg: 0.0,
        },
    );
    let ground = one(
        &mut doc,
        Command::CreateLevel {
            name: "Ground".to_owned(),
            elevation_m: 0.0,
            is_building_story: true,
            color: [0.18, 0.50, 0.93, 0.30],
            extent_m: 10.0,
        },
    );
    one(
        &mut doc,
        Command::CreateLevel {
            name: "Level 2".to_owned(),
            elevation_m: 3.0,
            is_building_story: true,
            color: [0.93, 0.52, 0.16, 0.30],
            extent_m: 10.0,
        },
    );

    // Floor plate 8 x 6 m, 0.3 m thick (extruded downward), two holes.
    let outline = [[0.0, 0.0], [8.0, 0.0], [8.0, 6.0], [0.0, 6.0]];
    let outer = attached_loop(
        &mut doc,
        ground,
        &outline.map(|[u, v]| [u, v, 0.0]),
    );
    let plate_face = one(
        &mut doc,
        Command::CreateFace {
            outer,
            holes: vec![],
            plane: None,
        },
    );
    let path = attached_path(&mut doc, ground, [0.0, 0.0, 0.0], [0.0, 0.0, -0.3]);
    let plate = one(
        &mut doc,
        Command::CreateExtrusion {
            profile: plate_face,
            path,
        },
    );
    place_element(&mut doc, "Floor plate 1", plate, ground);
    let mut holes = Vec::new();
    for hole in [
        [[1.0, 1.0], [2.0, 1.0], [2.0, 2.0], [1.0, 2.0]],
        [[5.0, 3.0], [6.5, 3.0], [6.5, 4.0], [5.0, 4.0]],
    ] {
        holes.push(attached_loop(&mut doc, ground, &hole.map(|[u, v]| [u, v, 0.0])));
        ok(
            &mut doc,
            Command::UpdateFace {
                id: plate_face,
                outer: None,
                holes: Some(holes.clone()),
                plane: None,
                coalesce: false,
            },
        );
    }

    // A closed wall run traced on the plate edge (CCW, grows inward),
    // 2.7 m high, 0.2 m thick, butt joins at the four convex corners:
    // each next segment's start is trimmed by the thickness.
    let (height, thickness) = (2.7, 0.2);
    let corners: [[f64; 2]; 4] = [[0.0, 0.0], [8.0, 0.0], [8.0, 6.0], [0.0, 6.0]];
    let mut wall_faces = Vec::new();
    for i in 0..4 {
        let a = corners[i];
        let b = corners[(i + 1) % 4];
        let len = ((b[0] - a[0]).powi(2) + (b[1] - a[1]).powi(2)).sqrt();
        let d = [(b[0] - a[0]) / len, (b[1] - a[1]) / len];
        let normal = [-d[1], d[0]];
        let start = [a[0] + d[0] * thickness, a[1] + d[1] * thickness];
        let end = b;
        let wire = attached_loop(
            &mut doc,
            ground,
            &[
                [start[0], start[1], 0.0],
                [end[0], end[1], 0.0],
                [end[0], end[1], height],
                [start[0], start[1], height],
            ],
        );
        let face = one(
            &mut doc,
            Command::CreateFace {
                outer: wire,
                holes: vec![],
                plane: None,
            },
        );
        let path = attached_path(
            &mut doc,
            ground,
            [start[0], start[1], 0.0],
            [
                start[0] + normal[0] * thickness,
                start[1] + normal[1] * thickness,
                0.0,
            ],
        );
        let extrusion = one(&mut doc, Command::CreateExtrusion { profile: face, path });
        place_element(&mut doc, &format!("Wall {}", i + 1), extrusion, ground);
        wall_faces.push((face, start, d));
    }

    // Windows: 1.2 x 1.0 m, sill at 0.9 m, on walls 1 and 2.
    for (face, start, d) in wall_faces.iter().take(2) {
        let at = |u: f64, v: f64| [start[0] + d[0] * u, start[1] + d[1] * u, v];
        let wire = attached_loop(
            &mut doc,
            ground,
            &[at(2.0, 0.9), at(3.2, 0.9), at(3.2, 1.9), at(2.0, 1.9)],
        );
        ok(
            &mut doc,
            Command::UpdateFace {
                id: *face,
                outer: None,
                holes: Some(vec![wire]),
                plane: None,
                coalesce: false,
            },
        );
    }
    doc
}

#[test]
#[ignore = "generator: writes the fixture only when VIMD_WRITE_COMPAT_FIXTURE is set"]
fn write_fixture() {
    if std::env::var_os("VIMD_WRITE_COMPAT_FIXTURE").is_none() {
        return;
    }
    let bytes = build_project().save().expect("save");
    std::fs::write(FIXTURE, bytes).expect("write fixture");
}

#[test]
fn saved_authoring_project_loads_evaluates_and_resaves_identically() {
    let bytes = std::fs::read(FIXTURE).expect("read fixture");
    let mut doc = Document::load(&bytes).expect("a saved project must still load");
    doc.debug_validate().expect("loaded graph is consistent");

    // Same bytes back: the encoding of every existing kind is unchanged.
    assert_eq!(
        doc.save().expect("save"),
        bytes,
        "load -> save must reproduce the saved bytes"
    );

    // The content survived.
    let count = |kind: EntityKind| doc.entities().filter(|(_, r)| r.kind() == kind).count();
    assert_eq!(count(EntityKind::Site), 1);
    assert_eq!(count(EntityKind::Level), 2);
    assert_eq!(count(EntityKind::Element), 5, "one plate + four walls");
    assert_eq!(count(EntityKind::Instance), 5);
    let names: Vec<String> = doc
        .entities()
        .filter_map(|(_, r)| match &r.params {
            Params::Element { name } => Some(name.clone()),
            _ => None,
        })
        .collect();
    assert!(names.contains(&"Floor plate 1".to_owned()));
    assert!(names.contains(&"Wall 4".to_owned()));
    let hole_counts: Vec<usize> = doc
        .entities()
        .filter(|(_, r)| r.kind() == EntityKind::Face)
        .map(|(_, r)| {
            r.inputs
                .get(slot::FACE_HOLES)
                .map(|s| s.referenced().count())
                .unwrap_or(0)
        })
        .collect();
    assert_eq!(
        hole_counts.iter().filter(|n| **n > 0).count(),
        3,
        "the plate face and two wall faces carry holes"
    );
    assert_eq!(hole_counts.iter().sum::<usize>(), 4, "two plate holes, two windows");

    // And it evaluates cleanly: five element meshes, no errors.
    let mut engine = Engine::new();
    engine.evaluate_pending(&mut doc);
    let updates = engine.poll_updates(&doc);
    assert!(updates.errors.is_empty(), "{:?}", updates.errors);
    assert_eq!(updates.meshes.len(), 5);
}

#[test]
fn the_generator_still_reproduces_the_fixture() {
    // The construction above still yields the saved bytes: the commands
    // the app submits compile to the same entities as before.
    let bytes = std::fs::read(FIXTURE).expect("read fixture");
    assert_eq!(build_project().save().expect("save"), bytes);
}

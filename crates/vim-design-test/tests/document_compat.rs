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
//! `fixtures/authoring_project_v2.vimd` was saved by the library as it
//! was before the `Wall` and `Workplane` entity kinds existed: the same
//! Site, levels, and walls with windows, and a floor plate that is a
//! `Sketch` (two solid faces, a through void, a pocket void) instead of
//! an extrusion.
//!
//! `fixtures/authoring_project_v3.vimd` was saved by the library as it
//! was before the `WallRun` entity kind existed: the Site, levels, a
//! ceiling workplane, the Sketch floor plate, and `Wall` entities —
//! fixed height, up to Level 2, and up to the workplane — with windows
//! and a door.
//!
//! `authoring_project_v4.vimd` adds `WallRun` entities to that project:
//! a closed run on Level 2 with a window, a door, a niche, and a gable
//! segment profile, and an open run up to the ceiling workplane.
//!
//! `authoring_project_v5.vimd` adds a room layout on the ground level:
//! three rooms (one with higher precedence cutting into another, one
//! with a hidden edge), a window, a door, and a niche.
//!
//! `write_fixture` / `write_fixture_v2` / `write_fixture_v3` /
//! `write_fixture_v4` / `write_fixture_v5` are the generators. They only
//! write when the environment variable `VIMD_WRITE_COMPAT_FIXTURE` is
//! set; regenerate a fixture only on an intentional, announced format
//! break.

use vim_design_lib::entity::slot;
use vim_design_lib::eval::Engine;
use vim_design_lib::sketch::{Sketch, SketchDirection, SketchFaceKind, ops};
use vim_design_lib::{Command, Document, EntityId, EntityKind, Params};

const FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/fixtures/authoring_project.vimd"
);
const FIXTURE_V2: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/fixtures/authoring_project_v2.vimd"
);
const FIXTURE_V3: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/fixtures/authoring_project_v3.vimd"
);
const FIXTURE_V5: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/fixtures/authoring_project_v5.vimd"
);
const FIXTURE_V4: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/fixtures/authoring_project_v4.vimd"
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
    let (mut doc, ground) = seed();
    extrusion_plate(&mut doc, ground);
    walls_with_windows(&mut doc, ground);
    doc
}

/// The Sketch-plate project with `Wall` entities and a workplane.
fn build_project_v3() -> Document {
    use vim_design_lib::wall::{default_profile, ops as wall_ops};
    let (mut doc, ground) = seed();
    let second = doc
        .entities()
        .find(|(_, r)| matches!(&r.params, Params::Level { name, .. } if name == "Level 2"))
        .map(|(id, _)| *id)
        .expect("Level 2");
    let ceiling = one(
        &mut doc,
        Command::CreateWorkplane {
            parent: ground,
            name: "Ceiling".to_owned(),
            offset_m: 2.6,
            color: [0.5, 0.5, 0.5, 0.25],
            extent_m: 10.0,
        },
    );
    sketch_plate(&mut doc, ground);
    // A closed CCW run on the plate edge, material inward (left).
    let corners: [[f64; 2]; 4] = [[0.0, 0.0], [8.0, 0.0], [8.0, 6.0], [0.0, 6.0]];
    let window = [[2.0, 0.9], [3.2, 0.9], [3.2, 1.9], [2.0, 1.9]];
    let door = [[1.0, -0.5], [1.9, -0.5], [1.9, 2.1], [1.0, 2.1]];
    // (top plane, top offset, fixed height, H for editing, void)
    type WallSpec = (Option<EntityId>, f64, f64, f64, Option<[[f64; 2]; 4]>);
    let walls: [WallSpec; 4] = [
        (None, 0.0, 2.7, 2.7, Some(window)),
        (None, 0.0, 2.7, 2.7, None),
        (Some(second), -0.3, 2.7, 2.7, Some(door)),
        (Some(ceiling), 0.0, 2.7, 2.6, Some(window)),
    ];
    for (i, (top, top_offset, height, h, void)) in walls.into_iter().enumerate() {
        let a = corners[i];
        let b = corners[(i + 1) % 4];
        let length = ((b[0] - a[0]).powi(2) + (b[1] - a[1]).powi(2)).sqrt();
        let (mut profile, mut top_points) = default_profile(length, 0.2);
        if let Some(outline) = void {
            (profile, top_points) = wall_ops::add_face(
                &profile,
                &top_points,
                h,
                &outline,
                SketchFaceKind::Void { depth: None },
            )
            .expect("add_face");
        }
        let wall = one(
            &mut doc,
            Command::CreateWall {
                base: ground,
                top,
                start: a,
                end: b,
                height_m: height,
                top_offset_m: top_offset,
                profile,
                top_points,
            },
        );
        one(
            &mut doc,
            Command::CreateElement {
                name: format!("Wall {}", i + 1),
                members: vec![wall],
                level: ground,
            },
        );
    }
    doc
}

/// The wall-run project plus a room layout.
fn build_project_v5() -> Document {
    use vim_design_lib::room_layout::RoomOpening;
    use vim_design_lib::wall_run::OpeningKind;
    let mut doc = build_project_v4();
    let ground = doc
        .entities()
        .find(|(_, r)| matches!(&r.params, Params::Level { name, .. } if name == "Ground"))
        .map(|(id, _)| *id)
        .expect("Ground");
    let layout = one(
        &mut doc,
        Command::CreateRoomLayout {
            plane: ground,
            top: None,
            rooms: vec![],
            thickness_m: 0.114,
            height_m: 2.7,
            top_offset_m: 0.0,
            openings: vec![],
        },
    );
    let mut rooms = Vec::new();
    for (name, precedence, corners, hidden) in [
        ("Room 001", 0, [[20.0, 0.0], [24.0, 3.0]], vec![]),
        ("Room 002", 0, [[24.0, 0.0], [27.0, 3.0]], vec![3]),
        ("Room 003", 1, [[23.0, 2.0], [25.0, 5.0]], vec![]),
    ] {
        let room = vim_design_lib::room::from_rectangle(name, precedence, corners[0], corners[1]).expect("room");
        rooms.push(one(
            &mut doc,
            Command::CreateRoom {
                plane: ground,
                name: room.name,
                precedence,
                boundary: room.boundary,
                hidden_edges: hidden,
                layout: Some(layout),
            },
        ));
    }
    let opening = |id, room, edge, offset_m, kind, depth_m| RoomOpening {
        id,
        room,
        edge,
        offset_m,
        sill_m: 0.9,
        width_m: 0.9,
        height_m: if kind == OpeningKind::Door { 2.1 } else { 1.2 },
        kind,
        depth_m,
    };
    ok(
        &mut doc,
        Command::UpdateRoomLayout {
            id: layout,
            plane: None,
            top: None,
            rooms: None,
            thickness_m: None,
            height_m: None,
            top_offset_m: None,
            openings: Some(vec![
                opening(0, rooms[0], 0, 1.0, OpeningKind::Window, None),
                opening(1, rooms[1], 0, 1.0, OpeningKind::Door, None),
                opening(2, rooms[0], 3, 1.0, OpeningKind::Window, Some(0.03)),
            ]),
            coalesce: false,
        },
    );
    one(
        &mut doc,
        Command::CreateElement { name: "Rooms".to_owned(), members: vec![layout], level: ground },
    );
    doc
}

/// The wall project plus two wall runs.
fn build_project_v4() -> Document {
    use vim_design_lib::wall_run::{Opening, OpeningKind, RunPoint, SegmentProfile};
    let mut doc = build_project_v3();
    let plane = |doc: &Document, wanted: &str| {
        doc.entities()
            .find(|(_, r)| match &r.params {
                Params::Level { name, .. } | Params::Workplane { name, .. } => name == wanted,
                _ => false,
            })
            .map(|(id, _)| *id)
            .expect("plane")
    };
    let ground = plane(&doc, "Ground");
    let second = plane(&doc, "Level 2");
    let ceiling = plane(&doc, "Ceiling");
    let points = |uv: &[[f64; 2]]| -> Vec<RunPoint> {
        uv.iter()
            .enumerate()
            .map(|(i, uv)| RunPoint { id: i as u32, uv: *uv })
            .collect()
    };
    let opening = |id, segment, offset_m, sill_m, width_m, height_m, kind, depth_m| Opening {
        id,
        segment,
        offset_m,
        sill_m,
        width_m,
        height_m,
        kind,
        depth_m,
    };
    // A gable on segment 1 (6 m long), drawn at H = 2.7: the upper
    // points are top-anchored.
    let gable = ops::add_face(
        &Sketch::default(),
        &[[0.0, 0.0], [6.0, 0.0], [6.0, 2.7], [3.0, 3.9], [0.0, 2.7]],
        SketchFaceKind::Solid { thickness: 0.2 },
    )
    .expect("gable");
    let top_points: Vec<u32> = gable.points.iter().filter(|p| p.uv[1] > 2.0).map(|p| p.id).collect();
    let gable = vim_design_lib::wall::stored_profile(&gable, &top_points, 2.7);
    let closed = one(
        &mut doc,
        Command::CreateWallRun {
            base: second,
            top: None,
            points: points(&[[0.0, 0.0], [8.0, 0.0], [8.0, 6.0], [0.0, 6.0]]),
            closed: true,
            thickness_m: 0.2,
            height_m: 2.7,
            top_offset_m: 0.0,
            openings: vec![
                opening(0, 0, 1.0, 0.9, 1.2, 1.2, OpeningKind::Window, None),
                opening(1, 0, 4.0, 0.0, 0.9, 2.1, OpeningKind::Door, None),
                opening(2, 2, 2.0, 1.0, 1.0, 0.8, OpeningKind::Window, Some(0.05)),
            ],
            profiles: vec![SegmentProfile { segment: 1, profile: gable, top_points }],
        },
    );
    one(
        &mut doc,
        Command::CreateElement { name: "Run 1".to_owned(), members: vec![closed], level: second },
    );
    let open = one(
        &mut doc,
        Command::CreateWallRun {
            base: ground,
            top: Some(ceiling),
            points: points(&[[10.0, 0.0], [14.0, 0.0], [16.0, 3.0]]),
            closed: false,
            thickness_m: 0.15,
            height_m: 2.7,
            top_offset_m: -0.1,
            openings: vec![opening(0, 1, 0.8, 1.0, 1.0, 1.0, OpeningKind::Window, None)],
            profiles: vec![],
        },
    );
    one(
        &mut doc,
        Command::CreateElement { name: "Run 2".to_owned(), members: vec![open], level: ground },
    );
    doc
}

/// The same project with the floor plate authored as a `Sketch`.
fn build_project_v2() -> Document {
    let (mut doc, ground) = seed();
    sketch_plate(&mut doc, ground);
    walls_with_windows(&mut doc, ground);
    doc
}

/// A Sketch floor plate: two solid faces, a through void, a pocket.
fn sketch_plate(doc: &mut Document, ground: EntityId) {
    let mut plate = Sketch::default();
    for (outline, kind) in [
        (
            [[0.0, 0.0], [8.0, 0.0], [8.0, 6.0], [0.0, 6.0]],
            SketchFaceKind::Solid { thickness: 0.3 },
        ),
        (
            [[8.0, 0.0], [10.0, 0.0], [10.0, 3.0], [8.0, 3.0]],
            SketchFaceKind::Solid { thickness: 0.2 },
        ),
        (
            [[1.0, 1.0], [2.0, 1.0], [2.0, 2.0], [1.0, 2.0]],
            SketchFaceKind::Void { depth: None },
        ),
        (
            [[5.0, 3.0], [6.5, 3.0], [6.5, 4.0], [5.0, 4.0]],
            SketchFaceKind::Void { depth: Some(0.1) },
        ),
    ] {
        plate = ops::add_face(&plate, &outline, kind).expect("add_face");
    }
    let sketch = one(
        doc,
        Command::CreateSketch {
            plane: ground,
            sketch: plate,
            direction: SketchDirection::Below,
        },
    );
    one(
        doc,
        Command::CreateElement {
            name: "Floor plate 1".to_owned(),
            members: vec![sketch],
            level: ground,
        },
    );
}

/// Site and the two default levels; returns (document, Ground).
fn seed() -> (Document, EntityId) {
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
    (doc, ground)
}

fn extrusion_plate(doc: &mut Document, ground: EntityId) {
    let doc = &mut *doc;
    // Floor plate 8 x 6 m, 0.3 m thick (extruded downward), two holes.
    let outline = [[0.0, 0.0], [8.0, 0.0], [8.0, 6.0], [0.0, 6.0]];
    let outer = attached_loop(
        doc,
        ground,
        &outline.map(|[u, v]| [u, v, 0.0]),
    );
    let plate_face = one(
        doc,
        Command::CreateFace {
            outer,
            holes: vec![],
            plane: None,
        },
    );
    let path = attached_path(doc, ground, [0.0, 0.0, 0.0], [0.0, 0.0, -0.3]);
    let plate = one(
        doc,
        Command::CreateExtrusion {
            profile: plate_face,
            path,
        },
    );
    place_element(doc, "Floor plate 1", plate, ground);
    let mut holes = Vec::new();
    for hole in [
        [[1.0, 1.0], [2.0, 1.0], [2.0, 2.0], [1.0, 2.0]],
        [[5.0, 3.0], [6.5, 3.0], [6.5, 4.0], [5.0, 4.0]],
    ] {
        holes.push(attached_loop(doc, ground, &hole.map(|[u, v]| [u, v, 0.0])));
        ok(
            doc,
            Command::UpdateFace {
                id: plate_face,
                outer: None,
                holes: Some(holes.clone()),
                plane: None,
                coalesce: false,
            },
        );
    }

}

fn walls_with_windows(doc: &mut Document, ground: EntityId) {
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
            doc,
            ground,
            &[
                [start[0], start[1], 0.0],
                [end[0], end[1], 0.0],
                [end[0], end[1], height],
                [start[0], start[1], height],
            ],
        );
        let face = one(
            doc,
            Command::CreateFace {
                outer: wire,
                holes: vec![],
                plane: None,
            },
        );
        let path = attached_path(
            doc,
            ground,
            [start[0], start[1], 0.0],
            [
                start[0] + normal[0] * thickness,
                start[1] + normal[1] * thickness,
                0.0,
            ],
        );
        let extrusion = one(doc, Command::CreateExtrusion { profile: face, path });
        place_element(doc, &format!("Wall {}", i + 1), extrusion, ground);
        wall_faces.push((face, start, d));
    }

    // Windows: 1.2 x 1.0 m, sill at 0.9 m, on walls 1 and 2.
    for (face, start, d) in wall_faces.iter().take(2) {
        let at = |u: f64, v: f64| [start[0] + d[0] * u, start[1] + d[1] * u, v];
        let wire = attached_loop(
            doc,
            ground,
            &[at(2.0, 0.9), at(3.2, 0.9), at(3.2, 1.9), at(2.0, 1.9)],
        );
        ok(
            doc,
            Command::UpdateFace {
                id: *face,
                outer: None,
                holes: Some(vec![wire]),
                plane: None,
                coalesce: false,
            },
        );
    }
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
#[ignore = "generator: writes the fixture only when VIMD_WRITE_COMPAT_FIXTURE is set"]
fn write_fixture_v2() {
    if std::env::var_os("VIMD_WRITE_COMPAT_FIXTURE").is_none() {
        return;
    }
    let bytes = build_project_v2().save().expect("save");
    std::fs::write(FIXTURE_V2, bytes).expect("write fixture");
}

#[test]
#[ignore = "generator: writes the fixture only when VIMD_WRITE_COMPAT_FIXTURE is set"]
fn write_fixture_v3() {
    if std::env::var_os("VIMD_WRITE_COMPAT_FIXTURE").is_none() {
        return;
    }
    let bytes = build_project_v3().save().expect("save");
    std::fs::write(FIXTURE_V3, bytes).expect("write fixture");
}

#[test]
#[ignore = "generator: writes the fixture only when VIMD_WRITE_COMPAT_FIXTURE is set"]
fn write_fixture_v5() {
    if std::env::var_os("VIMD_WRITE_COMPAT_FIXTURE").is_none() {
        return;
    }
    let bytes = build_project_v5().save().expect("save");
    std::fs::write(FIXTURE_V5, bytes).expect("write fixture");
}

#[test]
fn saved_room_project_loads_evaluates_and_resaves_identically() {
    let bytes = std::fs::read(FIXTURE_V5).expect("read fixture");
    let mut doc = Document::load(&bytes).expect("a saved project must still load");
    doc.debug_validate().expect("loaded graph is consistent");
    assert_eq!(doc.save().expect("save"), bytes, "load -> save must reproduce the saved bytes");
    let count = |kind: EntityKind| doc.entities().filter(|(_, r)| r.kind() == kind).count();
    assert_eq!(count(EntityKind::WallRun), 2);
    assert_eq!(count(EntityKind::Room), 3);
    assert_eq!(count(EntityKind::RoomLayout), 1);
    assert_eq!(count(EntityKind::Element), 8);
    let mut engine = Engine::new();
    engine.evaluate_pending(&mut doc);
    let updates = engine.poll_updates(&doc);
    assert!(updates.errors.is_empty(), "{:?}", updates.errors);
    assert_eq!(updates.meshes.len(), 8);
    assert_eq!(build_project_v5().save().expect("save"), bytes, "generator reproduces it");
}

#[test]
#[ignore = "generator: writes the fixture only when VIMD_WRITE_COMPAT_FIXTURE is set"]
fn write_fixture_v4() {
    if std::env::var_os("VIMD_WRITE_COMPAT_FIXTURE").is_none() {
        return;
    }
    let bytes = build_project_v4().save().expect("save");
    std::fs::write(FIXTURE_V4, bytes).expect("write fixture");
}

#[test]
fn saved_wall_run_project_loads_evaluates_and_resaves_identically() {
    let bytes = std::fs::read(FIXTURE_V4).expect("read fixture");
    let mut doc = Document::load(&bytes).expect("a saved project must still load");
    doc.debug_validate().expect("loaded graph is consistent");
    assert_eq!(doc.save().expect("save"), bytes, "load -> save must reproduce the saved bytes");
    let count = |kind: EntityKind| doc.entities().filter(|(_, r)| r.kind() == kind).count();
    assert_eq!(count(EntityKind::Workplane), 1);
    assert_eq!(count(EntityKind::Sketch), 1);
    assert_eq!(count(EntityKind::Wall), 4);
    assert_eq!(count(EntityKind::WallRun), 2);
    assert_eq!(count(EntityKind::Element), 7);
    let mut engine = Engine::new();
    engine.evaluate_pending(&mut doc);
    let updates = engine.poll_updates(&doc);
    assert!(updates.errors.is_empty(), "{:?}", updates.errors);
    assert_eq!(updates.meshes.len(), 7);
    assert_eq!(build_project_v4().save().expect("save"), bytes, "generator reproduces it");
}

#[test]
fn saved_wall_project_loads_evaluates_and_resaves_identically() {
    let bytes = std::fs::read(FIXTURE_V3).expect("read fixture");
    let mut doc = Document::load(&bytes).expect("a saved project must still load");
    doc.debug_validate().expect("loaded graph is consistent");
    assert_eq!(doc.save().expect("save"), bytes, "load -> save must reproduce the saved bytes");
    let count = |kind: EntityKind| doc.entities().filter(|(_, r)| r.kind() == kind).count();
    assert_eq!(count(EntityKind::Workplane), 1);
    assert_eq!(count(EntityKind::Sketch), 1);
    assert_eq!(count(EntityKind::Wall), 4);
    assert_eq!(count(EntityKind::Element), 5);
    let mut engine = Engine::new();
    engine.evaluate_pending(&mut doc);
    let updates = engine.poll_updates(&doc);
    assert!(updates.errors.is_empty(), "{:?}", updates.errors);
    assert_eq!(updates.meshes.len(), 5);
    assert_eq!(build_project_v3().save().expect("save"), bytes, "generator reproduces it");
}

#[test]
fn saved_sketch_project_loads_evaluates_and_resaves_identically() {
    let bytes = std::fs::read(FIXTURE_V2).expect("read fixture");
    let mut doc = Document::load(&bytes).expect("a saved project must still load");
    doc.debug_validate().expect("loaded graph is consistent");
    assert_eq!(
        doc.save().expect("save"),
        bytes,
        "load -> save must reproduce the saved bytes"
    );
    let count = |kind: EntityKind| doc.entities().filter(|(_, r)| r.kind() == kind).count();
    assert_eq!(count(EntityKind::Site), 1);
    assert_eq!(count(EntityKind::Level), 2);
    assert_eq!(count(EntityKind::Sketch), 1);
    assert_eq!(count(EntityKind::Element), 5, "one plate + four walls");
    let plate = doc
        .entities()
        .find_map(|(_, r)| match &r.params {
            Params::Sketch { sketch, .. } => Some(sketch.clone()),
            _ => None,
        })
        .expect("sketch plate");
    assert_eq!(plate.faces.len(), 4);

    let mut engine = Engine::new();
    engine.evaluate_pending(&mut doc);
    let updates = engine.poll_updates(&doc);
    assert!(updates.errors.is_empty(), "{:?}", updates.errors);
    assert_eq!(updates.meshes.len(), 5);
    assert_eq!(build_project_v2().save().expect("save"), bytes, "generator reproduces it");
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

/// The saved walls convert into a wall run where they share their
/// planes, and the run round-trips through save and load.
#[test]
fn saved_walls_convert_into_a_wall_run() {
    let bytes = std::fs::read(FIXTURE_V3).expect("read fixture");
    let mut doc = Document::load(&bytes).expect("load");
    let mut walls: Vec<EntityId> = doc
        .entities()
        .filter(|(_, r)| r.kind() == EntityKind::Wall)
        .map(|(id, _)| *id)
        .collect();
    walls.sort_unstable();
    assert!(matches!(
        vim_design_lib::wall_run::from_walls(&doc, &walls),
        Err(vim_design_lib::wall_run::FromWallsError::MixedPlanes(_))
    ));
    let (run, base, top) = vim_design_lib::wall_run::from_walls(&doc, &walls[..2]).expect("convert");
    assert_eq!((run.points.len(), run.openings.len(), run.closed), (3, 1, false));
    let id = one(
        &mut doc,
        Command::CreateWallRun {
            base,
            top,
            points: run.points.clone(),
            closed: run.closed,
            thickness_m: run.thickness_m,
            height_m: run.height_m,
            top_offset_m: run.top_offset_m,
            openings: run.openings.clone(),
            profiles: run.profiles.clone(),
        },
    );
    let mut engine = Engine::new();
    engine.evaluate_pending(&mut doc);
    assert!(engine.poll_updates(&doc).errors.is_empty());
    let saved = doc.save().expect("save");
    let reloaded = Document::load(&saved).expect("load");
    assert_eq!(reloaded.save().expect("save"), saved);
    assert!(matches!(reloaded.entity(id).map(|r| &r.params), Some(Params::WallRun { .. })));
}

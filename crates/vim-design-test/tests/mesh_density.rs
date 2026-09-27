//! Mesh density of planar prism owners (sketch plates, walls).

use vim_design_lib::eval::Engine;
use vim_design_lib::sketch::{Sketch, SketchDirection, SketchFaceKind, ops};
use vim_design_lib::wall::{self, ops as wall_ops};
use vim_design_lib::{Command, Document, EntityId, Mesh};
use vim_design_test::{assert_watertight, mesh_volume, one};

fn level(doc: &mut Document) -> EntityId {
    one(
        doc,
        Command::CreateLevel {
            name: "Ground".to_owned(),
            elevation_m: 0.0,
            is_building_story: true,
            color: [0.2, 0.5, 0.9, 0.3],
            extent_m: 10.0,
        },
    )
}

fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> Vec<[f64; 2]> {
    vec![[x0, y0], [x1, y0], [x1, y1], [x0, y1]]
}

fn plate(faces: &[(Vec<[f64; 2]>, SketchFaceKind)]) -> Mesh {
    let mut doc = Document::new();
    let ground = level(&mut doc);
    let mut s = Sketch::default();
    for (outline, kind) in faces {
        s = ops::add_face(&s, outline, *kind).expect("add_face");
    }
    let sketch = one(
        &mut doc,
        Command::CreateSketch { plane: ground, sketch: s, direction: SketchDirection::Below },
    );
    mesh_of(&mut doc, sketch)
}

fn mesh_of(doc: &mut Document, owner: EntityId) -> Mesh {
    let mut engine = Engine::new();
    engine.evaluate_pending(doc);
    let updates = engine.poll_updates(doc);
    assert!(updates.errors.is_empty(), "{:?}", updates.errors);
    engine.mesh(owner).expect("meshed").clone()
}

fn wall_with_window() -> Mesh {
    wall_along([0.0, 0.0], [4.0, 0.0])
}

fn wall_along(start: [f64; 2], end: [f64; 2]) -> Mesh {
    let mut doc = Document::new();
    let ground = level(&mut doc);
    let length = ((end[0] - start[0]).powi(2) + (end[1] - start[1]).powi(2)).sqrt();
    let (p, t) = wall::default_profile(length, 0.2);
    let (p, t) = wall_ops::add_face(
        &p,
        &t,
        2.7,
        &rect(1.0, 0.9, 2.2, 1.9),
        SketchFaceKind::Void { depth: None },
    )
    .expect("window");
    let w = one(
        &mut doc,
        Command::CreateWall {
            base: ground,
            top: None,
            start,
            end,
            height_m: 2.7,
            top_offset_m: 0.0,
            profile: p,
            top_points: t,
        },
    );
    mesh_of(&mut doc, w)
}

const SOLID: SketchFaceKind = SketchFaceKind::Solid { thickness: 0.3 };
const THROUGH: SketchFaceKind = SketchFaceKind::Void { depth: None };
const POCKET: SketchFaceKind = SketchFaceKind::Void { depth: Some(0.1) };

fn assert_counts(name: &str, mesh: &Mesh, triangles: usize, volume: f64) {
    assert_eq!(mesh.triangle_count(), triangles, "{name}: triangles");
    assert_watertight(mesh);
    let v = mesh_volume(mesh);
    assert!((v - volume).abs() < 1e-6, "{name}: volume {v}, expected {volume}");
}

/// A planar face with n boundary vertices and h holes is n + 2h - 2
/// triangles, and every face is one face (stacked layers merge, internal
/// faces vanish).
#[test]
fn planar_owners_mesh_to_the_minimal_triangle_count() {
    assert_counts("rect plate", &plate(&[(rect(0.0, 0.0, 4.0, 3.0), SOLID)]), 12, 12.0 * 0.3);
    assert_counts(
        "plate with two holes",
        &plate(&[
            (rect(0.0, 0.0, 4.0, 3.0), SOLID),
            (rect(0.5, 0.5, 1.5, 1.5), THROUGH),
            (rect(2.5, 1.0, 3.5, 2.0), THROUGH),
        ]),
        52,
        (12.0 - 2.0) * 0.3,
    );
    assert_counts(
        "plate with a pocket",
        &plate(&[(rect(0.0, 0.0, 4.0, 3.0), SOLID), (rect(1.0, 1.0, 2.0, 2.0), POCKET)]),
        28,
        12.0 * 0.3 - 0.1,
    );
    assert_counts("wall with a window", &wall_with_window(), 32, (4.0 * 2.7 - 1.2) * 0.2);
    assert_counts("rotated wall", &wall_along([1.0, 1.0], [4.0, 5.0]), 32, (5.0 * 2.7 - 1.2) * 0.2);
    assert_counts(
        "rotated plate",
        &plate(&[(vec![[0.0, 0.0], [4.0, 1.3], [2.7, 5.2], [-1.3, 4.0]], SOLID)]),
        12,
        0.3 * (4.0 * 5.2 - 1.3 * 2.7 + 2.7 * 4.0 + 1.3 * 5.2) / 2.0,
    );
}

/// The saved sketch plate (fixtures v2 and v3) was 140 triangles with
/// kernel tessellation.
#[test]
fn the_fixture_sketch_plate_is_62_triangles() {
    for name in ["authoring_project_v2.vimd", "authoring_project_v3.vimd"] {
        let path = format!("{}/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
        let bytes = std::fs::read(path).expect("fixture");
        let mut doc = Document::load(&bytes).expect("load");
        let mut engine = Engine::new();
        engine.evaluate_pending(&mut doc);
        let updates = engine.poll_updates(&doc);
        let counts: Vec<usize> = updates.meshes.iter().map(|m| m.mesh.triangle_count()).collect();
        assert!(counts.contains(&62), "{name}: {counts:?}");
        assert!(!counts.iter().any(|c| *c > 62), "{name}: {counts:?}");
        for update in &updates.meshes {
            assert_watertight(&update.mesh);
        }
    }
}

#[test]
#[ignore = "diagnostic printout"]
fn print_triangle_counts() {
    let cases = [
        ("rect plate", plate(&[(rect(0.0, 0.0, 4.0, 3.0), SOLID)])),
        (
            "rect plate + 2 holes",
            plate(&[
                (rect(0.0, 0.0, 4.0, 3.0), SOLID),
                (rect(0.5, 0.5, 1.5, 1.5), THROUGH),
                (rect(2.5, 1.0, 3.5, 2.0), THROUGH),
            ]),
        ),
        (
            "rect plate + pocket",
            plate(&[(rect(0.0, 0.0, 4.0, 3.0), SOLID), (rect(1.0, 1.0, 2.0, 2.0), POCKET)]),
        ),
        ("wall + window", wall_with_window()),
        ("big plate 30x20 + 2 holes", plate(&[
            (rect(0.0, 0.0, 30.0, 20.0), SOLID),
            (rect(2.0, 2.0, 5.0, 6.0), THROUGH),
            (rect(20.0, 10.0, 25.0, 15.0), THROUGH),
        ])),
        ("rotated plate", plate(&[(vec![[0.0, 0.0], [4.0, 1.3], [2.7, 5.2], [-1.3, 4.0]], SOLID)])),
        ("wall 12 m + window", wall_along([0.0, 0.0], [12.0, 0.0])),
        ("rotated wall 5 m", wall_along([1.0, 1.0], [4.0, 5.0])),
    ];
    for (name, mesh) in cases {
        println!("{name}: {} triangles, {} vertices", mesh.triangle_count(), mesh.vertex_count());
    }
}

#[test]
#[ignore = "diagnostic printout"]
fn print_fixture_triangle_counts() {
    for name in ["authoring_project.vimd", "authoring_project_v2.vimd", "authoring_project_v3.vimd"] {
        let path = format!("{}/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
        let bytes = std::fs::read(path).expect("fixture");
        let mut doc = Document::load(&bytes).expect("load");
        let mut engine = Engine::new();
        engine.evaluate_pending(&mut doc);
        let updates = engine.poll_updates(&doc);
        let counts: Vec<usize> = updates.meshes.iter().map(|m| m.mesh.triangle_count()).collect();
        println!("{name}: {counts:?}");
    }
}

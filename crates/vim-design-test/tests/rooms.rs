//! Rooms and room layouts: effective regions, the shared wall network,
//! junctions, hidden edges, openings, spaces, the top constraint,
//! commands, undo, and the pure operations.

use vim_design_lib::eval::{Engine, EvalErrorKind, EvalState, SubRefResolution};
use vim_design_lib::room::{self, RoomData, RoomError, ops as room_ops};
use vim_design_lib::room_layout::{
    self, DEFAULT_PARTITION_THICKNESS_M, LayoutError, LayoutInput, LayoutIssue, RegionStatus,
    RoomLayoutData, RoomOpening, ops as layout_ops,
};
use vim_design_lib::subref::RoomWallPart;
use vim_design_lib::wall_run::OpeningKind;
use vim_design_lib::{Command, Document, EntityId, Mesh, ProvenancePath, SubRef, VimStatus};
use vim_design_test::{assert_watertight, mesh_bbox, mesh_volume, one, ok, save};

const T: f64 = 0.2;
const H: f64 = 2.7;

fn level(doc: &mut Document, name: &str, elevation_m: f64) -> EntityId {
    one(
        doc,
        Command::CreateLevel {
            name: name.to_owned(),
            elevation_m,
            is_building_story: true,
            color: [0.2, 0.5, 0.9, 0.3],
            extent_m: 10.0,
        },
    )
}

fn set_elevation(doc: &mut Document, id: EntityId, elevation: f64) {
    ok(
        doc,
        Command::UpdateLevel {
            id,
            name: None,
            elevation_m: Some(elevation),
            is_building_story: None,
            color: None,
            extent_m: None,
            coalesce: false,
        },
    );
}

fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> Vec<[f64; 2]> {
    vec![[x0, y0], [x1, y0], [x1, y1], [x0, y1]]
}

/// A test scene: a ground level with one layout and its rooms.
struct Scene {
    doc: Document,
    engine: Engine,
    ground: EntityId,
    layout: EntityId,
    rooms: Vec<EntityId>,
}

impl Scene {
    fn new(top: Option<f64>) -> Scene {
        let mut doc = Document::new();
        let ground = level(&mut doc, "Ground", 0.0);
        let top = top.map(|elevation| level(&mut doc, "Level 2", elevation));
        let layout = one(
            &mut doc,
            Command::CreateRoomLayout {
                plane: ground,
                top,
                rooms: vec![],
                thickness_m: T,
                height_m: H,
                top_offset_m: 0.0,
                openings: vec![],
            },
        );
        let mut engine = Engine::new();
        engine.set_translation_factoring(true);
        Scene { doc, engine, ground, layout, rooms: vec![] }
    }

    fn with_rooms(polygons: &[Vec<[f64; 2]>]) -> Scene {
        let mut scene = Scene::new(None);
        for polygon in polygons {
            scene.add(polygon, 0);
        }
        scene
    }

    fn add(&mut self, polygon: &[[f64; 2]], precedence: i32) -> EntityId {
        let name = room::default_name(&self.doc);
        let data = room::from_polygon(&name, precedence, polygon).expect("room");
        let id = one(
            &mut self.doc,
            Command::CreateRoom {
                plane: self.ground,
                name: data.name,
                precedence,
                boundary: data.boundary,
                hidden_edges: vec![],
                layout: Some(self.layout),
            },
        );
        self.rooms.push(id);
        id
    }

    fn room(&self, id: EntityId) -> RoomData {
        RoomData::from_params(&self.doc.entity(id).expect("room").params).expect("room params")
    }

    fn layout_data(&self) -> RoomLayoutData {
        RoomLayoutData::from_params(&self.doc.entity(self.layout).expect("layout").params).expect("params")
    }

    fn input(&self) -> LayoutInput {
        room_layout::inputs(&self.doc, self.layout).expect("inputs")
    }

    fn update_room(&mut self, id: EntityId, data: RoomData) {
        ok(
            &mut self.doc,
            Command::UpdateRoom {
                id,
                plane: None,
                name: Some(data.name),
                precedence: Some(data.precedence),
                boundary: Some(data.boundary),
                hidden_edges: Some(data.hidden_edges),
                coalesce: false,
            },
        );
    }

    fn set_openings(&mut self, openings: Vec<RoomOpening>) {
        ok(
            &mut self.doc,
            Command::UpdateRoomLayout {
                id: self.layout,
                plane: None,
                top: None,
                rooms: None,
                thickness_m: None,
                height_m: None,
                top_offset_m: None,
                openings: Some(openings),
                coalesce: false,
            },
        );
    }

    /// Evaluate; the layout's mesh (asserting no errors anywhere).
    fn mesh(&mut self) -> Mesh {
        self.engine.evaluate_pending(&mut self.doc);
        let updates = self.engine.poll_updates(&self.doc);
        assert!(updates.errors.is_empty(), "{:?}", updates.errors);
        let mesh = self.engine.mesh(self.layout).expect("meshed").clone();
        assert_watertight(&mesh);
        mesh
    }
}

fn assert_near(actual: f64, expected: f64) {
    assert!((actual - expected).abs() < 1e-5, "expected {expected}, got {actual}");
}

fn opening(id: u32, room: EntityId, edge: u32, offset_m: f64, width_m: f64, kind: OpeningKind) -> RoomOpening {
    RoomOpening { id, room, edge, offset_m, sill_m: 0.9, width_m, height_m: 1.2, kind, depth_m: None }
}

// ---------------------------------------------------------------------
// Golden volumes: the footprint is the union of centered strokes.
// ---------------------------------------------------------------------

#[test]
fn two_adjacent_rooms_share_one_wall() {
    let mut scene = Scene::with_rooms(&[rect(0.0, 0.0, 4.0, 3.0), rect(4.0, 0.0, 7.0, 3.0)]);
    let m = scene.mesh();
    // Outer ring minus the two room interiors.
    let footprint = (7.0 + T) * (3.0 + T) - (4.0 - T) * (3.0 - T) - (3.0 - T) * (3.0 - T);
    assert_near(mesh_volume(&m), footprint * H);
    let (min, max) = mesh_bbox(&m);
    assert_near(min[0], -T / 2.0);
    assert_near(max[0], 7.0 + T / 2.0);
    assert_near(max[2], H);
    let regions = scene.engine.room_regions(scene.layout).expect("regions");
    assert_eq!(regions.len(), 2);
    assert_near(regions[0].area_m2, 12.0);
    assert_near(regions[1].area_m2, 9.0);
    assert!(regions.iter().all(|r| r.status == RegionStatus::Whole));
}

/// The area of a polygon offset outward by `d` with miter joins (inward
/// for a negative `d`): A + P d + d^2 * sum(tan(turn / 2)).
fn offset_area(polygon: &[[f64; 2]], d: f64) -> f64 {
    let n = polygon.len();
    let mut area = 0.0;
    let mut perimeter = 0.0;
    let mut corners = 0.0;
    for i in 0..n {
        let (a, b, c) = (polygon[i], polygon[(i + 1) % n], polygon[(i + 2) % n]);
        area += a[0] * b[1] - a[1] * b[0];
        perimeter += ((b[0] - a[0]).powi(2) + (b[1] - a[1]).powi(2)).sqrt();
        let (u, v) = ([b[0] - a[0], b[1] - a[1]], [c[0] - b[0], c[1] - b[1]]);
        let turn = (u[0] * v[1] - u[1] * v[0]).atan2(u[0] * v[0] + u[1] * v[1]);
        corners += (turn / 2.0).tan();
    }
    area / 2.0 + perimeter * d + d * d * corners
}

#[test]
fn a_single_room_is_its_perimeter_times_the_thickness() {
    // A closed mitered ring: outer offset minus inner offset = P t.
    let l_shape = vec![[0.0, 0.0], [6.0, 0.0], [6.0, 2.0], [3.0, 2.0], [3.0, 5.0], [0.0, 5.0]];
    let mut scene = Scene::with_rooms(&[l_shape]);
    assert_near(mesh_volume(&scene.mesh()), 22.0 * T * H);
    // A 60 degree corner (a parallelogram) and an equilateral triangle.
    let (c, s) = (60f64.to_radians().cos(), 60f64.to_radians().sin());
    for polygon in [
        vec![[0.0, 0.0], [4.0, 0.0], [4.0 + 2.0 * c, 2.0 * s], [2.0 * c, 2.0 * s]],
        vec![[0.0, 0.0], [4.0, 0.0], [4.0 * c, 4.0 * s]],
    ] {
        let perimeter: f64 = (0..polygon.len())
            .map(|i| {
                let (a, b) = (polygon[i], polygon[(i + 1) % polygon.len()]);
                ((b[0] - a[0]).powi(2) + (b[1] - a[1]).powi(2)).sqrt()
            })
            .sum();
        let mut scene = Scene::with_rooms(&[polygon]);
        assert_near(mesh_volume(&scene.mesh()), perimeter * T * H);
    }
}

#[test]
fn t_and_x_junctions_are_exact() {
    // Four rooms in a 2 x 2 grid: one X junction and four T junctions.
    let mut scene = Scene::with_rooms(&[
        rect(0.0, 0.0, 4.0, 3.0),
        rect(4.0, 0.0, 8.0, 3.0),
        rect(0.0, 3.0, 4.0, 6.0),
        rect(4.0, 3.0, 8.0, 6.0),
    ]);
    let footprint = (8.0 + T) * (6.0 + T) - 4.0 * (4.0 - T) * (3.0 - T);
    assert_near(mesh_volume(&scene.mesh()), footprint * H);
}

#[test]
fn a_partly_shared_edge_and_separate_rooms() {
    // The small room shares part of the big room's right edge: two T
    // junctions on that edge.
    let mut scene = Scene::with_rooms(&[rect(0.0, 0.0, 4.0, 3.0), rect(4.0, 1.0, 6.0, 2.0)]);
    let h = T / 2.0;
    let union = [[0.0, 0.0], [4.0, 0.0], [4.0, 1.0], [6.0, 1.0], [6.0, 2.0], [4.0, 2.0], [4.0, 3.0], [0.0, 3.0]];
    let footprint = offset_area(&union, h)
        - offset_area(&rect(0.0, 0.0, 4.0, 3.0), -h)
        - offset_area(&rect(4.0, 1.0, 6.0, 2.0), -h);
    assert_near(mesh_volume(&scene.mesh()), footprint * H);
    // Rooms apart: each its own ring.
    let mut scene = Scene::with_rooms(&[rect(0.0, 0.0, 4.0, 3.0), rect(6.0, 0.0, 8.0, 2.0)]);
    assert_near(mesh_volume(&scene.mesh()), (14.0 + 8.0) * T * H);
}

#[test]
fn a_higher_room_cuts_into_a_lower_one() {
    let mut scene = Scene::new(None);
    let lower = scene.add(&rect(0.0, 0.0, 4.0, 4.0), 0);
    let higher = scene.add(&rect(3.0, 1.0, 6.0, 3.0), 1);
    let m = scene.mesh();
    let regions = scene.engine.room_regions(scene.layout).expect("regions").to_vec();
    let area = |id| regions.iter().find(|r| r.room == id).map(|r| r.area_m2).expect("region");
    assert_near(area(lower), 16.0 - 2.0);
    assert_near(area(higher), 6.0);
    // Dilated union minus each eroded effective region.
    let h = T / 2.0;
    let union = [[0.0, 0.0], [4.0, 0.0], [4.0, 1.0], [6.0, 1.0], [6.0, 3.0], [4.0, 3.0], [4.0, 4.0], [0.0, 4.0]];
    let lower_region = [[0.0, 0.0], [4.0, 0.0], [4.0, 1.0], [3.0, 1.0], [3.0, 3.0], [4.0, 3.0], [4.0, 4.0], [0.0, 4.0]];
    let higher_region = rect(3.0, 1.0, 6.0, 3.0);
    let footprint = offset_area(&union, h) - offset_area(&lower_region, -h) - offset_area(&higher_region, -h);
    assert_near(mesh_volume(&m), footprint * H);

    // Swapping the precedence swaps the cut: the higher room is now cut.
    let mut data = scene.room(lower);
    data.precedence = 2;
    scene.update_room(lower, data);
    scene.mesh();
    let regions = scene.engine.room_regions(scene.layout).expect("regions").to_vec();
    let area = |id| regions.iter().find(|r| r.room == id).map(|r| r.area_m2).expect("region");
    assert_near(area(lower), 16.0);
    assert_near(area(higher), 4.0);

    // A room entirely under a higher one is empty; one cut in two has two pieces.
    let mut scene = Scene::new(None);
    let small = scene.add(&rect(1.0, 1.0, 2.0, 2.0), 0);
    let band = scene.add(&rect(5.0, 0.0, 6.0, 4.0), 0);
    let _big = scene.add(&rect(0.0, 0.0, 3.0, 3.0), 5);
    let _bar = scene.add(&rect(4.0, 1.5, 7.0, 2.5), 5);
    scene.mesh();
    let regions = scene.engine.room_regions(scene.layout).expect("regions").to_vec();
    let status = |id| regions.iter().find(|r| r.room == id).map(|r| r.status).expect("region");
    assert_eq!(status(small), RegionStatus::Empty);
    assert_eq!(status(band), RegionStatus::Pieces(2));
}

#[test]
fn a_hidden_edge_generates_no_wall_and_the_rooms_stay_distinct() {
    let open_plan = (7.0 + T) * (3.0 + T) - (7.0 - T) * (3.0 - T);
    // Hidden on the left room's edge, on the right room's edge, on both.
    for (hide_left, hide_right) in [(true, false), (false, true), (true, true)] {
        let mut scene = Scene::with_rooms(&[rect(0.0, 0.0, 4.0, 3.0), rect(4.0, 0.0, 7.0, 3.0)]);
        let (left, right) = (scene.rooms[0], scene.rooms[1]);
        if hide_left {
            let data = room_ops::set_hidden(&scene.room(left), &[1], true).expect("hide");
            scene.update_room(left, data);
        }
        if hide_right {
            let data = room_ops::set_hidden(&scene.room(right), &[3], true).expect("hide");
            scene.update_room(right, data);
        }
        assert_near(mesh_volume(&scene.mesh()), open_plan * H);
        let regions = scene.engine.room_regions(scene.layout).expect("regions");
        assert_eq!(regions.len(), 2);
        assert_near(regions[0].area_m2, 12.0);
        assert_near(regions[1].area_m2, 9.0);
    }
    // A hidden outside edge leaves a gap with square wall ends.
    let mut scene = Scene::with_rooms(&[rect(0.0, 0.0, 4.0, 3.0)]);
    let id = scene.rooms[0];
    let data = room_ops::set_hidden(&scene.room(id), &[0], true).expect("hide");
    scene.update_room(id, data);
    // Three walls: the left and right ones end square at the base line.
    let expected = (4.0 + T) * (3.0 + T / 2.0) - (4.0 - T) * (3.0 - T / 2.0);
    assert_near(mesh_volume(&scene.mesh()), expected * H);
}

// ---------------------------------------------------------------------
// Openings.
// ---------------------------------------------------------------------

fn two_rooms() -> Scene {
    Scene::with_rooms(&[rect(0.0, 0.0, 4.0, 3.0), rect(4.0, 0.0, 7.0, 3.0)])
}

fn two_room_volume() -> f64 {
    ((7.0 + T) * (3.0 + T) - (4.0 - T) * (3.0 - T) - (3.0 - T) * (3.0 - T)) * H
}

#[test]
fn openings_on_a_shared_wall_a_door_and_a_niche() {
    let mut scene = two_rooms();
    let (left, right) = (scene.rooms[0], scene.rooms[1]);
    // The shared wall runs along the left room's edge 1: T junctions at
    // both ends keep openings half a thickness away.
    let spans = room_layout::opening_span(&scene.input(), left, 1).expect("span");
    assert_eq!(spans.len(), 1);
    assert_near(spans[0].0, T / 2.0);
    assert_near(spans[0].1, 3.0 - T / 2.0);
    // The same wall seen from the right room's edge 3.
    let spans = room_layout::opening_span(&scene.input(), right, 3).expect("span");
    assert_near(spans[0].0, T / 2.0);
    scene.set_openings(vec![
        opening(0, left, 1, 1.0, 1.0, OpeningKind::Window),
        RoomOpening { height_m: 2.1, ..opening(1, left, 0, 1.5, 0.9, OpeningKind::Door) },
        RoomOpening { depth_m: Some(0.05), ..opening(2, right, 1, 1.0, 1.0, OpeningKind::Window) },
    ]);
    let m = scene.mesh();
    let removed = 1.0 * 1.2 * T + 0.9 * 2.1 * T + 1.0 * 1.2 * 0.05;
    assert_near(mesh_volume(&m), two_room_volume() - removed);
    let (min, _) = mesh_bbox(&m);
    assert_near(min[2], 0.0);
    assert!(m.positions.iter().any(|p| (f64::from(p[2]) - 2.1).abs() < 1e-6), "door head");
}

#[test]
fn an_opening_that_stops_fitting_is_reported_and_the_rest_evaluates() {
    let mut scene = two_rooms();
    let left = scene.rooms[0];
    scene.set_openings(vec![
        opening(0, left, 0, 1.0, 1.0, OpeningKind::Window),
        opening(1, left, 3, 1.0, 1.0, OpeningKind::Window),
    ]);
    scene.mesh();
    // Shrink the left room: its edge 3 (x = 0) becomes 1.5 m long.
    let data = room_ops::move_edges(&scene.room(left), &[2], [0.0, -1.5]).expect("move");
    scene.update_room(left, data);
    scene.engine.evaluate_pending(&mut scene.doc);
    let updates = scene.engine.poll_updates(&scene.doc);
    let diag = updates.errors.iter().find(|(id, _)| *id == scene.layout).map(|(_, d)| d.clone());
    assert!(
        diag.as_ref().is_some_and(|d| d.kind == EvalErrorKind::Degenerate && d.message.contains("opening 1")),
        "{diag:?}"
    );
    assert_eq!(
        scene.engine.room_layout_issues(scene.layout),
        Some(&[LayoutIssue::OpeningDoesNotFit { opening: 1 }][..])
    );
    // The mesh is current: the shrunk room with its fitting window.
    let m = scene.engine.mesh(scene.layout).expect("mesh").clone();
    assert_watertight(&m);
    let (_, max) = mesh_bbox(&m);
    assert_near(max[1], 3.0 + T / 2.0);
    // Deleting the misfit clears the error.
    let data = layout_ops::delete_opening(&scene.input(), 1).expect("delete");
    scene.set_openings(data.openings);
    scene.engine.evaluate_pending(&mut scene.doc);
    let updates = scene.engine.poll_updates(&scene.doc);
    assert!(updates.errors_cleared.contains(&scene.layout));
    assert!(matches!(scene.engine.state(scene.layout), Some(EvalState::UpToDate { .. })));
}

#[test]
fn faces_are_named_by_room_edge_opening_and_height() {
    let mut scene = two_rooms();
    let (left, right) = (scene.rooms[0], scene.rooms[1]);
    scene.set_openings(vec![opening(4, left, 1, 1.0, 1.0, OpeningKind::Window)]);
    scene.mesh();
    let faces = |path: ProvenancePath| scene.engine.resolve_subref(&SubRef { owner: scene.layout, path });
    let wall = |room, edge, part| ProvenancePath::RoomWall { room, edge, part };
    let some = |r: Result<SubRefResolution, _>| matches!(r, Ok(SubRefResolution::Faces(n)) if n >= 1);
    // The shared wall: each side is named by the room it faces.
    assert!(some(faces(wall(left, 1, RoomWallPart::Inside))));
    assert!(some(faces(wall(right, 3, RoomWallPart::Inside))));
    // The outside of the bottom wall.
    assert!(some(faces(wall(left, 0, RoomWallPart::Outside))));
    assert!(some(faces(wall(left, 0, RoomWallPart::Inside))));
    assert!(some(faces(wall(left, 1, RoomWallPart::Opening { opening: 4 }))));
    assert!(some(faces(ProvenancePath::LayoutCap { z_um: 2_700_000, up: true })));
    assert!(some(faces(ProvenancePath::LayoutCap { z_um: 0, up: false })));
    assert!(some(faces(ProvenancePath::LayoutCap { z_um: 900_000, up: true })), "the sill");
}

#[test]
fn triangle_counts_are_minimal() {
    // One ring with two holes. The outside faces of the bottom and top
    // walls are two faces each (named by the left and the right room),
    // so the caps have 14 boundary vertices: 2 x (14 + 4 - 2) cap
    // triangles and 14 side quads.
    assert_eq!(two_rooms().mesh().indices.len() / 3, 60);
    // A single rectangle room: 2 x (8 - 2 + 2) + 8 x 2.
    assert_eq!(Scene::with_rooms(&[rect(0.0, 0.0, 4.0, 3.0)]).mesh().indices.len() / 3, 32);
}

// ---------------------------------------------------------------------
// Spaces, the top constraint, deletion, undo.
// ---------------------------------------------------------------------

#[test]
fn a_base_only_layout_is_level_local_and_root_drags_are_transform_only() {
    let mut scene = two_rooms();
    scene.mesh();
    let evals = scene.engine.eval_count(scene.layout);
    let tess = scene.engine.tessellation_count(scene.layout);
    set_elevation(&mut scene.doc, scene.ground, 1.5);
    scene.engine.evaluate_pending(&mut scene.doc);
    let updates = scene.engine.poll_updates(&scene.doc);
    assert!(updates.meshes.is_empty(), "no re-mesh");
    assert_eq!(updates.base_transforms.first().map(|t| t.transform[11]), Some(1.5));
    assert_eq!(scene.engine.eval_count(scene.layout), evals);
    assert_eq!(scene.engine.tessellation_count(scene.layout), tess);
}

fn topped() -> (Scene, EntityId) {
    let mut scene = Scene::new(Some(3.0));
    scene.add(&rect(0.0, 0.0, 4.0, 3.0), 0);
    scene.add(&rect(4.0, 0.0, 7.0, 3.0), 0);
    let second = scene
        .doc
        .entities()
        .find(|(_, r)| matches!(&r.params, vim_design_lib::Params::Level { name, .. } if name == "Level 2"))
        .map(|(id, _)| *id)
        .expect("Level 2");
    (scene, second)
}

#[test]
fn a_topped_layout_follows_its_top_plane() {
    let (mut scene, second) = topped();
    let footprint = two_room_volume() / H;
    assert_near(mesh_volume(&scene.mesh()), footprint * 3.0);
    assert_eq!(room_layout::layout_top_height(&scene.doc, scene.layout), Some(3.0));
    set_elevation(&mut scene.doc, second, 3.5);
    assert_near(mesh_volume(&scene.mesh()), footprint * 3.5);
}

#[test]
fn deleting_the_top_level_disconnects_the_layout() {
    let (mut scene, second) = topped();
    let before_volume = mesh_volume(&scene.mesh());
    let before = save(&scene.doc);
    ok(&mut scene.doc, Command::DeleteLevel { id: second, cascade: true });
    assert!(scene.doc.entity(scene.layout).is_some());
    assert_eq!(scene.layout_data().height_m, 3.0);
    let top = scene
        .doc
        .entity(scene.layout)
        .and_then(|r| r.inputs.get(vim_design_lib::entity::slot::ROOM_LAYOUT_TOP))
        .and_then(|s| s.referenced().next());
    assert_eq!(top, None);
    assert_near(mesh_volume(&scene.mesh()), before_volume);
    scene.doc.undo().expect("undo");
    assert_eq!(save(&scene.doc), before, "byte-exact undo");
    // Deleting the plane takes the layout and its rooms.
    ok(&mut scene.doc, Command::DeleteLevel { id: scene.ground, cascade: true });
    assert!(scene.doc.entity(scene.layout).is_none());
    assert!(scene.rooms.iter().all(|r| scene.doc.entity(*r).is_none()));
    scene.doc.undo().expect("undo");
    assert_eq!(save(&scene.doc), before);
}

#[test]
fn deleting_a_room_removes_it_from_its_layout_with_its_openings() {
    let mut scene = two_rooms();
    let (left, right) = (scene.rooms[0], scene.rooms[1]);
    scene.set_openings(vec![
        opening(0, left, 0, 1.0, 1.0, OpeningKind::Window),
        opening(1, right, 0, 1.0, 1.0, OpeningKind::Window),
    ]);
    let before = save(&scene.doc);
    let depth = scene.doc.undo_depth();
    ok(&mut scene.doc, Command::DeleteRoom { id: left });
    assert_eq!(scene.doc.undo_depth(), depth + 1, "one undo step");
    assert!(scene.doc.entity(left).is_none());
    assert_eq!(scene.input().rooms.len(), 1);
    assert_eq!(scene.layout_data().openings.iter().map(|o| o.id).collect::<Vec<_>>(), vec![1]);
    // The remaining room alone: its perimeter times the thickness, less its window.
    assert_near(mesh_volume(&scene.mesh()), 12.0 * T * H - 1.2 * T);
    scene.doc.undo().expect("undo");
    assert_eq!(save(&scene.doc), before, "byte-exact undo");
}

#[test]
fn updates_undo_byte_exactly_drags_coalesce_and_edits_reanchor_openings() {
    let mut scene = two_rooms();
    let left = scene.rooms[0];
    scene.set_openings(vec![opening(0, left, 0, 2.5, 1.0, OpeningKind::Window)]);
    let before = save(&scene.doc);
    // Insert a point on the bottom edge at 2 m: the window moves to the
    // new edge (start id 4) at 0.5 m, in the same undo step.
    let (data, new_id) = room_ops::insert_point(&scene.room(left), 0, 2.0).expect("insert");
    let depth = scene.doc.undo_depth();
    scene.update_room(left, data);
    assert_eq!(scene.doc.undo_depth(), depth + 1);
    let moved = scene.layout_data().openings[0];
    assert_eq!((moved.edge, moved.offset_m), (new_id, 0.5));
    let after = save(&scene.doc);
    scene.doc.undo().expect("undo");
    assert_eq!(save(&scene.doc), before);
    scene.doc.redo().expect("redo");
    assert_eq!(save(&scene.doc), after);

    // A layout height drag of 10 coalesced updates is one undo step.
    let depth = scene.doc.undo_depth();
    for step in 1..=10 {
        ok(
            &mut scene.doc,
            Command::UpdateRoomLayout {
                id: scene.layout,
                plane: None,
                top: None,
                rooms: None,
                thickness_m: None,
                height_m: Some(H + f64::from(step) * 0.05),
                top_offset_m: None,
                openings: None,
                coalesce: true,
            },
        );
    }
    assert_eq!(scene.doc.undo_depth(), depth + 1);
    scene.doc.undo().expect("undo");
    assert_eq!(save(&scene.doc), after);
}

#[test]
fn structural_problems_reject() {
    let mut scene = two_rooms();
    let bytes = save(&scene.doc);
    let (left, _) = (scene.rooms[0], scene.rooms[1]);
    let bad_room = |boundary, hidden: Vec<u32>| Command::CreateRoom {
        plane: scene.ground,
        name: "Bad".to_owned(),
        precedence: 0,
        boundary,
        hidden_edges: hidden,
        layout: None,
    };
    let two = room::from_rectangle("R", 0, [0.0, 0.0], [1.0, 1.0]).expect("rect").boundary;
    assert_eq!(scene.doc.submit(bad_room(two[..2].to_vec(), vec![])).err(), Some(VimStatus::InvalidRoom));
    assert_eq!(scene.doc.submit(bad_room(two.clone(), vec![9])).err(), Some(VimStatus::InvalidRoom));
    let layout_update = |openings, thickness| Command::UpdateRoomLayout {
        id: scene.layout,
        plane: None,
        top: None,
        rooms: None,
        thickness_m: thickness,
        height_m: None,
        top_offset_m: None,
        openings,
        coalesce: false,
    };
    assert_eq!(scene.doc.submit(layout_update(None, Some(0.0))).err(), Some(VimStatus::InvalidRoomLayout));
    let dup = vec![opening(0, left, 0, 1.0, 1.0, OpeningKind::Window); 2];
    assert_eq!(scene.doc.submit(layout_update(Some(dup), None)).err(), Some(VimStatus::InvalidRoomLayout));
    let stranger = vec![opening(0, scene.ground, 0, 1.0, 1.0, OpeningKind::Window)];
    assert_eq!(scene.doc.submit(layout_update(Some(stranger), None)).err(), Some(VimStatus::InvalidRoomLayout));
    // A room cannot be in two layouts, nor in a layout of another plane.
    assert_eq!(
        scene
            .doc
            .submit(Command::CreateRoomLayout {
                plane: scene.ground,
                top: None,
                rooms: vec![left],
                thickness_m: T,
                height_m: H,
                top_offset_m: 0.0,
                openings: vec![],
            })
            .err(),
        Some(VimStatus::InvalidRoomLayout)
    );
    assert_eq!(save(&scene.doc), bytes, "rejections change nothing");

    // A self-crossing room commits; the layout reports it and the rest evaluates.
    let bow = room::RoomData {
        name: "Bow".to_owned(),
        precedence: 0,
        boundary: room::from_rectangle("R", 0, [10.0, 0.0], [12.0, 2.0]).expect("rect").boundary,
        hidden_edges: vec![],
    };
    let mut crossed = bow.clone();
    crossed.boundary.swap(1, 2);
    assert_eq!(room::validate(&crossed), Err(RoomError::SelfIntersecting));
    let id = one(
        &mut scene.doc,
        Command::CreateRoom {
            plane: scene.ground,
            name: crossed.name.clone(),
            precedence: 0,
            boundary: crossed.boundary.clone(),
            hidden_edges: vec![],
            layout: Some(scene.layout),
        },
    );
    scene.engine.evaluate_pending(&mut scene.doc);
    let _ = scene.engine.poll_updates(&scene.doc);
    assert_eq!(
        scene.engine.room_layout_issues(scene.layout),
        Some(&[LayoutIssue::RoomInvalid { room: id, error: RoomError::SelfIntersecting }][..])
    );
    let m = scene.engine.mesh(scene.layout).expect("mesh");
    assert_near(mesh_volume(m), two_room_volume());
    let regions = scene.engine.room_regions(scene.layout).expect("regions");
    assert_eq!(regions.last().map(|r| r.status), Some(RegionStatus::Invalid(RoomError::SelfIntersecting)));
}

#[test]
fn elements_accept_rooms_and_layouts_and_sweep_them() {
    let mut scene = two_rooms();
    let element = one(
        &mut scene.doc,
        Command::CreateElement { name: "Rooms".to_owned(), members: vec![scene.layout], level: scene.ground },
    );
    scene.engine.evaluate_pending(&mut scene.doc);
    let updates = scene.engine.poll_updates(&scene.doc);
    assert!(updates.errors.is_empty(), "{:?}", updates.errors);
    let m = scene.engine.mesh(element).expect("element mesh");
    assert_near(mesh_volume(m), two_room_volume());
    ok(&mut scene.doc, Command::DeleteElement { id: element, sweep_orphans: true });
    assert!(scene.doc.entity(scene.layout).is_none(), "swept");
    assert!(scene.rooms.iter().all(|r| scene.doc.entity(*r).is_none()), "rooms swept");
    scene.doc.undo().expect("undo");
    // A room as an element member contributes no geometry.
    let room_element = one(
        &mut scene.doc,
        Command::CreateElement { name: "Room".to_owned(), members: vec![scene.rooms[0]], level: scene.ground },
    );
    scene.engine.evaluate_pending(&mut scene.doc);
    let updates = scene.engine.poll_updates(&scene.doc);
    assert!(updates.errors.is_empty(), "{:?}", updates.errors);
    assert!(scene.engine.mesh(room_element).is_none());
    // Deleting a room that an element holds is rejected.
    assert_eq!(
        scene.doc.submit(Command::DeleteRoom { id: scene.rooms[0] }).err(),
        Some(VimStatus::HasDependents)
    );
}

#[test]
fn the_default_partition_is_a_stud_with_gypsum_on_both_faces() {
    assert!((DEFAULT_PARTITION_THICKNESS_M - (0.089 + 2.0 * 0.0127)).abs() < 1e-3);
    assert_eq!(vim_design_lib::wall::DEFAULT_PARTITION_THICKNESS_M, DEFAULT_PARTITION_THICKNESS_M);
}

// ---------------------------------------------------------------------
// Operations.
// ---------------------------------------------------------------------

#[test]
fn room_operations() {
    let mut doc = Document::new();
    assert_eq!(room::default_name(&doc), "Room 001");
    let ground = level(&mut doc, "Ground", 0.0);
    let base = room::from_rectangle("Room 001", 0, [4.0, 3.0], [0.0, 0.0]).expect("rect");
    assert!(base.signed_area() > 0.0, "counter-clockwise");
    one(
        &mut doc,
        Command::CreateRoom {
            plane: ground,
            name: base.name.clone(),
            precedence: 0,
            boundary: base.boundary.clone(),
            hidden_edges: vec![],
            layout: None,
        },
    );
    assert_eq!(room::default_name(&doc), "Room 002");

    let moved = room_ops::move_points(&base, &[2], [1.0, 1.0]).expect("move");
    assert_eq!(moved.boundary[2].uv, [5.0, 4.0]);
    assert_eq!(moved.boundary[0].uv, base.boundary[0].uv);
    let set = room_ops::set_point(&base, 0, [-1.0, 0.0]).expect("set");
    assert_eq!(set.boundary[0].uv, [-1.0, 0.0]);
    let edges = room_ops::move_edges(&base, &[1], [1.0, 0.0]).expect("edges");
    assert_eq!((edges.boundary[1].uv, edges.boundary[2].uv), ([5.0, 0.0], [5.0, 3.0]));
    let hidden = room_ops::set_hidden(&base, &[0], true).expect("hide");
    let (split, id) = room_ops::insert_point(&hidden, 0, 1.0).expect("insert");
    assert_eq!(id, 4);
    assert_eq!(split.hidden_edges, vec![0, 4], "the new edge keeps the flag");
    let merged = room_ops::delete_points(&split, &[4]).expect("delete");
    assert_eq!(merged.hidden_edges, vec![0]);
    let collapsed = room_ops::delete_edges(&base, &[0]).expect("delete edge");
    assert_eq!(collapsed.edges(), vec![0, 2, 3]);
    assert_eq!(collapsed.boundary[0].uv, [0.0, 0.0], "the first point keeps its position");
    // Invalid results are errors.
    assert_eq!(room_ops::set_point(&base, 0, [4.0, 4.0]).err(), Some(RoomError::SelfIntersecting));
    assert_eq!(room_ops::delete_points(&base, &[0, 1]).err(), Some(RoomError::TooFewPoints));
    assert_eq!(room_ops::set_point(&base, 2, [4.0, 0.0]).err(), Some(RoomError::ZeroLengthEdge(1)));
    assert_eq!(room_ops::insert_point(&base, 0, 4.0).err(), Some(RoomError::InvalidParameter));
    let reversed = RoomData { boundary: base.boundary.iter().rev().copied().collect(), ..base.clone() };
    assert_eq!(room::validate(&reversed), Err(RoomError::Clockwise));
    assert_eq!(
        room::from_polygon("Flat", 0, &[[0.0, 0.0], [1.0, 0.0], [2.0, 0.0], [1.0, 0.0]]).err(),
        Some(RoomError::SelfIntersecting)
    );
}

#[test]
fn layout_operations() {
    let mut scene = two_rooms();
    let (left, right) = (scene.rooms[0], scene.rooms[1]);
    let third = scene.add(&rect(0.0, 3.0, 7.0, 5.0), 0);
    let input = scene.input();
    assert_eq!(layout_ops::ranking(&input), vec![left, right, third]);
    assert_eq!(layout_ops::bring_forward(&input, left), Ok(None));
    assert_eq!(layout_ops::bring_forward(&input, right), Ok(Some(1)));
    assert_eq!(layout_ops::send_backward(&input, right), Ok(Some(-1)));
    assert_eq!(layout_ops::send_backward(&input, third), Ok(None));
    assert_eq!(layout_ops::bring_to_front(&input, third), Ok(1));
    assert_eq!(layout_ops::send_to_back(&input, left), Ok(-1));
    assert_eq!(layout_ops::bring_forward(&input, scene.ground), Err(LayoutError::RoomNotInLayout(scene.ground)));

    let (data, id) = layout_ops::add_opening(&input, opening(99, left, 0, 1.0, 1.0, OpeningKind::Window)).expect("add");
    assert_eq!(id, 0);
    scene.set_openings(data.openings);
    let input = scene.input();
    // The bottom wall of the left room: an L corner at (0, 0) and a T at (4, 0).
    assert_eq!(room_layout::opening_span(&input, left, 0).expect("span").len(), 1);
    let at = |data: &RoomLayoutData| data.openings[0];
    let low = layout_ops::move_opening(&input, 0, [-10.0, -10.0]).expect("move");
    assert_near(at(&low).offset_m, T / 2.0);
    assert_eq!(at(&low).sill_m, 0.0);
    let high = layout_ops::move_opening(&input, 0, [10.0, 0.0]).expect("move");
    assert_near(at(&high).offset_m, 4.0 - T / 2.0 - 1.0);
    let wide = RoomOpening { width_m: 5.0, ..at(&input.layout) };
    assert_eq!(layout_ops::set_opening(&input, wide), Err(LayoutError::OpeningDoesNotFit(0)));
    let resized = layout_ops::set_opening(&input, RoomOpening { width_m: 2.0, ..at(&input.layout) }).expect("set");
    assert_eq!(at(&resized).width_m, 2.0);
    assert_eq!(layout_ops::delete_opening(&input, 7), Err(LayoutError::UnknownOpening(7)));
    let (rooms, data) = layout_ops::remove_room(&input, left);
    assert_eq!(rooms, vec![right, third]);
    assert!(data.openings.is_empty());
    assert_eq!(layout_ops::add_room(&rooms, left), vec![right, third, left]);
    assert_eq!(room_layout::validate(&input), Ok(()));
}

// ---------------------------------------------------------------------
// Performance.
// ---------------------------------------------------------------------

fn six_rooms() -> Scene {
    let mut scene = Scene::with_rooms(&[
        rect(0.0, 0.0, 4.0, 4.0),
        rect(4.0, 0.0, 7.0, 4.0),
        rect(7.0, 0.0, 11.0, 4.0),
        rect(0.0, 4.0, 5.0, 8.0),
        vec![[5.0, 4.0], [11.0, 4.0], [11.0, 8.0], [8.0, 8.0], [5.0, 6.5]],
        vec![[5.0, 6.5], [8.0, 8.0], [5.0, 8.0]],
    ]);
    let r = scene.rooms.clone();
    let window = |id, room, edge, offset| opening(id, room, edge, offset, 1.0, OpeningKind::Window);
    let door = |id, room, edge, offset| RoomOpening { height_m: 2.1, ..opening(id, room, edge, offset, 0.9, OpeningKind::Door) };
    scene.set_openings(vec![
        window(0, r[0], 0, 1.0),
        window(1, r[1], 0, 1.0),
        window(2, r[2], 0, 1.5),
        window(3, r[2], 1, 1.0),
        window(4, r[3], 3, 1.0),
        window(5, r[4], 1, 1.5),
        door(6, r[0], 1, 1.0),
        door(7, r[1], 1, 1.5),
        door(8, r[3], 0, 2.0),
        RoomOpening { depth_m: Some(0.05), ..window(9, r[3], 2, 1.5) },
    ]);
    scene
}

#[test]
fn a_six_room_layout_with_ten_openings() {
    let mut scene = six_rooms();
    let m = scene.mesh();
    assert!(scene.engine.room_layout_issues(scene.layout).is_some_and(|i| i.is_empty()));
    assert!(mesh_volume(&m) > 0.0);
}

/// Evaluation time of the six-room layout
/// (`cargo test --release -p vim-design-test --test rooms -- --ignored --nocapture`).
#[test]
#[ignore = "timing printout"]
fn print_room_layout_timing() {
    let scene = six_rooms();
    let bytes = save(&scene.doc);
    let rounds = 20;
    let start = std::time::Instant::now();
    let mut triangles = 0;
    for _ in 0..rounds {
        let mut doc = Document::load(&bytes).expect("load");
        let mut engine = Engine::new();
        engine.evaluate_pending(&mut doc);
        triangles = engine.mesh(scene.layout).map_or(0, |m| m.indices.len() / 3);
    }
    let whole = start.elapsed().as_secs_f64() * 1000.0 / f64::from(rounds);
    let input = scene.input();
    let start = std::time::Instant::now();
    for _ in 0..rounds {
        let _ = room_layout::validate(&input);
    }
    let arrangement = start.elapsed().as_secs_f64() * 1000.0 / f64::from(rounds);
    println!("six rooms, ten openings: {whole:.2} ms per evaluation (document), {triangles} triangles; validate {arrangement:.2} ms");
}

// ---------------------------------------------------------------------
// Random rooms.
// ---------------------------------------------------------------------

proptest::proptest! {
    #![proptest_config(proptest::prelude::ProptestConfig::with_cases(48))]

    /// Random overlapping grid rectangles with random precedence: the
    /// layout evaluates watertight, and the effective regions tile the
    /// union of the rooms exactly (no overlap, nothing lost).
    #[test]
    fn random_rooms_tile_their_union(
        rooms in proptest::collection::vec((0i32..6, 0i32..6, 1i32..4, 1i32..4, -2i32..3), 1..6),
    ) {
        let mut scene = Scene::new(None);
        let mut cells = std::collections::BTreeSet::new();
        for (x, y, w, h, precedence) in &rooms {
            scene.add(&rect(f64::from(*x), f64::from(*y), f64::from(x + w), f64::from(y + h)), *precedence);
            for cx in *x..(x + w) {
                for cy in *y..(y + h) {
                    cells.insert((cx, cy));
                }
            }
        }
        let m = scene.mesh();
        proptest::prop_assert!(mesh_volume(&m) > 0.0);
        let regions = scene.engine.room_regions(scene.layout).expect("regions");
        let total: f64 = regions.iter().map(|r| r.area_m2).sum();
        proptest::prop_assert!((total - cells.len() as f64).abs() < 1e-6, "{total} vs {}", cells.len());
    }
}

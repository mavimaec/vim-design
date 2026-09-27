//! Wall runs: joins at any angle, golden volumes, openings, profiles,
//! names, spaces, top constraints, the top-plane disconnect, commands,
//! undo, and the pure editing operations.

use vim_design_lib::eval::{Engine, EvalErrorKind, SubRefResolution};
use vim_design_lib::sketch::{Sketch, SketchFaceKind, ops as sketch_ops};
use vim_design_lib::subref::RunPart;
use vim_design_lib::wall_run::{
    self, MITER_LIMIT, Opening, OpeningKind, RunPoint, SegmentProfile, WallRunData, WallRunError,
    ops::{self, RunEnd},
};
use vim_design_lib::{Command, Document, EntityId, Mesh, Params, ProvenancePath, SubRef, VimStatus};
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

fn data(points: &[[f64; 2]], closed: bool) -> WallRunData {
    WallRunData {
        points: points
            .iter()
            .enumerate()
            .map(|(i, uv)| RunPoint { id: i as u32, uv: *uv })
            .collect(),
        closed,
        thickness_m: T,
        height_m: H,
        top_offset_m: 0.0,
        openings: vec![],
        profiles: vec![],
    }
}

fn create(run: &WallRunData, base: EntityId, top: Option<EntityId>) -> Command {
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
    }
}

fn update(id: EntityId, run: &WallRunData, coalesce: bool) -> Command {
    Command::UpdateWallRun {
        id,
        base: None,
        top: None,
        points: Some(run.points.clone()),
        closed: Some(run.closed),
        thickness_m: Some(run.thickness_m),
        height_m: Some(run.height_m),
        top_offset_m: Some(run.top_offset_m),
        openings: Some(run.openings.clone()),
        profiles: Some(run.profiles.clone()),
        coalesce,
    }
}

struct Placed {
    doc: Document,
    engine: Engine,
    ground: EntityId,
    run: EntityId,
    element: EntityId,
}

fn place(run: &WallRunData) -> Placed {
    let mut doc = Document::new();
    let ground = level(&mut doc, "Ground", 0.0);
    let id = one(&mut doc, create(run, ground, None));
    let element = one(
        &mut doc,
        Command::CreateElement { name: "Run".to_owned(), members: vec![id], level: ground },
    );
    let mut engine = Engine::new();
    engine.set_translation_factoring(true);
    Placed { doc, engine, ground, run: id, element }
}

fn mesh(placed: &mut Placed) -> Mesh {
    placed.engine.evaluate_pending(&mut placed.doc);
    let updates = placed.engine.poll_updates(&placed.doc);
    assert!(updates.errors.is_empty(), "{:?}", updates.errors);
    placed.engine.mesh(placed.element).expect("meshed").clone()
}

fn run_mesh(run: &WallRunData) -> Mesh {
    let mut placed = place(run);
    let m = mesh(&mut placed);
    assert_watertight(&m);
    m
}

fn assert_near(actual: f64, expected: f64) {
    assert!((actual - expected).abs() < 1e-5, "expected {expected}, got {actual}");
}

// ---------------------------------------------------------------------
// Reference footprints, computed independently of the implementation:
// the miter point of offset lines meeting at P is P + t (n0 + n1) /
// (1 + n0 . n1).
// ---------------------------------------------------------------------

fn left_normal(a: [f64; 2], b: [f64; 2]) -> [f64; 2] {
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    let l = (dx * dx + dy * dy).sqrt();
    [-dy / l, dx / l]
}

fn miter(p: [f64; 2], n0: [f64; 2], n1: [f64; 2], t: f64) -> [f64; 2] {
    let k = t / (1.0 + n0[0] * n1[0] + n0[1] * n1[1]);
    [p[0] + k * (n0[0] + n1[0]), p[1] + k * (n0[1] + n1[1])]
}

fn shoelace(polygon: &[[f64; 2]]) -> f64 {
    let n = polygon.len();
    (0..n)
        .map(|i| {
            let (a, b) = (polygon[i], polygon[(i + 1) % n]);
            a[0] * b[1] - a[1] * b[0]
        })
        .sum::<f64>()
        / 2.0
}

/// Footprint area of an open run with mitered joins (no bevels).
fn open_footprint(points: &[[f64; 2]], t: f64) -> f64 {
    let n = points.len();
    let normals: Vec<[f64; 2]> = points.windows(2).map(|w| left_normal(w[0], w[1])).collect();
    let mut polygon: Vec<[f64; 2]> = points.to_vec();
    let last = normals[n - 2];
    polygon.push([points[n - 1][0] + t * last[0], points[n - 1][1] + t * last[1]]);
    for i in (1..n - 1).rev() {
        polygon.push(miter(points[i], normals[i - 1], normals[i], t));
    }
    polygon.push([points[0][0] + t * normals[0][0], points[0][1] + t * normals[0][1]]);
    shoelace(&polygon).abs()
}

/// Footprint area of a closed run with mitered joins (no bevels).
fn closed_footprint(points: &[[f64; 2]], t: f64) -> f64 {
    let n = points.len();
    let normals: Vec<[f64; 2]> = (0..n).map(|i| left_normal(points[i], points[(i + 1) % n])).collect();
    let offset: Vec<[f64; 2]> =
        (0..n).map(|i| miter(points[i], normals[(i + n - 1) % n], normals[i], t)).collect();
    (shoelace(points) - shoelace(&offset)).abs()
}

fn polar(angle_deg: f64, length: f64, from: [f64; 2]) -> [f64; 2] {
    let a = angle_deg.to_radians();
    [from[0] + length * a.cos(), from[1] + length * a.sin()]
}

/// Two 4 m segments with `interior` degrees between them, turning left
/// (`left = true`) or right.
fn corner(interior: f64, left: bool) -> Vec<[f64; 2]> {
    let turn = 180.0 - interior;
    let heading = if left { turn } else { -turn };
    let p = [4.0, 0.0];
    vec![[0.0, 0.0], p, polar(heading, 4.0, p)]
}

// ---------------------------------------------------------------------
// Golden volumes.
// ---------------------------------------------------------------------

#[test]
fn a_straight_run_is_a_box_of_twelve_triangles() {
    let m = run_mesh(&data(&[[0.0, 0.0], [4.0, 0.0]], false));
    assert_near(mesh_volume(&m), 4.0 * T * H);
    assert_eq!(m.indices.len() / 3, 12);
    let (min, max) = mesh_bbox(&m);
    assert_near(min[1], 0.0);
    assert_near(max[1], T);
    assert_near(max[2], H);
}

#[test]
fn corners_at_any_angle_are_exact_miters() {
    for interior in [90.0, 60.0, 135.0] {
        for left in [true, false] {
            let points = corner(interior, left);
            let m = run_mesh(&data(&points, false));
            let expected = open_footprint(&points, T) * H;
            assert_near(mesh_volume(&m), expected);
        }
    }
    // The 90 degree left turn, by hand: 4 x 0.2 + 3 x 0.2 - 0.2 x 0.2.
    let m = run_mesh(&data(&[[0.0, 0.0], [4.0, 0.0], [4.0, 3.0]], false));
    assert_near(mesh_volume(&m), (0.8 + 0.6 - 0.04) * H);
}

#[test]
fn closed_triangle_and_pentagon_volumes_are_footprint_times_height() {
    let triangle = [[0.0, 0.0], [5.0, 0.0], [2.0, 4.0]];
    let pentagon: Vec<[f64; 2]> = (0..5).map(|i| polar(90.0 + 72.0 * f64::from(i), 3.0, [0.0, 0.0])).collect();
    for points in [triangle.to_vec(), pentagon.clone()] {
        // Counter-clockwise: material inside; clockwise: outside.
        for reversed in [false, true] {
            let mut p = points.clone();
            if reversed {
                p.reverse();
            }
            let m = run_mesh(&data(&p, true));
            assert_near(mesh_volume(&m), closed_footprint(&p, T) * H);
        }
    }
}

#[test]
fn sharp_outside_corners_are_beveled() {
    // Interior 20 degrees turning right: the outside miter would reach
    // t / sin(10 deg) = 5.8 t, past the limit.
    let points = corner(20.0, false);
    assert!(T / (10.0_f64.to_radians().sin()) > MITER_LIMIT * T);
    let m = run_mesh(&data(&points, false));
    // The bevel removes the miter triangle beyond the chord between the
    // two offset-line ends.
    let n0 = left_normal(points[0], points[1]);
    let n1 = left_normal(points[1], points[2]);
    let p = points[1];
    let q0 = [p[0] + T * n0[0], p[1] + T * n0[1]];
    let q1 = [p[0] + T * n1[0], p[1] + T * n1[1]];
    let m_point = miter(p, n0, n1, T);
    let cut = shoelace(&[q0, m_point, q1]).abs();
    assert_near(mesh_volume(&m), (open_footprint(&points, T) - cut) * H);
    // Inside a sharp turn the miter is exact (no limit applies there).
    let points = corner(20.0, true);
    let m = run_mesh(&data(&points, false));
    assert_near(mesh_volume(&m), open_footprint(&points, T) * H);
}

fn opening(id: u32, segment: u32, offset_m: f64, width_m: f64, kind: OpeningKind) -> Opening {
    Opening { id, segment, offset_m, sill_m: 0.9, width_m, height_m: 1.2, kind, depth_m: None }
}

#[test]
fn windows_doors_and_niches_remove_their_volume() {
    let points = [[0.0, 0.0], [5.0, 0.0], [5.0, 4.0]];
    let base = open_footprint(&points, T) * H;
    let mut run = data(&points, false);
    run.openings = vec![
        opening(0, 0, 0.5, 1.2, OpeningKind::Window),
        Opening { height_m: 2.1, ..opening(1, 0, 2.5, 0.9, OpeningKind::Door) },
        Opening { depth_m: Some(0.05), ..opening(2, 1, 1.0, 1.5, OpeningKind::Window) },
    ];
    let m = run_mesh(&run);
    let removed = 1.2 * 1.2 * T + 0.9 * 2.1 * T + 1.5 * 1.2 * 0.05;
    assert_near(mesh_volume(&m), base - removed);
    // The door cuts the bottom edge: nothing below the base.
    let (min, _) = mesh_bbox(&m);
    assert_near(min[2], 0.0);
}

/// A segment profile from an outline drawn at top reference `H`: points
/// at or above `H` are top-anchored.
fn profile_from(segment: u32, outline: &[[f64; 2]]) -> SegmentProfile {
    let effective = sketch_ops::add_face(&Sketch::default(), outline, SketchFaceKind::Solid { thickness: T })
        .expect("profile face");
    let top_points: Vec<u32> =
        effective.points.iter().filter(|p| p.uv[1] >= H - 1e-9).map(|p| p.id).collect();
    let profile = vim_design_lib::wall::stored_profile(&effective, &top_points, H);
    SegmentProfile { segment, profile, top_points }
}

fn gable(segment: u32, length: f64, rise: f64) -> SegmentProfile {
    profile_from(segment, &[[0.0, 0.0], [length, 0.0], [length, H], [length / 2.0, H + rise], [0.0, H]])
}

#[test]
fn a_gable_segment_profile() {
    // One segment: the elevation is the gable exactly.
    let mut run = data(&[[0.0, 0.0], [6.0, 0.0]], false);
    run.profiles = vec![gable(0, 6.0, 1.5)];
    let m = run_mesh(&run);
    assert_near(mesh_volume(&m), T * (6.0 * H + 6.0 * 1.5 / 2.0));
    let (_, max) = mesh_bbox(&m);
    assert_near(max[2], H + 1.5);

    // The gable on the second segment of a 90 degree corner: its sloped
    // top continues through the join wedge.
    let mut run = data(&[[0.0, 0.0], [4.0, 0.0], [4.0, 3.0]], false);
    run.profiles = vec![gable(1, 3.0, 1.0)];
    let m = run_mesh(&run);
    // Segment 0: its footprint quad at H. Segment 1: width s (the wedge)
    // for s < t, then t; height H + slope * s up to the apex, then down.
    let seg0 = (4.0 * T - T * T / 2.0) * H;
    let slope = 1.0 / 1.5;
    let wedge = H * T * T / 2.0 + slope * T.powi(3) / 3.0;
    let rest = T * (H * (3.0 - T) + 1.5 - slope * T * T / 2.0);
    assert_near(mesh_volume(&m), seg0 + wedge + rest);
}

#[test]
fn a_profile_that_is_not_a_band_at_a_join_is_an_error() {
    // The apex sits inside the join zone of the corner.
    let mut run = data(&[[0.0, 0.0], [4.0, 0.0], [4.0, 3.0]], false);
    run.profiles = vec![profile_from(1, &[[0.0, 0.0], [3.0, 0.0], [3.0, H], [0.1, H + 1.0], [0.0, H]])];
    // Structure and spans are fine; only the evaluation sees it.
    assert_eq!(wall_run::validate(&run), Ok(()));
    let mut placed = place(&run);
    placed.engine.evaluate_pending(&mut placed.doc);
    let updates = placed.engine.poll_updates(&placed.doc);
    let kind = updates.errors.iter().find(|(id, _)| *id == placed.run).map(|(_, d)| d.kind);
    assert_eq!(kind, Some(EvalErrorKind::Degenerate));
}

// ---------------------------------------------------------------------
// Names, triangle counts, spaces, top constraint.
// ---------------------------------------------------------------------

#[test]
fn faces_are_named_by_segment_and_part() {
    let points = [[0.0, 0.0], [5.0, 0.0], [5.0, 4.0]];
    let mut run = data(&points, false);
    run.openings = vec![
        opening(7, 0, 1.0, 1.2, OpeningKind::Window),
        Opening { depth_m: Some(0.05), ..opening(8, 1, 1.0, 1.0, OpeningKind::Window) },
    ];
    let mut placed = place(&run);
    mesh(&mut placed);
    let faces = |segment: u32, part: RunPart| {
        placed.engine.resolve_subref(&SubRef {
            owner: placed.run,
            path: ProvenancePath::RunFace { segment, part },
        })
    };
    for segment in [0, 1] {
        for part in [RunPart::Reference, RunPart::Opposite, RunPart::Top, RunPart::Bottom, RunPart::Start, RunPart::End] {
            assert!(
                matches!(faces(segment, part), Ok(SubRefResolution::Faces(n)) if n >= 1),
                "segment {segment} {part:?}: {:?}",
                faces(segment, part)
            );
        }
    }
    // A through window has four reveals; a niche has four and a back.
    assert_eq!(faces(0, RunPart::Opening { opening: 7 }), Ok(SubRefResolution::Faces(4)));
    assert_eq!(faces(1, RunPart::Opening { opening: 8 }), Ok(SubRefResolution::Faces(4)));
    assert_eq!(faces(1, RunPart::NicheBack { depth_um: 50_000 }), Ok(SubRefResolution::Faces(1)));
}

#[test]
fn triangle_counts_are_minimal() {
    let count = |run: &WallRunData| run_mesh(run).indices.len() / 3;
    // A 90 degree corner: an L-shaped prism (6 vertices a cap).
    assert_eq!(count(&data(&[[0.0, 0.0], [4.0, 0.0], [4.0, 3.0]], false)), 20);
    // A closed rectangle: a square ring (8 cap vertices, a hole).
    assert_eq!(count(&data(&[[0.0, 0.0], [4.0, 0.0], [4.0, 3.0], [0.0, 3.0]], true)), 32);
    // A window adds its four reveals and splits both faces.
    let mut run = data(&[[0.0, 0.0], [4.0, 0.0]], false);
    run.openings = vec![opening(0, 0, 1.0, 1.2, OpeningKind::Window)];
    assert_eq!(count(&run), 32);
}

#[test]
fn a_base_only_run_is_level_local_and_root_drags_are_transform_only() {
    let mut placed = place(&data(&[[0.0, 0.0], [4.0, 0.0], [4.0, 3.0]], false));
    mesh(&mut placed);
    let evals = placed.engine.eval_count(placed.run);
    let tess = placed.engine.tessellation_count(placed.element);
    set_elevation(&mut placed.doc, placed.ground, 1.5);
    placed.engine.evaluate_pending(&mut placed.doc);
    let updates = placed.engine.poll_updates(&placed.doc);
    assert!(updates.meshes.is_empty(), "no re-mesh");
    assert_eq!(updates.base_transforms.first().map(|t| t.transform[11]), Some(1.5));
    assert_eq!(placed.engine.eval_count(placed.run), evals);
    assert_eq!(placed.engine.tessellation_count(placed.element), tess);
}

fn topped_run(top_offset_m: f64) -> (Placed, EntityId) {
    let mut doc = Document::new();
    let ground = level(&mut doc, "Ground", 0.0);
    let second = level(&mut doc, "Level 2", 3.0);
    let mut run = data(&[[0.0, 0.0], [4.0, 0.0], [4.0, 3.0]], false);
    run.top_offset_m = top_offset_m;
    run.height_m = 9.0;
    run.openings = vec![opening(0, 0, 1.0, 1.2, OpeningKind::Window)];
    let id = one(&mut doc, create(&run, ground, Some(second)));
    let element = one(
        &mut doc,
        Command::CreateElement { name: "Run".to_owned(), members: vec![id], level: ground },
    );
    let mut engine = Engine::new();
    engine.set_translation_factoring(true);
    (Placed { doc, engine, ground, run: id, element }, second)
}

#[test]
fn a_topped_run_follows_its_top_plane_and_windows_keep_their_sill() {
    let (mut placed, second) = topped_run(-0.3);
    let footprint = open_footprint(&[[0.0, 0.0], [4.0, 0.0], [4.0, 3.0]], T);
    let window = 1.2 * 1.2 * T;
    let before = mesh(&mut placed);
    assert_near(mesh_volume(&before), footprint * 2.7 - window);
    assert_eq!(wall_run::run_top_height(&placed.doc, placed.run), Some(3.0 - 0.3));

    set_elevation(&mut placed.doc, second, 3.5);
    let after = mesh(&mut placed);
    assert_near(mesh_volume(&after), footprint * 3.2 - window);
    let (_, max) = mesh_bbox(&after);
    assert_near(max[2], 3.2);
    assert!(after.positions.iter().any(|p| (f64::from(p[2]) - 0.9).abs() < 1e-6), "sill kept");
}

fn run_top(doc: &Document, run: EntityId) -> Option<EntityId> {
    doc.entity(run)
        .and_then(|r| r.inputs.get(vim_design_lib::entity::slot::WALL_RUN_TOP))
        .and_then(|s| s.referenced().next())
}

fn run_data(doc: &Document, run: EntityId) -> WallRunData {
    WallRunData::from_params(&doc.entity(run).expect("run").params).expect("run params")
}

#[test]
fn deleting_the_top_level_disconnects_the_run() {
    let (mut placed, second) = topped_run(-0.3);
    let before_mesh = mesh(&mut placed);
    let before = save(&placed.doc);
    let depth = placed.doc.undo_depth();

    ok(&mut placed.doc, Command::DeleteLevel { id: second, cascade: true });
    assert_eq!(placed.doc.undo_depth(), depth + 1, "one undo step");
    assert!(placed.doc.entity(placed.run).is_some(), "the run survives");
    assert_eq!(run_top(&placed.doc, placed.run), None);
    assert_eq!(run_data(&placed.doc, placed.run).height_m, 3.0 - 0.3);
    let after_mesh = mesh(&mut placed);
    assert_near(mesh_volume(&after_mesh), mesh_volume(&before_mesh));

    placed.doc.undo().expect("undo");
    assert_eq!(save(&placed.doc), before, "byte-exact undo");
    placed.doc.redo().expect("redo");
    assert_eq!(run_top(&placed.doc, placed.run), None);

    // Deleting the BASE level takes the run (and its element).
    placed.doc.undo().expect("undo");
    ok(&mut placed.doc, Command::DeleteLevel { id: placed.ground, cascade: true });
    assert!(placed.doc.entity(placed.run).is_none());
    assert!(placed.doc.entity(placed.element).is_none());
    placed.doc.undo().expect("undo");
    assert_eq!(save(&placed.doc), before, "byte-exact undo");
}

#[test]
fn a_workplane_cascade_disconnects_runs_that_reach_it() {
    let mut doc = Document::new();
    let ground = level(&mut doc, "Ground", 0.0);
    let ceiling = one(
        &mut doc,
        Command::CreateWorkplane {
            parent: ground,
            name: "Ceiling".to_owned(),
            offset_m: 2.4,
            color: [0.5; 4],
            extent_m: 8.0,
        },
    );
    let run = data(&[[0.0, 0.0], [4.0, 0.0]], false);
    let topped = one(&mut doc, create(&run, ground, Some(ceiling)));
    let on_it = one(&mut doc, create(&run, ceiling, None));
    let before = save(&doc);
    ok(&mut doc, Command::DeleteWorkplaneCascade { id: ceiling });
    assert!(doc.entity(on_it).is_none());
    assert_eq!(run_top(&doc, topped), None);
    assert_eq!(run_data(&doc, topped).height_m, 2.4);
    doc.undo().expect("undo");
    assert_eq!(save(&doc), before);
}

// ---------------------------------------------------------------------
// Commands, undo, validation tiers, ownership.
// ---------------------------------------------------------------------

#[test]
fn update_undo_redo_is_byte_exact_and_drags_coalesce() {
    let mut placed = place(&data(&[[0.0, 0.0], [4.0, 0.0], [4.0, 3.0]], false));
    let mesh_before = mesh(&mut placed);
    let bytes_before = save(&placed.doc);
    let run = run_data(&placed.doc, placed.run);
    let (edited, _) = ops::add_opening(&run, opening(0, 0, 1.0, 1.2, OpeningKind::Window)).expect("add");
    ok(&mut placed.doc, update(placed.run, &edited, false));
    let bytes_after = save(&placed.doc);
    placed.doc.undo().expect("undo");
    assert_eq!(save(&placed.doc), bytes_before);
    assert_eq!(mesh(&mut placed).indices, mesh_before.indices);
    placed.doc.redo().expect("redo");
    assert_eq!(save(&placed.doc), bytes_after);

    // A point drag of 10 coalesced updates is one undo step.
    let depth = placed.doc.undo_depth();
    let mut current = edited;
    for _ in 0..10 {
        current = ops::move_points(&current, &[2], [0.1, 0.0]).expect("move");
        ok(&mut placed.doc, update(placed.run, &current, true));
    }
    assert_eq!(placed.doc.undo_depth(), depth + 1);
    placed.doc.undo().expect("undo drag");
    assert_eq!(save(&placed.doc), bytes_after);
}

#[test]
fn structural_problems_reject_and_geometric_ones_are_evaluation_errors() {
    let mut doc = Document::new();
    let ground = level(&mut doc, "Ground", 0.0);
    let bytes = save(&doc);
    let reject = |doc: &mut Document, run: &WallRunData| doc.submit(create(run, ground, None)).err();
    let mut bad = data(&[[0.0, 0.0]], false);
    assert_eq!(reject(&mut doc, &bad), Some(VimStatus::InvalidWallRun));
    bad = data(&[[0.0, 0.0], [1.0, 0.0]], false);
    bad.thickness_m = 0.0;
    assert_eq!(reject(&mut doc, &bad), Some(VimStatus::InvalidWallRun));
    bad = data(&[[0.0, 0.0], [1.0, 0.0]], false);
    bad.points[1].id = 0;
    assert_eq!(reject(&mut doc, &bad), Some(VimStatus::InvalidWallRun));
    bad = data(&[[0.0, 0.0], [1.0, 0.0]], false);
    bad.openings = vec![opening(0, 9, 0.1, 0.5, OpeningKind::Window)];
    assert_eq!(reject(&mut doc, &bad), Some(VimStatus::InvalidWallRun));
    bad = data(&[[0.0, 0.0], [1.0, f64::NAN]], false);
    assert_eq!(reject(&mut doc, &bad), Some(VimStatus::InvalidWallRun));
    assert_eq!(save(&doc), bytes, "rejections change nothing");

    // A window in the join zone commits but does not evaluate.
    let mut geometric = data(&[[0.0, 0.0], [4.0, 0.0], [4.0, 3.0]], false);
    geometric.openings = vec![opening(0, 0, 3.0, 0.9, OpeningKind::Window)];
    assert_eq!(
        wall_run::validate(&geometric),
        Err(WallRunError::OpeningOutsideClearSpan(0))
    );
    let id = one(&mut doc, create(&geometric, ground, None));
    let mut engine = Engine::new();
    engine.evaluate_pending(&mut doc);
    let updates = engine.poll_updates(&doc);
    let kind = updates.errors.iter().find(|(e, _)| *e == id).map(|(_, d)| d.kind);
    assert_eq!(kind, Some(EvalErrorKind::Degenerate));
    assert_eq!(
        doc.submit(Command::DeleteWallRun { id: ground }).err(),
        Some(VimStatus::WrongEntityKind)
    );
}

#[test]
fn deleting_the_element_sweeps_the_run() {
    let mut placed = place(&data(&[[0.0, 0.0], [4.0, 0.0]], false));
    ok(
        &mut placed.doc,
        Command::DeleteElement { id: placed.element, sweep_orphans: true },
    );
    assert!(placed.doc.entity(placed.run).is_none(), "swept");
    placed.doc.undo().expect("undo");
    ok(&mut placed.doc, Command::DeleteElement { id: placed.element, sweep_orphans: false });
    ok(&mut placed.doc, Command::DeleteWallRun { id: placed.run });
    assert!(placed.doc.entity(placed.run).is_none());
}

// ---------------------------------------------------------------------
// Editing operations.
// ---------------------------------------------------------------------

fn uv(run: &WallRunData, id: u32) -> [f64; 2] {
    run.points.iter().find(|p| p.id == id).map(|p| p.uv).expect("point")
}

fn segments(run: &WallRunData) -> Vec<u32> {
    run.segments()
}

#[test]
fn moving_points_and_edges() {
    let mut run = data(&[[0.0, 0.0], [4.0, 0.0], [4.0, 3.0]], false);
    run.openings = vec![opening(0, 0, 1.0, 1.0, OpeningKind::Window)];
    let moved = ops::move_points(&run, &[2], [1.0, 0.5]).expect("move");
    assert_eq!(uv(&moved, 2), [5.0, 3.5]);
    assert_eq!(uv(&moved, 0), uv(&run, 0), "others bit for bit");
    assert_eq!(moved.openings, run.openings);
    let set = ops::set_point(&run, 0, [-1.0, 0.0]).expect("set");
    assert_eq!(uv(&set, 0), [-1.0, 0.0]);
    let edges = ops::move_edges(&run, &[1], [0.5, 0.0]).expect("move edge");
    assert_eq!((uv(&edges, 1), uv(&edges, 2)), ([4.5, 0.0], [4.5, 3.0]));
    // Invalid results are errors, and nothing changes.
    assert_eq!(ops::set_point(&run, 2, [4.0, 0.0]), Err(WallRunError::ZeroLengthSegment(1)));
    assert_eq!(ops::set_point(&run, 0, [3.5, 0.0]), Err(WallRunError::OpeningOutsideClearSpan(0)));
    assert_eq!(ops::move_points(&run, &[9], [1.0, 0.0]), Err(WallRunError::UnknownPoint(9)));
    assert_eq!(ops::move_edges(&run, &[2], [1.0, 0.0]), Err(WallRunError::UnknownSegment(2)));
}

#[test]
fn inserting_a_point_reassigns_openings_and_splits_the_profile() {
    let mut run = data(&[[0.0, 0.0], [6.0, 0.0]], false);
    run.openings = vec![
        opening(0, 0, 0.5, 1.0, OpeningKind::Window),
        opening(1, 0, 4.0, 1.0, OpeningKind::Window),
    ];
    let (split, id) = ops::insert_point(&run, 0, 3.0, H).expect("insert");
    assert_eq!(id, 2);
    assert_eq!(split.points.iter().map(|p| p.id).collect::<Vec<_>>(), vec![0, 2, 1]);
    assert_eq!(uv(&split, 2), [3.0, 0.0]);
    assert_eq!((split.openings[0].segment, split.openings[0].offset_m), (0, 0.5));
    assert_eq!((split.openings[1].segment, split.openings[1].offset_m), (2, 1.0));
    assert_eq!(
        ops::insert_point(&run, 0, 4.5, H).map(|_| ()),
        Err(WallRunError::OpeningStraddlesSplit(1))
    );
    assert_eq!(ops::insert_point(&run, 0, 6.0, H).map(|_| ()), Err(WallRunError::InvalidParameter));

    // A gable splits at the new point; the volume does not change.
    let mut run = data(&[[0.0, 0.0], [6.0, 0.0]], false);
    run.profiles = vec![gable(0, 6.0, 1.5)];
    let (split, id) = ops::insert_point(&run, 0, 2.0, H).expect("insert");
    assert_eq!(split.profiles.iter().map(|p| p.segment).collect::<Vec<_>>(), vec![0, id]);
    let expected = T * (6.0 * H + 6.0 * 1.5 / 2.0);
    assert_near(mesh_volume(&run_mesh(&run)), expected);
    assert_near(mesh_volume(&run_mesh(&split)), expected);
    // The part after the split starts at u = 0 and keeps its anchors.
    let second = &split.profiles[1];
    assert!(second.profile.points.iter().all(|p| p.uv[0] >= -1e-9 && p.uv[0] <= 4.0 + 1e-9));
    assert!(!second.top_points.is_empty());
}

#[test]
fn deleting_points_merges_segments_and_keeps_opening_positions() {
    let mut run = data(&[[0.0, 0.0], [2.0, 0.0], [4.0, 0.0], [4.0, 3.0]], false);
    run.openings = vec![opening(0, 1, 0.5, 1.0, OpeningKind::Window)];
    let merged = ops::delete_points(&run, &[1]).expect("delete");
    assert_eq!(segments(&merged), vec![0, 2]);
    assert_eq!((merged.openings[0].segment, merged.openings[0].offset_m), (0, 2.5));
    // Deleting the corner would move the opening off the wall line.
    let mut corner_opening = run.clone();
    corner_opening.openings = vec![opening(0, 2, 1.0, 1.0, OpeningKind::Window)];
    assert_eq!(ops::delete_points(&corner_opening, &[2]), Err(WallRunError::OpeningDisplaced(0)));
    // An open run's end point shortens it; too few points is an error.
    let shorter = ops::delete_points(&run, &[3]).expect("delete end");
    assert_eq!(segments(&shorter), vec![0, 1]);
    assert_eq!(ops::delete_points(&data(&[[0.0, 0.0], [1.0, 0.0]], false), &[1]), Err(WallRunError::TooFewPoints));
}

#[test]
fn deleting_edges_merges_into_the_first_point() {
    let mut run = data(&[[0.0, 0.0], [4.0, 0.0], [4.0, 3.0], [0.0, 3.0]], false);
    run.openings = vec![
        opening(0, 1, 1.0, 1.0, OpeningKind::Window),
        opening(1, 0, 1.0, 1.0, OpeningKind::Window),
    ];
    let merged = ops::delete_edges(&run, &[1]).expect("delete edge");
    assert_eq!(merged.points.iter().map(|p| p.id).collect::<Vec<_>>(), vec![0, 1, 3]);
    assert_eq!(uv(&merged, 1), [4.0, 0.0], "the first point keeps its position");
    assert_eq!(merged.openings.iter().map(|o| o.id).collect::<Vec<_>>(), vec![1], "the edge's openings go");
    // In a closed run the closing edge merges the first point into the last.
    let closed = data(&[[0.0, 0.0], [4.0, 0.0], [4.0, 3.0], [0.0, 3.0]], true);
    let merged = ops::delete_edges(&closed, &[3]).expect("delete closing edge");
    assert_eq!(merged.points.iter().map(|p| p.id).collect::<Vec<_>>(), vec![1, 2, 3]);
    assert!(merged.closed);
}

#[test]
fn extending_and_closing() {
    let run = data(&[[0.0, 0.0], [4.0, 0.0]], false);
    let (longer, id) = ops::extend(&run, RunEnd::End, [4.0, 3.0]).expect("extend end");
    assert_eq!((id, segments(&longer)), (2, vec![0, 1]));
    let (longer, id) = ops::extend(&longer, RunEnd::Start, [0.0, 3.0]).expect("extend start");
    assert_eq!(id, 3);
    assert_eq!(longer.points.first().map(|p| p.id), Some(3));
    let mut closed = ops::set_closed(&longer, true).expect("close");
    assert_eq!(segments(&closed), vec![3, 0, 1, 2]);
    assert_near(
        mesh_volume(&run_mesh(&closed)),
        closed_footprint(&[[0.0, 3.0], [0.0, 0.0], [4.0, 0.0], [4.0, 3.0]], T) * H,
    );
    assert_eq!(ops::extend(&closed, RunEnd::End, [9.0, 9.0]).map(|_| ()), Err(WallRunError::WrongRunKind));
    // Opening the run removes the closing segment with its openings.
    closed.openings = vec![opening(0, 2, 1.0, 1.0, OpeningKind::Window)];
    let open = ops::set_closed(&closed, false).expect("open");
    assert!(!open.closed && open.openings.is_empty());
    assert_eq!(segments(&open), vec![3, 0, 1]);
}

#[test]
fn opening_operations_clamp_to_the_clear_span() {
    let run = data(&[[0.0, 0.0], [4.0, 0.0], [4.0, 3.0]], false);
    assert_eq!(wall_run::segment_clear_span(&run, 1), Ok((0.2, 3.0)));
    assert_eq!(wall_run::segment_clear_span(&run, 0), Ok((0.0, 3.8)));
    let (run, id) = ops::add_opening(&run, opening(99, 1, 1.0, 1.0, OpeningKind::Window)).expect("add");
    assert_eq!(id, 0, "the next free id");
    let (run, second) = ops::add_opening(&run, opening(0, 0, 1.0, 1.0, OpeningKind::Door)).expect("add");
    assert_eq!(second, 1);
    let at = |run: &WallRunData, id: u32| run.openings.iter().find(|o| o.id == id).copied().expect("opening");
    let low = ops::move_opening(&run, id, [-10.0, -10.0]).expect("move");
    assert_eq!((at(&low, id).offset_m, at(&low, id).sill_m), (0.2, 0.0));
    let high = ops::move_opening(&run, id, [10.0, 0.5]).expect("move");
    assert_near(at(&high, id).offset_m, 2.0);
    assert_near(at(&high, id).sill_m, 1.4);
    let door = ops::move_opening(&run, second, [0.0, 1.0]).expect("move door");
    assert_eq!(at(&door, second).sill_m, at(&run, second).sill_m, "a door stays on the base");
    let wide = Opening { width_m: 3.5, ..at(&run, id) };
    assert_eq!(ops::set_opening(&run, wide), Err(WallRunError::OpeningOutsideClearSpan(id)));
    let resized = ops::set_opening(&run, Opening { width_m: 1.5, ..at(&run, id) }).expect("set");
    assert_eq!(at(&resized, id).width_m, 1.5);
    let overlapping = Opening { offset_m: 1.5, ..at(&run, second) };
    assert_eq!(ops::set_opening(&run, overlapping).err(), None, "another segment");
    assert_eq!(
        ops::add_opening(&run, opening(0, 1, 1.5, 1.0, OpeningKind::Window)).map(|_| ()),
        Err(WallRunError::OpeningsOverlap(0, 2))
    );
    let fewer = ops::delete_opening(&run, id).expect("delete");
    assert_eq!(fewer.openings.len(), 1);
    assert_eq!(ops::delete_opening(&run, 42), Err(WallRunError::UnknownOpening(42)));
}

// ---------------------------------------------------------------------
// Conversion from walls.
// ---------------------------------------------------------------------

fn m4_wall(doc: &mut Document, base: EntityId, start: [f64; 2], end: [f64; 2], void: Option<[[f64; 2]; 4]>) -> EntityId {
    let length = ((end[0] - start[0]).powi(2) + (end[1] - start[1]).powi(2)).sqrt();
    let (mut profile, mut top_points) = vim_design_lib::wall::default_profile(length, T);
    if let Some(corners) = void {
        (profile, top_points) = vim_design_lib::wall::ops::add_face(
            &profile,
            &top_points,
            H,
            &corners,
            SketchFaceKind::Void { depth: None },
        )
        .expect("void");
    }
    one(
        doc,
        Command::CreateWall { base, top: None, start, end, height_m: H, top_offset_m: 0.0, profile, top_points },
    )
}

#[test]
fn connected_walls_convert_into_one_run() {
    let mut doc = Document::new();
    let ground = level(&mut doc, "Ground", 0.0);
    let a = m4_wall(&mut doc, ground, [0.0, 0.0], [5.0, 0.0], Some([[1.0, 0.9], [2.2, 0.9], [2.2, 2.1], [1.0, 2.1]]));
    let b = m4_wall(&mut doc, ground, [5.0, 0.0], [5.0, 4.0], Some([[1.0, -0.5], [1.9, -0.5], [1.9, 2.1], [1.0, 2.1]]));
    let c = m4_wall(&mut doc, ground, [5.0, 4.0], [0.0, 0.0], None);
    let (run, base, top) = wall_run::from_walls(&doc, &[a, b, c]).expect("convert");
    assert_eq!((base, top), (ground, None));
    assert!(run.closed);
    assert_eq!(run.points.len(), 3);
    assert!(run.profiles.is_empty(), "plain walls need no profile");
    assert_eq!(run.openings.len(), 2);
    let window = run.openings[0];
    assert_eq!((window.kind, window.segment, window.offset_m, window.sill_m), (OpeningKind::Window, 0, 1.0, 0.9));
    let door = run.openings[1];
    assert_eq!((door.kind, door.segment, door.height_m), (OpeningKind::Door, 1, 2.1));
    let id = one(&mut doc, create(&run, base, top));
    let mut engine = Engine::new();
    engine.evaluate_pending(&mut doc);
    assert!(engine.poll_updates(&doc).errors.is_empty());
    assert!(matches!(doc.entity(id).map(|r| &r.params), Some(Params::WallRun { .. })));

    // Walls that do not connect, or are not walls, do not convert.
    let far = m4_wall(&mut doc, ground, [9.0, 9.0], [10.0, 9.0], None);
    assert_eq!(
        wall_run::from_walls(&doc, &[a, far]).map(|_| ()),
        Err(wall_run::FromWallsError::NotConnected(far))
    );
    assert_eq!(
        wall_run::from_walls(&doc, &[ground]).map(|_| ()),
        Err(wall_run::FromWallsError::NotAWall(ground))
    );
    assert_eq!(wall_run::from_walls(&doc, &[]).map(|_| ()), Err(wall_run::FromWallsError::NoWalls));
}

// ---------------------------------------------------------------------
// Prototype measurements.
// ---------------------------------------------------------------------

/// Evaluation time per segment and triangle counts
/// (`cargo test --release -p vim-design-test --test wall_runs -- --ignored --nocapture`).
#[test]
#[ignore]
fn print_wall_run_timings() {
    let cases: Vec<(&str, WallRunData)> = vec![
        ("straight", data(&[[0.0, 0.0], [4.0, 0.0]], false)),
        ("L 90", data(&[[0.0, 0.0], [4.0, 0.0], [4.0, 3.0]], false)),
        ("rectangle closed", data(&[[0.0, 0.0], [4.0, 0.0], [4.0, 3.0], [0.0, 3.0]], true)),
        ("pentagon closed", data(&(0..5).map(|i| polar(90.0 + 72.0 * f64::from(i), 3.0, [0.0, 0.0])).collect::<Vec<_>>(), true)),
        ("L with 3 openings", {
            let mut r = data(&[[0.0, 0.0], [5.0, 0.0], [5.0, 4.0]], false);
            r.openings = vec![
                opening(0, 0, 0.5, 1.2, OpeningKind::Window),
                Opening { height_m: 2.1, ..opening(1, 0, 2.5, 0.9, OpeningKind::Door) },
                Opening { depth_m: Some(0.05), ..opening(2, 1, 1.0, 1.5, OpeningKind::Window) },
            ];
            r
        }),
        ("gable corner", {
            let mut r = data(&[[0.0, 0.0], [4.0, 0.0], [4.0, 3.0]], false);
            r.profiles = vec![gable(1, 3.0, 1.0)];
            r
        }),
    ];
    for (name, run) in cases {
        let segments = run.segment_count();
        let rounds = 20;
        let start = std::time::Instant::now();
        let mut triangles = 0;
        for _ in 0..rounds {
            let m = run_mesh(&run);
            triangles = m.indices.len() / 3;
        }
        let per_segment = start.elapsed().as_secs_f64() * 1000.0 / f64::from(rounds) / segments as f64;
        println!("{name:<20} segments {segments} triangles {triangles:>3} {per_segment:.3} ms/segment");
    }
}

// ---------------------------------------------------------------------
// Random runs.
// ---------------------------------------------------------------------

proptest::proptest! {
    #![proptest_config(proptest::prelude::ProptestConfig::with_cases(64))]

    /// Any valid open run with turns up to 140 degrees (no bevels) meshes
    /// watertight to its mitered footprint times its height.
    #[test]
    fn random_open_runs_are_exact(
        turns in proptest::collection::vec(-140i16..=140, 1..5),
        lengths in proptest::collection::vec(20u8..60, 5),
    ) {
        let mut points = vec![[0.0, 0.0]];
        let mut heading = 0.0_f64;
        for (i, turn) in std::iter::once(&0i16).chain(turns.iter()).enumerate() {
            heading += f64::from(*turn);
            let last = *points.last().expect("a point");
            points.push(polar(heading, f64::from(lengths[i % lengths.len()]) * 0.1, last));
        }
        let run = data(&points, false);
        if wall_run::validate(&run).is_ok() {
            let m = run_mesh(&run);
            let v = mesh_volume(&m);
            let expected = open_footprint(&points, T) * H;
            // f32 positions: a relative tolerance.
            proptest::prop_assert!((v - expected).abs() < 1e-5 * expected.max(1.0), "{v} vs {expected}");
        }
    }
}

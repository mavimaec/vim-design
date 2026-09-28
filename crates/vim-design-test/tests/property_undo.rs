//! Test plan (e), property part (docs/ARCHITECTURE.md §12):
//! for any random command sequence, undo-all restores a state that
//! serializes byte-identically to the initial save, and redo-all
//! restores the final save.

use proptest::prelude::*;
use vim_design_lib::sketch::{Sketch, SketchDirection, SketchFaceKind, ops};
use vim_design_lib::wall::{self, ops as wall_ops};
use vim_design_lib::room::{self, RoomData, ops as room_ops};
use vim_design_lib::room_layout::{self, RoomLayoutData, RoomOpening, ops as layout_ops};
use vim_design_lib::wall_run::{
    self, Opening, OpeningKind, RunPoint, WallRunData,
    ops::{self as run_ops, RunEnd},
};
use vim_design_lib::{Command, Document, EntityId, EntityKind, Params};

/// Abstract operations; indexes are resolved against the entities that
/// exist when the op runs (mod count), so every generated sequence is
/// meaningful. Ops that cannot apply (no candidate of the right kind)
/// degrade to a control-point create so sequences stay non-trivial.
#[derive(Debug, Clone)]
enum Op {
    CreateCp(i16, i16, i16),
    CreateLine(usize, usize),
    CreateSpline(Vec<usize>),
    CreateEdge(usize),
    CreateWire(usize),
    CreateFace(usize),
    UpdateCp { pick: usize, x: i16, coalesce: bool },
    DeleteAny(usize),
    CreateCylinder { x: i16, r: u8, h: u8 },
    // Authoring kinds (docs/AUTHORING.md): the Site singleton (duplicate
    // creates reject — that's the point), levels, plane attachment, and
    // the cascade delete (a whole command group to invert).
    CreateSite(i16),
    CreateLevel(i16),
    CreateElement { member: usize, level: usize },
    DeleteElement { pick: usize, sweep: bool },
    // Sketches: creation on a level, then random edits through the
    // topology operations, each stored with one UpdateSketch.
    CreateSketch { level: usize, x: i8, w: u8, h: u8, void: bool },
    EditSketch { pick: usize, op: u8, a: usize, b: usize, x: i8, coalesce: bool },
    // Workplanes and walls: nested planes, walls with and without a top
    // constraint, and random profile edits through the anchor-keeping
    // wall operations.
    CreateWorkplane { parent: usize, offset: i8 },
    CreateWall { base: usize, top: Option<usize>, len: u8 },
    EditWall { pick: usize, op: u8, a: usize, x: i8, coalesce: bool },
    UpdateLevelElevation { pick: usize, elevation: i16, coalesce: bool },
    AttachCp { cp: usize, level: usize, detach: bool },
    DeleteLevelCascade(usize),
    // Wall runs: creation (open or closed, with or without a top), random
    // edits through the pure run operations, and the workplane cascade
    // (which disconnects topped runs and walls).
    CreateWallRun { base: usize, top: Option<usize>, corners: u8, closed: bool },
    EditWallRun { pick: usize, op: u8, a: usize, x: i8, coalesce: bool },
    DeleteWorkplaneCascade(usize),
    // Rooms and room layouts: layouts on planes, rooms added to them,
    // boundary edits through the room operations (which re-anchor the
    // layout's openings), openings through the layout operations, and
    // the room delete that also edits its layout.
    CreateRoomLayout { plane: usize, top: Option<usize> },
    CreateRoom { layout: usize, x: i8, y: i8, w: u8, h: u8, precedence: i8 },
    EditRoom { pick: usize, op: u8, a: usize, x: i8, coalesce: bool },
    EditRoomLayout { pick: usize, op: u8, a: usize, x: i8, coalesce: bool },
    DeleteRoom(usize),
}

fn op_strategy() -> impl Strategy<Value = Op> {
    prop_oneof![
        3 => (any::<i16>(), any::<i16>(), any::<i16>())
            .prop_map(|(x, y, z)| Op::CreateCp(x, y, z)),
        2 => (any::<usize>(), any::<usize>()).prop_map(|(a, b)| Op::CreateLine(a, b)),
        1 => prop::collection::vec(any::<usize>(), 2..6).prop_map(Op::CreateSpline),
        1 => any::<usize>().prop_map(Op::CreateEdge),
        1 => any::<usize>().prop_map(Op::CreateWire),
        1 => any::<usize>().prop_map(Op::CreateFace),
        3 => (any::<usize>(), any::<i16>(), any::<bool>())
            .prop_map(|(pick, x, coalesce)| Op::UpdateCp { pick, x, coalesce }),
        2 => any::<usize>().prop_map(Op::DeleteAny),
        1 => (any::<i16>(), 1u8..200, 1u8..200)
            .prop_map(|(x, r, h)| Op::CreateCylinder { x, r, h }),
        1 => any::<i16>().prop_map(Op::CreateSite),
        2 => any::<i16>().prop_map(Op::CreateLevel),
        2 => (any::<usize>(), any::<i16>(), any::<bool>())
            .prop_map(|(pick, elevation, coalesce)| Op::UpdateLevelElevation {
                pick,
                elevation,
                coalesce,
            }),
        2 => (any::<usize>(), any::<usize>(), any::<bool>())
            .prop_map(|(cp, level, detach)| Op::AttachCp { cp, level, detach }),
        1 => any::<usize>().prop_map(Op::DeleteLevelCascade),
        2 => (any::<usize>(), any::<usize>())
            .prop_map(|(member, level)| Op::CreateElement { member, level }),
        // The orphan sweep is a whole reference-counted collection to
        // invert mechanically — stress both flag values.
        2 => (any::<usize>(), any::<bool>())
            .prop_map(|(pick, sweep)| Op::DeleteElement { pick, sweep }),
        2 => (any::<usize>(), any::<i8>(), 1u8..40, 1u8..40, any::<bool>())
            .prop_map(|(level, x, w, h, void)| Op::CreateSketch { level, x, w, h, void }),
        4 => (any::<usize>(), 0u8..7, any::<usize>(), any::<usize>(), any::<i8>(), any::<bool>())
            .prop_map(|(pick, op, a, b, x, coalesce)| Op::EditSketch { pick, op, a, b, x, coalesce }),
        1 => (any::<usize>(), any::<i8>())
            .prop_map(|(parent, offset)| Op::CreateWorkplane { parent, offset }),
        2 => (any::<usize>(), prop::option::of(any::<usize>()), 1u8..60)
            .prop_map(|(base, top, len)| Op::CreateWall { base, top, len }),
        3 => (any::<usize>(), 0u8..6, any::<usize>(), any::<i8>(), any::<bool>())
            .prop_map(|(pick, op, a, x, coalesce)| Op::EditWall { pick, op, a, x, coalesce }),
        2 => (any::<usize>(), prop::option::of(any::<usize>()), 2u8..6, any::<bool>())
            .prop_map(|(base, top, corners, closed)| Op::CreateWallRun { base, top, corners, closed }),
        4 => (any::<usize>(), 0u8..10, any::<usize>(), any::<i8>(), any::<bool>())
            .prop_map(|(pick, op, a, x, coalesce)| Op::EditWallRun { pick, op, a, x, coalesce }),
        1 => any::<usize>().prop_map(Op::DeleteWorkplaneCascade),
        1 => (any::<usize>(), prop::option::of(any::<usize>()))
            .prop_map(|(plane, top)| Op::CreateRoomLayout { plane, top }),
        2 => (any::<usize>(), any::<i8>(), any::<i8>(), 1u8..60, 1u8..60, -3i8..3)
            .prop_map(|(layout, x, y, w, h, precedence)| Op::CreateRoom { layout, x, y, w, h, precedence }),
        3 => (any::<usize>(), 0u8..8, any::<usize>(), any::<i8>(), any::<bool>())
            .prop_map(|(pick, op, a, x, coalesce)| Op::EditRoom { pick, op, a, x, coalesce }),
        3 => (any::<usize>(), 0u8..6, any::<usize>(), any::<i8>(), any::<bool>())
            .prop_map(|(pick, op, a, x, coalesce)| Op::EditRoomLayout { pick, op, a, x, coalesce }),
        1 => any::<usize>().prop_map(Op::DeleteRoom),
    ]
}

/// Construction planes: levels and workplanes, ascending ids.
fn planes(doc: &Document) -> Vec<EntityId> {
    let mut ids = ids_of_kind(doc, EntityKind::Level);
    ids.extend(ids_of_kind(doc, EntityKind::Workplane));
    ids.sort_unstable();
    ids
}

/// One random wall edit, or `None` when it does not apply.
fn edit_wall(doc: &Document, id: EntityId, op: u8, a: usize, x: i8, coalesce: bool) -> Option<Command> {
    let (profile, top_points) = match &doc.entity(id)?.params {
        Params::Wall { profile, top_points, .. } => (profile.clone(), top_points.clone()),
        _ => return None,
    };
    let height = wall::wall_top_height(doc, id)?;
    let offset = f64::from(x) * 0.02;
    let point = profile.points.get(a % profile.points.len().max(1)).map(|p| p.id);
    let edited = match op {
        0 => {
            let all = vim_design_lib::sketch::edges(&profile);
            let edge = all.get(a % all.len().max(1))?;
            wall_ops::insert_point_on_edge(&profile, &top_points, height, edge.a, edge.b, 0.5)
        }
        1 => wall_ops::move_points(&profile, &top_points, height, &[point?], [offset, offset]),
        2 => wall_ops::add_face(
            &profile,
            &top_points,
            height,
            &[[0.5, 0.5], [1.0, 0.5], [1.0, 1.0], [0.5, 1.0]],
            SketchFaceKind::Void { depth: None },
        ),
        3 => wall_ops::set_anchor(&profile, &top_points, height, &[point?], a.is_multiple_of(2)),
        4 => wall_ops::delete_points(&profile, &top_points, height, &[point?]),
        _ => {
            return Some(Command::UpdateWall {
                id,
                base: None,
                top: None,
                start: None,
                end: None,
                height_m: Some(1.0 + f64::from(x.unsigned_abs()) * 0.05),
                top_offset_m: Some(offset),
                profile: None,
                top_points: None,
                coalesce,
            });
        }
    };
    let (profile, top_points) = edited.ok()?;
    Some(Command::UpdateWall {
        id,
        base: None,
        top: None,
        start: None,
        end: None,
        height_m: None,
        top_offset_m: None,
        profile: Some(profile),
        top_points: Some(top_points),
        coalesce,
    })
}

/// One random wall-run edit through `wall_run::ops`, or `None` when it
/// does not apply (an invalid result is an op error, not a command).
fn edit_wall_run(doc: &Document, id: EntityId, op: u8, a: usize, x: i8, coalesce: bool) -> Option<Command> {
    let run = WallRunData::from_params(&doc.entity(id)?.params)?;
    let height = wall_run::run_top_height(doc, id)?;
    let d = f64::from(x) * 0.02;
    let point = run.points.get(a % run.points.len().max(1))?.id;
    let segments = run.segments();
    let segment = *segments.get(a % segments.len().max(1))?;
    let opening = run.openings.get(a % run.openings.len().max(1)).map(|o| o.id);
    let edited = match op {
        0 => run_ops::move_points(&run, &[point], [d, -d]),
        1 => run_ops::move_edges(&run, &[segment], [d, d]),
        2 => {
            let length = run.segment_length(segment)?;
            run_ops::insert_point(&run, segment, length * 0.5, height).map(|(r, _)| r)
        }
        3 => run_ops::delete_points(&run, &[point]),
        4 => run_ops::delete_edges(&run, &[segment]),
        5 => run_ops::extend(&run, if a.is_multiple_of(2) { RunEnd::Start } else { RunEnd::End }, [d * 50.0, 3.0])
            .map(|(r, _)| r),
        6 => run_ops::set_closed(&run, !run.closed),
        7 => {
            let (lo, hi) = wall_run::segment_clear_span(&run, segment).ok()?;
            let width = ((hi - lo) * 0.4).min(1.0);
            run_ops::add_opening(
                &run,
                Opening {
                    id: 0,
                    segment,
                    offset_m: lo + (hi - lo - width) * 0.5,
                    sill_m: 0.9,
                    width_m: width,
                    height_m: 1.0,
                    kind: if x < 0 { OpeningKind::Door } else { OpeningKind::Window },
                    depth_m: if a.is_multiple_of(3) { Some(0.05) } else { None },
                },
            )
            .map(|(r, _)| r)
        }
        8 => run_ops::move_opening(&run, opening?, [d, d]),
        _ => run_ops::delete_opening(&run, opening?),
    };
    let run = edited.ok()?;
    Some(Command::UpdateWallRun {
        id,
        base: None,
        top: None,
        points: Some(run.points),
        closed: Some(run.closed),
        thickness_m: None,
        height_m: None,
        top_offset_m: None,
        openings: Some(run.openings),
        profiles: Some(run.profiles),
        coalesce,
    })
}

/// One random room edit through `room::ops`, or `None`.
fn edit_room(doc: &Document, id: EntityId, op: u8, a: usize, x: i8, coalesce: bool) -> Option<Command> {
    let room = RoomData::from_params(&doc.entity(id)?.params)?;
    let d = f64::from(x) * 0.05;
    let edges = room.edges();
    let edge = *edges.get(a % edges.len().max(1))?;
    let edited = match op {
        0 => room_ops::move_points(&room, &[edge], [d, -d]),
        1 => room_ops::move_edges(&room, &[edge], [d, d]),
        2 => {
            let (p, q) = room.edge_ends(edge)?;
            let length = ((q[0] - p[0]).powi(2) + (q[1] - p[1]).powi(2)).sqrt();
            room_ops::insert_point(&room, edge, length / 2.0).map(|(r, _)| r)
        }
        3 => room_ops::delete_points(&room, &[edge]),
        4 => room_ops::delete_edges(&room, &[edge]),
        5 => room_ops::set_hidden(&room, &[edge], !room.is_hidden(edge)),
        6 => {
            return Some(Command::UpdateRoom {
                id,
                plane: None,
                name: None,
                precedence: Some(i32::from(x) % 4),
                boundary: None,
                hidden_edges: None,
                coalesce,
            });
        }
        _ => room_ops::set_point(&room, edge, [d, d]),
    };
    let room = edited.ok()?;
    Some(Command::UpdateRoom {
        id,
        plane: None,
        name: None,
        precedence: None,
        boundary: Some(room.boundary),
        hidden_edges: Some(room.hidden_edges),
        coalesce,
    })
}

/// One random layout edit through `room_layout::ops`, or `None`.
fn edit_room_layout(doc: &Document, id: EntityId, op: u8, a: usize, x: i8, coalesce: bool) -> Option<Command> {
    let input = room_layout::inputs(doc, id)?;
    let d = f64::from(x) * 0.02;
    let update = |data: RoomLayoutData| Command::UpdateRoomLayout {
        id,
        plane: None,
        top: None,
        rooms: None,
        thickness_m: None,
        height_m: None,
        top_offset_m: None,
        openings: Some(data.openings),
        coalesce,
    };
    let opening = input.layout.openings.get(a % input.layout.openings.len().max(1)).map(|o| o.id);
    match op {
        0 | 1 => {
            let (room, data) = input.rooms.get(a % input.rooms.len().max(1))?;
            let edges = data.edges();
            let edge = *edges.get(a % edges.len().max(1))?;
            let spans = room_layout::opening_span(&input, *room, edge).ok()?;
            let (lo, hi) = *spans.first()?;
            let width = ((hi - lo) * 0.5).min(1.0);
            let opening = RoomOpening {
                id: 0,
                room: *room,
                edge,
                offset_m: lo + (hi - lo - width) / 2.0,
                sill_m: 0.9,
                width_m: width,
                height_m: 1.0,
                kind: if op == 0 { OpeningKind::Window } else { OpeningKind::Door },
                depth_m: if x < 0 { Some(0.03) } else { None },
            };
            layout_ops::add_opening(&input, opening).ok().map(|(data, _)| update(data))
        }
        2 => layout_ops::move_opening(&input, opening?, [d, d]).ok().map(update),
        3 => layout_ops::delete_opening(&input, opening?).ok().map(update),
        4 => Some(Command::UpdateRoomLayout {
            id,
            plane: None,
            top: None,
            rooms: None,
            thickness_m: Some(0.1 + f64::from(x.unsigned_abs()) * 0.001),
            height_m: Some(2.0 + d.abs()),
            top_offset_m: None,
            openings: None,
            coalesce,
        }),
        _ => {
            let (room, _) = input.rooms.get(a % input.rooms.len().max(1))?;
            let precedence = layout_ops::bring_forward(&input, *room).ok()??;
            Some(Command::UpdateRoom {
                id: *room,
                plane: None,
                name: None,
                precedence: Some(precedence),
                boundary: None,
                hidden_edges: None,
                coalesce,
            })
        }
    }
}

fn stored_sketch(doc: &Document, id: EntityId) -> Option<Sketch> {
    doc.entity(id).and_then(|record| match &record.params {
        Params::Sketch { sketch, .. } => Some(sketch.clone()),
        _ => None,
    })
}

/// Apply one random topology operation; `None` when it does not apply.
fn edit_sketch(sketch: &Sketch, op: u8, a: usize, b: usize, x: i8) -> Option<Sketch> {
    let offset = f64::from(x) * 0.05;
    let point = |i: usize| sketch.points.get(i % sketch.points.len().max(1)).map(|p| p.id);
    let face = |i: usize| sketch.faces.get(i % sketch.faces.len().max(1)).map(|f| f.id);
    let edge = |i: usize| {
        let all = vim_design_lib::sketch::edges(sketch);
        all.get(i % all.len().max(1)).map(|e| (e.a, e.b))
    };
    let result = match op {
        0 => ops::move_points(sketch, &[point(a)?], [offset, 0.0]),
        1 => {
            let (p, q) = edge(a)?;
            ops::insert_point_on_edge(sketch, p, q, 0.25 + f64::from(b as u8 % 50) / 100.0)
        }
        2 => ops::split_faces(sketch, [offset, -50.0], [offset + 0.3, 50.0]),
        3 => ops::delete_faces(sketch, &[face(a)?]),
        4 => ops::delete_edges(sketch, &[edge(a)?]),
        5 => ops::delete_points(sketch, &[point(a)?]),
        _ => ops::add_face(
            sketch,
            &[
                [offset, offset],
                [offset + 1.0, offset],
                [offset + 1.0, offset + 1.0],
                [offset, offset + 1.0],
            ],
            if b.is_multiple_of(2) {
                SketchFaceKind::Solid { thickness: 0.1 + f64::from(b as u8 % 5) * 0.1 }
            } else {
                SketchFaceKind::Void { depth: if b.is_multiple_of(3) { None } else { Some(0.15) } }
            },
        ),
    };
    result.ok()
}

/// Ids of a given kind, in deterministic (ascending) order.
fn ids_of_kind(doc: &Document, kind: EntityKind) -> Vec<EntityId> {
    doc.entities()
        .filter(|(_, record)| record.kind() == kind)
        .map(|(id, _)| *id)
        .collect()
}

fn pick(ids: &[EntityId], index: usize) -> Option<EntityId> {
    if ids.is_empty() {
        None
    } else {
        ids.get(index % ids.len()).copied()
    }
}

/// Interpret one op as a command. Rejections are fine (rejected commands
/// must be no-ops; that's asserted separately in rejections.rs).
fn run_op(doc: &mut Document, op: &Op) {
    let fallback = Command::CreateControlPoint {
        position: [0.5, 0.5, 0.5],
    };
    let cmd = match op {
        Op::CreateCp(x, y, z) => Command::CreateControlPoint {
            position: [f64::from(*x), f64::from(*y), f64::from(*z)],
        },
        Op::CreateLine(a, b) => {
            let cps = ids_of_kind(doc, EntityKind::ControlPoint);
            match (pick(&cps, *a), pick(&cps, *b)) {
                (Some(start), Some(end)) => Command::CreateLine { start, end },
                _ => fallback,
            }
        }
        Op::CreateSpline(indexes) => {
            let cps = ids_of_kind(doc, EntityKind::ControlPoint);
            let picked: Vec<EntityId> =
                indexes.iter().filter_map(|i| pick(&cps, *i)).collect();
            if picked.is_empty() {
                fallback
            } else {
                Command::CreateSpline {
                    control_points: picked,
                    degree: None,
                    knots: None,
                }
            }
        }
        Op::CreateEdge(i) => {
            let mut curves = ids_of_kind(doc, EntityKind::Line);
            curves.extend(ids_of_kind(doc, EntityKind::Spline));
            curves.sort_unstable();
            match pick(&curves, *i) {
                Some(curve) => Command::CreateEdge { curve },
                None => fallback,
            }
        }
        Op::CreateWire(i) => {
            let edges = ids_of_kind(doc, EntityKind::Edge);
            match pick(&edges, *i) {
                Some(edge) => Command::CreateWire { edges: vec![edge] },
                None => fallback,
            }
        }
        Op::CreateFace(i) => {
            let wires = ids_of_kind(doc, EntityKind::Wire);
            match pick(&wires, *i) {
                Some(outer) => Command::CreateFace {
                    outer,
                    holes: vec![],
                    plane: None,
                },
                None => fallback,
            }
        }
        Op::UpdateCp { pick: p, x, coalesce } => {
            let cps = ids_of_kind(doc, EntityKind::ControlPoint);
            match pick(&cps, *p) {
                Some(id) => Command::UpdateControlPoint {
                    id,
                    position: [f64::from(*x), 0.0, 0.0],
                    coalesce: *coalesce,
                },
                None => fallback,
            }
        }
        Op::DeleteAny(i) => {
            let all: Vec<EntityId> = doc.entities().map(|(id, _)| *id).collect();
            match pick(&all, *i) {
                // May be rejected with HasDependents — that's the point.
                Some(id) => match doc.entity(id).map(|r| r.kind()) {
                    Some(EntityKind::ControlPoint) => Command::DeleteControlPoint { id },
                    Some(EntityKind::Line) => Command::DeleteLine { id },
                    Some(EntityKind::Spline) => Command::DeleteSpline { id },
                    Some(EntityKind::Edge) => Command::DeleteEdge { id },
                    Some(EntityKind::Wire) => Command::DeleteWire { id },
                    Some(EntityKind::Face) => Command::DeleteFace { id },
                    Some(EntityKind::Circle) => Command::DeleteCircle { id },
                    Some(EntityKind::Extrusion) => Command::DeleteExtrusion { id },
                    Some(EntityKind::Sketch) => Command::DeleteSketch { id },
                    Some(EntityKind::Wall) => Command::DeleteWall { id },
                    Some(EntityKind::WallRun) => Command::DeleteWallRun { id },
                    Some(EntityKind::Room) => Command::DeleteRoom { id },
                    Some(EntityKind::RoomLayout) => Command::DeleteRoomLayout { id },
                    Some(EntityKind::Workplane) => Command::DeleteWorkplane { id },
                    _ => fallback,
                },
                None => fallback,
            }
        }
        Op::CreateCylinder { x, r, h } => Command::CreateCylinder {
            center: [f64::from(*x), 0.0, 0.0],
            radius: f64::from(*r) * 0.01,
            height: f64::from(*h) * 0.05,
        },
        // May be rejected with SingletonExists after the first — the
        // rejection path must be a byte-exact no-op like any other.
        Op::CreateSite(lat) => Command::CreateSite {
            latitude_deg: f64::from(*lat) * 0.001,
            longitude_deg: -73.5674,
            elevation_m: 36.0,
            true_north_deg: 0.0,
        },
        Op::CreateLevel(elev) => Command::CreateLevel {
            name: format!("L{elev}"),
            elevation_m: f64::from(*elev) * 0.01,
            is_building_story: true,
            color: [0.2, 0.5, 0.9, 0.35],
            extent_m: 10.0,
        },
        Op::UpdateLevelElevation { pick: p, elevation, coalesce } => {
            let levels = ids_of_kind(doc, EntityKind::Level);
            match pick(&levels, *p) {
                Some(id) => Command::UpdateLevel {
                    id,
                    name: None,
                    elevation_m: Some(f64::from(*elevation) * 0.01),
                    is_building_story: None,
                    color: None,
                    extent_m: None,
                    coalesce: *coalesce,
                },
                None => fallback,
            }
        }
        Op::AttachCp { cp, level, detach } => {
            let cps = ids_of_kind(doc, EntityKind::ControlPoint);
            let levels = ids_of_kind(doc, EntityKind::Level);
            match (pick(&cps, *cp), pick(&levels, *level)) {
                (Some(id), Some(plane)) => Command::UpdateControlPointPlane {
                    id,
                    plane: if *detach { None } else { Some(plane) },
                    position: None,
                },
                _ => fallback,
            }
        }
        Op::CreateElement { member, level } => {
            let mut producers = ids_of_kind(doc, EntityKind::Extrusion);
            producers.extend(ids_of_kind(doc, EntityKind::Revolve));
            producers.extend(ids_of_kind(doc, EntityKind::Sketch));
            producers.extend(ids_of_kind(doc, EntityKind::Wall));
            producers.extend(ids_of_kind(doc, EntityKind::WallRun));
            producers.extend(ids_of_kind(doc, EntityKind::RoomLayout));
            producers.extend(ids_of_kind(doc, EntityKind::Room));
            producers.sort_unstable();
            let levels = ids_of_kind(doc, EntityKind::Level);
            match (pick(&producers, *member), pick(&levels, *level)) {
                (Some(member), Some(level)) => Command::CreateElement {
                    name: "e".to_owned(),
                    members: vec![member],
                    level,
                },
                _ => fallback,
            }
        }
        Op::CreateSketch { level, x, w, h, void } => {
            let levels = ids_of_kind(doc, EntityKind::Level);
            let x0 = f64::from(*x) * 0.1;
            let (w, h) = (f64::from(*w) * 0.1, f64::from(*h) * 0.1);
            let rect = [[x0, 0.0], [x0 + w, 0.0], [x0 + w, h], [x0, h]];
            let sketch = ops::add_face(&Sketch::default(), &rect, SketchFaceKind::Solid { thickness: 0.3 })
                .ok()
                .and_then(|s| {
                    if *void {
                        let hole = [
                            [x0 + w * 0.25, h * 0.25],
                            [x0 + w * 0.75, h * 0.25],
                            [x0 + w * 0.75, h * 0.75],
                            [x0 + w * 0.25, h * 0.75],
                        ];
                        ops::add_face(&s, &hole, SketchFaceKind::Void { depth: Some(0.1) }).ok()
                    } else {
                        Some(s)
                    }
                });
            match (pick(&levels, *level), sketch) {
                (Some(plane), Some(sketch)) => Command::CreateSketch {
                    plane,
                    sketch,
                    direction: SketchDirection::Below,
                },
                _ => fallback,
            }
        }
        Op::EditSketch { pick: p, op, a, b, x, coalesce } => {
            let sketches = ids_of_kind(doc, EntityKind::Sketch);
            let edited = pick(&sketches, *p).and_then(|id| {
                stored_sketch(doc, id)
                    .and_then(|s| edit_sketch(&s, *op, *a, *b, *x))
                    .map(|sketch| (id, sketch))
            });
            match edited {
                Some((id, sketch)) => Command::UpdateSketch {
                    id,
                    sketch,
                    coalesce: *coalesce,
                },
                None => fallback,
            }
        }
        Op::CreateWorkplane { parent, offset } => match pick(&planes(doc), *parent) {
            Some(parent) => Command::CreateWorkplane {
                parent,
                name: "wp".to_owned(),
                offset_m: f64::from(*offset) * 0.05,
                color: [0.5, 0.5, 0.5, 0.2],
                extent_m: 5.0,
            },
            None => fallback,
        },
        Op::CreateWall { base, top, len } => {
            let all = planes(doc);
            let length = f64::from(*len) * 0.1;
            let (profile, top_points) = wall::default_profile(length, 0.2);
            match pick(&all, *base) {
                Some(base) => Command::CreateWall {
                    base,
                    top: top.and_then(|t| pick(&all, t)),
                    start: [0.0, 0.0],
                    end: [length, 0.0],
                    height_m: 2.7,
                    top_offset_m: 0.0,
                    profile,
                    top_points,
                },
                None => fallback,
            }
        }
        Op::EditWall { pick: p, op, a, x, coalesce } => {
            let walls = ids_of_kind(doc, EntityKind::Wall);
            pick(&walls, *p)
                .and_then(|id| edit_wall(doc, id, *op, *a, *x, *coalesce))
                .unwrap_or(fallback)
        }
        Op::CreateWallRun { base, top, corners, closed } => {
            let all = planes(doc);
            let n = usize::from(*corners).max(2);
            let closed = *closed && n >= 3;
            let points: Vec<RunPoint> = (0..n)
                .map(|i| {
                    let angle = std::f64::consts::TAU * i as f64 / n as f64;
                    let uv = if closed {
                        [3.0 * angle.cos(), 3.0 * angle.sin()]
                    } else {
                        [4.0 * i as f64, if i % 2 == 0 { 0.0 } else { 2.0 }]
                    };
                    RunPoint { id: i as u32, uv }
                })
                .collect();
            match pick(&all, *base) {
                Some(base) => Command::CreateWallRun {
                    base,
                    top: top.and_then(|t| pick(&all, t)),
                    points,
                    closed,
                    thickness_m: 0.2,
                    height_m: 2.7,
                    top_offset_m: 0.0,
                    openings: vec![],
                    profiles: vec![],
                },
                None => fallback,
            }
        }
        Op::EditWallRun { pick: p, op, a, x, coalesce } => {
            let runs = ids_of_kind(doc, EntityKind::WallRun);
            pick(&runs, *p)
                .and_then(|id| edit_wall_run(doc, id, *op, *a, *x, *coalesce))
                .unwrap_or(fallback)
        }
        Op::CreateRoomLayout { plane, top } => {
            let all = planes(doc);
            match pick(&all, *plane) {
                Some(plane) => Command::CreateRoomLayout {
                    plane,
                    top: top.and_then(|t| pick(&all, t)),
                    rooms: vec![],
                    thickness_m: room_layout::DEFAULT_PARTITION_THICKNESS_M,
                    height_m: 2.7,
                    top_offset_m: 0.0,
                    openings: vec![],
                },
                None => fallback,
            }
        }
        Op::CreateRoom { layout, x, y, w, h, precedence } => {
            let layouts = ids_of_kind(doc, EntityKind::RoomLayout);
            let target = pick(&layouts, *layout).and_then(|id| {
                let plane = doc.entity(id)?.inputs.first()?.referenced().next()?;
                Some((id, plane))
            });
            let (x0, y0) = (f64::from(*x) * 0.1, f64::from(*y) * 0.1);
            let (w, h) = (f64::from(*w) * 0.1, f64::from(*h) * 0.1);
            let room = room::from_rectangle(&room::default_name(doc), i32::from(*precedence), [x0, y0], [x0 + w, y0 + h]);
            match (target, room) {
                (Some((layout, plane)), Ok(room)) => Command::CreateRoom {
                    plane,
                    name: room.name,
                    precedence: room.precedence,
                    boundary: room.boundary,
                    hidden_edges: vec![],
                    layout: Some(layout),
                },
                _ => fallback,
            }
        }
        Op::EditRoom { pick: p, op, a, x, coalesce } => {
            let rooms = ids_of_kind(doc, EntityKind::Room);
            pick(&rooms, *p)
                .and_then(|id| edit_room(doc, id, *op, *a, *x, *coalesce))
                .unwrap_or(fallback)
        }
        Op::EditRoomLayout { pick: p, op, a, x, coalesce } => {
            let layouts = ids_of_kind(doc, EntityKind::RoomLayout);
            pick(&layouts, *p)
                .and_then(|id| edit_room_layout(doc, id, *op, *a, *x, *coalesce))
                .unwrap_or(fallback)
        }
        Op::DeleteRoom(i) => {
            let rooms = ids_of_kind(doc, EntityKind::Room);
            match pick(&rooms, *i) {
                Some(id) => Command::DeleteRoom { id },
                None => fallback,
            }
        }
        Op::DeleteWorkplaneCascade(i) => {
            let workplanes = ids_of_kind(doc, EntityKind::Workplane);
            match pick(&workplanes, *i) {
                Some(id) => Command::DeleteWorkplaneCascade { id },
                None => fallback,
            }
        }
        Op::DeleteElement { pick: p, sweep } => {
            let elements = ids_of_kind(doc, EntityKind::Element);
            match pick(&elements, *p) {
                Some(id) => Command::DeleteElement {
                    id,
                    sweep_orphans: *sweep,
                },
                None => fallback,
            }
        }
        // Deletes the whole dependent closure as ONE undo group — the
        // heaviest mechanical-inversion stress in the suite.
        Op::DeleteLevelCascade(i) => {
            let levels = ids_of_kind(doc, EntityKind::Level);
            match pick(&levels, *i) {
                Some(id) => Command::DeleteLevel { id, cascade: true },
                None => fallback,
            }
        }
    };
    // Rejections allowed; successes and rejections must both keep the
    // document consistent (checked by the properties below).
    let _ = doc.submit(cmd);
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(96))]

    #[test]
    fn undo_all_and_redo_all_are_byte_exact(ops in prop::collection::vec(op_strategy(), 1..40)) {
        let mut doc = Document::new();
        let initial = doc.save().expect("initial save");

        for op in &ops {
            run_op(&mut doc, op);
        }
        doc.debug_validate().expect("invariants after sequence");
        let final_bytes = doc.save().expect("final save");

        // Undo everything: byte-identical to the initial save (the
        // persisted next-id is derived from the entity map, so the
        // monotonic in-memory allocator does not leak into the bytes).
        while doc.can_undo() {
            doc.undo().expect("undo must succeed");
        }
        prop_assert_eq!(doc.save().expect("save after undo-all"), initial);

        // Redo everything: byte-identical to the final save.
        while doc.can_redo() {
            doc.redo().expect("redo must succeed");
        }
        prop_assert_eq!(doc.save().expect("save after redo-all"), final_bytes.clone());
        doc.debug_validate().expect("invariants after redo-all");

        // Test plan (g): save -> load -> save byte-identity on the result.
        let reloaded = Document::load(&final_bytes).expect("load final save");
        prop_assert_eq!(reloaded.save().expect("re-save"), final_bytes);
    }
}

//! Test plan (e), property part (docs/ARCHITECTURE.md §12):
//! for any random command sequence, undo-all restores a state that
//! serializes byte-identically to the initial save, and redo-all
//! restores the final save.

use proptest::prelude::*;
use vim_design_lib::{Command, Document, EntityId, EntityKind};

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
    UpdateLevelElevation { pick: usize, elevation: i16, coalesce: bool },
    AttachCp { cp: usize, level: usize, detach: bool },
    DeleteLevelCascade(usize),
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
    ]
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

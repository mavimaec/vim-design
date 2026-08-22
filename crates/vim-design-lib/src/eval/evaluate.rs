//! Pure per-entity evaluators and the topological-wave scheduler
//! (docs/ARCHITECTURE.md §§3.3, 6.2).
//!
//! `evaluate_waves` is a pure function of a snapshot: cloned entity
//! records, document settings, and the previous results map. The engine
//! feeds it the dirty closure and merges the outcomes; nothing here
//! mutates shared state, which is what makes per-wave parallelism (rayon
//! under the `parallel` feature) and the future background-thread wrapper
//! safe.

use std::collections::BTreeMap;

use crate::document::DocumentSettings;
use crate::entity::{EntityKind, EntityRecord, Params, SlotValue, slot};
use crate::id::EntityId;
use crate::kernel::{
    self, CurveSpec, KernelError, KernelFace, PlaneSpec, WireSpec,
};

use super::types::{EvalDiag, EvalErrorKind, Evaluated};

#[cfg(feature = "parallel")]
use rayon::prelude::*;

/// The engine's per-entity cache entry, visible to the scheduler so the
/// lookup can serve stale values (docs/ARCHITECTURE.md §6.4).
#[derive(Debug, Default)]
pub(crate) struct EntityEval {
    /// Last successful value (retained through later failures).
    pub value: Option<Evaluated>,
    /// Generation at which `value` was produced.
    pub value_generation: u64,
    /// Current state (`None` = never evaluated).
    pub state: Option<super::types::EvalState>,
}

/// Read view over "fresh outcomes of earlier waves" + "previous results".
/// An input in fresh-error state falls back to its stale value.
pub(crate) struct Lookup<'a> {
    pub fresh: &'a BTreeMap<EntityId, Result<Evaluated, EvalDiag>>,
    pub base: &'a BTreeMap<EntityId, EntityEval>,
}

impl Lookup<'_> {
    fn value(&self, id: EntityId) -> Option<&Evaluated> {
        match self.fresh.get(&id) {
            Some(Ok(value)) => Some(value),
            // Fresh failure (or not in this batch): fall back to the last
            // successful value, if any — stale retention (§6.4).
            Some(Err(_)) | None => self.base.get(&id).and_then(|e| e.value.as_ref()),
        }
    }

    fn is_error(&self, id: EntityId) -> bool {
        match self.fresh.get(&id) {
            Some(Ok(_)) => false,
            Some(Err(_)) => true,
            None => matches!(
                self.base.get(&id).and_then(|e| e.state.as_ref()),
                Some(super::types::EvalState::Error { .. })
            ),
        }
    }
}

// ---------------------------------------------------------------------
// Wave scheduling.
// ---------------------------------------------------------------------

/// Split `records` into topological waves: an entity lands in the wave
/// after the deepest of its in-batch inputs (entities whose inputs are
/// all outside the batch land in wave 0). Kahn-style level BFS.
pub(crate) fn topo_waves(
    records: &BTreeMap<EntityId, EntityRecord>,
) -> Vec<Vec<EntityId>> {
    let mut in_degree: BTreeMap<EntityId, usize> = BTreeMap::new();
    let mut dependents: BTreeMap<EntityId, Vec<EntityId>> = BTreeMap::new();
    for (id, record) in records {
        let inputs: Vec<EntityId> = record
            .referenced()
            .filter(|input| records.contains_key(input))
            .collect();
        in_degree.insert(*id, inputs.len());
        for input in inputs {
            dependents.entry(input).or_default().push(*id);
        }
    }
    let mut waves: Vec<Vec<EntityId>> = Vec::new();
    let mut current: Vec<EntityId> = in_degree
        .iter()
        .filter(|(_, deg)| **deg == 0)
        .map(|(id, _)| *id)
        .collect();
    while !current.is_empty() {
        let mut next: Vec<EntityId> = Vec::new();
        for id in &current {
            if let Some(deps) = dependents.get(id) {
                for dep in deps {
                    if let Some(deg) = in_degree.get_mut(dep) {
                        *deg = deg.saturating_sub(1);
                        if *deg == 0 {
                            next.push(*dep);
                        }
                    }
                }
            }
        }
        waves.push(std::mem::take(&mut current));
        current = next;
    }
    waves
}

/// Evaluate the batch wave by wave. Pure: only reads `records`,
/// `settings`, and `base`; returns one outcome per entity. Within a wave
/// entities evaluate in parallel under the `parallel` feature
/// (docs/ARCHITECTURE.md §6.2); the wasm fallback is sequential.
pub(crate) fn evaluate_waves(
    records: &BTreeMap<EntityId, EntityRecord>,
    settings: &DocumentSettings,
    base: &BTreeMap<EntityId, EntityEval>,
) -> BTreeMap<EntityId, Result<Evaluated, EvalDiag>> {
    let waves = topo_waves(records);
    let mut fresh: BTreeMap<EntityId, Result<Evaluated, EvalDiag>> = BTreeMap::new();
    for wave in waves {
        let lookup = Lookup {
            fresh: &fresh,
            base,
        };
        let run = |id: &EntityId| -> Option<(EntityId, Result<Evaluated, EvalDiag>)> {
            let record = records.get(id)?;
            Some((*id, evaluate_entity(record, &lookup, settings)))
        };
        #[cfg(feature = "parallel")]
        let outcomes: Vec<_> = wave.par_iter().filter_map(run).collect();
        #[cfg(not(feature = "parallel"))]
        let outcomes: Vec<_> = wave.iter().filter_map(run).collect();
        fresh.extend(outcomes);
    }
    fresh
}

// ---------------------------------------------------------------------
// Per-entity evaluators.
// ---------------------------------------------------------------------

fn diag(kind: EvalErrorKind, message: impl Into<String>) -> EvalDiag {
    EvalDiag::new(kind, message)
}

fn kernel_diag(err: KernelError) -> EvalDiag {
    let kind = match &err {
        KernelError::NotPlanar => EvalErrorKind::NotPlanar,
        KernelError::Degenerate(_) => EvalErrorKind::Degenerate,
        KernelError::Topology(_) => EvalErrorKind::Kernel,
        KernelError::Tessellation(_) => EvalErrorKind::Tessellation,
        KernelError::Unsupported(_) => EvalErrorKind::NotYetSupported,
        KernelError::Unresolved(_) => EvalErrorKind::UnresolvedSubRef,
        KernelError::Panic(_) => EvalErrorKind::InternalPanic,
    };
    diag(kind, err.to_string())
}

/// The id in a single slot, if wired.
fn single_id(record: &EntityRecord, idx: usize) -> Option<EntityId> {
    match record.inputs.get(idx) {
        Some(SlotValue::One(id)) => *id,
        _ => None,
    }
}

/// The ids in a multi slot.
fn multi_ids(record: &EntityRecord, idx: usize) -> Vec<EntityId> {
    match record.inputs.get(idx) {
        Some(SlotValue::Many(ids)) => ids.clone(),
        _ => Vec::new(),
    }
}

/// Fetch a required input's evaluated value with a precise diagnostic.
fn require<'a>(
    lookup: &'a Lookup<'_>,
    id: Option<EntityId>,
    what: &str,
) -> Result<&'a Evaluated, EvalDiag> {
    let id = id.ok_or_else(|| {
        diag(EvalErrorKind::MissingInput, format!("{what} input is not wired"))
    })?;
    lookup.value(id).ok_or_else(|| {
        if lookup.is_error(id) {
            diag(
                EvalErrorKind::UpstreamError,
                format!("{what} input (entity {}) is in error with no stale geometry", id.0),
            )
        } else {
            diag(
                EvalErrorKind::MissingInput,
                format!("{what} input (entity {}) has not been evaluated", id.0),
            )
        }
    })
}

fn as_point(value: &Evaluated, what: &str) -> Result<[f64; 3], EvalDiag> {
    match value {
        Evaluated::Point(p) => Ok(*p),
        other => Err(diag(
            EvalErrorKind::UpstreamError,
            format!("{what} input evaluated to {other:?}, expected a point"),
        )),
    }
}

fn as_curve<'a>(value: &'a Evaluated, what: &str) -> Result<&'a CurveSpec, EvalDiag> {
    match value {
        Evaluated::Curve(c) | Evaluated::Edge(c) => Ok(c),
        other => Err(diag(
            EvalErrorKind::UpstreamError,
            format!("{what} input evaluated to {other:?}, expected a curve"),
        )),
    }
}

/// Evaluate one entity from its record and already-evaluated inputs.
pub(crate) fn evaluate_entity(
    record: &EntityRecord,
    lookup: &Lookup<'_>,
    settings: &DocumentSettings,
) -> Result<Evaluated, EvalDiag> {
    let tol = settings.kernel_tolerance;
    match record.kind() {
        EntityKind::ControlPoint => match &record.params {
            Params::ControlPoint { position } => Ok(Evaluated::Point(*position)),
            _ => Err(params_mismatch(record)),
        },
        EntityKind::Plane => match &record.params {
            Params::Plane { origin, normal } => {
                let unit = kernel::normalized(*normal, tol).ok_or_else(|| {
                    diag(EvalErrorKind::Degenerate, "plane normal is (near-)zero")
                })?;
                Ok(Evaluated::Plane {
                    origin: *origin,
                    normal: unit,
                })
            }
            _ => Err(params_mismatch(record)),
        },
        EntityKind::Circle => {
            let radius = match &record.params {
                Params::Circle { radius } => *radius,
                _ => return Err(params_mismatch(record)),
            };
            if radius <= tol {
                return Err(diag(
                    EvalErrorKind::Degenerate,
                    format!("circle radius {radius} m is not positive"),
                ));
            }
            let center = as_point(
                require(lookup, single_id(record, slot::CIRCLE_CENTER), "center")?,
                "center",
            )?;
            let normal = match single_id(record, slot::CIRCLE_PLANE) {
                Some(plane_id) => match require(lookup, Some(plane_id), "plane")? {
                    Evaluated::Plane { normal, .. } => *normal,
                    other => {
                        return Err(diag(
                            EvalErrorKind::UpstreamError,
                            format!("plane input evaluated to {other:?}"),
                        ));
                    }
                },
                // Default: horizontal circle at the center (Z-up, §7).
                None => [0.0, 0.0, 1.0],
            };
            Ok(Evaluated::Curve(CurveSpec::Circle {
                center,
                normal,
                radius,
            }))
        }
        EntityKind::Line => {
            let start = as_point(
                require(lookup, single_id(record, slot::LINE_START), "start")?,
                "start",
            )?;
            let end = as_point(
                require(lookup, single_id(record, slot::LINE_END), "end")?,
                "end",
            )?;
            let length = kernel::norm([
                end[0] - start[0],
                end[1] - start[1],
                end[2] - start[2],
            ]);
            if length <= tol {
                return Err(diag(
                    EvalErrorKind::Degenerate,
                    format!("line endpoints coincide within tolerance ({length} m)"),
                ));
            }
            Ok(Evaluated::Curve(CurveSpec::Segment { start, end }))
        }
        EntityKind::Spline => {
            // v1: the control points are the Bézier control polygon
            // (`degree`/`knots` params are accepted but not yet
            // interpreted — the evaluator chooses, per Params docs).
            let ids = multi_ids(record, slot::SPLINE_CONTROL_POINTS);
            if ids.len() < 2 {
                return Err(diag(
                    EvalErrorKind::Degenerate,
                    "a spline needs at least two control points",
                ));
            }
            let mut control_points = Vec::with_capacity(ids.len());
            for id in ids {
                let point = as_point(
                    require(lookup, Some(id), "control point")?,
                    "control point",
                )?;
                control_points.push(point);
            }
            Ok(Evaluated::Curve(CurveSpec::Bezier { control_points }))
        }
        EntityKind::Edge => {
            // v1: an edge is the full interval of its curve.
            let curve = as_curve(
                require(lookup, single_id(record, slot::EDGE_CURVE), "curve")?,
                "curve",
            )?;
            Ok(Evaluated::Edge(curve.clone()))
        }
        EntityKind::Wire => {
            let ids = multi_ids(record, slot::WIRE_EDGES);
            let mut specs: Vec<(CurveSpec, EntityId)> = Vec::with_capacity(ids.len());
            for id in ids {
                let curve = as_curve(require(lookup, Some(id), "edge")?, "edge")?;
                // The Edge entity id rides along as the curve's *source*:
                // the stable id provenance naming derives from (§3.4).
                specs.push((curve.clone(), id));
            }
            let wire = chain_wire(specs, tol)?;
            Ok(Evaluated::Wire(wire))
        }
        EntityKind::Face => {
            let outer = match require(lookup, single_id(record, slot::FACE_OUTER), "outer wire")? {
                Evaluated::Wire(wire) => wire.clone(),
                other => {
                    return Err(diag(
                        EvalErrorKind::UpstreamError,
                        format!("outer wire input evaluated to {other:?}"),
                    ));
                }
            };
            let mut holes = Vec::new();
            for id in multi_ids(record, slot::FACE_HOLES) {
                match require(lookup, Some(id), "hole wire")? {
                    Evaluated::Wire(wire) => holes.push(wire.clone()),
                    other => {
                        return Err(diag(
                            EvalErrorKind::UpstreamError,
                            format!("hole wire input evaluated to {other:?}"),
                        ));
                    }
                }
            }
            let plane = match single_id(record, slot::FACE_PLANE) {
                Some(plane_id) => match require(lookup, Some(plane_id), "plane")? {
                    Evaluated::Plane { origin, normal } => Some(PlaneSpec {
                        origin: *origin,
                        normal: *normal,
                    }),
                    other => {
                        return Err(diag(
                            EvalErrorKind::UpstreamError,
                            format!("plane input evaluated to {other:?}"),
                        ));
                    }
                },
                None => None,
            };
            let face = kernel::make_face(&outer, &holes, plane, tol).map_err(kernel_diag)?;
            Ok(Evaluated::Face {
                face,
                material: single_id(record, slot::FACE_MATERIAL),
            })
        }
        EntityKind::Solid => {
            let ids = multi_ids(record, slot::SOLID_FACES);
            let mut faces: Vec<KernelFace> = Vec::with_capacity(ids.len());
            let mut material = None;
            for id in ids {
                match require(lookup, Some(id), "face")? {
                    Evaluated::Face { face, material: m } => {
                        faces.push(face.clone());
                        if material.is_none() {
                            material = *m;
                        }
                    }
                    other => {
                        return Err(diag(
                            EvalErrorKind::UpstreamError,
                            format!("face input evaluated to {other:?}"),
                        ));
                    }
                }
            }
            let solid = kernel::solid_from_faces(&faces).map_err(kernel_diag)?;
            Ok(Evaluated::Solid { solid, material })
        }
        EntityKind::Material => match &record.params {
            Params::Material { .. } => Ok(Evaluated::Material),
            _ => Err(params_mismatch(record)),
        },
        EntityKind::Extrusion => {
            let (face, material) =
                match require(lookup, single_id(record, slot::EXTRUSION_PROFILE), "profile")? {
                    Evaluated::Face { face, material } => (face.clone(), *material),
                    other => {
                        return Err(diag(
                            EvalErrorKind::UpstreamError,
                            format!("profile input evaluated to {other:?}"),
                        ));
                    }
                };
            let path = as_curve(
                require(lookup, single_id(record, slot::EXTRUSION_PATH), "path")?,
                "path",
            )?;
            let direction = match path {
                CurveSpec::Segment { start, end } => [
                    end[0] - start[0],
                    end[1] - start[1],
                    end[2] - start[2],
                ],
                CurveSpec::Bezier { .. } => {
                    // Documented v1 limitation (task scope): sweeps along
                    // spline paths are a follow-up; the wiring is legal,
                    // the evaluation reports a clean per-entity error.
                    return Err(diag(
                        EvalErrorKind::NotYetSupported,
                        "extrusion along a spline path is not yet supported \
                         (v1 sweeps straight-line paths only)",
                    ));
                }
                CurveSpec::Circle { .. } => {
                    return Err(diag(
                        EvalErrorKind::NotYetSupported,
                        "extrusion along a circular path is not yet supported",
                    ));
                }
            };
            let solid = kernel::extrude_solid(&face, direction, tol).map_err(kernel_diag)?;
            Ok(Evaluated::Solid { solid, material })
        }
        EntityKind::Revolve => {
            let angle = match &record.params {
                Params::Revolve { angle_radians, .. } => *angle_radians,
                _ => return Err(params_mismatch(record)),
            };
            let (face, material) =
                match require(lookup, single_id(record, slot::REVOLVE_PROFILE), "profile")? {
                    Evaluated::Face { face, material } => (face.clone(), *material),
                    other => {
                        return Err(diag(
                            EvalErrorKind::UpstreamError,
                            format!("profile input evaluated to {other:?}"),
                        ));
                    }
                };
            let axis = as_curve(
                require(lookup, single_id(record, slot::REVOLVE_AXIS), "axis")?,
                "axis",
            )?;
            let (origin, direction) = match axis {
                CurveSpec::Segment { start, end } => (
                    *start,
                    [
                        end[0] - start[0],
                        end[1] - start[1],
                        end[2] - start[2],
                    ],
                ),
                other => {
                    return Err(diag(
                        EvalErrorKind::UpstreamError,
                        format!("axis input evaluated to {other:?}, expected a line"),
                    ));
                }
            };
            let solid = kernel::revolve_solid(&face, origin, direction, angle, tol)
                .map_err(kernel_diag)?;
            Ok(Evaluated::Solid { solid, material })
        }
        EntityKind::Element => {
            let ids = multi_ids(record, slot::ELEMENT_MEMBERS);
            let mut members = Vec::with_capacity(ids.len());
            for id in ids {
                match require(lookup, Some(id), "member")? {
                    Evaluated::Solid { solid, material } => {
                        members.push((id, solid.clone(), *material));
                    }
                    other => {
                        return Err(diag(
                            EvalErrorKind::UpstreamError,
                            format!("member entity {} evaluated to {other:?}, expected a solid", id.0),
                        ));
                    }
                }
            }
            Ok(Evaluated::SolidSet(members))
        }
        EntityKind::Instance => {
            let transform = match &record.params {
                Params::Instance { transform } => *transform,
                _ => return Err(params_mismatch(record)),
            };
            let element = single_id(record, slot::INSTANCE_ELEMENT).ok_or_else(|| {
                diag(EvalErrorKind::MissingInput, "instance has no element input")
            })?;
            Ok(Evaluated::Instance { element, transform })
        }
        EntityKind::Chamfer => {
            let (distance, sub_edges) = match &record.params {
                Params::Chamfer {
                    distance,
                    sub_edges,
                } => (*distance, sub_edges.clone()),
                _ => return Err(params_mismatch(record)),
            };
            let target_id = single_id(record, slot::CHAMFER_TARGET).ok_or_else(|| {
                diag(EvalErrorKind::MissingInput, "chamfer has no target input")
            })?;
            let (solid, material) =
                match require(lookup, Some(target_id), "target")? {
                    Evaluated::Solid { solid, material } => (solid, *material),
                    other => {
                        return Err(diag(
                            EvalErrorKind::UpstreamError,
                            format!("target input evaluated to {other:?}, expected a solid"),
                        ));
                    }
                };
            let mut addresses: Vec<kernel::EdgeAddress> = Vec::new();
            for sub in &sub_edges {
                // A SubRef names topology of its owner; a chamfer can
                // only blend edges of its own target (§3.4).
                if sub.owner != target_id {
                    return Err(diag(
                        EvalErrorKind::UnresolvedSubRef,
                        format!(
                            "SubRef owner (entity {}) is not the chamfer's target \
                             (entity {})",
                            sub.owner.0, target_id.0
                        ),
                    ));
                }
                match sub.path.canonical() {
                    crate::subref::ProvenancePath::SharedEdge { a, b } => {
                        addresses.push(kernel::EdgeAddress::Shared { a: *a, b: *b });
                    }
                    other => {
                        return Err(diag(
                            EvalErrorKind::UnresolvedSubRef,
                            format!(
                                "provenance path {other:?} addresses a face, not an \
                                 edge (use a SharedEdge path)"
                            ),
                        ));
                    }
                }
            }
            // Authored Edge entities (and, later, Selections) from the
            // edges slot: matched to coincident solid edges.
            for edge_id in multi_ids(record, slot::CHAMFER_EDGES) {
                let curve = as_curve(require(lookup, Some(edge_id), "edge")?, "edge")?;
                addresses.push(kernel::EdgeAddress::Coincident(curve.clone()));
            }
            let chamfered = kernel::chamfer_solid(solid, &addresses, distance, tol)
                .map_err(kernel_diag)?;
            Ok(Evaluated::Solid {
                solid: chamfered,
                material,
            })
        }
        // Not evaluated in this milestone (docs/ARCHITECTURE.md §3.5,
        // §5.2): clean per-entity errors, never pipeline failures.
        EntityKind::Selection => Err(diag(
            EvalErrorKind::NotYetImplemented,
            "Selection evaluation is not implemented in this milestone",
        )),
        EntityKind::SectionBox => Err(diag(
            EvalErrorKind::NotYetImplemented,
            "SectionBox evaluation is not implemented in this milestone",
        )),
    }
}

fn params_mismatch(record: &EntityRecord) -> EvalDiag {
    diag(
        EvalErrorKind::Kernel,
        format!(
            "params variant does not match entity kind {:?} (substrate bug)",
            record.kind()
        ),
    )
}

// ---------------------------------------------------------------------
// Wire chaining (docs/ARCHITECTURE.md §3.1: closure and ordering are
// validated at evaluation time).
// ---------------------------------------------------------------------

fn points_near(a: [f64; 3], b: [f64; 3], tol: f64) -> bool {
    kernel::norm([a[0] - b[0], a[1] - b[1], a[2] - b[2]]) <= tol
}

/// Chain the edges of a wire head-to-tail within `tol`, flipping edge
/// orientation where needed (edges authored backwards still chain), and
/// validate that the loop closes. A single closed curve (full circle) is
/// a valid wire on its own; closed curves cannot be chained with others.
/// Each curve carries its source `Edge` entity id — the stable id
/// provenance naming derives from (docs/ARCHITECTURE.md §3.4) — which
/// follows the curve through reordering and flips.
fn chain_wire(specs: Vec<(CurveSpec, EntityId)>, tol: f64) -> Result<WireSpec, EvalDiag> {
    if specs.is_empty() {
        return Err(EvalDiag::new(
            EvalErrorKind::WireNotClosed,
            "wire has no edges",
        ));
    }
    let closed_count = specs
        .iter()
        .filter(|(s, _)| s.endpoints().is_none())
        .count();
    if closed_count > 0 {
        if let [(curve, source)] = specs.as_slice() {
            return Ok(WireSpec::with_sources(
                vec![curve.clone()],
                vec![*source],
            ));
        }
        return Err(EvalDiag::new(
            EvalErrorKind::WireNotClosed,
            "a closed curve (full circle) cannot be chained with other edges in one wire",
        ));
    }
    if specs.len() == 1 {
        return Err(EvalDiag::new(
            EvalErrorKind::WireNotClosed,
            "a single open edge cannot form a closed wire",
        ));
    }

    // The first edge fixes the traversal direction; both of its
    // orientations are legal (a fully reversed edge list still chains).
    match chain_attempt(specs.clone(), false, tol) {
        Ok(wire) => Ok(wire),
        Err(forward_error) => {
            chain_attempt(specs, true, tol).map_err(|_| forward_error)
        }
    }
}

/// One chaining attempt, with the first edge optionally reversed.
fn chain_attempt(
    specs: Vec<(CurveSpec, EntityId)>,
    flip_first: bool,
    tol: f64,
) -> Result<WireSpec, EvalDiag> {
    let mut iter = specs.into_iter();
    let Some((mut first, first_source)) = iter.next() else {
        return Err(EvalDiag::new(
            EvalErrorKind::WireNotClosed,
            "wire has no edges",
        ));
    };
    if flip_first {
        first = first.reversed();
    }
    let Some((chain_start, mut current_end)) = first.endpoints() else {
        return Err(EvalDiag::new(
            EvalErrorKind::WireNotClosed,
            "edge has no endpoints",
        ));
    };
    let mut chained: Vec<CurveSpec> = vec![first];
    let mut sources: Vec<EntityId> = vec![first_source];
    for (index, (spec, source)) in iter.enumerate() {
        let Some((start, end)) = spec.endpoints() else {
            return Err(EvalDiag::new(
                EvalErrorKind::WireNotClosed,
                "edge has no endpoints",
            ));
        };
        if points_near(start, current_end, tol) {
            current_end = end;
            chained.push(spec);
        } else if points_near(end, current_end, tol) {
            current_end = start;
            chained.push(spec.reversed());
        } else {
            return Err(EvalDiag::new(
                EvalErrorKind::WireNotClosed,
                format!(
                    "edge {} does not chain: neither endpoint meets the previous \
                     edge's end within {tol} m",
                    index + 2
                ),
            ));
        }
        sources.push(source);
    }
    if !points_near(current_end, chain_start, tol) {
        return Err(EvalDiag::new(
            EvalErrorKind::WireNotClosed,
            format!("wire does not close: last edge ends away from the first edge's start (> {tol} m)"),
        ));
    }
    Ok(WireSpec::with_sources(chained, sources))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(start: [f64; 3], end: [f64; 3]) -> CurveSpec {
        CurveSpec::Segment { start, end }
    }

    #[test]
    fn chain_accepts_flipped_edges_and_rejects_gaps() {
        let tol = 1e-6;
        // Triangle with the second edge authored backwards; sources must
        // follow their curves through reordering and flips.
        let ok = chain_wire(
            vec![
                (seg([0.0; 3], [1.0, 0.0, 0.0]), EntityId(11)),
                (seg([0.0, 1.0, 0.0], [1.0, 0.0, 0.0]), EntityId(12)), // flipped
                (seg([0.0, 1.0, 0.0], [0.0; 3]), EntityId(13)),
            ],
            tol,
        );
        assert!(ok.is_ok());
        if let Ok(wire) = ok {
            assert_eq!(
                wire.sources,
                vec![EntityId(11), EntityId(12), EntityId(13)]
            );
        }

        // Gap: does not close.
        let gap = chain_wire(
            vec![
                (seg([0.0; 3], [1.0, 0.0, 0.0]), EntityId(11)),
                (seg([1.0, 0.0, 0.0], [1.0, 1.0, 0.0]), EntityId(12)),
            ],
            tol,
        );
        assert!(matches!(
            gap.err().map(|d| d.kind),
            Some(EvalErrorKind::WireNotClosed)
        ));

        // Single open edge cannot close.
        let single = chain_wire(vec![(seg([0.0; 3], [1.0, 0.0, 0.0]), EntityId(11))], tol);
        assert!(single.is_err());

        // Single full circle is fine.
        let circle = chain_wire(
            vec![(
                CurveSpec::Circle {
                    center: [0.0; 3],
                    normal: [0.0, 0.0, 1.0],
                    radius: 1.0,
                },
                EntityId(11),
            )],
            tol,
        );
        assert!(circle.is_ok());
    }

    #[test]
    fn waves_respect_dependencies() {
        use crate::entity::{EntityRecord, Params, SlotValue};
        let mut records = BTreeMap::new();
        records.insert(
            EntityId(1),
            EntityRecord {
                id: EntityId(1),
                params: Params::ControlPoint { position: [0.0; 3] },
                inputs: vec![],
            },
        );
        records.insert(
            EntityId(2),
            EntityRecord {
                id: EntityId(2),
                params: Params::ControlPoint { position: [1.0, 0.0, 0.0] },
                inputs: vec![],
            },
        );
        records.insert(
            EntityId(3),
            EntityRecord {
                id: EntityId(3),
                params: Params::Line,
                inputs: vec![
                    SlotValue::One(Some(EntityId(1))),
                    SlotValue::One(Some(EntityId(2))),
                ],
            },
        );
        let waves = topo_waves(&records);
        assert_eq!(waves.len(), 2);
        assert_eq!(
            waves.first().map(Vec::len).unwrap_or_default() + waves.get(1).map(Vec::len).unwrap_or_default(),
            3
        );
        assert_eq!(waves.get(1), Some(&vec![EntityId(3)]));
    }
}

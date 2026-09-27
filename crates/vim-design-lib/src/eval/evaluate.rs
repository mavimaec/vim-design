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

// ---------------------------------------------------------------------
// Evaluation spaces (translation factoring — see eval/mod.rs docs).
// ---------------------------------------------------------------------

/// The coordinate space an entity's geometry is evaluated in.
///
/// `Level(l)` = level-local space of construction plane `l` (identical
/// to world except that the level's origin is treated as zero) — chosen
/// when the entity's *entire* spatial input closure is attached to that
/// one level. The mesh owner then carries the level origin as a base
/// transform, so an elevation drag is transform-only. Anything else
/// (unattached points, mixed frames, explicit Plane inputs) is `World`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Space {
    World,
    Level(EntityId),
}

/// Structural space assignment for the dirty batch (waves in topo
/// order), consulting `cache` for out-of-batch inputs. Two passes:
/// bottom-up combination, then a top-down demotion so kernel *handles*
/// (Face/Solid values) never cross spaces — a handle whose consumer is
/// World-space is itself World, recursively; plain-data specs (points,
/// curves, wires) may cross and are translated at consumption.
pub(crate) fn compute_spaces(
    graph: &crate::graph::GraphState,
    records: &BTreeMap<EntityId, EntityRecord>,
    waves: &[Vec<EntityId>],
    cache: &BTreeMap<EntityId, Space>,
) -> BTreeMap<EntityId, Space> {
    let mut spaces: BTreeMap<EntityId, Space> = BTreeMap::new();
    let space_of = |spaces: &BTreeMap<EntityId, Space>, id: EntityId| -> Space {
        spaces
            .get(&id)
            .or_else(|| cache.get(&id))
            .copied()
            .unwrap_or(Space::World)
    };
    // Combine the spaces of the record's *spatial* inputs (geometry-kind
    // slots only): a unique level wins, anything mixed is World.
    let combine = |spaces: &BTreeMap<EntityId, Space>,
                   inputs: &mut dyn Iterator<Item = EntityId>|
     -> Space {
        let mut unified: Option<Space> = None;
        for input in inputs {
            let space = space_of(spaces, input);
            unified = Some(match unified {
                None => space,
                Some(existing) if existing == space => existing,
                Some(_) => return Space::World,
            });
        }
        unified.unwrap_or(Space::World)
    };
    let spatial = |kind: EntityKind| {
        matches!(
            kind,
            EntityKind::ControlPoint
                | EntityKind::Line
                | EntityKind::Circle
                | EntityKind::Spline
                | EntityKind::Edge
                | EntityKind::Wire
                | EntityKind::Face
                | EntityKind::Solid
                | EntityKind::Extrusion
                | EntityKind::Revolve
                | EntityKind::Chamfer
                | EntityKind::Sketch
                | EntityKind::Wall
        )
    };

    // Pass 1: bottom-up.
    for wave in waves {
        for id in wave {
            let Some(record) = records.get(id) else { continue };
            let space = match record.kind() {
                // Geometry on a construction plane lives in the space of
                // the plane's root level (a workplane's offset is part of
                // the level-local geometry).
                EntityKind::ControlPoint => plane_space(graph, record, slot::CONTROL_POINT_PLANE),
                EntityKind::Sketch => plane_space(graph, record, slot::SKETCH_PLANE),
                // A wall constrained by a top plane depends on two planes:
                // world space.
                EntityKind::Wall => {
                    if record.inputs.get(slot::WALL_TOP).is_some_and(|s| !s.is_empty()) {
                        Space::World
                    } else {
                        plane_space(graph, record, slot::WALL_BASE)
                    }
                }
                // An explicit Plane input anchors the entity to world
                // coordinates (conservative disqualifier, documented).
                EntityKind::Circle
                    if record
                        .inputs
                        .get(slot::CIRCLE_PLANE)
                        .is_some_and(|s| !s.is_empty()) =>
                {
                    Space::World
                }
                EntityKind::Face
                    if record
                        .inputs
                        .get(slot::FACE_PLANE)
                        .is_some_and(|s| !s.is_empty()) =>
                {
                    Space::World
                }
                EntityKind::Element => {
                    // Members only: the level *association* slot is
                    // data-only and must not affect spaces.
                    let mut members = record
                        .inputs
                        .get(slot::ELEMENT_MEMBERS)
                        .map(|s| s.referenced().collect::<Vec<_>>())
                        .unwrap_or_default()
                        .into_iter();
                    combine(&spaces, &mut members)
                }
                kind if spatial(kind) => {
                    // Spatial inputs only: materials, planes (guarded
                    // above), and levels never affect the space.
                    let mut inputs = record
                        .referenced()
                        .filter(|input| {
                            graph
                                .get(*input)
                                .map(|r| spatial(r.kind()))
                                .unwrap_or(false)
                        })
                        .collect::<Vec<_>>()
                        .into_iter();
                    combine(&spaces, &mut inputs)
                }
                _ => Space::World,
            };
            spaces.insert(*id, space);
        }
    }

    // Pass 2: top-down demotion (reverse topo): a handle producer with a
    // World-space handle consumer becomes World itself.
    for wave in waves.iter().rev() {
        for id in wave {
            let Some(record) = records.get(id) else { continue };
            if spaces.get(id) != Some(&Space::World) {
                continue;
            }
            // Handle-consuming slots per kind.
            let handle_inputs: Vec<EntityId> = match record.kind() {
                EntityKind::Extrusion => record
                    .inputs
                    .get(slot::EXTRUSION_PROFILE)
                    .map(|s| s.referenced().collect())
                    .unwrap_or_default(),
                EntityKind::Revolve => record
                    .inputs
                    .get(slot::REVOLVE_PROFILE)
                    .map(|s| s.referenced().collect())
                    .unwrap_or_default(),
                EntityKind::Solid => record
                    .inputs
                    .get(slot::SOLID_FACES)
                    .map(|s| s.referenced().collect())
                    .unwrap_or_default(),
                EntityKind::Chamfer => record
                    .inputs
                    .get(slot::CHAMFER_TARGET)
                    .map(|s| s.referenced().collect())
                    .unwrap_or_default(),
                EntityKind::Element => record
                    .inputs
                    .get(slot::ELEMENT_MEMBERS)
                    .map(|s| s.referenced().collect())
                    .unwrap_or_default(),
                _ => Vec::new(),
            };
            for input in handle_inputs {
                if records.contains_key(&input) {
                    spaces.insert(input, Space::World);
                }
                // Out-of-batch handle inputs cannot disagree: a wiring
                // or space change dirties the whole downstream closure,
                // so producer and consumer are always in-batch together
                // when either side changes.
            }
        }
    }
    spaces
}

/// The space of an entity drawn on the plane wired into `slot`: the
/// plane's root level, or world space when the slot is empty or the
/// chain does not end at a level.
fn plane_space(graph: &crate::graph::GraphState, record: &EntityRecord, slot_index: usize) -> Space {
    record
        .inputs
        .get(slot_index)
        .and_then(|s| s.referenced().next())
        .and_then(|plane| crate::workplane::root_level_in(graph, plane))
        .map_or(Space::World, Space::Level)
}

/// Cheap-and-exact value equality for the early cutoff. Plain-data
/// variants compare exactly; kernel-handle variants (`Face`, `Solid`,
/// `SolidSet`) are NOT comparable and always count as changed
/// (documented limitation — comparing BREP handles would be neither
/// cheap nor reliable).
fn value_equal(a: &Evaluated, b: &Evaluated) -> bool {
    match (a, b) {
        (Evaluated::Point(x), Evaluated::Point(y)) => x == y,
        (
            Evaluated::Plane { origin: ao, normal: an },
            Evaluated::Plane { origin: bo, normal: bn },
        ) => ao == bo && an == bn,
        (Evaluated::Curve(x), Evaluated::Curve(y))
        | (Evaluated::Edge(x), Evaluated::Edge(y)) => x == y,
        (Evaluated::Wire(x), Evaluated::Wire(y)) => x == y,
        (Evaluated::Material, Evaluated::Material) => true,
        (Evaluated::Site, Evaluated::Site) => true,
        (
            Evaluated::Frame {
                origin: ao,
                x_axis: ax,
                y_axis: ay,
                z_axis: az,
                level_offset: al,
            },
            Evaluated::Frame {
                origin: bo,
                x_axis: bx,
                y_axis: by,
                z_axis: bz,
                level_offset: bl,
            },
        ) => ao == bo && ax == bx && ay == by && az == bz && al == bl,
        (
            Evaluated::Instance { element: ae, transform: at },
            Evaluated::Instance { element: be, transform: bt },
        ) => ae == be && at == bt,
        _ => false,
    }
}

/// The inputs an entity's evaluator actually READS — the change-driven
/// re-evaluation trigger set. Data-only association edges are exempt:
/// an `Element`'s level slot has zero geometric effect (docs/AUTHORING.md
/// §4 — the element evaluator ignores it), so a level *value* change
/// (elevation drag) must never re-evaluate the element through that
/// edge. Rewiring the association still re-evaluates (the element is a
/// commit-gate root then), and cascade/dependent semantics are
/// structural, not evaluation — both unaffected by this exemption.
fn evaluation_inputs(record: &EntityRecord) -> Vec<EntityId> {
    match record.kind() {
        EntityKind::Element => record
            .inputs
            .get(slot::ELEMENT_MEMBERS)
            .map(|s| s.referenced().collect())
            .unwrap_or_default(),
        _ => record.referenced().collect(),
    }
}

/// Result of one wave-batch evaluation.
pub(crate) struct WaveResult {
    /// Outcomes for the entities that actually evaluated (skipped
    /// entities are absent — their cached value/state stands).
    pub outcomes: BTreeMap<EntityId, Result<Evaluated, EvalDiag>>,
    /// Entities whose observable result changed (value inequality,
    /// incomparable kernel handles, or an error-state transition).
    pub changed: std::collections::BTreeSet<EntityId>,
    /// Levels whose `Frame` changed (drives cross-space re-evaluation
    /// and transform-only mesh re-placement).
    pub frames_changed: std::collections::BTreeSet<EntityId>,
}

/// Evaluate the batch wave by wave with the early cutoff
/// (docs eval/mod.rs "Evaluation performance"): a non-root entity
/// evaluates only if it has never evaluated, an input's result changed,
/// or a cross-space input's level frame moved. Pure: only reads its
/// arguments. Within a wave entities evaluate in parallel under the
/// `parallel` feature (docs/ARCHITECTURE.md §6.2).
pub(crate) fn evaluate_waves(
    records: &BTreeMap<EntityId, EntityRecord>,
    waves: &[Vec<EntityId>],
    roots: &std::collections::BTreeSet<EntityId>,
    spaces: &BTreeMap<EntityId, Space>,
    settings: &DocumentSettings,
    base: &BTreeMap<EntityId, EntityEval>,
) -> WaveResult {
    let mut fresh: BTreeMap<EntityId, Result<Evaluated, EvalDiag>> = BTreeMap::new();
    let mut changed = std::collections::BTreeSet::new();
    let mut frames_changed = std::collections::BTreeSet::new();
    let space_of = |id: EntityId| spaces.get(&id).copied().unwrap_or(Space::World);
    for wave in waves {
        let todo: Vec<EntityId> = wave
            .iter()
            .filter(|id| {
                let Some(record) = records.get(id) else { return false };
                if roots.contains(id) {
                    return true; // its own params/wiring changed
                }
                let Some(entry) = base.get(id) else {
                    return true; // never evaluated (first sight / load)
                };
                if entry.state.is_none() {
                    return true;
                }
                let own_space = space_of(**id);
                // Level-local evaluation reads a frame's axes and its
                // offset from the root level, never its world origin (the
                // root origin travels as the owner's base transform). A
                // frame that only moved with its root level therefore does
                // not re-evaluate level-local consumers.
                let frame_local_unchanged = |input: EntityId| {
                    let local = |value: &Evaluated| match value {
                        Evaluated::Frame {
                            x_axis,
                            y_axis,
                            z_axis,
                            level_offset,
                            ..
                        } => Some((*x_axis, *y_axis, *z_axis, *level_offset)),
                        _ => None,
                    };
                    let old = base.get(&input).and_then(|e| e.value.as_ref()).and_then(local);
                    let new = match fresh.get(&input) {
                        Some(Ok(value)) => local(value),
                        _ => None,
                    };
                    old.is_some() && old == new
                };
                evaluation_inputs(record).into_iter().any(|input| {
                    if changed.contains(&input) {
                        return !(matches!(own_space, Space::Level(_))
                            && frame_local_unchanged(input));
                    }
                    // Cross-space consumption: a level-local input is
                    // world-ified with the level's CURRENT origin, so a
                    // frame move re-evaluates the consumer even though
                    // the input's local value is unchanged.
                    match space_of(input) {
                        Space::Level(level) => {
                            frames_changed.contains(&level)
                                && own_space != Space::Level(level)
                        }
                        Space::World => false,
                    }
                })
            })
            .copied()
            .collect();
        let lookup = Lookup {
            fresh: &fresh,
            base,
        };
        let run = |id: &EntityId| -> Option<(EntityId, Result<Evaluated, EvalDiag>)> {
            let record = records.get(id)?;
            Some((*id, evaluate_entity(record, &lookup, settings, spaces)))
        };
        #[cfg(feature = "parallel")]
        let outcomes: Vec<_> = todo.par_iter().filter_map(run).collect();
        #[cfg(not(feature = "parallel"))]
        let outcomes: Vec<_> = todo.iter().filter_map(run).collect();
        for (id, outcome) in outcomes {
            let previous = base.get(&id);
            let was_error = matches!(
                previous.and_then(|e| e.state.as_ref()),
                Some(super::types::EvalState::Error { .. })
            ) || previous.is_none();
            let result_changed = match &outcome {
                Err(_) => true,
                Ok(value) => {
                    was_error
                        || previous
                            .and_then(|e| e.value.as_ref())
                            .map(|old| !value_equal(value, old))
                            .unwrap_or(true)
                }
            };
            if result_changed {
                changed.insert(id);
                if records.get(&id).map(|r| r.kind()) == Some(EntityKind::Level) {
                    frames_changed.insert(id);
                }
            }
            fresh.insert(id, outcome);
        }
    }
    WaveResult {
        outcomes: fresh,
        changed,
        frames_changed,
    }
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

/// The origin of a level's frame (for world-ifying level-local inputs
/// at space crossings).
fn frame_origin(lookup: &Lookup<'_>, level: EntityId) -> Result<[f64; 3], EvalDiag> {
    match lookup.value(level) {
        Some(Evaluated::Frame { origin, .. }) => Ok(*origin),
        _ => Err(diag(
            EvalErrorKind::UpstreamError,
            format!("level entity {} has no evaluated frame", level.0),
        )),
    }
}

/// Offset to apply to an input evaluated in `input_space` when consumed
/// by an entity in `own_space` (zero when spaces match; the level origin
/// when a level-local value crosses into world space).
fn crossing_offset(
    lookup: &Lookup<'_>,
    own_space: Space,
    input_space: Space,
    spaces_note: &str,
) -> Result<[f64; 3], EvalDiag> {
    match (input_space, own_space) {
        (a, b) if a == b => Ok([0.0; 3]),
        (Space::Level(level), Space::World) => frame_origin(lookup, level),
        // A world (or other-level) input consumed in level-local space
        // cannot happen by construction (the combine rule makes any
        // mixed consumer World); reaching here is a substrate bug.
        _ => Err(diag(
            EvalErrorKind::Kernel,
            format!("inconsistent evaluation spaces at {spaces_note} (substrate bug)"),
        )),
    }
}

fn translate_point(p: [f64; 3], offset: [f64; 3]) -> [f64; 3] {
    [p[0] + offset[0], p[1] + offset[1], p[2] + offset[2]]
}

fn translate_curve(spec: &CurveSpec, offset: [f64; 3]) -> CurveSpec {
    if offset == [0.0; 3] {
        return spec.clone();
    }
    match spec {
        CurveSpec::Segment { start, end } => CurveSpec::Segment {
            start: translate_point(*start, offset),
            end: translate_point(*end, offset),
        },
        CurveSpec::Circle {
            center,
            normal,
            radius,
        } => CurveSpec::Circle {
            center: translate_point(*center, offset),
            normal: *normal,
            radius: *radius,
        },
        CurveSpec::Bezier { control_points } => CurveSpec::Bezier {
            control_points: control_points
                .iter()
                .map(|p| translate_point(*p, offset))
                .collect(),
        },
    }
}

fn translate_wire(wire: &WireSpec, offset: [f64; 3]) -> WireSpec {
    if offset == [0.0; 3] {
        return wire.clone();
    }
    WireSpec::with_sources(
        wire.curves.iter().map(|c| translate_curve(c, offset)).collect(),
        wire.sources.clone(),
    )
}

/// Evaluate one entity from its record and already-evaluated inputs.
/// Geometry is produced in the entity's assigned [`Space`]: level-local
/// values stay local (translation factoring); level-local INPUTS
/// consumed by a World-space entity are world-ified here with the
/// level's current origin.
pub(crate) fn evaluate_entity(
    record: &EntityRecord,
    lookup: &Lookup<'_>,
    settings: &DocumentSettings,
    spaces: &BTreeMap<EntityId, Space>,
) -> Result<Evaluated, EvalDiag> {
    let own_space = spaces.get(&record.id).copied().unwrap_or(Space::World);
    let space_of = |id: EntityId| spaces.get(&id).copied().unwrap_or(Space::World);
    let input_offset = |lookup: &Lookup<'_>, id: EntityId, what: &str| {
        crossing_offset(lookup, own_space, space_of(id), what)
    };
    let tol = settings.kernel_tolerance;
    match record.kind() {
        EntityKind::ControlPoint => {
            let position = match &record.params {
                Params::ControlPoint { position } => *position,
                _ => return Err(params_mismatch(record)),
            };
            match single_id(record, slot::CONTROL_POINT_PLANE) {
                // Unattached: stored coordinates ARE world coordinates.
                None => Ok(Evaluated::Point(position)),
                // Attached: stored coordinates are (u, v, w) in the
                // construction plane's frame (docs/AUTHORING.md §3).
                // Under translation factoring (own space = the level's)
                // the point stays LEVEL-LOCAL — the frame's origin is
                // delivered as the owner's base transform, never baked
                // here; otherwise (factoring off, or a world-space
                // assignment) the full world position is baked.
                Some(plane_id) => match require(lookup, Some(plane_id), "plane")? {
                    Evaluated::Frame {
                        origin,
                        x_axis,
                        y_axis,
                        z_axis,
                        level_offset,
                    } => {
                        let [u, v, w] = position;
                        let base = match own_space {
                            Space::Level(_) => *level_offset,
                            Space::World => *origin,
                        };
                        let point = [
                            base[0] + u * x_axis[0] + v * y_axis[0] + w * z_axis[0],
                            base[1] + u * x_axis[1] + v * y_axis[1] + w * z_axis[1],
                            base[2] + u * x_axis[2] + v * y_axis[2] + w * z_axis[2],
                        ];
                        Ok(Evaluated::Point(point))
                    }
                    other => Err(diag(
                        EvalErrorKind::UpstreamError,
                        format!("plane input evaluated to {other:?}, expected a frame"),
                    )),
                },
            }
        }
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
            let center_id = single_id(record, slot::CIRCLE_CENTER);
            let center = as_point(require(lookup, center_id, "center")?, "center")?;
            let center = translate_point(
                center,
                center_id
                    .map(|id| input_offset(lookup, id, "circle center"))
                    .transpose()?
                    .unwrap_or([0.0; 3]),
            );
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
            let start_id = single_id(record, slot::LINE_START);
            let end_id = single_id(record, slot::LINE_END);
            let start = as_point(require(lookup, start_id, "start")?, "start")?;
            let end = as_point(require(lookup, end_id, "end")?, "end")?;
            let start = translate_point(
                start,
                start_id
                    .map(|id| input_offset(lookup, id, "line start"))
                    .transpose()?
                    .unwrap_or([0.0; 3]),
            );
            let end = translate_point(
                end,
                end_id
                    .map(|id| input_offset(lookup, id, "line end"))
                    .transpose()?
                    .unwrap_or([0.0; 3]),
            );
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
                let offset = input_offset(lookup, id, "spline control point")?;
                control_points.push(translate_point(point, offset));
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
                let offset = input_offset(lookup, id, "wire edge")?;
                // The Edge entity id rides along as the curve's *source*:
                // the stable id provenance naming derives from (§3.4).
                specs.push((translate_curve(curve, offset), id));
            }
            let wire = chain_wire(specs, tol)?;
            Ok(Evaluated::Wire(wire))
        }
        EntityKind::Face => {
            let outer_id = single_id(record, slot::FACE_OUTER);
            let outer = match require(lookup, outer_id, "outer wire")? {
                Evaluated::Wire(wire) => wire.clone(),
                other => {
                    return Err(diag(
                        EvalErrorKind::UpstreamError,
                        format!("outer wire input evaluated to {other:?}"),
                    ));
                }
            };
            let outer = translate_wire(
                &outer,
                outer_id
                    .map(|id| input_offset(lookup, id, "face outer wire"))
                    .transpose()?
                    .unwrap_or([0.0; 3]),
            );
            let mut holes = Vec::new();
            for id in multi_ids(record, slot::FACE_HOLES) {
                match require(lookup, Some(id), "hole wire")? {
                    Evaluated::Wire(wire) => {
                        let offset = input_offset(lookup, id, "face hole wire")?;
                        holes.push(translate_wire(wire, offset));
                    }
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
            let axis_id = single_id(record, slot::REVOLVE_AXIS);
            let axis = as_curve(require(lookup, axis_id, "axis")?, "axis")?;
            let axis_offset = axis_id
                .map(|id| input_offset(lookup, id, "revolve axis"))
                .transpose()?
                .unwrap_or([0.0; 3]);
            let axis = &translate_curve(axis, axis_offset);
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
                    // A sketch member contributes all of its prisms.
                    Evaluated::SolidSet(prisms) => members.extend(prisms.iter().cloned()),
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
            let slot_edge_ids = multi_ids(record, slot::CHAMFER_EDGES);
            if sub_edges.is_empty() && slot_edge_ids.is_empty() {
                return Err(diag(
                    EvalErrorKind::Degenerate,
                    "chamfer has no edge targets",
                ));
            }
            let mut addresses: Vec<kernel::EdgeAddress> = Vec::new();
            for target_ref in &sub_edges {
                match target_ref {
                    crate::subref::EdgeTarget::One(sub) => {
                        // A SubRef names topology of its owner; a chamfer
                        // can only blend edges of its own target (§3.4).
                        if sub.owner != target_id {
                            return Err(diag(
                                EvalErrorKind::UnresolvedSubRef,
                                format!(
                                    "SubRef owner (entity {}) is not the chamfer's \
                                     target (entity {})",
                                    sub.owner.0, target_id.0
                                ),
                            ));
                        }
                        match sub.path.canonical() {
                            crate::subref::ProvenancePath::SharedEdge { a, b } => {
                                addresses
                                    .push(kernel::EdgeAddress::Shared { a: *a, b: *b });
                            }
                            other => {
                                return Err(diag(
                                    EvalErrorKind::UnresolvedSubRef,
                                    format!(
                                        "provenance path {other:?} addresses a face, \
                                         not an edge (use a SharedEdge path)"
                                    ),
                                ));
                            }
                        }
                    }
                    crate::subref::EdgeTarget::Set(set) => {
                        if set.owner != target_id {
                            return Err(diag(
                                EvalErrorKind::UnresolvedSubRef,
                                format!(
                                    "SubRefSet owner (entity {}) is not the chamfer's \
                                     target (entity {})",
                                    set.owner.0, target_id.0
                                ),
                            ));
                        }
                        // Live expansion against the target's CURRENT
                        // topology; an empty expansion is a valid no-op
                        // (the liveness contract, §3.5).
                        let expansion = kernel::expand_query(solid, &set.query)
                            .map_err(kernel_diag)?;
                        if !expansion.face_paths.is_empty() {
                            return Err(diag(
                                EvalErrorKind::UnresolvedSubRef,
                                "query expands to faces; chamfer edges need an \
                                 edge-valued query (RimEdges/VerticalEdges)",
                            ));
                        }
                        for path in expansion.edge_paths {
                            if let crate::subref::ProvenancePath::SharedEdge { a, b } =
                                path
                            {
                                addresses
                                    .push(kernel::EdgeAddress::Shared { a: *a, b: *b });
                            }
                        }
                    }
                }
            }
            // Authored Edge entities (and, later, Selections) from the
            // edges slot: matched to coincident solid edges.
            for edge_id in slot_edge_ids {
                let curve = as_curve(require(lookup, Some(edge_id), "edge")?, "edge")?;
                let offset = input_offset(lookup, edge_id, "chamfer edge")?;
                addresses.push(kernel::EdgeAddress::Coincident(translate_curve(
                    curve, offset,
                )));
            }
            if addresses.is_empty() {
                // Targets were configured but every query set expanded to
                // nothing (e.g. RimEdges{HolesOnly} on a hole-less
                // profile): the chamfer is a pass-through no-op that
                // starts blending as soon as matching topology appears.
                return Ok(Evaluated::Solid {
                    solid: solid.clone(),
                    material,
                });
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
        // Geolocation metadata: pass-through marker, never geometry.
        EntityKind::Site => match &record.params {
            Params::Site { .. } => Ok(Evaluated::Site),
            _ => Err(params_mismatch(record)),
        },
        // A level IS a construction plane (docs/AUTHORING.md §§2–3):
        // frame at (0, 0, elevation) with world axes — the trivially
        // deterministic basis (basis-stability rule). Display square
        // color/extent are renderer overlay inputs, not geometry: levels
        // are never tessellated and never own meshes.
        EntityKind::Level => match &record.params {
            Params::Level { elevation_m, .. } => Ok(Evaluated::Frame {
                origin: [0.0, 0.0, *elevation_m],
                x_axis: [1.0, 0.0, 0.0],
                y_axis: [0.0, 1.0, 0.0],
                z_axis: [0.0, 0.0, 1.0],
                level_offset: [0.0; 3],
            }),
            _ => Err(params_mismatch(record)),
        },
        EntityKind::Sketch => evaluate_sketch(record, lookup, own_space, tol),
        EntityKind::Workplane => {
            let offset = match &record.params {
                Params::Workplane { offset_m, .. } => *offset_m,
                _ => return Err(params_mismatch(record)),
            };
            if !offset.is_finite() {
                return Err(diag(EvalErrorKind::Degenerate, "workplane offset is not finite"));
            }
            match require(lookup, single_id(record, slot::WORKPLANE_PARENT), "parent")? {
                Evaluated::Frame {
                    origin,
                    x_axis,
                    y_axis,
                    z_axis,
                    level_offset,
                } => {
                    let shift = |p: [f64; 3]| {
                        [
                            p[0] + offset * z_axis[0],
                            p[1] + offset * z_axis[1],
                            p[2] + offset * z_axis[2],
                        ]
                    };
                    Ok(Evaluated::Frame {
                        origin: shift(*origin),
                        x_axis: *x_axis,
                        y_axis: *y_axis,
                        z_axis: *z_axis,
                        level_offset: shift(*level_offset),
                    })
                }
                other => Err(diag(
                    EvalErrorKind::UpstreamError,
                    format!("parent input evaluated to {other:?}, expected a frame"),
                )),
            }
        }
        EntityKind::Wall => evaluate_wall(record, lookup, own_space, tol),
    }
}

/// A construction-plane frame as the evaluating entity sees it: the
/// origin is the level offset in level-local space and the world origin
/// otherwise.
struct PlaneFrame {
    origin: [f64; 3],
    world_origin: [f64; 3],
    x_axis: [f64; 3],
    y_axis: [f64; 3],
    z_axis: [f64; 3],
}

fn plane_frame(
    lookup: &Lookup<'_>,
    plane: Option<EntityId>,
    what: &str,
    own_space: Space,
) -> Result<PlaneFrame, EvalDiag> {
    match require(lookup, plane, what)? {
        Evaluated::Frame {
            origin,
            x_axis,
            y_axis,
            z_axis,
            level_offset,
        } => Ok(PlaneFrame {
            origin: match own_space {
                Space::Level(_) => *level_offset,
                Space::World => *origin,
            },
            world_origin: *origin,
            x_axis: *x_axis,
            y_axis: *y_axis,
            z_axis: *z_axis,
        }),
        other => Err(diag(
            EvalErrorKind::UpstreamError,
            format!("{what} input evaluated to {other:?}, expected a frame"),
        )),
    }
}

/// The prisms of a 2D profile placed in 3D: profile point (u, v) sits at
/// `origin + u * x_axis + v * y_axis`, and material at depth d sits
/// `d` along `depth_axis`. Faces are named from the profile's stable
/// ids (`SketchSide`, `SketchCap`).
fn profile_prisms(
    owner: EntityId,
    profile: &crate::sketch::Sketch,
    origin: [f64; 3],
    x_axis: [f64; 3],
    y_axis: [f64; 3],
    depth_axis: [f64; 3],
    tol: f64,
) -> Result<Evaluated, EvalDiag> {
    use crate::subref::ProvenancePath;

    let at = |uv: [f64; 2], depth: f64| -> [f64; 3] {
        let [u, v] = uv;
        [
            origin[0] + u * x_axis[0] + v * y_axis[0] + depth * depth_axis[0],
            origin[1] + u * x_axis[1] + v * y_axis[1] + depth * depth_axis[1],
            origin[2] + u * x_axis[2] + v * y_axis[2] + depth * depth_axis[2],
        ]
    };
    let depth_um = |depth: f64| (depth * 1.0e6).round() as i64;
    let prisms = crate::sketch::layers::layered_prisms(profile)
        .map_err(|err| diag(EvalErrorKind::Degenerate, format!("profile: {err}")))?;
    let mut solids = Vec::with_capacity(prisms.len());
    for prism in prisms {
        let loops: Vec<kernel::PrismLoop> = prism
            .contours
            .iter()
            .map(|contour| kernel::PrismLoop {
                points: contour.points.iter().map(|uv| at(*uv, prism.top)).collect(),
                names: contour
                    .sides
                    .iter()
                    .map(|side| {
                        side.map(|s| ProvenancePath::SketchSide {
                            face: s.face,
                            a: s.a,
                            b: s.b,
                        })
                    })
                    .collect(),
            })
            .collect();
        let Some((outer, holes)) = loops.split_first() else {
            continue;
        };
        let step = prism.bottom - prism.top;
        let extrude = [
            step * depth_axis[0],
            step * depth_axis[1],
            step * depth_axis[2],
        ];
        let solid = kernel::prism_solid(
            outer,
            holes,
            extrude,
            ProvenancePath::SketchCap {
                depth_um: depth_um(prism.top),
                toward_plane: true,
            },
            ProvenancePath::SketchCap {
                depth_um: depth_um(prism.bottom),
                toward_plane: false,
            },
            tol,
        )
        .map_err(kernel_diag)?;
        solids.push((owner, solid, None));
    }
    Ok(Evaluated::SolidSet(solids))
}

/// Evaluate a sketch into its prisms, in its plane's frame. In
/// level-local space the frame origin is the plane's offset from its
/// root level (the root elevation travels as the owner's base
/// transform); in world space it is the frame's origin.
fn evaluate_sketch(
    record: &EntityRecord,
    lookup: &Lookup<'_>,
    own_space: Space,
    tol: f64,
) -> Result<Evaluated, EvalDiag> {
    use crate::sketch::SketchDirection;

    let (sketch, direction) = match &record.params {
        Params::Sketch { sketch, direction } => (sketch, *direction),
        _ => return Err(params_mismatch(record)),
    };
    let frame = plane_frame(lookup, single_id(record, slot::SKETCH_PLANE), "plane", own_space)?;
    let sign = match direction {
        SketchDirection::Below => -1.0,
        SketchDirection::Above => 1.0,
    };
    let z = frame.z_axis;
    profile_prisms(
        record.id,
        sketch,
        frame.origin,
        frame.x_axis,
        frame.y_axis,
        [sign * z[0], sign * z[1], sign * z[2]],
        tol,
    )
}

/// Evaluate a wall: its effective elevation profile (top-anchored
/// points raised to the top reference height) in the wall's vertical
/// frame — u along the reference line from `start`, v along the base
/// plane's normal — with material toward the left of start -> end.
fn evaluate_wall(
    record: &EntityRecord,
    lookup: &Lookup<'_>,
    own_space: Space,
    tol: f64,
) -> Result<Evaluated, EvalDiag> {
    let (start, end, height_m, top_offset_m, profile, top_points) = match &record.params {
        Params::Wall {
            start,
            end,
            height_m,
            top_offset_m,
            profile,
            top_points,
        } => (*start, *end, *height_m, *top_offset_m, profile, top_points),
        _ => return Err(params_mismatch(record)),
    };
    let base = plane_frame(lookup, single_id(record, slot::WALL_BASE), "base", own_space)?;
    let height = match single_id(record, slot::WALL_TOP) {
        Some(top_id) => {
            let top = plane_frame(lookup, Some(top_id), "top", Space::World)?;
            let rise = [
                top.world_origin[0] - base.world_origin[0],
                top.world_origin[1] - base.world_origin[1],
                top.world_origin[2] - base.world_origin[2],
            ];
            kernel::dot(rise, base.z_axis) + top_offset_m
        }
        None => height_m,
    };
    if !(height.is_finite() && height > tol) {
        return Err(diag(
            EvalErrorKind::Degenerate,
            format!("wall top reference is {height} m above its base (must be positive)"),
        ));
    }
    let (du, dv) = (end[0] - start[0], end[1] - start[1]);
    let length = (du * du + dv * dv).sqrt();
    if !(length.is_finite() && length > tol) {
        return Err(diag(EvalErrorKind::Degenerate, "wall reference line has no length"));
    }
    let (du, dv) = (du / length, dv / length);
    let in_plane = |u: f64, v: f64| -> [f64; 3] {
        [
            u * base.x_axis[0] + v * base.y_axis[0],
            u * base.x_axis[1] + v * base.y_axis[1],
            u * base.x_axis[2] + v * base.y_axis[2],
        ]
    };
    let along = in_plane(du, dv);
    let left = in_plane(-dv, du);
    let start_3d = in_plane(start[0], start[1]);
    let origin = [
        base.origin[0] + start_3d[0],
        base.origin[1] + start_3d[1],
        base.origin[2] + start_3d[2],
    ];
    let effective = crate::wall::effective_profile(profile, top_points, height);
    profile_prisms(record.id, &effective, origin, along, base.z_axis, left, tol)
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
                inputs: vec![SlotValue::One(None)],
            },
        );
        records.insert(
            EntityId(2),
            EntityRecord {
                id: EntityId(2),
                params: Params::ControlPoint { position: [1.0, 0.0, 0.0] },
                inputs: vec![SlotValue::One(None)],
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

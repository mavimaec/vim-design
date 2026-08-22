//! User-level commands (docs/ARCHITECTURE.md §4).
//!
//! Commands are *intents*; they compile to sequences of the four primitive
//! deltas, applied speculatively: each delta is validated and applied in
//! order, and if any step fails the already-applied deltas roll back in
//! reverse (see `Document::submit`), leaving the document byte-identical
//! to its state before the attempt.
//!
//! Validation is **structural only**: ids exist, slot kinds match, no
//! cycles, no dependents on delete. Geometric failures surface later as
//! per-entity evaluation errors (§6.4), never as command rejections.

use serde::{Deserialize, Serialize};

use crate::document::Document;
use crate::entity::{
    EntityKind, EntityRecord, Params, SlotValue, slot,
};
use crate::delta::Delta;
use crate::id::EntityId;
use crate::selection::{PredicateAst, SelectionScope};
use crate::status::VimStatus;

/// The closed set of user-level commands — the full requirements list
/// (docs/PROJECT_REQUIREMENTS.md) plus the composite cylinder commands.
///
/// `Update*` commands carry a `coalesce` flag: consecutive coalesced
/// updates to the same entity merge into one undo step (first old-value
/// wins, last new-value wins — docs/ARCHITECTURE.md §4.2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Command {
    // -- ControlPoint --------------------------------------------------
    CreateControlPoint {
        position: [f64; 3],
    },
    UpdateControlPoint {
        id: EntityId,
        position: [f64; 3],
        coalesce: bool,
    },
    DeleteControlPoint {
        id: EntityId,
    },
    // -- Plane -----------------------------------------------------------
    CreatePlane {
        origin: [f64; 3],
        normal: [f64; 3],
    },
    UpdatePlane {
        id: EntityId,
        origin: Option<[f64; 3]>,
        normal: Option<[f64; 3]>,
        coalesce: bool,
    },
    DeletePlane {
        id: EntityId,
    },
    // -- Circle ------------------------------------------------------
    CreateCircle {
        center: EntityId,
        plane: Option<EntityId>,
        radius: f64,
    },
    UpdateCircle {
        id: EntityId,
        radius: Option<f64>,
        /// `Some(cp)` rewires the center input.
        center: Option<EntityId>,
        /// `Some(None)` clears the plane input; `Some(Some(p))` rewires it.
        plane: Option<Option<EntityId>>,
        coalesce: bool,
    },
    DeleteCircle {
        id: EntityId,
    },
    // -- Line ---------------------------------------------------------
    CreateLine {
        start: EntityId,
        end: EntityId,
    },
    UpdateLine {
        id: EntityId,
        start: Option<EntityId>,
        end: Option<EntityId>,
        coalesce: bool,
    },
    DeleteLine {
        id: EntityId,
    },
    // -- Spline -----------------------------------------------------
    CreateSpline {
        control_points: Vec<EntityId>,
        degree: Option<u32>,
        knots: Option<Vec<f64>>,
    },
    UpdateSpline {
        id: EntityId,
        control_points: Option<Vec<EntityId>>,
        degree: Option<Option<u32>>,
        knots: Option<Option<Vec<f64>>>,
        coalesce: bool,
    },
    DeleteSpline {
        id: EntityId,
    },
    // -- Edge -----------------------------------------------------------
    CreateEdge {
        curve: EntityId,
    },
    UpdateEdge {
        id: EntityId,
        curve: EntityId,
        coalesce: bool,
    },
    DeleteEdge {
        id: EntityId,
    },
    // -- Wire -----------------------------------------------------------
    /// An ordered, closed loop of edges — the boundary of a face. Order
    /// is semantic (loop traversal); closure is validated at evaluation
    /// time, not structurally.
    CreateWire {
        edges: Vec<EntityId>,
    },
    UpdateWire {
        id: EntityId,
        edges: Vec<EntityId>,
        coalesce: bool,
    },
    DeleteWire {
        id: EntityId,
    },
    // -- Face -----------------------------------------------------------
    /// A face from one outer wire plus optional inner hole wires and an
    /// optional explicit surface plane (when absent, the evaluator will
    /// infer a planar surface from the outer wire).
    CreateFace {
        outer: EntityId,
        holes: Vec<EntityId>,
        plane: Option<EntityId>,
    },
    UpdateFace {
        id: EntityId,
        outer: Option<EntityId>,
        holes: Option<Vec<EntityId>>,
        /// `Some(None)` clears the plane input; `Some(Some(p))` rewires it.
        plane: Option<Option<EntityId>>,
        coalesce: bool,
    },
    /// Assign (or clear) the material input of a face.
    UpdateFaceMaterial {
        face: EntityId,
        material: Option<EntityId>,
    },
    DeleteFace {
        id: EntityId,
    },
    // -- Solid ---------------------------------------------------------
    CreateSolid {
        faces: Vec<EntityId>,
    },
    UpdateSolid {
        id: EntityId,
        faces: Vec<EntityId>,
        coalesce: bool,
    },
    DeleteSolid {
        id: EntityId,
    },
    // -- Material ---------------------------------------------------
    CreateMaterial {
        name: String,
        color: [f64; 3],
        roughness: f64,
    },
    UpdateMaterial {
        id: EntityId,
        name: Option<String>,
        color: Option<[f64; 3]>,
        roughness: Option<f64>,
        coalesce: bool,
    },
    DeleteMaterial {
        id: EntityId,
    },
    // -- Extrusion ------------------------------------------------------
    CreateExtrusion {
        profile: EntityId,
        path: EntityId,
    },
    UpdateExtrusion {
        id: EntityId,
        profile: Option<EntityId>,
        path: Option<EntityId>,
        coalesce: bool,
    },
    DeleteExtrusion {
        id: EntityId,
    },
    // -- Chamfer ----------------------------------------------------
    /// `edges` may mix explicit `Edge` ids and `Selection` ids
    /// (docs/ARCHITECTURE.md §3.5).
    CreateChamfer {
        distance: f64,
        edges: Vec<EntityId>,
    },
    UpdateChamfer {
        id: EntityId,
        distance: Option<f64>,
        edges: Option<Vec<EntityId>>,
        coalesce: bool,
    },
    DeleteChamfer {
        id: EntityId,
    },
    // -- SectionBox ----------------------------------------------------
    CreateSectionBox {
        min: [f64; 3],
        max: [f64; 3],
    },
    UpdateSectionBox {
        id: EntityId,
        min: Option<[f64; 3]>,
        max: Option<[f64; 3]>,
        coalesce: bool,
    },
    DeleteSectionBox {
        id: EntityId,
    },
    // -- Element -------------------------------------------------------
    CreateElement {
        name: String,
        members: Vec<EntityId>,
    },
    UpdateElement {
        id: EntityId,
        name: Option<String>,
        members: Option<Vec<EntityId>>,
        coalesce: bool,
    },
    DeleteElement {
        id: EntityId,
    },
    // -- Instance -----------------------------------------------------
    CreateInstance {
        element: EntityId,
        transform: [f64; 12],
    },
    UpdateInstance {
        id: EntityId,
        transform: Option<[f64; 12]>,
        element: Option<EntityId>,
        coalesce: bool,
    },
    DeleteInstance {
        id: EntityId,
    },
    // -- Selection ------------------------------------------------------
    CreateSelection {
        predicate: PredicateAst,
        scope: SelectionScope,
        frozen: bool,
    },
    UpdateSelection {
        id: EntityId,
        predicate: Option<PredicateAst>,
        scope: Option<SelectionScope>,
        frozen: Option<bool>,
        coalesce: bool,
    },
    DeleteSelection {
        id: EntityId,
    },
    // -- Composites (docs/ARCHITECTURE.md §4.2) --------------------------
    /// Vertical cylinder at `center`: expands to control points, circle,
    /// edge, face, line path, and extrusion — one undo step.
    CreateCylinder {
        center: [f64; 3],
        radius: f64,
        height: f64,
    },
    /// Update a cylinder created by `CreateCylinder`, addressed by its
    /// extrusion id. Rejected (`InvalidCommand`) if the subgraph no longer
    /// has the cylinder shape (e.g. the path was rewired to a spline).
    UpdateCylinder {
        extrusion: EntityId,
        center: Option<[f64; 3]>,
        radius: Option<f64>,
        height: Option<f64>,
        coalesce: bool,
    },
    /// Delete the whole cylinder subgraph leaf-first as one transaction.
    DeleteCylinder {
        extrusion: EntityId,
    },
}

/// Result of a successful command.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommandOutput {
    /// Ids created by this command, in creation order. For composites this
    /// lists every constituent entity.
    pub created_ids: Vec<EntityId>,
}

impl Command {
    /// UI/history label for the undo step ("Undo Create Cylinder").
    pub fn label(&self) -> &'static str {
        match self {
            Command::CreateControlPoint { .. } => "CreateControlPoint",
            Command::UpdateControlPoint { .. } => "UpdateControlPoint",
            Command::DeleteControlPoint { .. } => "DeleteControlPoint",
            Command::CreatePlane { .. } => "CreatePlane",
            Command::UpdatePlane { .. } => "UpdatePlane",
            Command::DeletePlane { .. } => "DeletePlane",
            Command::CreateCircle { .. } => "CreateCircle",
            Command::UpdateCircle { .. } => "UpdateCircle",
            Command::DeleteCircle { .. } => "DeleteCircle",
            Command::CreateLine { .. } => "CreateLine",
            Command::UpdateLine { .. } => "UpdateLine",
            Command::DeleteLine { .. } => "DeleteLine",
            Command::CreateSpline { .. } => "CreateSpline",
            Command::UpdateSpline { .. } => "UpdateSpline",
            Command::DeleteSpline { .. } => "DeleteSpline",
            Command::CreateEdge { .. } => "CreateEdge",
            Command::UpdateEdge { .. } => "UpdateEdge",
            Command::DeleteEdge { .. } => "DeleteEdge",
            Command::CreateWire { .. } => "CreateWire",
            Command::UpdateWire { .. } => "UpdateWire",
            Command::DeleteWire { .. } => "DeleteWire",
            Command::CreateFace { .. } => "CreateFace",
            Command::UpdateFace { .. } => "UpdateFace",
            Command::UpdateFaceMaterial { .. } => "UpdateFaceMaterial",
            Command::DeleteFace { .. } => "DeleteFace",
            Command::CreateSolid { .. } => "CreateSolid",
            Command::UpdateSolid { .. } => "UpdateSolid",
            Command::DeleteSolid { .. } => "DeleteSolid",
            Command::CreateMaterial { .. } => "CreateMaterial",
            Command::UpdateMaterial { .. } => "UpdateMaterial",
            Command::DeleteMaterial { .. } => "DeleteMaterial",
            Command::CreateExtrusion { .. } => "CreateExtrusion",
            Command::UpdateExtrusion { .. } => "UpdateExtrusion",
            Command::DeleteExtrusion { .. } => "DeleteExtrusion",
            Command::CreateChamfer { .. } => "CreateChamfer",
            Command::UpdateChamfer { .. } => "UpdateChamfer",
            Command::DeleteChamfer { .. } => "DeleteChamfer",
            Command::CreateSectionBox { .. } => "CreateSectionBox",
            Command::UpdateSectionBox { .. } => "UpdateSectionBox",
            Command::DeleteSectionBox { .. } => "DeleteSectionBox",
            Command::CreateElement { .. } => "CreateElement",
            Command::UpdateElement { .. } => "UpdateElement",
            Command::DeleteElement { .. } => "DeleteElement",
            Command::CreateInstance { .. } => "CreateInstance",
            Command::UpdateInstance { .. } => "UpdateInstance",
            Command::DeleteInstance { .. } => "DeleteInstance",
            Command::CreateSelection { .. } => "CreateSelection",
            Command::UpdateSelection { .. } => "UpdateSelection",
            Command::DeleteSelection { .. } => "DeleteSelection",
            Command::CreateCylinder { .. } => "CreateCylinder",
            Command::UpdateCylinder { .. } => "UpdateCylinder",
            Command::DeleteCylinder { .. } => "DeleteCylinder",
        }
    }

    /// Coalescing key (docs/ARCHITECTURE.md §4.2): `Some` only when the
    /// command is an update with `coalesce: true`. Two consecutive
    /// committed groups with equal keys merge into one undo step.
    pub fn coalesce_key(&self) -> Option<(&'static str, EntityId)> {
        let (flag, id) = match self {
            Command::UpdateControlPoint { id, coalesce, .. }
            | Command::UpdatePlane { id, coalesce, .. }
            | Command::UpdateCircle { id, coalesce, .. }
            | Command::UpdateLine { id, coalesce, .. }
            | Command::UpdateSpline { id, coalesce, .. }
            | Command::UpdateEdge { id, coalesce, .. }
            | Command::UpdateWire { id, coalesce, .. }
            | Command::UpdateFace { id, coalesce, .. }
            | Command::UpdateSolid { id, coalesce, .. }
            | Command::UpdateMaterial { id, coalesce, .. }
            | Command::UpdateExtrusion { id, coalesce, .. }
            | Command::UpdateChamfer { id, coalesce, .. }
            | Command::UpdateSectionBox { id, coalesce, .. }
            | Command::UpdateElement { id, coalesce, .. }
            | Command::UpdateInstance { id, coalesce, .. }
            | Command::UpdateSelection { id, coalesce, .. } => (*coalesce, *id),
            Command::UpdateCylinder {
                extrusion, coalesce, ..
            } => (*coalesce, *extrusion),
            _ => return None,
        };
        flag.then_some((self.label(), id))
    }
}

// ---------------------------------------------------------------------
// Compilation: command -> deltas, applied speculatively via the Document.
// ---------------------------------------------------------------------

/// Read helper: the id in a single (`One`) slot, or `InvalidCommand` if
/// the slot is empty/multi (used by composites that require a specific
/// subgraph shape).
fn single_ref(record: &EntityRecord, slot_idx: usize) -> Result<EntityId, VimStatus> {
    match record.inputs.get(slot_idx) {
        Some(SlotValue::One(Some(id))) => Ok(*id),
        _ => Err(VimStatus::InvalidCommand),
    }
}

/// Read helper: the ids in a multi (`Many`) slot.
fn many_refs(record: &EntityRecord, slot_idx: usize) -> Result<Vec<EntityId>, VimStatus> {
    match record.inputs.get(slot_idx) {
        Some(SlotValue::Many(ids)) => Ok(ids.clone()),
        _ => Err(VimStatus::InvalidCommand),
    }
}

/// Execution context: applies deltas through the document while tracking
/// them for rollback, and offers the small vocabulary the per-command
/// compilation code is written in.
struct Ctx<'a> {
    doc: &'a mut Document,
    applied: &'a mut Vec<Delta>,
    created: Vec<EntityId>,
}

impl<'a> Ctx<'a> {
    /// Apply one delta speculatively (tracked for rollback).
    fn apply(&mut self, delta: Delta) -> Result<(), VimStatus> {
        self.doc.apply_tracked(delta, self.applied)
    }

    /// Allocate a fresh id and insert a new record.
    fn create(
        &mut self,
        params: Params,
        inputs: Vec<SlotValue>,
    ) -> Result<EntityId, VimStatus> {
        let id = self.doc.alloc_entity_id();
        let record = EntityRecord { id, params, inputs };
        self.apply(Delta::Insert { id, record })?;
        self.created.push(id);
        Ok(id)
    }

    /// Cloned record for `id`, or `EntityNotFound`.
    fn record(&self, id: EntityId) -> Result<EntityRecord, VimStatus> {
        self.doc
            .graph_ref()
            .get(id)
            .cloned()
            .ok_or(VimStatus::EntityNotFound)
    }

    /// Cloned record with the expected kind, or `WrongEntityKind`.
    fn expect_kind(
        &self,
        id: EntityId,
        kind: EntityKind,
    ) -> Result<EntityRecord, VimStatus> {
        let record = self.record(id)?;
        if record.kind() != kind {
            return Err(VimStatus::WrongEntityKind);
        }
        Ok(record)
    }

    /// Emit a `SetParams` delta (skipped when nothing changes, so no-op
    /// updates do not pollute the undo stack).
    fn set_params(&mut self, id: EntityId, new: Params) -> Result<(), VimStatus> {
        let old = self.record(id)?.params;
        if old == new {
            return Ok(());
        }
        self.apply(Delta::SetParams { id, old, new })
    }

    /// Emit a `Rewire` delta replacing a whole slot value (skipped when
    /// unchanged).
    fn rewire(
        &mut self,
        id: EntityId,
        slot_idx: usize,
        new: SlotValue,
    ) -> Result<(), VimStatus> {
        let record = self.record(id)?;
        let old = record
            .inputs
            .get(slot_idx)
            .cloned()
            .ok_or(VimStatus::SlotIndexOutOfRange)?;
        if old == new {
            return Ok(());
        }
        self.apply(Delta::Rewire {
            id,
            slot: slot_idx,
            old,
            new,
        })
    }

    /// Delete an entity of the expected kind. Reject-if-dependents (and
    /// everything else) is enforced by the `Remove` delta itself.
    fn delete(&mut self, id: EntityId, kind: EntityKind) -> Result<(), VimStatus> {
        let record = self.expect_kind(id, kind)?;
        self.apply(Delta::Remove { id, record })
    }
}

/// Compile `command` to deltas and apply them speculatively through
/// `doc`, tracking every applied delta in `applied` so the caller
/// (`Document::submit`) can roll back on failure.
pub(crate) fn execute(
    doc: &mut Document,
    command: &Command,
    applied: &mut Vec<Delta>,
) -> Result<CommandOutput, VimStatus> {
    let mut ctx = Ctx {
        doc,
        applied,
        created: Vec::new(),
    };
    run(&mut ctx, command)?;
    Ok(CommandOutput {
        created_ids: ctx.created,
    })
}

#[allow(clippy::too_many_lines)] // One arm per command; splitting would obscure the 1:1 mapping.
fn run(ctx: &mut Ctx<'_>, command: &Command) -> Result<(), VimStatus> {
    match command {
        // -- ControlPoint ------------------------------------------------
        Command::CreateControlPoint { position } => {
            ctx.create(Params::ControlPoint { position: *position }, vec![])?;
            Ok(())
        }
        Command::UpdateControlPoint { id, position, .. } => {
            ctx.expect_kind(*id, EntityKind::ControlPoint)?;
            ctx.set_params(*id, Params::ControlPoint { position: *position })
        }
        Command::DeleteControlPoint { id } => ctx.delete(*id, EntityKind::ControlPoint),

        // -- Plane ---------------------------------------------------------
        Command::CreatePlane { origin, normal } => {
            ctx.create(
                Params::Plane {
                    origin: *origin,
                    normal: *normal,
                },
                vec![],
            )?;
            Ok(())
        }
        Command::UpdatePlane {
            id, origin, normal, ..
        } => {
            let record = ctx.expect_kind(*id, EntityKind::Plane)?;
            let (old_origin, old_normal) = match record.params {
                Params::Plane { origin, normal } => (origin, normal),
                _ => return Err(VimStatus::ParamsKindMismatch),
            };
            ctx.set_params(
                *id,
                Params::Plane {
                    origin: origin.unwrap_or(old_origin),
                    normal: normal.unwrap_or(old_normal),
                },
            )
        }
        Command::DeletePlane { id } => ctx.delete(*id, EntityKind::Plane),

        // -- Circle ----------------------------------------------------
        Command::CreateCircle {
            center,
            plane,
            radius,
        } => {
            ctx.create(
                Params::Circle { radius: *radius },
                vec![SlotValue::One(Some(*center)), SlotValue::One(*plane)],
            )?;
            Ok(())
        }
        Command::UpdateCircle {
            id,
            radius,
            center,
            plane,
            ..
        } => {
            ctx.expect_kind(*id, EntityKind::Circle)?;
            if let Some(radius) = radius {
                ctx.set_params(*id, Params::Circle { radius: *radius })?;
            }
            if let Some(center) = center {
                ctx.rewire(*id, slot::CIRCLE_CENTER, SlotValue::One(Some(*center)))?;
            }
            if let Some(plane) = plane {
                ctx.rewire(*id, slot::CIRCLE_PLANE, SlotValue::One(*plane))?;
            }
            Ok(())
        }
        Command::DeleteCircle { id } => ctx.delete(*id, EntityKind::Circle),

        // -- Line -------------------------------------------------------
        Command::CreateLine { start, end } => {
            ctx.create(
                Params::Line,
                vec![SlotValue::One(Some(*start)), SlotValue::One(Some(*end))],
            )?;
            Ok(())
        }
        Command::UpdateLine { id, start, end, .. } => {
            ctx.expect_kind(*id, EntityKind::Line)?;
            if let Some(start) = start {
                ctx.rewire(*id, slot::LINE_START, SlotValue::One(Some(*start)))?;
            }
            if let Some(end) = end {
                ctx.rewire(*id, slot::LINE_END, SlotValue::One(Some(*end)))?;
            }
            Ok(())
        }
        Command::DeleteLine { id } => ctx.delete(*id, EntityKind::Line),

        // -- Spline ---------------------------------------------------
        Command::CreateSpline {
            control_points,
            degree,
            knots,
        } => {
            ctx.create(
                Params::Spline {
                    degree: *degree,
                    knots: knots.clone(),
                },
                vec![SlotValue::Many(control_points.clone())],
            )?;
            Ok(())
        }
        Command::UpdateSpline {
            id,
            control_points,
            degree,
            knots,
            ..
        } => {
            let record = ctx.expect_kind(*id, EntityKind::Spline)?;
            let (old_degree, old_knots) = match record.params {
                Params::Spline { degree, knots } => (degree, knots),
                _ => return Err(VimStatus::ParamsKindMismatch),
            };
            ctx.set_params(
                *id,
                Params::Spline {
                    degree: degree.unwrap_or(old_degree),
                    knots: knots.clone().unwrap_or(old_knots),
                },
            )?;
            if let Some(cps) = control_points {
                ctx.rewire(*id, slot::SPLINE_CONTROL_POINTS, SlotValue::Many(cps.clone()))?;
            }
            Ok(())
        }
        Command::DeleteSpline { id } => ctx.delete(*id, EntityKind::Spline),

        // -- Edge -----------------------------------------------------------
        Command::CreateEdge { curve } => {
            ctx.create(Params::Edge, vec![SlotValue::One(Some(*curve))])?;
            Ok(())
        }
        Command::UpdateEdge { id, curve, .. } => {
            ctx.expect_kind(*id, EntityKind::Edge)?;
            ctx.rewire(*id, slot::EDGE_CURVE, SlotValue::One(Some(*curve)))
        }
        Command::DeleteEdge { id } => ctx.delete(*id, EntityKind::Edge),

        // -- Wire -----------------------------------------------------------
        Command::CreateWire { edges } => {
            ctx.create(Params::Wire, vec![SlotValue::Many(edges.clone())])?;
            Ok(())
        }
        Command::UpdateWire { id, edges, .. } => {
            ctx.expect_kind(*id, EntityKind::Wire)?;
            ctx.rewire(*id, slot::WIRE_EDGES, SlotValue::Many(edges.clone()))
        }
        Command::DeleteWire { id } => ctx.delete(*id, EntityKind::Wire),

        // -- Face -----------------------------------------------------------
        Command::CreateFace {
            outer,
            holes,
            plane,
        } => {
            ctx.create(
                Params::Face,
                vec![
                    SlotValue::One(Some(*outer)),
                    SlotValue::Many(holes.clone()),
                    SlotValue::One(None),
                    SlotValue::One(*plane),
                ],
            )?;
            Ok(())
        }
        Command::UpdateFace {
            id,
            outer,
            holes,
            plane,
            ..
        } => {
            ctx.expect_kind(*id, EntityKind::Face)?;
            if let Some(outer) = outer {
                ctx.rewire(*id, slot::FACE_OUTER, SlotValue::One(Some(*outer)))?;
            }
            if let Some(holes) = holes {
                ctx.rewire(*id, slot::FACE_HOLES, SlotValue::Many(holes.clone()))?;
            }
            if let Some(plane) = plane {
                ctx.rewire(*id, slot::FACE_PLANE, SlotValue::One(*plane))?;
            }
            Ok(())
        }
        Command::UpdateFaceMaterial { face, material } => {
            ctx.expect_kind(*face, EntityKind::Face)?;
            ctx.rewire(*face, slot::FACE_MATERIAL, SlotValue::One(*material))
        }
        Command::DeleteFace { id } => ctx.delete(*id, EntityKind::Face),

        // -- Solid -----------------------------------------------------
        Command::CreateSolid { faces } => {
            ctx.create(Params::Solid, vec![SlotValue::Many(faces.clone())])?;
            Ok(())
        }
        Command::UpdateSolid { id, faces, .. } => {
            ctx.expect_kind(*id, EntityKind::Solid)?;
            ctx.rewire(*id, slot::SOLID_FACES, SlotValue::Many(faces.clone()))
        }
        Command::DeleteSolid { id } => ctx.delete(*id, EntityKind::Solid),

        // -- Material -----------------------------------------------
        Command::CreateMaterial {
            name,
            color,
            roughness,
        } => {
            ctx.create(
                Params::Material {
                    name: name.clone(),
                    color: *color,
                    roughness: *roughness,
                },
                vec![],
            )?;
            Ok(())
        }
        Command::UpdateMaterial {
            id,
            name,
            color,
            roughness,
            ..
        } => {
            let record = ctx.expect_kind(*id, EntityKind::Material)?;
            let (old_name, old_color, old_roughness) = match record.params {
                Params::Material {
                    name,
                    color,
                    roughness,
                } => (name, color, roughness),
                _ => return Err(VimStatus::ParamsKindMismatch),
            };
            ctx.set_params(
                *id,
                Params::Material {
                    name: name.clone().unwrap_or(old_name),
                    color: color.unwrap_or(old_color),
                    roughness: roughness.unwrap_or(old_roughness),
                },
            )
        }
        Command::DeleteMaterial { id } => ctx.delete(*id, EntityKind::Material),

        // -- Extrusion --------------------------------------------------
        Command::CreateExtrusion { profile, path } => {
            ctx.create(
                Params::Extrusion,
                vec![SlotValue::One(Some(*profile)), SlotValue::One(Some(*path))],
            )?;
            Ok(())
        }
        Command::UpdateExtrusion {
            id, profile, path, ..
        } => {
            ctx.expect_kind(*id, EntityKind::Extrusion)?;
            if let Some(profile) = profile {
                ctx.rewire(*id, slot::EXTRUSION_PROFILE, SlotValue::One(Some(*profile)))?;
            }
            if let Some(path) = path {
                ctx.rewire(*id, slot::EXTRUSION_PATH, SlotValue::One(Some(*path)))?;
            }
            Ok(())
        }
        Command::DeleteExtrusion { id } => ctx.delete(*id, EntityKind::Extrusion),

        // -- Chamfer -------------------------------------------------
        Command::CreateChamfer { distance, edges } => {
            ctx.create(
                Params::Chamfer {
                    distance: *distance,
                },
                vec![SlotValue::Many(edges.clone())],
            )?;
            Ok(())
        }
        Command::UpdateChamfer {
            id,
            distance,
            edges,
            ..
        } => {
            ctx.expect_kind(*id, EntityKind::Chamfer)?;
            if let Some(distance) = distance {
                ctx.set_params(
                    *id,
                    Params::Chamfer {
                        distance: *distance,
                    },
                )?;
            }
            if let Some(edges) = edges {
                ctx.rewire(*id, slot::CHAMFER_EDGES, SlotValue::Many(edges.clone()))?;
            }
            Ok(())
        }
        Command::DeleteChamfer { id } => ctx.delete(*id, EntityKind::Chamfer),

        // -- SectionBox ---------------------------------------------------
        Command::CreateSectionBox { min, max } => {
            ctx.create(
                Params::SectionBox {
                    min: *min,
                    max: *max,
                },
                vec![],
            )?;
            Ok(())
        }
        Command::UpdateSectionBox { id, min, max, .. } => {
            let record = ctx.expect_kind(*id, EntityKind::SectionBox)?;
            let (old_min, old_max) = match record.params {
                Params::SectionBox { min, max } => (min, max),
                _ => return Err(VimStatus::ParamsKindMismatch),
            };
            ctx.set_params(
                *id,
                Params::SectionBox {
                    min: min.unwrap_or(old_min),
                    max: max.unwrap_or(old_max),
                },
            )
        }
        Command::DeleteSectionBox { id } => ctx.delete(*id, EntityKind::SectionBox),

        // -- Element ------------------------------------------------------
        Command::CreateElement { name, members } => {
            ctx.create(
                Params::Element { name: name.clone() },
                vec![SlotValue::Many(members.clone())],
            )?;
            Ok(())
        }
        Command::UpdateElement {
            id, name, members, ..
        } => {
            let record = ctx.expect_kind(*id, EntityKind::Element)?;
            let old_name = match record.params {
                Params::Element { name } => name,
                _ => return Err(VimStatus::ParamsKindMismatch),
            };
            ctx.set_params(
                *id,
                Params::Element {
                    name: name.clone().unwrap_or(old_name),
                },
            )?;
            if let Some(members) = members {
                ctx.rewire(*id, slot::ELEMENT_MEMBERS, SlotValue::Many(members.clone()))?;
            }
            Ok(())
        }
        Command::DeleteElement { id } => ctx.delete(*id, EntityKind::Element),

        // -- Instance ----------------------------------------------------
        Command::CreateInstance { element, transform } => {
            ctx.create(
                Params::Instance {
                    transform: *transform,
                },
                vec![SlotValue::One(Some(*element))],
            )?;
            Ok(())
        }
        Command::UpdateInstance {
            id,
            transform,
            element,
            ..
        } => {
            ctx.expect_kind(*id, EntityKind::Instance)?;
            if let Some(transform) = transform {
                ctx.set_params(
                    *id,
                    Params::Instance {
                        transform: *transform,
                    },
                )?;
            }
            if let Some(element) = element {
                ctx.rewire(*id, slot::INSTANCE_ELEMENT, SlotValue::One(Some(*element)))?;
            }
            Ok(())
        }
        Command::DeleteInstance { id } => ctx.delete(*id, EntityKind::Instance),

        // -- Selection --------------------------------------------------
        Command::CreateSelection {
            predicate,
            scope,
            frozen,
        } => {
            // Scope ids live in params (docs/ARCHITECTURE.md §3.5), but
            // they still get a structural existence check at creation.
            validate_scope(ctx, scope)?;
            ctx.create(
                Params::Selection {
                    predicate: predicate.clone(),
                    scope: scope.clone(),
                    frozen: *frozen,
                },
                vec![],
            )?;
            Ok(())
        }
        Command::UpdateSelection {
            id,
            predicate,
            scope,
            frozen,
            ..
        } => {
            let record = ctx.expect_kind(*id, EntityKind::Selection)?;
            let (old_predicate, old_scope, old_frozen) = match record.params {
                Params::Selection {
                    predicate,
                    scope,
                    frozen,
                } => (predicate, scope, frozen),
                _ => return Err(VimStatus::ParamsKindMismatch),
            };
            if let Some(scope) = scope {
                validate_scope(ctx, scope)?;
            }
            ctx.set_params(
                *id,
                Params::Selection {
                    predicate: predicate.clone().unwrap_or(old_predicate),
                    scope: scope.clone().unwrap_or(old_scope),
                    frozen: frozen.unwrap_or(old_frozen),
                },
            )
        }
        Command::DeleteSelection { id } => ctx.delete(*id, EntityKind::Selection),

        // -- Composites -----------------------------------------------------
        Command::CreateCylinder {
            center,
            radius,
            height,
        } => create_cylinder(ctx, *center, *radius, *height),
        Command::UpdateCylinder {
            extrusion,
            center,
            radius,
            height,
            ..
        } => update_cylinder(ctx, *extrusion, *center, *radius, *height),
        Command::DeleteCylinder { extrusion } => delete_cylinder(ctx, *extrusion),
    }
}

/// Existence check for selection-scope ids (kind constraints are the
/// predicate's business, not the scope's).
fn validate_scope(ctx: &Ctx<'_>, scope: &SelectionScope) -> Result<(), VimStatus> {
    match scope {
        SelectionScope::Entities(ids) => {
            for id in ids {
                ctx.record(*id)?;
            }
            Ok(())
        }
        SelectionScope::Element(id) => {
            ctx.expect_kind(*id, EntityKind::Element)?;
            Ok(())
        }
        SelectionScope::Global { .. } => Ok(()),
    }
}

// ---------------------------------------------------------------------
// Cylinder composite (docs/ARCHITECTURE.md §4.2).
//
// Structure (8 entities; the center control point is shared by the circle
// and the path line's start):
//
//   center_cp --> circle --> edge --> wire --> face --+
//       |                                             +--> extrusion
//       +--------> line <-- top_cp -------------------+
// ---------------------------------------------------------------------

/// The resolved wiring of a cylinder subgraph, addressed by extrusion id.
struct CylinderShape {
    extrusion: EntityId,
    face: EntityId,
    wire: EntityId,
    edge: EntityId,
    circle: EntityId,
    line: EntityId,
    center_cp: EntityId,
    top_cp: EntityId,
    center: [f64; 3],
    height: f64,
}

fn create_cylinder(
    ctx: &mut Ctx<'_>,
    center: [f64; 3],
    radius: f64,
    height: f64,
) -> Result<(), VimStatus> {
    let [cx, cy, cz] = center;
    let center_cp = ctx.create(Params::ControlPoint { position: center }, vec![])?;
    let top_cp = ctx.create(
        Params::ControlPoint {
            position: [cx, cy, cz + height],
        },
        vec![],
    )?;
    let circle = ctx.create(
        Params::Circle { radius },
        vec![SlotValue::One(Some(center_cp)), SlotValue::One(None)],
    )?;
    let edge = ctx.create(Params::Edge, vec![SlotValue::One(Some(circle))])?;
    let wire = ctx.create(Params::Wire, vec![SlotValue::Many(vec![edge])])?;
    let face = ctx.create(
        Params::Face,
        vec![
            SlotValue::One(Some(wire)),
            SlotValue::Many(Vec::new()),
            SlotValue::One(None), // material
            SlotValue::One(None), // plane (inferred at evaluation time)
        ],
    )?;
    let line = ctx.create(
        Params::Line,
        vec![SlotValue::One(Some(center_cp)), SlotValue::One(Some(top_cp))],
    )?;
    ctx.create(
        Params::Extrusion,
        vec![SlotValue::One(Some(face)), SlotValue::One(Some(line))],
    )?;
    Ok(())
}

/// Resolve a cylinder's constituent ids from its extrusion, rejecting
/// with `InvalidCommand` if the subgraph is not cylinder-shaped anymore.
fn resolve_cylinder(ctx: &Ctx<'_>, extrusion: EntityId) -> Result<CylinderShape, VimStatus> {
    let ext = ctx.expect_kind(extrusion, EntityKind::Extrusion)?;
    let face_id = single_ref(&ext, slot::EXTRUSION_PROFILE)?;
    let line_id = single_ref(&ext, slot::EXTRUSION_PATH)?;

    let line = ctx.record(line_id)?;
    if line.kind() != EntityKind::Line {
        return Err(VimStatus::InvalidCommand);
    }
    let start_cp_id = single_ref(&line, slot::LINE_START)?;
    let top_cp_id = single_ref(&line, slot::LINE_END)?;

    let face = ctx.record(face_id)?;
    let wire_id = single_ref(&face, slot::FACE_OUTER)?;
    let wire = ctx.record(wire_id)?;
    if wire.kind() != EntityKind::Wire {
        return Err(VimStatus::InvalidCommand);
    }
    let edges = many_refs(&wire, slot::WIRE_EDGES)?;
    let edge_id = match edges.as_slice() {
        [only] => *only,
        _ => return Err(VimStatus::InvalidCommand),
    };
    let edge = ctx.record(edge_id)?;
    let circle_id = single_ref(&edge, slot::EDGE_CURVE)?;
    let circle = ctx.record(circle_id)?;
    if circle.kind() != EntityKind::Circle {
        return Err(VimStatus::InvalidCommand);
    }
    let center_cp_id = single_ref(&circle, slot::CIRCLE_CENTER)?;
    // The cylinder composite shares the center control point as the line
    // start; a subgraph rewired away from that shape is not a cylinder.
    if start_cp_id != center_cp_id {
        return Err(VimStatus::InvalidCommand);
    }

    let center = match ctx.record(center_cp_id)?.params {
        Params::ControlPoint { position } => position,
        _ => return Err(VimStatus::InvalidCommand),
    };
    let top = match ctx.record(top_cp_id)?.params {
        Params::ControlPoint { position } => position,
        _ => return Err(VimStatus::InvalidCommand),
    };
    let ([_, _, cz], [_, _, tz]) = (center, top);
    Ok(CylinderShape {
        extrusion,
        face: face_id,
        wire: wire_id,
        edge: edge_id,
        circle: circle_id,
        line: line_id,
        center_cp: center_cp_id,
        top_cp: top_cp_id,
        center,
        height: tz - cz,
    })
}

fn update_cylinder(
    ctx: &mut Ctx<'_>,
    extrusion: EntityId,
    center: Option<[f64; 3]>,
    radius: Option<f64>,
    height: Option<f64>,
) -> Result<(), VimStatus> {
    let shape = resolve_cylinder(ctx, extrusion)?;
    let new_center = center.unwrap_or(shape.center);
    let new_height = height.unwrap_or(shape.height);
    let [cx, cy, cz] = new_center;
    ctx.set_params(
        shape.center_cp,
        Params::ControlPoint {
            position: new_center,
        },
    )?;
    ctx.set_params(
        shape.top_cp,
        Params::ControlPoint {
            position: [cx, cy, cz + new_height],
        },
    )?;
    if let Some(radius) = radius {
        ctx.set_params(shape.circle, Params::Circle { radius })?;
    }
    Ok(())
}

/// Delete the cylinder subgraph leaf-first as one transaction. Any
/// external dependent on any constituent rejects the whole composite
/// (with full rollback of the already-removed constituents).
fn delete_cylinder(ctx: &mut Ctx<'_>, extrusion: EntityId) -> Result<(), VimStatus> {
    let shape = resolve_cylinder(ctx, extrusion)?;
    ctx.delete(shape.extrusion, EntityKind::Extrusion)?;
    ctx.delete(shape.face, EntityKind::Face)?;
    ctx.delete(shape.wire, EntityKind::Wire)?;
    ctx.delete(shape.edge, EntityKind::Edge)?;
    ctx.delete(shape.circle, EntityKind::Circle)?;
    ctx.delete(shape.line, EntityKind::Line)?;
    ctx.delete(shape.top_cp, EntityKind::ControlPoint)?;
    ctx.delete(shape.center_cp, EntityKind::ControlPoint)?;
    Ok(())
}

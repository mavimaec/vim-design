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
use crate::subref::{EdgeTarget, FaceTarget, SubRef, SubRefSet};

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
    /// Attach (`plane: Some(level)`) or detach (`plane: None`) a control
    /// point to a construction plane, optionally rewriting its stored
    /// coordinates in the same atomic step. When attached, the stored
    /// coordinates are interpreted as (u, v, w) in the plane's evaluated
    /// Frame (docs/AUTHORING.md §3). The world-position-preserving
    /// conversion is explicitly NOT the library's job: the intent layer
    /// computes the equivalent coordinates and passes them as
    /// `position` so the rewire + rewrite land as one undoable command
    /// and the geometry does not jump.
    UpdateControlPointPlane {
        id: EntityId,
        plane: Option<EntityId>,
        /// Simultaneous coordinate rewrite (world coords when detaching,
        /// (u,v,w) frame coords when attaching); `None` keeps the stored
        /// values untouched.
        position: Option<[f64; 3]>,
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
    /// Assign (or clear, with `material: None`) a material on
    /// *generated* faces of an `Extrusion` or `Revolve`, addressed by a
    /// provenance target (docs/ARCHITECTURE.md §§3.4–3.5): a concrete
    /// path (`FaceTarget::One`, e.g. `CapEnd`) or a live query set
    /// (`FaceTarget::Set`, e.g. `SideFaces { HolesOnly }` — membership
    /// re-expands on every evaluation). The counterpart of
    /// `UpdateFaceMaterial` for topology that has no `EntityId`.
    /// Whether the target *resolves* is evaluation-time semantics
    /// (§6.4); structurally any target may be assigned.
    UpdateSubFaceMaterial {
        owner: EntityId,
        target: FaceTarget,
        material: Option<EntityId>,
    },
    // -- Revolve ---------------------------------------------------------
    /// Revolve a profile face about an axis line. `angle_radians` defaults
    /// to 2π (a closed solid of revolution) when `None`.
    CreateRevolve {
        profile: EntityId,
        axis: EntityId,
        angle_radians: Option<f64>,
    },
    UpdateRevolve {
        id: EntityId,
        profile: Option<EntityId>,
        axis: Option<EntityId>,
        angle_radians: Option<f64>,
        coalesce: bool,
    },
    DeleteRevolve {
        id: EntityId,
    },
    // -- Chamfer ----------------------------------------------------
    /// Chamfer edges of `target` (an `Extrusion`/`Revolve`/`Solid`/
    /// `Chamfer`). `sub_edges` addresses concrete generated edges by
    /// provenance (`SharedEdge` SubRefs owned by the target); live query
    /// sets (`SubRefSet`, e.g. `VerticalEdges{OuterOnly}`) are attached
    /// via `UpdateChamferEdgeSets`; `edges` may mix authored `Edge` ids
    /// (matched to coincident solid edges at evaluation time) and
    /// `Selection` ids (docs/ARCHITECTURE.md §§3.4–3.5). Params store
    /// both singles and sets as one canonical `Vec<EdgeTarget>`.
    CreateChamfer {
        target: EntityId,
        distance: f64,
        edges: Vec<EntityId>,
        sub_edges: Vec<SubRef>,
    },
    UpdateChamfer {
        id: EntityId,
        distance: Option<f64>,
        target: Option<EntityId>,
        edges: Option<Vec<EntityId>>,
        /// `Some(v)` replaces the chamfer's concrete (`EdgeTarget::One`)
        /// entries; query-set entries are managed independently by
        /// `UpdateChamferEdgeSets` and are preserved.
        sub_edges: Option<Vec<SubRef>>,
        coalesce: bool,
    },
    /// Replace a chamfer's live query-set edge targets
    /// (`EdgeTarget::Set` entries; docs/ARCHITECTURE.md §3.5). Concrete
    /// `sub_edges` entries are preserved — the two lists are managed
    /// independently and merge into `Params::Chamfer::sub_edges`.
    UpdateChamferEdgeSets {
        id: EntityId,
        edge_sets: Vec<SubRefSet>,
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
    /// Every element is associated with exactly one level (mandatory,
    /// 2026-08-23 — docs/AUTHORING.md §4): element creation is rejected
    /// (`MissingRequiredSlot`) when no Level exists to associate with.
    CreateElement {
        name: String,
        members: Vec<EntityId>,
        level: EntityId,
    },
    UpdateElement {
        id: EntityId,
        name: Option<String>,
        members: Option<Vec<EntityId>>,
        coalesce: bool,
    },
    /// Delete an element. With `sweep_orphans` (the default), the
    /// element's construction-geometry input closure is garbage-collected
    /// afterwards: members whose only consumer was this element — and
    /// transitively their now-unreferenced inputs — are deleted leaf-first
    /// in the same command group (one undo step). Geometry still
    /// referenced elsewhere (shared control points, selection scopes)
    /// survives by the ordinary dependent rules. Only construction kinds
    /// are swept (ControlPoint/Line/Circle/Spline/Edge/Wire/Face/
    /// Extrusion/Revolve/Solid/Chamfer) — never Site, Level, Material,
    /// Selection, Plane (authored reference geometry), Element, or
    /// Instance. `sweep_orphans: false` is the keep-geometry escape hatch
    /// for re-grouping workflows. Deleting an *instance* never sweeps —
    /// the last placement must not destroy the reusable definition.
    DeleteElement {
        id: EntityId,
        #[serde(default = "default_true")]
        sweep_orphans: bool,
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
    // -- Site (docs/AUTHORING.md §1) --------------------------------------
    /// Create the document's singleton geolocation record. Rejected with
    /// `SingletonExists` if a Site already exists (enforced at the delta
    /// gate, so composites cannot smuggle one in). The library provides
    /// no defaults — seeding (e.g. Montreal) is the application's job.
    CreateSite {
        latitude_deg: f64,
        longitude_deg: f64,
        elevation_m: f64,
        true_north_deg: f64,
    },
    UpdateSite {
        id: EntityId,
        latitude_deg: Option<f64>,
        longitude_deg: Option<f64>,
        elevation_m: Option<f64>,
        true_north_deg: Option<f64>,
        coalesce: bool,
    },
    DeleteSite {
        id: EntityId,
    },
    // -- Level (docs/AUTHORING.md §§2–3) ------------------------------------
    CreateLevel {
        name: String,
        elevation_m: f64,
        is_building_story: bool,
        color: [f32; 4],
        extent_m: f64,
    },
    UpdateLevel {
        id: EntityId,
        name: Option<String>,
        elevation_m: Option<f64>,
        is_building_story: Option<bool>,
        color: Option<[f32; 4]>,
        extent_m: Option<f64>,
        coalesce: bool,
    },
    /// Delete a level. With `cascade: false` this is the standard
    /// reject-if-dependents delete. With `cascade: true` it expands at
    /// compile time to a leaf-first delete of the level's full
    /// TRANSITIVE dependent closure (attached control points and
    /// everything downstream of them; associated elements and their
    /// instances) plus the level itself — ONE undo group, so undoing a
    /// confirmed level deletion restores everything atomically
    /// (docs/AUTHORING.md §2). Inputs of deleted entities that are not
    /// themselves dependents of the level (e.g. an associated element's
    /// member solids) are NOT deleted.
    DeleteLevel {
        id: EntityId,
        cascade: bool,
    },
    /// Re-associate an element with a different level (association is
    /// mandatory — there is no dissociated state). Data-only
    /// (docs/AUTHORING.md §4): zero geometric effect — re-evaluation
    /// after this rewire produces byte-identical meshes.
    UpdateElementLevel {
        element: EntityId,
        level: EntityId,
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

/// Serde default for `DeleteElement::sweep_orphans` — sweeping is the
/// normal semantic; opting out is explicit.
fn default_true() -> bool {
    true
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
            Command::UpdateControlPointPlane { .. } => "UpdateControlPointPlane",
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
            Command::UpdateSubFaceMaterial { .. } => "UpdateSubFaceMaterial",
            Command::CreateRevolve { .. } => "CreateRevolve",
            Command::UpdateRevolve { .. } => "UpdateRevolve",
            Command::DeleteRevolve { .. } => "DeleteRevolve",
            Command::CreateChamfer { .. } => "CreateChamfer",
            Command::UpdateChamfer { .. } => "UpdateChamfer",
            Command::UpdateChamferEdgeSets { .. } => "UpdateChamferEdgeSets",
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
            Command::CreateSite { .. } => "CreateSite",
            Command::UpdateSite { .. } => "UpdateSite",
            Command::DeleteSite { .. } => "DeleteSite",
            Command::CreateLevel { .. } => "CreateLevel",
            Command::UpdateLevel { .. } => "UpdateLevel",
            Command::DeleteLevel { .. } => "DeleteLevel",
            Command::UpdateElementLevel { .. } => "UpdateElementLevel",
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
            | Command::UpdateRevolve { id, coalesce, .. }
            | Command::UpdateChamfer { id, coalesce, .. }
            | Command::UpdateChamferEdgeSets { id, coalesce, .. }
            | Command::UpdateSectionBox { id, coalesce, .. }
            | Command::UpdateElement { id, coalesce, .. }
            | Command::UpdateInstance { id, coalesce, .. }
            | Command::UpdateSelection { id, coalesce, .. }
            | Command::UpdateSite { id, coalesce, .. }
            | Command::UpdateLevel { id, coalesce, .. } => (*coalesce, *id),
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
            ctx.create(
                Params::ControlPoint { position: *position },
                vec![SlotValue::One(None)], // plane: unattached (world coords)
            )?;
            Ok(())
        }
        Command::UpdateControlPoint { id, position, .. } => {
            ctx.expect_kind(*id, EntityKind::ControlPoint)?;
            ctx.set_params(*id, Params::ControlPoint { position: *position })
        }
        Command::UpdateControlPointPlane { id, plane, position } => {
            ctx.expect_kind(*id, EntityKind::ControlPoint)?;
            ctx.rewire(*id, slot::CONTROL_POINT_PLANE, SlotValue::One(*plane))?;
            if let Some(position) = position {
                ctx.set_params(*id, Params::ControlPoint { position: *position })?;
            }
            Ok(())
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
                Params::Extrusion {
                    face_materials: Vec::new(),
                },
                vec![
                    SlotValue::One(Some(*profile)),
                    SlotValue::One(Some(*path)),
                    SlotValue::Many(Vec::new()),
                ],
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
        Command::UpdateSubFaceMaterial {
            owner,
            target,
            material,
        } => update_sub_face_material(ctx, *owner, target, *material),

        // -- Revolve -----------------------------------------------------
        Command::CreateRevolve {
            profile,
            axis,
            angle_radians,
        } => {
            ctx.create(
                Params::Revolve {
                    angle_radians: angle_radians.unwrap_or(std::f64::consts::TAU),
                    face_materials: Vec::new(),
                },
                vec![
                    SlotValue::One(Some(*profile)),
                    SlotValue::One(Some(*axis)),
                    SlotValue::Many(Vec::new()),
                ],
            )?;
            Ok(())
        }
        Command::UpdateRevolve {
            id,
            profile,
            axis,
            angle_radians,
            ..
        } => {
            let record = ctx.expect_kind(*id, EntityKind::Revolve)?;
            if let Some(angle_radians) = angle_radians {
                let face_materials = match record.params {
                    Params::Revolve { face_materials, .. } => face_materials,
                    _ => return Err(VimStatus::ParamsKindMismatch),
                };
                ctx.set_params(
                    *id,
                    Params::Revolve {
                        angle_radians: *angle_radians,
                        face_materials,
                    },
                )?;
            }
            if let Some(profile) = profile {
                ctx.rewire(*id, slot::REVOLVE_PROFILE, SlotValue::One(Some(*profile)))?;
            }
            if let Some(axis) = axis {
                ctx.rewire(*id, slot::REVOLVE_AXIS, SlotValue::One(Some(*axis)))?;
            }
            Ok(())
        }
        Command::DeleteRevolve { id } => ctx.delete(*id, EntityKind::Revolve),

        // -- Chamfer -------------------------------------------------
        Command::CreateChamfer {
            target,
            distance,
            edges,
            sub_edges,
        } => {
            let targets: Vec<EdgeTarget> =
                sub_edges.iter().cloned().map(EdgeTarget::One).collect();
            ctx.create(
                Params::Chamfer {
                    distance: *distance,
                    sub_edges: canonical_sub_edges(&targets),
                },
                vec![
                    SlotValue::One(Some(*target)),
                    SlotValue::Many(edges.clone()),
                ],
            )?;
            Ok(())
        }
        Command::UpdateChamfer {
            id,
            distance,
            target,
            edges,
            sub_edges,
            ..
        } => {
            let record = ctx.expect_kind(*id, EntityKind::Chamfer)?;
            if distance.is_some() || sub_edges.is_some() {
                let (old_distance, old_targets) = match record.params {
                    Params::Chamfer {
                        distance,
                        sub_edges,
                    } => (distance, sub_edges),
                    _ => return Err(VimStatus::ParamsKindMismatch),
                };
                let merged = match sub_edges {
                    // Replace the One entries; Set entries are managed by
                    // UpdateChamferEdgeSets and preserved here.
                    Some(singles) => {
                        let mut merged: Vec<EdgeTarget> = old_targets
                            .iter()
                            .filter(|t| matches!(t, EdgeTarget::Set(_)))
                            .cloned()
                            .collect();
                        merged.extend(singles.iter().cloned().map(EdgeTarget::One));
                        canonical_sub_edges(&merged)
                    }
                    None => old_targets,
                };
                ctx.set_params(
                    *id,
                    Params::Chamfer {
                        distance: distance.unwrap_or(old_distance),
                        sub_edges: merged,
                    },
                )?;
            }
            if let Some(target) = target {
                ctx.rewire(*id, slot::CHAMFER_TARGET, SlotValue::One(Some(*target)))?;
            }
            if let Some(edges) = edges {
                ctx.rewire(*id, slot::CHAMFER_EDGES, SlotValue::Many(edges.clone()))?;
            }
            Ok(())
        }
        Command::UpdateChamferEdgeSets { id, edge_sets, .. } => {
            let record = ctx.expect_kind(*id, EntityKind::Chamfer)?;
            let (distance, old_targets) = match record.params {
                Params::Chamfer {
                    distance,
                    sub_edges,
                } => (distance, sub_edges),
                _ => return Err(VimStatus::ParamsKindMismatch),
            };
            // Replace the Set entries; One entries are preserved.
            let mut merged: Vec<EdgeTarget> = old_targets
                .iter()
                .filter(|t| matches!(t, EdgeTarget::One(_)))
                .cloned()
                .collect();
            merged.extend(edge_sets.iter().cloned().map(EdgeTarget::Set));
            ctx.set_params(
                *id,
                Params::Chamfer {
                    distance,
                    sub_edges: canonical_sub_edges(&merged),
                },
            )
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
        Command::CreateElement { name, members, level } => {
            ctx.create(
                Params::Element { name: name.clone() },
                vec![
                    SlotValue::Many(members.clone()),
                    SlotValue::One(Some(*level)), // mandatory association
                ],
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
        Command::DeleteElement { id, sweep_orphans } => {
            let record = ctx.expect_kind(*id, EntityKind::Element)?;
            let candidates = if *sweep_orphans {
                sweepable_input_closure(ctx.doc.graph_ref(), record.referenced())
            } else {
                std::collections::BTreeSet::new()
            };
            // Reject-if-dependents (instances) is enforced by the Remove
            // delta as usual; the sweep runs only after the element
            // itself is gone.
            ctx.apply(Delta::Remove {
                id: *id,
                record,
            })?;
            sweep_orphans_now(ctx, candidates)
        }

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
            // Explicit scope ids are mirrored into the scope slot so they
            // are real graph edges (dependents pin them; 2026-08-23).
            validate_scope(ctx, scope)?;
            ctx.create(
                Params::Selection {
                    predicate: predicate.clone(),
                    scope: scope.clone(),
                    frozen: *frozen,
                },
                vec![SlotValue::Many(scope_mirror_ids(scope))],
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
                ctx.rewire(
                    *id,
                    slot::SELECTION_SCOPE,
                    SlotValue::Many(scope_mirror_ids(scope)),
                )?;
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

        // -- Site ----------------------------------------------------------
        Command::CreateSite {
            latitude_deg,
            longitude_deg,
            elevation_m,
            true_north_deg,
        } => {
            // The singleton rule is enforced by the Insert delta itself
            // (SingletonExists), covering composites and speculative
            // apply with the same check.
            ctx.create(
                Params::Site {
                    latitude_deg: *latitude_deg,
                    longitude_deg: *longitude_deg,
                    elevation_m: *elevation_m,
                    true_north_deg: *true_north_deg,
                },
                vec![],
            )?;
            Ok(())
        }
        Command::UpdateSite {
            id,
            latitude_deg,
            longitude_deg,
            elevation_m,
            true_north_deg,
            ..
        } => {
            let record = ctx.expect_kind(*id, EntityKind::Site)?;
            let (old_lat, old_lon, old_elev, old_north) = match record.params {
                Params::Site {
                    latitude_deg,
                    longitude_deg,
                    elevation_m,
                    true_north_deg,
                } => (latitude_deg, longitude_deg, elevation_m, true_north_deg),
                _ => return Err(VimStatus::ParamsKindMismatch),
            };
            ctx.set_params(
                *id,
                Params::Site {
                    latitude_deg: latitude_deg.unwrap_or(old_lat),
                    longitude_deg: longitude_deg.unwrap_or(old_lon),
                    elevation_m: elevation_m.unwrap_or(old_elev),
                    true_north_deg: true_north_deg.unwrap_or(old_north),
                },
            )
        }
        Command::DeleteSite { id } => ctx.delete(*id, EntityKind::Site),

        // -- Level ---------------------------------------------------------
        Command::CreateLevel {
            name,
            elevation_m,
            is_building_story,
            color,
            extent_m,
        } => {
            ctx.create(
                Params::Level {
                    name: name.clone(),
                    elevation_m: *elevation_m,
                    is_building_story: *is_building_story,
                    color: *color,
                    extent_m: *extent_m,
                },
                vec![],
            )?;
            Ok(())
        }
        Command::UpdateLevel {
            id,
            name,
            elevation_m,
            is_building_story,
            color,
            extent_m,
            ..
        } => {
            let record = ctx.expect_kind(*id, EntityKind::Level)?;
            let params = match record.params {
                Params::Level {
                    name: old_name,
                    elevation_m: old_elev,
                    is_building_story: old_story,
                    color: old_color,
                    extent_m: old_extent,
                } => Params::Level {
                    name: name.clone().unwrap_or(old_name),
                    elevation_m: elevation_m.unwrap_or(old_elev),
                    is_building_story: is_building_story.unwrap_or(old_story),
                    color: color.unwrap_or(old_color),
                    extent_m: extent_m.unwrap_or(old_extent),
                },
                _ => return Err(VimStatus::ParamsKindMismatch),
            };
            ctx.set_params(*id, params)
        }
        Command::DeleteLevel { id, cascade } => {
            if *cascade {
                delete_level_cascade(ctx, *id)
            } else {
                ctx.delete(*id, EntityKind::Level)
            }
        }
        Command::UpdateElementLevel { element, level } => {
            ctx.expect_kind(*element, EntityKind::Element)?;
            ctx.rewire(*element, slot::ELEMENT_LEVEL, SlotValue::One(Some(*level)))
        }

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

/// Canonicalize chamfer edge targets (SharedEdge operand order and
/// Union member order are insensitive — docs/ARCHITECTURE.md §§3.4–3.5)
/// and sort for deterministic params equality/serialization.
fn canonical_sub_edges(sub_edges: &[EdgeTarget]) -> Vec<EdgeTarget> {
    let mut canon: Vec<EdgeTarget> =
        sub_edges.iter().map(EdgeTarget::canonical).collect();
    canon.sort();
    canon.dedup();
    canon
}

/// Compile `UpdateSubFaceMaterial`: update the owner's params assignment
/// list (sorted by target; one material per target) and mirror the
/// material ids into the owner's `face_materials` slot so they are real
/// graph edges (reject-if-dependents, dirty propagation).
fn update_sub_face_material(
    ctx: &mut Ctx<'_>,
    owner: EntityId,
    target: &FaceTarget,
    material: Option<EntityId>,
) -> Result<(), VimStatus> {
    let record = ctx.record(owner)?;
    let target = target.canonical();
    let (params, slot_idx) = match record.params {
        Params::Extrusion { face_materials } => {
            let updated = upsert_assignment(face_materials, target, material);
            (
                Params::Extrusion {
                    face_materials: updated,
                },
                slot::EXTRUSION_FACE_MATERIALS,
            )
        }
        Params::Revolve {
            angle_radians,
            face_materials,
        } => {
            let updated = upsert_assignment(face_materials, target, material);
            (
                Params::Revolve {
                    angle_radians,
                    face_materials: updated,
                },
                slot::REVOLVE_FACE_MATERIALS,
            )
        }
        _ => return Err(VimStatus::WrongEntityKind),
    };
    let mut ids: Vec<EntityId> = match &params {
        Params::Extrusion { face_materials }
        | Params::Revolve { face_materials, .. } => {
            face_materials.iter().map(|(_, id)| *id).collect()
        }
        _ => Vec::new(),
    };
    ids.sort();
    ids.dedup();
    // Rewire first: it validates that the material ids exist with the
    // accepted kind before any params change is applied.
    ctx.rewire(owner, slot_idx, SlotValue::Many(ids))?;
    ctx.set_params(owner, params)
}

/// Replace/insert/remove the assignment for `target`, keeping the list
/// sorted (One-before-Set precedence; deterministic serialization).
fn upsert_assignment(
    mut list: Vec<(FaceTarget, EntityId)>,
    target: FaceTarget,
    material: Option<EntityId>,
) -> Vec<(FaceTarget, EntityId)> {
    list.retain(|(t, _)| *t != target);
    if let Some(material) = material {
        list.push((target, material));
    }
    list.sort();
    list
}

/// The graph-edge mirror of a selection scope: explicit ids for
/// `Entities`/`Element` scopes (sorted, deduplicated), empty for
/// `Global` (the sanctioned implicit dependency, docs/ARCHITECTURE.md
/// §3.5).
fn scope_mirror_ids(scope: &SelectionScope) -> Vec<EntityId> {
    let mut ids = match scope {
        SelectionScope::Entities(ids) => ids.clone(),
        SelectionScope::Element(id) => vec![*id],
        SelectionScope::Global { .. } => Vec::new(),
    };
    ids.sort();
    ids.dedup();
    ids
}

/// Kinds the orphan sweep may collect: the geometry construction chain.
/// Deliberately excluded (flagged decisions, docs/AUTHORING.md §4):
/// `Site`/`Level` (document structure), `Material` (shared assets),
/// `Selection` (queries), `Plane` (authored reference geometry),
/// `Element`/`Instance` (grouping/placement are deleted explicitly,
/// never collected).
fn sweepable(kind: EntityKind) -> bool {
    matches!(
        kind,
        EntityKind::ControlPoint
            | EntityKind::Line
            | EntityKind::Circle
            | EntityKind::Spline
            | EntityKind::Edge
            | EntityKind::Wire
            | EntityKind::Face
            | EntityKind::Extrusion
            | EntityKind::Revolve
            | EntityKind::Solid
            | EntityKind::Chamfer
    )
}

/// Transitive input closure of `roots`, traversing only sweepable kinds
/// (a non-sweepable node is neither collected nor traversed through —
/// e.g. a face's material or a point's level stops the walk).
fn sweepable_input_closure(
    graph: &crate::graph::GraphState,
    roots: impl IntoIterator<Item = EntityId>,
) -> std::collections::BTreeSet<EntityId> {
    let mut closure = std::collections::BTreeSet::new();
    let mut stack: Vec<EntityId> = roots.into_iter().collect();
    while let Some(id) = stack.pop() {
        let Some(record) = graph.get(id) else { continue };
        if !sweepable(record.kind()) {
            continue;
        }
        if closure.insert(id) {
            stack.extend(record.referenced());
        }
    }
    closure
}

/// Scoped reference-counting collection: iteratively delete candidates
/// whose remaining dependent count is zero, leaf-first, until a
/// fixpoint. Survivors (shared inputs, selection-scoped geometry) are
/// left alone by the ordinary reject-if-dependents rules — no special
/// cases. Runs inside the calling command's group: one undo step.
fn sweep_orphans_now(
    ctx: &mut Ctx<'_>,
    mut candidates: std::collections::BTreeSet<EntityId>,
) -> Result<(), VimStatus> {
    loop {
        let deletable: Vec<EntityId> = candidates
            .iter()
            .filter(|id| {
                ctx.doc.graph_ref().contains(**id)
                    && !ctx.doc.graph_ref().has_dependents(**id)
            })
            .copied()
            .collect();
        if deletable.is_empty() {
            return Ok(());
        }
        for id in deletable {
            let record = ctx.record(id)?;
            ctx.apply(Delta::Remove { id, record })?;
            candidates.remove(&id);
        }
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
    let center_cp = ctx.create(
        Params::ControlPoint { position: center },
        vec![SlotValue::One(None)],
    )?;
    let top_cp = ctx.create(
        Params::ControlPoint {
            position: [cx, cy, cz + height],
        },
        vec![SlotValue::One(None)],
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
        Params::Extrusion {
            face_materials: Vec::new(),
        },
        vec![
            SlotValue::One(Some(face)),
            SlotValue::One(Some(line)),
            SlotValue::Many(Vec::new()),
        ],
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
/// Cascade form of DeleteLevel (docs/AUTHORING.md §2): delete the
/// level's full transitive dependent closure leaf-first, then the level
/// itself, as one transaction. Every dependent of a closure member is
/// itself in the closure (the closure is transitive downstream), so the
/// leaf-first sweep always terminates; a non-progressing pass would be a
/// substrate bug and rejects cleanly.
fn delete_level_cascade(ctx: &mut Ctx<'_>, level: EntityId) -> Result<(), VimStatus> {
    ctx.expect_kind(level, EntityKind::Level)?;
    let mut remaining = ctx.doc.graph_ref().dirty_closure([level]);
    // Cascaded element deletions sweep too (2026-08-23): collect every
    // closure element's construction-input closure up front, so a level
    // cascade leaves zero orphaned geometry (association is mandatory,
    // so every element of the level is in the dependent closure).
    let mut sweep_candidates = std::collections::BTreeSet::new();
    for id in &remaining {
        if let Some(record) = ctx.doc.graph_ref().get(*id) {
            if record.kind() == EntityKind::Element {
                sweep_candidates.extend(sweepable_input_closure(
                    ctx.doc.graph_ref(),
                    record.referenced(),
                ));
            }
        }
    }
    while !remaining.is_empty() {
        let deletable: Vec<EntityId> = remaining
            .iter()
            .filter(|id| !ctx.doc.graph_ref().has_dependents(**id))
            .copied()
            .collect();
        if deletable.is_empty() {
            return Err(VimStatus::InvalidCommand);
        }
        for id in deletable {
            let record = ctx.record(id)?;
            ctx.apply(Delta::Remove { id, record })?;
            remaining.remove(&id);
        }
    }
    sweep_orphans_now(ctx, sweep_candidates)
}

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

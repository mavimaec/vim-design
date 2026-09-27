//! The evaluation engine and poll facade (docs/ARCHITECTURE.md §6).
//!
//! One `Engine` per `Document`. `evaluate_pending` drains the document's
//! dirty closure, evaluates it in topological waves (parallel within a
//! wave under the `parallel` feature), tessellates the affected mesh
//! owners, and records a changed-set that `poll_updates` drains as a
//! coalesced delta with tombstones (§6.3).

use std::collections::{BTreeMap, BTreeSet};

use crate::document::Document;
use crate::entity::{EntityKind, EntityRecord, slot};
use crate::id::EntityId;
use crate::kernel::{self, KernelSolid, RawMesh};

use super::evaluate::{EntityEval, Space, compute_spaces, evaluate_waves, topo_waves};
use super::types::{
    EvalDiag, EvalErrorKind, EvalState, Evaluated, InstanceUpdate, Mesh, MeshUpdate,
    QueryResolution, SubRefResolution, Submesh, Updates,
};

#[cfg(feature = "parallel")]
use rayon::prelude::*;

/// A cached mesh for one owner.
#[derive(Debug, Clone)]
struct MeshEntry {
    mesh: Mesh,
    generation: u64,
    /// The level whose local space the mesh is evaluated in (`None` =
    /// world space, identity base transform).
    base_level: Option<EntityId>,
}

/// The evaluation engine: derived-layer cache + poll cursor for one
/// document (the caller owns the `Document`/`Engine` pair; the Rust-level
/// analogue of the FFI handle).
#[derive(Debug, Default)]
pub struct Engine {
    /// Lazily set on the first `evaluate_pending`: a document loaded from
    /// bytes arrives with an empty dirty set, so the first evaluation
    /// seeds every entity.
    initialized: bool,
    results: BTreeMap<EntityId, EntityEval>,
    meshes: BTreeMap<EntityId, MeshEntry>,
    /// Current mesh-owner set (see module docs for the ownership rule).
    owners: BTreeSet<EntityId>,
    instances: BTreeMap<EntityId, ([f64; 12], EntityId)>,
    evaluated_generation: u64,
    // -- changed-set since the last poll (coalesced by id) ---------------
    changed_meshes: BTreeSet<EntityId>,
    removed_meshes: BTreeSet<EntityId>,
    changed_instances: BTreeSet<EntityId>,
    removed_instances: BTreeSet<EntityId>,
    /// Entities whose error state transitioned (set, cleared, or diag
    /// changed); the poll reports the *current* state.
    error_transitions: BTreeSet<EntityId>,
    /// Parametric changed-set: directly-touched entity ids pumped from
    /// the document's commit gate (docs/ARCHITECTURE.md §6.3). Deleted
    /// ids stay in here deliberately — the widget bound to the entity
    /// needs to hear that it went away.
    params_touched: BTreeSet<EntityId>,
    /// Interest filter for `params_changed` (never for meshes/instances/
    /// errors). `None` = report all touched ids.
    params_watch: Option<BTreeSet<EntityId>>,
    /// Translation factoring opt-in (default OFF — the poll then
    /// behaves exactly as before this milestone: world-baked meshes,
    /// identity base transforms). Renderers that compose
    /// `instance ∘ base` enable it via `set_translation_factoring`.
    factoring: bool,
    /// Structural evaluation space per entity (translation factoring —
    /// see the module docs). Cached; recomputed for dirty entities each
    /// round (spaces depend only on wiring, and wiring changes dirty).
    /// Empty (all-world) while factoring is disabled.
    spaces: BTreeMap<EntityId, Space>,
    /// Owners needing a transform-only re-placement at the next poll
    /// (their base level's frame moved; their geometry did not).
    transform_dirty: BTreeSet<EntityId>,
    // -- instrumentation --------------------------------------------------
    eval_counts: BTreeMap<EntityId, u64>,
    tess_counts: BTreeMap<EntityId, u64>,
}

impl Engine {
    pub fn new() -> Self {
        Self::default()
    }

    // -- queries -----------------------------------------------------------

    /// Everything at or below this generation is evaluated and meshed.
    pub fn evaluated_generation(&self) -> u64 {
        self.evaluated_generation
    }

    /// Current evaluation state of an entity (`None` = never evaluated).
    pub fn state(&self, id: EntityId) -> Option<&EvalState> {
        self.results.get(&id).and_then(|e| e.state.as_ref())
    }

    /// The entity's current (possibly stale) evaluated value.
    pub fn value(&self, id: EntityId) -> Option<&Evaluated> {
        self.results.get(&id).and_then(|e| e.value.as_ref())
    }

    /// Current mesh of a mesh owner (element or standalone solid
    /// producer), including stale-retained meshes.
    pub fn mesh(&self, id: EntityId) -> Option<&Mesh> {
        self.meshes.get(&id).map(|entry| &entry.mesh)
    }

    /// How many times `id` has been (re-)evaluated — instrumentation for
    /// incrementality tests and profiling.
    pub fn eval_count(&self, id: EntityId) -> u64 {
        self.eval_counts.get(&id).copied().unwrap_or(0)
    }

    /// How many times `id`'s mesh has been (re-)tessellated.
    pub fn tessellation_count(&self, id: EntityId) -> u64 {
        self.tess_counts.get(&id).copied().unwrap_or(0)
    }

    /// Total evaluations performed since engine creation.
    pub fn total_eval_count(&self) -> u64 {
        self.eval_counts.values().sum()
    }

    /// Register (or clear, with `None`) the interest filter for
    /// [`Updates::params_changed`]: with `Some(watch)`, only watched ids
    /// are reported. The filter applies **only** to `params_changed` —
    /// mesh, instance, and error reporting are never filtered.
    ///
    /// The filter is applied at drain time against the *current* watch
    /// set; accumulation is unconditional. Documented edge case: dirt
    /// accumulated before a watch change is filtered by the NEW set at
    /// the next poll, and every drain clears the whole accumulator — so
    /// narrowing the watch set discards the non-matching ids already
    /// accumulated (they are not retained for a later unwatch).
    pub fn set_params_watch(&mut self, watch: Option<BTreeSet<EntityId>>) {
        self.params_watch = watch;
    }

    /// Opt into translation factoring (see the module docs): qualified
    /// owners then deliver level-LOCAL meshes plus base transforms, and
    /// elevation drags become transform-only polls. OFF by default so
    /// existing renderers that ignore [`MeshUpdate::base_transform`]
    /// keep rendering correctly. Set it once, before the first
    /// `evaluate_pending` on a document (toggling later resets the
    /// space cache; already-delivered meshes are only re-derived when
    /// their entities next change).
    pub fn set_translation_factoring(&mut self, enabled: bool) {
        if self.factoring != enabled {
            self.factoring = enabled;
            self.spaces.clear();
        }
    }

    /// Resolve a provenance-named subelement reference against its
    /// owner's **current** evaluated solid (docs/ARCHITECTURE.md §3.4).
    /// Face paths resolve to faces, `SharedEdge` paths to edges; a
    /// reference that no longer matches (source edge deleted, cap gone
    /// on an angle change) is a typed error — never a silent re-bind.
    pub fn resolve_subref(
        &self,
        subref: &crate::subref::SubRef,
    ) -> Result<SubRefResolution, EvalDiag> {
        let value = self
            .results
            .get(&subref.owner)
            .and_then(|e| e.value.as_ref())
            .ok_or_else(|| {
                EvalDiag::new(
                    EvalErrorKind::UnresolvedSubRef,
                    format!(
                        "owner entity {} has no evaluated geometry",
                        subref.owner.0
                    ),
                )
            })?;
        // A sketch owner is a set of prisms: a path resolves across all
        // of them (one name may cover faces of several prisms).
        let solids: Vec<&KernelSolid> = match value {
            Evaluated::Solid { solid, .. } => vec![solid],
            Evaluated::SolidSet(members) => members.iter().map(|(_, s, _)| s).collect(),
            _ => {
                return Err(EvalDiag::new(
                    EvalErrorKind::UnresolvedSubRef,
                    format!(
                        "owner entity {} did not evaluate to a solid",
                        subref.owner.0
                    ),
                ));
            }
        };
        let (faces, edges) = solids
            .iter()
            .map(|solid| kernel::match_counts(solid, &subref.path))
            .fold((0, 0), |(f, e), (df, de)| (f + df, e + de));
        if subref.path.is_edge() {
            if edges > 0 {
                return Ok(SubRefResolution::Edges(edges));
            }
        } else if faces > 0 {
            return Ok(SubRefResolution::Faces(faces));
        }
        Err(EvalDiag::new(
            EvalErrorKind::UnresolvedSubRef,
            format!(
                "path {:?} resolves to no topology on entity {}",
                subref.path, subref.owner.0
            ),
        ))
    }

    /// Expand a provenance query against its owner's **current**
    /// evaluated solid and report the matched face/edge counts
    /// (docs/ARCHITECTURE.md §3.5). An empty expansion is a valid
    /// `Ok(QueryResolution { faces: 0, edges: 0 })` — membership is
    /// live, so re-resolving after upstream edits reflects topology
    /// that appeared or vanished. Errors only for structural mismatch:
    /// no evaluated geometry, a non-solid owner, or an owner without a
    /// sweep provenance model.
    pub fn resolve_query(
        &self,
        set: &crate::subref::SubRefSet,
    ) -> Result<QueryResolution, EvalDiag> {
        let value = self
            .results
            .get(&set.owner)
            .and_then(|e| e.value.as_ref())
            .ok_or_else(|| {
                EvalDiag::new(
                    EvalErrorKind::UnresolvedSubRef,
                    format!("owner entity {} has no evaluated geometry", set.owner.0),
                )
            })?;
        let Evaluated::Solid { solid, .. } = value else {
            return Err(EvalDiag::new(
                EvalErrorKind::UnresolvedSubRef,
                format!("owner entity {} did not evaluate to a solid", set.owner.0),
            ));
        };
        let expansion = kernel::expand_query(solid, &set.query).map_err(|err| {
            EvalDiag::new(EvalErrorKind::UnresolvedSubRef, err.to_string())
        })?;
        let mut resolution = QueryResolution::default();
        for path in &expansion.face_paths {
            resolution.faces += kernel::match_counts(solid, path).0;
        }
        for path in &expansion.edge_paths {
            resolution.edges += kernel::match_counts(solid, path).1;
        }
        Ok(resolution)
    }

    // -- evaluation ---------------------------------------------------------

    /// Drain the document's dirty closure and bring the derived layer up
    /// to date: topological-wave evaluation (§6.2), tessellation of the
    /// affected mesh owners (§6.3), stale retention on per-entity errors
    /// (§6.4). Synchronous v1 entry point — the wave computation itself
    /// is a pure function of a snapshot, so the async/background wrapper
    /// can be added without restructuring.
    pub fn evaluate_pending(&mut self, doc: &mut Document) {
        // Pump the parametric changed-set (delta targets recorded at the
        // document's commit gate — §6.3) into the poll accumulator. The
        // fresh targets double as the cutoff ROOTS: entities whose own
        // params/wiring changed must re-evaluate; everything else only
        // re-evaluates when an input's result actually changed.
        let roots = doc.take_params_touched();
        self.params_touched.extend(roots.iter().copied());
        let mut dirty = doc.take_dirty();
        if !self.initialized {
            self.initialized = true;
            // Load path / late attach: a loaded document has a clean
            // dirty set but nothing evaluated yet.
            dirty.extend(doc.entities().map(|(id, _)| *id));
        }
        let generation = doc.committed_generation();
        if dirty.is_empty() {
            self.evaluated_generation = generation;
            return;
        }

        // 1. Removals (tombstones): dirty ids no longer in the document.
        let mut alive: BTreeMap<EntityId, EntityRecord> = BTreeMap::new();
        for id in &dirty {
            match doc.entity(*id) {
                Some(record) => {
                    alive.insert(*id, record.clone());
                }
                None => self.remove_entity(*id),
            }
        }

        // 2. Assign evaluation spaces for the batch (structural; cached
        //    spaces of untouched entities stay valid), then evaluate in
        //    topological waves with the early cutoff (pure function of
        //    the snapshot + previous results).
        let waves = topo_waves(&alive);
        if self.factoring {
            let fresh_spaces =
                compute_spaces(doc.graph_ref(), &alive, &waves, &self.spaces);
            self.spaces.extend(fresh_spaces);
        }
        let settings = doc.settings().clone();
        let wave_result = evaluate_waves(
            &alive,
            &waves,
            &roots,
            &self.spaces,
            &settings,
            &self.results,
        );
        let frames_changed = wave_result.frames_changed;
        let changed = wave_result.changed;

        // 3. Merge outcomes: stale retention + error transitions (§6.4).
        for (id, outcome) in wave_result.outcomes {
            *self.eval_counts.entry(id).or_insert(0) += 1;
            let entry = self.results.entry(id).or_default();
            match outcome {
                Ok(value) => {
                    let was_error =
                        matches!(entry.state, Some(EvalState::Error { .. }));
                    entry.value = Some(value);
                    entry.value_generation = generation;
                    entry.state = Some(EvalState::UpToDate { generation });
                    if was_error {
                        self.error_transitions.insert(id);
                    }
                }
                Err(diag) => {
                    let changed = match &entry.state {
                        Some(EvalState::Error { diag: old, .. }) => *old != diag,
                        _ => true,
                    };
                    let stale_generation =
                        entry.value.as_ref().map(|_| entry.value_generation);
                    entry.state = Some(EvalState::Error {
                        diag,
                        stale_generation,
                    });
                    if changed {
                        self.error_transitions.insert(id);
                    }
                }
            }
        }

        // 4. Instance placements.
        for (id, record) in &alive {
            if record.kind() != EntityKind::Instance {
                continue;
            }
            if let Some(Evaluated::Instance { element, transform }) =
                self.results.get(id).and_then(|e| e.value.as_ref())
            {
                self.instances.insert(*id, (*transform, *element));
                self.changed_instances.insert(*id);
                self.removed_instances.remove(id);
            }
        }

        // 5. Mesh ownership + tessellation (owners whose geometry
        //    actually changed; cutoff-skipped owners are untouched).
        self.refresh_meshes(doc, &alive, &changed, generation);

        // 6. Transform-only re-placements: owners riding a level whose
        //    frame moved but whose (level-local) geometry did not.
        if !frames_changed.is_empty() {
            for (owner, entry) in &self.meshes {
                if let Some(level) = entry.base_level
                    && frames_changed.contains(&level)
                    && !self.changed_meshes.contains(owner)
                {
                    self.transform_dirty.insert(*owner);
                }
            }
        }

        self.evaluated_generation = generation;
    }

    /// Drop all derived state for a deleted entity and emit tombstones.
    fn remove_entity(&mut self, id: EntityId) {
        self.results.remove(&id);
        self.eval_counts.remove(&id);
        self.tess_counts.remove(&id);
        self.error_transitions.remove(&id);
        self.owners.remove(&id);
        self.spaces.remove(&id);
        self.transform_dirty.remove(&id);
        if self.meshes.remove(&id).is_some() {
            self.changed_meshes.remove(&id);
            self.removed_meshes.insert(id);
        }
        if self.instances.remove(&id).is_some() {
            self.changed_instances.remove(&id);
            self.removed_instances.insert(id);
        }
    }

    /// Recompute the mesh-owner set and (re-)tessellate the owners whose
    /// geometry or ownership changed.
    ///
    /// **Ownership rule (the poll-facade contract):** every `Element` is
    /// a mesh owner; every solid producer (`Extrusion`, `Revolve`,
    /// `Solid`) that is *not* wired into any element's members slot is an
    /// implicit standalone owner keyed by its own id. Wrapping a solid
    /// producer into an element therefore tombstones its standalone mesh
    /// and delivers it under the element id from then on.
    fn refresh_meshes(
        &mut self,
        doc: &Document,
        alive_dirty: &BTreeMap<EntityId, EntityRecord>,
        changed: &BTreeSet<EntityId>,
        generation: u64,
    ) {
        let _ = alive_dirty;
        // Consumed producers: wired into an element's members slot, or
        // targeted by a chamfer (the chamfer replaces its target as mesh
        // owner — the chamfered solid IS the target's render shape).
        let mut consumed: BTreeSet<EntityId> = BTreeSet::new();
        for (_, record) in doc.entities() {
            match record.kind() {
                EntityKind::Element => {
                    for member in record
                        .inputs
                        .get(slot::ELEMENT_MEMBERS)
                        .map(|s| s.referenced().collect::<Vec<_>>())
                        .unwrap_or_default()
                    {
                        consumed.insert(member);
                    }
                }
                EntityKind::Chamfer => {
                    for target in record
                        .inputs
                        .get(slot::CHAMFER_TARGET)
                        .map(|s| s.referenced().collect::<Vec<_>>())
                        .unwrap_or_default()
                    {
                        consumed.insert(target);
                    }
                }
                _ => {}
            }
        }
        let owners_now: BTreeSet<EntityId> = doc
            .entities()
            .filter(|(id, record)| match record.kind() {
                EntityKind::Element => true,
                EntityKind::Extrusion
                | EntityKind::Revolve
                | EntityKind::Solid
                | EntityKind::Chamfer
                | EntityKind::Sketch
                | EntityKind::Wall => !consumed.contains(id),
                _ => false,
            })
            .map(|(id, _)| *id)
            .collect();

        // Per-producer sub-face material assignments (canonical paths),
        // resolved through chamfer chains: a chamfer renders its target's
        // painted faces (provenance survives the blend — §3.4).
        let assignments = collect_assignments(doc);

        // Owners that ceased to exist as owners: tombstone their meshes.
        let dropped: Vec<EntityId> = self
            .meshes
            .keys()
            .filter(|id| !owners_now.contains(id))
            .copied()
            .collect();
        for id in dropped {
            self.meshes.remove(&id);
            self.changed_meshes.remove(&id);
            self.removed_meshes.insert(id);
        }

        // Candidates: owners whose value actually CHANGED this round
        // (cutoff-skipped owners keep their meshes untouched — that is
        // the early-cutoff/translation-factoring payoff) or that just
        // became owners.
        let candidates: Vec<EntityId> = owners_now
            .iter()
            .filter(|id| changed.contains(id) || !self.owners.contains(*id))
            .copied()
            .collect();
        self.owners = owners_now;

        // Tessellate candidates (parallel on native — §6.2).
        let chordal = doc.settings().chordal_tolerance;
        let results = &self.results;
        let assignments = &assignments;
        let build = |id: &EntityId| -> (EntityId, Option<Result<Mesh, EvalDiag>>) {
            (*id, build_owner_mesh(results, assignments, *id, chordal))
        };
        #[cfg(feature = "parallel")]
        let built: Vec<_> = candidates.par_iter().map(build).collect();
        #[cfg(not(feature = "parallel"))]
        let built: Vec<_> = candidates.iter().map(build).collect();

        for (id, outcome) in built {
            match outcome {
                None => {
                    // No solid value available (upstream error with no
                    // stale geometry): retain whatever mesh exists (§6.4).
                }
                Some(Ok(mesh)) if mesh.indices.is_empty() => {
                    // No material left (a sketch whose voids remove
                    // everything, or with no solid faces): the owner has
                    // no mesh. Tombstone the previous one; not an error.
                    if self.meshes.remove(&id).is_some() {
                        self.changed_meshes.remove(&id);
                        self.removed_meshes.insert(id);
                    }
                    self.transform_dirty.remove(&id);
                }
                Some(Ok(mesh)) => {
                    *self.tess_counts.entry(id).or_insert(0) += 1;
                    let base_level = match self.spaces.get(&id) {
                        Some(Space::Level(level)) => Some(*level),
                        _ => None,
                    };
                    self.meshes.insert(
                        id,
                        MeshEntry {
                            mesh,
                            generation,
                            base_level,
                        },
                    );
                    self.changed_meshes.insert(id);
                    self.removed_meshes.remove(&id);
                    self.transform_dirty.remove(&id);
                }
                Some(Err(diag)) => {
                    // Tessellation failure: per-entity error on the owner;
                    // the previous mesh (if any) is retained.
                    if let Some(entry) = self.results.get_mut(&id) {
                        let changed = match &entry.state {
                            Some(EvalState::Error { diag: old, .. }) => *old != diag,
                            _ => true,
                        };
                        let stale_generation =
                            entry.value.as_ref().map(|_| entry.value_generation);
                        entry.state = Some(EvalState::Error {
                            diag,
                            stale_generation,
                        });
                        if changed {
                            self.error_transitions.insert(id);
                        }
                    }
                }
            }
        }
    }

    /// Current base transform of a mesh entry: identity for world-space
    /// owners, translation to the base level's frame origin otherwise
    /// (read fresh at poll time — coalescing is structural, §6.3).
    fn base_transform_of(&self, entry: &MeshEntry) -> [f64; 12] {
        let Some(level) = entry.base_level else {
            return super::types::IDENTITY_TRANSFORM;
        };
        match self.results.get(&level).and_then(|e| e.value.as_ref()) {
            Some(Evaluated::Frame { origin, .. }) => {
                let [x, y, z] = *origin;
                [
                    1.0, 0.0, 0.0, x, //
                    0.0, 1.0, 0.0, y, //
                    0.0, 0.0, 1.0, z,
                ]
            }
            _ => super::types::IDENTITY_TRANSFORM,
        }
    }

    // -- poll facade (docs/ARCHITECTURE.md §6.3) -----------------------------

    /// Drain the changed-set accumulated since the previous poll:
    /// coalesced latest-state upserts keyed by stable ids, tombstones for
    /// removals, error transitions, and settledness counters. A second
    /// poll without intervening edits returns an empty, settled delta.
    pub fn poll_updates(&mut self, doc: &Document) -> Updates {
        let mut updates = Updates {
            committed_generation: doc.committed_generation(),
            evaluated_generation: self.evaluated_generation,
            pending_count: doc.dirty_set().len(),
            ..Updates::default()
        };

        for id in std::mem::take(&mut self.changed_meshes) {
            if let Some(entry) = self.meshes.get(&id) {
                updates.meshes.push(MeshUpdate {
                    id,
                    generation: entry.generation,
                    mesh: entry.mesh.clone(),
                    base_transform: self.base_transform_of(entry),
                });
            }
        }
        // Transform-only re-placements (geometry unchanged): never
        // overlaps the mesh upserts above.
        for id in std::mem::take(&mut self.transform_dirty) {
            if let Some(entry) = self.meshes.get(&id) {
                updates.base_transforms.push(super::types::BaseTransformUpdate {
                    id,
                    transform: self.base_transform_of(entry),
                });
            }
        }
        // Never tombstone an id that currently has a mesh (delete +
        // recreate between polls coalesces to a plain upsert).
        updates.meshes_removed = std::mem::take(&mut self.removed_meshes)
            .into_iter()
            .filter(|id| !self.meshes.contains_key(id))
            .collect();

        for id in std::mem::take(&mut self.changed_instances) {
            if let Some((transform, element)) = self.instances.get(&id) {
                updates.instances.push(InstanceUpdate {
                    id,
                    element_id: *element,
                    transform: *transform,
                });
            }
        }
        updates.instances_removed = std::mem::take(&mut self.removed_instances)
            .into_iter()
            .filter(|id| !self.instances.contains_key(id))
            .collect();

        for id in std::mem::take(&mut self.error_transitions) {
            match self.results.get(&id).and_then(|e| e.state.as_ref()) {
                Some(EvalState::Error { diag, .. }) => {
                    updates.errors.push((id, diag.clone()));
                }
                Some(EvalState::UpToDate { .. }) => updates.errors_cleared.push(id),
                None => {}
            }
        }

        // Parametric changed-set: drain (single-consumer — the drain
        // clears), filtered by the current watch set. Ids are reported
        // whether or not the entity still exists: a deleted id appears
        // exactly once (its Remove delta targeted it) so the bound
        // widget can react, and never again unless re-touched (undo of
        // the delete re-inserts and re-reports it, alive). BTreeSet
        // drain = sorted + deduplicated.
        let drained = std::mem::take(&mut self.params_touched);
        updates.params_changed = match &self.params_watch {
            None => drained.into_iter().collect(),
            Some(watch) => drained
                .into_iter()
                .filter(|id| watch.contains(id))
                .collect(),
        };

        updates
    }
}

/// Per-producer sub-face material assignments (canonical paths), with a
/// chamfer resolving to its target chain's assignments: paints survive a
/// chamfer because provenance does (docs/ARCHITECTURE.md §3.4).
fn collect_assignments(doc: &Document) -> BTreeMap<EntityId, Assignments> {
    // Direct assignments from Extrusion/Revolve params.
    let mut direct: BTreeMap<EntityId, Assignments> = BTreeMap::new();
    for (id, record) in doc.entities() {
        match &record.params {
            crate::entity::Params::Extrusion { face_materials }
            | crate::entity::Params::Revolve { face_materials, .. }
                if !face_materials.is_empty() =>
            {
                direct.insert(
                    *id,
                    face_materials
                        .iter()
                        .map(|(target, material)| (target.canonical(), *material))
                        .collect(),
                );
            }
            _ => {}
        }
    }
    // Chamfers inherit their (transitive) target's assignments.
    let mut resolved = direct.clone();
    for (id, record) in doc.entities() {
        if record.kind() != EntityKind::Chamfer {
            continue;
        }
        let mut current = *id;
        for _ in 0..64 {
            let Some(target) = doc
                .entity(current)
                .and_then(|r| r.inputs.get(slot::CHAMFER_TARGET))
                .and_then(|s| s.referenced().next())
            else {
                break;
            };
            if let Some(assigns) = direct.get(&target) {
                resolved.insert(*id, assigns.clone());
                break;
            }
            current = target;
        }
    }
    resolved
}

type Assignments = Vec<(crate::subref::FaceTarget, EntityId)>;

/// Build the mesh for one owner from its evaluated value.
///
/// Returns `None` when no (even stale) solid geometry is available —
/// the caller retains the previous mesh. Standalone producers get one
/// submesh per material group (sub-face assignments split the buffer);
/// elements concatenate their members the same way.
fn build_owner_mesh(
    results: &BTreeMap<EntityId, EntityEval>,
    assignments: &BTreeMap<EntityId, Assignments>,
    id: EntityId,
    chordal_tolerance: f64,
) -> Option<Result<Mesh, EvalDiag>> {
    let value = results.get(&id)?.value.as_ref()?;
    let mut mesh = Mesh::default();
    let outcome = match value {
        Evaluated::Solid { solid, material } => append_solid(
            &mut mesh,
            solid,
            *material,
            assignments.get(&id),
            chordal_tolerance,
        ),
        // An empty set (no material) yields an empty mesh, which the
        // caller turns into a tombstone.
        Evaluated::SolidSet(members) => {
            members.iter().try_for_each(|(member, solid, material)| {
                append_solid(
                    &mut mesh,
                    solid,
                    *material,
                    assignments.get(member),
                    chordal_tolerance,
                )
            })
        }
        _ => return None,
    };
    Some(outcome.map(|()| mesh))
}

fn tessellation_diag(err: kernel::KernelError) -> EvalDiag {
    EvalDiag::new(
        match err {
            kernel::KernelError::Panic(_) => EvalErrorKind::InternalPanic,
            _ => EvalErrorKind::Tessellation,
        },
        err.to_string(),
    )
}

fn range_diag(what: &str) -> EvalDiag {
    EvalDiag::new(
        EvalErrorKind::Tessellation,
        format!("merged mesh exceeds u32 {what} range"),
    )
}

/// Tessellate one solid per BREP face, group the faces by material
/// (provenance-target assignments — concrete paths and live query sets,
/// falling back to the solid's inherited material), and append one
/// submesh per non-empty group.
fn append_solid(
    mesh: &mut Mesh,
    solid: &KernelSolid,
    default_material: Option<EntityId>,
    assignments: Option<&Assignments>,
    chordal_tolerance: f64,
) -> Result<(), EvalDiag> {
    let face_meshes: Vec<RawMesh> =
        kernel::tessellate_faces(solid, chordal_tolerance).map_err(tessellation_diag)?;
    let paths = solid.face_paths();

    // Pre-expand assignment targets against THIS solid's current
    // topology: One(path) matches exactly; Set(query) matches whatever
    // its live expansion covers right now (empty on a no-model solid or
    // a non-matching filter — the liveness contract, §3.5). Assignments
    // are sorted One-before-Set, so explicit paints take precedence.
    let matchers: Vec<(Vec<crate::subref::ProvenancePath>, EntityId)> = assignments
        .map(|assigns| {
            assigns
                .iter()
                .map(|(target, material)| {
                    let matched = match target {
                        crate::subref::FaceTarget::One(path) => vec![path.canonical()],
                        crate::subref::FaceTarget::Set(query) => {
                            kernel::expand_query(solid, query)
                                .map(|e| e.face_paths)
                                .unwrap_or_default()
                        }
                    };
                    (matched, *material)
                })
                .collect()
        })
        .unwrap_or_default();

    // Group face indices by their material, deterministically
    // (None-material group first, then ascending material id).
    let mut groups: BTreeMap<Option<EntityId>, Vec<usize>> = BTreeMap::new();
    for index in 0..face_meshes.len() {
        let material = paths
            .get(index)
            .and_then(|p| p.as_ref())
            .and_then(|path| {
                let canonical = path.canonical();
                matchers
                    .iter()
                    .find(|(matched, _)| matched.contains(&canonical))
                    .map(|(_, material)| *material)
            })
            .or(default_material);
        groups.entry(material).or_default().push(index);
    }

    for (material, face_indices) in groups {
        let index_start = u32::try_from(mesh.indices.len())
            .map_err(|_| range_diag("index"))?;
        let mut index_count: u32 = 0;
        for face_index in face_indices {
            let Some(raw) = face_meshes.get(face_index) else {
                continue;
            };
            let base = u32::try_from(mesh.positions.len())
                .map_err(|_| range_diag("vertex"))?;
            let count =
                u32::try_from(raw.indices.len()).map_err(|_| range_diag("index"))?;
            mesh.positions.extend_from_slice(&raw.positions);
            mesh.normals.extend_from_slice(&raw.normals);
            mesh.indices
                .extend(raw.indices.iter().map(|i| i.saturating_add(base)));
            index_count = index_count.saturating_add(count);
        }
        if index_count > 0 {
            mesh.submeshes.push(Submesh {
                material,
                index_start,
                index_count,
            });
        }
    }
    if mesh.indices.is_empty() {
        return Err(EvalDiag::new(
            EvalErrorKind::Tessellation,
            "tessellation produced an empty mesh",
        ));
    }
    Ok(())
}

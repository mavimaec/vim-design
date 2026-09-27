//! The `Document`: the accumulating parametric state of one "VimDesign"
//! object — entities, dependency graph, undo/redo stacks, dirty tracking
//! (docs/ARCHITECTURE.md §§3–4, 6.1, 10).

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::command::{Command, CommandOutput};
use crate::delta::{Delta, apply_delta};
use crate::entity::EntityRecord;
use crate::graph::GraphState;
use crate::id::{EntityId, IdAllocator};
use crate::status::VimStatus;

/// Persisted per-document settings (docs/ARCHITECTURE.md §§6.3, 7).
/// Units: meters and radians.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DocumentSettings {
    /// Kernel point-coincidence / topology tolerance (1 µm).
    pub kernel_tolerance: f64,
    /// Display/merge tolerance (snapping, mesh dedup).
    pub merge_tolerance: f64,
    /// Tessellation chordal deviation tolerance (1 mm default).
    pub chordal_tolerance: f64,
    /// Tessellation angular tolerance (~20° default).
    pub angular_tolerance: f64,
}

impl Default for DocumentSettings {
    fn default() -> Self {
        Self {
            kernel_tolerance: 1e-6,
            merge_tolerance: 1e-5,
            chordal_tolerance: 1e-3,
            angular_tolerance: 20.0_f64.to_radians(),
        }
    }
}

/// One undo step: a committed command's label and its delta list
/// (docs/ARCHITECTURE.md §4.2). Undo applies the deltas inverted in
/// reverse order; redo re-applies them forward.
#[derive(Debug, Clone)]
pub struct CommandGroup {
    /// History label for UI ("Undo CreateCylinder").
    pub label: String,
    /// Coalescing identity: consecutive groups with equal `Some` keys
    /// merge (docs/ARCHITECTURE.md §4.2).
    coalesce_key: Option<(&'static str, EntityId)>,
    /// The applied deltas, in application order.
    pub deltas: Vec<Delta>,
}

/// A VIM Design document: the authoritative parametric layer.
#[derive(Debug)]
pub struct Document {
    settings: DocumentSettings,
    /// Monotonic id source. Deliberately NOT delta-tracked: undoing a
    /// create does not roll the counter back, so ids are never re-issued
    /// within a session — a fresh create after undo can never collide
    /// with ids held by the redo stack or by external callers
    /// (docs/ARCHITECTURE.md §3.1). Undo of a delete still restores the
    /// *original* id because `Remove` deltas carry the full record.
    /// Serialization persists a next-id *derived* from the entity map
    /// (see `serialization::derived_next_id`), so save stays a pure
    /// function of the parametric state.
    ids: IdAllocator,
    graph: GraphState,
    undo_stack: Vec<CommandGroup>,
    redo_stack: Vec<CommandGroup>,
    /// Bumped once per committed change (submit/undo/redo) — the
    /// generation the evaluation layer will chase (docs/ARCHITECTURE.md §6.3).
    committed_generation: u64,
    /// Dirty closure accumulated since the last `take_dirty` — the ids
    /// whose derived geometry the evaluation layer must recompute.
    pending_dirty: BTreeSet<EntityId>,
    /// Directly-touched entity ids (delta *targets*, not the downstream
    /// closure) accumulated since the last `take_params_touched` — the
    /// parametric changed-set feeding the dirty pump
    /// (docs/ARCHITECTURE.md §6.3). Recorded at the commit gate, so
    /// submit, undo, and redo all feed it through one code path and a
    /// rejected command's rolled-back deltas never appear.
    pending_params_touched: BTreeSet<EntityId>,
}

impl Default for Document {
    fn default() -> Self {
        Self::new()
    }
}

impl Document {
    /// Create a new, empty document with default settings.
    pub fn new() -> Self {
        Self {
            settings: DocumentSettings::default(),
            ids: IdAllocator::new(),
            graph: GraphState::new(),
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            committed_generation: 0,
            pending_dirty: BTreeSet::new(),
            pending_params_touched: BTreeSet::new(),
        }
    }

    /// Reassemble a document from persisted parts (load path — see
    /// `serialization`). Undo/redo stacks start empty: they are not
    /// persisted (docs/ARCHITECTURE.md §10).
    pub(crate) fn from_parts(
        settings: DocumentSettings,
        next_id: u64,
        graph: GraphState,
    ) -> Self {
        let mut ids = IdAllocator::with_next(next_id);
        // Defensive: never allow the counter at or below an existing id.
        for (id, _) in graph.iter() {
            ids.ensure_above(*id);
        }
        Self {
            settings,
            ids,
            graph,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            committed_generation: 0,
            pending_dirty: BTreeSet::new(),
            pending_params_touched: BTreeSet::new(),
        }
    }

    // -- Read access -----------------------------------------------------

    pub fn settings(&self) -> &DocumentSettings {
        &self.settings
    }

    /// The entity record for `id`, if it exists.
    pub fn entity(&self, id: EntityId) -> Option<&EntityRecord> {
        self.graph.get(id)
    }

    /// Number of live entities.
    pub fn entity_count(&self) -> usize {
        self.graph.len()
    }

    /// Deterministic (id-ordered) iteration over all entities.
    pub fn entities(&self) -> impl Iterator<Item = (&EntityId, &EntityRecord)> {
        self.graph.iter()
    }

    /// Direct dependents of `id` (the queryable list behind
    /// `VimStatus::HasDependents` — docs/ARCHITECTURE.md §3.2).
    pub fn dependents(&self, id: EntityId) -> Result<Vec<EntityId>, VimStatus> {
        if !self.graph.contains(id) {
            return Err(VimStatus::EntityNotFound);
        }
        Ok(self.graph.dependents(id))
    }

    /// Generation counter, bumped per committed change. (Also the value
    /// behind the FFI's `generation()` placeholder query.)
    pub fn committed_generation(&self) -> u64 {
        self.committed_generation
    }

    /// Back-compat alias used by the FFI seam.
    pub fn generation(&self) -> u64 {
        self.committed_generation
    }

    /// Full structural invariant check (rebuilds the reverse index from
    /// scratch, verifies acyclicity and slot typing). Exposed for tests.
    pub fn debug_validate(&self) -> Result<(), VimStatus> {
        self.graph.debug_validate()
    }

    // -- Dirty tracking (evaluation-layer feed, docs/ARCHITECTURE.md §6.1) --

    /// The pending dirty closure: every entity whose derived geometry is
    /// out of date since the last `take_dirty`.
    pub fn dirty_set(&self) -> &BTreeSet<EntityId> {
        &self.pending_dirty
    }

    /// Drain the pending dirty closure (the evaluation layer consumes
    /// this once per evaluation kickoff).
    pub fn take_dirty(&mut self) -> BTreeSet<EntityId> {
        std::mem::take(&mut self.pending_dirty)
    }

    /// The pending parametric changed-set: entity ids directly touched
    /// by committed deltas (targets only, never the downstream closure)
    /// since the last `take_params_touched`.
    pub fn params_touched(&self) -> &BTreeSet<EntityId> {
        &self.pending_params_touched
    }

    /// Drain the pending parametric changed-set (the evaluation engine
    /// pumps this into its poll accumulator — docs/ARCHITECTURE.md §6.3).
    pub fn take_params_touched(&mut self) -> BTreeSet<EntityId> {
        std::mem::take(&mut self.pending_params_touched)
    }

    // -- Command submission (docs/ARCHITECTURE.md §4) ---------------------

    /// Submit a command. On success the command's deltas are committed as
    /// one undo step (possibly coalesced into the previous step) and the
    /// dirty closure is extended. On failure every speculatively applied
    /// delta has been rolled back in reverse: the document is
    /// byte-identical to its state before the attempt.
    pub fn submit(&mut self, command: Command) -> Result<CommandOutput, VimStatus> {
        let mut applied: Vec<Delta> = Vec::new();
        match crate::command::execute(self, &command, &mut applied) {
            Ok(output) => {
                self.commit(&command, applied);
                Ok(output)
            }
            Err(status) => {
                self.rollback(applied);
                Err(status)
            }
        }
    }

    /// Roll back speculatively applied deltas in reverse order. Each
    /// inverse must apply cleanly (the forward delta just succeeded); a
    /// failure here would be a substrate bug, and the loop presses on to
    /// restore as much state as possible rather than crash.
    fn rollback(&mut self, applied: Vec<Delta>) {
        for delta in applied.iter().rev() {
            // Cannot fail for a just-applied delta; ignore-with-continue
            // is the never-crash fallback (debug_validate in tests would
            // catch the inconsistency).
            let _ = apply_delta(&mut self.graph, &delta.invert());
        }
    }

    /// Commit an applied delta list as an undo step; clears the redo
    /// stack, coalesces when eligible, bumps the generation, and extends
    /// the dirty closure.
    fn commit(&mut self, command: &Command, deltas: Vec<Delta>) {
        self.mark_dirty(&deltas);
        self.committed_generation = self.committed_generation.saturating_add(1);
        // A new command always invalidates the redo stack — even a no-op
        // update (empty delta list) expresses new user intent.
        self.redo_stack.clear();
        if deltas.is_empty() {
            // Nothing changed (e.g. update to identical values): no undo
            // step to record.
            return;
        }
        let key = command.coalesce_key();
        if let Some(key_value) = key
            && let Some(top) = self.undo_stack.last_mut()
            && top.coalesce_key == Some(key_value)
        {
            coalesce_into(&mut top.deltas, deltas);
            return;
        }
        self.undo_stack.push(CommandGroup {
            label: command.label().to_owned(),
            coalesce_key: key,
            deltas,
        });
    }

    /// Extend the dirty closure with every entity touched by `deltas`
    /// plus its downstream transitive closure (docs/ARCHITECTURE.md §6.1),
    /// and the parametric changed-set with the delta *targets* alone.
    /// Removed entities appear as roots (tombstones for the evaluation
    /// layer); they have no downstream by construction (reject-if-dependents).
    ///
    /// This is the single recording gate of the dirty pump (§6.3):
    /// `submit`, `undo`, and `redo` all pass their committed delta lists
    /// through here, so all three feed the same accumulators with no
    /// special-casing, and speculative deltas rolled back by a rejected
    /// command are never recorded.
    fn mark_dirty(&mut self, deltas: &[Delta]) {
        self.pending_params_touched
            .extend(deltas.iter().map(|delta| delta.target()));
        let closure = self
            .graph
            .dirty_closure(deltas.iter().map(|delta| delta.target()));
        self.pending_dirty.extend(closure);
    }

    // -- Undo / redo (docs/ARCHITECTURE.md §4.2) --------------------------

    pub fn can_undo(&self) -> bool {
        !self.undo_stack.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo_stack.is_empty()
    }

    /// Number of undo steps available (coalesced updates count once).
    pub fn undo_depth(&self) -> usize {
        self.undo_stack.len()
    }

    /// Number of redo steps available.
    pub fn redo_depth(&self) -> usize {
        self.redo_stack.len()
    }

    /// Label of the next undo step, if any (for UI history).
    pub fn undo_label(&self) -> Option<&str> {
        self.undo_stack.last().map(|group| group.label.as_str())
    }

    /// Undo the most recent command group: applies its deltas inverted in
    /// reverse order, moves the group to the redo stack.
    pub fn undo(&mut self) -> Result<(), VimStatus> {
        let group = self.undo_stack.pop().ok_or(VimStatus::NothingToUndo)?;
        let inverted: Vec<Delta> = group.deltas.iter().rev().map(Delta::invert).collect();
        match self.apply_all_or_rollback(&inverted) {
            Ok(()) => {
                self.mark_dirty(&inverted);
                self.committed_generation = self.committed_generation.saturating_add(1);
                self.redo_stack.push(group);
                Ok(())
            }
            Err(status) => {
                // Substrate bug guard: restore the group so the stack
                // still matches the (rolled-back) document state.
                self.undo_stack.push(group);
                Err(status)
            }
        }
    }

    /// Redo the most recently undone command group.
    pub fn redo(&mut self) -> Result<(), VimStatus> {
        let group = self.redo_stack.pop().ok_or(VimStatus::NothingToRedo)?;
        match self.apply_all_or_rollback(&group.deltas) {
            Ok(()) => {
                self.mark_dirty(&group.deltas);
                self.committed_generation = self.committed_generation.saturating_add(1);
                self.undo_stack.push(group);
                Ok(())
            }
            Err(status) => {
                self.redo_stack.push(group);
                Err(status)
            }
        }
    }

    /// Apply a delta sequence atomically: on any failure, roll back the
    /// prefix that already applied and return the error.
    fn apply_all_or_rollback(&mut self, deltas: &[Delta]) -> Result<(), VimStatus> {
        let mut applied: Vec<Delta> = Vec::with_capacity(deltas.len());
        for delta in deltas {
            match apply_delta(&mut self.graph, delta) {
                Ok(()) => applied.push(delta.clone()),
                Err(status) => {
                    self.rollback(applied);
                    return Err(status);
                }
            }
        }
        Ok(())
    }

    // -- Serialization (docs/ARCHITECTURE.md §10) -------------------------

    /// Serialize the parametric layer (settings, next-id, entities) to
    /// the VIMD envelope. Deterministic: the same state always produces
    /// the same bytes. Undo/redo stacks are not persisted.
    pub fn save(&self) -> Result<Vec<u8>, VimStatus> {
        crate::serialization::save(self)
    }

    /// Load a document from bytes produced by [`Self::save`]. Malformed
    /// data yields an error, never a panic; the reverse index is rebuilt
    /// and fully validated.
    pub fn load(bytes: &[u8]) -> Result<Document, VimStatus> {
        crate::serialization::load(bytes)
    }

    /// Dev/debug JSON export for diffing documents in tests
    /// (docs/ARCHITECTURE.md §10). Feature-gated: not part of wasm/FFI builds.
    #[cfg(any(test, feature = "json-debug"))]
    pub fn save_json(&self) -> Result<String, VimStatus> {
        crate::serialization::save_json(self)
    }

    // -- Internal seams for command compilation & serialization ----------

    pub(crate) fn alloc_entity_id(&mut self) -> EntityId {
        self.ids.allocate()
    }

    pub(crate) fn graph_ref(&self) -> &GraphState {
        &self.graph
    }

    /// Apply one delta speculatively, tracking it for rollback on
    /// failure of a later step (docs/ARCHITECTURE.md §4.1).
    pub(crate) fn apply_tracked(
        &mut self,
        delta: Delta,
        applied: &mut Vec<Delta>,
    ) -> Result<(), VimStatus> {
        apply_delta(&mut self.graph, &delta)?;
        applied.push(delta);
        Ok(())
    }
}

/// Merge freshly committed deltas into an existing coalesced group:
/// `SetParams` deltas merge per entity and `Rewire` deltas per
/// (entity, slot) — first `old` wins, last `new` wins
/// (docs/ARCHITECTURE.md §4.1). Other deltas append (they cannot occur in
/// coalescable updates today, but appending keeps the merge total).
fn coalesce_into(existing: &mut Vec<Delta>, incoming: Vec<Delta>) {
    for delta in incoming {
        let merged = existing.iter_mut().any(|slot| match (slot, &delta) {
            (
                Delta::SetParams { id: eid, new, .. },
                Delta::SetParams {
                    id, new: incoming_new, ..
                },
            ) if eid == id => {
                *new = incoming_new.clone();
                true
            }
            (
                Delta::Rewire {
                    id: eid,
                    slot: eslot,
                    new,
                    ..
                },
                Delta::Rewire {
                    id,
                    slot,
                    new: incoming_new,
                    ..
                },
            ) if eid == id && eslot == slot => {
                *new = incoming_new.clone();
                true
            }
            _ => false,
        });
        if !merged {
            existing.push(delta);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_document_is_empty_and_quiescent() {
        let doc = Document::new();
        assert_eq!(doc.entity_count(), 0);
        assert_eq!(doc.committed_generation(), 0);
        assert!(!doc.can_undo());
        assert!(!doc.can_redo());
        assert!(doc.dirty_set().is_empty());
    }

    #[test]
    fn submit_bumps_generation_and_tracks_dirty() {
        let mut doc = Document::new();
        let out = doc.submit(Command::CreateControlPoint {
            position: [1.0, 2.0, 3.0],
        });
        let created = out.ok().and_then(|o| o.created_ids.first().copied());
        assert!(created.is_some());
        assert_eq!(doc.committed_generation(), 1);
        let dirty = doc.take_dirty();
        assert_eq!(dirty.into_iter().collect::<Vec<_>>(), created.into_iter().collect::<Vec<_>>());
        assert!(doc.dirty_set().is_empty());
    }

    #[test]
    fn rejected_command_changes_nothing() {
        let mut doc = Document::new();
        let before = doc.save();
        let result = doc.submit(Command::DeleteLine { id: EntityId(42) });
        assert_eq!(result.err(), Some(VimStatus::EntityNotFound));
        assert_eq!(doc.save().ok(), before.ok());
        assert_eq!(doc.committed_generation(), 0);
        assert!(!doc.can_undo());
    }

    #[test]
    fn coalesced_updates_merge_first_old_last_new() {
        let mut doc = Document::new();
        let id = doc
            .submit(Command::CreateControlPoint {
                position: [0.0, 0.0, 0.0],
            })
            .ok()
            .and_then(|o| o.created_ids.first().copied());
        let Some(id) = id else {
            unreachable!("create succeeded above");
        };
        for i in 1..=5 {
            let status = doc.submit(Command::UpdateControlPoint {
                id,
                position: [i as f64, 0.0, 0.0],
                coalesce: true,
            });
            assert!(status.is_ok());
        }
        // One create step + one coalesced update step.
        assert_eq!(doc.undo_stack.len(), 2);
        let top = doc.undo_stack.last().map(|g| g.deltas.clone()).unwrap_or_default();
        assert_eq!(
            top,
            vec![Delta::SetParams {
                id,
                old: crate::entity::Params::ControlPoint {
                    position: [0.0, 0.0, 0.0]
                },
                new: crate::entity::Params::ControlPoint {
                    position: [5.0, 0.0, 0.0]
                },
            }]
        );
        // Undo the whole drag in one step.
        assert_eq!(doc.undo(), Ok(()));
        let pos = doc.entity(id).map(|r| r.params.clone());
        assert_eq!(
            pos,
            Some(crate::entity::Params::ControlPoint {
                position: [0.0, 0.0, 0.0]
            })
        );
    }

    #[test]
    fn undo_of_create_does_not_roll_back_next_id() {
        let mut doc = Document::new();
        let first = doc
            .submit(Command::CreateControlPoint {
                position: [0.0; 3],
            })
            .ok()
            .and_then(|o| o.created_ids.first().copied());
        assert_eq!(doc.undo(), Ok(()));
        let second = doc
            .submit(Command::CreateControlPoint {
                position: [0.0; 3],
            })
            .ok()
            .and_then(|o| o.created_ids.first().copied());
        // The counter did not rewind: the second create gets a fresh id.
        assert_ne!(first, second);
        assert!(second > first);
    }
}

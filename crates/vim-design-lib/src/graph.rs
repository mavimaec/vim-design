//! Dependency-graph state: entity storage plus the derived reverse index
//! (docs/ARCHITECTURE.md §3.2).
//!
//! Storage is a `BTreeMap` for deterministic iteration (and therefore
//! deterministic serialization). Edges point input → dependent in the
//! `downstream` index, which is derived state: rebuildable from scratch
//! and checked against the incremental version by [`GraphState::debug_validate`].

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use crate::entity::{EntityRecord, slots};
use crate::id::EntityId;
use crate::status::VimStatus;

/// The parametric layer's graph: entities and the derived downstream
/// adjacency. Mutated exclusively through `delta::apply_delta`.
#[derive(Debug, Default, Clone)]
pub struct GraphState {
    entities: BTreeMap<EntityId, EntityRecord>,
    /// Derived reverse index: input id → ids of entities that consume it.
    downstream: BTreeMap<EntityId, BTreeSet<EntityId>>,
}

impl GraphState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Rebuild a graph from a raw entity map (load path). Validates all
    /// structural invariants; a payload that violates them is malformed.
    pub fn from_entities(entities: BTreeMap<EntityId, EntityRecord>) -> Result<Self, VimStatus> {
        let mut graph = Self {
            entities,
            downstream: BTreeMap::new(),
        };
        graph.downstream = graph.rebuild_downstream();
        graph.validate_all().map_err(|_| VimStatus::MalformedData)?;
        Ok(graph)
    }

    pub fn get(&self, id: EntityId) -> Option<&EntityRecord> {
        self.entities.get(&id)
    }

    pub fn contains(&self, id: EntityId) -> bool {
        self.entities.contains_key(&id)
    }

    pub fn len(&self) -> usize {
        self.entities.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entities.is_empty()
    }

    /// Deterministic (id-ordered) iteration over all entities.
    pub fn iter(&self) -> impl Iterator<Item = (&EntityId, &EntityRecord)> {
        self.entities.iter()
    }

    /// The raw entity map (serialization reads this; never mutated
    /// directly elsewhere).
    pub fn entities(&self) -> &BTreeMap<EntityId, EntityRecord> {
        &self.entities
    }

    /// Direct dependents of `id`, sorted ascending. This is the queryable
    /// list backing `VimStatus::HasDependents` rejections.
    pub fn dependents(&self, id: EntityId) -> Vec<EntityId> {
        self.downstream
            .get(&id)
            .map(|set| set.iter().copied().collect())
            .unwrap_or_default()
    }

    /// True if `id` has at least one dependent.
    pub fn has_dependents(&self, id: EntityId) -> bool {
        self.downstream.get(&id).is_some_and(|set| !set.is_empty())
    }

    /// Would wiring `input` into a slot of `dependent` create a cycle?
    /// The new edge is `input → dependent`; a cycle exists iff `input` is
    /// already reachable downstream from `dependent` (or is `dependent`).
    pub fn would_create_cycle(&self, dependent: EntityId, input: EntityId) -> bool {
        if dependent == input {
            return true;
        }
        let mut queue = VecDeque::from([dependent]);
        let mut seen = BTreeSet::from([dependent]);
        while let Some(current) = queue.pop_front() {
            if let Some(nexts) = self.downstream.get(&current) {
                for &next in nexts {
                    if next == input {
                        return true;
                    }
                    if seen.insert(next) {
                        queue.push_back(next);
                    }
                }
            }
        }
        false
    }

    /// Downstream transitive closure of `roots`, **including** the roots
    /// themselves — the dirty set a commit hands to the evaluation layer
    /// (docs/ARCHITECTURE.md §6.1).
    pub fn dirty_closure(
        &self,
        roots: impl IntoIterator<Item = EntityId>,
    ) -> BTreeSet<EntityId> {
        let mut closure: BTreeSet<EntityId> = BTreeSet::new();
        let mut queue: VecDeque<EntityId> = VecDeque::new();
        for root in roots {
            if closure.insert(root) {
                queue.push_back(root);
            }
        }
        while let Some(current) = queue.pop_front() {
            if let Some(nexts) = self.downstream.get(&current) {
                for &next in nexts {
                    if closure.insert(next) {
                        queue.push_back(next);
                    }
                }
            }
        }
        closure
    }

    // ---- Mutation (delta module only) ---------------------------------

    /// Insert a fully validated record and index its edges. The caller
    /// (`delta::apply_delta`) performs all validation first.
    pub(crate) fn insert_record(&mut self, record: EntityRecord) {
        self.add_edges(&record);
        self.entities.insert(record.id, record);
    }

    /// Remove a record and de-index its edges.
    pub(crate) fn remove_record(&mut self, id: EntityId) -> Option<EntityRecord> {
        let record = self.entities.remove(&id)?;
        self.remove_edges(&record);
        self.downstream.remove(&id);
        Some(record)
    }

    /// Mutable access for `delta::apply_delta` (params/slot updates).
    /// Callers that change `inputs` must keep the edge index in sync via
    /// `remove_edges`/`add_edges` around the mutation.
    pub(crate) fn get_mut(&mut self, id: EntityId) -> Option<&mut EntityRecord> {
        self.entities.get_mut(&id)
    }

    /// Register every input edge of `record` in the downstream index.
    pub(crate) fn add_edges(&mut self, record: &EntityRecord) {
        for input in record.referenced() {
            self.downstream.entry(input).or_default().insert(record.id);
        }
    }

    /// Unregister every input edge of `record` from the downstream index.
    /// Set semantics make duplicate references within one record safe as
    /// long as add/remove always operate on whole records.
    pub(crate) fn remove_edges(&mut self, record: &EntityRecord) {
        for input in record.referenced() {
            let now_empty = match self.downstream.get_mut(&input) {
                Some(set) => {
                    set.remove(&record.id);
                    set.is_empty()
                }
                None => false,
            };
            if now_empty {
                self.downstream.remove(&input);
            }
        }
    }

    // ---- Validation ----------------------------------------------------

    /// Rebuild the downstream index from scratch (ground truth).
    fn rebuild_downstream(&self) -> BTreeMap<EntityId, BTreeSet<EntityId>> {
        let mut rebuilt: BTreeMap<EntityId, BTreeSet<EntityId>> = BTreeMap::new();
        for (id, record) in &self.entities {
            for input in record.referenced() {
                rebuilt.entry(input).or_default().insert(*id);
            }
        }
        rebuilt
    }

    /// Kahn's algorithm over the stored edges; `Ok` iff acyclic.
    fn check_acyclic(&self) -> Result<(), VimStatus> {
        let mut in_degree: BTreeMap<EntityId, usize> = self
            .entities
            .keys()
            .map(|id| (*id, 0usize))
            .collect();
        for record in self.entities.values() {
            let unique_inputs: BTreeSet<EntityId> = record.referenced().collect();
            if let Some(count) = in_degree.get_mut(&record.id) {
                *count = unique_inputs.len();
            }
        }
        let mut queue: VecDeque<EntityId> = in_degree
            .iter()
            .filter(|(_, deg)| **deg == 0)
            .map(|(id, _)| *id)
            .collect();
        let mut visited = 0usize;
        while let Some(current) = queue.pop_front() {
            visited = visited.saturating_add(1);
            if let Some(dependents) = self.downstream.get(&current) {
                for dependent in dependents {
                    if let Some(deg) = in_degree.get_mut(dependent) {
                        *deg = deg.saturating_sub(1);
                        if *deg == 0 {
                            queue.push_back(*dependent);
                        }
                    }
                }
            }
        }
        if visited == self.entities.len() {
            Ok(())
        } else {
            Err(VimStatus::WouldCreateCycle)
        }
    }

    /// Full structural validation: every record shape-checks against its
    /// slot table, every reference exists with an accepted kind, the
    /// derived index matches a from-scratch rebuild, and the graph is
    /// acyclic. Used by the load path and by [`Self::debug_validate`].
    pub fn validate_all(&self) -> Result<(), VimStatus> {
        for (id, record) in &self.entities {
            if record.id != *id {
                return Err(VimStatus::DeltaMismatch);
            }
            if record.id == EntityId::INVALID {
                return Err(VimStatus::MalformedData);
            }
            record.validate_shape()?;
            let decls = slots(record.kind());
            for (decl, value) in decls.iter().zip(record.inputs.iter()) {
                for referenced in value.referenced() {
                    match self.entities.get(&referenced) {
                        None => return Err(VimStatus::EntityNotFound),
                        Some(target) if !decl.accepts(target.kind()) => {
                            return Err(VimStatus::SlotKindMismatch);
                        }
                        Some(_) => {}
                    }
                }
            }
        }
        if self.rebuild_downstream() != self.downstream {
            return Err(VimStatus::DeltaMismatch);
        }
        self.check_acyclic()
    }

    /// Debug invariant check (docs/ARCHITECTURE.md §3.2): rebuilds the
    /// reverse index from scratch and checks acyclicity. Called after
    /// every delta in debug builds; also available to tests.
    pub fn debug_validate(&self) -> Result<(), VimStatus> {
        self.validate_all()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entity::{Params, SlotValue};

    fn cp(id: u64) -> EntityRecord {
        EntityRecord {
            id: EntityId(id),
            params: Params::ControlPoint {
                position: [0.0, 0.0, 0.0],
            },
            inputs: vec![],
        }
    }

    fn line(id: u64, a: u64, b: u64) -> EntityRecord {
        EntityRecord {
            id: EntityId(id),
            params: Params::Line,
            inputs: vec![
                SlotValue::One(Some(EntityId(a))),
                SlotValue::One(Some(EntityId(b))),
            ],
        }
    }

    fn simple_graph() -> GraphState {
        let mut graph = GraphState::new();
        graph.insert_record(cp(1));
        graph.insert_record(cp(2));
        graph.insert_record(line(3, 1, 2));
        graph
    }

    #[test]
    fn downstream_index_tracks_edges() {
        let graph = simple_graph();
        assert_eq!(graph.dependents(EntityId(1)), vec![EntityId(3)]);
        assert_eq!(graph.dependents(EntityId(3)), Vec::<EntityId>::new());
        assert!(graph.debug_validate().is_ok());
    }

    #[test]
    fn cycle_detection_sees_transitive_paths() {
        let graph = simple_graph();
        // line(3) depends on cp(1): wiring 3 as an input of 1 would cycle.
        assert!(graph.would_create_cycle(EntityId(1), EntityId(3)));
        assert!(graph.would_create_cycle(EntityId(3), EntityId(3)));
        assert!(!graph.would_create_cycle(EntityId(3), EntityId(2)));
    }

    #[test]
    fn dirty_closure_includes_roots_and_downstream() {
        let graph = simple_graph();
        let closure = graph.dirty_closure([EntityId(1)]);
        assert_eq!(
            closure.into_iter().collect::<Vec<_>>(),
            vec![EntityId(1), EntityId(3)]
        );
    }

    #[test]
    fn from_entities_rejects_dangling_reference() {
        let mut entities = BTreeMap::new();
        entities.insert(EntityId(3), line(3, 1, 2)); // cps 1 and 2 missing
        assert_eq!(
            GraphState::from_entities(entities).err(),
            Some(VimStatus::MalformedData)
        );
    }
}

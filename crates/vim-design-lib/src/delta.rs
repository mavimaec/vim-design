//! The delta kernel (docs/ARCHITECTURE.md §4.1).
//!
//! The four primitive deltas are the only operations that ever mutate the
//! parametric layer. Each delta carries enough prior state to be inverted
//! mechanically: applying a delta and then its inverse restores
//! byte-identical state. `apply_delta` validates *before* mutating, so a
//! rejected delta leaves the graph untouched — the building block for
//! speculative apply + rollback at the command level.

use serde::{Deserialize, Serialize};

use crate::entity::{EntityRecord, Params, SlotValue, slots, validate_slot_value};
use crate::graph::GraphState;
use crate::id::EntityId;
use crate::status::VimStatus;

/// Index of a slot within an entity's static slot table.
pub type SlotIdx = usize;

/// A primitive mutation of the parametric layer. Exactly four exist;
/// commands compile to sequences of these (docs/ARCHITECTURE.md §4.1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Delta {
    /// Add an entity. Inverse: `Remove` with the same record.
    Insert { id: EntityId, record: EntityRecord },
    /// Remove an entity. Carries the full record so the inverse `Insert`
    /// restores it — including its original id (docs/ARCHITECTURE.md §3.1).
    Remove { id: EntityId, record: EntityRecord },
    /// Replace an entity's params. Inverse: swap `old`/`new`.
    SetParams {
        id: EntityId,
        old: Params,
        new: Params,
    },
    /// Replace one whole slot value (single or multi). Storing the full
    /// old and new values keeps multi-slot rewires mechanically
    /// invertible. Inverse: swap `old`/`new`.
    Rewire {
        id: EntityId,
        slot: SlotIdx,
        old: SlotValue,
        new: SlotValue,
    },
}

impl Delta {
    /// The mechanical inverse (docs/ARCHITECTURE.md §4.1). There are
    /// exactly these four inversion rules — no per-command inverses.
    pub fn invert(&self) -> Delta {
        match self {
            Delta::Insert { id, record } => Delta::Remove {
                id: *id,
                record: record.clone(),
            },
            Delta::Remove { id, record } => Delta::Insert {
                id: *id,
                record: record.clone(),
            },
            Delta::SetParams { id, old, new } => Delta::SetParams {
                id: *id,
                old: new.clone(),
                new: old.clone(),
            },
            Delta::Rewire { id, slot, old, new } => Delta::Rewire {
                id: *id,
                slot: *slot,
                old: new.clone(),
                new: old.clone(),
            },
        }
    }

    /// The entity this delta touches (a dirty-set root).
    pub fn target(&self) -> EntityId {
        match self {
            Delta::Insert { id, .. }
            | Delta::Remove { id, .. }
            | Delta::SetParams { id, .. }
            | Delta::Rewire { id, .. } => *id,
        }
    }
}

/// Validate `value` as the new content of `record`'s slot `slot_idx`:
/// shape, required-ness, referenced ids exist with accepted kinds, and no
/// cycle through `record.id`.
fn validate_new_slot_value(
    graph: &GraphState,
    dependent: EntityId,
    kind_slots: &'static [crate::entity::SlotDecl],
    slot_idx: SlotIdx,
    value: &SlotValue,
) -> Result<(), VimStatus> {
    let decl = kind_slots
        .get(slot_idx)
        .ok_or(VimStatus::SlotIndexOutOfRange)?;
    validate_slot_value(decl, value)?;
    for referenced in value.referenced() {
        let target = graph.get(referenced).ok_or(VimStatus::EntityNotFound)?;
        // Cycle check before kind check so a self-reference reports as a
        // cycle rather than as an incidental kind mismatch.
        if graph.would_create_cycle(dependent, referenced) {
            return Err(VimStatus::WouldCreateCycle);
        }
        if !decl.accepts(target.kind()) {
            return Err(VimStatus::SlotKindMismatch);
        }
    }
    Ok(())
}

/// Apply one delta to the graph. All validation happens before any
/// mutation, so `Err` guarantees the graph is unchanged. In debug builds
/// the full invariant check runs after every successful application
/// (docs/ARCHITECTURE.md §3.2).
pub fn apply_delta(graph: &mut GraphState, delta: &Delta) -> Result<(), VimStatus> {
    apply_delta_inner(graph, delta)?;
    #[cfg(debug_assertions)]
    {
        // A failure here is a substrate bug: surface it as an error (the
        // never-crash contract forbids asserting/panicking).
        graph.debug_validate()?;
    }
    Ok(())
}

fn apply_delta_inner(graph: &mut GraphState, delta: &Delta) -> Result<(), VimStatus> {
    match delta {
        Delta::Insert { id, record } => {
            if *id == EntityId::INVALID {
                return Err(VimStatus::EntityNotFound);
            }
            if record.id != *id {
                return Err(VimStatus::DeltaMismatch);
            }
            if graph.contains(*id) {
                return Err(VimStatus::DuplicateEntityId);
            }
            // Singleton gate (docs/AUTHORING.md §1): enforced at the
            // delta level, so commands, composites, speculative apply,
            // undo, and redo all hit the same check. Undo of a delete
            // re-inserts fine — the original is gone by then.
            if record.kind() == crate::entity::EntityKind::Site
                && graph.iter().any(|(_, r)| {
                    r.kind() == crate::entity::EntityKind::Site
                })
            {
                return Err(VimStatus::SingletonExists);
            }
            record.validate_shape()?;
            // References must exist with accepted kinds. A brand-new node
            // cannot create a cycle: it has no dependents yet and cannot
            // reference itself (it is not in the graph until now).
            let decls = slots(record.kind());
            for (decl, value) in decls.iter().zip(record.inputs.iter()) {
                for referenced in value.referenced() {
                    if referenced == *id {
                        return Err(VimStatus::WouldCreateCycle);
                    }
                    let target =
                        graph.get(referenced).ok_or(VimStatus::EntityNotFound)?;
                    if !decl.accepts(target.kind()) {
                        return Err(VimStatus::SlotKindMismatch);
                    }
                }
            }
            graph.insert_record(record.clone());
            Ok(())
        }
        Delta::Remove { id, record } => {
            let current = graph.get(*id).ok_or(VimStatus::EntityNotFound)?;
            // The recorded prior state must match reality, or inversion
            // (re-Insert of `record`) would not restore the document.
            if current != record {
                return Err(VimStatus::DeltaMismatch);
            }
            // Reject-if-dependents (docs/ARCHITECTURE.md §3.2).
            if graph.has_dependents(*id) {
                return Err(VimStatus::HasDependents);
            }
            graph.remove_record(*id);
            Ok(())
        }
        Delta::SetParams { id, old, new } => {
            let current = graph.get(*id).ok_or(VimStatus::EntityNotFound)?;
            if current.params != *old {
                return Err(VimStatus::DeltaMismatch);
            }
            // Params may never change the entity's kind: the slot table
            // (and therefore all wiring validation) is keyed by kind.
            if new.kind() != current.kind() {
                return Err(VimStatus::ParamsKindMismatch);
            }
            if let Some(record) = graph.get_mut(*id) {
                record.params = new.clone();
            }
            Ok(())
        }
        Delta::Rewire { id, slot, old, new } => {
            let current = graph.get(*id).ok_or(VimStatus::EntityNotFound)?;
            let current_value = current
                .inputs
                .get(*slot)
                .ok_or(VimStatus::SlotIndexOutOfRange)?;
            if current_value != old {
                return Err(VimStatus::DeltaMismatch);
            }
            validate_new_slot_value(graph, *id, slots(current.kind()), *slot, new)?;
            // Re-index around the mutation using whole records so that an
            // input referenced by several slots keeps its edge.
            let before = current.clone();
            graph.remove_edges(&before);
            if let Some(record) = graph.get_mut(*id) {
                if let Some(slot_value) = record.inputs.get_mut(*slot) {
                    *slot_value = new.clone();
                }
            }
            if let Some(after) = graph.get(*id).cloned() {
                graph.add_edges(&after);
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entity::slot;

    fn cp_record(id: u64, z: f64) -> EntityRecord {
        EntityRecord {
            id: EntityId(id),
            params: Params::ControlPoint {
                position: [0.0, 0.0, z],
            },
            inputs: vec![SlotValue::One(None)], // unattached plane slot
        }
    }

    fn line_record(id: u64, a: u64, b: u64) -> EntityRecord {
        EntityRecord {
            id: EntityId(id),
            params: Params::Line,
            inputs: vec![
                SlotValue::One(Some(EntityId(a))),
                SlotValue::One(Some(EntityId(b))),
            ],
        }
    }

    fn seeded() -> GraphState {
        let mut graph = GraphState::new();
        for delta in [
            Delta::Insert {
                id: EntityId(1),
                record: cp_record(1, 0.0),
            },
            Delta::Insert {
                id: EntityId(2),
                record: cp_record(2, 1.0),
            },
            Delta::Insert {
                id: EntityId(3),
                record: line_record(3, 1, 2),
            },
        ] {
            assert_eq!(apply_delta(&mut graph, &delta), Ok(()));
        }
        graph
    }

    /// Applying a delta then its inverse restores identical state — for
    /// each of the four primitives (docs/ARCHITECTURE.md §4.1).
    #[test]
    fn every_primitive_round_trips_through_its_inverse() {
        let baseline = seeded();
        let deltas = [
            Delta::Insert {
                id: EntityId(9),
                record: cp_record(9, 2.0),
            },
            Delta::SetParams {
                id: EntityId(1),
                old: Params::ControlPoint {
                    position: [0.0, 0.0, 0.0],
                },
                new: Params::ControlPoint {
                    position: [5.0, 0.0, 0.0],
                },
            },
            Delta::Rewire {
                id: EntityId(3),
                slot: slot::LINE_END,
                old: SlotValue::One(Some(EntityId(2))),
                new: SlotValue::One(Some(EntityId(1))),
            },
        ];
        for delta in deltas {
            let mut graph = seeded();
            assert_eq!(apply_delta(&mut graph, &delta), Ok(()));
            assert_eq!(apply_delta(&mut graph, &delta.invert()), Ok(()));
            assert_eq!(graph.entities(), baseline.entities());
        }
        // Remove round-trip needs a dependent-free target.
        let mut graph = seeded();
        let remove = Delta::Remove {
            id: EntityId(3),
            record: line_record(3, 1, 2),
        };
        assert_eq!(apply_delta(&mut graph, &remove), Ok(()));
        assert_eq!(apply_delta(&mut graph, &remove.invert()), Ok(()));
        assert_eq!(graph.entities(), baseline.entities());
    }

    #[test]
    fn invert_is_an_involution() {
        let delta = Delta::Rewire {
            id: EntityId(3),
            slot: 0,
            old: SlotValue::Many(vec![EntityId(1)]),
            new: SlotValue::Many(vec![EntityId(1), EntityId(2)]),
        };
        assert_eq!(delta.invert().invert(), delta);
    }

    #[test]
    fn rejected_deltas_leave_graph_untouched() {
        let baseline = seeded();
        let rejects = [
            (
                Delta::Insert {
                    id: EntityId(1),
                    record: cp_record(1, 0.0),
                },
                VimStatus::DuplicateEntityId,
            ),
            (
                Delta::Remove {
                    id: EntityId(1),
                    record: cp_record(1, 0.0),
                },
                VimStatus::HasDependents,
            ),
            (
                Delta::Remove {
                    id: EntityId(3),
                    record: line_record(3, 1, 1), // stale record
                },
                VimStatus::DeltaMismatch,
            ),
            (
                Delta::SetParams {
                    id: EntityId(1),
                    old: Params::ControlPoint {
                        position: [9.0, 9.0, 9.0], // stale old
                    },
                    new: Params::ControlPoint {
                        position: [1.0, 1.0, 1.0],
                    },
                },
                VimStatus::DeltaMismatch,
            ),
            (
                Delta::SetParams {
                    id: EntityId(1),
                    old: Params::ControlPoint {
                        position: [0.0, 0.0, 0.0],
                    },
                    new: Params::Line, // kind change
                },
                VimStatus::ParamsKindMismatch,
            ),
            (
                Delta::Rewire {
                    id: EntityId(3),
                    slot: slot::LINE_END,
                    old: SlotValue::One(Some(EntityId(2))),
                    new: SlotValue::One(Some(EntityId(3))), // self-cycle
                },
                VimStatus::WouldCreateCycle,
            ),
            (
                Delta::Rewire {
                    id: EntityId(3),
                    slot: slot::LINE_END,
                    old: SlotValue::One(Some(EntityId(2))),
                    new: SlotValue::One(Some(EntityId(3000))), // unknown id
                },
                VimStatus::EntityNotFound,
            ),
            (
                Delta::Rewire {
                    id: EntityId(3),
                    slot: slot::LINE_END,
                    old: SlotValue::One(Some(EntityId(2))),
                    new: SlotValue::One(None), // required slot emptied
                },
                VimStatus::MissingRequiredSlot,
            ),
        ];
        for (delta, expected) in rejects {
            let mut graph = seeded();
            assert_eq!(apply_delta(&mut graph, &delta), Err(expected));
            assert_eq!(graph.entities(), baseline.entities());
            assert!(graph.debug_validate().is_ok());
        }
    }
}

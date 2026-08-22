//! Subelement references — provenance naming (docs/ARCHITECTURE.md §3.4).
//!
//! Generated topology (an extrusion's lateral faces, a chamfer's blend
//! face) has no `EntityId`; it is referenced by *provenance*: the stable
//! ids of the inputs that gave rise to it. This module defines the
//! storable/serializable reference types only — **resolution of a
//! `SubRef` against evaluated geometry is evaluation-layer work and is
//! out of scope for the parametric substrate.**

use serde::{Deserialize, Serialize};

use crate::id::EntityId;

/// Provenance path of a generated subelement within its owner's output.
///
/// Paths are derived from the stable ids of the inputs that produced the
/// subelement, never from kernel output indices (the topological-naming
/// rule, docs/ARCHITECTURE.md §3.4). The variant set grows with the
/// evaluators; it is a closed serde enum so it stays expressible over the
/// C ABI.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ProvenancePath {
    /// A lateral face/edge swept from a source input (e.g. the face an
    /// extrusion sweeps from profile edge `source`).
    Side { source: EntityId },
    /// The cap at the start of a sweep (the profile itself).
    CapStart,
    /// The cap at the end of a sweep.
    CapEnd,
    /// A blend face generated between two inputs (e.g. a chamfer face
    /// named by the pair of faces adjacent to the blended edge).
    Blend { a: EntityId, b: EntityId },
    /// A face produced by cutting with a plane (e.g. a section-box cap).
    Cut { plane: EntityId },
}

/// Reference to a generated subelement: the entity whose evaluation owns
/// the topology, plus the provenance path identifying it within that
/// output.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct SubRef {
    pub owner: EntityId,
    pub path: ProvenancePath,
}

/// A reference to either a whole entity or a generated subelement.
/// Selections evaluate to ordered `Vec<Ref>` sets (docs/ARCHITECTURE.md
/// §3.5); ordering derives from the `Ord` impl for determinism.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Ref {
    Entity(EntityId),
    Sub(SubRef),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subref_orders_deterministically() {
        let a = Ref::Entity(EntityId(1));
        let b = Ref::Sub(SubRef {
            owner: EntityId(1),
            path: ProvenancePath::CapStart,
        });
        // Entities sort before subrefs (enum variant order); stable order
        // is what matters, not the specific choice.
        assert!(a < b);
    }
}

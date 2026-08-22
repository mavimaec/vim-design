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
    /// A generated **edge**, addressed as the intersection of two named
    /// faces (e.g. an extrusion's top rim segment =
    /// `SharedEdge { CapEnd, Side { e } }`). Chamfer blend faces are also
    /// named by the `SharedEdge` path of the edge they replace
    /// (docs/ARCHITECTURE.md §3.4: "a chamfer's blend face is named by
    /// the edge it blends"). Operands are kept in canonical (sorted)
    /// order — build via [`ProvenancePath::shared_edge`].
    SharedEdge {
        a: Box<ProvenancePath>,
        b: Box<ProvenancePath>,
    },
}

impl ProvenancePath {
    /// Canonical `SharedEdge` constructor: operand order does not matter,
    /// so operands are sorted for deterministic equality/serialization.
    pub fn shared_edge(a: ProvenancePath, b: ProvenancePath) -> ProvenancePath {
        let (a, b) = if a <= b { (a, b) } else { (b, a) };
        ProvenancePath::SharedEdge {
            a: Box::new(a),
            b: Box::new(b),
        }
    }

    /// The canonical form of this path (`SharedEdge` operands sorted,
    /// recursively). Non-canonical forms can arrive over serde/FFI.
    pub fn canonical(&self) -> ProvenancePath {
        match self {
            ProvenancePath::SharedEdge { a, b } => {
                ProvenancePath::shared_edge(a.canonical(), b.canonical())
            }
            other => other.clone(),
        }
    }

    /// True when this path addresses an edge rather than a face.
    pub fn is_edge(&self) -> bool {
        matches!(self, ProvenancePath::SharedEdge { .. })
    }
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
    fn shared_edge_is_order_insensitive() {
        let side = ProvenancePath::Side {
            source: EntityId(7),
        };
        let ab = ProvenancePath::shared_edge(side.clone(), ProvenancePath::CapEnd);
        let ba = ProvenancePath::shared_edge(ProvenancePath::CapEnd, side);
        assert_eq!(ab, ba);
        assert!(ab.is_edge());
        assert_eq!(ab.canonical(), ab);
        // Non-canonical operand order canonicalizes to the same value.
        let raw = ProvenancePath::SharedEdge {
            a: Box::new(ProvenancePath::CapEnd),
            b: Box::new(ProvenancePath::CapStart),
        };
        assert_eq!(
            raw.canonical(),
            ProvenancePath::shared_edge(ProvenancePath::CapStart, ProvenancePath::CapEnd)
        );
    }

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

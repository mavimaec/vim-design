//! Construction-plane chains: levels and the workplanes nested under
//! them.
//!
//! A workplane is offset along its parent's normal; its parent is a
//! level or another workplane. Every chain ends at a level, the root.
//! Level frames use the world axes, so a plane's height above the scene
//! origin is its root level's elevation plus the offsets along the chain.

use crate::document::Document;
use crate::entity::{EntityKind, Params, slot};
use crate::graph::GraphState;
use crate::id::EntityId;

/// Longest parent chain followed. The graph is acyclic, so this only
/// bounds the walk against corrupt input.
const MAX_CHAIN: usize = 1024;

fn parent_of(graph: &GraphState, id: EntityId) -> Option<EntityId> {
    graph
        .get(id)?
        .inputs
        .get(slot::WORKPLANE_PARENT)?
        .referenced()
        .next()
}

/// The root level of a construction plane in the graph.
pub(crate) fn root_level_in(graph: &GraphState, plane: EntityId) -> Option<EntityId> {
    let mut current = plane;
    for _ in 0..MAX_CHAIN {
        match graph.get(current)?.kind() {
            EntityKind::Level => return Some(current),
            EntityKind::Workplane => current = parent_of(graph, current)?,
            _ => return None,
        }
    }
    None
}

/// The root level of a construction plane: the plane itself for a level,
/// the level at the end of the parent chain for a workplane, `None` for
/// any other entity. An element drawn on a workplane is associated with
/// this level.
pub fn root_level(doc: &Document, plane: EntityId) -> Option<EntityId> {
    root_level_in(doc.graph_ref(), plane)
}

/// Height of a construction plane above the scene origin (meters), from
/// params only: the root level's elevation plus the workplane offsets
/// along the chain. `None` for an entity that is not a construction
/// plane.
pub fn plane_elevation(doc: &Document, plane: EntityId) -> Option<f64> {
    let graph = doc.graph_ref();
    let mut current = plane;
    let mut offsets = Vec::new();
    for _ in 0..MAX_CHAIN {
        match &graph.get(current)?.params {
            // Add from the root outward, as evaluation does, so the result
            // matches the evaluated frame exactly.
            Params::Level { elevation_m, .. } => {
                return Some(offsets.iter().rev().fold(*elevation_m, |h, off| h + off));
            }
            Params::Workplane { offset_m, .. } => {
                offsets.push(*offset_m);
                current = parent_of(graph, current)?;
            }
            _ => return None,
        }
    }
    None
}

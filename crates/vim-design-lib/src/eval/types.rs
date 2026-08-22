//! Public data types of the evaluation layer: evaluated values, per-entity
//! evaluation state/diagnostics (docs/ARCHITECTURE.md §6.4), and the poll
//! facade payloads (§6.3).

use crate::id::EntityId;
use crate::kernel::{CurveSpec, KernelFace, KernelSolid, WireSpec};

// ---------------------------------------------------------------------
// Evaluated values (docs/ARCHITECTURE.md §3.3).
// ---------------------------------------------------------------------

/// The result of evaluating one entity — a closed enum. Kernel types stay
/// behind the `kernel` seam ([`KernelFace`]/[`KernelSolid`] are opaque
/// handles; curves and wires are plain-data specs).
#[derive(Debug, Clone)]
pub enum Evaluated {
    /// A point in space (meters). Produced by `ControlPoint`.
    Point([f64; 3]),
    /// An infinite plane with unit normal. Produced by `Plane`.
    Plane { origin: [f64; 3], normal: [f64; 3] },
    /// An oriented curve. Produced by `Line`, `Circle`, `Spline`.
    Curve(CurveSpec),
    /// A trimmed, oriented curve. Produced by `Edge` (v1 trims are always
    /// the full interval of the input curve).
    Edge(CurveSpec),
    /// A validated closed loop of curves, ordered and oriented
    /// head-to-tail. Produced by `Wire`.
    Wire(WireSpec),
    /// A planar BREP face plus its (optional) material assignment.
    /// Produced by `Face`.
    Face {
        face: KernelFace,
        material: Option<EntityId>,
    },
    /// A closed BREP solid plus the material it inherits (an extrusion or
    /// revolve inherits its profile face's material; a `Solid` entity the
    /// first material found among its faces — v1 whole-solid materials,
    /// per-face materials arrive with `SubRef` resolution).
    /// Produced by `Extrusion`, `Revolve`, `Solid`.
    Solid {
        solid: KernelSolid,
        material: Option<EntityId>,
    },
    /// Marker for materials (the render parameters live in the entity's
    /// `Params::Material`; readers query the document). Produced by
    /// `Material`.
    Material,
    /// The solids collected from an `Element`'s members:
    /// `(member entity id, solid, material)` in member-slot order.
    SolidSet(Vec<(EntityId, KernelSolid, Option<EntityId>)>),
    /// A placement of an element. Produced by `Instance`.
    Instance {
        element: EntityId,
        /// Rigid row-major 4x3 transform (see `Params::Instance`).
        transform: [f64; 12],
    },
}

// ---------------------------------------------------------------------
// Per-entity evaluation state (docs/ARCHITECTURE.md §6.4).
// ---------------------------------------------------------------------

/// Classification of a per-entity evaluation failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvalErrorKind {
    /// A required input has never produced a value (and has no stale
    /// value to fall back to).
    MissingInput,
    /// An input is in error and offers no stale geometry to evaluate
    /// against.
    UpstreamError,
    /// Wire validation failed: edges do not chain into a single closed
    /// loop within kernel tolerance.
    WireNotClosed,
    /// The face's wires are not coplanar within tolerance (or are
    /// degenerate — zero area admits no plane).
    NotPlanar,
    /// Degenerate geometry (zero-length line/axis, zero radius, zero
    /// sweep angle, ...).
    Degenerate,
    /// The kernel rejected the operation (topology/boolean failure).
    Kernel,
    /// Tessellation failed (dropped face or empty mesh).
    Tessellation,
    /// The entity kind is not evaluated in this milestone
    /// (`Selection`, `SectionBox`).
    NotYetImplemented,
    /// A provenance-named subelement reference (`SubRef`) did not resolve
    /// against the owner's current topology — the source edge was
    /// deleted, a cap vanished on an angle change, the named faces no
    /// longer share an edge (docs/ARCHITECTURE.md §3.4: never a silent
    /// re-bind).
    UnresolvedSubRef,
    /// The wiring is valid but this configuration is not supported yet
    /// (e.g. a spline extrusion path).
    NotYetSupported,
    /// The kernel panicked; caught at the seam (docs/ARCHITECTURE.md §8).
    InternalPanic,
}

/// A per-entity evaluation diagnostic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvalDiag {
    pub kind: EvalErrorKind,
    pub message: String,
}

impl EvalDiag {
    pub fn new(kind: EvalErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for EvalDiag {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}: {}", self.kind, self.message)
    }
}

/// Evaluation state of one entity (docs/ARCHITECTURE.md §6.4). On
/// failure the entity's last successful value and mesh are retained
/// (stale) and downstream evaluates against them where possible.
#[derive(Debug, Clone, PartialEq)]
pub enum EvalState {
    /// Evaluated successfully at `generation`.
    UpToDate { generation: u64 },
    /// Evaluation failed; `stale_generation` is the generation of the
    /// retained last-successful value (`None` if there never was one).
    Error {
        diag: EvalDiag,
        stale_generation: Option<u64>,
    },
}

/// What a `SubRef` currently resolves to on its owner's evaluated
/// geometry (counts only — kernel topology never crosses the facade;
/// face/edge *indices* are an internal detail, docs/ARCHITECTURE.md §3.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubRefResolution {
    /// A face path resolved to this many BREP faces (a `Side` of a
    /// full revolve spans several kernel faces; they share one name).
    Faces(usize),
    /// A `SharedEdge` path resolved to this many kernel edges.
    Edges(usize),
}

// ---------------------------------------------------------------------
// Poll facade payloads (docs/ARCHITECTURE.md §6.3).
// ---------------------------------------------------------------------

/// Renderer-ready triangle mesh. `positions`/`normals` are parallel
/// per-vertex arrays; `indices` is a triangle list; `submeshes`
/// partitions the index buffer by material.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Mesh {
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub indices: Vec<u32>,
    pub submeshes: Vec<Submesh>,
}

impl Mesh {
    pub fn triangle_count(&self) -> usize {
        self.indices.len() / 3
    }

    pub fn vertex_count(&self) -> usize {
        self.positions.len()
    }
}

/// A contiguous index-buffer range rendered with one material.
/// `material` is a `Material` entity id — readers fetch its color and
/// roughness from the document (`Params::Material`); `None` means "use
/// the caller's default material".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Submesh {
    pub material: Option<EntityId>,
    /// Offset into `indices` (a multiple of 3).
    pub index_start: u32,
    /// Number of indices (a multiple of 3).
    pub index_count: u32,
}

/// Upsert of one renderable mesh, keyed by the mesh-owner entity id — an
/// `Element`, or a standalone solid producer (`Extrusion` / `Revolve` /
/// `Solid` not consumed by any element); see the module docs for the
/// ownership rule.
#[derive(Debug, Clone)]
pub struct MeshUpdate {
    /// Element id or standalone solid-producer id.
    pub id: EntityId,
    /// Committed generation at which this mesh was produced.
    pub generation: u64,
    pub mesh: Mesh,
}

/// Upsert of one instance placement.
#[derive(Debug, Clone, Copy)]
pub struct InstanceUpdate {
    pub id: EntityId,
    pub element_id: EntityId,
    /// Rigid row-major 4x3 transform.
    pub transform: [f64; 12],
}

/// The changed-set delta returned by one poll (docs/ARCHITECTURE.md
/// §6.3): coalesced latest-state upserts keyed by stable ids, explicit
/// tombstones for removals, per-entity error transitions, and the
/// settledness counters. Apply order: removals first, then upserts.
#[derive(Debug, Clone, Default)]
pub struct Updates {
    /// Mesh upserts (one entry per changed owner — coalesced).
    pub meshes: Vec<MeshUpdate>,
    /// Tombstones: owners whose mesh disappeared (deleted, or absorbed
    /// into an element).
    pub meshes_removed: Vec<EntityId>,
    /// Instance upserts.
    pub instances: Vec<InstanceUpdate>,
    /// Tombstones: deleted instances.
    pub instances_removed: Vec<EntityId>,
    /// Entities newly in error (or whose diagnostic changed), with the
    /// current diagnostic.
    pub errors: Vec<(EntityId, EvalDiag)>,
    /// Entities whose error state cleared since the last poll.
    pub errors_cleared: Vec<EntityId>,
    /// Latest committed command generation (`Document`).
    pub committed_generation: u64,
    /// Everything at or below this generation is fully evaluated and
    /// meshed. `evaluated == committed` means quiescent.
    pub evaluated_generation: u64,
    /// Entities whose re-evaluation is still pending (dirty but not yet
    /// evaluated). Zero when quiescent.
    pub pending_count: usize,
}

impl Updates {
    /// True when nothing changed and nothing is pending.
    pub fn is_settled_and_empty(&self) -> bool {
        self.meshes.is_empty()
            && self.meshes_removed.is_empty()
            && self.instances.is_empty()
            && self.instances_removed.is_empty()
            && self.errors.is_empty()
            && self.errors_cleared.is_empty()
            && self.pending_count == 0
            && self.committed_generation == self.evaluated_generation
    }
}

//! Entities, params, and static slot declarations
//! (docs/ARCHITECTURE.md §§3.1–3.2).
//!
//! Every entity kind declares its inputs as **ordered, typed slots** in a
//! static table ([`slots`]). The tables are the single source of truth for
//! command validation, kind-checking on rewires, and reject-if-dependents.
//! Units are meters, Z-up (§7).

use serde::{Deserialize, Serialize};

use crate::id::EntityId;
use crate::selection::{PredicateAst, SelectionScope};
use crate::status::VimStatus;
use crate::subref::{EdgeTarget, FaceTarget};

/// The closed set of entity kinds (docs/ARCHITECTURE.md §3.1).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
pub enum EntityKind {
    ControlPoint,
    Plane,
    Circle,
    Line,
    Spline,
    Edge,
    Wire,
    Face,
    Solid,
    Material,
    Extrusion,
    Revolve,
    Chamfer,
    SectionBox,
    Element,
    Instance,
    Selection,
}

/// Per-kind parameters — a closed serde enum with one variant per
/// [`EntityKind`]. Params hold *values only*; references to other
/// entities live in the record's slots. Minimal-but-real: these fields
/// will grow with the evaluators.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Params {
    /// A movable point in space (meters).
    ControlPoint { position: [f64; 3] },
    /// An infinite plane through `origin` with unit `normal`.
    Plane { origin: [f64; 3], normal: [f64; 3] },
    /// A circle of `radius` meters around its center input, on its
    /// optional plane input (defaults to horizontal at the center).
    Circle { radius: f64 },
    /// A straight segment between its two control-point inputs.
    Line,
    /// A NURBS-ish curve over its control-point inputs. `degree`/`knots`
    /// are optional-simple: `None` means "let the evaluator choose".
    Spline {
        degree: Option<u32>,
        knots: Option<Vec<f64>>,
    },
    /// A topological edge over its curve input.
    Edge,
    /// An ordered, closed loop of edge inputs — the boundary of a face
    /// (per PROJECT_REQUIREMENTS.md; supersedes the earlier
    /// face-directly-from-edges model).
    Wire,
    /// A face bounded by one outer wire input plus optional inner hole
    /// wires; optional material input; optional plane surface input
    /// (when wired, the plane is the face's surface — when absent, the
    /// evaluator infers a planar surface from the wire).
    Face,
    /// A solid bounded by its face inputs.
    Solid,
    /// A render material.
    Material {
        name: String,
        color: [f64; 3],
        roughness: f64,
    },
    /// Sweep of the profile face input along the path input.
    ///
    /// `face_materials` assigns materials to *generated* faces by
    /// provenance target (docs/ARCHITECTURE.md §§3.4–3.5): a concrete
    /// path (`FaceTarget::One` — e.g. paint `CapEnd`) or a live query
    /// set (`FaceTarget::Set` — e.g. paint `SideFaces { HolesOnly }`,
    /// which re-expands on every evaluation so later-added holes are
    /// painted automatically). Kept sorted (One before Set = explicit
    /// paints take precedence); the referenced material ids are mirrored
    /// in the entity's `face_materials` slot so they are real graph
    /// edges (reject-if-dependents, dirty propagation).
    Extrusion {
        face_materials: Vec<(FaceTarget, EntityId)>,
    },
    /// Revolution of the profile face input about the axis line input by
    /// `angle_radians` (default 2π = closed solid of revolution; angles
    /// are radians per docs/ARCHITECTURE.md §7). `face_materials` as on
    /// `Extrusion`.
    Revolve {
        angle_radians: f64,
        face_materials: Vec<(FaceTarget, EntityId)>,
    },
    /// Chamfer of `distance` meters over edges of its target solid
    /// producer. Edges are addressed by `sub_edges` — concrete
    /// provenance-named `SubRef`s and/or live `SubRefSet` queries, both
    /// owned by the target (docs/ARCHITECTURE.md §§3.4–3.5) — and/or by
    /// the entity's edges slot (authored `Edge` entities matched by
    /// curve coincidence, or a `Selection`). Query targets that expand
    /// to nothing are pass-through no-ops, not errors.
    Chamfer {
        distance: f64,
        sub_edges: Vec<EdgeTarget>,
    },
    /// Axis-aligned section box (min/max corners in meters). Stays out of
    /// the dependency graph by design (docs/ARCHITECTURE.md §14).
    SectionBox { min: [f64; 3], max: [f64; 3] },
    /// A named group of entities — the reusable definition.
    Element { name: String },
    /// Placement of an `Element`: rigid 4x3 transform, row-major
    /// `[r00 r01 r02 tx, r10 r11 r12 ty, r20 r21 r22 tz]`.
    Instance { transform: [f64; 12] },
    /// A query entity (docs/ARCHITECTURE.md §3.5). `frozen` selections
    /// keep their last resolved membership (snapshot semantics).
    Selection {
        predicate: PredicateAst,
        scope: SelectionScope,
        frozen: bool,
    },
}

impl Params {
    /// The entity kind these params belong to.
    pub fn kind(&self) -> EntityKind {
        match self {
            Params::ControlPoint { .. } => EntityKind::ControlPoint,
            Params::Plane { .. } => EntityKind::Plane,
            Params::Circle { .. } => EntityKind::Circle,
            Params::Line => EntityKind::Line,
            Params::Spline { .. } => EntityKind::Spline,
            Params::Edge => EntityKind::Edge,
            Params::Wire => EntityKind::Wire,
            Params::Face => EntityKind::Face,
            Params::Solid => EntityKind::Solid,
            Params::Material { .. } => EntityKind::Material,
            Params::Extrusion { .. } => EntityKind::Extrusion,
            Params::Revolve { .. } => EntityKind::Revolve,
            Params::Chamfer { .. } => EntityKind::Chamfer,
            Params::SectionBox { .. } => EntityKind::SectionBox,
            Params::Element { .. } => EntityKind::Element,
            Params::Instance { .. } => EntityKind::Instance,
            Params::Selection { .. } => EntityKind::Selection,
        }
    }
}

/// The value stored in one slot of an entity record.
///
/// Single slots hold `One(Option<EntityId>)` (None = empty optional slot);
/// multi slots hold `Many(Vec<EntityId>)`. The `Rewire` delta replaces a
/// whole slot value (old + new), so multi-slot edits invert mechanically
/// like everything else (docs/ARCHITECTURE.md §4.1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum SlotValue {
    One(Option<EntityId>),
    Many(Vec<EntityId>),
}

impl SlotValue {
    /// Iterate the entity ids referenced by this slot value.
    pub fn referenced(&self) -> impl Iterator<Item = EntityId> + '_ {
        let (single, many) = match self {
            SlotValue::One(opt) => (opt.as_slice(), [].as_slice()),
            SlotValue::Many(ids) => ([].as_slice(), ids.as_slice()),
        };
        single.iter().chain(many.iter()).copied()
    }

    /// True when the slot holds no reference at all.
    pub fn is_empty(&self) -> bool {
        match self {
            SlotValue::One(opt) => opt.is_none(),
            SlotValue::Many(ids) => ids.is_empty(),
        }
    }
}

/// Static declaration of one input slot (docs/ARCHITECTURE.md §3.2).
#[derive(Debug, Clone, Copy)]
pub struct SlotDecl {
    /// Semantic name (for diagnostics and the future FFI surface).
    pub name: &'static str,
    /// Entity kinds this slot accepts.
    pub accepted: &'static [EntityKind],
    /// Required: single slots must be `Some`, multi slots non-empty.
    pub required: bool,
    /// Multi slots hold `SlotValue::Many`; single slots `SlotValue::One`.
    pub multi: bool,
}

impl SlotDecl {
    /// Whether this slot accepts entities of `kind`.
    pub fn accepts(&self, kind: EntityKind) -> bool {
        self.accepted.contains(&kind)
    }
}

/// Well-known slot indices, kept next to the tables below so command
/// compilation never uses bare numbers.
pub mod slot {
    pub const CIRCLE_CENTER: usize = 0;
    pub const CIRCLE_PLANE: usize = 1;
    pub const LINE_START: usize = 0;
    pub const LINE_END: usize = 1;
    pub const SPLINE_CONTROL_POINTS: usize = 0;
    pub const EDGE_CURVE: usize = 0;
    pub const WIRE_EDGES: usize = 0;
    pub const FACE_OUTER: usize = 0;
    pub const FACE_HOLES: usize = 1;
    pub const FACE_MATERIAL: usize = 2;
    pub const FACE_PLANE: usize = 3;
    pub const SOLID_FACES: usize = 0;
    pub const EXTRUSION_PROFILE: usize = 0;
    pub const EXTRUSION_PATH: usize = 1;
    pub const EXTRUSION_FACE_MATERIALS: usize = 2;
    pub const REVOLVE_PROFILE: usize = 0;
    pub const REVOLVE_AXIS: usize = 1;
    pub const REVOLVE_FACE_MATERIALS: usize = 2;
    pub const CHAMFER_TARGET: usize = 0;
    pub const CHAMFER_EDGES: usize = 1;
    pub const ELEMENT_MEMBERS: usize = 0;
    pub const INSTANCE_ELEMENT: usize = 0;
}

const NO_SLOTS: &[SlotDecl] = &[];

const CIRCLE_SLOTS: &[SlotDecl] = &[
    SlotDecl {
        name: "center",
        accepted: &[EntityKind::ControlPoint],
        required: true,
        multi: false,
    },
    SlotDecl {
        name: "plane",
        accepted: &[EntityKind::Plane],
        required: false,
        multi: false,
    },
];

const LINE_SLOTS: &[SlotDecl] = &[
    SlotDecl {
        name: "start",
        accepted: &[EntityKind::ControlPoint],
        required: true,
        multi: false,
    },
    SlotDecl {
        name: "end",
        accepted: &[EntityKind::ControlPoint],
        required: true,
        multi: false,
    },
];

const SPLINE_SLOTS: &[SlotDecl] = &[SlotDecl {
    name: "control_points",
    accepted: &[EntityKind::ControlPoint],
    required: true,
    multi: true,
}];

const EDGE_SLOTS: &[SlotDecl] = &[SlotDecl {
    name: "curve",
    accepted: &[EntityKind::Line, EntityKind::Spline, EntityKind::Circle],
    required: true,
    multi: false,
}];

const WIRE_SLOTS: &[SlotDecl] = &[SlotDecl {
    name: "edges",
    accepted: &[EntityKind::Edge],
    required: true,
    multi: true,
}];

const FACE_SLOTS: &[SlotDecl] = &[
    SlotDecl {
        name: "outer",
        accepted: &[EntityKind::Wire],
        required: true,
        multi: false,
    },
    SlotDecl {
        name: "holes",
        accepted: &[EntityKind::Wire],
        required: false,
        multi: true,
    },
    SlotDecl {
        name: "material",
        accepted: &[EntityKind::Material],
        required: false,
        multi: false,
    },
    // Optional explicit surface: when wired, the plane is the face's
    // surface; when absent, the evaluator infers a planar surface from
    // the outer wire (evaluation-time semantics; structural here).
    SlotDecl {
        name: "plane",
        accepted: &[EntityKind::Plane],
        required: false,
        multi: false,
    },
];

const SOLID_SLOTS: &[SlotDecl] = &[SlotDecl {
    name: "faces",
    accepted: &[EntityKind::Face],
    required: true,
    multi: true,
}];

// Mirror of the material ids in `Params::Extrusion::face_materials` /
// `Params::Revolve::face_materials`: the slot makes the assignments real
// graph edges (kept in sync by the `UpdateSubFaceMaterial` command).
const FACE_MATERIALS_SLOT: SlotDecl = SlotDecl {
    name: "face_materials",
    accepted: &[EntityKind::Material],
    required: false,
    multi: true,
};

const EXTRUSION_SLOTS: &[SlotDecl] = &[
    SlotDecl {
        name: "profile",
        accepted: &[EntityKind::Face],
        required: true,
        multi: false,
    },
    SlotDecl {
        name: "path",
        accepted: &[EntityKind::Line, EntityKind::Spline],
        required: true,
        multi: false,
    },
    FACE_MATERIALS_SLOT,
];

const REVOLVE_SLOTS: &[SlotDecl] = &[
    SlotDecl {
        name: "profile",
        accepted: &[EntityKind::Face],
        required: true,
        multi: false,
    },
    // The axis is a straight line by construction: origin = line start,
    // direction = start -> end (docs/ARCHITECTURE.md §3.1).
    SlotDecl {
        name: "axis",
        accepted: &[EntityKind::Line],
        required: true,
        multi: false,
    },
    FACE_MATERIALS_SLOT,
];

// Chamfer: slot 0 targets the solid producer whose edges are blended
// (the chamfer replaces the target as mesh owner — eval layer); slot 1
// optionally holds authored `Edge` entities (matched by curve
// coincidence) and/or a `Selection` (docs/ARCHITECTURE.md §3.5).
// Generated edges are addressed via `Params::Chamfer::sub_edges`.
const CHAMFER_SLOTS: &[SlotDecl] = &[
    SlotDecl {
        name: "target",
        accepted: &[
            EntityKind::Extrusion,
            EntityKind::Revolve,
            EntityKind::Solid,
            EntityKind::Chamfer,
        ],
        required: true,
        multi: false,
    },
    SlotDecl {
        name: "edges",
        accepted: &[EntityKind::Edge, EntityKind::Selection],
        required: false,
        multi: true,
    },
];

const ELEMENT_SLOTS: &[SlotDecl] = &[SlotDecl {
    name: "members",
    accepted: &[
        EntityKind::Solid,
        EntityKind::Extrusion,
        EntityKind::Revolve,
        EntityKind::Chamfer,
    ],
    required: true,
    multi: true,
}];

const INSTANCE_SLOTS: &[SlotDecl] = &[SlotDecl {
    name: "element",
    accepted: &[EntityKind::Element],
    required: true,
    multi: false,
}];

/// The static slot table for an entity kind — the single source of truth
/// for structural validation (docs/ARCHITECTURE.md §3.2).
pub fn slots(kind: EntityKind) -> &'static [SlotDecl] {
    match kind {
        EntityKind::ControlPoint
        | EntityKind::Plane
        | EntityKind::Material
        | EntityKind::SectionBox
        | EntityKind::Selection => NO_SLOTS,
        EntityKind::Circle => CIRCLE_SLOTS,
        EntityKind::Line => LINE_SLOTS,
        EntityKind::Spline => SPLINE_SLOTS,
        EntityKind::Edge => EDGE_SLOTS,
        EntityKind::Wire => WIRE_SLOTS,
        EntityKind::Face => FACE_SLOTS,
        EntityKind::Solid => SOLID_SLOTS,
        EntityKind::Extrusion => EXTRUSION_SLOTS,
        EntityKind::Revolve => REVOLVE_SLOTS,
        EntityKind::Chamfer => CHAMFER_SLOTS,
        EntityKind::Element => ELEMENT_SLOTS,
        EntityKind::Instance => INSTANCE_SLOTS,
    }
}

/// One entity: id, params, and input slots. The `inputs` vector always
/// has exactly one `SlotValue` per declared slot, shape-matched to the
/// declaration (`One` vs `Many`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EntityRecord {
    pub id: EntityId,
    pub params: Params,
    pub inputs: Vec<SlotValue>,
}

impl EntityRecord {
    /// The entity's kind (derived from its params variant).
    pub fn kind(&self) -> EntityKind {
        self.params.kind()
    }

    /// Iterate every entity id referenced by any input slot.
    pub fn referenced(&self) -> impl Iterator<Item = EntityId> + '_ {
        self.inputs.iter().flat_map(|slot| slot.referenced())
    }

    /// Validate arity, `One`/`Many` shape, and required-ness of `inputs`
    /// against the kind's slot table. Kind-checking of the referenced
    /// entities needs graph access and lives in `graph::GraphState`.
    pub fn validate_shape(&self) -> Result<(), VimStatus> {
        let decls = slots(self.kind());
        if self.inputs.len() != decls.len() {
            return Err(VimStatus::SlotShapeMismatch);
        }
        for (decl, value) in decls.iter().zip(self.inputs.iter()) {
            validate_slot_value(decl, value)?;
        }
        Ok(())
    }
}

/// Shape + required-ness check for one slot value against its declaration.
pub fn validate_slot_value(decl: &SlotDecl, value: &SlotValue) -> Result<(), VimStatus> {
    match (decl.multi, value) {
        (false, SlotValue::One(_)) | (true, SlotValue::Many(_)) => {}
        _ => return Err(VimStatus::SlotShapeMismatch),
    }
    if decl.required && value.is_empty() {
        return Err(VimStatus::MissingRequiredSlot);
    }
    Ok(())
}

/// An empty (all-unset) input vector shaped for `kind` — the starting
/// point for building records during command compilation.
pub fn empty_inputs(kind: EntityKind) -> Vec<SlotValue> {
    slots(kind)
        .iter()
        .map(|decl| {
            if decl.multi {
                SlotValue::Many(Vec::new())
            } else {
                SlotValue::One(None)
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL_KINDS: &[EntityKind] = &[
        EntityKind::ControlPoint,
        EntityKind::Plane,
        EntityKind::Circle,
        EntityKind::Line,
        EntityKind::Spline,
        EntityKind::Edge,
        EntityKind::Wire,
        EntityKind::Face,
        EntityKind::Solid,
        EntityKind::Material,
        EntityKind::Extrusion,
        EntityKind::Revolve,
        EntityKind::Chamfer,
        EntityKind::SectionBox,
        EntityKind::Element,
        EntityKind::Instance,
        EntityKind::Selection,
    ];

    #[test]
    fn slot_tables_are_well_formed() {
        for kind in ALL_KINDS {
            for decl in slots(*kind) {
                assert!(!decl.name.is_empty());
                assert!(
                    !decl.accepted.is_empty(),
                    "{kind:?}/{} accepts nothing",
                    decl.name
                );
            }
        }
    }

    #[test]
    fn empty_inputs_match_shape() {
        for kind in ALL_KINDS {
            let inputs = empty_inputs(*kind);
            assert_eq!(inputs.len(), slots(*kind).len());
            for (decl, value) in slots(*kind).iter().zip(inputs.iter()) {
                assert!(validate_slot_value(decl, value).is_ok() || decl.required);
            }
        }
    }

    #[test]
    fn extrusion_slots_match_architecture() {
        let decls = slots(EntityKind::Extrusion);
        let profile = decls.get(slot::EXTRUSION_PROFILE);
        let path = decls.get(slot::EXTRUSION_PATH);
        assert!(profile.is_some_and(|d| d.accepts(EntityKind::Face) && !d.multi));
        assert!(path.is_some_and(|d| {
            d.accepts(EntityKind::Line) && d.accepts(EntityKind::Spline) && !d.multi
        }));
    }

    #[test]
    fn revolve_slots_match_architecture() {
        let decls = slots(EntityKind::Revolve);
        let profile = decls.get(slot::REVOLVE_PROFILE);
        let axis = decls.get(slot::REVOLVE_AXIS);
        assert!(profile.is_some_and(|d| {
            d.accepts(EntityKind::Face) && d.required && !d.multi
        }));
        assert!(axis.is_some_and(|d| {
            d.accepts(EntityKind::Line)
                && !d.accepts(EntityKind::Spline)
                && d.required
                && !d.multi
        }));
        assert_eq!(
            Params::Revolve {
                angle_radians: std::f64::consts::TAU,
                face_materials: vec![]
            }
            .kind(),
            EntityKind::Revolve
        );
    }

    #[test]
    fn chamfer_targets_a_producer_and_accepts_edges_and_selections() {
        let decls = slots(EntityKind::Chamfer);
        let target = decls.get(slot::CHAMFER_TARGET);
        assert!(target.is_some_and(|d| {
            !d.multi
                && d.required
                && d.accepts(EntityKind::Extrusion)
                && d.accepts(EntityKind::Revolve)
                && d.accepts(EntityKind::Solid)
                && d.accepts(EntityKind::Chamfer)
        }));
        let edges = decls.get(slot::CHAMFER_EDGES);
        assert!(edges.is_some_and(|d| {
            d.multi
                && !d.required
                && d.accepts(EntityKind::Edge)
                && d.accepts(EntityKind::Selection)
        }));
    }

    #[test]
    fn sweep_producers_have_a_face_materials_slot() {
        for kind in [EntityKind::Extrusion, EntityKind::Revolve] {
            let idx = if kind == EntityKind::Extrusion {
                slot::EXTRUSION_FACE_MATERIALS
            } else {
                slot::REVOLVE_FACE_MATERIALS
            };
            let decl = slots(kind).get(idx);
            assert!(decl.is_some_and(|d| {
                d.multi && !d.required && d.accepts(EntityKind::Material)
            }));
        }
    }

    #[test]
    fn params_kind_is_consistent() {
        assert_eq!(
            Params::ControlPoint {
                position: [0.0; 3]
            }
            .kind(),
            EntityKind::ControlPoint
        );
        assert_eq!(
            Params::Extrusion {
                face_materials: vec![]
            }
            .kind(),
            EntityKind::Extrusion
        );
    }
}

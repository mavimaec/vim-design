//! Edit Mode: interactive editing of an element's 2D profile — faces
//! (closed loops of shared points) that are either SOLID (with their own
//! thickness) or VOID (cut from the solids, to a depth or through).
//!
//! Layering:
//! - [`ProfileModel`] is the adapter to the profile data: a plain-data
//!   [`ProfileView`] for hit-testing and drawing, and pure [`Edit`] ops
//!   that return a NEW value (never mutate). `profile` implements it for
//!   the library's `Sketch`; the app stores each accepted value with one
//!   `UpdateSketch` — one edit, one undo step.
//! - `interact` holds the screen-space logic: hit-testing, marquee
//!   inclusion, which points a drag moves, and move snapping.
//! - `session` holds Edit Mode's session state: selection, drag, marquee,
//!   defaults for new faces.
//!
//! This is a profile editor, not a general mesh editor: points live on
//! one construction plane and faces are simple polygons.

pub mod interact;
pub mod presets;
pub mod profile;
pub mod session;

use super::geom::{Invalid, P2, signed_area};

pub type PointId = u32;
pub type FaceId = u32;

/// What a face does to the element.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FaceKind {
    /// Adds material: the face extruded by its thickness.
    Solid { thickness: f64 },
    /// Removes material from the solids below the plane, `depth` deep
    /// (`None` = all the way through). May extend outside the solids.
    Void { depth: Option<f64> },
}

impl FaceKind {
    pub fn is_void(self) -> bool {
        matches!(self, FaceKind::Void { .. })
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProfilePoint {
    pub id: PointId,
    pub uv: P2,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProfileFace {
    pub id: FaceId,
    /// Closed loop of point ids.
    pub points: Vec<PointId>,
    pub kind: FaceKind,
}

/// An undirected edge between two points (stored sorted).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EdgeKey(pub PointId, pub PointId);

impl EdgeKey {
    pub fn new(a: PointId, b: PointId) -> Self {
        if a <= b { EdgeKey(a, b) } else { EdgeKey(b, a) }
    }
}

/// Plain-data snapshot of a profile.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProfileView {
    pub points: Vec<ProfilePoint>,
    pub faces: Vec<ProfileFace>,
}

impl ProfileView {
    pub fn point(&self, id: PointId) -> Option<P2> {
        self.points.iter().find(|p| p.id == id).map(|p| p.uv)
    }

    pub fn face(&self, id: FaceId) -> Option<&ProfileFace> {
        self.faces.iter().find(|f| f.id == id)
    }

    /// The face's loop as coordinates (missing points are skipped).
    pub fn polygon(&self, face: &ProfileFace) -> Vec<P2> {
        face.points.iter().filter_map(|id| self.point(*id)).collect()
    }

    /// Every edge once, in first-seen loop order.
    pub fn edges(&self) -> Vec<EdgeKey> {
        let mut seen = std::collections::BTreeSet::new();
        let mut out = Vec::new();
        for f in &self.faces {
            let n = f.points.len();
            for i in 0..n {
                let e = EdgeKey::new(f.points[i], f.points[(i + 1) % n]);
                if e.0 != e.1 && seen.insert(e) {
                    out.push(e);
                }
            }
        }
        out
    }

    /// Absolute area of a face.
    pub fn area(&self, face: &ProfileFace) -> f64 {
        signed_area(&self.polygon(face)).abs()
    }
}

/// One user edit of a profile. Moves of edges and faces move their
/// points; the adapter maps each variant to its store's operation.
#[derive(Debug, Clone, PartialEq)]
pub enum Edit {
    MovePoints { ids: Vec<PointId>, delta: P2 },
    MoveEdges { edges: Vec<EdgeKey>, delta: P2 },
    MoveFaces { faces: Vec<FaceId>, delta: P2 },
    /// A new point on an edge, in every loop that shares it.
    InsertPoint { edge: EdgeKey, uv: P2 },
    AddFace { outline: Vec<P2>, kind: FaceKind },
    /// Split every face the segment crosses from edge to edge.
    SplitFaces { a: P2, b: P2 },
    DeletePoints(Vec<PointId>),
    /// Each edge's two points merge into the first one in loop order.
    DeleteEdges(Vec<EdgeKey>),
    DeleteFaces(Vec<FaceId>),
    SetKind { faces: Vec<FaceId>, kind: FaceKind },
}

/// Why an edit was refused. `message` is the short toast text.
#[derive(Debug, Clone, PartialEq)]
pub enum EditError {
    /// The result would contain an invalid face.
    Invalid(Invalid),
    /// The edit refers to items that do not exist (stale selection).
    Unknown,
    /// The edit would change nothing (e.g. a split line crossing no face).
    NoEffect(&'static str),
}

impl EditError {
    pub fn message(&self) -> &'static str {
        match self {
            EditError::Invalid(i) => i.message(),
            EditError::Unknown => "Those items no longer exist",
            EditError::NoEffect(m) => m,
        }
    }
}

/// The adapter to profile data: a value type with a plain-data view and
/// pure edit operations.
pub trait ProfileModel: Clone {
    fn view(&self) -> ProfileView;
    fn apply(&self, edit: &Edit) -> Result<Self, EditError>;
}

//! The profile adapter over the library's `Sketch`: every edit is one
//! of the library's pure topology operations, followed by the live
//! geometric validation (a self-crossing or zero-area face is refused
//! before it reaches the document). [`apply_wall`] maps the same edits
//! onto `wall::ops` for a wall's anchored profile.

use vim_design_lib::sketch::{self, Sketch, SketchError, SketchFaceKind, ops};

use super::{
    EdgeKey, Edit, EditError, FaceKind, ProfileFace, ProfileModel, ProfilePoint, ProfileView,
};
use crate::authoring::geom::{Invalid, P2};

impl From<SketchFaceKind> for FaceKind {
    fn from(k: SketchFaceKind) -> Self {
        match k {
            SketchFaceKind::Solid { thickness } => FaceKind::Solid { thickness },
            SketchFaceKind::Void { depth } => FaceKind::Void { depth },
        }
    }
}

impl From<FaceKind> for SketchFaceKind {
    fn from(k: FaceKind) -> Self {
        match k {
            FaceKind::Solid { thickness } => SketchFaceKind::Solid { thickness },
            FaceKind::Void { depth } => SketchFaceKind::Void { depth },
        }
    }
}

/// User-facing meaning of a library sketch error.
pub fn edit_error(e: SketchError) -> EditError {
    match e {
        SketchError::SelfIntersecting { .. } => EditError::Invalid(Invalid::SelfIntersecting),
        SketchError::ZeroArea { .. } => EditError::Invalid(Invalid::ZeroArea),
        SketchError::TooFewPoints { .. } => EditError::Invalid(Invalid::TooFewPoints),
        SketchError::NothingToSplit => EditError::NoEffect("Draw the line across a face"),
        SketchError::InvalidParameter => EditError::NoEffect("Press on an edge away from its ends"),
        SketchError::InvalidThickness { .. } | SketchError::InvalidDepth { .. } => {
            EditError::NoEffect("Thickness and depth must be more than zero")
        }
        SketchError::UnknownPoint(_)
        | SketchError::UnknownFace(_)
        | SketchError::UnknownEdge(..)
        | SketchError::MissingPoint { .. } => EditError::Unknown,
        SketchError::NonFinite
        | SketchError::DuplicatePointId(_)
        | SketchError::DuplicateFaceId(_)
        | SketchError::IdSpaceExhausted => EditError::NoEffect("That edit is not possible"),
    }
}

/// A sketch from faces given as outlines (coincident corners become
/// shared points).
pub fn sketch_from_faces(faces: &[(Vec<P2>, FaceKind)]) -> Result<Sketch, EditError> {
    let mut s = Sketch::default();
    for (outline, kind) in faces {
        s = ops::add_face(&s, outline, (*kind).into()).map_err(edit_error)?;
    }
    Ok(s)
}

fn pairs(edges: &[EdgeKey]) -> Vec<(u32, u32)> {
    edges.iter().map(|e| (e.0, e.1)).collect()
}

/// Where `uv` projects on the edge a-b, as a parameter from `a`.
fn edge_param(a: P2, b: P2, uv: P2) -> f64 {
    let ab = [b[0] - a[0], b[1] - a[1]];
    let len2 = ab[0] * ab[0] + ab[1] * ab[1];
    if len2 <= f64::EPSILON {
        0.5
    } else {
        ((uv[0] - a[0]) * ab[0] + (uv[1] - a[1]) * ab[1]) / len2
    }
}

/// A wall profile edit: `edit` (in EFFECTIVE coordinates, as the user
/// sees the wall at top reference `height`) applied through `wall::ops`
/// to the STORED profile and its top anchors. Returns the stored profile
/// and anchors for one `UpdateWall`.
pub fn apply_wall(
    profile: &Sketch,
    top_points: &[u32],
    height: f64,
    edit: &Edit,
) -> Result<(Sketch, Vec<u32>), EditError> {
    use vim_design_lib::wall::ops as w;
    let (p, t, h) = (profile, top_points, height);
    match edit {
        Edit::MovePoints { ids, delta } => w::move_points(p, t, h, ids, *delta),
        Edit::MoveEdges { edges, delta } => w::move_edges(p, t, h, &pairs(edges), *delta),
        Edit::MoveFaces { faces, delta } => w::move_faces(p, t, h, faces, *delta),
        Edit::InsertPoint { edge, uv } => {
            let eff = vim_design_lib::wall::effective_profile(p, t, h);
            let (a, b) = (eff.uv(edge.0).map_err(edit_error)?, eff.uv(edge.1).map_err(edit_error)?);
            w::insert_point_on_edge(p, t, h, edge.0, edge.1, edge_param(a, b, *uv))
        }
        Edit::AddFace { outline, kind } => w::add_face(p, t, h, outline, (*kind).into()),
        Edit::SplitFaces { a, b } => w::split_faces(p, t, h, *a, *b),
        Edit::DeletePoints(ids) => w::delete_points(p, t, h, ids),
        Edit::DeleteEdges(edges) => w::delete_edges(p, t, h, &pairs(edges)),
        Edit::DeleteFaces(ids) => w::delete_faces(p, t, h, ids),
        // Face kinds do not depend on the anchors (the stored profile is
        // checked for structure only: its geometry is the effective one's).
        Edit::SetKind { faces, kind } => {
            let mut next = p.clone();
            let mut any = false;
            for f in next.faces.iter_mut().filter(|f| faces.contains(&f.id)) {
                f.kind = (*kind).into();
                any = true;
            }
            if !any {
                return Err(EditError::Unknown);
            }
            sketch::validate_structure(&next).map(|_| (next, t.to_vec()))
        }
    }
    .map_err(edit_error)
}

impl ProfileModel for Sketch {
    fn view(&self) -> ProfileView {
        ProfileView {
            points: self.points.iter().map(|p| ProfilePoint { id: p.id, uv: p.uv }).collect(),
            faces: self
                .faces
                .iter()
                .map(|f| ProfileFace { id: f.id, points: f.points.clone(), kind: f.kind.into() })
                .collect(),
        }
    }

    fn apply(&self, edit: &Edit) -> Result<Self, EditError> {
        let next = match edit {
            Edit::MovePoints { ids, delta } => ops::move_points(self, ids, *delta),
            Edit::MoveEdges { edges, delta } => ops::move_edges(self, &pairs(edges), *delta),
            Edit::MoveFaces { faces, delta } => ops::move_faces(self, faces, *delta),
            Edit::InsertPoint { edge, uv } => {
                let (a, b) = (self.uv(edge.0).map_err(edit_error)?, self.uv(edge.1).map_err(edit_error)?);
                ops::insert_point_on_edge(self, edge.0, edge.1, edge_param(a, b, *uv))
            }
            Edit::AddFace { outline, kind } => ops::add_face(self, outline, (*kind).into()),
            Edit::SplitFaces { a, b } => ops::split_faces(self, *a, *b),
            Edit::DeletePoints(ids) => ops::delete_points(self, ids),
            Edit::DeleteEdges(edges) => ops::delete_edges(self, &pairs(edges)),
            Edit::DeleteFaces(ids) => ops::delete_faces(self, ids),
            Edit::SetKind { faces, kind } => {
                let mut s = self.clone();
                let mut any = false;
                for f in s.faces.iter_mut().filter(|f| faces.contains(&f.id)) {
                    f.kind = (*kind).into();
                    any = true;
                }
                if !any {
                    return Err(EditError::Unknown);
                }
                sketch::validate_structure(&s).map(|_| s)
            }
        }
        .map_err(edit_error)?;
        sketch::validate(&next).map_err(edit_error)?;
        Ok(next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOLID: FaceKind = FaceKind::Solid { thickness: 0.3 };

    fn square() -> Sketch {
        sketch_from_faces(&[(vec![[0.0, 0.0], [4.0, 0.0], [4.0, 4.0], [0.0, 4.0]], SOLID)]).expect("square")
    }

    fn corner(s: &Sketch, uv: P2) -> u32 {
        s.points.iter().find(|p| p.uv == uv).map(|p| p.id).expect("corner")
    }

    #[test]
    fn moves_and_live_validation() {
        let s = square();
        let a = corner(&s, [0.0, 0.0]);
        let moved = s.apply(&Edit::MovePoints { ids: vec![a], delta: [-1.0, 0.0] }).expect("move");
        assert_eq!(moved.view().point(a), Some([-1.0, 0.0]));
        // Dragging a corner across the far edge makes a bowtie: refused.
        let b = corner(&s, [4.0, 0.0]);
        let bad = s.apply(&Edit::MovePoints { ids: vec![b], delta: [-6.0, 2.0] });
        assert_eq!(bad, Err(EditError::Invalid(Invalid::SelfIntersecting)));
    }

    #[test]
    fn insert_split_and_the_three_delete_rules() {
        let s = square();
        let (a, b) = (corner(&s, [0.0, 0.0]), corner(&s, [4.0, 0.0]));
        let ins = s.apply(&Edit::InsertPoint { edge: EdgeKey::new(a, b), uv: [1.0, 0.3] }).expect("insert");
        assert_eq!(ins.faces[0].points.len(), 5);
        assert!(ins.points.iter().any(|p| p.uv == [1.0, 0.0]), "projected onto the edge");

        let split = s.apply(&Edit::SplitFaces { a: [2.0, -1.0], b: [2.0, 5.0] }).expect("split");
        assert_eq!(split.faces.len(), 2);
        assert_eq!(split.points.len(), 6, "the chord points are shared");
        assert_eq!(
            s.apply(&Edit::SplitFaces { a: [5.0, 0.0], b: [6.0, 3.0] }),
            Err(EditError::NoEffect("Draw the line across a face"))
        );
        // Delete the shared chord edge: its points merge into one.
        let v = split.view();
        let chord = v
            .edges()
            .into_iter()
            .find(|e| v.point(e.0).is_some_and(|p| p[0] == 2.0) && v.point(e.1).is_some_and(|p| p[0] == 2.0))
            .expect("chord");
        let merged = split.apply(&Edit::DeleteEdges(vec![chord])).expect("merge");
        assert_eq!(merged.points.len(), 5);
        // Delete a face: its private points go, shared ones stay.
        let one = split.apply(&Edit::DeleteFaces(vec![split.faces[0].id])).expect("delete face");
        assert_eq!((one.faces.len(), one.points.len()), (1, 4));
        // Delete a point: the neighbours reconnect.
        let tri = one.apply(&Edit::DeletePoints(vec![one.faces[0].points[0]])).expect("delete point");
        assert_eq!(tri.faces[0].points.len(), 3);
    }

    #[test]
    fn voids_outside_and_kinds() {
        let s = square()
            .apply(&Edit::AddFace {
                outline: vec![[3.0, 1.0], [6.0, 1.0], [6.0, 3.0], [3.0, 3.0]],
                kind: FaceKind::Void { depth: None },
            })
            .expect("void may extend outside");
        let void = s.faces[1].id;
        let s = s
            .apply(&Edit::SetKind { faces: vec![void], kind: FaceKind::Void { depth: Some(0.1) } })
            .expect("depth");
        assert_eq!(s.view().faces[1].kind, FaceKind::Void { depth: Some(0.1) });
        let bad = s.apply(&Edit::SetKind { faces: vec![void], kind: FaceKind::Void { depth: Some(0.0) } });
        assert!(matches!(bad, Err(EditError::NoEffect(_))));
    }

    #[test]
    fn wall_edits_keep_the_top_anchors() {
        let (profile, top) = vim_design_lib::wall::default_profile(4.0, 0.2);
        let h = 2.7;
        // A gable: a point inserted on the top edge follows the top ...
        let edge = EdgeKey::new(2, 3);
        let (p1, t1) = apply_wall(&profile, &top, h, &Edit::InsertPoint { edge, uv: [2.0, 2.7] }).expect("insert");
        let apex = p1.points.iter().map(|p| p.id).find(|id| !profile.points.iter().any(|q| q.id == *id)).expect("new point");
        assert!(t1.contains(&apex), "on an edge between two anchored points");
        // ... and raising it stores its v from the top reference.
        let (p2, t2) = apply_wall(&p1, &t1, h, &Edit::MovePoints { ids: vec![apex], delta: [0.0, 1.0] }).expect("raise");
        assert_eq!(p2.point(apex).map(|p| p.uv), Some([2.0, 1.0]));
        let eff = vim_design_lib::wall::effective_profile(&p2, &t2, h);
        assert_eq!(eff.point(apex).map(|p| p.uv), Some([2.0, 3.7]));
        // A window void keeps its sill when the height changes.
        let window = vec![[1.0, 0.9], [2.0, 0.9], [2.0, 2.1], [1.0, 2.1]];
        let (p3, t3) = apply_wall(&p2, &t2, h, &Edit::AddFace { outline: window, kind: FaceKind::Void { depth: None } }).expect("window");
        assert_eq!(t3, t2, "a window's points are not anchored");
        let taller = vim_design_lib::wall::effective_profile(&p3, &t3, 3.5);
        assert!(taller.points.iter().any(|p| p.uv == [1.0, 0.9]));
        // Kinds change without touching the anchors.
        let (p4, t4) = apply_wall(&p3, &t3, h, &Edit::SetKind { faces: vec![0], kind: FaceKind::Solid { thickness: 0.3 } }).expect("thicker");
        assert_eq!((p4.faces[0].kind, t4), (SketchFaceKind::Solid { thickness: 0.3 }, t3));
    }
}

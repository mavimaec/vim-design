//! Screen-space interaction for Edit Mode: what is under the pointer,
//! what a marquee encloses, which points a drag moves, and where a
//! dragged point snaps. Positions are canvas pixels; `proj` maps profile
//! (u, v) to canvas pixels for the current view.

use std::collections::BTreeSet;

use super::{EdgeKey, FaceId, PointId, ProfileView};
use crate::authoring::geom::{P2, dist, point_in_polygon};
use crate::authoring::snap::{self, SnapInput, SnapResult};

pub type Proj<'a> = &'a dyn Fn(P2) -> Option<[f32; 2]>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectMode {
    Points,
    Edges,
    Faces,
}

impl SelectMode {
    pub fn name(self) -> &'static str {
        match self {
            SelectMode::Points => "points",
            SelectMode::Edges => "edges",
            SelectMode::Faces => "faces",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Hit {
    Point(PointId),
    /// An edge, with the profile point on it nearest the pointer.
    Edge { edge: EdgeKey, uv: P2 },
    Face(FaceId),
}

/// The selected items of the current selection mode.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Selection {
    pub points: BTreeSet<PointId>,
    pub edges: BTreeSet<EdgeKey>,
    pub faces: BTreeSet<FaceId>,
}

impl Selection {
    pub fn is_empty(&self) -> bool {
        self.points.is_empty() && self.edges.is_empty() && self.faces.is_empty()
    }

    pub fn len(&self) -> usize {
        self.points.len() + self.edges.len() + self.faces.len()
    }

    pub fn contains(&self, hit: &Hit) -> bool {
        match hit {
            Hit::Point(id) => self.points.contains(id),
            Hit::Edge { edge, .. } => self.edges.contains(edge),
            Hit::Face(id) => self.faces.contains(id),
        }
    }

    pub fn insert(&mut self, hit: &Hit) {
        match hit {
            Hit::Point(id) => {
                self.points.insert(*id);
            }
            Hit::Edge { edge, .. } => {
                self.edges.insert(*edge);
            }
            Hit::Face(id) => {
                self.faces.insert(*id);
            }
        }
    }

    pub fn toggle(&mut self, hit: &Hit) {
        if self.contains(hit) {
            match hit {
                Hit::Point(id) => {
                    self.points.remove(id);
                }
                Hit::Edge { edge, .. } => {
                    self.edges.remove(edge);
                }
                Hit::Face(id) => {
                    self.faces.remove(id);
                }
            }
        } else {
            self.insert(hit);
        }
    }

    pub fn extend(&mut self, other: &Selection) {
        self.points.extend(&other.points);
        self.edges.extend(&other.edges);
        self.faces.extend(&other.faces);
    }

    /// Forget items that no longer exist.
    pub fn prune(&mut self, view: &ProfileView) {
        self.points.retain(|id| view.point(*id).is_some());
        let edges: BTreeSet<EdgeKey> = view.edges().into_iter().collect();
        self.edges.retain(|e| edges.contains(e));
        self.faces.retain(|id| view.face(*id).is_some());
    }
}

fn px_dist(a: [f32; 2], b: [f32; 2]) -> f32 {
    (a[0] - b[0]).hypot(a[1] - b[1])
}

fn seg_px_dist(p: [f32; 2], a: [f32; 2], b: [f32; 2]) -> f32 {
    let (abx, aby) = (b[0] - a[0], b[1] - a[1]);
    let len2 = abx * abx + aby * aby;
    let t = if len2 <= 1e-9 {
        0.0
    } else {
        (((p[0] - a[0]) * abx + (p[1] - a[1]) * aby) / len2).clamp(0.0, 1.0)
    };
    (p[0] - a[0] - abx * t).hypot(p[1] - a[1] - aby * t)
}

/// Nearest point within `tol` pixels.
pub fn hit_point(view: &ProfileView, proj: Proj, pos: [f32; 2], tol: f32) -> Option<PointId> {
    view.points
        .iter()
        .filter_map(|p| proj(p.uv).map(|s| (px_dist(s, pos), p.id)))
        .filter(|(d, _)| *d <= tol)
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, id)| id)
}

/// Nearest edge within `tol` pixels, with the point on it nearest the
/// pointer (`cursor_uv`, the pointer on the profile plane).
pub fn hit_edge(
    view: &ProfileView,
    proj: Proj,
    pos: [f32; 2],
    tol: f32,
    cursor_uv: Option<P2>,
) -> Option<(EdgeKey, P2)> {
    let mut best: Option<(f32, EdgeKey)> = None;
    for e in view.edges() {
        let (Some(a), Some(b)) = (view.point(e.0), view.point(e.1)) else { continue };
        let (Some(sa), Some(sb)) = (proj(a), proj(b)) else { continue };
        let d = seg_px_dist(pos, sa, sb);
        if d <= tol && best.is_none_or(|(bd, _)| d < bd) {
            best = Some((d, e));
        }
    }
    let (_, e) = best?;
    let (a, b) = (view.point(e.0)?, view.point(e.1)?);
    let c = cursor_uv.unwrap_or([(a[0] + b[0]) / 2.0, (a[1] + b[1]) / 2.0]);
    let ab = [b[0] - a[0], b[1] - a[1]];
    let len2 = (ab[0] * ab[0] + ab[1] * ab[1]).max(1e-12);
    let t = (((c[0] - a[0]) * ab[0] + (c[1] - a[1]) * ab[1]) / len2).clamp(0.0, 1.0);
    Some((e, [a[0] + ab[0] * t, a[1] + ab[1] * t]))
}

/// The face under the pointer: voids first (they sit on top of the
/// solids they cut), then the smallest face (the innermost).
pub fn hit_face(view: &ProfileView, cursor_uv: Option<P2>) -> Option<FaceId> {
    let c = cursor_uv?;
    view.faces
        .iter()
        .filter(|f| point_in_polygon(c, &view.polygon(f)))
        .min_by(|a, b| {
            b.kind
                .is_void()
                .cmp(&a.kind.is_void())
                .then(view.area(a).total_cmp(&view.area(b)))
        })
        .map(|f| f.id)
}

/// What the pointer is over, in the current selection mode.
pub fn hit(
    view: &ProfileView,
    mode: SelectMode,
    proj: Proj,
    pos: [f32; 2],
    tol: f32,
    cursor_uv: Option<P2>,
) -> Option<Hit> {
    match mode {
        SelectMode::Points => hit_point(view, proj, pos, tol).map(Hit::Point),
        SelectMode::Edges => hit_edge(view, proj, pos, tol, cursor_uv).map(|(edge, uv)| Hit::Edge { edge, uv }),
        SelectMode::Faces => hit_face(view, cursor_uv).map(Hit::Face),
    }
}

/// Items of `mode` lying entirely inside the pixel rectangle
/// `[x0, y0, x1, y1]` (any corner order).
pub fn marquee(view: &ProfileView, mode: SelectMode, proj: Proj, rect: [f32; 4]) -> Selection {
    let (x0, x1) = (rect[0].min(rect[2]), rect[0].max(rect[2]));
    let (y0, y1) = (rect[1].min(rect[3]), rect[1].max(rect[3]));
    let inside = |id: PointId| {
        view.point(id)
            .and_then(proj)
            .is_some_and(|s| s[0] >= x0 && s[0] <= x1 && s[1] >= y0 && s[1] <= y1)
    };
    let mut sel = Selection::default();
    match mode {
        SelectMode::Points => sel.points = view.points.iter().map(|p| p.id).filter(|id| inside(*id)).collect(),
        SelectMode::Edges => sel.edges = view.edges().into_iter().filter(|e| inside(e.0) && inside(e.1)).collect(),
        SelectMode::Faces => {
            sel.faces = view
                .faces
                .iter()
                .filter(|f| f.points.iter().all(|id| inside(*id)))
                .map(|f| f.id)
                .collect();
        }
    }
    sel
}

/// The points a drag of the selection moves.
pub fn moving_points(view: &ProfileView, sel: &Selection) -> BTreeSet<PointId> {
    let mut ids: BTreeSet<PointId> = sel.points.clone();
    ids.extend(sel.edges.iter().flat_map(|e| [e.0, e.1]));
    for f in view.faces.iter().filter(|f| sel.faces.contains(&f.id)) {
        ids.extend(f.points.iter().copied());
    }
    ids
}

/// The point a drag snaps with: the grabbed point, or the moving point
/// nearest the press.
pub fn drag_anchor(view: &ProfileView, grabbed: &Hit, moving: &BTreeSet<PointId>, press_uv: P2) -> Option<P2> {
    if let Hit::Point(id) = grabbed {
        return view.point(*id);
    }
    moving
        .iter()
        .filter_map(|id| view.point(*id))
        .min_by(|a, b| dist(*a, press_uv).total_cmp(&dist(*b, press_uv)))
}

/// Snap a dragged anchor: to non-moving points, to horizontal/vertical
/// alignment with where it started, and to the grid.
pub fn snap_move(
    view: &ProfileView,
    moving: &BTreeSet<PointId>,
    anchor_start: P2,
    raw: P2,
    enabled: bool,
    step: f64,
    tolerance: f64,
) -> SnapResult {
    let vertices: Vec<P2> = view.points.iter().filter(|p| !moving.contains(&p.id)).map(|p| p.uv).collect();
    snap::snap(&SnapInput {
        raw,
        enabled,
        step,
        tolerance,
        close_target: None,
        prev: Some(anchor_start),
        first: None,
        vertices: &vertices,
        edges: &[],
        align: &[],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authoring::edit::profile::sketch_from_faces;
    use crate::authoring::edit::{FaceKind, ProfileModel};

    /// 10 px per meter, y down, origin at (100, 100).
    fn proj(p: P2) -> Option<[f32; 2]> {
        Some([100.0 + p[0] as f32 * 10.0, 100.0 - p[1] as f32 * 10.0])
    }

    fn two_squares() -> ProfileView {
        sketch_from_faces(&[
            (vec![[0.0, 0.0], [4.0, 0.0], [4.0, 4.0], [0.0, 4.0]], FaceKind::Solid { thickness: 0.3 }),
            (vec![[1.0, 1.0], [2.0, 1.0], [2.0, 2.0], [1.0, 2.0]], FaceKind::Void { depth: None }),
        ])
        .expect("sketch")
        .view()
    }

    #[test]
    fn hit_testing_per_mode() {
        let v = two_squares();
        let corner = hit(&v, SelectMode::Points, &proj, [141.0, 99.0], 3.0, None);
        assert!(matches!(corner, Some(Hit::Point(id)) if v.point(id) == Some([4.0, 0.0])));
        let edge = hit(&v, SelectMode::Edges, &proj, [120.0, 101.0], 3.0, Some([2.0, -0.1]));
        match edge {
            Some(Hit::Edge { uv, .. }) => assert_eq!(uv, [2.0, 0.0]),
            other => panic!("expected an edge, got {other:?}"),
        }
        // Inside the void: the void wins over the solid under it.
        let face = hit(&v, SelectMode::Faces, &proj, [115.0, 85.0], 3.0, Some([1.5, 1.5]));
        assert_eq!(face, Some(Hit::Face(v.faces[1].id)));
        let solid = hit(&v, SelectMode::Faces, &proj, [135.0, 65.0], 3.0, Some([3.5, 3.5]));
        assert_eq!(solid, Some(Hit::Face(v.faces[0].id)));
        assert_eq!(hit(&v, SelectMode::Points, &proj, [300.0, 300.0], 3.0, None), None);
    }

    #[test]
    fn marquee_selects_items_fully_inside() {
        let v = two_squares();
        // A box around the void only (pixels 105..125 x 75..95).
        let rect = [105.0, 75.0, 125.0, 95.0];
        assert_eq!(marquee(&v, SelectMode::Points, &proj, rect).points.len(), 4);
        assert_eq!(marquee(&v, SelectMode::Edges, &proj, rect).edges.len(), 4);
        let faces = marquee(&v, SelectMode::Faces, &proj, rect).faces;
        assert_eq!(faces.into_iter().collect::<Vec<_>>(), vec![v.faces[1].id]);
        // Reversed corners work the same.
        assert_eq!(marquee(&v, SelectMode::Points, &proj, [125.0, 95.0, 105.0, 75.0]).points.len(), 4);
    }

    #[test]
    fn drag_moves_selection_with_snapping() {
        let v = two_squares();
        let mut sel = Selection::default();
        sel.faces.insert(v.faces[1].id);
        let moving = moving_points(&v, &sel);
        assert_eq!(moving.len(), 4);
        let grabbed = Hit::Face(v.faces[1].id);
        let anchor = drag_anchor(&v, &grabbed, &moving, [1.9, 1.9]).expect("anchor");
        assert_eq!(anchor, [2.0, 2.0]);
        // Dragging near the outer corner snaps onto it.
        let s = snap_move(&v, &moving, anchor, [3.9, 3.95], true, 0.5, 0.2);
        assert_eq!(s.point, [4.0, 4.0]);
        // Otherwise the grid (and axis alignment with the start).
        let s = snap_move(&v, &moving, anchor, [2.7, 2.1], true, 0.5, 0.2);
        assert_eq!(s.point, [2.5, 2.0]);
    }

    #[test]
    fn long_press_on_edge_inserts_a_point() {
        let m = sketch_from_faces(&[(
            vec![[0.0, 0.0], [4.0, 0.0], [4.0, 4.0], [0.0, 4.0]],
            FaceKind::Solid { thickness: 0.3 },
        )])
        .expect("sketch");
        let v = m.view();
        let (edge, uv) = hit_edge(&v, &proj, [130.0, 101.5], 4.0, Some([3.0, -0.15])).expect("edge");
        let m = m.apply(&crate::authoring::edit::Edit::InsertPoint { edge, uv }).expect("insert");
        assert_eq!(m.view().faces[0].points.len(), 5);
        assert!(m.view().points.iter().any(|p| p.uv == [3.0, 0.0]));
    }
}

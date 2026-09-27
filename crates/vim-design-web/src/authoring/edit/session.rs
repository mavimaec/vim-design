//! Edit Mode session state: the profile being edited, selection, drag,
//! marquee, and the defaults for new faces. Session state only — the
//! profile itself lives in the document; the app writes every accepted
//! edit there and hands the stored value back with `set_model`.

use std::collections::BTreeSet;

use super::interact::{Hit, SelectMode, Selection};
use super::{Edit, EditError, FaceKind, PointId, ProfileModel, ProfileView};
use crate::authoring::geom::P2;
use vim_design_lib::EntityId;

/// Default thickness of new solid faces and depth of new voids (m).
pub const DEFAULT_SOLID_THICKNESS_M: f64 = 0.3;
pub const DEFAULT_VOID_DEPTH_M: f64 = 0.1;

/// An in-progress drag of the selection.
#[derive(Debug, Clone)]
pub struct Drag<P> {
    pub moving: BTreeSet<PointId>,
    /// The snapping point's position when the drag began.
    pub anchor: P2,
    pub delta: P2,
    /// The profile with the move applied (Err: the move is invalid).
    pub preview: Result<P, EditError>,
}

/// An in-progress marquee (canvas pixels).
#[derive(Debug, Clone)]
pub struct Marquee {
    pub rect: [f32; 4],
    pub additive: bool,
    /// Selection when the marquee began (kept when additive).
    pub base: Selection,
}

#[derive(Debug, Clone)]
pub struct EditSession<P: ProfileModel> {
    /// The element being edited (`None`: a new element not created yet).
    pub element: Option<EntityId>,
    pub name: String,
    /// The construction plane the profile lives on.
    pub level: EntityId,
    pub model: P,
    pub mode: SelectMode,
    pub selection: Selection,
    pub hover: Option<Hit>,
    pub drag: Option<Drag<P>>,
    pub marquee: Option<Marquee>,
    /// Defaults for new faces.
    pub new_thickness: f64,
    pub new_void_depth: f64,
    pub new_void_through: bool,
    /// Wall profiles: points anchored to the wall's TOP reference (they
    /// follow the wall height) — a mirror of the wall's `top_points`,
    /// reloaded with the profile. Unused for floor plates.
    pub top_points: BTreeSet<PointId>,
}

impl<P: ProfileModel> EditSession<P> {
    pub fn new(model: P, level: EntityId, element: Option<EntityId>, name: String) -> Self {
        Self {
            element,
            name,
            level,
            model,
            mode: SelectMode::Faces,
            selection: Selection::default(),
            hover: None,
            drag: None,
            marquee: None,
            new_thickness: DEFAULT_SOLID_THICKNESS_M,
            new_void_depth: DEFAULT_VOID_DEPTH_M,
            new_void_through: true,
            top_points: BTreeSet::new(),
        }
    }

    /// The profile to draw: the drag preview when valid.
    pub fn view(&self) -> ProfileView {
        match &self.drag {
            Some(Drag { preview: Ok(p), .. }) => p.view(),
            _ => self.model.view(),
        }
    }

    pub fn new_void_kind(&self) -> FaceKind {
        FaceKind::Void { depth: (!self.new_void_through).then_some(self.new_void_depth) }
    }

    pub fn new_solid_kind(&self) -> FaceKind {
        FaceKind::Solid { thickness: self.new_thickness }
    }

    /// Make a stored profile current (after an edit is written to the
    /// document, or after undo/redo): forget vanished selection items.
    pub fn set_model(&mut self, model: P) {
        self.model = model;
        self.drag = None;
        let view = self.model.view();
        self.selection.prune(&view);
        self.top_points.retain(|id| view.point(*id).is_some());
        if self.hover.is_some() {
            self.hover = None;
        }
    }

    /// Switch the selection mode (the selection is cleared).
    pub fn set_mode(&mut self, mode: SelectMode) {
        if self.mode != mode {
            self.mode = mode;
            self.selection = Selection::default();
            self.hover = None;
        }
    }

    /// Tap: select the item (additive toggles it), or clear on empty.
    pub fn tap(&mut self, hit: Option<Hit>, additive: bool) {
        match (hit, additive) {
            (Some(h), true) => self.selection.toggle(&h),
            (Some(h), false) => {
                self.selection = Selection::default();
                self.selection.insert(&h);
            }
            (None, true) => {}
            (None, false) => self.selection = Selection::default(),
        }
    }

    /// Start dragging `grabbed`: it joins the selection (replacing it
    /// unless it is already selected or `additive`).
    pub fn begin_drag(&mut self, grabbed: &Hit, anchor: P2, additive: bool) {
        if !self.selection.contains(grabbed) {
            if !additive {
                self.selection = Selection::default();
            }
            self.selection.insert(grabbed);
        }
        let moving = super::interact::moving_points(&self.model.view(), &self.selection);
        self.drag = Some(Drag { moving, anchor, delta: [0.0, 0.0], preview: Ok(self.model.clone()) });
    }

    /// The edit that moves the current selection by `delta`.
    pub fn move_edit(&self, delta: P2) -> Edit {
        match self.mode {
            SelectMode::Points => Edit::MovePoints { ids: self.selection.points.iter().copied().collect(), delta },
            SelectMode::Edges => Edit::MoveEdges { edges: self.selection.edges.iter().copied().collect(), delta },
            SelectMode::Faces => Edit::MoveFaces { faces: self.selection.faces.iter().copied().collect(), delta },
        }
    }

    pub fn update_drag(&mut self, delta: P2) {
        let edit = self.move_edit(delta);
        let preview = self.model.apply(&edit);
        if let Some(d) = self.drag.as_mut() {
            d.delta = delta;
            d.preview = preview;
        }
    }

    /// Release: returns the move edit to accept, or why it snaps back.
    /// `Ok(None)`: nothing moved.
    pub fn end_drag(&mut self) -> Result<Option<Edit>, EditError> {
        let Some(d) = self.drag.take() else { return Ok(None) };
        if d.delta[0].abs() < 1e-9 && d.delta[1].abs() < 1e-9 {
            return Ok(None);
        }
        d.preview?;
        Ok(Some(self.move_edit(d.delta)))
    }

    /// The points of the selection (a point, an edge's two points, or a
    /// face's loop) — what an anchor change applies to.
    pub fn selected_points(&self) -> BTreeSet<PointId> {
        super::interact::moving_points(&self.model.view(), &self.selection)
    }

    /// The edit deleting the current selection.
    pub fn delete_edit(&self) -> Option<Edit> {
        if self.selection.is_empty() {
            return None;
        }
        Some(match self.mode {
            SelectMode::Points => Edit::DeletePoints(self.selection.points.iter().copied().collect()),
            SelectMode::Edges => Edit::DeleteEdges(self.selection.edges.iter().copied().collect()),
            SelectMode::Faces => Edit::DeleteFaces(self.selection.faces.iter().copied().collect()),
        })
    }

    /// Kinds of the selected faces (Faces mode only).
    pub fn selected_kinds(&self) -> Vec<FaceKind> {
        let view = self.model.view();
        view.faces
            .iter()
            .filter(|f| self.selection.faces.contains(&f.id))
            .map(|f| f.kind)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authoring::edit::profile::sketch_from_faces;
    use vim_design_lib::sketch::Sketch;

    fn session() -> EditSession<Sketch> {
        let m = sketch_from_faces(&[(
            vec![[0.0, 0.0], [4.0, 0.0], [4.0, 4.0], [0.0, 4.0]],
            FaceKind::Solid { thickness: 0.3 },
        )])
        .expect("sketch");
        EditSession::new(m, EntityId(1), None, "Floor plate 1".into())
    }

    fn corner(s: &EditSession<Sketch>, uv: P2) -> u32 {
        s.model.view().points.iter().find(|p| p.uv == uv).map(|p| p.id).expect("corner")
    }

    #[test]
    fn drag_preview_rejects_invalid_moves() {
        let mut s = session();
        s.set_mode(SelectMode::Points);
        let c = corner(&s, [4.0, 0.0]);
        s.begin_drag(&Hit::Point(c), [4.0, 0.0], false);
        s.update_drag([-6.0, 2.0]);
        assert!(s.drag.as_ref().is_some_and(|d| d.preview.is_err()), "bowtie preview");
        assert!(s.end_drag().is_err(), "rejected on release: snaps back");
        assert_eq!(s.model.view().point(c), Some([4.0, 0.0]));
        s.begin_drag(&Hit::Point(c), [4.0, 0.0], false);
        s.update_drag([1.0, 0.0]);
        let edit = s.end_drag().expect("valid").expect("moved");
        let next = s.model.apply(&edit).expect("apply");
        s.set_model(next);
        assert_eq!(s.model.view().point(c), Some([5.0, 0.0]));
    }

    #[test]
    fn tap_select_and_delete_prunes_selection() {
        let mut s = session();
        let f = s.model.view().faces[0].id;
        s.tap(Some(Hit::Face(f)), false);
        assert_eq!(s.selection.len(), 1);
        let del = s.delete_edit().expect("delete");
        let next = s.model.apply(&del).expect("apply");
        s.set_model(next);
        assert!(s.selection.is_empty());
        assert!(s.model.view().faces.is_empty());
        s.tap(None, false);
        assert!(s.delete_edit().is_none());
    }

    #[test]
    fn move_edits_follow_the_mode() {
        let mut s = session();
        let c = corner(&s, [0.0, 0.0]);
        s.set_mode(SelectMode::Points);
        s.tap(Some(Hit::Point(c)), false);
        assert!(matches!(s.move_edit([1.0, 0.0]), Edit::MovePoints { .. }));
        s.set_mode(SelectMode::Faces);
        assert!(s.selection.is_empty(), "switching modes clears the selection");
        assert!(matches!(s.move_edit([1.0, 0.0]), Edit::MoveFaces { .. }));
    }

    #[test]
    fn anchors_follow_the_selection_and_the_profile() {
        let mut s = session();
        let top_right = corner(&s, [4.0, 4.0]);
        s.set_mode(SelectMode::Edges);
        let top_left = corner(&s, [0.0, 4.0]);
        s.tap(Some(Hit::Edge { edge: crate::authoring::edit::EdgeKey::new(top_left, top_right), uv: [2.0, 4.0] }), false);
        let ids = s.selected_points();
        assert_eq!(ids.len(), 2, "an edge anchors both its points");
        s.top_points = ids;
        // Deleting a point drops it from the anchors.
        s.set_mode(SelectMode::Points);
        s.tap(Some(Hit::Point(top_left)), false);
        let del = s.delete_edit().expect("delete");
        let next = s.model.apply(&del).expect("apply");
        s.set_model(next);
        assert_eq!(s.top_points.len(), 1);
    }
}

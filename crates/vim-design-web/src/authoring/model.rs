//! The app's element model, DERIVED from the document — never stored
//! beside it (the document is the single source of truth, so undo, redo,
//! reload, and import need no special cases).
//!
//! Recognition rules (structural, from entity kinds + wiring + params):
//! - **Floor plate** = an `Element` whose first member is an `Extrusion`
//!   whose profile `Face` has an outer wire of straight `Line` edges
//!   whose control points are all attached to ONE level at one `w`
//!   (a horizontal profile), and whose path is a vertical line attached
//!   to the same level. Its holes are the face's hole wires (same
//!   rules). Thickness = the path's `w` extent.
//! - **Wall** = an `Element` whose first member is a library `Wall`
//!   entity (a reference line on a construction plane plus an elevation
//!   profile, `vim_design_lib::wall`).
//! - **Legacy wall** = the Element -> Extrusion -> Face chain of the
//!   first wall tool, with a VERTICAL profile: all points attached to one level, lying in one
//!   vertical plane, spanning a `w` range; the path is a horizontal line
//!   across that plane (the thickness). The wall axis (`u`) is the
//!   drawn direction, read from the profile's first edge along the base;
//!   `v` is world up. Its windows are the face's hole wires.
//! - **Sketch plate** = an `Element` whose first member is a `Sketch` on
//!   a construction plane (a level or a workplane) hanging below it: the profile-edited floor plate (faces of
//!   their own thickness, voids). The legacy extrusion plate above stays
//!   recognized for documents from before sketches.
//! - Anything else is listed as a generic element (name + level +
//!   delete).

use vim_design_lib::entity::slot;
use vim_design_lib::sketch::{Sketch, SketchDirection, SketchFaceKind};
use vim_design_lib::{Document, EntityId, EntityKind, Params};

use super::geom::{P2, signed_area};

#[derive(Debug, Clone, PartialEq)]
pub struct HoleModel {
    pub wire: EntityId,
    /// Level-local (u, v) outline, in wire order.
    pub outline: Vec<P2>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PlateModel {
    pub element: EntityId,
    pub name: String,
    /// Association (data): the element's level slot.
    pub level: EntityId,
    /// Attachment (geometry): the level the outline points live on.
    pub plane_level: EntityId,
    /// The profile's `w` on the plane level (0 for app-drawn plates).
    pub top_w: f64,
    pub extrusion: EntityId,
    pub face: EntityId,
    pub path_start: EntityId,
    pub path_end: EntityId,
    /// Positive thickness (meters).
    pub thickness: f64,
    /// True when the path runs downward (the app's convention).
    pub downward: bool,
    /// Level-local (u, v) outline, in wire order.
    pub outline: Vec<P2>,
    pub holes: Vec<HoleModel>,
    /// Net area (outline minus holes), m².
    pub area: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LegacyWallModel {
    pub element: EntityId,
    pub name: String,
    /// Association (data): the element's level slot.
    pub level: EntityId,
    /// Attachment (geometry): the level the profile points live on.
    pub plane_level: EntityId,
    pub extrusion: EntityId,
    pub face: EntityId,
    pub path_start: EntityId,
    pub path_end: EntityId,
    /// Base line on the level (u, v), in the drawn direction.
    pub start: P2,
    pub end: P2,
    /// `w` of the base line on the plane level (0 for app-drawn walls).
    pub base_w: f64,
    pub height: f64,
    pub thickness: f64,
    /// Unit horizontal vector from the reference face into the body.
    pub normal: P2,
    /// Profile control points on the top edge (moved by height edits).
    pub top_cps: Vec<EntityId>,
    /// Profile outline in wall-local coordinates (u along, v up).
    pub profile: Vec<P2>,
    /// Windows (face holes), outlines in wall-local coordinates.
    pub windows: Vec<HoleModel>,
}

impl LegacyWallModel {
    pub fn length(&self) -> f64 {
        super::geom::dist(self.start, self.end)
    }

    /// Unit drawn direction (the wall-local `u` axis).
    pub fn dir(&self) -> P2 {
        let l = self.length().max(1e-12);
        [(self.end[0] - self.start[0]) / l, (self.end[1] - self.start[1]) / l]
    }

    /// Wall-local (u, v) -> level-frame (u, v, w).
    pub fn to_level(&self, p: P2) -> [f64; 3] {
        let d = self.dir();
        [self.start[0] + d[0] * p[0], self.start[1] + d[1] * p[0], self.base_w + p[1]]
    }

    /// Level-frame point -> wall-local (u, v) (projected onto the face).
    pub fn to_local(&self, p: [f64; 3]) -> P2 {
        let d = self.dir();
        [(p[0] - self.start[0]) * d[0] + (p[1] - self.start[1]) * d[1], p[2] - self.base_w]
    }

    /// Top of the highest window above the base (0 without windows).
    pub fn highest_window_top(&self) -> f64 {
        self.windows
            .iter()
            .flat_map(|w| w.outline.iter().map(|p| p[1]))
            .fold(0.0, f64::max)
    }
}

/// A wall of the library's `Wall` entity (the current wall tool).
#[derive(Debug, Clone, PartialEq)]
pub struct WallModel {
    pub element: EntityId,
    pub name: String,
    /// Association (data): the element's level slot.
    pub level: EntityId,
    /// The `Wall` entity.
    pub wall: EntityId,
    /// Construction plane of the base (a level or a workplane) and the
    /// optional top plane (the height follows it).
    pub base: EntityId,
    pub top: Option<EntityId>,
    /// Reference line in the base plane; the material is on its left.
    pub start: P2,
    pub end: P2,
    /// The fixed height (used when `top` is unwired).
    pub height_m: f64,
    pub top_offset_m: f64,
    /// Stored profile and its top-anchored point ids.
    pub profile: Sketch,
    pub top_points: Vec<u32>,
    /// Top reference height H above the base (`wall::wall_top_height`).
    pub top_height: f64,
}

impl WallModel {
    pub fn length(&self) -> f64 {
        super::geom::dist(self.start, self.end)
    }

    pub fn dir(&self) -> P2 {
        let l = self.length().max(1e-12);
        [(self.end[0] - self.start[0]) / l, (self.end[1] - self.start[1]) / l]
    }

    /// Unit vector into the material: the left of start -> end.
    pub fn normal(&self) -> P2 {
        // `+ 0.0` turns a negative zero into zero.
        super::walls::left(self.dir()).map(|c| c + 0.0)
    }

    /// The profile as the user sees and edits it (anchored points raised
    /// by H).
    pub fn effective(&self) -> Sketch {
        vim_design_lib::wall::effective_profile(&self.profile, &self.top_points, self.top_height)
    }

    /// Thickness of the solid faces (the thickest one; 0 without any).
    pub fn thickness(&self) -> f64 {
        self.profile
            .faces
            .iter()
            .filter_map(|f| match f.kind {
                SketchFaceKind::Solid { thickness } => Some(thickness),
                SketchFaceKind::Void { .. } => None,
            })
            .fold(0.0, f64::max)
    }

    /// Highest point of the effective profile (at least H).
    pub fn top_v(&self) -> f64 {
        self.effective().points.iter().map(|p| p.uv[1]).fold(self.top_height, f64::max)
    }
}

/// What the app needs of any wall to face it, pick it, and snap to it:
/// its reference line on a plane, height, thickness side.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WallLine {
    pub element: EntityId,
    /// The construction plane the reference line lies on.
    pub plane: EntityId,
    pub start: P2,
    pub end: P2,
    /// Height of the base line above the plane.
    pub base_w: f64,
    /// Height of the wall's top above its base.
    pub height: f64,
    pub thickness: f64,
    /// Unit horizontal vector from the reference face into the body.
    pub normal: P2,
}

impl WallLine {
    pub fn length(&self) -> f64 {
        super::geom::dist(self.start, self.end)
    }

    pub fn dir(&self) -> P2 {
        let l = self.length().max(1e-12);
        [(self.end[0] - self.start[0]) / l, (self.end[1] - self.start[1]) / l]
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SketchPlateModel {
    pub element: EntityId,
    pub name: String,
    /// Association (data): the element's level slot.
    pub level: EntityId,
    /// The sketch's construction plane (a level or a workplane).
    pub plane_level: EntityId,
    pub sketch_entity: EntityId,
    pub sketch: Sketch,
}

#[derive(Debug, Clone, PartialEq)]
pub struct OtherModel {
    pub element: EntityId,
    pub name: String,
    pub level: Option<EntityId>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ElementModel {
    Plate(PlateModel),
    SketchPlate(SketchPlateModel),
    Wall(WallModel),
    LegacyWall(LegacyWallModel),
    Other(OtherModel),
}

impl ElementModel {
    pub fn element(&self) -> EntityId {
        match self {
            ElementModel::Plate(p) => p.element,
            ElementModel::SketchPlate(p) => p.element,
            ElementModel::Wall(w) => w.element,
            ElementModel::LegacyWall(w) => w.element,
            ElementModel::Other(o) => o.element,
        }
    }

    pub fn name(&self) -> &str {
        match self {
            ElementModel::Plate(p) => &p.name,
            ElementModel::SketchPlate(p) => &p.name,
            ElementModel::Wall(w) => &w.name,
            ElementModel::LegacyWall(w) => &w.name,
            ElementModel::Other(o) => &o.name,
        }
    }

    pub fn level(&self) -> Option<EntityId> {
        match self {
            ElementModel::Plate(p) => Some(p.level),
            ElementModel::SketchPlate(p) => Some(p.level),
            ElementModel::Wall(w) => Some(w.level),
            ElementModel::LegacyWall(w) => Some(w.level),
            ElementModel::Other(o) => o.level,
        }
    }

    pub fn kind_name(&self) -> &'static str {
        match self {
            ElementModel::Plate(_) | ElementModel::SketchPlate(_) => "floor_plate",
            ElementModel::Wall(_) | ElementModel::LegacyWall(_) => "wall",
            ElementModel::Other(_) => "element",
        }
    }

    /// The wall's reference line and extent (walls only).
    pub fn wall_line(&self) -> Option<WallLine> {
        match self {
            ElementModel::Wall(w) => Some(WallLine {
                element: w.element,
                plane: w.base,
                start: w.start,
                end: w.end,
                base_w: 0.0,
                height: w.top_v(),
                thickness: w.thickness(),
                normal: w.normal(),
            }),
            ElementModel::LegacyWall(w) => Some(WallLine {
                element: w.element,
                plane: w.plane_level,
                start: w.start,
                end: w.end,
                base_w: w.base_w,
                height: w.height,
                thickness: w.thickness,
                normal: w.normal,
            }),
            _ => None,
        }
    }
}

fn input_of(doc: &Document, id: EntityId, index: usize) -> Vec<EntityId> {
    doc.entity(id)
        .and_then(|record| record.inputs.get(index))
        .map(|s| s.referenced().collect())
        .unwrap_or_default()
}

fn first_input(doc: &Document, id: EntityId, index: usize) -> Option<EntityId> {
    input_of(doc, id, index).first().copied()
}

fn kind_of(doc: &Document, id: EntityId) -> Option<EntityKind> {
    doc.entity(id).map(|e| e.kind())
}

/// A control point's attachment plane and stored coordinates.
pub fn control_point(doc: &Document, id: EntityId) -> Option<(Option<EntityId>, [f64; 3])> {
    let record = doc.entity(id)?;
    match &record.params {
        Params::ControlPoint { position } => {
            Some((first_input(doc, id, slot::CONTROL_POINT_PLANE), *position))
        }
        _ => None,
    }
}

/// Endpoints of a `Line` entity.
fn line_points(doc: &Document, line: EntityId) -> Option<(EntityId, EntityId)> {
    if kind_of(doc, line)? != EntityKind::Line {
        return None;
    }
    Some((
        first_input(doc, line, slot::LINE_START)?,
        first_input(doc, line, slot::LINE_END)?,
    ))
}

/// The control points of a closed wire of straight edges, chained in
/// loop order (edge orientation may vary).
pub fn wire_loop(doc: &Document, wire: EntityId) -> Option<Vec<EntityId>> {
    if kind_of(doc, wire)? != EntityKind::Wire {
        return None;
    }
    let mut segs: Vec<(EntityId, EntityId)> = Vec::new();
    for edge in input_of(doc, wire, slot::WIRE_EDGES) {
        let curve = first_input(doc, edge, slot::EDGE_CURVE)?;
        segs.push(line_points(doc, curve)?);
    }
    if segs.len() < 3 {
        return None;
    }
    let (a, b) = segs[0];
    let (c, d) = segs[1];
    let start = if b == c || b == d { a } else { b };
    let mut current = if start == a { b } else { a };
    let mut loop_pts = vec![start];
    for &(p, q) in &segs[1..] {
        loop_pts.push(current);
        current = if p == current {
            q
        } else if q == current {
            p
        } else {
            return None;
        };
    }
    (current == start).then_some(loop_pts)
}

/// A wire's outline in the frame of one level: every point must be
/// attached to the same level at the same `w`. Returns (level, w, uv).
fn planar_outline(doc: &Document, wire: EntityId) -> Option<(EntityId, f64, Vec<P2>)> {
    let cps = wire_loop(doc, wire)?;
    let mut level = None;
    let mut w0 = None;
    let mut out = Vec::with_capacity(cps.len());
    for cp in cps {
        let (plane, [u, v, w]) = control_point(doc, cp)?;
        let plane = plane?;
        if *level.get_or_insert(plane) != plane {
            return None;
        }
        if (w - *w0.get_or_insert(w)).abs() > 1e-9 {
            return None;
        }
        out.push([u, v]);
    }
    Some((level?, w0?, out))
}

fn derive_plate(doc: &Document, element: EntityId, name: &str, level: EntityId) -> Option<PlateModel> {
    let extrusion = first_input(doc, element, slot::ELEMENT_MEMBERS)?;
    if kind_of(doc, extrusion)? != EntityKind::Extrusion {
        return None;
    }
    let face = first_input(doc, extrusion, slot::EXTRUSION_PROFILE)?;
    let path = first_input(doc, extrusion, slot::EXTRUSION_PATH)?;
    let outer = first_input(doc, face, slot::FACE_OUTER)?;
    let (plane_level, top_w, outline) = planar_outline(doc, outer)?;
    let (path_start, path_end) = line_points(doc, path)?;
    let (sp, s) = control_point(doc, path_start)?;
    let (ep, e) = control_point(doc, path_end)?;
    if sp != Some(plane_level) || ep != Some(plane_level) {
        return None;
    }
    // A vertical path: no in-plane component.
    if (s[0] - e[0]).abs() > 1e-9 || (s[1] - e[1]).abs() > 1e-9 {
        return None;
    }
    let dw = e[2] - s[2];
    if dw.abs() < 1e-9 {
        return None;
    }
    let mut holes = Vec::new();
    for wire in input_of(doc, face, slot::FACE_HOLES) {
        match planar_outline(doc, wire) {
            Some((l, w, hole)) if l == plane_level && (w - top_w).abs() < 1e-9 => {
                holes.push(HoleModel { wire, outline: hole });
            }
            // A foreign hole shape: still list it (deletable), outline
            // unknown to the app's validation.
            _ => holes.push(HoleModel { wire, outline: Vec::new() }),
        }
    }
    let area = signed_area(&outline).abs()
        - holes.iter().map(|h| signed_area(&h.outline).abs()).sum::<f64>();
    Some(PlateModel {
        element,
        name: name.to_owned(),
        level,
        plane_level,
        top_w,
        extrusion,
        face,
        path_start,
        path_end,
        thickness: dw.abs(),
        downward: dw < 0.0,
        outline,
        holes,
        area,
    })
}

/// A wire's control points with their level-frame coordinates; every
/// point must be attached to the same level.
/// A loop's level and its (control point, level-frame coordinates).
type AttachedLoop = (EntityId, Vec<(EntityId, [f64; 3])>);

fn attached_loop(doc: &Document, wire: EntityId) -> Option<AttachedLoop> {
    let cps = wire_loop(doc, wire)?;
    let mut level = None;
    let mut out = Vec::with_capacity(cps.len());
    for cp in cps {
        let (plane, pos) = control_point(doc, cp)?;
        let plane = plane?;
        if *level.get_or_insert(plane) != plane {
            return None;
        }
        out.push((cp, pos));
    }
    Some((level?, out))
}

fn derive_legacy_wall(doc: &Document, element: EntityId, name: &str, level: EntityId) -> Option<LegacyWallModel> {
    let extrusion = first_input(doc, element, slot::ELEMENT_MEMBERS)?;
    if kind_of(doc, extrusion)? != EntityKind::Extrusion {
        return None;
    }
    let face = first_input(doc, extrusion, slot::EXTRUSION_PROFILE)?;
    let path = first_input(doc, extrusion, slot::EXTRUSION_PATH)?;
    let outer = first_input(doc, face, slot::FACE_OUTER)?;
    let (plane_level, pts) = attached_loop(doc, outer)?;
    let base_w = pts.iter().map(|(_, p)| p[2]).fold(f64::INFINITY, f64::min);
    let top_w = pts.iter().map(|(_, p)| p[2]).fold(f64::NEG_INFINITY, f64::max);
    if top_w - base_w < 1e-6 {
        return None; // horizontal profile: not a wall
    }
    // Wall axis: the first edge, when it runs along the base; otherwise
    // the first two base points in loop order.
    let first_edge = input_of(doc, outer, slot::WIRE_EDGES).first().copied();
    let from_edge = first_edge
        .and_then(|e| first_input(doc, e, slot::EDGE_CURVE))
        .and_then(|c| line_points(doc, c))
        .and_then(|(a, b)| Some((control_point(doc, a)?.1, control_point(doc, b)?.1)))
        .filter(|(a, b)| (a[2] - base_w).abs() < 1e-9 && (b[2] - base_w).abs() < 1e-9);
    let (a, b) = match from_edge {
        Some(ab) => ab,
        None => {
            let mut base = pts.iter().map(|(_, p)| *p).filter(|p| (p[2] - base_w).abs() < 1e-9);
            (base.next()?, base.next()?)
        }
    };
    let (start, end) = ([a[0], a[1]], [b[0], b[1]]);
    let len = super::geom::dist(start, end);
    if len < 1e-6 {
        return None;
    }
    let d = [(end[0] - start[0]) / len, (end[1] - start[1]) / len];
    // Vertical plane: every point projects onto the base line.
    let off_plane = |p: &[f64; 3]| ((p[0] - start[0]) * d[1] - (p[1] - start[1]) * d[0]).abs();
    if pts.iter().any(|(_, p)| off_plane(p) > 1e-6) {
        return None;
    }
    let (path_start, path_end) = line_points(doc, path)?;
    let (sp, s) = control_point(doc, path_start)?;
    let (ep, e) = control_point(doc, path_end)?;
    if sp != Some(plane_level) || ep != Some(plane_level) || (s[2] - e[2]).abs() > 1e-9 {
        return None;
    }
    let v = [e[0] - s[0], e[1] - s[1]];
    let thickness = v[0].hypot(v[1]);
    if thickness < 1e-6 {
        return None;
    }
    let normal = [v[0] / thickness, v[1] / thickness];
    if (normal[0] * d[0] + normal[1] * d[1]).abs() > 1e-6 {
        return None; // thickness not across the wall
    }
    let local = |p: &[f64; 3]| -> P2 {
        [(p[0] - start[0]) * d[0] + (p[1] - start[1]) * d[1], p[2] - base_w]
    };
    let profile: Vec<P2> = pts.iter().map(|(_, p)| local(p)).collect();
    let top_cps = pts
        .iter()
        .filter(|(_, p)| (p[2] - top_w).abs() < 1e-9)
        .map(|(cp, _)| *cp)
        .collect();
    let windows = input_of(doc, face, slot::FACE_HOLES)
        .into_iter()
        .map(|wire| {
            let outline = match attached_loop(doc, wire) {
                Some((l, hole)) if l == plane_level && hole.iter().all(|(_, p)| off_plane(p) < 1e-6) => {
                    hole.iter().map(|(_, p)| local(p)).collect()
                }
                _ => Vec::new(),
            };
            HoleModel { wire, outline }
        })
        .collect();
    Some(LegacyWallModel {
        element,
        name: name.to_owned(),
        level,
        plane_level,
        extrusion,
        face,
        path_start,
        path_end,
        start,
        end,
        base_w,
        height: top_w - base_w,
        thickness,
        normal,
        top_cps,
        profile,
        windows,
    })
}

fn derive_sketch_plate(
    doc: &Document,
    element: EntityId,
    name: &str,
    level: EntityId,
) -> Option<SketchPlateModel> {
    let sketch_entity = first_input(doc, element, slot::ELEMENT_MEMBERS)?;
    let (sketch, direction) = sketch_params(doc, sketch_entity)?;
    let plane_level = first_input(doc, sketch_entity, slot::SKETCH_PLANE)?;
    if direction != SketchDirection::Below
        || !matches!(kind_of(doc, plane_level)?, EntityKind::Level | EntityKind::Workplane)
    {
        return None;
    }
    Some(SketchPlateModel {
        element,
        name: name.to_owned(),
        level,
        plane_level,
        sketch_entity,
        sketch,
    })
}

fn derive_wall(doc: &Document, element: EntityId, name: &str, level: EntityId) -> Option<WallModel> {
    let wall = first_input(doc, element, slot::ELEMENT_MEMBERS)?;
    let Params::Wall { start, end, height_m, top_offset_m, profile, top_points } = &doc.entity(wall)?.params
    else {
        return None;
    };
    Some(WallModel {
        element,
        name: name.to_owned(),
        level,
        wall,
        base: first_input(doc, wall, slot::WALL_BASE)?,
        top: first_input(doc, wall, slot::WALL_TOP),
        start: *start,
        end: *end,
        height_m: *height_m,
        top_offset_m: *top_offset_m,
        profile: profile.clone(),
        top_points: top_points.clone(),
        top_height: vim_design_lib::wall::wall_top_height(doc, wall).unwrap_or(*height_m),
    })
}

/// A sketch entity's profile and direction.
pub fn sketch_params(doc: &Document, id: EntityId) -> Option<(Sketch, SketchDirection)> {
    match doc.entity(id).map(|e| &e.params) {
        Some(Params::Sketch { sketch, direction }) => Some((sketch.clone(), *direction)),
        _ => None,
    }
}

/// Derive the element list from the document, ordered by element id
/// (creation order).
pub fn derive(doc: &Document) -> Vec<ElementModel> {
    doc.entities()
        .filter_map(|(id, record)| match &record.params {
            Params::Element { name } => {
                let level = first_input(doc, *id, slot::ELEMENT_LEVEL);
                let recognized = level.and_then(|l| {
                    derive_plate(doc, *id, name, l)
                        .map(ElementModel::Plate)
                        .or_else(|| derive_sketch_plate(doc, *id, name, l).map(ElementModel::SketchPlate))
                        .or_else(|| derive_wall(doc, *id, name, l).map(ElementModel::Wall))
                        .or_else(|| derive_legacy_wall(doc, *id, name, l).map(ElementModel::LegacyWall))
                });
                Some(recognized.unwrap_or_else(|| {
                    ElementModel::Other(OtherModel { element: *id, name: name.clone(), level })
                }))
            }
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authoring::ops;

    #[test]
    fn plate_with_hole_round_trips_through_the_document() {
        let mut doc = Document::new();
        let ground = ops::seed_new_project(&mut doc).unwrap_or(EntityId::INVALID);
        let outline = [[0.0, 0.0], [0.0, 4.0], [6.0, 4.0], [6.0, 0.0]]; // CW on purpose
        let ids = ops::commit_plate(&mut doc, ground, &outline, 0.3, "Floor plate 1");
        let ids = ids.expect("plate commits");
        let hole = [[1.0, 1.0], [2.0, 1.0], [2.0, 2.0], [1.0, 2.0]];
        let wire = ops::commit_hole(&mut doc, ground, ids.face, &hole).expect("hole commits");

        let model = derive(&doc);
        assert_eq!(model.len(), 1);
        let ElementModel::Plate(p) = &model[0] else {
            panic!("expected a floor plate, got {model:?}");
        };
        assert_eq!(p.element, ids.element);
        assert_eq!(p.level, ground);
        assert_eq!(p.plane_level, ground);
        assert!((p.thickness - 0.3).abs() < 1e-12 && p.downward);
        assert!(signed_area(&p.outline) > 0.0, "outline normalized CCW");
        assert_eq!(p.holes.len(), 1);
        assert_eq!(p.holes[0].wire, wire);
        assert!((p.area - 23.0).abs() < 1e-9);

        // Reload: the derived model is identical.
        let bytes = doc.save().expect("save");
        let loaded = Document::load(&bytes).expect("load");
        assert_eq!(derive(&loaded), model);

        // Delete the hole: the model updates and the hole's private
        // construction geometry is gone.
        let before = doc.entity_count();
        ops::delete_hole(&mut doc, ids.face, wire).expect("delete hole");
        assert_eq!(doc.entity_count(), before - 13, "wire + 4 edges + 4 lines + 4 points");
        let ElementModel::Plate(p) = &derive(&doc)[0] else { panic!() };
        assert!(p.holes.is_empty());
        assert_eq!(ops::next_element_name(&doc, "Floor plate"), "Floor plate 2");

        // Deleting the element sweeps everything but the site + levels.
        ops::delete_element(&mut doc, ids.element).expect("delete element");
        assert!(derive(&doc).is_empty());
        assert_eq!(doc.entity_count(), 3, "site + 2 levels remain");
    }

    #[test]
    fn app_plates_are_level_local_so_elevation_edits_are_transform_only() {
        use vim_design_lib::Command;
        use vim_design_lib::eval::Engine;
        let mut doc = Document::new();
        let ground = ops::seed_new_project(&mut doc).expect("seed");
        let mut engine = Engine::new();
        engine.set_translation_factoring(true);
        let ids = ops::commit_plate(&mut doc, ground, &[[0.0, 0.0], [4.0, 0.0], [4.0, 3.0]], 0.3, "P")
            .expect("plate");
        ops::commit_hole(&mut doc, ground, ids.face, &[[1.0, 0.5], [2.0, 0.5], [2.0, 1.0]])
            .expect("hole");
        engine.evaluate_pending(&mut doc);
        let first = engine.poll_updates(&doc);
        assert_eq!(first.meshes.len(), 1, "one element mesh");
        assert!(first.errors.is_empty(), "{:?}", first.errors);
        doc.submit(Command::UpdateLevel {
            id: ground,
            name: None,
            elevation_m: Some(1.5),
            is_building_story: None,
            color: None,
            extent_m: None,
            coalesce: false,
        })
        .expect("elevation edit");
        engine.evaluate_pending(&mut doc);
        let drag = engine.poll_updates(&doc);
        assert!(drag.meshes.is_empty(), "no re-tessellation");
        assert_eq!(drag.base_transforms.len(), 1);
        assert_eq!(drag.base_transforms[0].transform[11], 1.5);
    }

    #[test]
    fn legacy_walls_and_windows_round_trip_through_the_document() {
        use crate::authoring::walls;
        use vim_design_lib::Command;
        use vim_design_lib::eval::Engine;
        let mut doc = Document::new();
        let ground = ops::seed_new_project(&mut doc).expect("seed");
        let square = [[0.0, 0.0], [4.0, 0.0], [4.0, 3.0], [0.0, 3.0]];
        let segs = walls::wall_segments(&square, true, 0.2, false).expect("segments");
        let ids = ops::commit_legacy_walls(&mut doc, ground, &segs, 2.7, 0.2).expect("walls");
        assert_eq!(ids.len(), 4);
        let model = derive(&doc);
        let walls: Vec<&LegacyWallModel> = model
            .iter()
            .filter_map(|e| if let ElementModel::LegacyWall(w) = e { Some(w) } else { None })
            .collect();
        assert_eq!(walls.len(), 4);
        let names: Vec<&str> = walls.iter().map(|w| w.name.as_str()).collect();
        assert_eq!(names, ["Wall 1", "Wall 2", "Wall 3", "Wall 4"]);
        let bottom = walls.iter().find(|w| w.start[1] == 0.0 && w.end[1] == 0.0).expect("bottom");
        assert!((bottom.height - 2.7).abs() < 1e-12 && (bottom.thickness - 0.2).abs() < 1e-12);
        assert_eq!(bottom.normal, [0.0, 1.0], "inward");
        assert_eq!(bottom.top_cps.len(), 2);
        assert!((bottom.length() - 3.8).abs() < 1e-9);
        // A window in the bottom wall's face.
        let win = [[1.0, 0.9], [2.0, 0.9], [2.0, 2.0], [1.0, 2.0]];
        let pts: Vec<[f64; 3]> = win.iter().map(|p| bottom.to_level(*p)).collect();
        let (face, element) = (bottom.face, bottom.element);
        let wire = ops::commit_window(&mut doc, ground, face, &pts).expect("window");
        let wall = derive(&doc)
            .into_iter()
            .find_map(|e| match e {
                ElementModel::LegacyWall(w) if w.element == element => Some(w),
                _ => None,
            })
            .expect("wall");
        assert_eq!(wall.windows.len(), 1);
        assert_eq!(wall.windows[0].wire, wire);
        for (a, b) in wall.windows[0].outline.iter().zip(win.iter()) {
            assert!((a[0] - b[0]).abs() < 1e-9 && (a[1] - b[1]).abs() < 1e-9);
        }
        assert!((wall.highest_window_top() - 2.0).abs() < 1e-9);
        // The kernel builds the walls (and the window hole) without errors,
        // and an elevation edit is transform-only.
        let mut engine = Engine::new();
        engine.set_translation_factoring(true);
        engine.evaluate_pending(&mut doc);
        let first = engine.poll_updates(&doc);
        assert_eq!(first.meshes.len(), 4);
        assert!(first.errors.is_empty(), "{:?}", first.errors);
        doc.submit(Command::UpdateLevel {
            id: ground,
            name: None,
            elevation_m: Some(0.5),
            is_building_story: None,
            color: None,
            extent_m: None,
            coalesce: false,
        })
        .expect("elevation edit");
        engine.evaluate_pending(&mut doc);
        let drag = engine.poll_updates(&doc);
        assert!(drag.meshes.is_empty());
        assert_eq!(drag.base_transforms.len(), 4);
        // Reload keeps the model; deleting the window sweeps its geometry.
        let loaded = Document::load(&doc.save().expect("save")).expect("load");
        assert_eq!(derive(&loaded), derive(&doc));
        let before = doc.entity_count();
        ops::delete_hole(&mut doc, face, wire).expect("delete window");
        assert_eq!(doc.entity_count(), before - 13);
    }

    #[test]
    fn legacy_plate_converts_to_a_sketch_in_place() {
        use vim_design_lib::eval::Engine;
        let mut doc = Document::new();
        let ground = ops::seed_new_project(&mut doc).expect("seed");
        let ids = ops::commit_plate(&mut doc, ground, &[[0.0, 0.0], [6.0, 0.0], [6.0, 4.0], [0.0, 4.0]], 0.3, "Floor plate 1")
            .expect("plate");
        ops::commit_hole(&mut doc, ground, ids.face, &[[1.0, 1.0], [2.0, 1.0], [2.0, 2.0], [1.0, 2.0]]).expect("hole");
        let ElementModel::Plate(p) = derive(&doc)[0].clone() else { panic!("legacy plate") };
        ops::convert_legacy_plate(&mut doc, &p).expect("convert");
        let model = derive(&doc);
        assert_eq!(model.len(), 1);
        let ElementModel::SketchPlate(sp) = &model[0] else { panic!("sketch plate, got {model:?}") };
        assert_eq!((sp.element, sp.name.as_str(), sp.level), (ids.element, "Floor plate 1", ground));
        assert_eq!(sp.sketch.faces.len(), 2);
        // Only site + 2 levels + element + instance + sketch remain.
        assert_eq!(doc.entity_count(), 6, "the old construction chain is gone");
        let mut engine = Engine::new();
        engine.set_translation_factoring(true);
        engine.evaluate_pending(&mut doc);
        let up = engine.poll_updates(&doc);
        assert!(up.errors.is_empty(), "{:?}", up.errors);
        assert_eq!(up.meshes.len(), 1);
    }

    fn level2(doc: &Document) -> EntityId {
        ops::levels_sorted(doc).iter().find(|l| l.name == "Level 2").map(|l| l.id).expect("level 2")
    }

    fn walls_of(doc: &Document) -> Vec<WallModel> {
        derive(doc)
            .into_iter()
            .filter_map(|e| if let ElementModel::Wall(w) = e { Some(w) } else { None })
            .collect()
    }

    fn set_elevation(doc: &mut Document, level: EntityId, elevation: f64) {
        use vim_design_lib::Command;
        doc.submit(Command::UpdateLevel {
            id: level,
            name: None,
            elevation_m: Some(elevation),
            is_building_story: None,
            color: None,
            extent_m: None,
            coalesce: false,
        })
        .expect("elevation edit");
    }

    #[test]
    fn new_walls_are_library_walls_fixed_or_up_to_a_plane() {
        use crate::authoring::walls;
        use vim_design_lib::eval::Engine;
        let mut doc = Document::new();
        let ground = ops::seed_new_project(&mut doc).expect("seed");
        let upper = level2(&doc);
        let square = [[0.0, 0.0], [4.0, 0.0], [4.0, 3.0], [0.0, 3.0]];
        let segs = walls::wall_segments(&square, true, 0.2, false).expect("segments");
        let fixed = ops::WallHeight { height_m: 2.7, top: None, top_offset_m: 0.0 };
        ops::commit_walls(&mut doc, ground, ground, &segs, fixed, 0.2).expect("walls");
        let ws = walls_of(&doc);
        assert_eq!(ws.len(), 4);
        let bottom = ws.iter().find(|w| w.start[1] == 0.0 && w.end[1] == 0.0).expect("bottom");
        assert_eq!(bottom.normal(), [0.0, 1.0], "an unflipped loop grows inward: material on the left");
        assert!((bottom.top_height - 2.7).abs() < 1e-12 && (bottom.thickness() - 0.2).abs() < 1e-12);
        assert_eq!(bottom.top_points, vec![2, 3]);
        assert!((bottom.length() - 3.8).abs() < 1e-9);

        // A flipped run: the line is stored reversed, the material stays
        // on the flip side.
        let run = [[0.0, 5.0], [4.0, 5.0]];
        let flipped = walls::wall_segments(&run, false, 0.2, true).expect("run");
        let up_to = ops::WallHeight { height_m: 2.7, top: Some(upper), top_offset_m: -0.3 };
        ops::commit_walls(&mut doc, ground, ground, &flipped, up_to, 0.2).expect("flipped wall");
        let w5 = walls_of(&doc).into_iter().find(|w| w.name == "Wall 5").expect("wall 5");
        assert_eq!((w5.start, w5.end), ([4.0, 5.0], [0.0, 5.0]));
        assert_eq!(w5.normal(), flipped[0].normal);
        assert_eq!(w5.top, Some(upper));
        assert!((w5.top_height - 2.7).abs() < 1e-12, "3.0 - 0.3");

        let mut engine = Engine::new();
        engine.set_translation_factoring(true);
        engine.evaluate_pending(&mut doc);
        let first = engine.poll_updates(&doc);
        assert_eq!(first.meshes.len(), 5);
        assert!(first.errors.is_empty(), "{:?}", first.errors);
        // Dragging the top level re-meshes only the wall up to it.
        set_elevation(&mut doc, upper, 3.5);
        engine.evaluate_pending(&mut doc);
        let drag = engine.poll_updates(&doc);
        let remeshed: Vec<EntityId> = drag.meshes.iter().map(|m| m.id).collect();
        assert_eq!(remeshed, vec![w5.element]);
        let w5 = walls_of(&doc).into_iter().find(|w| w.name == "Wall 5").expect("wall 5");
        assert!((w5.top_height - 3.2).abs() < 1e-12);
        // Dragging the base level: fixed walls move by transform only.
        set_elevation(&mut doc, ground, 0.5);
        engine.evaluate_pending(&mut doc);
        let drag = engine.poll_updates(&doc);
        assert_eq!(drag.meshes.len(), 1, "only the wall with a top constraint re-meshes");
        assert_eq!(drag.meshes[0].id, w5.element);
        assert_eq!(drag.base_transforms.len(), 4);
        // Reload keeps the model.
        let loaded = Document::load(&doc.save().expect("save")).expect("load");
        assert_eq!(derive(&loaded), derive(&doc));
    }

    #[test]
    fn legacy_wall_converts_in_place_with_its_windows() {
        use crate::authoring::walls;
        use vim_design_lib::eval::Engine;
        let mut doc = Document::new();
        let ground = ops::seed_new_project(&mut doc).expect("seed");
        // Flipped on purpose: the thickness is on the right of the line.
        let run = [[0.0, 0.0], [4.0, 0.0]];
        let segs = walls::wall_segments(&run, false, 0.2, true).expect("segments");
        ops::commit_legacy_walls(&mut doc, ground, &segs, 2.7, 0.2).expect("legacy wall");
        let ElementModel::LegacyWall(legacy) = derive(&doc)[0].clone() else { panic!("legacy wall") };
        let win = [[1.0, 0.9], [2.0, 0.9], [2.0, 2.0], [1.0, 2.0]];
        let pts: Vec<[f64; 3]> = win.iter().map(|p| legacy.to_level(*p)).collect();
        ops::commit_window(&mut doc, ground, legacy.face, &pts).expect("window");
        let ElementModel::LegacyWall(legacy) = derive(&doc)[0].clone() else { panic!("legacy wall") };
        ops::convert_legacy_wall(&mut doc, &legacy).expect("convert");
        let ws = walls_of(&doc);
        assert_eq!(ws.len(), 1);
        let w = &ws[0];
        assert_eq!((w.element, w.name.as_str(), w.level, w.base, w.top), (legacy.element, "Wall 1", ground, ground, None));
        assert_eq!((w.start, w.end), ([4.0, 0.0], [0.0, 0.0]), "reversed: the material stays on the same side");
        assert_eq!(w.normal(), legacy.normal);
        assert!((w.top_height - 2.7).abs() < 1e-12 && (w.thickness() - 0.2).abs() < 1e-12);
        assert_eq!(w.top_points.len(), 2, "the top corners follow the height");
        let eff = w.effective();
        let void = eff.faces.iter().find(|f| f.kind == SketchFaceKind::Void { depth: None }).expect("window void");
        let mut outline = vim_design_lib::sketch::face_polygon(&eff, void.id).expect("outline");
        outline.sort_by(|a, b| a[0].total_cmp(&b[0]).then(a[1].total_cmp(&b[1])));
        let mirrored = [[2.0, 0.9], [2.0, 2.0], [3.0, 0.9], [3.0, 2.0]];
        for (a, b) in outline.iter().zip(mirrored.iter()) {
            assert!((a[0] - b[0]).abs() < 1e-9 && (a[1] - b[1]).abs() < 1e-9, "{outline:?}");
        }
        // Site + 2 levels + element + wall: the construction chain and the
        // instance are gone.
        assert_eq!(doc.entity_count(), 5);
        let mut engine = Engine::new();
        engine.set_translation_factoring(true);
        engine.evaluate_pending(&mut doc);
        let up = engine.poll_updates(&doc);
        assert!(up.errors.is_empty(), "{:?}", up.errors);
        assert_eq!(up.meshes.len(), 1);
    }

    #[test]
    fn workplanes_hold_floors_and_wall_tops_and_delete_with_their_contents() {
        use crate::authoring::walls;
        use vim_design_lib::Command;
        let mut doc = Document::new();
        let ground = ops::seed_new_project(&mut doc).expect("seed");
        let ceiling = ops::one(
            &mut doc,
            Command::CreateWorkplane {
                parent: ground,
                name: "Ceiling".to_owned(),
                offset_m: 2.4,
                color: [0.2, 0.4, 0.8, 0.3],
                extent_m: 10.0,
            },
        )
        .expect("workplane");
        let nested = ops::one(
            &mut doc,
            Command::CreateWorkplane {
                parent: ceiling,
                name: "Bulkhead".to_owned(),
                offset_m: -0.3,
                color: [0.2, 0.4, 0.8, 0.3],
                extent_m: 10.0,
            },
        )
        .expect("nested");
        assert_eq!(vim_design_lib::workplane::root_level(&doc, nested), Some(ground));
        let wps = ops::workplanes(&doc);
        assert_eq!(wps.iter().map(|w| (w.name.as_str(), w.parent)).collect::<Vec<_>>(), [("Bulkhead", ceiling), ("Ceiling", ground)]);
        // A ceiling plate on the workplane, associated with Ground.
        let (sketch, _) = vim_design_lib::wall::default_profile(3.0, 0.1);
        let (plate, _) = ops::create_sketch_element(&mut doc, ceiling, ground, &sketch, "Ceiling plate").expect("plate");
        let ElementModel::SketchPlate(p) = derive(&doc).into_iter().find(|e| e.element() == plate).expect("plate") else {
            panic!("a sketch plate on a workplane")
        };
        assert_eq!((p.plane_level, p.level), (ceiling, ground));
        // Walls: one on Ground up to the ceiling, one on the bulkhead.
        let seg = walls::wall_segments(&[[0.0, 0.0], [3.0, 0.0]], false, 0.2, false).expect("seg");
        let up_to = ops::WallHeight { height_m: 2.7, top: Some(ceiling), top_offset_m: 0.0 };
        let topped = ops::commit_walls(&mut doc, ground, ground, &seg, up_to, 0.2).expect("topped")[0];
        let fixed = ops::WallHeight { height_m: 1.0, top: None, top_offset_m: 0.0 };
        let on_nested = ops::commit_walls(&mut doc, nested, ground, &seg, fixed, 0.2).expect("on nested")[0];
        let contents = ops::workplane_contents(&doc, ceiling);
        assert_eq!(contents.workplanes, vec![ceiling, nested]);
        assert_eq!(contents.elements.len(), 2, "{contents:?}");
        assert!(contents.elements.contains(&plate) && contents.elements.contains(&on_nested));
        assert_eq!(contents.topped_walls.len(), 1);
        // Plain delete is refused; the cascade takes the contents and
        // turns the topped wall fixed at its current height.
        assert_eq!(doc.submit(Command::DeleteWorkplane { id: ceiling }).err(), Some(vim_design_lib::VimStatus::HasDependents));
        let depth = doc.undo_depth();
        ops::delete_workplane_cascade(&mut doc, ceiling).expect("cascade");
        assert!(ops::workplanes(&doc).is_empty());
        let model = derive(&doc);
        assert_eq!(model.len(), 1);
        let ElementModel::Wall(w) = &model[0] else { panic!("the topped wall stays") };
        assert_eq!((w.element, w.top), (topped, None));
        assert!((w.top_height - 2.4).abs() < 1e-12);
        ops::rollback_to(&mut doc, depth);
        assert_eq!(ops::workplanes(&doc).len(), 2);
        assert_eq!(derive(&doc).len(), 3);
    }

    #[test]
    fn runs_are_recovered_from_their_walls_and_rewritten() {
        use crate::authoring::runs::chain_of;
        use crate::authoring::walls;
        let mut doc = Document::new();
        let ground = ops::seed_new_project(&mut doc).expect("seed");
        let fixed = ops::WallHeight { height_m: 2.7, top: None, top_offset_m: 0.0 };
        // A room drawn clockwise (normalized CCW, grows inward) and a
        // flipped open run of two segments at 60°.
        let room = [[0.0, 0.0], [0.0, 3.0], [4.0, 3.0], [4.0, 0.0]];
        let segs = walls::wall_segments(&room, true, 0.2, false).expect("room");
        ops::commit_walls(&mut doc, ground, ground, &segs, fixed, 0.2).expect("room walls");
        let run = [[10.0, 0.0], [14.0, 0.0], [16.0, 3.4641016151377544]];
        let segs = walls::wall_segments(&run, false, 0.2, true).expect("run");
        ops::commit_walls(&mut doc, ground, ground, &segs, fixed, 0.2).expect("run walls");
        let ws = walls_of(&doc);
        assert_eq!(ws.len(), 6);
        let refs: Vec<&WallModel> = ws.iter().collect();
        for w in &ws[..4] {
            let c = chain_of(&refs, w.element).expect("room chain");
            assert!(c.closed && !c.flip, "{c:?}");
            assert_eq!(c.elements.len(), 4);
            let mut pts = c.points.clone();
            pts.sort_by(|a, b| a[0].total_cmp(&b[0]).then(a[1].total_cmp(&b[1])));
            let near = |a: [f64; 2], b: [f64; 2]| (a[0] - b[0]).abs() < 1e-9 && (a[1] - b[1]).abs() < 1e-9;
            assert!(pts.iter().zip([[0.0, 0.0], [0.0, 3.0], [4.0, 0.0], [4.0, 3.0]]).all(|(a, b)| near(*a, b)), "{pts:?}");
            assert!(signed_area(&c.points) > 0.0, "counter-clockwise");
        }
        // An open run may come back in either direction: drawn flipped,
        // or reversed and unflipped — the same walls.
        let open = chain_of(&refs, ws[5].element).expect("open chain");
        assert!(!open.closed);
        let (mut order, mut expect) = (vec![ws[4].element, ws[5].element], run.to_vec());
        if !open.flip {
            order.reverse();
            expect.reverse();
        }
        assert_eq!(open.elements, order);
        for (a, b) in open.points.iter().zip(expect.iter()) {
            assert!((a[0] - b[0]).abs() < 1e-9 && (a[1] - b[1]).abs() < 1e-9, "{:?}", open.points);
        }

        // Rewrite the room with a corner moved and a point inserted: the
        // four walls are reused (in order), a fifth is created.
        let room_chain = chain_of(&refs, ws[0].element).expect("room");
        let existing: Vec<WallModel> =
            room_chain.elements.iter().filter_map(|e| ws.iter().find(|w| w.element == *e).cloned()).collect();
        let mut pts = room_chain.points.clone();
        pts.insert(1, [(pts[0][0] + pts[1][0]) / 2.0, (pts[0][1] + pts[1][1]) / 2.0 - 1.0]);
        let spec = ops::RunSpec { base: ground, level: ground, points: pts, closed: true, flip: false, thickness: 0.2, height: fixed };
        let out = ops::rewrite_run(&mut doc, &existing, &spec).expect("rewrite");
        assert_eq!(out.len(), 5);
        assert_eq!(&out[..4], &room_chain.elements[..]);
        let after = walls_of(&doc);
        assert_eq!(after.len(), 7);
        let refs: Vec<&WallModel> = after.iter().collect();
        let c = chain_of(&refs, out[0]).expect("rewritten chain");
        assert_eq!((c.elements.len(), c.closed), (5, true));
        // Fewer points: the extra walls are deleted.
        let spec = ops::RunSpec { points: room_chain.points[..3].to_vec(), ..spec };
        let out = ops::rewrite_run(&mut doc, &after.iter().filter(|w| c.elements.contains(&w.element)).cloned().collect::<Vec<_>>(), &spec)
            .expect("shrink");
        assert_eq!(out.len(), 3);
        assert_eq!(walls_of(&doc).len(), 5);
    }
}

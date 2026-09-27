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
//! - Anything else is listed as a generic element (name + level +
//!   delete). Walls (vertical profiles) join the recognized kinds in
//!   Milestone 2.

use vim_design_lib::entity::slot;
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
pub struct OtherModel {
    pub element: EntityId,
    pub name: String,
    pub level: Option<EntityId>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ElementModel {
    Plate(PlateModel),
    Other(OtherModel),
}

impl ElementModel {
    pub fn element(&self) -> EntityId {
        match self {
            ElementModel::Plate(p) => p.element,
            ElementModel::Other(o) => o.element,
        }
    }

    pub fn name(&self) -> &str {
        match self {
            ElementModel::Plate(p) => &p.name,
            ElementModel::Other(o) => &o.name,
        }
    }

    pub fn level(&self) -> Option<EntityId> {
        match self {
            ElementModel::Plate(p) => Some(p.level),
            ElementModel::Other(o) => o.level,
        }
    }

    pub fn kind_name(&self) -> &'static str {
        match self {
            ElementModel::Plate(_) => "floor_plate",
            ElementModel::Other(_) => "element",
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

/// Derive the element list from the document, ordered by element id
/// (creation order).
pub fn derive(doc: &Document) -> Vec<ElementModel> {
    doc.entities()
        .filter_map(|(id, record)| match &record.params {
            Params::Element { name } => {
                let level = first_input(doc, *id, slot::ELEMENT_LEVEL);
                let plate = level.and_then(|l| derive_plate(doc, *id, name, l));
                Some(match plate {
                    Some(p) => ElementModel::Plate(p),
                    None => ElementModel::Other(OtherModel {
                        element: *id,
                        name: name.clone(),
                        level,
                    }),
                })
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
}

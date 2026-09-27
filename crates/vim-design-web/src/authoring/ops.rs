//! Document operations of the authoring layer: every model change the
//! apps make is compiled here into ordinary library commands. Callers
//! wrap each operation in ONE gesture group (one undo step) and roll
//! back on failure.
//!
//! Construction rules:
//! - every control point of an authored element is ATTACHED to the level
//!   it is drawn on, with coordinates stored as the level-frame
//!   (u, v, w) — so a level elevation edit moves the element and, with
//!   translation factoring, is transform-only;
//! - every element is ASSOCIATED with that level (mandatory slot).

use vim_design_lib::entity::slot;
use vim_design_lib::{Command, Document, EntityId, EntityKind, Params, VimStatus};

use vim_design_lib::sketch::{Sketch, SketchDirection};

use super::edit::FaceKind;
use super::edit::profile::sketch_from_faces;
use super::geom::{P2, normalized_ccw};
use super::model::PlateModel;
use super::walls::WallSeg;

/// Site defaults: downtown Montreal. The default lives in the app, not
/// in the library.
pub const DEFAULT_LATITUDE: f64 = 45.5019;
pub const DEFAULT_LONGITUDE: f64 = -73.5674;
pub const DEFAULT_SITE_ELEVATION: f64 = 36.0;
pub const DEFAULT_TRUE_NORTH: f64 = 0.0;

/// Display half-size of level squares (meters).
pub const LEVEL_EXTENT_M: f64 = 10.0;

/// Level colors for the authoring app (RGBA). Distinct hues; add-level
/// cycles through them.
pub const APP_LEVEL_COLORS: [[f32; 4]; 6] = [
    [0.18, 0.50, 0.93, 0.30], // blue    (Ground)
    [0.93, 0.52, 0.16, 0.30], // orange  (Level 2)
    [0.20, 0.68, 0.42, 0.30], // green
    [0.62, 0.36, 0.86, 0.30], // violet
    [0.86, 0.30, 0.44, 0.30], // rose
    [0.10, 0.66, 0.72, 0.30], // teal
];

/// Submit a command that must succeed.
pub fn ok(doc: &mut Document, cmd: Command) -> Result<Vec<EntityId>, String> {
    let label = cmd.label();
    doc.submit(cmd)
        .map(|out| out.created_ids)
        .map_err(|status| format!("{label} rejected: {status:?}"))
}

/// Submit a command that must create exactly one entity.
pub fn one(doc: &mut Document, cmd: Command) -> Result<EntityId, String> {
    let label = cmd.label();
    let ids = ok(doc, cmd)?;
    match ids.as_slice() {
        [id] => Ok(*id),
        other => Err(format!("{label}: expected 1 created id, got {}", other.len())),
    }
}

/// Seed a new authoring project: the Site singleton (Montreal) plus the
/// levels "Ground" (0 m) and "Level 2" (3 m). Returns the Ground id.
pub fn seed_new_project(doc: &mut Document) -> Result<EntityId, String> {
    one(
        doc,
        Command::CreateSite {
            latitude_deg: DEFAULT_LATITUDE,
            longitude_deg: DEFAULT_LONGITUDE,
            elevation_m: DEFAULT_SITE_ELEVATION,
            true_north_deg: DEFAULT_TRUE_NORTH,
        },
    )?;
    let ground = one(
        doc,
        Command::CreateLevel {
            name: "Ground".to_owned(),
            elevation_m: 0.0,
            is_building_story: true,
            color: APP_LEVEL_COLORS[0],
            extent_m: LEVEL_EXTENT_M,
        },
    )?;
    one(
        doc,
        Command::CreateLevel {
            name: "Level 2".to_owned(),
            elevation_m: 3.0,
            is_building_story: true,
            color: APP_LEVEL_COLORS[1],
            extent_m: LEVEL_EXTENT_M,
        },
    )?;
    Ok(ground)
}

/// Attach control points to a level keeping their stored coordinates
/// verbatim (they already ARE the level-frame (u, v, w)).
pub fn attach_all(doc: &mut Document, level: EntityId, cps: &[EntityId]) -> Result<(), String> {
    for cp in cps {
        ok(
            doc,
            Command::UpdateControlPointPlane {
                id: *cp,
                plane: Some(level),
                position: None,
            },
        )?;
    }
    Ok(())
}

/// Build a level-attached closed loop from (u, v) outline points (w = 0
/// on the level plane): control points (attached) -> lines -> edges ->
/// wire. Returns the wire id.
pub fn build_attached_outline(
    doc: &mut Document,
    level: EntityId,
    points: &[P2],
) -> Result<EntityId, String> {
    let pts: Vec<[f64; 3]> = points.iter().map(|[u, v]| [*u, *v, 0.0]).collect();
    build_attached_loop(doc, level, &pts)
}

/// Build a level-attached closed loop from level-frame (u, v, w)
/// points: control points (attached) -> lines -> edges -> wire.
pub fn build_attached_loop(
    doc: &mut Document,
    level: EntityId,
    points: &[[f64; 3]],
) -> Result<EntityId, String> {
    let mut cps = Vec::with_capacity(points.len());
    for p in points {
        let cp = one(doc, Command::CreateControlPoint { position: *p })?;
        ok(
            doc,
            Command::UpdateControlPointPlane { id: cp, plane: Some(level), position: None },
        )?;
        cps.push(cp);
    }
    let mut edges = Vec::with_capacity(cps.len());
    for i in 0..cps.len() {
        let line = one(
            doc,
            Command::CreateLine { start: cps[i], end: cps[(i + 1) % cps.len()] },
        )?;
        edges.push(one(doc, Command::CreateEdge { curve: line })?);
    }
    one(doc, Command::CreateWire { edges })
}

/// Commit a wall run: one element per segment, each a vertical profile
/// rectangle (base line at `w = 0`, top at `w = height`) extruded along
/// the segment normal by `thickness`. Every point is attached to the
/// construction `plane`; every element is associated with `level` (the
/// plane's root story level). Returns the element ids in run order.
pub fn commit_walls(
    doc: &mut Document,
    plane: EntityId,
    level: EntityId,
    segments: &[WallSeg],
    height: f64,
    thickness: f64,
) -> Result<Vec<EntityId>, String> {
    let mut elements = Vec::with_capacity(segments.len());
    for seg in segments {
        let [sx, sy] = seg.start;
        let [ex, ey] = seg.end;
        // Wire order A -> B -> C -> D: the first edge runs along the base
        // in the drawn direction (the model reads the wall axis from it).
        let profile = [[sx, sy, 0.0], [ex, ey, 0.0], [ex, ey, height], [sx, sy, height]];
        let wire = build_attached_loop(doc, plane, &profile)?;
        let face = one(doc, Command::CreateFace { outer: wire, holes: vec![], plane: None })?;
        let start_cp = one(doc, Command::CreateControlPoint { position: [sx, sy, 0.0] })?;
        let end_cp = one(
            doc,
            Command::CreateControlPoint {
                position: [sx + seg.normal[0] * thickness, sy + seg.normal[1] * thickness, 0.0],
            },
        )?;
        let path = one(doc, Command::CreateLine { start: start_cp, end: end_cp })?;
        let extrusion = one(doc, Command::CreateExtrusion { profile: face, path })?;
        attach_all(doc, plane, &[start_cp, end_cp])?;
        let name = next_element_name(doc, "Wall");
        let element = one(
            doc,
            Command::CreateElement { name, members: vec![extrusion], level },
        )?;
        one(
            doc,
            Command::CreateInstance {
                element,
                transform: [
                    1.0, 0.0, 0.0, 0.0, //
                    0.0, 1.0, 0.0, 0.0, //
                    0.0, 0.0, 1.0, 0.0,
                ],
            },
        )?;
        elements.push(element);
    }
    Ok(elements)
}

/// Append a window (hole wire from level-frame (u, v, w) points) to a
/// wall's profile face. Returns the new wire id.
pub fn commit_window(
    doc: &mut Document,
    level: EntityId,
    face: EntityId,
    points: &[[f64; 3]],
) -> Result<EntityId, String> {
    let wire = build_attached_loop(doc, level, points)?;
    let mut holes = face_holes(doc, face);
    holes.push(wire);
    ok(
        doc,
        Command::UpdateFace { id: face, outer: None, holes: Some(holes), plane: None, coalesce: false },
    )?;
    Ok(wire)
}

/// Ids of a committed floor plate.
#[derive(Debug, Clone, Copy)]
pub struct PlateIds {
    pub element: EntityId,
    pub face: EntityId,
    pub extrusion: EntityId,
}

/// Commit a floor plate: attached outline (normalized CCW) -> face ->
/// DOWNWARD extrusion by `thickness` (top face on the level plane) ->
/// element associated with the level + identity instance.
pub fn commit_plate(
    doc: &mut Document,
    level: EntityId,
    points: &[P2],
    thickness: f64,
    name: &str,
) -> Result<PlateIds, String> {
    let outline = normalized_ccw(points);
    let wire = build_attached_outline(doc, level, &outline)?;
    let face = one(
        doc,
        Command::CreateFace {
            outer: wire,
            holes: vec![],
            plane: None,
        },
    )?;
    let start_cp = one(doc, Command::CreateControlPoint { position: [0.0, 0.0, 0.0] })?;
    let end_cp = one(
        doc,
        Command::CreateControlPoint {
            position: [0.0, 0.0, -thickness],
        },
    )?;
    let path = one(
        doc,
        Command::CreateLine {
            start: start_cp,
            end: end_cp,
        },
    )?;
    let extrusion = one(doc, Command::CreateExtrusion { profile: face, path })?;
    // Attach the WHOLE spatial closure (path points too): one unattached
    // point would demote the owner out of level-local evaluation.
    attach_all(doc, level, &[start_cp, end_cp])?;
    let element = one(
        doc,
        Command::CreateElement {
            name: name.to_owned(),
            members: vec![extrusion],
            level,
        },
    )?;
    one(
        doc,
        Command::CreateInstance {
            element,
            transform: [
                1.0, 0.0, 0.0, 0.0, //
                0.0, 1.0, 0.0, 0.0, //
                0.0, 0.0, 1.0, 0.0,
            ],
        },
    )?;
    Ok(PlateIds { element, face, extrusion })
}

/// Append a hole (attached outline wire) to `face`'s holes slot. The
/// library normalizes hole winding. Returns the new wire id.
pub fn commit_hole(
    doc: &mut Document,
    level: EntityId,
    face: EntityId,
    points: &[P2],
) -> Result<EntityId, String> {
    let outline = normalized_ccw(points);
    let wire = build_attached_outline(doc, level, &outline)?;
    let mut holes = face_holes(doc, face);
    holes.push(wire);
    ok(
        doc,
        Command::UpdateFace {
            id: face,
            outer: None,
            holes: Some(holes),
            plane: None,
            coalesce: false,
        },
    )?;
    Ok(wire)
}

/// The hole wires of a face, in slot order.
pub fn face_holes(doc: &Document, face: EntityId) -> Vec<EntityId> {
    doc.entity(face)
        .and_then(|record| record.inputs.get(slot::FACE_HOLES))
        .map(|s| s.referenced().collect())
        .unwrap_or_default()
}

fn input_of(doc: &Document, id: EntityId, index: usize) -> Vec<EntityId> {
    doc.entity(id)
        .and_then(|record| record.inputs.get(index))
        .map(|s| s.referenced().collect())
        .unwrap_or_default()
}

/// Delete `id` if (and only if) nothing depends on it any more.
fn delete_if_unreferenced(doc: &mut Document, id: EntityId) -> Result<(), String> {
    let Some(kind) = doc.entity(id).map(|e| e.kind()) else {
        return Ok(()); // already gone (shared input deleted earlier)
    };
    if !doc.dependents(id).map_err(|s| format!("{s:?}"))?.is_empty() {
        return Ok(()); // still shared — keep it
    }
    let cmd = match kind {
        EntityKind::Wire => Command::DeleteWire { id },
        EntityKind::Edge => Command::DeleteEdge { id },
        EntityKind::Line => Command::DeleteLine { id },
        EntityKind::ControlPoint => Command::DeleteControlPoint { id },
        _ => return Ok(()), // only construction kinds are collected here
    };
    ok(doc, cmd).map(|_| ())
}

/// Remove a hole from a face AND collect its now-orphaned construction
/// geometry (wire, edges, lines, points) — the hole analog of the
/// element orphan sweep. Shared inputs survive by the dependent rules.
pub fn delete_hole(doc: &mut Document, face: EntityId, wire: EntityId) -> Result<(), String> {
    let holes = face_holes(doc, face);
    if !holes.contains(&wire) {
        return Err("the hole is not part of this face".to_owned());
    }
    let remaining: Vec<EntityId> = holes.into_iter().filter(|h| *h != wire).collect();
    ok(
        doc,
        Command::UpdateFace {
            id: face,
            outer: None,
            holes: Some(remaining),
            plane: None,
            coalesce: false,
        },
    )?;
    let edges = input_of(doc, wire, slot::WIRE_EDGES);
    let curves: Vec<EntityId> = edges
        .iter()
        .flat_map(|e| input_of(doc, *e, slot::EDGE_CURVE))
        .collect();
    let mut points: Vec<EntityId> = curves
        .iter()
        .flat_map(|c| {
            let mut p = input_of(doc, *c, slot::LINE_START);
            p.extend(input_of(doc, *c, slot::LINE_END));
            p
        })
        .collect();
    points.sort();
    points.dedup();
    delete_if_unreferenced(doc, wire)?;
    for e in &edges {
        delete_if_unreferenced(doc, *e)?;
    }
    for c in &curves {
        delete_if_unreferenced(doc, *c)?;
    }
    for p in &points {
        delete_if_unreferenced(doc, *p)?;
    }
    Ok(())
}

/// Delete an element completely: its instances first (an instance is a
/// dependent, so the library rejects deleting a still-placed element),
/// then the element itself with the orphan sweep, which collects its
/// private construction geometry. The caller wraps this in one gesture.
pub fn delete_element(doc: &mut Document, element: EntityId) -> Result<(), String> {
    if doc.entity(element).map(|e| e.kind()) != Some(EntityKind::Element) {
        return Err("not an element".to_owned());
    }
    let dependents = doc.dependents(element).map_err(|s| format!("{s:?}"))?;
    for id in dependents {
        if doc.entity(id).map(|e| e.kind()) == Some(EntityKind::Instance) {
            ok(doc, Command::DeleteInstance { id })?;
        }
    }
    ok(doc, Command::DeleteElement { id: element, sweep_orphans: true }).map(|_| ())
}

/// Create a floor plate from a sketch: the sketch hangs below the
/// construction `plane` and one element, associated with `level` (the
/// plane's root story level), owns it. Returns (element, sketch).
pub fn create_sketch_element(
    doc: &mut Document,
    plane: EntityId,
    level: EntityId,
    sketch: &Sketch,
    name: &str,
) -> Result<(EntityId, EntityId), String> {
    let sketch_id = one(
        doc,
        Command::CreateSketch { plane, sketch: sketch.clone(), direction: SketchDirection::Below },
    )?;
    let element = one(
        doc,
        Command::CreateElement { name: name.to_owned(), members: vec![sketch_id], level },
    )?;
    Ok((element, sketch_id))
}

fn delete_command(kind: EntityKind, id: EntityId) -> Option<Command> {
    Some(match kind {
        EntityKind::ControlPoint => Command::DeleteControlPoint { id },
        EntityKind::Line => Command::DeleteLine { id },
        EntityKind::Circle => Command::DeleteCircle { id },
        EntityKind::Spline => Command::DeleteSpline { id },
        EntityKind::Edge => Command::DeleteEdge { id },
        EntityKind::Wire => Command::DeleteWire { id },
        EntityKind::Face => Command::DeleteFace { id },
        EntityKind::Solid => Command::DeleteSolid { id },
        EntityKind::Extrusion => Command::DeleteExtrusion { id },
        EntityKind::Revolve => Command::DeleteRevolve { id },
        EntityKind::Chamfer => Command::DeleteChamfer { id },
        EntityKind::Sketch => Command::DeleteSketch { id },
        _ => return None, // levels, materials, elements, ...: never swept
    })
}

/// Delete `root` and then every construction entity of its input
/// closure that nothing else uses any more (leaf-first by repetition).
pub fn delete_construction_closure(doc: &mut Document, root: EntityId) -> Result<(), String> {
    let mut closure = std::collections::BTreeSet::new();
    let mut stack = vec![root];
    while let Some(id) = stack.pop() {
        let Some(record) = doc.entity(id) else { continue };
        if delete_command(record.kind(), id).is_none() || !closure.insert(id) {
            continue;
        }
        stack.extend(record.referenced());
    }
    loop {
        let mut progress = false;
        for id in closure.iter().rev() {
            let Some(kind) = doc.entity(*id).map(|e| e.kind()) else { continue };
            if !doc.dependents(*id).map_err(|s| format!("{s:?}"))?.is_empty() {
                continue;
            }
            if let Some(cmd) = delete_command(kind, *id) {
                ok(doc, cmd)?;
                progress = true;
            }
        }
        if !progress {
            break;
        }
    }
    if doc.entity(root).is_some() {
        return Err("the old geometry is still in use".to_owned());
    }
    Ok(())
}

/// Convert a legacy (extrusion) floor plate into a sketch plate in
/// place: the same element, name, and level; the outline becomes a solid
/// face with the plate's thickness and every hole a through void; the
/// old construction chain is deleted. Returns the sketch id.
pub fn convert_legacy_plate(doc: &mut Document, plate: &PlateModel) -> Result<EntityId, String> {
    if plate.top_w.abs() > 1e-9 || !plate.downward {
        return Err("this plate is not a plain floor plate".to_owned());
    }
    let mut faces = vec![(plate.outline.clone(), FaceKind::Solid { thickness: plate.thickness })];
    faces.extend(
        plate
            .holes
            .iter()
            .filter(|h| h.outline.len() >= 3)
            .map(|h| (h.outline.clone(), FaceKind::Void { depth: None })),
    );
    let sketch = sketch_from_faces(&faces).map_err(|e| e.message().to_owned())?;
    let sketch_id = one(
        doc,
        Command::CreateSketch { plane: plate.plane_level, sketch, direction: SketchDirection::Below },
    )?;
    ok(
        doc,
        Command::UpdateElement { id: plate.element, name: None, members: Some(vec![sketch_id]), coalesce: false },
    )?;
    delete_construction_closure(doc, plate.extrusion)?;
    Ok(sketch_id)
}

/// Next free "Floor plate N" name, derived from the document (never a
/// stored counter: undo/redo/reload keep it honest).
pub fn next_element_name(doc: &Document, prefix: &str) -> String {
    let max = doc
        .entities()
        .filter_map(|(_, r)| match &r.params {
            Params::Element { name } => name
                .strip_prefix(prefix)
                .and_then(|rest| rest.trim().parse::<u64>().ok()),
            _ => None,
        })
        .max()
        .unwrap_or(0);
    format!("{prefix} {}", max + 1)
}

/// One level's params, read back for the level manager UI.
#[derive(Debug, Clone)]
pub struct LevelInfo {
    pub id: EntityId,
    pub name: String,
    pub elevation_m: f64,
    pub is_building_story: bool,
    pub color: [f32; 4],
    pub extent_m: f64,
}

/// All levels, sorted by elevation ASCENDING (ties broken by id). The
/// order is always derived, never stored.
pub fn levels_sorted(doc: &Document) -> Vec<LevelInfo> {
    let mut levels: Vec<LevelInfo> = doc
        .entities()
        .filter_map(|(id, record)| match &record.params {
            Params::Level {
                name,
                elevation_m,
                is_building_story,
                color,
                extent_m,
            } => Some(LevelInfo {
                id: *id,
                name: name.clone(),
                elevation_m: *elevation_m,
                is_building_story: *is_building_story,
                color: *color,
                extent_m: *extent_m,
            }),
            _ => None,
        })
        .collect();
    levels.sort_by(|a, b| {
        a.elevation_m
            .partial_cmp(&b.elevation_m)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.id.cmp(&b.id))
    });
    levels
}

/// The Site singleton's params (id, lat, lon, elevation, true north).
pub fn site_params(doc: &Document) -> Option<(EntityId, f64, f64, f64, f64)> {
    doc.entities().find_map(|(id, record)| match &record.params {
        Params::Site {
            latitude_deg,
            longitude_deg,
            elevation_m,
            true_north_deg,
        } => Some((*id, *latitude_deg, *longitude_deg, *elevation_m, *true_north_deg)),
        _ => None,
    })
}

/// Roll the document back to `depth` after a failed multi-command
/// operation.
pub fn rollback_to(doc: &mut Document, depth: usize) {
    while doc.undo_depth() > depth {
        if doc.undo().is_err() {
            break;
        }
    }
}

/// Human-readable status for UI messages.
pub fn status_text(status: VimStatus) -> String {
    format!("{status:?}")
}

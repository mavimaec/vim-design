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
//! - every element is ASSOCIATED with that level (mandatory slot); an
//!   element drawn on a workplane is associated with the workplane's
//!   root story level.

use vim_design_lib::entity::slot;
use vim_design_lib::{Command, Document, EntityId, EntityKind, Params, VimStatus};

use vim_design_lib::sketch::{Sketch, SketchDirection, SketchFaceKind};

use super::edit::FaceKind;
use super::edit::profile::sketch_from_faces;
use super::geom::{P2, dist, normalized_ccw};
use super::model::{LegacyWallModel, PlateModel, WallModel};
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

/// How tall new walls are: a fixed height, or up to a construction
/// plane plus an offset (the wall height then follows that plane).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WallHeight {
    /// The wall's `height_m` (its height when `top` is `None`).
    pub height_m: f64,
    pub top: Option<EntityId>,
    pub top_offset_m: f64,
}

/// The reference line of a segment for the library's `Wall`, whose
/// material grows to the LEFT of start -> end: a segment whose thickness
/// is on its right (flip side) is stored reversed.
pub fn wall_line(seg: &WallSeg) -> (P2, P2) {
    let d = [seg.end[0] - seg.start[0], seg.end[1] - seg.start[1]];
    let left = seg.normal[0] * -d[1] + seg.normal[1] * d[0] >= 0.0;
    if left { (seg.start, seg.end) } else { (seg.end, seg.start) }
}

/// Commit a wall run: one `Wall` per segment, on the construction
/// `plane`, with the default profile (a rectangle of the wall's height
/// with both top corners anchored to the top), each owned by one
/// element associated with `level` (the plane's root story level).
/// Returns the element ids in run order.
pub fn commit_walls(
    doc: &mut Document,
    plane: EntityId,
    level: EntityId,
    segments: &[WallSeg],
    height: WallHeight,
    thickness: f64,
) -> Result<Vec<EntityId>, String> {
    let mut elements = Vec::with_capacity(segments.len());
    for seg in segments {
        let (start, end) = wall_line(seg);
        let (profile, top_points) = vim_design_lib::wall::default_profile(seg.length(), thickness);
        let wall = one(
            doc,
            Command::CreateWall {
                base: plane,
                top: height.top,
                start,
                end,
                height_m: height.height_m,
                top_offset_m: height.top_offset_m,
                profile,
                top_points,
            },
        )?;
        let name = next_element_name(doc, "Wall");
        elements.push(one(doc, Command::CreateElement { name, members: vec![wall], level })?);
    }
    Ok(elements)
}

/// A wall run to (re)build: its points on the base plane, open or
/// closed, thickness side, thickness, and height mode.
#[derive(Debug, Clone, PartialEq)]
pub struct RunSpec {
    pub base: EntityId,
    pub level: EntityId,
    pub points: Vec<P2>,
    pub closed: bool,
    pub flip: bool,
    pub thickness: f64,
    pub height: WallHeight,
}

/// The top reference height a wall of `height` gets on `base`.
fn top_height(doc: &Document, base: EntityId, height: &WallHeight) -> f64 {
    let elevation = |p: EntityId| vim_design_lib::workplane::plane_elevation(doc, p).unwrap_or(0.0);
    match height.top {
        Some(top) => elevation(top) + height.top_offset_m - elevation(base),
        None => height.height_m,
    }
}

/// A wall's profile for a new length and thickness: the default
/// rectangle, with the old wall's void faces (openings) kept where they
/// still fit — mirrored along the wall when its line was reversed.
fn resized_profile(old: &WallModel, length: f64, thickness: f64, reversed: bool, h: f64) -> (Sketch, Vec<u32>) {
    let (mut profile, mut anchors) = vim_design_lib::wall::default_profile(length, thickness);
    let effective = old.effective();
    for f in &effective.faces {
        let SketchFaceKind::Void { depth } = f.kind else { continue };
        let Ok(poly) = vim_design_lib::sketch::face_polygon(&effective, f.id) else { continue };
        let poly: Vec<P2> = poly.iter().map(|p| if reversed { [old.length() - p[0], p[1]] } else { *p }).collect();
        if poly.iter().any(|p| p[0] <= 0.0 || p[0] >= length) {
            continue; // no longer on the wall
        }
        if let Ok((p, a)) = vim_design_lib::wall::ops::add_face(&profile, &anchors, h, &poly, SketchFaceKind::Void { depth }) {
            (profile, anchors) = (p, a);
        }
    }
    (profile, anchors)
}

/// Build a run as walls, reusing `existing` (the run's current walls, in
/// run order): segment i updates the i-th wall (only when it changed),
/// new segments create walls, extra walls are deleted. Butt joins as in
/// the wall tool. A changed wall's profile is rebuilt (openings kept);
/// an unchanged wall keeps its own. Returns the run's elements in order.
/// The caller wraps this in one gesture.
pub fn rewrite_run(doc: &mut Document, existing: &[WallModel], spec: &RunSpec) -> Result<Vec<EntityId>, String> {
    let segs = super::walls::wall_segments(&spec.points, spec.closed, spec.thickness, spec.flip)
        .map_err(|e| e.message().to_owned())?;
    let h = top_height(doc, spec.base, &spec.height);
    let mut out = Vec::with_capacity(segs.len());
    for (i, seg) in segs.iter().enumerate() {
        let (start, end) = wall_line(seg);
        let Some(old) = existing.get(i) else {
            let made = commit_walls(doc, spec.base, spec.level, std::slice::from_ref(seg), spec.height, spec.thickness)?;
            out.extend(made);
            continue;
        };
        let same_line = |a: P2, b: P2| dist(a, b) < 1e-9;
        let unchanged = same_line(old.start, start)
            && same_line(old.end, end)
            && (old.thickness() - spec.thickness).abs() < 1e-12
            && old.base == spec.base
            && old.top == spec.height.top
            && (old.top_offset_m - spec.height.top_offset_m).abs() < 1e-12
            && (spec.height.top.is_some() || (old.height_m - spec.height.height_m).abs() < 1e-12);
        if !unchanged {
            let od = old.dir();
            let nd = [end[0] - start[0], end[1] - start[1]];
            let reversed = od[0] * nd[0] + od[1] * nd[1] < 0.0;
            let (profile, top_points) = resized_profile(old, dist(start, end), spec.thickness, reversed, h);
            ok(
                doc,
                Command::UpdateWall {
                    id: old.wall,
                    base: Some(spec.base),
                    top: Some(spec.height.top),
                    start: Some(start),
                    end: Some(end),
                    height_m: Some(spec.height.height_m),
                    top_offset_m: Some(spec.height.top_offset_m),
                    profile: Some(profile),
                    top_points: Some(top_points),
                    coalesce: false,
                },
            )?;
        }
        out.push(old.element);
    }
    for old in existing.iter().skip(segs.len()) {
        delete_element(doc, old.element)?;
    }
    Ok(out)
}

/// Convert a legacy (extrusion) wall into a library `Wall` in place: the
/// same element, name, and level; the reference line from its base line
/// (reversed when its thickness is on the right), its level as the base,
/// a fixed height, the profile outline as a solid face of its thickness
/// with the top corners anchored to the top, and every window a through
/// void. The old construction chain is deleted. Returns the wall id.
pub fn convert_legacy_wall(doc: &mut Document, wall: &LegacyWallModel) -> Result<EntityId, String> {
    if wall.base_w.abs() > 1e-9 || wall.height <= 0.0 {
        return Err("this wall is not a plain wall".to_owned());
    }
    let length = wall.length();
    let d = wall.dir();
    // Material on the left: keep the line; on the right: reverse it and
    // mirror the profile along the wall.
    let left = wall.normal[0] * -d[1] + wall.normal[1] * d[0] >= 0.0;
    let (start, end) = if left { (wall.start, wall.end) } else { (wall.end, wall.start) };
    let map = |p: &P2| -> P2 { if left { *p } else { [length - p[0], p[1]] } };
    let mut faces = vec![(
        wall.profile.iter().map(map).collect::<Vec<_>>(),
        FaceKind::Solid { thickness: wall.thickness },
    )];
    faces.extend(
        wall.windows
            .iter()
            .filter(|h| h.outline.len() >= 3)
            .map(|h| (h.outline.iter().map(map).collect(), FaceKind::Void { depth: None })),
    );
    let effective = sketch_from_faces(&faces).map_err(|e| e.message().to_owned())?;
    let top_points: Vec<u32> = effective
        .points
        .iter()
        .filter(|p| (p.uv[1] - wall.height).abs() < 1e-6)
        .map(|p| p.id)
        .collect();
    let profile = vim_design_lib::wall::stored_profile(&effective, &top_points, wall.height);
    let id = one(
        doc,
        Command::CreateWall {
            base: wall.plane_level,
            top: None,
            start,
            end,
            height_m: wall.height,
            top_offset_m: 0.0,
            profile,
            top_points,
        },
    )?;
    // The legacy instance (identity) is not needed by a wall element.
    for dep in doc.dependents(wall.element).map_err(|st| format!("{st:?}"))? {
        if doc.entity(dep).map(|e| e.kind()) == Some(EntityKind::Instance) {
            ok(doc, Command::DeleteInstance { id: dep })?;
        }
    }
    ok(
        doc,
        Command::UpdateElement { id: wall.element, name: None, members: Some(vec![id]), coalesce: false },
    )?;
    delete_construction_closure(doc, wall.extrusion)?;
    Ok(id)
}

/// Height a wall keeps when its top plane is deleted while its top
/// reference is at or below its base (meters).
const MIN_UNWIRED_WALL_HEIGHT_M: f64 = 0.1;

/// One workplane's params and parent, read back for the tree and the
/// level manager.
#[derive(Debug, Clone, PartialEq)]
pub struct WorkplaneInfo {
    pub id: EntityId,
    pub parent: EntityId,
    pub name: String,
    pub offset_m: f64,
    pub color: [f32; 4],
    pub extent_m: f64,
}

/// Every workplane, by offset then id (siblings in height order).
pub fn workplanes(doc: &Document) -> Vec<WorkplaneInfo> {
    let mut out: Vec<WorkplaneInfo> = doc
        .entities()
        .filter_map(|(id, record)| match &record.params {
            Params::Workplane { name, offset_m, color, extent_m } => Some(WorkplaneInfo {
                id: *id,
                parent: record.inputs.get(slot::WORKPLANE_PARENT)?.referenced().next()?,
                name: name.clone(),
                offset_m: *offset_m,
                color: *color,
                extent_m: *extent_m,
            }),
            _ => None,
        })
        .collect();
    out.sort_by(|a, b| a.offset_m.total_cmp(&b.offset_m).then(a.id.cmp(&b.id)));
    out
}

/// What deleting a workplane takes with it: nested workplanes, floor
/// plates and walls standing on them, and walls that only reach up to
/// one of them (those keep their current height instead).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct WorkplaneContents {
    pub workplanes: Vec<EntityId>,
    /// Elements drawn on the planes (deleted).
    pub elements: Vec<EntityId>,
    /// Walls whose top is one of the planes (their top is unwired).
    pub topped_walls: Vec<EntityId>,
}

fn element_of(doc: &Document, member: EntityId) -> Option<EntityId> {
    doc.dependents(member)
        .ok()?
        .into_iter()
        .find(|d| doc.entity(*d).map(|e| e.kind()) == Some(EntityKind::Element))
}

/// The contents of a workplane (itself included in `workplanes`, nested
/// planes after their parents).
pub fn workplane_contents(doc: &Document, id: EntityId) -> WorkplaneContents {
    let mut out = WorkplaneContents::default();
    let mut stack = vec![id];
    while let Some(plane) = stack.pop() {
        out.workplanes.push(plane);
        for dep in doc.dependents(plane).unwrap_or_default() {
            let Some(record) = doc.entity(dep) else { continue };
            let on_base = || record.inputs.get(slot::WALL_BASE).and_then(|s| s.referenced().next()) == Some(plane);
            match record.kind() {
                EntityKind::Workplane => stack.push(dep),
                EntityKind::Wall if !on_base() => out.topped_walls.push(dep),
                EntityKind::Sketch | EntityKind::Wall | EntityKind::ControlPoint => {
                    let owner = if record.kind() == EntityKind::ControlPoint { None } else { element_of(doc, dep) };
                    if let Some(e) = owner.filter(|e| !out.elements.contains(e)) {
                        out.elements.push(e);
                    }
                }
                _ => {}
            }
        }
    }
    // A wall both on and up to the deleted planes is simply deleted.
    out.topped_walls.retain(|w| element_of(doc, *w).is_none_or(|e| !out.elements.contains(&e)));
    out
}

/// Delete a workplane with its contents (see [`WorkplaneContents`]).
/// The caller wraps this in one gesture.
pub fn delete_workplane_cascade(doc: &mut Document, id: EntityId) -> Result<(), String> {
    let contents = workplane_contents(doc, id);
    for wall in &contents.topped_walls {
        let h = vim_design_lib::wall::wall_top_height(doc, *wall).ok_or("a wall has no height")?;
        ok(
            doc,
            Command::UpdateWall {
                id: *wall,
                base: None,
                top: Some(None),
                start: None,
                end: None,
                // A wall whose top was at or below its base keeps a
                // valid (tiny) height the user can fix.
                height_m: Some(h.max(MIN_UNWIRED_WALL_HEIGHT_M)),
                top_offset_m: Some(0.0),
                profile: None,
                top_points: None,
                coalesce: false,
            },
        )?;
    }
    for element in &contents.elements {
        delete_element(doc, *element)?;
    }
    for plane in contents.workplanes.iter().rev() {
        ok(doc, Command::DeleteWorkplane { id: *plane })?;
    }
    Ok(())
}

/// Append a window (hole wire from level-frame (u, v, w) points) to a
/// legacy wall's profile face (test fixtures only). Returns the wire id.
#[cfg(test)]
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

/// Build legacy (extrusion) walls as the first wall tool did (test
/// fixtures only: documents from before the `Wall` entity).
#[cfg(test)]
pub fn commit_legacy_walls(
    doc: &mut Document,
    level: EntityId,
    segments: &[WallSeg],
    height: f64,
    thickness: f64,
) -> Result<Vec<EntityId>, String> {
    let mut elements = Vec::with_capacity(segments.len());
    for seg in segments {
        let [sx, sy] = seg.start;
        let [ex, ey] = seg.end;
        let profile = [[sx, sy, 0.0], [ex, ey, 0.0], [ex, ey, height], [sx, sy, height]];
        let wire = build_attached_loop(doc, level, &profile)?;
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
        attach_all(doc, level, &[start_cp, end_cp])?;
        let name = next_element_name(doc, "Wall");
        let element = one(doc, Command::CreateElement { name, members: vec![extrusion], level })?;
        one(
            doc,
            Command::CreateInstance {
                element,
                transform: [1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            },
        )?;
        elements.push(element);
    }
    Ok(elements)
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
        EntityKind::Wall => Command::DeleteWall { id },
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

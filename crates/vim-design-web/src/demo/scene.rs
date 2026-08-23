//! The acceptance scene, authored through the real command API: a floor
//! plate with a hole (the ground), plus a cube, a cylinder, and a cone
//! standing on it. Every object is built in its own local frame, wrapped
//! in an `Element`, and placed with an `Instance` (exercising the
//! element/instance path of the mesh facade). The returned [`SceneIds`]
//! registry holds exactly the entity ids the sliders need to update.
//!
//! Authoring-tool phase (docs/AUTHORING.md): the document is seeded with
//! the Site singleton (downtown Montreal — the default lives HERE, in
//! the app, per §1) and two levels; every object's profile control
//! points are attached to "Ground" and every element is associated with
//! it, so dragging Ground's elevation moves the whole scene.

use vim_design_lib::entity::slot;
use vim_design_lib::{Command, Document, EntityId, EntityKind, Params, ProvenancePath, SubRef};

/// Site defaults (docs/AUTHORING.md §1): downtown Montreal.
pub const DEFAULT_LATITUDE: f64 = 45.5019;
pub const DEFAULT_LONGITUDE: f64 = -73.5674;
pub const DEFAULT_SITE_ELEVATION: f64 = 36.0;
pub const DEFAULT_TRUE_NORTH: f64 = 0.0;

/// Display half-size of level overlay squares (meters).
pub const LEVEL_EXTENT_M: f64 = 5.0;

/// Palette for level colors (RGBA, translucent). Add-level cycles it.
pub const LEVEL_COLORS: [[f32; 4]; 6] = [
    [0.24, 0.62, 0.95, 0.28], // azure    (Ground)
    [0.95, 0.62, 0.20, 0.28], // orange   (Level 2)
    [0.45, 0.85, 0.45, 0.28], // green
    [0.85, 0.45, 0.85, 0.28], // magenta
    [0.95, 0.90, 0.30, 0.28], // yellow
    [0.50, 0.90, 0.90, 0.28], // cyan
];

/// Default parameter values (meters) — must match the sliders' initial
/// values in `www/index.html`.
pub const DEFAULT_CUBE_SIZE: f64 = 1.0;
pub const DEFAULT_PLATE_THICKNESS: f64 = 0.3;
pub const DEFAULT_CYL_RADIUS: f64 = 0.4;
pub const DEFAULT_CYL_HEIGHT: f64 = 1.2;
pub const DEFAULT_CONE_RADIUS: f64 = 0.5;
pub const DEFAULT_CONE_HEIGHT: f64 = 1.4;

/// Plate footprint: 6 m x 4 m centered on the origin, with a centered
/// 1.5 m x 1.5 m square hole. The plate is extruded *downward* so its top
/// face stays at z = 0 whatever the thickness — the other objects always
/// stand on it.
const PLATE_HALF_W: f64 = 3.0;
const PLATE_HALF_D: f64 = 2.0;
const HOLE_HALF: f64 = 0.75;

/// Entity ids the interactive layer needs after the scene is built.
pub struct SceneIds {
    /// Cube base square corners, CCW starting at (-s/2, -s/2, 0).
    pub cube_base_cps: [EntityId; 4],
    /// Cube base profile edges (same order as the corners); the chamfer
    /// addresses the top rim as SharedEdge(CapEnd, Side{edge}).
    pub cube_base_edges: [EntityId; 4],
    /// Top control point of the cube's extrusion path (z = size).
    pub cube_top_cp: EntityId,
    /// The cube's extrusion — the chamfer's target.
    pub cube_extrusion: EntityId,
    /// The cube's element. Its members slot is swapped between the
    /// extrusion (no chamfer) and the chamfer entity (which replaces its
    /// target as the render shape) so the instance always draws the
    /// current shape.
    pub cube_element: EntityId,
    /// Bottom control point of the plate's downward extrusion path
    /// (z = -thickness).
    pub plate_bottom_cp: EntityId,
    /// The cylinder composite's extrusion (addressed by `UpdateCylinder`).
    pub cyl_extrusion: EntityId,
    /// The composite's circle (its params carry the current radius).
    pub cyl_circle: EntityId,
    /// Top control point of the composite's path line (z = height).
    pub cyl_top_cp: EntityId,
    /// Cone profile rim control point ((radius, 0, 0)).
    pub cone_rim_cp: EntityId,
    /// Cone profile apex control point ((0, 0, height)).
    pub cone_apex_cp: EntityId,
    /// The Site singleton (geolocation metadata).
    pub site: EntityId,
    /// The "Ground" level the scene is authored on.
    pub ground: EntityId,
}

/// Submit a command that must succeed during scene construction.
fn ok(doc: &mut Document, cmd: Command) -> Result<Vec<EntityId>, String> {
    let label = cmd.label();
    doc.submit(cmd)
        .map(|out| out.created_ids)
        .map_err(|status| format!("{label} rejected while building the scene: {status:?}"))
}

/// Submit a command that must create exactly one entity.
fn one(doc: &mut Document, cmd: Command) -> Result<EntityId, String> {
    let label = cmd.label();
    let ids = ok(doc, cmd)?;
    match ids.as_slice() {
        [id] => Ok(*id),
        other => Err(format!("{label}: expected 1 created id, got {}", other.len())),
    }
}

/// A closed polygon loop: control points -> lines -> edges -> wire.
struct Loop {
    cps: Vec<EntityId>,
    edges: Vec<EntityId>,
    wire: EntityId,
}

fn build_loop(doc: &mut Document, corners: &[[f64; 3]]) -> Result<Loop, String> {
    let mut cps = Vec::with_capacity(corners.len());
    for corner in corners {
        cps.push(one(doc, Command::CreateControlPoint { position: *corner })?);
    }
    let mut edges = Vec::with_capacity(cps.len());
    for i in 0..cps.len() {
        let line = one(
            doc,
            Command::CreateLine {
                start: cps[i],
                end: cps[(i + 1) % cps.len()],
            },
        )?;
        edges.push(one(doc, Command::CreateEdge { curve: line })?);
    }
    let wire = one(doc, Command::CreateWire { edges: edges.clone() })?;
    Ok(Loop { cps, edges, wire })
}

/// Face from an outer wire (plus optional holes), extruded along a line
/// from `path_from` to `path_to`. Returns (face, path_to control point,
/// extrusion).
#[allow(clippy::type_complexity)]
fn extrude_face(
    doc: &mut Document,
    outer: EntityId,
    holes: Vec<EntityId>,
    path_from: [f64; 3],
    path_to: [f64; 3],
) -> Result<(EntityId, EntityId, EntityId), String> {
    let face = one(
        doc,
        Command::CreateFace {
            outer,
            holes,
            plane: None,
        },
    )?;
    let start_cp = one(doc, Command::CreateControlPoint { position: path_from })?;
    let end_cp = one(doc, Command::CreateControlPoint { position: path_to })?;
    let path = one(
        doc,
        Command::CreateLine {
            start: start_cp,
            end: end_cp,
        },
    )?;
    let extrusion = one(doc, Command::CreateExtrusion { profile: face, path })?;
    Ok((face, end_cp, extrusion))
}

/// Material + face assignment.
fn assign_material(
    doc: &mut Document,
    face: EntityId,
    name: &str,
    color: [f64; 3],
    roughness: f64,
) -> Result<EntityId, String> {
    let material = one(
        doc,
        Command::CreateMaterial {
            name: name.to_owned(),
            color,
            roughness,
        },
    )?;
    ok(
        doc,
        Command::UpdateFaceMaterial {
            face,
            material: Some(material),
        },
    )?;
    Ok(material)
}

/// Wrap a solid producer in an element (associated with `level` —
/// association is mandatory) and place one instance of it.
/// Returns the element id.
fn place(
    doc: &mut Document,
    name: &str,
    member: EntityId,
    level: EntityId,
    translate: [f64; 3],
) -> Result<EntityId, String> {
    let element = one(
        doc,
        Command::CreateElement {
            name: name.to_owned(),
            members: vec![member],
            level,
        },
    )?;
    let [x, y, z] = translate;
    one(
        doc,
        Command::CreateInstance {
            element,
            transform: [
                1.0, 0.0, 0.0, x, //
                0.0, 1.0, 0.0, y, //
                0.0, 0.0, 1.0, z,
            ],
        },
    )?;
    Ok(element)
}

/// Build the whole scene; returns the slider registry.
/// Attach control points to a construction plane. The scene is authored
/// with Ground at elevation 0, whose frame is the identity — so keeping
/// the stored coordinates verbatim (`position: None`) IS the
/// world-preserving conversion: (u, v, w) == (x, y, z) at attach time,
/// and no point jumps. This "attach at creation" choice (we control
/// creation) avoids conversion arithmetic entirely; a later attach to a
/// non-zero level would need the explicit `position: Some(world - frame
/// origin)` form documented in docs/AUTHORING.md §3.
fn attach_all(doc: &mut Document, plane: EntityId, cps: &[EntityId]) -> Result<(), String> {
    for cp in cps {
        ok(
            doc,
            Command::UpdateControlPointPlane {
                id: *cp,
                plane: Some(plane),
                position: None,
            },
        )?;
    }
    Ok(())
}

pub fn build_scene(doc: &mut Document) -> Result<SceneIds, String> {
    // --- Site singleton: the Montreal default lives in the app ---------
    let site = one(
        doc,
        Command::CreateSite {
            latitude_deg: DEFAULT_LATITUDE,
            longitude_deg: DEFAULT_LONGITUDE,
            elevation_m: DEFAULT_SITE_ELEVATION,
            true_north_deg: DEFAULT_TRUE_NORTH,
        },
    )?;

    // --- Levels: Ground (the scene's construction plane) + Level 2 -----
    let ground = one(
        doc,
        Command::CreateLevel {
            name: "Ground".to_owned(),
            elevation_m: 0.0,
            is_building_story: true,
            color: LEVEL_COLORS[0],
            extent_m: LEVEL_EXTENT_M,
        },
    )?;
    one(
        doc,
        Command::CreateLevel {
            name: "Level 2".to_owned(),
            elevation_m: 3.0,
            is_building_story: true,
            color: LEVEL_COLORS[1],
            extent_m: LEVEL_EXTENT_M,
        },
    )?;

    // --- Floor plate with a hole (ground, instanced at identity) -------
    let outer = build_loop(
        doc,
        &[
            [-PLATE_HALF_W, -PLATE_HALF_D, 0.0],
            [PLATE_HALF_W, -PLATE_HALF_D, 0.0],
            [PLATE_HALF_W, PLATE_HALF_D, 0.0],
            [-PLATE_HALF_W, PLATE_HALF_D, 0.0],
        ],
    )?;
    let hole = build_loop(
        doc,
        &[
            [-HOLE_HALF, -HOLE_HALF, 0.0],
            [HOLE_HALF, -HOLE_HALF, 0.0],
            [HOLE_HALF, HOLE_HALF, 0.0],
            [-HOLE_HALF, HOLE_HALF, 0.0],
        ],
    )?;
    let (plate_face, plate_bottom_cp, plate_extrusion) = extrude_face(
        doc,
        outer.wire,
        vec![hole.wire],
        [0.0, 0.0, 0.0],
        [0.0, 0.0, -DEFAULT_PLATE_THICKNESS],
    )?;
    assign_material(doc, plate_face, "concrete", [0.62, 0.61, 0.58], 0.9)?;
    attach_all(doc, ground, &outer.cps)?;
    attach_all(doc, ground, &hole.cps)?;
    let plate_element = place(doc, "floor plate", plate_extrusion, ground, [0.0, 0.0, 0.0])?;

    // --- Cube (base square centered on its local origin) ---------------
    let s = DEFAULT_CUBE_SIZE / 2.0;
    let cube_loop = build_loop(
        doc,
        &[
            [-s, -s, 0.0],
            [s, -s, 0.0],
            [s, s, 0.0],
            [-s, s, 0.0],
        ],
    )?;
    let (cube_face, cube_top_cp, cube_extrusion) = extrude_face(
        doc,
        cube_loop.wire,
        vec![],
        [0.0, 0.0, 0.0],
        [0.0, 0.0, DEFAULT_CUBE_SIZE],
    )?;
    assign_material(doc, cube_face, "brick", [0.72, 0.26, 0.20], 0.8)?;
    attach_all(doc, ground, &cube_loop.cps)?;
    let cube_element = place(doc, "cube", cube_extrusion, ground, [-1.9, -1.0, 0.0])?;
    let cube_base_cps: [EntityId; 4] = cube_loop
        .cps
        .try_into()
        .map_err(|_| "cube base loop must have 4 control points".to_owned())?;
    let cube_base_edges: [EntityId; 4] = cube_loop
        .edges
        .try_into()
        .map_err(|_| "cube base loop must have 4 edges".to_owned())?;

    // --- Cylinder (the composite command) -------------------------------
    let cyl_ids = ok(
        doc,
        Command::CreateCylinder {
            center: [0.0, 0.0, 0.0],
            radius: DEFAULT_CYL_RADIUS,
            height: DEFAULT_CYL_HEIGHT,
        },
    )?;
    let cyl_extrusion = *cyl_ids
        .last()
        .ok_or_else(|| "CreateCylinder created no entities".to_owned())?;
    let find_kind = |doc: &Document, kind: EntityKind, what: &str| -> Result<EntityId, String> {
        cyl_ids
            .iter()
            .copied()
            .find(|id| doc.entity(*id).is_some_and(|e| e.kind() == kind))
            .ok_or_else(|| format!("CreateCylinder produced no {what}"))
    };
    let cyl_face = find_kind(doc, EntityKind::Face, "face")?;
    let cyl_circle = find_kind(doc, EntityKind::Circle, "circle")?;
    // The path line runs from the (shared) center control point to the
    // top control point; read the top cp off the line's `end` slot.
    let cyl_line = find_kind(doc, EntityKind::Line, "path line")?;
    let cyl_top_cp = doc
        .entity(cyl_line)
        .and_then(|line| line.inputs.get(1))
        .and_then(|slot| slot.referenced().next())
        .ok_or_else(|| "cylinder path line has no end control point".to_owned())?;
    let cyl_center_cp = doc
        .entity(cyl_line)
        .and_then(|line| line.inputs.first())
        .and_then(|slot| slot.referenced().next())
        .ok_or_else(|| "cylinder path line has no start control point".to_owned())?;
    assign_material(doc, cyl_face, "steel blue", [0.22, 0.42, 0.72], 0.4)?;
    // Attach BOTH path endpoints: the composite shares the center cp as
    // the path start, so attaching only the center would change the path
    // vector (and thus the height) when the level moves.
    attach_all(doc, ground, &[cyl_center_cp, cyl_top_cp])?;
    let cyl_element = place(doc, "cylinder", cyl_extrusion, ground, [1.9, -1.0, 0.0])?;

    // --- Cone (right-triangle profile revolved 2π about the Z axis) ----
    let base_cp = one(doc, Command::CreateControlPoint { position: [0.0, 0.0, 0.0] })?;
    let cone_rim_cp = one(
        doc,
        Command::CreateControlPoint {
            position: [DEFAULT_CONE_RADIUS, 0.0, 0.0],
        },
    )?;
    let cone_apex_cp = one(
        doc,
        Command::CreateControlPoint {
            position: [0.0, 0.0, DEFAULT_CONE_HEIGHT],
        },
    )?;
    let mut cone_edges = Vec::with_capacity(3);
    for (a, b) in [
        (base_cp, cone_rim_cp),
        (cone_rim_cp, cone_apex_cp),
        (cone_apex_cp, base_cp),
    ] {
        let line = one(doc, Command::CreateLine { start: a, end: b })?;
        cone_edges.push(one(doc, Command::CreateEdge { curve: line })?);
    }
    let cone_wire = one(doc, Command::CreateWire { edges: cone_edges })?;
    let cone_face = one(
        doc,
        Command::CreateFace {
            outer: cone_wire,
            holes: vec![],
            plane: None,
        },
    )?;
    let cone_axis = one(
        doc,
        Command::CreateLine {
            start: base_cp,
            end: cone_apex_cp,
        },
    )?;
    let cone_revolve = one(
        doc,
        Command::CreateRevolve {
            profile: cone_face,
            axis: cone_axis,
            angle_radians: None, // 2π — closed solid of revolution
        },
    )?;
    assign_material(doc, cone_face, "amber", [0.88, 0.63, 0.14], 0.6)?;
    attach_all(doc, ground, &[base_cp, cone_rim_cp, cone_apex_cp])?;
    // Back-right, clear of the cube's line of sight from the default
    // camera even when the cube is at its maximum size.
    let cone_element = place(doc, "cone", cone_revolve, ground, [1.2, 1.4, 0.0])?;

    Ok(SceneIds {
        cube_base_cps,
        cube_base_edges,
        cube_top_cp,
        cube_extrusion,
        cube_element,
        plate_bottom_cp,
        cyl_extrusion,
        cyl_circle,
        cyl_top_cp,
        cone_rim_cp,
        cone_apex_cp,
        site,
        ground,
    })
}

// ---------------------------------------------------------------------
// Authoring-tool readers (site + levels are enumerated from the
// document — order derived from elevation, never stored).
// ---------------------------------------------------------------------

/// One level's params, read back for the level manager UI.
pub struct LevelInfo {
    pub id: EntityId,
    pub name: String,
    pub elevation_m: f64,
    pub is_building_story: bool,
    pub color: [f32; 4],
    pub extent_m: f64,
}

/// All levels, sorted by elevation ASCENDING (ties broken by id for
/// determinism). The UI displays them top-story-first (descending); the
/// sort itself is always derived, never stored (docs/AUTHORING.md §2).
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

/// The Site singleton's params (there is at most one; `None` on a
/// document without a site).
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

/// Top-rim edges of the cube for the chamfer, addressed by provenance
/// (docs/ARCHITECTURE.md §3.4): each is the intersection of the sweep's
/// end cap with the lateral face swept from one base profile edge.
///
/// KNOWN KERNEL LIMIT (verified 2026-08-22 against monstertruck-fillet):
/// only *non-adjacent* straight edges chamfer correctly in one
/// operation. All four rim edges together fail typed ("shell is not
/// connected" — corner blending is unsupported), and chaining
/// chamfer-of-chamfer to work around it silently no-ops on the edges
/// adjacent to an existing blend. The demo therefore chamfers the two
/// *opposite* rim edges (south + north), the largest supported set.
pub fn cube_chamfer_sub_edges(ids: &SceneIds) -> Vec<SubRef> {
    [ids.cube_base_edges[0], ids.cube_base_edges[2]]
        .iter()
        .map(|edge| SubRef {
            owner: ids.cube_extrusion,
            path: ProvenancePath::shared_edge(
                ProvenancePath::CapEnd,
                ProvenancePath::Side { source: *edge },
            ),
        })
        .collect()
}

/// The chamfer currently targeting the cube's extrusion, if any, with
/// its distance. Derived from the document on every call rather than
/// cached: undo/redo can create and delete the chamfer entity behind the
/// app's back, so the document is the only reliable source.
pub fn find_cube_chamfer(doc: &Document, ids: &SceneIds) -> Option<(EntityId, f64)> {
    doc.entities().find_map(|(id, record)| match &record.params {
        Params::Chamfer { distance, .. }
            if record
                .inputs
                .get(slot::CHAMFER_TARGET)
                .is_some_and(|slot| slot.referenced().next() == Some(ids.cube_extrusion)) =>
        {
            Some((*id, *distance))
        }
        _ => None,
    })
}

/// The slider parameters as currently stored in the document — the
/// single source of truth the UI resynchronizes from (startup and after
/// undo/redo).
pub struct CurrentParams {
    pub cube_size: f64,
    /// 0.0 when no chamfer entity targets the cube.
    pub cube_chamfer: f64,
    pub plate_thickness: f64,
    pub cyl_radius: f64,
    pub cyl_height: f64,
    pub cone_radius: f64,
    pub cone_height: f64,
}

/// Read the current parameter values back out of the document.
pub fn current_params(doc: &Document, ids: &SceneIds) -> CurrentParams {
    let cp_position = |id: EntityId| -> [f64; 3] {
        match doc.entity(id).map(|e| &e.params) {
            Some(Params::ControlPoint { position }) => *position,
            _ => [0.0; 3],
        }
    };
    let circle_radius = |id: EntityId| -> f64 {
        match doc.entity(id).map(|e| &e.params) {
            Some(Params::Circle { radius }) => *radius,
            _ => 0.0,
        }
    };
    CurrentParams {
        cube_size: cp_position(ids.cube_top_cp)[2],
        cube_chamfer: find_cube_chamfer(doc, ids).map_or(0.0, |(_, d)| d),
        plate_thickness: -cp_position(ids.plate_bottom_cp)[2],
        cyl_radius: circle_radius(ids.cyl_circle),
        cyl_height: cp_position(ids.cyl_top_cp)[2],
        cone_radius: cp_position(ids.cone_rim_cp)[0],
        cone_height: cp_position(ids.cone_apex_cp)[2],
    }
}

/// The commands one slider event submits (all `coalesce: true`).
pub fn cube_size_commands(ids: &SceneIds, size: f64) -> Vec<Command> {
    let h = size / 2.0;
    let corners = [[-h, -h, 0.0], [h, -h, 0.0], [h, h, 0.0], [-h, h, 0.0]];
    let mut cmds: Vec<Command> = ids
        .cube_base_cps
        .iter()
        .zip(corners)
        .map(|(id, position)| Command::UpdateControlPoint {
            id: *id,
            position,
            coalesce: true,
        })
        .collect();
    cmds.push(Command::UpdateControlPoint {
        id: ids.cube_top_cp,
        position: [0.0, 0.0, size],
        coalesce: true,
    });
    cmds
}

pub fn plate_thickness_command(ids: &SceneIds, thickness: f64) -> Command {
    Command::UpdateControlPoint {
        id: ids.plate_bottom_cp,
        position: [0.0, 0.0, -thickness],
        coalesce: true,
    }
}

pub fn cylinder_radius_command(ids: &SceneIds, radius: f64) -> Command {
    Command::UpdateCylinder {
        extrusion: ids.cyl_extrusion,
        center: None,
        radius: Some(radius),
        height: None,
        coalesce: true,
    }
}

pub fn cylinder_height_command(ids: &SceneIds, height: f64) -> Command {
    Command::UpdateCylinder {
        extrusion: ids.cyl_extrusion,
        center: None,
        radius: None,
        height: Some(height),
        coalesce: true,
    }
}

pub fn cone_radius_command(ids: &SceneIds, radius: f64) -> Command {
    Command::UpdateControlPoint {
        id: ids.cone_rim_cp,
        position: [radius, 0.0, 0.0],
        coalesce: true,
    }
}

pub fn cone_height_command(ids: &SceneIds, height: f64) -> Command {
    Command::UpdateControlPoint {
        id: ids.cone_apex_cp,
        position: [0.0, 0.0, height],
        coalesce: true,
    }
}

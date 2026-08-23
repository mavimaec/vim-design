//! VimDesignTest: shared fixtures for the integration & regression tests
//! in `tests/` (docs/ARCHITECTURE.md §12).
//!
//! Besides the structural chain, this crate builds the four-object
//! acceptance scene of the evaluation layer (cube, floor plate with a
//! hole, cylinder, cone) and provides golden-mesh helpers (bounding box,
//! divergence-theorem volume, watertightness).

use vim_design_lib::eval::Mesh;
use vim_design_lib::{Command, CommandOutput, Document, EntityId};

/// Identity rigid transform (row-major 4x3).
pub const IDENTITY_XFORM: [f64; 12] = [
    1.0, 0.0, 0.0, 0.0, //
    0.0, 1.0, 0.0, 0.0, //
    0.0, 0.0, 1.0, 0.0,
];

/// Rigid translation transform (row-major 4x3).
pub fn translation(x: f64, y: f64, z: f64) -> [f64; 12] {
    [
        1.0, 0.0, 0.0, x, //
        0.0, 1.0, 0.0, y, //
        0.0, 0.0, 1.0, z,
    ]
}

/// Submit a command that must succeed.
pub fn ok(doc: &mut Document, cmd: Command) -> CommandOutput {
    let label = cmd.label();
    match doc.submit(cmd) {
        Ok(output) => output,
        Err(status) => panic!("{label} unexpectedly rejected: {status:?}"),
    }
}

/// Submit a command that must succeed and create exactly one entity.
pub fn one(doc: &mut Document, cmd: Command) -> EntityId {
    let output = ok(doc, cmd);
    assert_eq!(output.created_ids.len(), 1, "expected exactly one created id");
    output.created_ids[0]
}

/// Serialize, asserting success.
pub fn save(doc: &Document) -> Vec<u8> {
    doc.save().expect("save should succeed")
}

/// Scenario postcondition (test plan item g): save -> load -> save must be
/// byte-identical, and the reloaded graph must pass full validation.
pub fn assert_save_load_roundtrip(doc: &Document) {
    let first = save(doc);
    let reloaded = Document::load(&first).expect("load of fresh save should succeed");
    reloaded
        .debug_validate()
        .expect("reloaded graph must satisfy all invariants");
    assert_eq!(reloaded.entity_count(), doc.entity_count());
    assert!(!reloaded.can_undo(), "undo stacks are not persisted");
    let second = save(&reloaded);
    assert_eq!(first, second, "save -> load -> save must be byte-identical");
}

/// The bottom-up chain from test plan item (a):
/// 4 control points -> spline -> edge -> wire -> face; 2 control points
/// -> line; extrusion(face, line); material; element with the extrusion;
/// 2 instances.
pub struct Chain {
    pub cps: [EntityId; 4],
    /// The level the element is (mandatorily) associated with.
    pub level: EntityId,
    pub spline: EntityId,
    pub edge: EntityId,
    pub wire: EntityId,
    pub face: EntityId,
    pub line_cps: [EntityId; 2],
    pub line: EntityId,
    pub extrusion: EntityId,
    pub material: EntityId,
    pub element: EntityId,
    pub instances: [EntityId; 2],
}

/// Build the standard chain (validating nothing beyond command success;
/// the chain_bottom_up test asserts the invariants step by step).
pub fn build_chain(doc: &mut Document) -> Chain {
    let cps = [
        one(doc, Command::CreateControlPoint { position: [0.0, 0.0, 0.0] }),
        one(doc, Command::CreateControlPoint { position: [1.0, 0.0, 0.0] }),
        one(doc, Command::CreateControlPoint { position: [1.0, 1.0, 0.0] }),
        one(doc, Command::CreateControlPoint { position: [0.0, 1.0, 0.0] }),
    ];
    let spline = one(
        doc,
        Command::CreateSpline {
            control_points: cps.to_vec(),
            degree: Some(3),
            knots: None,
        },
    );
    let edge = one(doc, Command::CreateEdge { curve: spline });
    let wire = one(doc, Command::CreateWire { edges: vec![edge] });
    let face = one(
        doc,
        Command::CreateFace {
            outer: wire,
            holes: vec![],
            plane: None,
        },
    );
    let line_cps = [
        one(doc, Command::CreateControlPoint { position: [0.0, 0.0, 0.0] }),
        one(doc, Command::CreateControlPoint { position: [0.0, 0.0, 3.0] }),
    ];
    let line = one(
        doc,
        Command::CreateLine {
            start: line_cps[0],
            end: line_cps[1],
        },
    );
    let extrusion = one(
        doc,
        Command::CreateExtrusion {
            profile: face,
            path: line,
        },
    );
    let material = one(
        doc,
        Command::CreateMaterial {
            name: "concrete".to_owned(),
            color: [0.7, 0.7, 0.65],
            roughness: 0.9,
        },
    );
    ok(
        doc,
        Command::UpdateFaceMaterial {
            face,
            material: Some(material),
        },
    );
    let level = one(
        doc,
        Command::CreateLevel {
            name: "Ground".to_owned(),
            elevation_m: 0.0,
            is_building_story: true,
            color: [0.2, 0.5, 0.9, 0.35],
            extent_m: 10.0,
        },
    );
    let element = one(
        doc,
        Command::CreateElement {
            name: "wall".to_owned(),
            members: vec![extrusion],
            level,
        },
    );
    let instances = [
        one(
            doc,
            Command::CreateInstance {
                element,
                transform: IDENTITY_XFORM,
            },
        ),
        one(
            doc,
            Command::CreateInstance {
                element,
                transform: translation(5.0, 0.0, 0.0),
            },
        ),
    ];
    Chain {
        cps,
        level,
        spline,
        edge,
        wire,
        face,
        line_cps,
        line,
        extrusion,
        material,
        element,
        instances,
    }
}

// ---------------------------------------------------------------------
// Acceptance-scene fixtures (evaluation layer): cube, floor plate with a
// hole, cylinder (composite), cone. These four objects are the scene the
// web demo renders.
// ---------------------------------------------------------------------

/// A closed rectangle of the parametric substrate: control points, lines
/// between consecutive corners, edges over the lines, and the wire.
pub struct RectLoop {
    pub cps: Vec<EntityId>,
    pub lines: Vec<EntityId>,
    pub edges: Vec<EntityId>,
    pub wire: EntityId,
}

/// Build a closed polygon loop (cps -> lines -> edges -> wire) over the
/// given corner positions, in order.
pub fn build_loop(doc: &mut Document, corners: &[[f64; 3]]) -> RectLoop {
    let cps: Vec<EntityId> = corners
        .iter()
        .map(|p| one(doc, Command::CreateControlPoint { position: *p }))
        .collect();
    let lines: Vec<EntityId> = (0..cps.len())
        .map(|i| {
            one(
                doc,
                Command::CreateLine {
                    start: cps[i],
                    end: cps[(i + 1) % cps.len()],
                },
            )
        })
        .collect();
    let edges: Vec<EntityId> = lines
        .iter()
        .map(|line| one(doc, Command::CreateEdge { curve: *line }))
        .collect();
    let wire = one(doc, Command::CreateWire { edges: edges.clone() });
    RectLoop {
        cps,
        lines,
        edges,
        wire,
    }
}

/// Acceptance object (a): a cube — square face (4 control points ->
/// lines -> edges -> wire -> face) extruded along a vertical line.
pub struct CubeFixture {
    pub base: RectLoop,
    pub face: EntityId,
    pub path_cps: [EntityId; 2],
    pub path: EntityId,
    pub extrusion: EntityId,
}

impl CubeFixture {
    /// Every entity of the cube's construction chain.
    pub fn all_ids(&self) -> Vec<EntityId> {
        let mut ids = self.base.cps.clone();
        ids.extend(&self.base.lines);
        ids.extend(&self.base.edges);
        ids.push(self.base.wire);
        ids.push(self.face);
        ids.extend(self.path_cps);
        ids.push(self.path);
        ids.push(self.extrusion);
        ids
    }
}

/// Build a cube of `size` x `size` x `height` meters at `origin`.
pub fn build_cube(doc: &mut Document, origin: [f64; 3], size: f64, height: f64) -> CubeFixture {
    let [ox, oy, oz] = origin;
    let base = build_loop(
        doc,
        &[
            [ox, oy, oz],
            [ox + size, oy, oz],
            [ox + size, oy + size, oz],
            [ox, oy + size, oz],
        ],
    );
    let face = one(
        doc,
        Command::CreateFace {
            outer: base.wire,
            holes: vec![],
            plane: None,
        },
    );
    let path_cps = [
        one(doc, Command::CreateControlPoint { position: origin }),
        one(
            doc,
            Command::CreateControlPoint {
                position: [ox, oy, oz + height],
            },
        ),
    ];
    let path = one(
        doc,
        Command::CreateLine {
            start: path_cps[0],
            end: path_cps[1],
        },
    );
    let extrusion = one(
        doc,
        Command::CreateExtrusion {
            profile: face,
            path,
        },
    );
    CubeFixture {
        base,
        face,
        path_cps,
        path,
        extrusion,
    }
}

/// Acceptance object (b): a floor plate — rectangular outer wire, an
/// optional square hole wire, extruded to a thickness.
pub struct PlateFixture {
    pub outer: RectLoop,
    pub hole: Option<RectLoop>,
    pub face: EntityId,
    pub path_cps: [EntityId; 2],
    pub path: EntityId,
    pub extrusion: EntityId,
}

/// Build a floor plate at `origin`: outer `width` x `depth`, thickness
/// `thickness`, and (when `with_hole`) a 1 m x 1 m hole at offset (1, 1).
pub fn build_plate(
    doc: &mut Document,
    origin: [f64; 3],
    width: f64,
    depth: f64,
    thickness: f64,
    with_hole: bool,
) -> PlateFixture {
    let [ox, oy, oz] = origin;
    let outer = build_loop(
        doc,
        &[
            [ox, oy, oz],
            [ox + width, oy, oz],
            [ox + width, oy + depth, oz],
            [ox, oy + depth, oz],
        ],
    );
    let hole = with_hole.then(|| {
        build_loop(
            doc,
            &[
                [ox + 1.0, oy + 1.0, oz],
                [ox + 2.0, oy + 1.0, oz],
                [ox + 2.0, oy + 2.0, oz],
                [ox + 1.0, oy + 2.0, oz],
            ],
        )
    });
    let face = one(
        doc,
        Command::CreateFace {
            outer: outer.wire,
            holes: hole.as_ref().map(|h| vec![h.wire]).unwrap_or_default(),
            plane: None,
        },
    );
    let path_cps = [
        one(doc, Command::CreateControlPoint { position: origin }),
        one(
            doc,
            Command::CreateControlPoint {
                position: [ox, oy, oz + thickness],
            },
        ),
    ];
    let path = one(
        doc,
        Command::CreateLine {
            start: path_cps[0],
            end: path_cps[1],
        },
    );
    let extrusion = one(
        doc,
        Command::CreateExtrusion {
            profile: face,
            path,
        },
    );
    PlateFixture {
        outer,
        hole,
        face,
        path_cps,
        path,
        extrusion,
    }
}

/// Acceptance object (c): the existing cylinder composite. Returns the
/// created ids; the extrusion (the mesh owner) is the last one.
pub fn build_cylinder(
    doc: &mut Document,
    center: [f64; 3],
    radius: f64,
    height: f64,
) -> Vec<EntityId> {
    ok(
        doc,
        Command::CreateCylinder {
            center,
            radius,
            height,
        },
    )
    .created_ids
}

/// Acceptance object (d): a cone — right-triangle profile face revolved
/// 2π about a vertical axis line.
pub struct ConeFixture {
    /// Control point at the base center (on the axis).
    pub base_cp: EntityId,
    /// Control point on the base rim (off the axis) — moving it onto the
    /// axis degenerates the profile (error-retention tests).
    pub rim_cp: EntityId,
    /// Control point at the apex (on the axis).
    pub apex_cp: EntityId,
    pub profile_lines: [EntityId; 3],
    pub profile_edges: [EntityId; 3],
    pub wire: EntityId,
    pub face: EntityId,
    pub axis: EntityId,
    pub revolve: EntityId,
}

impl ConeFixture {
    pub fn all_ids(&self) -> Vec<EntityId> {
        let mut ids = vec![self.base_cp, self.rim_cp, self.apex_cp];
        ids.extend(self.profile_lines);
        ids.extend(self.profile_edges);
        ids.extend([self.wire, self.face, self.axis, self.revolve]);
        ids
    }
}

/// Build a cone of base `radius` and `height` meters, apex up, at
/// `base_center` (base in the horizontal plane, axis vertical — Z-up).
pub fn build_cone(
    doc: &mut Document,
    base_center: [f64; 3],
    radius: f64,
    height: f64,
) -> ConeFixture {
    let [cx, cy, cz] = base_center;
    let base_cp = one(doc, Command::CreateControlPoint { position: base_center });
    let rim_cp = one(
        doc,
        Command::CreateControlPoint {
            position: [cx + radius, cy, cz],
        },
    );
    let apex_cp = one(
        doc,
        Command::CreateControlPoint {
            position: [cx, cy, cz + height],
        },
    );
    let profile_lines = [
        one(doc, Command::CreateLine { start: base_cp, end: rim_cp }),
        one(doc, Command::CreateLine { start: rim_cp, end: apex_cp }),
        one(doc, Command::CreateLine { start: apex_cp, end: base_cp }),
    ];
    let profile_edges = [
        one(doc, Command::CreateEdge { curve: profile_lines[0] }),
        one(doc, Command::CreateEdge { curve: profile_lines[1] }),
        one(doc, Command::CreateEdge { curve: profile_lines[2] }),
    ];
    let wire = one(
        doc,
        Command::CreateWire {
            edges: profile_edges.to_vec(),
        },
    );
    let face = one(
        doc,
        Command::CreateFace {
            outer: wire,
            holes: vec![],
            plane: None,
        },
    );
    // The axis is its own line entity from base center to apex (both on
    // the rotation axis).
    let axis = one(
        doc,
        Command::CreateLine {
            start: base_cp,
            end: apex_cp,
        },
    );
    let revolve = one(
        doc,
        Command::CreateRevolve {
            profile: face,
            axis,
            angle_radians: None, // default 2π = closed
        },
    );
    ConeFixture {
        base_cp,
        rim_cp,
        apex_cp,
        profile_lines,
        profile_edges,
        wire,
        face,
        axis,
        revolve,
    }
}

// ---------------------------------------------------------------------
// Golden-mesh helpers.
// ---------------------------------------------------------------------

/// Axis-aligned bounding box of a mesh's positions (`(min, max)`).
pub fn mesh_bbox(mesh: &Mesh) -> ([f64; 3], [f64; 3]) {
    let mut min = [f64::INFINITY; 3];
    let mut max = [f64::NEG_INFINITY; 3];
    for p in &mesh.positions {
        for axis in 0..3 {
            let v = f64::from(p[axis]);
            min[axis] = min[axis].min(v);
            max[axis] = max[axis].max(v);
        }
    }
    (min, max)
}

/// Assert the mesh bbox matches the expected corners within `tol` meters.
pub fn assert_bbox_near(mesh: &Mesh, expect_min: [f64; 3], expect_max: [f64; 3], tol: f64) {
    let (min, max) = mesh_bbox(mesh);
    for axis in 0..3 {
        assert!(
            (min[axis] - expect_min[axis]).abs() <= tol,
            "bbox min[{axis}] = {} expected {} (tol {tol})",
            min[axis],
            expect_min[axis]
        );
        assert!(
            (max[axis] - expect_max[axis]).abs() <= tol,
            "bbox max[{axis}] = {} expected {} (tol {tol})",
            max[axis],
            expect_max[axis]
        );
    }
}

/// Signed volume of the mesh via the divergence theorem. Positive for a
/// closed, outward-oriented surface.
pub fn mesh_volume(mesh: &Mesh) -> f64 {
    let p = |i: u32| -> [f64; 3] {
        let v = mesh.positions[i as usize];
        [f64::from(v[0]), f64::from(v[1]), f64::from(v[2])]
    };
    mesh.indices
        .chunks_exact(3)
        .map(|tri| {
            let (a, b, c) = (p(tri[0]), p(tri[1]), p(tri[2]));
            // det(a, b, c) / 6
            (a[0] * (b[1] * c[2] - b[2] * c[1])
                - a[1] * (b[0] * c[2] - b[2] * c[0])
                + a[2] * (b[0] * c[1] - b[1] * c[0]))
                / 6.0
        })
        .sum()
}

/// Quantize a position to a hashable key (1 µm grid, matching the kernel
/// tolerance — the mesh facade expands vertices per-face, so watertight
/// checks must match by position, not index).
fn quantized(p: [f32; 3]) -> [i64; 3] {
    let q = |x: f32| (f64::from(x) / 1e-6).round() as i64;
    [q(p[0]), q(p[1]), q(p[2])]
}

/// Assert the mesh is watertight: every directed edge (by quantized
/// position) is matched by exactly as many opposite directed edges.
pub fn assert_watertight(mesh: &Mesh) {
    use std::collections::HashMap;
    let mut edge_counts: HashMap<([i64; 3], [i64; 3]), i64> = HashMap::new();
    for tri in mesh.indices.chunks_exact(3) {
        let corners = [
            quantized(mesh.positions[tri[0] as usize]),
            quantized(mesh.positions[tri[1] as usize]),
            quantized(mesh.positions[tri[2] as usize]),
        ];
        for i in 0..3 {
            let a = corners[i];
            let b = corners[(i + 1) % 3];
            if a == b {
                continue; // degenerate sliver edge (collapsed at poles)
            }
            *edge_counts.entry((a, b)).or_insert(0) += 1;
            *edge_counts.entry((b, a)).or_insert(0) -= 1;
        }
    }
    let unmatched: Vec<_> = edge_counts
        .iter()
        .filter(|(_, count)| **count != 0)
        .take(5)
        .collect();
    assert!(
        unmatched.is_empty(),
        "mesh is not watertight: {} unmatched directed edges (first: {unmatched:?})",
        edge_counts.values().filter(|c| **c != 0).count()
    );
}

//! Thin seam over the `monstertruck` geometry kernel (docs/ARCHITECTURE.md §5.1).
//!
//! Kernel types never leak out of this module: the evaluation layer talks
//! to it through plain-data *specs* ([`CurveSpec`], [`WireSpec`],
//! [`PlaneSpec`]) and opaque handles ([`KernelFace`], [`KernelSolid`],
//! [`RawMesh`]). Every entry point is wrapped in `catch_unwind` — a
//! third-party panic surfaces as [`KernelError::Panic`], never as a crash
//! (docs/ARCHITECTURE.md §8; monstertruck returned typed errors in every
//! probe, but the backstop stands anyway).
//!
//! Orientation rules established by hands-on probes (2026-08-22):
//! - `try_attach_plane` orients the face along the outer wire's winding
//!   normal, so `make_face` re-winds the outer wire to the desired normal
//!   and hole wires to the opposite winding *before* attaching (a hole
//!   wound like the outer is treated as area added, not removed).
//! - `extrude` produces an inside-out solid when the profile normal
//!   opposes the sweep vector; `revolve` when the profile normal opposes
//!   the local sweep direction `axis × radial`. Both are fixed here by
//!   inverting the kernel face before sweeping (the authored face keeps
//!   its own normal semantics).
//!
//! Units are meters, Z-up, angles in radians (docs/ARCHITECTURE.md §7).

use std::panic::{AssertUnwindSafe, catch_unwind};

use monstertruck_meshing::rexport_polymesh::PolygonMesh;
use monstertruck_meshing::tessellation::shell_to_polygon_strict;
use monstertruck_modeling::{
    Edge as MtEdge, Face as MtFace, ParametricSurface3D, Point3, Rad, Shell as MtShell,
    Solid as MtSolid, Vector3, Vertex as MtVertex, Wire as MtWire, builder,
    builder::SweepAngle,
};

// ---------------------------------------------------------------------
// Plain-data specs (safe to construct anywhere; no kernel types inside).
// ---------------------------------------------------------------------

/// Geometric description of one curve, oriented start → end.
#[derive(Debug, Clone, PartialEq)]
pub enum CurveSpec {
    /// Straight segment.
    Segment { start: [f64; 3], end: [f64; 3] },
    /// Full circle (a closed curve; it cannot be chained with others in a
    /// wire — v1 edges are untrimmed, docs/ARCHITECTURE.md §3.1).
    Circle {
        center: [f64; 3],
        /// Unit normal of the circle's plane.
        normal: [f64; 3],
        radius: f64,
    },
    /// Bézier curve over its control polygon (first/last control points
    /// are the endpoints).
    Bezier { control_points: Vec<[f64; 3]> },
}

impl CurveSpec {
    /// Endpoints for open curves; `None` for closed curves (circles).
    pub fn endpoints(&self) -> Option<([f64; 3], [f64; 3])> {
        match self {
            CurveSpec::Segment { start, end } => Some((*start, *end)),
            CurveSpec::Circle { .. } => None,
            CurveSpec::Bezier { control_points } => {
                match (control_points.first(), control_points.last()) {
                    (Some(first), Some(last)) => Some((*first, *last)),
                    _ => None,
                }
            }
        }
    }

    /// The same curve traversed end → start.
    pub fn reversed(&self) -> CurveSpec {
        match self {
            CurveSpec::Segment { start, end } => CurveSpec::Segment {
                start: *end,
                end: *start,
            },
            CurveSpec::Circle {
                center,
                normal,
                radius,
            } => CurveSpec::Circle {
                center: *center,
                normal: neg(*normal),
                radius: *radius,
            },
            CurveSpec::Bezier { control_points } => CurveSpec::Bezier {
                control_points: control_points.iter().rev().copied().collect(),
            },
        }
    }
}

/// A closed loop of curves, already ordered and oriented head-to-tail
/// (the wire evaluator validates chaining/closure before building one).
/// A single closed curve (full circle) is also a valid wire.
#[derive(Debug, Clone, PartialEq)]
pub struct WireSpec {
    pub curves: Vec<CurveSpec>,
}

/// An infinite plane (explicit face surface).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlaneSpec {
    pub origin: [f64; 3],
    /// Unit normal.
    pub normal: [f64; 3],
}

// ---------------------------------------------------------------------
// Opaque handles.
// ---------------------------------------------------------------------

/// A planar BREP face. Kernel topology stays private; the plain-data
/// summary (normal/centroid, meters) is exposed for diagnostics.
#[derive(Debug, Clone)]
pub struct KernelFace {
    face: MtFace,
    normal: [f64; 3],
    centroid: [f64; 3],
}

impl KernelFace {
    /// Unit normal of the face (the outer wire's winding normal, or the
    /// explicit plane normal when one was supplied).
    pub fn normal(&self) -> [f64; 3] {
        self.normal
    }

    /// Centroid of the outer wire's sample points.
    pub fn centroid(&self) -> [f64; 3] {
        self.centroid
    }
}

/// A closed BREP solid.
#[derive(Debug, Clone)]
pub struct KernelSolid {
    solid: MtSolid,
}

/// Tessellated triangle mesh: flat GPU-ready buffers. Positions/normals
/// are parallel arrays; `indices` is a triangle list into them.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RawMesh {
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub indices: Vec<u32>,
}

impl RawMesh {
    pub fn triangle_count(&self) -> usize {
        self.indices.len() / 3
    }
}

/// Typed kernel failure. Every variant is a per-entity evaluation error
/// upstream (docs/ARCHITECTURE.md §6.4), never a crash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KernelError {
    /// The wire(s) are not coplanar within kernel tolerance — including
    /// the degenerate (zero-area) case, where no plane can be fitted.
    NotPlanar,
    /// Geometrically degenerate input (zero-length axis, zero radius,
    /// zero sweep, profile centroid on the revolve axis, ...).
    Degenerate(String),
    /// Kernel topology construction failed (non-simple wire, shared
    /// vertices between boundaries, open shell, ...).
    Topology(String),
    /// Tessellation refused (dropped face) or produced an empty mesh.
    Tessellation(String),
    /// Operation the seam does not support yet.
    Unsupported(String),
    /// The kernel panicked; caught at the seam (docs/ARCHITECTURE.md §8).
    Panic(String),
}

impl std::fmt::Display for KernelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            KernelError::NotPlanar => {
                write!(f, "wire is not planar (or degenerate) within tolerance")
            }
            KernelError::Degenerate(msg) => write!(f, "degenerate geometry: {msg}"),
            KernelError::Topology(msg) => write!(f, "topology error: {msg}"),
            KernelError::Tessellation(msg) => write!(f, "tessellation error: {msg}"),
            KernelError::Unsupported(msg) => write!(f, "unsupported: {msg}"),
            KernelError::Panic(msg) => write!(f, "kernel panic (caught): {msg}"),
        }
    }
}

// ---------------------------------------------------------------------
// Small vector helpers ([f64; 3], meters).
// ---------------------------------------------------------------------

fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    let ([ax, ay, az], [bx, by, bz]) = (a, b);
    [ax - bx, ay - by, az - bz]
}

fn add(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    let ([ax, ay, az], [bx, by, bz]) = (a, b);
    [ax + bx, ay + by, az + bz]
}

fn neg(a: [f64; 3]) -> [f64; 3] {
    let [x, y, z] = a;
    [-x, -y, -z]
}

fn scale(a: [f64; 3], s: f64) -> [f64; 3] {
    let [x, y, z] = a;
    [x * s, y * s, z * s]
}

pub(crate) fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    let ([ax, ay, az], [bx, by, bz]) = (a, b);
    ax * bx + ay * by + az * bz
}

pub(crate) fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    let ([ax, ay, az], [bx, by, bz]) = (a, b);
    [ay * bz - az * by, az * bx - ax * bz, ax * by - ay * bx]
}

pub(crate) fn norm(a: [f64; 3]) -> f64 {
    dot(a, a).sqrt()
}

/// Unit vector, or `None` when the length is below `tol`.
pub(crate) fn normalized(a: [f64; 3], tol: f64) -> Option<[f64; 3]> {
    let len = norm(a);
    if len <= tol { None } else { Some(scale(a, 1.0 / len)) }
}

fn point3(p: [f64; 3]) -> Point3 {
    let [x, y, z] = p;
    Point3::new(x, y, z)
}

fn vector3(v: [f64; 3]) -> Vector3 {
    let [x, y, z] = v;
    Vector3::new(x, y, z)
}

/// A deterministic unit vector perpendicular to unit vector `n`.
fn any_perpendicular(n: [f64; 3]) -> [f64; 3] {
    let [x, y, z] = n;
    // Cross with the axis most orthogonal to n.
    let candidate = if x.abs() <= y.abs() && x.abs() <= z.abs() {
        cross(n, [1.0, 0.0, 0.0])
    } else if y.abs() <= z.abs() {
        cross(n, [0.0, 1.0, 0.0])
    } else {
        cross(n, [0.0, 0.0, 1.0])
    };
    normalized(candidate, 0.0).unwrap_or([1.0, 0.0, 0.0])
}

// ---------------------------------------------------------------------
// Wire-spec analysis (pure math; used for winding decisions).
// ---------------------------------------------------------------------

/// Sample polygon of a wire (endpoints plus Bézier control points as a
/// coarse hull — good enough for winding/centroid decisions).
fn wire_sample_points(wire: &WireSpec) -> Vec<[f64; 3]> {
    let mut points = Vec::new();
    for curve in &wire.curves {
        match curve {
            CurveSpec::Segment { start, .. } => points.push(*start),
            CurveSpec::Bezier { control_points } => {
                // All but the last (the next curve's start repeats it).
                let take = control_points.len().saturating_sub(1);
                points.extend(control_points.iter().take(take).copied());
            }
            CurveSpec::Circle {
                center,
                normal,
                radius,
            } => {
                // Four points on the circle.
                if let Some(n) = normalized(*normal, 0.0) {
                    let u = any_perpendicular(n);
                    let v = cross(n, u);
                    for (cu, cv) in [(1.0, 0.0), (0.0, 1.0), (-1.0, 0.0), (0.0, -1.0)] {
                        points.push(add(
                            *center,
                            add(scale(u, cu * *radius), scale(v, cv * *radius)),
                        ));
                    }
                }
            }
        }
    }
    points
}

/// Newell-method winding normal of a wire (unnormalized zero for
/// degenerate wires). For a single-circle wire this is the circle normal.
fn wire_winding_normal(wire: &WireSpec) -> [f64; 3] {
    if let [CurveSpec::Circle { normal, .. }] = wire.curves.as_slice() {
        return *normal;
    }
    let points = wire_sample_points(wire);
    let mut n = [0.0, 0.0, 0.0];
    let count = points.len();
    for (i, current) in points.iter().enumerate() {
        let next = points.get((i + 1) % count.max(1)).unwrap_or(current);
        let ([cx, cy, cz], [nx, ny, nz]) = (*current, *next);
        let [ax, ay, az] = n;
        n = [
            ax + (cy - ny) * (cz + nz),
            ay + (cz - nz) * (cx + nx),
            az + (cx - nx) * (cy + ny),
        ];
    }
    n
}

fn wire_centroid(wire: &WireSpec) -> [f64; 3] {
    let points = wire_sample_points(wire);
    if points.is_empty() {
        return [0.0, 0.0, 0.0];
    }
    let sum = points
        .iter()
        .fold([0.0, 0.0, 0.0], |acc, p| add(acc, *p));
    scale(sum, 1.0 / points.len() as f64)
}

/// The wire re-wound so its winding normal has a positive dot product
/// with `desired` (no-op when it already does or when degenerate).
fn wound_along(wire: &WireSpec, desired: [f64; 3]) -> WireSpec {
    if dot(wire_winding_normal(wire), desired) < 0.0 {
        WireSpec {
            curves: wire
                .curves
                .iter()
                .rev()
                .map(CurveSpec::reversed)
                .collect(),
        }
    } else {
        wire.clone()
    }
}

// ---------------------------------------------------------------------
// Kernel construction.
// ---------------------------------------------------------------------

/// Panic backstop for every kernel entry point (docs/ARCHITECTURE.md §8).
fn guard<T>(body: impl FnOnce() -> Result<T, KernelError>) -> Result<T, KernelError> {
    match catch_unwind(AssertUnwindSafe(body)) {
        Ok(result) => result,
        Err(payload) => {
            let msg = payload
                .downcast_ref::<&str>()
                .map(|s| (*s).to_owned())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "unknown panic payload".to_owned());
            Err(KernelError::Panic(msg))
        }
    }
}

/// Build one kernel wire from a spec. Junction vertices are shared
/// between consecutive curves (topological closure requires vertex
/// *identity*, not positional coincidence — probe-verified).
fn build_wire(spec: &WireSpec) -> Result<MtWire, KernelError> {
    // Single full circle: swept vertex (the probe-proven recipe).
    if let [CurveSpec::Circle {
        center,
        normal,
        radius,
    }] = spec.curves.as_slice()
    {
        let n = normalized(*normal, 0.0)
            .ok_or_else(|| KernelError::Degenerate("circle normal is zero".to_owned()))?;
        let u = any_perpendicular(n);
        let start = add(*center, scale(u, *radius));
        let vertex = builder::vertex(point3(start));
        let wire: MtWire = builder::revolve(
            &vertex,
            point3(*center),
            vector3(n),
            SweepAngle::Closed,
            4,
        );
        return Ok(wire);
    }
    if spec
        .curves
        .iter()
        .any(|c| matches!(c, CurveSpec::Circle { .. }))
    {
        return Err(KernelError::Unsupported(
            "a full circle cannot be chained with other edges in one wire (v1 edges are untrimmed)"
                .to_owned(),
        ));
    }

    // Open curves chained head-to-tail: one shared vertex per junction.
    let endpoints: Vec<([f64; 3], [f64; 3])> = spec
        .curves
        .iter()
        .map(|c| {
            c.endpoints().ok_or_else(|| {
                KernelError::Degenerate("curve without endpoints in wire".to_owned())
            })
        })
        .collect::<Result<_, _>>()?;
    let vertices: Vec<MtVertex> = endpoints
        .iter()
        .map(|(start, _)| builder::vertex(point3(*start)))
        .collect();
    let count = vertices.len();
    if count < 2 {
        return Err(KernelError::Topology(
            "a wire needs at least two open edges or one closed curve".to_owned(),
        ));
    }
    let mut edges: Vec<MtEdge> = Vec::with_capacity(count);
    for (i, curve) in spec.curves.iter().enumerate() {
        let va = vertices
            .get(i)
            .ok_or_else(|| KernelError::Topology("vertex index out of range".to_owned()))?;
        let vb = vertices
            .get((i + 1) % count)
            .ok_or_else(|| KernelError::Topology("vertex index out of range".to_owned()))?;
        let edge: MtEdge = match curve {
            CurveSpec::Segment { .. } => builder::line(va, vb),
            CurveSpec::Bezier { control_points } => {
                let inner: Vec<Point3> = control_points
                    .iter()
                    .skip(1)
                    .take(control_points.len().saturating_sub(2))
                    .map(|p| point3(*p))
                    .collect();
                builder::bezier(va, vb, inner)
            }
            CurveSpec::Circle { .. } => {
                return Err(KernelError::Unsupported(
                    "circle inside multi-edge wire".to_owned(),
                ));
            }
        };
        edges.push(edge);
    }
    Ok(edges.into())
}

/// Build a planar face from an outer wire and hole wires.
///
/// The face normal is the explicit `plane` normal when given, else the
/// outer wire's winding normal. Hole wires are re-wound opposite to the
/// face normal (probe-verified requirement). Non-coplanar (or degenerate)
/// wires yield [`KernelError::NotPlanar`].
pub fn make_face(
    outer: &WireSpec,
    holes: &[WireSpec],
    plane: Option<PlaneSpec>,
    tol: f64,
) -> Result<KernelFace, KernelError> {
    guard(|| {
        let winding = wire_winding_normal(outer);
        let desired = match plane {
            Some(p) => normalized(p.normal, tol).ok_or_else(|| {
                KernelError::Degenerate("explicit plane normal is zero".to_owned())
            })?,
            None => normalized(winding, 0.0).ok_or(KernelError::NotPlanar)?,
        };
        // Explicit plane: every wire must actually lie on it.
        if let Some(p) = plane {
            for wire in std::iter::once(outer).chain(holes.iter()) {
                for point in wire_sample_points(wire) {
                    let distance = dot(sub(point, p.origin), desired).abs();
                    if distance > tol.max(1e-9) {
                        return Err(KernelError::NotPlanar);
                    }
                }
            }
        }

        let outer_wound = wound_along(outer, desired);
        let mut wires: Vec<MtWire> = vec![build_wire(&outer_wound)?];
        for hole in holes {
            let hole_wound = wound_along(hole, neg(desired));
            wires.push(build_wire(&hole_wound)?);
        }

        let face: MtFace = match builder::try_attach_plane(wires) {
            Ok(face) => face,
            Err(monstertruck_modeling::errors::Error::WireNotInOnePlane) => {
                return Err(KernelError::NotPlanar);
            }
            Err(other) => return Err(KernelError::Topology(other.to_string())),
        };

        // Belt-and-braces: make the kernel face's orientation match.
        let actual = face.oriented_surface().normal(0.0, 0.0);
        let face = if dot([actual.x, actual.y, actual.z], desired) < 0.0 {
            face.inverse()
        } else {
            face
        };

        Ok(KernelFace {
            face,
            normal: desired,
            centroid: wire_centroid(outer),
        })
    })
}

/// Extrude a face along `direction` (meters). The solid is always
/// outward-oriented regardless of the profile's normal.
pub fn extrude_solid(
    face: &KernelFace,
    direction: [f64; 3],
    tol: f64,
) -> Result<KernelSolid, KernelError> {
    guard(|| {
        let length = norm(direction);
        if length <= tol {
            return Err(KernelError::Degenerate(
                "extrusion direction has (near-)zero length".to_owned(),
            ));
        }
        let alignment = dot(face.normal, scale(direction, 1.0 / length));
        if alignment.abs() < 1e-6 {
            return Err(KernelError::Degenerate(
                "extrusion direction lies in the profile plane".to_owned(),
            ));
        }
        // Sweep away from the profile normal produces an inside-out solid
        // (probe-verified): sweep the inverted face instead.
        let profile = if alignment < 0.0 {
            face.face.inverse()
        } else {
            face.face.clone()
        };
        let solid: MtSolid = builder::extrude(&profile, vector3(direction));
        Ok(KernelSolid { solid })
    })
}

/// Revolve a face about the axis line (`origin`, `axis_direction`) by
/// `angle_radians`. `|angle| >= 2π` produces a closed solid of
/// revolution; smaller angles a partial solid with planar caps. Profiles
/// touching the axis (cones, domes) are supported (probe-verified).
pub fn revolve_solid(
    face: &KernelFace,
    origin: [f64; 3],
    axis_direction: [f64; 3],
    angle_radians: f64,
    tol: f64,
) -> Result<KernelSolid, KernelError> {
    guard(|| {
        let axis = normalized(axis_direction, tol).ok_or_else(|| {
            KernelError::Degenerate("revolve axis has (near-)zero length".to_owned())
        })?;
        if angle_radians.abs() < 1e-9 {
            return Err(KernelError::Degenerate(
                "revolve angle is (near-)zero".to_owned(),
            ));
        }
        // Radial offset of the profile centroid from the axis: needed to
        // know the local sweep direction (axis × radial).
        let offset = sub(face.centroid(), origin);
        let radial = sub(offset, scale(axis, dot(offset, axis)));
        let radial = normalized(radial, tol).ok_or_else(|| {
            KernelError::Degenerate(
                "profile centroid lies on the revolve axis (self-intersecting revolve)"
                    .to_owned(),
            )
        })?;
        let closed = angle_radians.abs() >= std::f64::consts::TAU - 1e-9;
        // Effective sweep sense: partial revolve with a negative angle
        // sweeps about the negated axis.
        let sweep_axis = if !closed && angle_radians < 0.0 {
            neg(axis)
        } else {
            axis
        };
        let sweep_dir = cross(sweep_axis, radial);
        // Profile normal opposing the sweep produces an inside-out solid
        // (probe-verified): sweep the inverted face instead.
        let profile = if dot(face.normal, sweep_dir) < 0.0 {
            face.face.inverse()
        } else {
            face.face.clone()
        };
        let solid: MtSolid = if closed {
            builder::revolve(
                &profile,
                point3(origin),
                vector3(axis),
                SweepAngle::Closed,
                4,
            )
        } else {
            let division = ((angle_radians.abs() / std::f64::consts::FRAC_PI_2).ceil()
                as usize)
                .max(1);
            builder::revolve(
                &profile,
                point3(origin),
                vector3(axis),
                SweepAngle::Partial(Rad(angle_radians)),
                division,
            )
        };
        Ok(KernelSolid { solid })
    })
}

/// Build a solid from explicit boundary faces (one closed shell — the v1
/// single-solid case, docs/ARCHITECTURE.md §3.1).
pub fn solid_from_faces(faces: &[KernelFace]) -> Result<KernelSolid, KernelError> {
    guard(|| {
        if faces.is_empty() {
            return Err(KernelError::Topology("no faces".to_owned()));
        }
        let shell: MtShell = faces.iter().map(|f| f.face.clone()).collect();
        match MtSolid::try_new(vec![shell]) {
            Ok(solid) => Ok(KernelSolid { solid }),
            Err(err) => Err(KernelError::Topology(err.to_string())),
        }
    })
}

/// Tessellate a solid at `chordal_tolerance` (meters; docs §6.3 default
/// 1 mm). Uses the strict path: a face the tessellator would silently
/// drop is a typed [`KernelError::Tessellation`] instead of a hole in the
/// mesh. Vertices are expanded to parallel position/normal buffers.
pub fn tessellate(
    solid: &KernelSolid,
    chordal_tolerance: f64,
) -> Result<RawMesh, KernelError> {
    guard(|| {
        let tolerance = if chordal_tolerance > 0.0 {
            chordal_tolerance
        } else {
            1e-3
        };
        let mut polygon = PolygonMesh::default();
        for shell in solid.solid.boundaries() {
            match shell_to_polygon_strict(shell, tolerance) {
                Ok(part) => polygon.merge(part),
                Err(err) => return Err(KernelError::Tessellation(err.to_string())),
            }
        }
        flatten_polygon(&polygon)
    })
}

/// Expand a kernel polygon mesh into flat position/normal/index buffers,
/// fan-triangulating any non-triangle faces and filling missing normals
/// from triangle geometry.
fn flatten_polygon(polygon: &PolygonMesh) -> Result<RawMesh, KernelError> {
    let expanded = polygon.expands(|attr| (attr.position, attr.normal));
    let attributes = expanded.attributes();
    let mut positions: Vec<[f32; 3]> = Vec::with_capacity(attributes.len());
    let mut normals_opt: Vec<Option<[f64; 3]>> = Vec::with_capacity(attributes.len());
    for (position, normal) in attributes {
        positions.push([position.x as f32, position.y as f32, position.z as f32]);
        normals_opt.push(normal.map(|n| [n.x, n.y, n.z]));
    }

    let mut indices: Vec<u32> = Vec::new();
    for face in expanded.face_iter() {
        let Some((&first, rest)) = face.split_first() else {
            continue;
        };
        for pair in rest.windows(2) {
            if let [b, c] = pair {
                for idx in [first, *b, *c] {
                    let idx32 = u32::try_from(idx).map_err(|_| {
                        KernelError::Tessellation("mesh exceeds u32 index range".to_owned())
                    })?;
                    indices.push(idx32);
                }
            }
        }
    }
    if indices.is_empty() {
        return Err(KernelError::Tessellation(
            "tessellation produced an empty mesh".to_owned(),
        ));
    }

    // Fill any missing normals from the geometry of the first triangle
    // that references the vertex.
    for triangle in indices.chunks_exact(3) {
        if let [a, b, c] = triangle {
            let (a, b, c) = (*a as usize, *b as usize, *c as usize);
            let needs = [a, b, c]
                .iter()
                .any(|&i| normals_opt.get(i).is_some_and(|n| n.is_none()));
            if !needs {
                continue;
            }
            let (Some(pa), Some(pb), Some(pc)) =
                (positions.get(a), positions.get(b), positions.get(c))
            else {
                continue;
            };
            let to64 = |p: &[f32; 3]| {
                let [x, y, z] = *p;
                [f64::from(x), f64::from(y), f64::from(z)]
            };
            let n = cross(sub(to64(pb), to64(pa)), sub(to64(pc), to64(pa)));
            let n = normalized(n, 0.0).unwrap_or([0.0, 0.0, 1.0]);
            for i in [a, b, c] {
                if let Some(slot) = normals_opt.get_mut(i) {
                    if slot.is_none() {
                        *slot = Some(n);
                    }
                }
            }
        }
    }

    let normals: Vec<[f32; 3]> = normals_opt
        .into_iter()
        .map(|n| {
            let [x, y, z] = n.unwrap_or([0.0, 0.0, 1.0]);
            [x as f32, y as f32, z as f32]
        })
        .collect();

    Ok(RawMesh {
        positions,
        normals,
        indices,
    })
}

/// Build a trivial kernel object and describe it. Retained from the
/// skeleton so the FFI/web placeholder probes keep working.
pub fn probe() -> String {
    let point = Point3::new(0.0, 0.0, 1.0);
    let vertex = builder::vertex(point);
    format!("monstertruck vertex created: {:?}", vertex)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square_wire(size: f64, z: f64) -> WireSpec {
        let corners = [
            [0.0, 0.0, z],
            [size, 0.0, z],
            [size, size, z],
            [0.0, size, z],
            [0.0, 0.0, z],
        ];
        WireSpec {
            curves: corners
                .windows(2)
                .filter_map(|pair| match pair {
                    [start, end] => Some(CurveSpec::Segment {
                        start: *start,
                        end: *end,
                    }),
                    _ => None,
                })
                .collect(),
        }
    }

    #[test]
    fn kernel_probe_links_monstertruck() {
        let desc = probe();
        assert!(desc.contains("monstertruck vertex created"));
    }

    #[test]
    fn square_face_extrudes_to_a_cube_mesh() {
        let face = make_face(&square_wire(1.0, 0.0), &[], None, 1e-6);
        assert!(face.is_ok());
        let Ok(face) = face else { return };
        let solid = extrude_solid(&face, [0.0, 0.0, 1.0], 1e-6);
        assert!(solid.is_ok());
        let Ok(solid) = solid else { return };
        let mesh = tessellate(&solid, 1e-3);
        assert!(mesh.is_ok());
        let Ok(mesh) = mesh else { return };
        assert_eq!(mesh.triangle_count(), 12);
        assert_eq!(mesh.positions.len(), mesh.normals.len());
    }

    #[test]
    fn downward_extrusion_is_not_inside_out() {
        let Ok(face) = make_face(&square_wire(1.0, 0.0), &[], None, 1e-6) else {
            unreachable!("square face must build");
        };
        let solid = extrude_solid(&face, [0.0, 0.0, -1.0], 1e-6);
        assert!(solid.is_ok());
    }

    #[test]
    fn degenerate_wires_report_not_planar() {
        // Collinear "triangle": zero area, no fittable plane.
        let wire = WireSpec {
            curves: vec![
                CurveSpec::Segment {
                    start: [0.0, 0.0, 0.0],
                    end: [1.0, 0.0, 0.0],
                },
                CurveSpec::Segment {
                    start: [1.0, 0.0, 0.0],
                    end: [2.0, 0.0, 0.0],
                },
                CurveSpec::Segment {
                    start: [2.0, 0.0, 0.0],
                    end: [0.0, 0.0, 0.0],
                },
            ],
        };
        assert_eq!(
            make_face(&wire, &[], None, 1e-6).err(),
            Some(KernelError::NotPlanar)
        );
    }

    #[test]
    fn zero_length_axis_is_degenerate() {
        let Ok(face) = make_face(&square_wire(1.0, 0.0), &[], None, 1e-6) else {
            unreachable!("square face must build");
        };
        let result = revolve_solid(&face, [0.0, 0.0, 0.0], [0.0, 0.0, 0.0], 1.0, 1e-6);
        assert!(matches!(result, Err(KernelError::Degenerate(_))));
    }
}

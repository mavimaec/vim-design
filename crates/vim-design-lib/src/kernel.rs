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
use monstertruck_meshing::tessellation::{MeshableShape, shell_to_polygon_strict};
use monstertruck_modeling::{
    BoundedCurve, Edge as MtEdge, Face as MtFace, FilletOptions, FilletProfile,
    Invertible, ParametricCurve, ParametricSurface3D, Point3, Rad, Shell as MtShell, Solid as MtSolid,
    Vector3, Vertex as MtVertex, Wire as MtWire, builder, builder::SweepAngle,
    fillet_edges,
};

use crate::id::EntityId;
use crate::subref::{CapId, ProvenancePath, ProvenanceQuery, WireFilter};

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
///
/// `sources` runs parallel to `curves`: the `Edge` **entity** each curve
/// came from — the stable ids provenance naming derives from
/// (docs/ARCHITECTURE.md §3.4). `EntityId::INVALID` marks curves without
/// an authored source (their swept faces stay unnamed).
#[derive(Debug, Clone, PartialEq)]
pub struct WireSpec {
    pub curves: Vec<CurveSpec>,
    pub sources: Vec<EntityId>,
}

impl WireSpec {
    /// A wire without source attribution (kernel tests, synthetic wires).
    pub fn from_curves(curves: Vec<CurveSpec>) -> Self {
        let sources = vec![EntityId::INVALID; curves.len()];
        Self { curves, sources }
    }

    /// A wire with per-curve source edge entities (lengths must match;
    /// a mismatch degrades to unattributed sources rather than failing).
    pub fn with_sources(curves: Vec<CurveSpec>, sources: Vec<EntityId>) -> Self {
        let sources = if sources.len() == curves.len() {
            sources
        } else {
            vec![EntityId::INVALID; curves.len()]
        };
        Self { curves, sources }
    }

    /// `(curve, source)` pairs.
    fn pairs(&self) -> impl Iterator<Item = (&CurveSpec, EntityId)> + '_ {
        self.curves
            .iter()
            .zip(self.sources.iter().copied().chain(std::iter::repeat(EntityId::INVALID)))
    }
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
/// summary (normal/centroid, meters) is exposed for diagnostics. The
/// profile wire specs are retained so sweeps can build the provenance
/// model (docs/ARCHITECTURE.md §3.4).
#[derive(Debug, Clone)]
pub struct KernelFace {
    face: MtFace,
    normal: [f64; 3],
    centroid: [f64; 3],
    outer: WireSpec,
    holes: Vec<WireSpec>,
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

/// How a solid was swept — retained so provenance can be *re-derived*
/// from geometry after downstream operations (chamfer) reshape the
/// face list. Purely plain-data.
#[derive(Debug, Clone)]
enum SweepModel {
    Extrusion {
        outer: WireSpec,
        holes: Vec<WireSpec>,
        plane_origin: [f64; 3],
        plane_normal: [f64; 3],
        direction: [f64; 3],
    },
    Revolve {
        outer: WireSpec,
        holes: Vec<WireSpec>,
        /// A point of the profile plane (the profile centroid): fixes
        /// the rotational reference position of the sweep's caps.
        plane_origin: [f64; 3],
        origin: [f64; 3],
        axis: [f64; 3],
        angle: f64,
    },
}

/// A closed BREP solid with provenance-named faces
/// (docs/ARCHITECTURE.md §3.4).
///
/// `provenance` runs parallel to the solid's faces in boundary/shell
/// iteration order and is **rebuilt on every evaluation** from geometry;
/// the face index itself is an internal detail that never crosses the
/// facade — only [`ProvenancePath`]s do.
#[derive(Debug, Clone)]
pub struct KernelSolid {
    solid: MtSolid,
    provenance: Vec<Option<ProvenancePath>>,
    model: Option<SweepModel>,
}

impl KernelSolid {
    /// Number of BREP faces.
    pub fn face_count(&self) -> usize {
        self.provenance.len()
    }

    /// Provenance path per face, in the same order as
    /// [`tessellate_faces`] output (`None` = unnamed face).
    pub fn face_paths(&self) -> &[Option<ProvenancePath>] {
        &self.provenance
    }
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
    /// A provenance-named subelement reference did not resolve against
    /// the current topology (docs/ARCHITECTURE.md §3.4 — never a silent
    /// re-bind).
    Unresolved(String),
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
            KernelError::Unresolved(msg) => {
                write!(f, "subelement reference did not resolve: {msg}")
            }
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
/// Sources stay aligned with their (reversed) curves.
fn wound_along(wire: &WireSpec, desired: [f64; 3]) -> WireSpec {
    if dot(wire_winding_normal(wire), desired) < 0.0 {
        WireSpec {
            curves: wire
                .curves
                .iter()
                .rev()
                .map(CurveSpec::reversed)
                .collect(),
            sources: wire.sources.iter().rev().copied().collect(),
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
            outer: outer.clone(),
            holes: holes.to_vec(),
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
        let model = SweepModel::Extrusion {
            outer: face.outer.clone(),
            holes: face.holes.clone(),
            plane_origin: face.centroid,
            plane_normal: face.normal,
            direction,
        };
        let provenance = classify_solid(&solid, &model);
        Ok(KernelSolid {
            solid,
            provenance,
            model: Some(model),
        })
    })
}

/// One boundary loop of a prism profile: points on one plane and a
/// provenance name per edge (`names[i]` names the edge from `points[i]`
/// to the next point, wrapping; `None` leaves that side unnamed).
#[derive(Debug, Clone, PartialEq)]
pub struct PrismLoop {
    pub points: Vec<[f64; 3]>,
    pub names: Vec<Option<ProvenancePath>>,
}

/// Build a prism: the planar profile `outer` minus `holes`, extruded by
/// `direction`. Every face is named by the caller: a lateral face by the
/// name of the profile edge that sweeps it, the profile-side cap by
/// `start_cap`, the far cap by `end_cap`. The solid carries no sweep
/// model, so provenance queries do not expand on it; single paths
/// resolve through its face names.
pub fn prism_solid(
    outer: &PrismLoop,
    holes: &[PrismLoop],
    direction: [f64; 3],
    start_cap: ProvenancePath,
    end_cap: ProvenancePath,
    tol: f64,
) -> Result<KernelSolid, KernelError> {
    guard(|| {
        // Each edge's source is its 1-based index into `names`; the
        // sweep classifier then names each lateral face by that index.
        let mut names: Vec<Option<ProvenancePath>> = Vec::new();
        let mut to_wire = |profile: &PrismLoop| -> WireSpec {
            let n = profile.points.len();
            let mut curves = Vec::with_capacity(n);
            let mut sources = Vec::with_capacity(n);
            for i in 0..n {
                let (Some(start), Some(end)) =
                    (profile.points.get(i), profile.points.get((i + 1) % n))
                else {
                    continue;
                };
                curves.push(CurveSpec::Segment {
                    start: *start,
                    end: *end,
                });
                names.push(profile.names.get(i).cloned().flatten());
                sources.push(EntityId(names.len() as u64));
            }
            WireSpec::with_sources(curves, sources)
        };
        let outer_wire = to_wire(outer);
        let hole_wires: Vec<WireSpec> = holes.iter().map(&mut to_wire).collect();
        let face = make_face(&outer_wire, &hole_wires, None, tol)?;
        let mut solid = extrude_solid(&face, direction, tol)?;
        solid.provenance = solid
            .provenance
            .iter()
            .map(|path| match path {
                Some(ProvenancePath::Side { source }) => usize::try_from(source.0)
                    .ok()
                    .and_then(|index| index.checked_sub(1))
                    .and_then(|index| names.get(index).cloned().flatten()),
                Some(ProvenancePath::CapStart) => Some(start_cap.clone()),
                Some(ProvenancePath::CapEnd) => Some(end_cap.clone()),
                _ => None,
            })
            .collect();
        solid.model = None;
        Ok(solid)
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
        let model = SweepModel::Revolve {
            outer: face.outer.clone(),
            holes: face.holes.clone(),
            plane_origin: face.centroid,
            origin,
            axis,
            angle: angle_radians,
        };
        let provenance = classify_solid(&solid, &model);
        Ok(KernelSolid {
            solid,
            provenance,
            model: Some(model),
        })
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
            // Authored faces are addressable by their own Face entity id
            // (docs/ARCHITECTURE.md §3.4): no generated-topology names.
            Ok(solid) => Ok(KernelSolid {
                provenance: vec![None; faces.len()],
                solid,
                model: None,
            }),
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
        let mesh = flatten_polygon(&polygon)?;
        if mesh.indices.is_empty() {
            return Err(KernelError::Tessellation(
                "tessellation produced an empty mesh".to_owned(),
            ));
        }
        Ok(mesh)
    })
}

/// Tessellate a solid into **one mesh per BREP face**, aligned with
/// [`KernelSolid::face_paths`] (same boundary/shell iteration order).
/// Shared edges are still discretized once per shell (the whole shell is
/// triangulated in one pass), so face meshes stitch watertight by
/// position. A silently-dropped face is a typed error, as in
/// [`tessellate`]. Empty per-face meshes (degenerate slivers, e.g. the
/// on-axis faces of a cone) are allowed and come back empty.
pub fn tessellate_faces(
    solid: &KernelSolid,
    chordal_tolerance: f64,
) -> Result<Vec<RawMesh>, KernelError> {
    guard(|| {
        let tolerance = if chordal_tolerance > 0.0 {
            chordal_tolerance
        } else {
            1e-3
        };
        let mut meshes: Vec<RawMesh> = Vec::with_capacity(solid.provenance.len());
        for shell in solid.solid.boundaries() {
            let meshed = shell.triangulation(tolerance);
            for (index, face) in meshed.face_iter().enumerate() {
                match face.surface() {
                    None => {
                        return Err(KernelError::Tessellation(format!(
                            "tessellation dropped face {index} (no usable mesh)"
                        )));
                    }
                    Some(mut poly) => {
                        if !face.orientation() {
                            poly.invert();
                        }
                        meshes.push(flatten_polygon(&poly)?);
                    }
                }
            }
        }
        Ok(meshes)
    })
}

// ---------------------------------------------------------------------
// Provenance classification (docs/ARCHITECTURE.md §3.4).
//
// Faces are named by *geometric membership* against the sweep model —
// "all boundary samples of this face lie on the surface swept by profile
// curve `e`" — never by kernel output index. Rebuilt on every
// evaluation; also re-run after downstream operations (chamfer) so
// surviving faces keep their upstream names.
// ---------------------------------------------------------------------

/// Membership tolerance for provenance classification (meters). Coarser
/// than the kernel tolerance on purpose: it only has to discriminate
/// faces that differ at model scale.
const CLASSIFY_TOL: f64 = 1e-5;

/// Sample points of a face's boundary: each edge's start vertex plus two
/// interior curve points (quarter + mid) — enough to discriminate caps,
/// swept sides, and blend faces.
fn face_boundary_samples(face: &MtFace) -> Vec<[f64; 3]> {
    let mut points = Vec::new();
    for wire in face.boundaries() {
        for edge in wire.edge_iter() {
            let p = edge.front().point();
            points.push([p.x, p.y, p.z]);
            let curve = edge.curve();
            let (t0, t1) = curve.range_tuple();
            for f in [0.25, 0.5] {
                let q = curve.subs(t0 + f * (t1 - t0));
                points.push([q.x, q.y, q.z]);
            }
        }
    }
    points
}

/// Evaluate a curve spec at parameter `t` in `[0, 1]`.
fn curve_spec_point(curve: &CurveSpec, t: f64) -> [f64; 3] {
    match curve {
        CurveSpec::Segment { start, end } => add(*start, scale(sub(*end, *start), t)),
        CurveSpec::Circle {
            center,
            normal,
            radius,
        } => {
            let n = normalized(*normal, 0.0).unwrap_or([0.0, 0.0, 1.0]);
            let u = any_perpendicular(n);
            let v = cross(n, u);
            let phi = t * std::f64::consts::TAU;
            add(
                *center,
                add(
                    scale(u, *radius * phi.cos()),
                    scale(v, *radius * phi.sin()),
                ),
            )
        }
        CurveSpec::Bezier { control_points } => {
            // De Casteljau.
            let mut pts: Vec<[f64; 3]> = control_points.clone();
            while pts.len() > 1 {
                pts = pts
                    .windows(2)
                    .filter_map(|pair| match pair {
                        [a, b] => Some(add(*a, scale(sub(*b, *a), t))),
                        _ => None,
                    })
                    .collect();
            }
            pts.first().copied().unwrap_or([0.0; 3])
        }
    }
}

/// Minimize `cost(curve(t))` over `t` in `[0, 1]`: dense sampling plus
/// ternary refinement around the best bracket. Robust for the smooth
/// costs used here (distances).
fn min_cost_over_curve(curve: &CurveSpec, cost: impl Fn([f64; 3]) -> f64) -> f64 {
    const SAMPLES: usize = 64;
    let mut best_index = 0usize;
    let mut best = f64::INFINITY;
    for i in 0..=SAMPLES {
        let t = i as f64 / SAMPLES as f64;
        let c = cost(curve_spec_point(curve, t));
        if c < best {
            best = c;
            best_index = i;
        }
    }
    let mut lo = (best_index.saturating_sub(1)) as f64 / SAMPLES as f64;
    let mut hi = ((best_index + 1).min(SAMPLES)) as f64 / SAMPLES as f64;
    for _ in 0..48 {
        let m1 = lo + (hi - lo) / 3.0;
        let m2 = hi - (hi - lo) / 3.0;
        if cost(curve_spec_point(curve, m1)) <= cost(curve_spec_point(curve, m2)) {
            hi = m2;
        } else {
            lo = m1;
        }
    }
    let refined = cost(curve_spec_point(curve, (lo + hi) / 2.0));
    refined.min(best)
}

/// 3D distance from `p` to a curve spec (exact for segments/circles,
/// sampled+refined for Béziers).
fn curve_distance_3d(curve: &CurveSpec, p: [f64; 3]) -> f64 {
    match curve {
        CurveSpec::Segment { start, end } => {
            let d = sub(*end, *start);
            let len2 = dot(d, d);
            if len2 <= f64::MIN_POSITIVE {
                return norm(sub(p, *start));
            }
            let t = (dot(sub(p, *start), d) / len2).clamp(0.0, 1.0);
            norm(sub(p, add(*start, scale(d, t))))
        }
        CurveSpec::Circle {
            center,
            normal,
            radius,
        } => {
            let n = normalized(*normal, 0.0).unwrap_or([0.0, 0.0, 1.0]);
            let v = sub(p, *center);
            let h = dot(v, n);
            let in_plane = sub(v, scale(n, h));
            let radial = norm(in_plane) - *radius;
            (h * h + radial * radial).sqrt()
        }
        CurveSpec::Bezier { .. } => {
            min_cost_over_curve(curve, |c| norm(sub(p, c)))
        }
    }
}

/// `(radial, axial)` profile coordinates of `p` about an axis.
fn rz_of(p: [f64; 3], origin: [f64; 3], axis: [f64; 3]) -> (f64, f64) {
    let v = sub(p, origin);
    let z = dot(v, axis);
    let r = norm(sub(v, scale(axis, z)));
    (r, z)
}

/// Distance in `(r, z)` profile space from `p` to the surface of
/// revolution swept by `curve` (membership test for revolve sides).
fn curve_rz_distance(
    curve: &CurveSpec,
    p: [f64; 3],
    origin: [f64; 3],
    axis: [f64; 3],
) -> f64 {
    let (rp, zp) = rz_of(p, origin, axis);
    min_cost_over_curve(curve, |c| {
        let (rc, zc) = rz_of(c, origin, axis);
        ((rp - rc).powi(2) + (zp - zc).powi(2)).sqrt()
    })
}

impl SweepModel {
    /// Classify one face (by its boundary samples) against this sweep:
    /// caps first, then per-source swept sides. `None` = no membership
    /// (e.g. a chamfer blend face).
    fn classify(&self, samples: &[[f64; 3]]) -> Option<ProvenancePath> {
        if samples.is_empty() {
            return None;
        }
        let on_plane = |origin: [f64; 3], normal: [f64; 3]| {
            samples
                .iter()
                .all(|p| dot(sub(*p, origin), normal).abs() <= CLASSIFY_TOL)
        };
        match self {
            SweepModel::Extrusion {
                outer,
                holes,
                plane_origin,
                plane_normal,
                direction,
            } => {
                if on_plane(*plane_origin, *plane_normal) {
                    return Some(ProvenancePath::CapStart);
                }
                if on_plane(add(*plane_origin, *direction), *plane_normal) {
                    return Some(ProvenancePath::CapEnd);
                }
                let axial = dot(*direction, *plane_normal);
                if axial.abs() <= f64::MIN_POSITIVE {
                    return None;
                }
                for (curve, source) in
                    outer.pairs().chain(holes.iter().flat_map(|h| h.pairs()))
                {
                    if source == EntityId::INVALID {
                        continue;
                    }
                    let all_on_swept = samples.iter().all(|p| {
                        // Project along the sweep direction onto the
                        // profile plane, then test curve membership.
                        let s = dot(sub(*p, *plane_origin), *plane_normal) / axial;
                        let projected = sub(*p, scale(*direction, s));
                        curve_distance_3d(curve, projected) <= CLASSIFY_TOL
                    });
                    if all_on_swept {
                        return Some(ProvenancePath::Side { source });
                    }
                }
                None
            }
            SweepModel::Revolve {
                outer,
                holes,
                plane_origin,
                origin,
                axis,
                angle,
            } => {
                let closed = angle.abs() >= std::f64::consts::TAU - 1e-9;
                if !closed {
                    // Caps are identified by ROTATIONAL POSITION, not by
                    // plane membership: at angle = π the start and end
                    // cap planes coincide (and the profile plane contains
                    // every on-axis sliver face). A cap's samples all sit
                    // at one rotation angle φ (on-axis samples exempt —
                    // they belong to every φ).
                    let radial_ref = sub(*plane_origin, *origin);
                    let radial_ref =
                        sub(radial_ref, scale(*axis, dot(radial_ref, *axis)));
                    if let Some(r0) = normalized(radial_ref, CLASSIFY_TOL) {
                        let y0 = cross(*axis, r0);
                        const ANG_EPS: f64 = 1e-6;
                        let phi_of = |p: &[f64; 3]| -> Option<f64> {
                            let v = sub(*p, *origin);
                            let v = sub(v, scale(*axis, dot(v, *axis)));
                            if norm(v) <= CLASSIFY_TOL {
                                return None; // on the axis: any φ
                            }
                            Some(dot(v, y0).atan2(dot(v, r0)))
                        };
                        let all_at = |target: f64| {
                            let mut off_axis = 0usize;
                            let ok = samples.iter().all(|p| match phi_of(p) {
                                None => true,
                                Some(phi) => {
                                    off_axis += 1;
                                    let delta = phi - target;
                                    delta.sin().abs() <= ANG_EPS
                                        && delta.cos() > 0.0
                                }
                            });
                            // All-on-axis faces are sliver side faces of
                            // an on-axis profile edge, never caps.
                            ok && off_axis > 0
                        };
                        if all_at(0.0) {
                            return Some(ProvenancePath::CapStart);
                        }
                        if all_at(*angle) {
                            return Some(ProvenancePath::CapEnd);
                        }
                    }
                }
                for (curve, source) in
                    outer.pairs().chain(holes.iter().flat_map(|h| h.pairs()))
                {
                    if source == EntityId::INVALID {
                        continue;
                    }
                    let all_on_swept = samples
                        .iter()
                        .all(|p| curve_rz_distance(curve, *p, *origin, *axis) <= CLASSIFY_TOL);
                    if all_on_swept {
                        return Some(ProvenancePath::Side { source });
                    }
                }
                None
            }
        }
    }
}

/// All faces of a solid in boundary/shell iteration order (the order
/// `provenance` and [`tessellate_faces`] use).
fn solid_faces(solid: &MtSolid) -> Vec<MtFace> {
    solid
        .boundaries()
        .iter()
        .flat_map(|shell| shell.face_iter().cloned())
        .collect()
}

/// Rebuild the face-provenance vector for `solid` from its sweep model.
fn classify_solid(solid: &MtSolid, model: &SweepModel) -> Vec<Option<ProvenancePath>> {
    solid_faces(solid)
        .iter()
        .map(|face| model.classify(&face_boundary_samples(face)))
        .collect()
}

// ---------------------------------------------------------------------
// SubRef resolution against a solid's provenance (docs §3.4).
// ---------------------------------------------------------------------

/// Indices of the faces named `path` (canonical comparison).
fn faces_with_path(solid: &KernelSolid, path: &ProvenancePath) -> Vec<usize> {
    let wanted = path.canonical();
    solid
        .provenance
        .iter()
        .enumerate()
        .filter(|(_, p)| p.as_ref().is_some_and(|p| p.canonical() == wanted))
        .map(|(i, _)| i)
        .collect()
}

/// Kernel edges shared between two face-index sets (deduped by edge id).
fn shared_edges_between(
    faces: &[MtFace],
    set_a: &[usize],
    set_b: &[usize],
) -> Vec<MtEdge> {
    let ids_a: std::collections::HashSet<_> = set_a
        .iter()
        .filter_map(|i| faces.get(*i))
        .flat_map(|f| f.boundaries().iter().flat_map(|w| w.edge_iter().map(|e| e.id()).collect::<Vec<_>>()).collect::<Vec<_>>())
        .collect();
    let mut seen = std::collections::HashSet::new();
    let mut result = Vec::new();
    for i in set_b {
        // A SharedEdge address with identical operands is meaningless;
        // guard against counting a face's own edges as shared.
        if set_a.contains(i) {
            continue;
        }
        let Some(face) = faces.get(*i) else { continue };
        for wire in face.boundaries() {
            for edge in wire.edge_iter() {
                if ids_a.contains(&edge.id()) && seen.insert(edge.id()) {
                    result.push(edge.clone());
                }
            }
        }
    }
    result
}

/// How many faces and edges a provenance path resolves to on `solid`.
/// Face paths resolve to faces; `SharedEdge` paths resolve to the kernel
/// edges shared by their two operand face sets. `(0, 0)` = unresolved.
pub fn match_counts(solid: &KernelSolid, path: &ProvenancePath) -> (usize, usize) {
    match path.canonical() {
        ProvenancePath::SharedEdge { a, b } => {
            let faces = solid_faces(&solid.solid);
            let set_a = faces_with_path(solid, &a);
            let set_b = faces_with_path(solid, &b);
            if set_a.is_empty() || set_b.is_empty() {
                return (0, 0);
            }
            (0, shared_edges_between(&faces, &set_a, &set_b).len())
        }
        other => (faces_with_path(solid, &other).len(), 0),
    }
}

// ---------------------------------------------------------------------
// Provenance-query expansion (docs/ARCHITECTURE.md §3.5, tier 1).
// ---------------------------------------------------------------------

/// The concrete provenance paths a query expands to on one solid.
/// Face-valued queries fill `face_paths`, edge-valued ones
/// `edge_paths`; a `Union` may fill both. **Empty expansion is valid**
/// (e.g. `HolesOnly` on a face without holes) — only paths that resolve
/// on the *current* topology are emitted, which is what makes query
/// membership live: it re-derives on every evaluation.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct QueryExpansion {
    pub face_paths: Vec<ProvenancePath>,
    pub edge_paths: Vec<ProvenancePath>,
}

impl QueryExpansion {
    fn push_face(&mut self, path: ProvenancePath) {
        if !self.face_paths.contains(&path) {
            self.face_paths.push(path);
        }
    }

    fn push_edge(&mut self, path: ProvenancePath) {
        if !self.edge_paths.contains(&path) {
            self.edge_paths.push(path);
        }
    }
}

/// Profile-edge sources of the sweep model selected by `filter`, in
/// wire order (outer wire first, then hole wires). Grouped per wire so
/// adjacency (`VerticalEdges`) stays within one wire.
fn filtered_wire_sources(model: &SweepModel, filter: WireFilter) -> Vec<Vec<EntityId>> {
    let (outer, holes) = match model {
        SweepModel::Extrusion { outer, holes, .. }
        | SweepModel::Revolve { outer, holes, .. } => (outer, holes),
    };
    let mut wires: Vec<&WireSpec> = Vec::new();
    match filter {
        WireFilter::All => {
            wires.push(outer);
            wires.extend(holes.iter());
        }
        WireFilter::OuterOnly => wires.push(outer),
        WireFilter::HolesOnly => wires.extend(holes.iter()),
    }
    wires
        .into_iter()
        .map(|wire| {
            wire.sources
                .iter()
                .copied()
                .filter(|source| *source != EntityId::INVALID)
                .collect()
        })
        .collect()
}

/// Expand a provenance query against a solid's current topology.
/// Requires a sweep model (extrusion/revolve lineage);
/// [`KernelError::Unresolved`] otherwise. Emits only paths that resolve
/// right now; empty expansions are valid results.
pub fn expand_query(
    solid: &KernelSolid,
    query: &ProvenanceQuery,
) -> Result<QueryExpansion, KernelError> {
    let Some(model) = &solid.model else {
        return Err(KernelError::Unresolved(
            "owner has no sweep provenance model (queries need an \
             extrusion/revolve lineage)"
                .to_owned(),
        ));
    };
    let mut expansion = QueryExpansion::default();
    expand_into(solid, model, query, &mut expansion);
    Ok(expansion)
}

fn cap_path(cap: CapId) -> ProvenancePath {
    match cap {
        CapId::Start => ProvenancePath::CapStart,
        CapId::End => ProvenancePath::CapEnd,
    }
}

fn expand_into(
    solid: &KernelSolid,
    model: &SweepModel,
    query: &ProvenanceQuery,
    expansion: &mut QueryExpansion,
) {
    match query {
        ProvenanceQuery::SideFaces { wires } => {
            for wire in filtered_wire_sources(model, *wires) {
                for source in wire {
                    let path = ProvenancePath::Side { source };
                    if match_counts(solid, &path).0 > 0 {
                        expansion.push_face(path);
                    }
                }
            }
        }
        ProvenanceQuery::Caps => {
            for path in [ProvenancePath::CapStart, ProvenancePath::CapEnd] {
                if match_counts(solid, &path).0 > 0 {
                    expansion.push_face(path);
                }
            }
        }
        ProvenanceQuery::RimEdges { cap, wires } => {
            let cap = cap_path(*cap);
            for wire in filtered_wire_sources(model, *wires) {
                for source in wire {
                    let path = ProvenancePath::shared_edge(
                        cap.clone(),
                        ProvenancePath::Side { source },
                    );
                    if match_counts(solid, &path).1 > 0 {
                        expansion.push_edge(path);
                    }
                }
            }
        }
        ProvenanceQuery::VerticalEdges { wires } => {
            // Side∩side edges between DISTINCT profile edges of the same
            // wire (adjacent sides share the swept junction edge).
            for wire in filtered_wire_sources(model, *wires) {
                for (i, a) in wire.iter().enumerate() {
                    for b in wire.iter().skip(i + 1) {
                        let path = ProvenancePath::shared_edge(
                            ProvenancePath::Side { source: *a },
                            ProvenancePath::Side { source: *b },
                        );
                        if match_counts(solid, &path).1 > 0 {
                            expansion.push_edge(path);
                        }
                    }
                }
            }
        }
        ProvenanceQuery::Union(members) => {
            for member in members {
                expand_into(solid, model, member, expansion);
            }
        }
    }
}

// ---------------------------------------------------------------------
// Chamfer (monstertruck-fillet, Chamfer profile — docs §5.2).
// ---------------------------------------------------------------------

/// Address of solid edges to chamfer. Provenance-based, never an index
/// (docs/ARCHITECTURE.md §3.4).
#[derive(Debug, Clone, PartialEq)]
pub enum EdgeAddress {
    /// A generated edge: the intersection of two provenance-named faces
    /// (a `ProvenancePath::SharedEdge`, canonicalized by the caller or
    /// here).
    Shared {
        a: ProvenancePath,
        b: ProvenancePath,
    },
    /// Solid edges geometrically coincident with an authored curve (an
    /// `Edge` entity's evaluated geometry) — e.g. an extrusion's bottom
    /// rim segment coincides with its profile edge.
    Coincident(CurveSpec),
}

/// Chamfer (flat bevel) the addressed edges of `target` by `distance`
/// meters. Straight edges between planar faces are the supported class
/// (docs §5.2); curved-edge failures surface as typed errors. The output
/// solid's provenance is re-derived: surviving faces keep their upstream
/// names (anti-topological-naming rule), and each blend face is named by
/// the `SharedEdge` path of the edge it replaces.
pub fn chamfer_solid(
    target: &KernelSolid,
    addresses: &[EdgeAddress],
    distance: f64,
    tol: f64,
) -> Result<KernelSolid, KernelError> {
    guard(|| {
        if distance <= tol {
            return Err(KernelError::Degenerate(format!(
                "chamfer distance {distance} m is not positive"
            )));
        }
        if addresses.is_empty() {
            return Err(KernelError::Degenerate(
                "chamfer has no edges to blend".to_owned(),
            ));
        }
        let boundaries = target.solid.boundaries();
        let [shell] = boundaries.as_slice() else {
            return Err(KernelError::Unsupported(
                "chamfer supports single-shell solids only (v1)".to_owned(),
            ));
        };
        let faces = solid_faces(&target.solid);

        // Resolve every address to kernel edges, remembering per selected
        // edge the blend name (the SharedEdge path of the edge) and
        // sample points (for blend-face attribution afterwards).
        let mut selected: Vec<MtEdge> = Vec::new();
        let mut seen_ids = std::collections::HashSet::new();
        let mut blend_info: Vec<(Option<ProvenancePath>, Vec<[f64; 3]>)> = Vec::new();
        for address in addresses {
            let (edges, blend_path) = match address {
                EdgeAddress::Shared { a, b } => {
                    let set_a = faces_with_path(target, a);
                    let set_b = faces_with_path(target, b);
                    if set_a.is_empty() || set_b.is_empty() {
                        return Err(KernelError::Unresolved(format!(
                            "SharedEdge operand resolves to no face \
                             ({:?} -> {} faces, {:?} -> {} faces)",
                            a,
                            set_a.len(),
                            b,
                            set_b.len()
                        )));
                    }
                    let edges = shared_edges_between(&faces, &set_a, &set_b);
                    if edges.is_empty() {
                        return Err(KernelError::Unresolved(format!(
                            "faces named {a:?} and {b:?} share no edge"
                        )));
                    }
                    (
                        edges,
                        Some(ProvenancePath::shared_edge(a.clone(), b.clone())),
                    )
                }
                EdgeAddress::Coincident(curve) => {
                    let edges = coincident_edges(shell, curve);
                    if edges.is_empty() {
                        return Err(KernelError::Unresolved(
                            "no solid edge coincides with the authored edge's curve"
                                .to_owned(),
                        ));
                    }
                    // Blend name: the SharedEdge of the two adjacent
                    // named faces, when both are named.
                    let blend =
                        adjacent_face_paths(&faces, &target.provenance, &edges).map(
                            |(a, b)| ProvenancePath::shared_edge(a, b),
                        );
                    (edges, blend)
                }
            };
            for edge in edges {
                if seen_ids.insert(edge.id()) {
                    let samples = edge_samples(&edge);
                    blend_info.push((blend_path.clone(), samples));
                    selected.push(edge);
                }
            }
        }

        // Apply the chamfer (flat bevel) via monstertruck-fillet.
        let mut new_shell: MtShell = shell.clone();
        let options =
            FilletOptions::constant(distance).with_profile(FilletProfile::Chamfer);
        fillet_edges(&mut new_shell, &selected, Some(&options))
            .map_err(|e| KernelError::Topology(format!("chamfer failed: {e:?}")))?;
        let solid = MtSolid::try_new(vec![new_shell])
            .map_err(|e| KernelError::Topology(format!("chamfered shell invalid: {e}")))?;

        // Re-derive provenance: (1) sweep-model membership (surviving and
        // trimmed faces keep their upstream names), (2) plane match
        // against the pre-chamfer faces (former blend faces in chamfer
        // chains), (3) nearest chamfered edge (new blend faces).
        let out_faces = solid_faces(&solid);
        let mut provenance: Vec<Option<ProvenancePath>> = match &target.model {
            Some(model) => classify_solid(&solid, model),
            None => vec![None; out_faces.len()],
        };
        for (index, slot) in provenance.iter_mut().enumerate() {
            if slot.is_some() {
                continue;
            }
            let Some(face) = out_faces.get(index) else {
                continue;
            };
            let samples = face_boundary_samples(face);
            if let Some(path) = plane_match(&samples, face, &faces, &target.provenance) {
                *slot = Some(path);
                continue;
            }
            // Nearest chamfered edge, within the chamfer's reach.
            let centroid = average(&samples);
            let mut best: Option<(f64, &Option<ProvenancePath>)> = None;
            for (path, edge_pts) in &blend_info {
                let d = edge_pts
                    .iter()
                    .map(|p| norm(sub(centroid, *p)))
                    .fold(f64::INFINITY, f64::min);
                if best.as_ref().is_none_or(|(bd, _)| d < *bd) {
                    best = Some((d, path));
                }
            }
            if let Some((d, path)) = best
                && d <= distance * 4.0
            {
                *slot = path.clone();
            }
        }

        Ok(KernelSolid {
            solid,
            provenance,
            model: target.model.clone(),
        })
    })
}

/// Sample points along a kernel edge (endpoints + interior points).
fn edge_samples(edge: &MtEdge) -> Vec<[f64; 3]> {
    let mut points = Vec::new();
    let front = edge.front().point();
    let back = edge.back().point();
    points.push([front.x, front.y, front.z]);
    points.push([back.x, back.y, back.z]);
    let curve = edge.curve();
    let (t0, t1) = curve.range_tuple();
    for f in [0.25, 0.5, 0.75] {
        let p = curve.subs(t0 + f * (t1 - t0));
        points.push([p.x, p.y, p.z]);
    }
    points
}

/// Solid edges whose sample points all lie on `curve` (within the
/// classification tolerance) — authored-edge coincidence matching.
fn coincident_edges(shell: &MtShell, curve: &CurveSpec) -> Vec<MtEdge> {
    let mut seen = std::collections::HashSet::new();
    let mut result = Vec::new();
    for edge in shell.edge_iter() {
        if !seen.insert(edge.id()) {
            continue;
        }
        let on_curve = edge_samples(&edge)
            .iter()
            .all(|p| curve_distance_3d(curve, *p) <= CLASSIFY_TOL);
        if on_curve {
            result.push(edge.clone());
        }
    }
    result
}

/// The provenance paths of the two faces adjacent to `edges` (used to
/// name the blend when the edge came from coincidence matching). `None`
/// unless exactly two distinct named faces are adjacent.
fn adjacent_face_paths(
    faces: &[MtFace],
    provenance: &[Option<ProvenancePath>],
    edges: &[MtEdge],
) -> Option<(ProvenancePath, ProvenancePath)> {
    let first = edges.first()?;
    let id = first.id();
    let mut adjacent: Vec<ProvenancePath> = Vec::new();
    for (index, face) in faces.iter().enumerate() {
        let touches = face
            .boundaries()
            .iter()
            .any(|w| w.edge_iter().any(|e| e.id() == id));
        if touches
            && let Some(Some(path)) = provenance.get(index)
            && !adjacent.contains(path)
        {
            adjacent.push(path.clone());
        }
    }
    match adjacent.as_slice() {
        [a, b] => Some((a.clone(), b.clone())),
        _ => None,
    }
}

/// Match a (planar) face against the pre-operation faces by plane
/// (normal direction + offset): inherits the name of an unchanged or
/// trimmed planar face whose plane it shares.
fn plane_match(
    samples: &[[f64; 3]],
    face: &MtFace,
    previous_faces: &[MtFace],
    previous_provenance: &[Option<ProvenancePath>],
) -> Option<ProvenancePath> {
    let normal = face.oriented_surface().normal(0.5, 0.5);
    let normal = normalized([normal.x, normal.y, normal.z], 0.0)?;
    let anchor = *samples.first()?;
    // Only planar faces participate.
    if !samples
        .iter()
        .all(|p| dot(sub(*p, anchor), normal).abs() <= CLASSIFY_TOL)
    {
        return None;
    }
    for (index, prev) in previous_faces.iter().enumerate() {
        let Some(Some(path)) = previous_provenance.get(index) else {
            continue;
        };
        let prev_normal = prev.oriented_surface().normal(0.5, 0.5);
        let Some(prev_normal) = normalized([prev_normal.x, prev_normal.y, prev_normal.z], 0.0)
        else {
            continue;
        };
        if cross(normal, prev_normal).iter().map(|c| c.abs()).sum::<f64>() > 1e-9 {
            continue;
        }
        let prev_samples = face_boundary_samples(prev);
        let Some(prev_anchor) = prev_samples.first() else {
            continue;
        };
        if dot(sub(*prev_anchor, anchor), normal).abs() <= CLASSIFY_TOL {
            return Some(path.clone());
        }
    }
    None
}

fn average(points: &[[f64; 3]]) -> [f64; 3] {
    if points.is_empty() {
        return [0.0; 3];
    }
    let sum = points.iter().fold([0.0; 3], |acc, p| add(acc, *p));
    scale(sum, 1.0 / points.len() as f64)
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
    // Empty meshes are legal here (degenerate sliver faces); callers
    // needing non-emptiness (whole-solid tessellation) check themselves.

    // Fill any missing normals from the geometry of the first triangle
    // that references the vertex.
    for &[a, b, c] in indices.as_chunks::<3>().0 {
        let (a, b, c) = (a as usize, b as usize, c as usize);
        let needs = [a, b, c]
            .iter()
            .any(|&i| normals_opt.get(i).is_some_and(|n| n.is_none()));
        if !needs {
            continue;
        }
        let (Some(pa), Some(pb), Some(pc)) = (positions.get(a), positions.get(b), positions.get(c))
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
            if let Some(slot) = normals_opt.get_mut(i)
                && slot.is_none()
            {
                *slot = Some(n);
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
        let curves: Vec<CurveSpec> = corners
            .windows(2)
            .filter_map(|pair| match pair {
                [start, end] => Some(CurveSpec::Segment {
                    start: *start,
                    end: *end,
                }),
                _ => None,
            })
            .collect();
        // Synthetic source ids 101.. so provenance tests can name sides.
        let sources = (0..curves.len()).map(|i| EntityId(101 + i as u64)).collect();
        WireSpec::with_sources(curves, sources)
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
        let wire = WireSpec::from_curves(vec![
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
        ]);
        assert_eq!(
            make_face(&wire, &[], None, 1e-6).err(),
            Some(KernelError::NotPlanar)
        );
    }

    #[test]
    fn extrusion_provenance_names_caps_and_sides() {
        let Ok(face) = make_face(&square_wire(1.0, 0.0), &[], None, 1e-6) else {
            unreachable!("square face must build");
        };
        let Ok(solid) = extrude_solid(&face, [0.0, 0.0, 1.0], 1e-6) else {
            unreachable!("extrude must succeed");
        };
        assert_eq!(solid.face_count(), 6);
        let paths = solid.face_paths();
        let caps = paths
            .iter()
            .filter(|p| {
                matches!(
                    p,
                    Some(ProvenancePath::CapStart) | Some(ProvenancePath::CapEnd)
                )
            })
            .count();
        assert_eq!(caps, 2, "one start + one end cap: {paths:?}");
        // Each synthetic source edge 101..=104 names exactly one side.
        for source in (101..=104).map(EntityId) {
            let named = paths
                .iter()
                .filter(|p| **p == Some(ProvenancePath::Side { source }))
                .count();
            assert_eq!(named, 1, "side for {source:?}: {paths:?}");
        }
        // match_counts: faces for face-paths, edges for SharedEdge paths.
        assert_eq!(
            match_counts(&solid, &ProvenancePath::CapEnd),
            (1, 0)
        );
        let rim = ProvenancePath::shared_edge(
            ProvenancePath::CapEnd,
            ProvenancePath::Side {
                source: EntityId(101),
            },
        );
        assert_eq!(match_counts(&solid, &rim), (0, 1));
        // Unresolvable path: source edge id that never existed.
        assert_eq!(
            match_counts(
                &solid,
                &ProvenancePath::Side {
                    source: EntityId(999)
                }
            ),
            (0, 0)
        );
        // tessellate_faces aligns with the provenance vector.
        let Ok(meshes) = tessellate_faces(&solid, 1e-3) else {
            unreachable!("tessellation must succeed");
        };
        assert_eq!(meshes.len(), 6);
        assert!(meshes.iter().all(|m| m.triangle_count() == 2));
    }

    #[test]
    fn chamfer_blends_a_rim_edge_and_propagates_provenance() {
        let Ok(face) = make_face(&square_wire(1.0, 0.0), &[], None, 1e-6) else {
            unreachable!("square face must build");
        };
        let Ok(solid) = extrude_solid(&face, [0.0, 0.0, 1.0], 1e-6) else {
            unreachable!("extrude must succeed");
        };
        let side = ProvenancePath::Side {
            source: EntityId(101),
        };
        let rim = EdgeAddress::Shared {
            a: ProvenancePath::CapEnd,
            b: side.clone(),
        };
        let Ok(chamfered) = chamfer_solid(&solid, &[rim], 0.1, 1e-6) else {
            unreachable!("straight-edge chamfer must succeed");
        };
        assert_eq!(chamfered.face_count(), 7, "one blend face added");
        let paths = chamfered.face_paths();
        // Trimmed faces keep their names.
        assert!(paths.contains(&Some(ProvenancePath::CapEnd)));
        assert!(paths.contains(&Some(side.clone())));
        // The blend face is named by the edge it replaces.
        let blend = ProvenancePath::shared_edge(ProvenancePath::CapEnd, side);
        assert!(
            paths.contains(&Some(blend)),
            "blend named by its SharedEdge: {paths:?}"
        );
        // Unresolvable chamfer address → typed Unresolved error.
        let bogus = EdgeAddress::Shared {
            a: ProvenancePath::CapStart,
            b: ProvenancePath::Side {
                source: EntityId(999),
            },
        };
        assert!(matches!(
            chamfer_solid(&solid, &[bogus], 0.1, 1e-6),
            Err(KernelError::Unresolved(_))
        ));
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

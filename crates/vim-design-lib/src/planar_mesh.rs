//! Direct meshing of planar solids built from 2D data (sketch prisms,
//! walls, wall runs).
//!
//! The kernel tessellates each BREP face on its own, including faces no
//! one can see: stacked sketch layers and adjacent wall-run pieces touch
//! along coincident, opposite-facing faces. This mesher works on the
//! plain-data faces of all solids of one member together:
//!
//! 1. vertices closer than the tolerance snap to one position; faces are
//!    grouped by plane; faces on one plane with the same orientation and
//!    provenance name are merged (a side split into stacked bands becomes
//!    one face), and boundary points on a straight line between their
//!    neighbours are dropped;
//! 2. on each plane, every region is reduced by the union of the
//!    opposite-facing regions (internal faces vanish, a partly covered
//!    cap keeps only its exposed part);
//! 3. every boundary edge gets the vertices of other faces that lie on
//!    it, so neighbouring faces share their boundary vertices exactly (no
//!    T-junctions, no cracks);
//! 4. each face is triangulated once (constrained Delaunay, `i_triangle`)
//!    with its exact input vertices and a flat normal.
//!
//! A planar face with `n` boundary vertices and `h` holes becomes
//! `n + 2h - 2` triangles; a rectangle plate is 12 triangles.

use i_overlay::core::fill_rule::FillRule;
use i_overlay::core::overlay_rule::OverlayRule;
use i_overlay::core::solver::Solver;
use i_overlay::float::overlay::{FloatOverlay, OverlayOptions};
use i_triangle::float::triangulatable::Triangulatable;

use crate::kernel::{PlanarFace, RawMesh, cross, dot, norm, normalized};
use crate::subref::ProvenancePath;

type P2 = [f64; 2];
type P3 = [f64; 3];

/// Coincidence tolerance for planes and vertices (meters).
const TOL: f64 = 1e-7;

fn sub(a: P3, b: P3) -> P3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn add(a: P3, b: P3) -> P3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

fn scale(a: P3, s: f64) -> P3 {
    [a[0] * s, a[1] * s, a[2] * s]
}

fn dist(a: P3, b: P3) -> f64 {
    norm(sub(a, b))
}

/// A plane with a deterministic 2D basis. `normal` is the canonical
/// orientation (first significant component positive).
struct Plane {
    normal: P3,
    offset: f64,
    u: P3,
    v: P3,
}

impl Plane {
    fn to_2d(&self, p: P3) -> P2 {
        [dot(p, self.u), dot(p, self.v)]
    }

    fn to_3d(&self, q: P2) -> P3 {
        add(
            scale(self.normal, self.offset),
            add(scale(self.u, q[0]), scale(self.v, q[1])),
        )
    }
}

/// Canonical plane of a face and the face's side (+1 when the face
/// normal equals the canonical normal).
fn canonical(normal: P3, point: P3) -> (P3, f64, f64) {
    let significant = normal.iter().copied().find(|c| c.abs() > 1e-9).unwrap_or(1.0);
    let sign = if significant < 0.0 { -1.0 } else { 1.0 };
    let n = scale(normal, sign);
    (n, dot(n, point), sign)
}

fn basis(n: P3) -> (P3, P3) {
    let [x, y, z] = n;
    let helper = if x.abs() <= y.abs() && x.abs() <= z.abs() {
        [1.0, 0.0, 0.0]
    } else if y.abs() <= z.abs() {
        [0.0, 1.0, 0.0]
    } else {
        [0.0, 0.0, 1.0]
    };
    let u = normalized(cross(n, helper), 0.0).unwrap_or([1.0, 0.0, 0.0]);
    (u, cross(n, u))
}

fn signed_area(polygon: &[P2]) -> f64 {
    let n = polygon.len();
    polygon
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let q = polygon.get((i + 1) % n.max(1)).copied().unwrap_or(*p);
            p[0] * q[1] - q[0] * p[1]
        })
        .sum::<f64>()
        / 2.0
}

type Shape = Vec<Vec<P2>>;

fn boolean(subject: &[Vec<P2>], clip: &[Vec<P2>], rule: OverlayRule) -> Result<Vec<Shape>, String> {
    let run = || {
        // Collinear points are dropped: the conformance step re-inserts
        // exactly the ones a neighbouring face needs.
        let mut options: OverlayOptions<f64, i64> = OverlayOptions::default();
        options.min_output_area = TOL * TOL;
        options.ogc = true;
        let (subject, clip) = (subject.to_vec(), clip.to_vec());
        FloatOverlay::<P2, i64>::from_subj_and_clip_custom(&subject, &clip, options, Solver::default())
            .overlay(rule, FillRule::NonZero)
    };
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(run))
        .map_err(|_| "polygon boolean failed".to_owned())
}

/// A merged, exposed face region on one plane.
struct Region {
    plane: usize,
    sign: f64,
    path: Option<ProvenancePath>,
    shapes: Vec<Shape>,
}

/// Distinct vertices (no two within tolerance) on a hash grid, for
/// snapping near-coincident points to one exact position.
struct VertexGrid {
    cells: std::collections::HashMap<[i64; 3], Vec<usize>>,
    points: Vec<P3>,
}

impl VertexGrid {
    fn new() -> VertexGrid {
        VertexGrid { cells: std::collections::HashMap::new(), points: Vec::new() }
    }

    fn cell(p: P3) -> [i64; 3] {
        [(p[0] / TOL).floor() as i64, (p[1] / TOL).floor() as i64, (p[2] / TOL).floor() as i64]
    }

    /// The nearest stored vertex within tolerance.
    fn nearest(&self, p: P3) -> Option<P3> {
        let [x, y, z] = Self::cell(p);
        let mut best: Option<(f64, P3)> = None;
        for dx in -1..=1 {
            for dy in -1..=1 {
                for dz in -1..=1 {
                    let Some(ids) = self.cells.get(&[x + dx, y + dy, z + dz]) else { continue };
                    for q in ids.iter().filter_map(|i| self.points.get(*i)) {
                        let d = dist(*q, p);
                        if d <= TOL && best.is_none_or(|(bd, _)| d < bd) {
                            best = Some((d, *q));
                        }
                    }
                }
            }
        }
        best.map(|(_, q)| q)
    }

    /// Store `p` unless a vertex within tolerance is stored already.
    fn insert(&mut self, p: P3) {
        if self.nearest(p).is_none() {
            self.cells.entry(Self::cell(p)).or_default().push(self.points.len());
            self.points.push(p);
        }
    }

    /// `p` snapped to the nearest stored vertex within tolerance.
    fn snap(&self, p: P3) -> P3 {
        self.nearest(p).unwrap_or(p)
    }
}

/// A ring without vertices that lie on the straight line between their
/// neighbours (within tolerance): a boolean keeps such points where the
/// input seams were not exactly collinear.
fn without_collinear(mut ring: Vec<P3>) -> Vec<P3> {
    let mut changed = true;
    while changed && ring.len() > 3 {
        changed = false;
        let n = ring.len();
        for i in 0..n {
            let (Some(a), Some(p), Some(b)) = (ring.get((i + n - 1) % n), ring.get(i), ring.get((i + 1) % n)) else {
                continue;
            };
            let ab = sub(*b, *a);
            let len2 = dot(ab, ab);
            if len2 <= TOL * TOL {
                continue;
            }
            let t = dot(sub(*p, *a), ab) / len2;
            if t > 0.0 && t < 1.0 && dist(*p, add(*a, scale(ab, t))) <= TOL {
                ring.remove(i);
                changed = true;
                break;
            }
        }
    }
    ring
}

/// A ring without consecutive (or closing) repeats within tolerance.
fn without_repeats(ring: Vec<P3>) -> Vec<P3> {
    let mut out: Vec<P3> = Vec::with_capacity(ring.len());
    for p in ring {
        if out.last().is_none_or(|q| dist(*q, p) > TOL) {
            out.push(p);
        }
    }
    while out.len() > 1 && matches!((out.first(), out.last()), (Some(a), Some(b)) if dist(*a, *b) <= TOL) {
        out.pop();
    }
    out
}

/// Insert onto each edge of `ring` the vertices of `all` that lie on it.
fn conform(ring: &[P3], all: &[P3]) -> Vec<P3> {
    let n = ring.len();
    let mut out = Vec::with_capacity(n);
    for (i, a) in ring.iter().enumerate() {
        out.push(*a);
        let Some(b) = ring.get((i + 1) % n) else { continue };
        let ab = sub(*b, *a);
        let len2 = dot(ab, ab);
        if len2 <= TOL * TOL {
            continue;
        }
        let mut inner: Vec<(f64, P3)> = all
            .iter()
            .filter_map(|p| {
                let t = dot(sub(*p, *a), ab) / len2;
                let on_line = dist(*p, add(*a, scale(ab, t))) <= TOL;
                let strictly_inside = t * len2.sqrt() > TOL && (1.0 - t) * len2.sqrt() > TOL;
                (on_line && strictly_inside).then_some((t, *p))
            })
            .collect();
        inner.sort_by(|x, y| x.0.total_cmp(&y.0));
        inner.dedup_by(|x, y| dist(x.1, y.1) <= TOL);
        out.extend(inner.into_iter().map(|(_, p)| p));
    }
    out
}

/// Triangulate one polygon-with-holes (2D, exact vertices kept) into
/// index triples over `points`.
fn triangulate(rings: &[Vec<P2>]) -> Result<(Vec<P2>, Vec<[u32; 3]>), String> {
    let points: Vec<P2> = rings.iter().flatten().copied().collect();
    let run = || {
        let shape: Vec<Vec<P2>> = rings.to_vec();
        shape.triangulate_as::<i64>().into_delaunay().to_triangulation::<u32>()
    };
    let triangulation = std::panic::catch_unwind(std::panic::AssertUnwindSafe(run))
        .map_err(|_| "triangulation failed".to_owned())?;
    let nearest = |q: &P2| -> Option<u32> {
        points
            .iter()
            .enumerate()
            .min_by(|(_, a), (_, b)| {
                let da = (a[0] - q[0]).powi(2) + (a[1] - q[1]).powi(2);
                let db = (b[0] - q[0]).powi(2) + (b[1] - q[1]).powi(2);
                da.total_cmp(&db)
            })
            .and_then(|(i, _)| u32::try_from(i).ok())
    };
    let map: Vec<u32> = triangulation
        .points
        .iter()
        .map(|q| nearest(q).ok_or_else(|| "empty polygon".to_owned()))
        .collect::<Result<_, _>>()?;
    let mut triangles: Vec<[u32; 3]> = triangulation
        .indices
        .as_chunks::<3>()
        .0
        .iter()
        .filter_map(|[a, b, c]| {
            Some([
                *map.get(*a as usize)?,
                *map.get(*b as usize)?,
                *map.get(*c as usize)?,
            ])
        })
        .collect();

    // Boundary points the triangulator merged away (collinear): split the
    // triangle whose edge carries each one.
    let used: std::collections::BTreeSet<u32> = triangles.iter().flatten().copied().collect();
    let point = |i: u32| points.get(i as usize).copied().unwrap_or([f64::NAN; 2]);
    for (index, p) in points.iter().enumerate() {
        let Ok(index) = u32::try_from(index) else { continue };
        if used.contains(&index) {
            continue;
        }
        let on_edge = |a: u32, b: u32| {
            let (pa, pb) = (point(a), point(b));
            let ab = [pb[0] - pa[0], pb[1] - pa[1]];
            let len2 = ab[0] * ab[0] + ab[1] * ab[1];
            if len2 <= TOL * TOL {
                return false;
            }
            let t = ((p[0] - pa[0]) * ab[0] + (p[1] - pa[1]) * ab[1]) / len2;
            let closest = [pa[0] + ab[0] * t, pa[1] + ab[1] * t];
            let off = ((p[0] - closest[0]).powi(2) + (p[1] - closest[1]).powi(2)).sqrt();
            off <= TOL && t > 0.0 && t < 1.0
        };
        let hit = triangles.iter().enumerate().find_map(|(ti, [a, b, c])| {
            [(*a, *b, *c), (*b, *c, *a), (*c, *a, *b)]
                .into_iter()
                .find(|(x, y, _)| on_edge(*x, *y))
                .map(|edge| (ti, edge))
        });
        if let Some((ti, (x, y, z))) = hit {
            if let Some(slot) = triangles.get_mut(ti) {
                *slot = [x, index, z];
            }
            triangles.push([index, y, z]);
        }
    }
    Ok((points, triangles))
}

/// Mesh the faces of one member: one `(name, mesh)` entry per exposed
/// face region.
pub(crate) fn mesh_planar_faces(
    faces: &[PlanarFace],
) -> Result<Vec<(Option<ProvenancePath>, RawMesh)>, String> {
    // 1. Planes.
    let mut planes: Vec<Plane> = Vec::new();
    let mut face_plane: Vec<(usize, f64)> = Vec::with_capacity(faces.len());
    for face in faces {
        let anchor = face
            .loops
            .first()
            .and_then(|l| l.first())
            .copied()
            .ok_or_else(|| "empty face".to_owned())?;
        let (n, offset, sign) = canonical(face.normal, anchor);
        let found = planes.iter().position(|p| {
            dist(p.normal, n) <= 1e-9 && (p.offset - offset).abs() <= TOL
        });
        let index = match found {
            Some(index) => index,
            None => {
                let (u, v) = basis(n);
                planes.push(Plane {
                    normal: n,
                    offset,
                    u,
                    v,
                });
                planes.len() - 1
            }
        };
        face_plane.push((index, sign));
    }

    // 2. Merge same-side same-name faces per plane, then remove what the
    //    opposite side covers. Near-coincident input vertices snap to one
    //    position first, so the booleans see shared edges exactly.
    let mut known = VertexGrid::new();
    for p in faces.iter().flat_map(|f| f.loops.iter().flatten()) {
        known.insert(*p);
    }
    let mut regions: Vec<Region> = Vec::new();
    for (plane_index, plane) in planes.iter().enumerate() {
        // Every ring in this plane's basis, counter-clockwise for outers.
        let rings_of = |sign: f64, path: Option<&Option<ProvenancePath>>| -> Vec<Vec<P2>> {
            faces
                .iter()
                .zip(face_plane.iter())
                .filter(|(f, (pi, s))| {
                    *pi == plane_index && *s == sign && path.is_none_or(|p| &f.path == p)
                })
                .flat_map(|(f, _)| {
                    f.loops.iter().map(|ring| {
                        let mut r: Vec<P2> = ring.iter().map(|p| plane.to_2d(known.snap(*p))).collect();
                        if sign < 0.0 {
                            r.reverse();
                        }
                        r
                    })
                })
                .collect()
        };
        for sign in [1.0, -1.0] {
            let mut names: Vec<Option<ProvenancePath>> = Vec::new();
            for (face, (pi, s)) in faces.iter().zip(face_plane.iter()) {
                if *pi == plane_index && *s == sign && !names.contains(&face.path) {
                    names.push(face.path.clone());
                }
            }
            let opposite = rings_of(-sign, None);
            for name in names {
                let own = rings_of(sign, Some(&name));
                let shapes = boolean(&own, &opposite, OverlayRule::Difference)?;
                if !shapes.is_empty() {
                    regions.push(Region {
                        plane: plane_index,
                        sign,
                        path: name,
                        shapes,
                    });
                }
            }
        }
    }

    // 3. Back to exact 3D rings, then conform shared boundaries.
    let mut rings3: Vec<Vec<Vec<Vec<P3>>>> = Vec::with_capacity(regions.len());
    for region in &regions {
        let Some(plane) = planes.get(region.plane) else { continue };
        rings3.push(
            region
                .shapes
                .iter()
                .map(|shape| {
                    shape
                        .iter()
                        .map(|ring| {
                            without_collinear(without_repeats(
                                ring.iter().map(|q| known.snap(plane.to_3d(*q))).collect(),
                            ))
                        })
                        .filter(|ring: &Vec<P3>| ring.len() >= 3)
                        .collect()
                })
                .collect(),
        );
    }
    let mut distinct = VertexGrid::new();
    for p in rings3.iter().flatten().flatten().flatten() {
        distinct.insert(*p);
    }
    let all = distinct.points;

    // 4. Triangulate.
    let mut out = Vec::with_capacity(regions.len());
    for (region, shapes) in regions.iter().zip(rings3.iter()) {
        let Some(plane) = planes.get(region.plane) else { continue };
        let normal = scale(plane.normal, region.sign);
        let n32 = [normal[0] as f32, normal[1] as f32, normal[2] as f32];
        let mut mesh = RawMesh::default();
        for shape in shapes {
            let conformed: Vec<Vec<P3>> = shape.iter().map(|ring| conform(ring, &all)).collect();
            let rings2: Vec<Vec<P2>> = conformed
                .iter()
                .map(|ring| ring.iter().map(|p| plane.to_2d(*p)).collect())
                .collect();
            let (points2, triangles) = triangulate(&rings2)?;
            let points3: Vec<P3> = conformed.iter().flatten().copied().collect();
            let base = u32::try_from(mesh.positions.len()).map_err(|_| "mesh too large")?;
            for p in &points3 {
                mesh.positions.push([p[0] as f32, p[1] as f32, p[2] as f32]);
                mesh.normals.push(n32);
            }
            for [a, b, c] in triangles {
                let pa = points2.get(a as usize).copied().unwrap_or([0.0; 2]);
                let pb = points2.get(b as usize).copied().unwrap_or([0.0; 2]);
                let pc = points2.get(c as usize).copied().unwrap_or([0.0; 2]);
                let area = signed_area(&[pa, pb, pc]);
                if area.abs() <= TOL * TOL {
                    continue; // degenerate sliver
                }
                // Counter-clockwise around the face normal.
                let (b, c) = if (area > 0.0) == (region.sign > 0.0) { (b, c) } else { (c, b) };
                mesh.indices.extend([base + a, base + b, base + c]);
            }
        }
        if !mesh.indices.is_empty() {
            out.push((region.path.clone(), mesh));
        }
    }
    Ok(out)
}

//! Layered evaluation of a sketch into prism footprints.
//!
//! Material hangs from the plane. At a point of the plane, the solid
//! interval is [0, thickest solid face covering it] and the removed
//! interval is [0, deepest void face covering it] (a void without depth
//! removes everything). Every distinct thickness and depth is a
//! breakpoint; between consecutive breakpoints `a < b` the footprint is
//!
//! ```text
//! union(solid faces with thickness >= b) - union(void faces with depth >= b or none)
//! ```
//!
//! and each polygon of it becomes a prism from depth `a` to `b`. A polygon
//! that repeats unchanged in the next layer extends the prism instead of
//! starting a new one, so a plain plate is one prism however many
//! breakpoints other faces add.
//!
//! The 2D booleans run in `i_overlay` (64-bit integer engine: the float
//! input is snapped to a grid far finer than the point tolerance).
//! Output slivers under the tolerance area are dropped.

use i_overlay::core::fill_rule::FillRule;
use i_overlay::core::overlay_rule::OverlayRule;
use i_overlay::core::solver::Solver;
use i_overlay::float::overlay::{FloatOverlay, OverlayOptions};

use super::geom::{self, P2};
use super::{POINT_TOLERANCE, Sketch, SketchError, SketchFaceKind, validate};

/// Coordinates beyond this magnitude (meters) are rejected before the
/// booleans: far outside any building, and inside the range the overlay
/// engine accepts without panicking.
const MAX_COORDINATE: f64 = 1.0e9;

/// The sketch edge a prism side comes from: the producing face and the
/// edge's point pair (sorted).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SideSource {
    pub face: u32,
    pub a: u32,
    pub b: u32,
}

/// One boundary loop of a prism footprint. `sides[i]` names the edge from
/// `points[i]` to `points[i + 1]` (wrapping); `None` when no sketch edge
/// carries it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Contour {
    pub points: Vec<P2>,
    pub sides: Vec<Option<SideSource>>,
}

/// A prism: footprint (outer loop counter-clockwise, then holes
/// clockwise) extruded from `top` to `bottom` depth (`top < bottom`,
/// meters away from the plane).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Prism {
    pub contours: Vec<Contour>,
    pub top: f64,
    pub bottom: f64,
}

/// Why a sketch has no prisms.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum LayerError {
    /// The sketch fails structural or geometric validation.
    Invalid(SketchError),
    /// A coordinate is too far from the plane origin.
    OutOfRange,
    /// The boolean engine panicked (reported, never propagated).
    BooleanFailed(String),
}

impl std::fmt::Display for LayerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LayerError::Invalid(err) => write!(f, "{err}"),
            LayerError::OutOfRange => write!(
                f,
                "a sketch coordinate is farther than {MAX_COORDINATE} m from the plane origin"
            ),
            LayerError::BooleanFailed(msg) => write!(f, "polygon boolean failed: {msg}"),
        }
    }
}

struct FaceData {
    id: u32,
    kind: SketchFaceKind,
    /// Counter-clockwise polygon.
    polygon: Vec<P2>,
    /// Loop edges as (point a, point b, uv a, uv b).
    edges: Vec<(u32, u32, P2, P2)>,
}

fn face_data(sketch: &Sketch) -> Result<Vec<FaceData>, LayerError> {
    let mut faces = Vec::with_capacity(sketch.faces.len());
    for face in &sketch.faces {
        let mut polygon = Vec::with_capacity(face.points.len());
        for id in &face.points {
            let uv = sketch.uv(*id).map_err(LayerError::Invalid)?;
            if uv.iter().any(|c| c.abs() > MAX_COORDINATE) {
                return Err(LayerError::OutOfRange);
            }
            polygon.push(uv);
        }
        let n = face.points.len();
        let mut edges = Vec::with_capacity(n);
        for i in 0..n {
            if let (Some(a), Some(b), Some(pa), Some(pb)) = (
                face.points.get(i),
                face.points.get((i + 1) % n),
                polygon.get(i),
                polygon.get((i + 1) % n),
            ) {
                edges.push((*a, *b, *pa, *pb));
            }
        }
        if geom::signed_area(&polygon) < 0.0 {
            polygon.reverse();
        }
        faces.push(FaceData {
            id: face.id,
            kind: face.kind,
            polygon,
            edges,
        });
    }
    Ok(faces)
}

/// Sorted distinct values (merging values closer than the tolerance).
fn breakpoints(faces: &[FaceData]) -> Vec<f64> {
    let mut values = vec![0.0];
    for face in faces {
        match face.kind {
            SketchFaceKind::Solid { thickness } => values.push(thickness),
            SketchFaceKind::Void { depth: Some(depth) } => values.push(depth),
            SketchFaceKind::Void { depth: None } => {}
        }
    }
    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    values.dedup_by(|b, a| (*b - *a).abs() <= POINT_TOLERANCE);
    values
}

fn overlay(subject: &[Vec<P2>], clip: &[Vec<P2>]) -> Result<Vec<Vec<Vec<P2>>>, LayerError> {
    let run = || {
        let mut options: OverlayOptions<f64, i64> = OverlayOptions::default();
        // Keep collinear vertices: every output edge must lie on ONE
        // sketch edge for provenance naming.
        options.preserve_input_collinear = true;
        options.preserve_output_collinear = true;
        options.min_output_area = POINT_TOLERANCE * POINT_TOLERANCE;
        options.ogc = true;
        let subject: Vec<Vec<P2>> = subject.to_vec();
        let clip: Vec<Vec<P2>> = clip.to_vec();
        let mut engine = FloatOverlay::<P2, i64>::from_subj_and_clip_custom(
            &subject,
            &clip,
            options,
            Solver::default(),
        );
        engine.overlay(OverlayRule::Difference, FillRule::NonZero)
    };
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(run)).map_err(|payload| {
        let msg = payload
            .downcast_ref::<&str>()
            .map(|s| (*s).to_owned())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "unknown panic".to_owned());
        LayerError::BooleanFailed(msg)
    })
}

/// Name one output edge: the sketch edge that contains it, preferring
/// faces active in this layer — solids first (the material side names
/// the wall), then voids, then any face — and the lowest face id, then
/// the lowest point pair, among equals.
fn name_edge(
    faces: &[FaceData],
    p: P2,
    q: P2,
    active_solid: &dyn Fn(&FaceData) -> bool,
    active_void: &dyn Fn(&FaceData) -> bool,
) -> Option<SideSource> {
    let mut best: Option<(u8, u32, u32, u32)> = None;
    for face in faces {
        let rank = if active_solid(face) {
            0
        } else if active_void(face) {
            1
        } else {
            2
        };
        for (a, b, pa, pb) in &face.edges {
            if geom::point_segment_distance(p, *pa, *pb) > POINT_TOLERANCE
                || geom::point_segment_distance(q, *pa, *pb) > POINT_TOLERANCE
            {
                continue;
            }
            let key = (rank, face.id, (*a).min(*b), (*a).max(*b));
            if best.is_none_or(|current| key < current) {
                best = Some(key);
            }
        }
    }
    best.map(|(_, face, a, b)| SideSource { face, a, b })
}

fn contours_equal(x: &[P2], y: &[P2]) -> bool {
    if x.len() != y.len() {
        return false;
    }
    let Some(first) = x.first() else {
        return true;
    };
    let n = y.len();
    (0..n).any(|offset| {
        y.get(offset)
            .is_some_and(|start| geom::dist(*start, *first) <= POINT_TOLERANCE)
            && x.iter().enumerate().all(|(i, p)| {
                y.get((i + offset) % n)
                    .is_some_and(|q| geom::dist(*p, *q) <= POINT_TOLERANCE)
            })
    })
}

fn shapes_equal(x: &[Contour], y: &[Contour]) -> bool {
    if x.len() != y.len() {
        return false;
    }
    let (Some(x_outer), Some(y_outer)) = (x.first(), y.first()) else {
        return x.is_empty() && y.is_empty();
    };
    if !contours_equal(&x_outer.points, &y_outer.points) {
        return false;
    }
    x.iter()
        .skip(1)
        .all(|hole| y.iter().skip(1).any(|other| contours_equal(&hole.points, &other.points)))
}

/// The prisms of a sketch. The sketch must be valid (structure and every
/// face's geometry); an empty result means no material remains.
pub(crate) fn layered_prisms(sketch: &Sketch) -> Result<Vec<Prism>, LayerError> {
    validate(sketch).map_err(LayerError::Invalid)?;
    let faces = face_data(sketch)?;
    let max_thickness = faces
        .iter()
        .filter_map(|f| match f.kind {
            SketchFaceKind::Solid { thickness } => Some(thickness),
            SketchFaceKind::Void { .. } => None,
        })
        .fold(0.0_f64, f64::max);
    let levels = breakpoints(&faces);

    let mut finished: Vec<Prism> = Vec::new();
    let mut open: Vec<Prism> = Vec::new();
    for pair in levels.windows(2) {
        let [a, b] = pair else { continue };
        let (a, b) = (*a, *b);
        if b > max_thickness + POINT_TOLERANCE {
            break;
        }
        let active_solid = |f: &FaceData| {
            matches!(f.kind, SketchFaceKind::Solid { thickness } if thickness >= b - POINT_TOLERANCE)
        };
        let active_void = |f: &FaceData| match f.kind {
            SketchFaceKind::Void { depth: None } => true,
            SketchFaceKind::Void { depth: Some(depth) } => depth >= b - POINT_TOLERANCE,
            SketchFaceKind::Solid { .. } => false,
        };
        let subject: Vec<Vec<P2>> = faces
            .iter()
            .filter(|f| active_solid(f))
            .map(|f| f.polygon.clone())
            .collect();
        let clip: Vec<Vec<P2>> = faces
            .iter()
            .filter(|f| active_void(f))
            .map(|f| f.polygon.clone())
            .collect();
        let shapes = if subject.is_empty() {
            Vec::new()
        } else {
            overlay(&subject, &clip)?
        };

        let mut layer: Vec<Vec<Contour>> = Vec::with_capacity(shapes.len());
        for shape in shapes {
            let contours: Vec<Contour> = shape
                .into_iter()
                .filter(|c| c.len() >= 3)
                .map(|points| {
                    let n = points.len();
                    let sides = (0..n)
                        .map(|i| match (points.get(i), points.get((i + 1) % n)) {
                            (Some(p), Some(q)) => {
                                name_edge(&faces, *p, *q, &active_solid, &active_void)
                            }
                            _ => None,
                        })
                        .collect();
                    Contour { points, sides }
                })
                .collect();
            if contours.is_empty() {
                continue;
            }
            layer.push(contours);
        }

        // Extend open prisms whose footprint repeats; close the others.
        let mut next_open: Vec<Prism> = Vec::with_capacity(layer.len());
        for contours in layer {
            let continued = open.iter().position(|prism| {
                (prism.bottom - a).abs() <= POINT_TOLERANCE
                    && shapes_equal(&prism.contours, &contours)
            });
            match continued {
                Some(index) => {
                    let mut prism = open.swap_remove(index);
                    prism.bottom = b;
                    next_open.push(prism);
                }
                None => next_open.push(Prism {
                    contours,
                    top: a,
                    bottom: b,
                }),
            }
        }
        finished.append(&mut open);
        open = next_open;
    }
    finished.append(&mut open);
    finished.sort_by(|x, y| {
        x.top
            .partial_cmp(&y.top)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    Ok(finished)
}

#[cfg(test)]
mod tests {
    use super::super::ops::add_face;
    use super::*;

    fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> Vec<P2> {
        vec![[x0, y0], [x1, y0], [x1, y1], [x0, y1]]
    }

    fn with(faces: &[(Vec<P2>, SketchFaceKind)]) -> Sketch {
        let mut sketch = Sketch::default();
        for (outline, kind) in faces {
            sketch = add_face(&sketch, outline, *kind).unwrap_or_default();
        }
        sketch
    }

    fn solid(t: f64) -> SketchFaceKind {
        SketchFaceKind::Solid { thickness: t }
    }

    fn void(d: Option<f64>) -> SketchFaceKind {
        SketchFaceKind::Void { depth: d }
    }

    fn volume(prisms: &[Prism]) -> f64 {
        prisms
            .iter()
            .map(|p| {
                let area: f64 = p.contours.iter().map(|c| geom::signed_area(&c.points)).sum();
                area * (p.bottom - p.top)
            })
            .sum()
    }

    #[test]
    fn a_plain_plate_is_one_prism_with_named_sides() {
        let sketch = with(&[(rect(0.0, 0.0, 4.0, 3.0), solid(0.3))]);
        let prisms = layered_prisms(&sketch).unwrap_or_default();
        assert_eq!(prisms.len(), 1);
        let prism = prisms.first().cloned();
        assert_eq!(prism.as_ref().map(|p| (p.top, p.bottom)), Some((0.0, 0.3)));
        assert!((volume(&prisms) - 3.6).abs() < 1e-9);
        let sides: Vec<Option<SideSource>> = prism
            .map(|p| p.contours.into_iter().flat_map(|c| c.sides).collect())
            .unwrap_or_default();
        assert_eq!(sides.len(), 4);
        assert!(sides.iter().all(|s| s.is_some_and(|s| s.face == 0)));
    }

    #[test]
    fn thicker_neighbour_adds_a_breakpoint_without_splitting_the_plate() {
        let sketch = with(&[
            (rect(0.0, 0.0, 2.0, 2.0), solid(0.2)),
            (rect(5.0, 0.0, 6.0, 1.0), solid(0.5)),
        ]);
        let prisms = layered_prisms(&sketch).unwrap_or_default();
        assert_eq!(prisms.len(), 2, "each face stays one prism");
        assert!((volume(&prisms) - (4.0 * 0.2 + 1.0 * 0.5)).abs() < 1e-9);
    }

    #[test]
    fn pocket_void_removes_only_its_depth() {
        let sketch = with(&[
            (rect(0.0, 0.0, 4.0, 4.0), solid(0.3)),
            (rect(1.0, 1.0, 2.0, 2.0), void(Some(0.1))),
        ]);
        let prisms = layered_prisms(&sketch).unwrap_or_default();
        assert!((volume(&prisms) - (16.0 * 0.3 - 1.0 * 0.1)).abs() < 1e-9);
        // The pocket walls are named by the void face (id 1).
        let pocket_walls = prisms
            .iter()
            .flat_map(|p| p.contours.iter().skip(1))
            .flat_map(|c| c.sides.iter())
            .filter(|s| s.is_some_and(|s| s.face == 1))
            .count();
        assert_eq!(pocket_walls, 4);
    }

    #[test]
    fn invalid_geometry_is_reported() {
        let mut sketch = with(&[(rect(0.0, 0.0, 1.0, 1.0), solid(0.2))]);
        if let Some(face) = sketch.faces.first_mut() {
            face.points.swap(1, 2);
        }
        assert!(matches!(
            layered_prisms(&sketch),
            Err(LayerError::Invalid(SketchError::SelfIntersecting { face: 0 }))
        ));
    }
}

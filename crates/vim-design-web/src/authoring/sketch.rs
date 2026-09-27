//! The in-progress sketch on a construction plane (view-only state:
//! nothing enters the document until the outline is committed, so
//! cancel simply drops it) plus the live validation rules.

use vim_design_lib::EntityId;

use super::geom::{
    self, EPS, Invalid, P2, dedup_closed, dist, rectangle, self_intersects, strictly_inside,
    validate_outline,
};
use super::snap::{SnapKind, SnapResult};
use super::walls;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SketchTool {
    Plate,
    Hole,
    /// A run of wall reference lines on the level plane.
    Wall,
    /// Edit Mode: a new solid or void face of the edited profile.
    Profile,
    /// Edit Mode: a two-point line that splits the faces it crosses.
    Split,
}

impl SketchTool {
    pub fn name(self) -> &'static str {
        match self {
            SketchTool::Plate => "plate",
            SketchTool::Hole => "hole",
            SketchTool::Wall => "wall",
            SketchTool::Profile => "profile",
            SketchTool::Split => "split",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    Polygon,
    Rect,
}

impl Shape {
    pub fn name(self) -> &'static str {
        match self {
            Shape::Polygon => "polygon",
            Shape::Rect => "rect",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaceOutcome {
    Added,
    /// Same spot as the previous vertex (double tap/click): ignored.
    Duplicate,
    /// No cursor on the plane (e.g. the ray missed it).
    NoCursor,
    /// The first vertex was hit with >= 3 points: the caller commits.
    CloseRequested,
    /// The rectangle's second corner was placed: the caller commits.
    RectComplete,
}

/// A floor plate outline and its holes, for hole validation.
pub struct PlateOutline<'a> {
    pub face: EntityId,
    pub outline: &'a [P2],
    pub holes: Vec<&'a [P2]>,
}

#[derive(Debug, Clone)]
pub struct Sketch {
    pub tool: SketchTool,
    pub shape: Shape,
    /// The construction plane (the active level at tool start).
    pub level: EntityId,
    /// Placed vertices (polygon) or corners (rectangle, at most 2).
    pub points: Vec<P2>,
    /// Current snapped pointer position (None when not over the plane).
    pub cursor: Option<SnapResult>,
}

impl Sketch {
    pub fn new(tool: SketchTool, shape: Shape, level: EntityId) -> Self {
        Self { tool, shape, level, points: Vec::new(), cursor: None }
    }

    pub fn set_shape(&mut self, shape: Shape) {
        if self.shape != shape {
            self.shape = shape;
            self.points.clear();
        }
    }

    /// The first vertex, when placing on it would close the loop.
    pub fn close_target(&self) -> Option<P2> {
        (self.shape == Shape::Polygon && self.points.len() >= 3)
            .then(|| self.points[0])
    }

    pub fn place(&mut self) -> PlaceOutcome {
        let Some(cursor) = &self.cursor else {
            return PlaceOutcome::NoCursor;
        };
        let p = cursor.point;
        match self.shape {
            Shape::Polygon => {
                if cursor.kind == SnapKind::First && self.points.len() >= 3 {
                    return PlaceOutcome::CloseRequested;
                }
                if self.points.last().is_some_and(|q| dist(*q, p) <= EPS) {
                    return PlaceOutcome::Duplicate;
                }
                self.points.push(p);
                PlaceOutcome::Added
            }
            Shape::Rect => {
                if self.points.first().is_some_and(|q| dist(*q, p) <= EPS) {
                    return PlaceOutcome::Duplicate;
                }
                self.points.push(p);
                if self.points.len() >= 2 {
                    self.points.truncate(2);
                    PlaceOutcome::RectComplete
                } else {
                    PlaceOutcome::Added
                }
            }
        }
    }

    pub fn undo_point(&mut self) -> bool {
        self.points.pop().is_some()
    }

    /// The outline a Finish would commit (placed points only).
    pub fn outline(&self) -> Vec<P2> {
        match self.shape {
            Shape::Polygon => dedup_closed(&self.points),
            Shape::Rect => match self.points.as_slice() {
                [a, b] => rectangle(*a, *b),
                _ => Vec::new(),
            },
        }
    }

    /// The outline including the cursor (what the user sees).
    pub fn preview(&self) -> Vec<P2> {
        let cursor = self.cursor.as_ref().map(|c| c.point);
        match self.shape {
            Shape::Polygon => {
                let mut pts = self.points.clone();
                if let Some(c) = cursor {
                    let closing = self.close_target().is_some_and(|f| dist(f, c) <= EPS);
                    if !closing && pts.last().is_none_or(|q| dist(*q, c) > EPS) {
                        pts.push(c);
                    }
                }
                pts
            }
            Shape::Rect => match (self.points.as_slice(), cursor) {
                ([a, b], _) => rectangle(*a, *b),
                ([a], Some(c)) => rectangle(*a, c),
                _ => Vec::new(),
            },
        }
    }
}

/// Validate a floor plate outline.
pub fn validate_plate(outline: &[P2]) -> Result<(), Invalid> {
    validate_outline(outline)
}

/// Validate a hole outline against the plates on the active level;
/// returns the index of the (automatically chosen) containing plate.
pub fn validate_hole(outline: &[P2], plates: &[PlateOutline]) -> Result<usize, Invalid> {
    validate_outline(outline)?;
    if plates.is_empty() {
        return Err(Invalid::NoPlateOnLevel);
    }
    let index = plates
        .iter()
        .position(|p| strictly_inside(outline, p.outline))
        .ok_or(Invalid::HoleOutsidePlate)?;
    let overlaps = plates[index]
        .holes
        .iter()
        .filter(|h| h.len() >= 3)
        .any(|h| !geom::disjoint(outline, h));
    if overlaps {
        return Err(Invalid::HoleOverlapsHole);
    }
    Ok(index)
}

/// Live status of a sketch for the UI.
#[derive(Debug, Clone, PartialEq)]
pub struct SketchStatus {
    /// Finish is allowed (the committed outline would be valid).
    pub can_finish: bool,
    /// The visible preview (with the cursor) is fine; false = draw red.
    pub preview_ok: bool,
    /// Why it is not fine (shown in the action bar / toast).
    pub reason: Option<Invalid>,
}

/// What a sketch is validated against.
pub enum SketchContext<'a> {
    Plate,
    /// Plates on the sketch plane (the hole picks its container).
    Hole(&'a [PlateOutline<'a>]),
    Wall { thickness: f64, flip: bool },
}

/// The committed shape of a finished wall sketch: the reference points
/// and whether the run is a closed loop.
pub fn wall_run(sketch: &Sketch, closing: bool) -> (Vec<P2>, bool) {
    match sketch.shape {
        Shape::Rect => (sketch.outline(), true),
        Shape::Polygon => (sketch.outline(), closing),
    }
}

pub fn status(sketch: &Sketch, ctx: &SketchContext) -> SketchStatus {
    let outline = sketch.outline();
    let finish = match (sketch.tool, ctx) {
        (SketchTool::Hole, SketchContext::Hole(plates)) => validate_hole(&outline, plates).map(|_| ()),
        (SketchTool::Wall, SketchContext::Wall { thickness, flip }) => {
            let (run, closed) = wall_run(sketch, false);
            walls::wall_segments(&run, closed, *thickness, *flip).map(|_| ())
        }
        (SketchTool::Split, _) => {
            if outline.len() >= 2 { Ok(()) } else { Err(Invalid::TooFewWallPoints) }
        }
        _ => validate_plate(&outline),
    };
    let preview = sketch.preview();
    let mut preview_err = None;
    let open_crossing = match sketch.shape {
        Shape::Polygon => self_intersects(&preview, false),
        Shape::Rect => false,
    };
    if open_crossing {
        preview_err = Some(Invalid::SelfIntersecting);
    } else if !preview.is_empty() {
        match ctx {
            SketchContext::Hole([]) => {
                preview_err = Some(Invalid::NoPlateOnLevel);
            }
            SketchContext::Hole(plates) => {
                let inside_one = plates.iter().any(|p| {
                    preview.iter().all(|q| geom::point_in_polygon(*q, p.outline))
                });
                if !inside_one {
                    preview_err = Some(Invalid::HoleOutsidePlate);
                }
            }
            _ => {}
        }
    }
    let enough = match (sketch.shape, sketch.tool) {
        (Shape::Rect, _) => sketch.points.len() >= 2,
        (Shape::Polygon, SketchTool::Wall | SketchTool::Split) => outline.len() >= 2,
        (Shape::Polygon, _) => outline.len() >= 3,
    };
    let finish_err = finish.err().filter(|_| enough);
    SketchStatus {
        can_finish: finish_err.is_none() && enough,
        preview_ok: preview_err.is_none(),
        reason: preview_err.or(finish_err),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cursor(p: P2, kind: SnapKind) -> Option<SnapResult> {
        Some(SnapResult { point: p, kind, guides: vec![] })
    }

    #[test]
    fn polygon_place_close_and_duplicates() {
        let mut s = Sketch::new(SketchTool::Plate, Shape::Polygon, EntityId(1));
        for p in [[0.0, 0.0], [4.0, 0.0], [4.0, 3.0]] {
            s.cursor = cursor(p, SnapKind::Grid);
            assert_eq!(s.place(), PlaceOutcome::Added);
        }
        assert_eq!(s.place(), PlaceOutcome::Duplicate);
        s.cursor = cursor([0.0, 0.0], SnapKind::First);
        assert_eq!(s.place(), PlaceOutcome::CloseRequested);
        let st = status(&s, &SketchContext::Plate);
        assert!(st.can_finish && st.preview_ok);
    }

    #[test]
    fn crossing_preview_is_red() {
        let mut s = Sketch::new(SketchTool::Plate, Shape::Polygon, EntityId(1));
        for p in [[0.0, 0.0], [4.0, 4.0], [4.0, 0.0]] {
            s.cursor = cursor(p, SnapKind::Grid);
            s.place();
        }
        s.cursor = cursor([0.0, 4.0], SnapKind::Grid);
        let st = status(&s, &SketchContext::Plate);
        assert!(!st.preview_ok);
        assert_eq!(st.reason, Some(Invalid::SelfIntersecting));
        s.place();
        assert!(!status(&s, &SketchContext::Plate).can_finish, "the bowtie cannot be finished");
    }

    #[test]
    fn rect_and_hole_targeting() {
        let plate = [[0.0, 0.0], [6.0, 0.0], [6.0, 4.0], [0.0, 4.0]];
        let existing = [[4.0, 1.0], [5.0, 1.0], [5.0, 2.0], [4.0, 2.0]];
        let plates = [PlateOutline { face: EntityId(7), outline: &plate, holes: vec![&existing] }];
        let mut s = Sketch::new(SketchTool::Hole, Shape::Rect, EntityId(1));
        s.cursor = cursor([1.0, 1.0], SnapKind::Grid);
        assert_eq!(s.place(), PlaceOutcome::Added);
        s.cursor = cursor([2.0, 2.0], SnapKind::Grid);
        assert_eq!(s.place(), PlaceOutcome::RectComplete);
        assert_eq!(validate_hole(&s.outline(), &plates), Ok(0));
        // Outside the plate.
        let outside = rectangle([5.5, 3.0], [7.0, 5.0]);
        assert_eq!(validate_hole(&outside, &plates), Err(Invalid::HoleOutsidePlate));
        // Touching the existing hole.
        let touching = rectangle([3.0, 1.0], [4.0, 2.0]);
        assert_eq!(validate_hole(&touching, &plates), Err(Invalid::HoleOverlapsHole));
        assert_eq!(validate_hole(&s.outline(), &[]), Err(Invalid::NoPlateOnLevel));
    }
}

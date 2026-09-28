//! Copy and paste: what a copy holds and how a paste places it. Pure
//! data (the wasm app drives it): a copy is taken from the selection of
//! the current mode, and a paste in the same mode places it where the
//! user taps.
//!
//! - Floor Edit Mode: the selected faces (outline + kind), placed with
//!   their bounding-box centre at the tap.
//! - Openings mode: the selected openings, placed along the tapped wall
//!   segment centred at the tap (each kept inside the clear span).
//! - Select mode: a whole element (a floor plate or a wall run), placed
//!   on the active plane with its bounding-box centre at the tap.
//!
//! A paste moves by a whole number of snap steps when snapping is on, so
//! a copy of an outline on the grid stays on the grid.

use vim_design_lib::EntityId;
use vim_design_lib::sketch::Sketch;
use vim_design_lib::wall_run::{Opening, WallRunData};

use super::edit::FaceKind;
use super::geom::P2;

/// What a copy holds.
#[derive(Debug, Clone, PartialEq)]
pub enum Clip {
    /// Profile faces (floor Edit Mode), in plane coordinates.
    Faces(Vec<(Vec<P2>, FaceKind)>),
    /// Wall run openings, in the order they were copied.
    Openings(Vec<Opening>),
    /// A floor plate: its sketch and name.
    Plate { sketch: Sketch, name: String },
    /// A wall run: its data, its top plane, and its name.
    Run { data: WallRunData, top: Option<EntityId>, base: EntityId, top_height: f64, name: String },
    /// A room (its layout places it on top of the rooms it overlaps).
    Room(vim_design_lib::room::RoomData),
}

impl Clip {
    /// Where the copy is used: "faces", "openings", or "element".
    pub fn context(&self) -> &'static str {
        match self {
            Clip::Faces(_) => "faces",
            Clip::Openings(_) => "openings",
            Clip::Plate { .. } | Clip::Run { .. } | Clip::Room(_) => "element",
        }
    }

    /// A short description for the page ("2 faces", "Door", "Wall 3").
    pub fn label(&self) -> String {
        match self {
            Clip::Faces(f) if f.len() == 1 => "1 face".to_owned(),
            Clip::Faces(f) => format!("{} faces", f.len()),
            Clip::Openings(o) if o.len() == 1 => match o[0].kind {
                vim_design_lib::wall_run::OpeningKind::Window => "Window".to_owned(),
                vim_design_lib::wall_run::OpeningKind::Door => "Door".to_owned(),
            },
            Clip::Openings(o) => format!("{} openings", o.len()),
            Clip::Plate { name, .. } | Clip::Run { name, .. } => name.clone(),
            Clip::Room(r) => r.name.clone(),
        }
    }

    /// The outlines the copy draws in plane coordinates (faces, a plate's
    /// faces, a run's reference line), for the paste preview.
    pub fn outlines(&self) -> Vec<(Vec<P2>, bool)> {
        match self {
            Clip::Faces(f) => f.iter().map(|(o, _)| (o.clone(), true)).collect(),
            Clip::Plate { sketch, .. } => sketch_outlines(sketch),
            Clip::Run { data, .. } => vec![(data.points.iter().map(|p| p.uv).collect(), data.closed)],
            Clip::Room(r) => vec![(r.polygon(), true)],
            Clip::Openings(_) => Vec::new(),
        }
    }

    /// The point placed at the tap: the centre of the copy's bounding box.
    pub fn anchor(&self) -> Option<P2> {
        bbox_centre(self.outlines().iter().flat_map(|(o, _)| o.iter().copied()))
    }
}

/// A sketch's faces as closed outlines.
pub fn sketch_outlines(sketch: &Sketch) -> Vec<(Vec<P2>, bool)> {
    let at = |id: u32| sketch.points.iter().find(|p| p.id == id).map(|p| p.uv);
    sketch.faces.iter().map(|f| (f.points.iter().filter_map(|id| at(*id)).collect(), true)).collect()
}

/// Centre of the bounding box of some points (`None` when empty).
pub fn bbox_centre(points: impl Iterator<Item = P2>) -> Option<P2> {
    let mut lo = [f64::INFINITY; 2];
    let mut hi = [f64::NEG_INFINITY; 2];
    for p in points {
        for k in 0..2 {
            lo[k] = lo[k].min(p[k]);
            hi[k] = hi[k].max(p[k]);
        }
    }
    lo[0].is_finite().then(|| [(lo[0] + hi[0]) / 2.0, (lo[1] + hi[1]) / 2.0])
}

/// The move that puts `anchor` at `target`: a whole number of `step`s on
/// each axis when snapping (`step` > 0), else exact.
pub fn paste_delta(anchor: P2, target: P2, step: Option<f64>) -> P2 {
    let d = [target[0] - anchor[0], target[1] - anchor[1]];
    match step.filter(|s| *s > 0.0) {
        Some(s) => [(d[0] / s).round() * s, (d[1] / s).round() * s],
        None => d,
    }
}

pub fn moved(points: &[P2], d: P2) -> Vec<P2> {
    points.iter().map(|p| [p[0] + d[0], p[1] + d[1]]).collect()
}

/// A sketch moved in its plane (ids and faces unchanged).
pub fn moved_sketch(sketch: &Sketch, d: P2) -> Sketch {
    let mut out = sketch.clone();
    for p in &mut out.points {
        p.uv = [p.uv[0] + d[0], p.uv[1] + d[1]];
    }
    out
}

/// A wall run moved in its plane (openings and profiles are segment
/// local, so they come along unchanged).
pub fn moved_run(data: &WallRunData, d: P2) -> WallRunData {
    let mut out = data.clone();
    for p in &mut out.points {
        p.uv = [p.uv[0] + d[0], p.uv[1] + d[1]];
    }
    out
}

/// Openings of a copy laid out along one segment: centred at `u` (the
/// tap along the segment) and keeping their spacing along their
/// segments. Each has `segment` set and id 0 (the run assigns ids).
pub fn laid_out(copies: &[Opening], segment: u32, u: f64) -> Vec<Opening> {
    let lo = copies.iter().map(|o| o.offset_m).fold(f64::INFINITY, f64::min);
    let hi = copies.iter().map(|o| o.offset_m + o.width_m).fold(f64::NEG_INFINITY, f64::max);
    if !lo.is_finite() {
        return Vec::new();
    }
    let shift = u - (lo + hi) / 2.0;
    copies
        .iter()
        .map(|o| Opening { id: 0, segment, offset_m: o.offset_m + shift, ..*o })
        .collect()
}

/// The name of a copy: the source's name without its number, then the
/// next free number of that name ("Floor plate 1" -> "Floor plate 2";
/// "Kitchen" -> "Kitchen 2"). `taken` lists every element name.
pub fn copy_name<'a>(source: &str, taken: impl Iterator<Item = &'a str>) -> String {
    let stem = strip_number(source);
    let mut max = 0u64;
    for name in taken {
        if name == stem {
            max = max.max(1);
        } else if let Some(n) = name.strip_prefix(stem).and_then(|r| r.strip_prefix(' ')).and_then(|r| r.parse::<u64>().ok()) {
            max = max.max(n);
        }
    }
    format!("{stem} {}", max.max(1) + 1)
}

/// A name without a trailing " <number>".
fn strip_number(name: &str) -> &str {
    match name.rsplit_once(' ') {
        Some((stem, n)) if !stem.is_empty() && !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) => stem,
        _ => name,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vim_design_lib::wall_run::OpeningKind;

    #[test]
    fn copy_names_follow_the_numbering() {
        let names = ["Floor plate 1", "Floor plate 3", "Wall 2", "Kitchen"];
        assert_eq!(copy_name("Floor plate 1", names.iter().copied()), "Floor plate 4");
        assert_eq!(copy_name("Wall 2", names.iter().copied()), "Wall 3");
        assert_eq!(copy_name("Kitchen", names.iter().copied()), "Kitchen 2");
        assert_eq!(copy_name("Lobby", names.iter().copied()), "Lobby 2");
        // A number that is the whole name stays the name.
        assert_eq!(copy_name("12", ["12"].iter().copied()), "12 2");
    }

    #[test]
    fn a_snapped_paste_moves_by_whole_steps() {
        let d = paste_delta([1.0, 1.0], [3.4, -0.6], Some(0.5));
        assert!((d[0] - 2.5).abs() < 1e-12 && (d[1] + 1.5).abs() < 1e-12, "{d:?}");
        assert_eq!(paste_delta([0.0, 0.0], [0.3, 0.2], None), [0.3, 0.2]);
    }

    #[test]
    fn openings_keep_their_spacing_around_the_tap() {
        let o = |offset: f64| Opening {
            id: 7,
            segment: 1,
            offset_m: offset,
            sill_m: 0.9,
            width_m: 1.0,
            height_m: 1.2,
            kind: OpeningKind::Window,
            depth_m: None,
        };
        let placed = laid_out(&[o(1.0), o(3.0)], 4, 10.0);
        // The group spans 1..4 (centre 2.5): now centred at 10.
        assert_eq!(placed.iter().map(|p| p.offset_m).collect::<Vec<_>>(), vec![8.5, 10.5]);
        assert!(placed.iter().all(|p| p.segment == 4 && p.id == 0 && p.sill_m == 0.9));
    }

    #[test]
    fn the_anchor_is_the_bounding_box_centre() {
        let clip = Clip::Faces(vec![
            (vec![[0.0, 0.0], [2.0, 0.0], [2.0, 1.0]], FaceKind::Solid { thickness: 0.3 }),
            (vec![[4.0, 3.0], [5.0, 3.0], [5.0, 4.0]], FaceKind::Void { depth: None }),
        ]);
        assert_eq!(clip.anchor(), Some([2.5, 2.0]));
        assert_eq!(clip.context(), "faces");
        assert_eq!(clip.label(), "2 faces");
    }
}

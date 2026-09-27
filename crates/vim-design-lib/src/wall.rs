//! Walls: a reference line on a construction plane and an elevation
//! profile.
//!
//! The profile is a [`Sketch`] in the wall's elevation: u runs along the
//! reference line from `start`, v runs up from the base plane. Solid
//! faces are wall material (each with its own thickness, measured toward
//! the left of start -> end), void faces are openings (a window, a door
//! that crosses the bottom edge, a niche with a depth under the
//! thickness).
//!
//! The wall's TOP REFERENCE is a height H above the base plane: the
//! wall's own `height_m`, or, when a top plane is wired, that plane's
//! height above the base plus `top_offset_m`. Profile points listed in
//! `top_points` are top-anchored: their stored v is measured from H, so
//! the top edge follows the top plane while windows keep their sill
//! height. The EFFECTIVE profile is the stored one with H added to the
//! anchored points' v; it is what evaluates and what the user edits.
//! [`ops`] edits in effective coordinates and keeps the anchors
//! consistent.

use crate::document::Document;
use crate::entity::{Params, slot};
use crate::id::EntityId;
use crate::sketch::{POINT_TOLERANCE, Sketch, SketchError, SketchFace, SketchFaceKind, SketchPoint};

pub mod ops;

/// Typed failure of a wall check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WallError {
    /// `start` and `end` coincide.
    ZeroLength,
    /// A coordinate, height, or offset is NaN or infinite.
    NonFinite,
    /// `height_m` is not positive.
    InvalidHeight,
    /// A top-anchored id is not a profile point.
    UnknownTopPoint(u32),
    /// The profile fails sketch structural validation.
    Profile(SketchError),
}

impl std::fmt::Display for WallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WallError::ZeroLength => write!(f, "the wall reference line has no length"),
            WallError::NonFinite => write!(f, "a wall coordinate or height is not finite"),
            WallError::InvalidHeight => write!(f, "the wall height must be positive"),
            WallError::UnknownTopPoint(id) => {
                write!(f, "top-anchored point {id} is not in the wall profile")
            }
            WallError::Profile(err) => write!(f, "wall profile: {err}"),
        }
    }
}

impl std::error::Error for WallError {}

/// Structural validity, as the wall commands require it: a finite
/// reference line with length, a finite positive height, a finite top
/// offset, a structurally valid profile, and top-anchored ids that are
/// profile points.
pub fn validate_structure(
    start: [f64; 2],
    end: [f64; 2],
    height_m: f64,
    top_offset_m: f64,
    profile: &Sketch,
    top_points: &[u32],
) -> Result<(), WallError> {
    if !start
        .iter()
        .chain(end.iter())
        .chain([height_m, top_offset_m].iter())
        .all(|c| c.is_finite())
    {
        return Err(WallError::NonFinite);
    }
    let length = ((end[0] - start[0]).powi(2) + (end[1] - start[1]).powi(2)).sqrt();
    if length <= POINT_TOLERANCE {
        return Err(WallError::ZeroLength);
    }
    if height_m <= 0.0 {
        return Err(WallError::InvalidHeight);
    }
    crate::sketch::validate_structure(profile).map_err(WallError::Profile)?;
    for id in top_points {
        if profile.point(*id).is_none() {
            return Err(WallError::UnknownTopPoint(*id));
        }
    }
    Ok(())
}

/// The default wall profile: the rectangle (0, 0), (length, 0),
/// (length, top), (0, top) with both top corners top-anchored at v = 0,
/// so the wall is exactly the top reference height high in both height
/// modes. One solid face (id 0) with `thickness`. Point ids are 0..=3
/// in that order; the returned list is the anchored ids (2, 3).
pub fn default_profile(length: f64, thickness: f64) -> (Sketch, Vec<u32>) {
    let points = vec![
        SketchPoint { id: 0, uv: [0.0, 0.0] },
        SketchPoint { id: 1, uv: [length, 0.0] },
        SketchPoint { id: 2, uv: [length, 0.0] },
        SketchPoint { id: 3, uv: [0.0, 0.0] },
    ];
    let faces = vec![SketchFace {
        id: 0,
        points: vec![0, 1, 2, 3],
        kind: SketchFaceKind::Solid { thickness },
    }];
    (Sketch { points, faces }, vec![2, 3])
}

/// The effective profile for top reference height `height`: anchored
/// points' v raised by `height`.
pub fn effective_profile(profile: &Sketch, top_points: &[u32], height: f64) -> Sketch {
    let mut effective = profile.clone();
    for point in &mut effective.points {
        if top_points.contains(&point.id) {
            point.uv[1] += height;
        }
    }
    effective
}

/// The stored profile for an effective one: anchored points' v lowered
/// by `height` (the inverse of [`effective_profile`]).
pub fn stored_profile(effective: &Sketch, top_points: &[u32], height: f64) -> Sketch {
    let mut stored = effective.clone();
    for point in &mut stored.points {
        if top_points.contains(&point.id) {
            point.uv[1] -= height;
        }
    }
    stored
}

/// The top reference height of a wall above its base plane (meters),
/// from params only, computed exactly as evaluation does: the fixed
/// height, or the top plane's height above the base plus the top offset.
/// `None` for an entity that is not a wall (or a broken plane chain).
pub fn wall_top_height(doc: &Document, wall: EntityId) -> Option<f64> {
    let record = doc.entity(wall)?;
    let (height_m, top_offset_m) = match &record.params {
        Params::Wall {
            height_m,
            top_offset_m,
            ..
        } => (*height_m, *top_offset_m),
        _ => return None,
    };
    let plane = |index: usize| record.inputs.get(index).and_then(|s| s.referenced().next());
    match plane(slot::WALL_TOP) {
        None => Some(height_m),
        Some(top) => {
            let base = crate::workplane::plane_elevation(doc, plane(slot::WALL_BASE)?)?;
            let top = crate::workplane::plane_elevation(doc, top)?;
            Some((top - base) + top_offset_m)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_profile_is_exactly_the_top_height() {
        let (profile, top) = default_profile(4.0, 0.2);
        assert_eq!(top, vec![2, 3]);
        assert_eq!(validate_structure([0.0; 2], [4.0, 0.0], 2.7, 0.0, &profile, &top), Ok(()));
        let effective = effective_profile(&profile, &top, 2.7);
        assert_eq!(crate::sketch::validate(&effective), Ok(()));
        assert_eq!(
            crate::sketch::face_polygon(&effective, 0),
            Ok(vec![[0.0, 0.0], [4.0, 0.0], [4.0, 2.7], [0.0, 2.7]])
        );
        assert_eq!(stored_profile(&effective, &top, 2.7), profile);
    }

    #[test]
    fn structural_rules() {
        let (profile, top) = default_profile(4.0, 0.2);
        assert_eq!(
            validate_structure([1.0, 1.0], [1.0, 1.0], 2.7, 0.0, &profile, &top),
            Err(WallError::ZeroLength)
        );
        assert_eq!(
            validate_structure([0.0; 2], [4.0, 0.0], 0.0, 0.0, &profile, &top),
            Err(WallError::InvalidHeight)
        );
        assert_eq!(
            validate_structure([0.0; 2], [f64::NAN, 0.0], 2.7, 0.0, &profile, &top),
            Err(WallError::NonFinite)
        );
        assert_eq!(
            validate_structure([0.0; 2], [4.0, 0.0], 2.7, 0.0, &profile, &[2, 9]),
            Err(WallError::UnknownTopPoint(9))
        );
        let mut bad = profile.clone();
        if let Some(face) = bad.faces.first_mut() {
            face.kind = SketchFaceKind::Solid { thickness: -0.1 };
        }
        assert_eq!(
            validate_structure([0.0; 2], [4.0, 0.0], 2.7, 0.0, &bad, &top),
            Err(WallError::Profile(SketchError::InvalidThickness { face: 0 }))
        );
    }
}

//! Editing operations on a room layout: its rooms, their ranking, and
//! its openings.
//!
//! The operations are pure functions of a [`LayoutInput`]. They return
//! what to put back with one command: the layout data (and rooms list)
//! for `UpdateRoomLayout`, or a room's new precedence for `UpdateRoom`.
//! An opening operation checks the layout's structure and that the
//! opening it touches fits its wall (other openings are not checked, so
//! one misfit after a room edit does not block editing the rest).

use crate::id::EntityId;
use crate::sketch::geom::P2;
use crate::wall_run::OpeningKind;

use super::{LayoutError, LayoutInput, RoomLayoutData, RoomOpening, arrange, validate_structure};

fn checked(input: &LayoutInput, data: RoomLayoutData, touched: u32) -> Result<RoomLayoutData, LayoutError> {
    let next = LayoutInput { layout: data, rooms: input.rooms.clone() };
    validate_structure(&next)?;
    let opening = next
        .layout
        .openings
        .iter()
        .find(|o| o.id == touched)
        .ok_or(LayoutError::UnknownOpening(touched))?;
    if !arrange(&next)?.fits(opening) {
        return Err(LayoutError::OpeningDoesNotFit(touched));
    }
    Ok(next.layout)
}

// ---------------------------------------------------------------------
// Rooms.
// ---------------------------------------------------------------------

/// The rooms list with `room` appended (no change when it is already
/// there).
pub fn add_room(rooms: &[EntityId], room: EntityId) -> Vec<EntityId> {
    let mut out = rooms.to_vec();
    if !out.contains(&room) {
        out.push(room);
    }
    out
}

/// The rooms list without `room`, and the layout data without the
/// openings anchored to its edges.
pub fn remove_room(input: &LayoutInput, room: EntityId) -> (Vec<EntityId>, RoomLayoutData) {
    let rooms = input.rooms.iter().map(|(id, _)| *id).filter(|id| *id != room).collect();
    let mut data = input.layout.clone();
    data.openings.retain(|o| o.room != room);
    (rooms, data)
}

/// The rooms of the layout in rank order (highest precedence first; ties
/// by entity id).
pub fn ranking(input: &LayoutInput) -> Vec<EntityId> {
    let mut ranked: Vec<(EntityId, i32)> = input.rooms.iter().map(|(id, r)| (*id, r.precedence)).collect();
    ranked.sort_by(|(ia, a), (ib, b)| b.cmp(a).then(ia.cmp(ib)));
    ranked.into_iter().map(|(id, _)| id).collect()
}

fn precedence_of(input: &LayoutInput, room: EntityId) -> Result<i32, LayoutError> {
    input.room(room).map(|r| r.precedence).ok_or(LayoutError::RoomNotInLayout(room))
}

/// The precedence that ranks `room` one place higher (just above the
/// room ranked directly above it), or `None` when it is first.
pub fn bring_forward(input: &LayoutInput, room: EntityId) -> Result<Option<i32>, LayoutError> {
    precedence_of(input, room)?;
    let order = ranking(input);
    let index = order.iter().position(|id| *id == room).unwrap_or(0);
    let Some(above) = index.checked_sub(1).and_then(|i| order.get(i)) else { return Ok(None) };
    Ok(Some(precedence_of(input, *above)?.saturating_add(1)))
}

/// The precedence that ranks `room` one place lower, or `None` when it
/// is last.
pub fn send_backward(input: &LayoutInput, room: EntityId) -> Result<Option<i32>, LayoutError> {
    precedence_of(input, room)?;
    let order = ranking(input);
    let index = order.iter().position(|id| *id == room).unwrap_or(0);
    let Some(below) = order.get(index + 1) else { return Ok(None) };
    Ok(Some(precedence_of(input, *below)?.saturating_sub(1)))
}

/// The precedence that ranks `room` first.
pub fn bring_to_front(input: &LayoutInput, room: EntityId) -> Result<i32, LayoutError> {
    let own = precedence_of(input, room)?;
    let top = input.rooms.iter().filter(|(id, _)| *id != room).map(|(_, r)| r.precedence).max();
    Ok(top.map_or(own, |p| p.saturating_add(1).max(own)))
}

/// The precedence that ranks `room` last.
pub fn send_to_back(input: &LayoutInput, room: EntityId) -> Result<i32, LayoutError> {
    let own = precedence_of(input, room)?;
    let bottom = input.rooms.iter().filter(|(id, _)| *id != room).map(|(_, r)| r.precedence).min();
    Ok(bottom.map_or(own, |p| p.saturating_sub(1).min(own)))
}

// ---------------------------------------------------------------------
// Openings.
// ---------------------------------------------------------------------

/// Add an opening; its id is replaced by the next free opening id,
/// which is returned.
pub fn add_opening(input: &LayoutInput, opening: RoomOpening) -> Result<(RoomLayoutData, u32), LayoutError> {
    let id = input.layout.next_opening_id();
    let mut data = input.layout.clone();
    data.openings.push(RoomOpening { id, ..opening });
    Ok((checked(input, data, id)?, id))
}

/// Move an opening by `delta`: along its edge (clamped into the span it
/// is in, or the nearest span) and up (a window's sill, clamped at the
/// base; a door ignores it).
pub fn move_opening(input: &LayoutInput, id: u32, delta: P2) -> Result<RoomLayoutData, LayoutError> {
    if !delta.iter().all(|v| v.is_finite()) {
        return Err(LayoutError::NonFinite);
    }
    let mut data = input.layout.clone();
    let opening = data.openings.iter_mut().find(|o| o.id == id).ok_or(LayoutError::UnknownOpening(id))?;
    let spans = arrange(input)?.spans(opening.room, opening.edge);
    let wanted = opening.offset_m + delta[0];
    let centre = wanted + opening.width_m / 2.0;
    let span = spans
        .iter()
        .filter(|(lo, hi)| hi - lo >= opening.width_m - 1e-9)
        .min_by(|a, b| {
            let d = |(lo, hi): &(f64, f64)| (centre - centre.clamp(*lo, *hi)).abs();
            d(a).total_cmp(&d(b))
        })
        .ok_or(LayoutError::OpeningDoesNotFit(id))?;
    opening.offset_m = wanted.clamp(span.0, (span.1 - opening.width_m).max(span.0));
    if opening.kind == OpeningKind::Window {
        opening.sill_m = (opening.sill_m + delta[1]).max(0.0);
    }
    checked(input, data, id)
}

/// Replace the opening with `opening.id`.
pub fn set_opening(input: &LayoutInput, opening: RoomOpening) -> Result<RoomLayoutData, LayoutError> {
    let mut data = input.layout.clone();
    let slot = data
        .openings
        .iter_mut()
        .find(|o| o.id == opening.id)
        .ok_or(LayoutError::UnknownOpening(opening.id))?;
    *slot = opening;
    checked(input, data, opening.id)
}

/// Delete an opening.
pub fn delete_opening(input: &LayoutInput, id: u32) -> Result<RoomLayoutData, LayoutError> {
    if !input.layout.openings.iter().any(|o| o.id == id) {
        return Err(LayoutError::UnknownOpening(id));
    }
    let mut data = input.layout.clone();
    data.openings.retain(|o| o.id != id);
    Ok(data)
}

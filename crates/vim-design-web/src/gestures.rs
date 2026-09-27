//! App-level gesture grouping for undo/redo (shared by the demo and the
//! authoring app).
//!
//! The document records one undo step per submitted command (consecutive
//! coalesced updates to one entity merge). A single user gesture often
//! submits many commands — drawing a floor plate creates dozens of
//! entities, a slider drag touches several control points per event — so
//! the app records the document's undo depth at each gesture start and
//! undoes/redoes whole gestures. Result: one user action = one undo step.
//!
//! There is ONE linear history. An Edit Mode is a [`Gestures::begin_session`]
//! span of it: every edit inside is an ordinary step; leaving with ✓
//! ([`Gestures::end_session`]) keeps the steps as they are; ✗
//! ([`Gestures::cancel_session`]) undoes back to the entry and drops them
//! (no redo). While a session is open, undo stops at its entry and redo
//! only replays the session's own steps, so undo never changes other
//! elements from inside a session.

use vim_design_lib::Document;

#[derive(Default)]
pub struct Gestures {
    /// Undo depths recorded at the start of each gesture.
    marks: Vec<usize>,
    /// Step counts popped by `undo`, consumed by `redo`.
    redo_counts: Vec<usize>,
    /// The open continuous gesture (slider drag, typing into a field).
    current: Option<String>,
    /// An open Edit Mode session: see [`Gestures::begin_session`].
    session: Option<Session>,
}

/// State saved when a session opens.
struct Session {
    /// Document undo depth at entry (✗ returns here).
    depth: usize,
    /// In-session undo never goes below this depth (≥ `depth`): a step
    /// that prepares the session (a legacy conversion) is not undone from
    /// inside it; after ✓ it is an ordinary step.
    floor: usize,
    /// Redo entries from before the session: never replayed inside it.
    redo_floor: usize,
}

impl Gestures {
    /// Begin (or continue) a named continuous gesture. Consecutive calls
    /// with the same name belong to one undo step until [`Self::end`],
    /// [`Self::one_shot`], `undo`, or `redo` closes it.
    pub fn begin(&mut self, doc: &Document, name: &str) {
        let _ = self.begin_continuing(doc, name);
    }

    /// Like [`Self::begin`], and reports whether the gesture was already
    /// open. Callers pass the result as the command's `coalesce` flag: the
    /// FIRST command of a gesture must not coalesce, or the document would
    /// merge it into the previous gesture's step (same entity, same
    /// command label) and one undo would revert both gestures.
    pub fn begin_continuing(&mut self, doc: &Document, name: &str) -> bool {
        let continuing = self.current.as_deref() == Some(name);
        if !continuing {
            self.marks.push(doc.undo_depth());
            self.current = Some(name.to_owned());
        }
        // Any new command invalidates the document's redo stack.
        self.clear_redo();
        continuing
    }

    /// Close the open continuous gesture (e.g. on a field's `change`
    /// event): the next edit starts a new undo step.
    pub fn end(&mut self) {
        self.current = None;
    }

    /// Record a one-shot operation (add/delete level, commit a drawn
    /// outline, ...) that was already submitted successfully: `depth` is
    /// the undo depth captured BEFORE the submit. Used instead of `begin`
    /// when the command may be rejected — a rejected command must not
    /// leave a stray gesture mark or clear the redo counts.
    pub fn one_shot(&mut self, depth: usize) {
        self.marks.push(depth);
        self.clear_redo();
        self.current = None;
    }

    fn clear_redo(&mut self) {
        self.redo_counts.clear();
        if let Some(s) = self.session.as_mut() {
            s.redo_floor = 0;
        }
    }

    /// Forget the redo history. Used after a failed multi-command
    /// operation was rolled back with `Document::undo` (which leaves the
    /// rolled-back steps on the document's redo stack): the app's redo
    /// must never replay them.
    pub fn invalidate_redo(&mut self) {
        self.clear_redo();
        self.current = None;
    }

    /// Open an Edit Mode session at the current depth: its edits are
    /// ordinary steps; undo inside stops at the entry.
    pub fn begin_session(&mut self, doc: &Document) {
        if self.session.is_none() {
            let depth = doc.undo_depth();
            self.session = Some(Session { depth, floor: depth, redo_floor: self.redo_counts.len() });
            self.current = None;
        }
    }

    pub fn in_session(&self) -> bool {
        self.session.is_some()
    }

    /// Raise the session's undo floor to the current depth: what was done
    /// so far (a conversion that prepares the edit) is not undone from
    /// inside the session; ✗ still reverts it, and after ✓ it is one
    /// ordinary undo step like the edits.
    pub fn set_session_floor(&mut self, doc: &Document) {
        if let Some(s) = self.session.as_mut() {
            s.floor = doc.undo_depth();
        }
    }

    /// ✓: close the session keeping its steps as they are (no collapse).
    /// Returns whether anything changed.
    pub fn end_session(&mut self, doc: &Document) -> bool {
        let Some(s) = self.session.take() else { return false };
        self.current = None;
        doc.undo_depth() > s.depth
    }

    /// ✗: close the session undoing back to its entry; its steps are
    /// dropped (they cannot be redone).
    pub fn cancel_session(&mut self, doc: &mut Document) {
        let Some(s) = self.session.take() else { return };
        let changed = doc.undo_depth() > s.depth;
        while doc.undo_depth() > s.depth {
            if doc.undo().is_err() {
                break;
            }
        }
        self.marks.retain(|m| *m < s.depth);
        if changed {
            self.redo_counts.clear();
        } else {
            // Undone session steps may sit above the earlier redo entries.
            self.redo_counts.truncate(s.redo_floor);
        }
        self.current = None;
    }

    pub fn undo(&mut self, doc: &mut Document) -> bool {
        let depth = doc.undo_depth();
        while self.marks.last().is_some_and(|m| *m >= depth) {
            self.marks.pop();
        }
        // Inside a session, never undo past its entry point.
        if let Some(s) = &self.session
            && self.marks.last().is_none_or(|m| *m < s.floor)
        {
            return false;
        }
        let Some(mark) = self.marks.pop() else {
            return false;
        };
        let steps = depth - mark;
        for _ in 0..steps {
            if doc.undo().is_err() {
                break;
            }
        }
        self.redo_counts.push(steps);
        self.current = None;
        true
    }

    pub fn redo(&mut self, doc: &mut Document) -> bool {
        if !self.can_redo() {
            return false;
        }
        let Some(steps) = self.redo_counts.pop() else {
            return false;
        };
        let mark = doc.undo_depth();
        for _ in 0..steps {
            if doc.redo().is_err() {
                break;
            }
        }
        self.marks.push(mark);
        self.current = None;
        true
    }

    pub fn can_undo(&self) -> bool {
        match &self.session {
            Some(s) => self.marks.last().is_some_and(|m| *m >= s.floor),
            None => !self.marks.is_empty(),
        }
    }

    pub fn can_redo(&self) -> bool {
        match &self.session {
            Some(s) => self.redo_counts.len() > s.redo_floor,
            None => !self.redo_counts.is_empty(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vim_design_lib::Command;

    fn cp(doc: &mut Document) -> vim_design_lib::EntityId {
        doc.submit(Command::CreateControlPoint { position: [0.0; 3] })
            .map(|o| o.created_ids[0])
            .unwrap_or(vim_design_lib::EntityId::INVALID)
    }

    #[test]
    fn continuous_gestures_do_not_merge_across_end() {
        let mut doc = Document::new();
        let mut g = Gestures::default();
        let depth = doc.undo_depth();
        let id = cp(&mut doc);
        g.one_shot(depth);
        for (i, x) in [1.0, 2.0, 3.0].iter().enumerate() {
            let coalesce = g.begin_continuing(&doc, "drag");
            assert_eq!(coalesce, i > 0);
            let _ = doc.submit(Command::UpdateControlPoint {
                id,
                position: [*x, 0.0, 0.0],
                coalesce,
            });
        }
        g.end();
        let coalesce = g.begin_continuing(&doc, "drag");
        assert!(!coalesce, "a new gesture after end() must not coalesce");
        let _ = doc.submit(Command::UpdateControlPoint {
            id,
            position: [9.0, 0.0, 0.0],
            coalesce,
        });
        let pos = |doc: &Document| match doc.entity(id).map(|e| &e.params) {
            Some(vim_design_lib::Params::ControlPoint { position }) => position[0],
            _ => f64::NAN,
        };
        assert!(g.undo(&mut doc));
        assert_eq!(pos(&doc), 3.0, "one undo reverts only the second gesture");
        assert!(g.undo(&mut doc));
        assert_eq!(pos(&doc), 0.0);
        assert!(g.redo(&mut doc));
        assert_eq!(pos(&doc), 3.0);
    }

    fn set_x(doc: &mut Document, g: &mut Gestures, id: vim_design_lib::EntityId, x: f64) {
        let depth = doc.undo_depth();
        let _ = doc.submit(Command::UpdateControlPoint { id, position: [x, 0.0, 0.0], coalesce: false });
        g.one_shot(depth);
    }

    fn x_of(doc: &Document, id: vim_design_lib::EntityId) -> f64 {
        match doc.entity(id).map(|e| &e.params) {
            Some(vim_design_lib::Params::ControlPoint { position }) => position[0],
            _ => f64::NAN,
        }
    }

    #[test]
    fn a_session_keeps_its_steps_and_stops_undo_at_its_entry() {
        let mut doc = Document::new();
        let mut g = Gestures::default();
        let depth = doc.undo_depth();
        let id = cp(&mut doc);
        g.one_shot(depth);
        g.begin_session(&doc);
        for x in [1.0, 2.0, 3.0] {
            set_x(&mut doc, &mut g, id, x);
        }
        // Inside: undo steps through the edits, but not past the entry.
        assert!(g.undo(&mut doc));
        assert_eq!(x_of(&doc, id), 2.0);
        assert!(g.redo(&mut doc));
        assert_eq!(x_of(&doc, id), 3.0);
        assert!(g.undo(&mut doc) && g.undo(&mut doc) && g.undo(&mut doc));
        assert_eq!(x_of(&doc, id), 0.0);
        assert!(!g.can_undo() && !g.undo(&mut doc), "the entry point is a wall");
        assert!(g.redo(&mut doc) && g.redo(&mut doc) && g.redo(&mut doc));
        assert!(g.end_session(&doc));
        // Outside: the same steps, one by one (no collapse), then the
        // creation before the session.
        assert!(g.undo(&mut doc));
        assert_eq!(x_of(&doc, id), 2.0);
        assert!(g.undo(&mut doc) && g.undo(&mut doc));
        assert_eq!(x_of(&doc, id), 0.0);
        assert!(g.undo(&mut doc));
        assert!(doc.entity(id).is_none());
    }

    #[test]
    fn cancel_restores_the_entry_state_and_drops_the_steps() {
        let mut doc = Document::new();
        let mut g = Gestures::default();
        let depth = doc.undo_depth();
        let id = cp(&mut doc);
        g.one_shot(depth);
        g.begin_session(&doc);
        set_x(&mut doc, &mut g, id, 5.0);
        set_x(&mut doc, &mut g, id, 6.0);
        g.undo(&mut doc);
        g.cancel_session(&mut doc);
        assert_eq!(x_of(&doc, id), 0.0);
        assert!(!g.can_redo(), "the session's steps cannot be redone");
        assert!(g.can_undo(), "earlier history is intact");
        // An empty session changes nothing.
        g.begin_session(&doc);
        assert!(!g.end_session(&doc));
        assert!(g.undo(&mut doc));
        assert!(doc.entity(id).is_none());
    }

    #[test]
    fn redo_from_before_a_session_is_not_replayed_inside_it() {
        let mut doc = Document::new();
        let mut g = Gestures::default();
        let depth = doc.undo_depth();
        let id = cp(&mut doc);
        g.one_shot(depth);
        set_x(&mut doc, &mut g, id, 1.0);
        assert!(g.undo(&mut doc));
        g.begin_session(&doc);
        assert!(!g.can_redo() && !g.redo(&mut doc), "the earlier redo belongs outside");
        g.cancel_session(&mut doc);
        assert!(g.can_redo(), "an empty session keeps the earlier redo");
        assert!(g.redo(&mut doc));
        assert_eq!(x_of(&doc, id), 1.0);
    }

    #[test]
    fn the_session_floor_keeps_preparation_out_of_session_undo() {
        let mut doc = Document::new();
        let mut g = Gestures::default();
        let depth = doc.undo_depth();
        let id = cp(&mut doc);
        g.one_shot(depth);
        g.begin_session(&doc);
        set_x(&mut doc, &mut g, id, 1.0); // preparation (a conversion)
        g.set_session_floor(&doc);
        set_x(&mut doc, &mut g, id, 2.0);
        assert!(g.undo(&mut doc));
        assert_eq!(x_of(&doc, id), 1.0);
        assert!(!g.can_undo() && !g.undo(&mut doc), "the preparation is below the floor");
        assert!(g.redo(&mut doc));
        assert!(g.end_session(&doc));
        // After ✓ the preparation is an ordinary step.
        assert!(g.undo(&mut doc) && g.undo(&mut doc));
        assert_eq!(x_of(&doc, id), 0.0);
        // And ✗ reverts it with the edits.
        g.begin_session(&doc);
        set_x(&mut doc, &mut g, id, 7.0);
        g.set_session_floor(&doc);
        g.cancel_session(&mut doc);
        assert_eq!(x_of(&doc, id), 0.0);
    }
}

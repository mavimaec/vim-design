//! App-level gesture grouping for undo/redo (shared by the demo and the
//! authoring app).
//!
//! The document records one undo step per submitted command (consecutive
//! coalesced updates to one entity merge). A single user gesture often
//! submits many commands — drawing a floor plate creates dozens of
//! entities, a slider drag touches several control points per event — so
//! the app records the document's undo depth at each gesture start and
//! undoes/redoes whole gestures. Result: one user action = one undo step.

use vim_design_lib::Document;

#[derive(Default)]
pub struct Gestures {
    /// Undo depths recorded at the start of each gesture.
    marks: Vec<usize>,
    /// Step counts popped by `undo`, consumed by `redo`.
    redo_counts: Vec<usize>,
    /// The open continuous gesture (slider drag, typing into a field).
    current: Option<String>,
    /// An open transaction (Edit Mode): see [`Gestures::begin_transaction`].
    transaction: Option<Transaction>,
}

/// State saved when a transaction opens.
struct Transaction {
    /// Document undo depth at entry.
    depth: usize,
    /// In-transaction undo never goes below this depth (≥ `depth`): steps
    /// before it belong to the transaction but are not user edits.
    floor: usize,
    /// Gesture marks at entry (restored on commit and rollback).
    marks: Vec<usize>,
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
        self.redo_counts.clear();
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
        self.redo_counts.clear();
        self.current = None;
    }

    /// Forget the redo history. Used after a failed multi-command
    /// operation was rolled back with `Document::undo` (which leaves the
    /// rolled-back steps on the document's redo stack): the app's redo
    /// must never replay them.
    pub fn invalidate_redo(&mut self) {
        self.redo_counts.clear();
        self.current = None;
    }

    /// Open a transaction: gestures inside it undo/redo one by one, but
    /// never past the entry point; [`Self::commit_transaction`] turns
    /// them into ONE gesture, [`Self::rollback_transaction`] reverts them.
    pub fn begin_transaction(&mut self, doc: &Document) {
        if self.transaction.is_none() {
            let depth = doc.undo_depth();
            self.transaction = Some(Transaction { depth, floor: depth, marks: self.marks.clone() });
            self.current = None;
        }
    }

    pub fn in_transaction(&self) -> bool {
        self.transaction.is_some()
    }

    /// Raise the transaction's undo floor to the current depth: what was
    /// done so far (e.g. a conversion that prepares the edit) cannot be
    /// undone step by step, but commit and rollback still include it.
    pub fn set_transaction_floor(&mut self, doc: &Document) {
        if let Some(t) = self.transaction.as_mut() {
            t.floor = doc.undo_depth();
            // The steps below the floor must not be undone one by one.
            self.marks.retain(|m| *m < t.depth);
        }
    }

    /// Close the transaction keeping its changes as a single undo step.
    /// Returns whether anything changed.
    pub fn commit_transaction(&mut self, doc: &Document) -> bool {
        let Some(t) = self.transaction.take() else { return false };
        self.marks = t.marks;
        self.current = None;
        let changed = doc.undo_depth() > t.depth;
        if changed {
            self.marks.push(t.depth);
            self.redo_counts.clear();
        }
        changed
    }

    /// Close the transaction reverting the document to its entry state.
    /// The transaction's redo history is dropped.
    pub fn rollback_transaction(&mut self, doc: &mut Document) {
        let Some(t) = self.transaction.take() else { return };
        while doc.undo_depth() > t.depth {
            if doc.undo().is_err() {
                break;
            }
        }
        self.marks = t.marks;
        self.redo_counts.clear();
        self.current = None;
    }

    pub fn undo(&mut self, doc: &mut Document) -> bool {
        let depth = doc.undo_depth();
        while self.marks.last().is_some_and(|m| *m >= depth) {
            self.marks.pop();
        }
        // Inside a transaction, never undo past its entry point.
        if let Some(t) = &self.transaction
            && self.marks.last().is_none_or(|m| *m < t.floor)
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
        match &self.transaction {
            Some(t) => self.marks.last().is_some_and(|m| *m >= t.floor),
            None => !self.marks.is_empty(),
        }
    }

    pub fn can_redo(&self) -> bool {
        !self.redo_counts.is_empty()
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
    fn transaction_commit_collapses_into_one_step() {
        let mut doc = Document::new();
        let mut g = Gestures::default();
        let depth = doc.undo_depth();
        let id = cp(&mut doc);
        g.one_shot(depth);
        g.begin_transaction(&doc);
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
        assert!(g.commit_transaction(&doc));
        // Outside: one undo reverts the whole session.
        assert!(g.undo(&mut doc));
        assert_eq!(x_of(&doc, id), 0.0);
        assert!(g.redo(&mut doc));
        assert_eq!(x_of(&doc, id), 3.0);
        assert!(g.undo(&mut doc) && g.undo(&mut doc));
        assert!(doc.entity(id).is_none(), "then the creation before it");
    }

    #[test]
    fn transaction_rollback_restores_the_entry_state() {
        let mut doc = Document::new();
        let mut g = Gestures::default();
        let depth = doc.undo_depth();
        let id = cp(&mut doc);
        g.one_shot(depth);
        g.begin_transaction(&doc);
        set_x(&mut doc, &mut g, id, 5.0);
        set_x(&mut doc, &mut g, id, 6.0);
        g.undo(&mut doc);
        g.rollback_transaction(&mut doc);
        assert_eq!(x_of(&doc, id), 0.0);
        assert!(!g.can_redo(), "the session's redo is dropped");
        assert!(g.can_undo(), "earlier history is intact");
        // An empty session commits nothing.
        g.begin_transaction(&doc);
        assert!(!g.commit_transaction(&doc));
        assert!(g.undo(&mut doc));
        assert!(doc.entity(id).is_none());
    }

    #[test]
    fn transaction_floor_keeps_preparation_out_of_step_undo() {
        let mut doc = Document::new();
        let mut g = Gestures::default();
        let depth = doc.undo_depth();
        let id = cp(&mut doc);
        g.one_shot(depth);
        g.begin_transaction(&doc);
        set_x(&mut doc, &mut g, id, 1.0); // preparation (a conversion)
        g.set_transaction_floor(&doc);
        set_x(&mut doc, &mut g, id, 2.0);
        assert!(g.undo(&mut doc));
        assert_eq!(x_of(&doc, id), 1.0);
        assert!(!g.can_undo() && !g.undo(&mut doc), "the preparation is below the floor");
        g.rollback_transaction(&mut doc);
        assert_eq!(x_of(&doc, id), 0.0, "rollback includes the preparation");
    }
}

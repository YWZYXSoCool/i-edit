//! The undo unit: one reversible replacement of a span of text.
//!
//! An [`Edit`] records what a single editing command did well enough to put it
//! back and take it away again: where the span started, what left, what came
//! in, and where the cursor sat before and after. Reversing the replacement is
//! then just the same splice with the two sides swapped, which is why undo and
//! redo are the one method each.
//!
//! The interesting part is coalescing. A typing burst should be *one* undo
//! step, not one per keystroke, so consecutive keystroke-sized edits are folded
//! together — see [`Edit::try_absorb`]. The `chainable` flag is what keeps a
//! run open: a merged edit is no longer keystroke-sized itself, but the run it
//! belongs to still is.

use crate::Cursor;
use crate::text::TextState;

#[derive(Debug, Clone)]
pub struct Edit {
    pub start: Cursor,
    pub removed: String,
    pub inserted: String,
    pub before: Cursor,
    pub after: Cursor,
    /// Whether this entry may still absorb an adjacent single-character edit.
    /// True for a freshly recorded keystroke-sized edit — and for the run such
    /// edits merge into — false for anything structural (a paste, a selection
    /// delete, a newline). It is what keeps "a", "b", "c" folding into one undo
    /// step instead of stopping after two: once merged, an edit is no longer
    /// keystroke-sized, but the run it belongs to is still open.
    chainable: bool,
}

impl Edit {
    pub fn undo(&self, state: &mut TextState) {
        state.apply_splice(self.start, &self.inserted, &self.removed);
        state.cursor = self.before;
        state.clamp_cursor();
    }

    pub fn redo(&self, state: &mut TextState) {
        state.apply_splice(self.start, &self.removed, &self.inserted);
        state.cursor = self.after;
        state.clamp_cursor();
    }

    /// A new edit, not yet part of any typing run.
    ///
    /// This is the constructor [`TextState`](crate::text::TextState) builds its
    /// commands from; `chainable` is decided afterwards by
    /// [`Self::chainable_if_atomic`], which is the only place that reads the
    /// shape of an edit to decide whether it may coalesce.
    pub(crate) fn new(
        start: Cursor,
        removed: String,
        inserted: String,
        before: Cursor,
        after: Cursor,
    ) -> Edit {
        Edit {
            start,
            removed,
            inserted,
            before,
            after,
            chainable: false,
        }
    }

    /// A no-op edit anchored at `cursor`: the safe return value for commands
    /// that decide not to change anything (e.g. backspace at the buffer start).
    pub fn identity(cursor: Cursor) -> Edit {
        Edit::new(cursor, String::new(), String::new(), cursor, cursor)
    }

    /// Same edit, flagged chainable when it is keystroke-sized.
    pub(crate) fn chainable_if_atomic(mut self) -> Edit {
        let chainable = self.is_atomic();
        self.chainable = chainable;
        self
    }

    /// True when this edit is a single character added or removed — the only
    /// shape we fold across keystrokes so a typing run becomes one undo step.
    fn is_atomic(&self) -> bool {
        // Counted in chars, not bytes, so a CJK char or emoji still counts as
        // one keystroke's worth of text. Neither side may carry a newline:
        // Enter and Backspace-at-a-line-start each end the run, matching how
        // every other editor groups a typing burst.
        let crosses_line = |s: &str| s == "\n";
        let single_insert = self.inserted.chars().count() == 1
            && self.removed.is_empty()
            && !crosses_line(&self.inserted);
        let single_remove = self.inserted.is_empty()
            && self.removed.chars().count() == 1
            && !crosses_line(&self.removed);
        single_insert || single_remove
    }

    /// Folds `next` onto `self` when they continue the same typing gesture.
    ///
    /// Returns whether it did. The merge is exact: it only combines edits that
    /// sit end to end, so a paste, a selection delete, or typing after moving
    /// the cursor each start a new entry.
    ///
    /// Growing the run's `String`s in place is what keeps this cheap: shaping
    /// the answer into a fresh `Edit` would re-copy the whole run on every
    /// keystroke, whereas `push_str` costs nothing but amortized growth — the
    /// same reason editing the line itself is cheap.
    pub(crate) fn try_absorb(&mut self, next: &Edit) -> bool {
        // `self` only has to be an open run; requiring it to still be
        // keystroke-sized would end every run at two characters.
        if !self.chainable || !next.is_atomic() {
            return false;
        }

        // Forward chain (typing, or deleting forward with the Delete key):
        // `next` begins exactly where `self` ended.
        if next.start.y == self.after.y && next.start.x == self.after.x {
            self.inserted.push_str(&next.inserted);
            self.removed.push_str(&next.removed);
            self.after = next.after;
            return true;
        }

        // Backward-delete chain (Backspace): `next` removes the char immediately
        // left of `self`, so its removed text belongs in front of `self.removed`
        // in document order.
        if next.start.y == self.start.y && next.start.x + next.removed.len() == self.start.x {
            self.removed.insert_str(0, &next.removed);
            self.start = next.start;
            self.before = next.before;
            self.after = next.after;
            return true;
        }

        false
    }

    /// Merges a contiguous forward chain of edits (those produced by a single
    /// key press, e.g. "delete the selection, then type") into one undo entry.
    ///
    /// The result is never chainable: a keystroke that rewrites a selection is
    /// its own unit, not part of the run around it.
    pub fn merge_chain(edits: &[Edit]) -> Edit {
        let mut acc = edits[0].clone();
        acc.chainable = false;
        for e in &edits[1..] {
            acc = Edit {
                start: acc.start,
                removed: acc.removed.clone() + &e.removed,
                inserted: acc.inserted.clone() + &e.inserted,
                before: acc.before,
                after: e.after,
                chainable: false,
            };
        }
        acc
    }
}

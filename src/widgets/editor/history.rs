//! The undo history behind [`EditorState`](super::EditorState).
//!
//! Every edit flows through [`History::record`], which stashes the reversible
//! [`Edit`] a [`TextState`](crate::text::TextState) primitive handed back. Two
//! things are folded on the way in:
//!
//! * **Transactions.** A key press gathers its edits in one transaction, so
//!   "replace the selection, then type" is a single step.
//! * **Typing runs.** Consecutive single-character typing or deletion is folded
//!   across keystrokes into one entry, so a burst types and undoes as a unit.
//!
//! Neither folding changes the *identity* of an entry: every push — including
//! one that merely extends an existing entry — takes a fresh serial. Comparing
//! the serial of the head of the undo stack against `saved_serial` is therefore
//! what answers "does the buffer equal what is on disk?", even after undo/redo
//! and even though a coalesced edit does not change the stack depth.

use crate::text::Edit;

/// An undo entry plus the identity `dirty` is computed from.
///
/// Every push — including one that merely extends an existing entry — takes a
/// fresh serial, so the head of the undo stack changes identity whenever the
/// buffer does.
#[derive(Debug, Clone)]
pub struct HistoryEntry {
    pub(crate) edit: Edit,
    serial: u64,
}

/// The undo/redo stacks, plus the transaction being gathered.
#[derive(Debug)]
pub struct History {
    /// Edits available to undo, newest on top.
    undo_stack: Vec<HistoryEntry>,
    /// Edits available to redo, newest on top.
    redo_stack: Vec<HistoryEntry>,
    /// Source of the serials handed out below; never decreases.
    next_serial: u64,
    /// Serial of the buffer state as last written to disk. Comparing it against
    /// the head of `undo_stack` answers "does the buffer equal what is on disk?"
    /// even after undo/redo and even though a coalesced edit does not change the
    /// stack depth. 0 always names the pristine, just-loaded buffer.
    saved_serial: u64,
    /// Set while `undo`/`redo` replay an edit, so their own mutation is not
    /// recorded back onto the history it is replaying.
    in_history_op: bool,
    /// Edits gathered during the current key press, merged into one undo entry.
    txn: Vec<Edit>,
    /// Whether edits are currently being gathered into [`Self::txn`].
    txn_active: bool,
}

impl Default for History {
    fn default() -> Self {
        Self {
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            next_serial: 1,
            saved_serial: 0,
            in_history_op: false,
            txn: Vec::new(),
            txn_active: false,
        }
    }
}

impl History {
    pub fn new() -> Self {
        Self::default()
    }

    /// Records `edit`, unless it is a no-op or we are replaying an undo/redo.
    ///
    /// While a transaction is open, edits are gathered rather than pushed
    /// immediately.
    pub fn record(&mut self, edit: Edit) {
        if self.in_history_op {
            return;
        }
        if edit.removed.is_empty() && edit.inserted.is_empty() {
            return;
        }
        if self.txn_active {
            self.txn.push(edit);
        } else {
            self.push_edit(edit);
        }
    }

    /// Opens a transaction that collects edits until [`Self::end_txn`].
    pub fn begin_txn(&mut self) {
        self.txn_active = true;
        self.txn.clear();
    }

    /// Closes the active transaction, merging its edits into one undo entry.
    ///
    /// Returns whether anything was recorded, which is how the editor tells a
    /// key press that changed the document from one that did not.
    pub fn end_txn(&mut self) -> bool {
        self.txn_active = false;
        if self.txn.is_empty() {
            return false;
        }
        // `pop` rather than `mem::take`: it moves the edit out while leaving the
        // transaction's allocation in place for the next key press to refill.
        if self.txn.len() == 1 {
            if let Some(edit) = self.txn.pop() {
                self.push_edit(edit);
            }
            return true;
        }
        self.push_edit(Edit::merge_chain(&self.txn));
        self.txn.clear();
        true
    }

    /// Pushes `edit` onto the undo stack, folding it into the top entry when it
    /// continues the same typing run, and discarding the redo chain. Both paths
    /// hand out a fresh serial, so even a fold reads as a new buffer state.
    fn push_edit(&mut self, edit: Edit) {
        if let Some(mut top) = self.undo_stack.pop() {
            if top.edit.try_absorb(&edit) {
                // The run grew, so its identity changed: a buffer whose run now
                // holds an extra character still differs from what was saved.
                top.serial = self.take_serial();
                self.undo_stack.push(top);
                self.redo_stack.clear();
                return;
            }
            self.undo_stack.push(top);
        }
        let serial = self.take_serial();
        self.undo_stack.push(HistoryEntry { edit, serial });
        self.redo_stack.clear();
    }

    fn take_serial(&mut self) -> u64 {
        let serial = self.next_serial;
        self.next_serial += 1;
        serial
    }

    /// Identity of the buffer's current position in history; 0 is the state a
    /// freshly loaded file starts from.
    fn current_serial(&self) -> u64 {
        self.undo_stack.last().map_or(0, |e| e.serial)
    }

    /// Anchors the saved point to the current position in history, so an undo
    /// that returns the buffer to this state clears `dirty` again.
    pub fn mark_saved(&mut self) {
        self.saved_serial = self.current_serial();
    }

    /// Whether the buffer differs from what was last written to disk.
    pub fn is_dirty(&self) -> bool {
        self.current_serial() != self.saved_serial
    }

    /// Drops everything: the history of a buffer does not follow it across
    /// files, so undo cannot reach into the previous one.
    pub fn reset(&mut self) {
        self.undo_stack.clear();
        self.redo_stack.clear();
        self.next_serial = 1;
        self.saved_serial = 0;
        self.in_history_op = false;
        self.txn.clear();
        self.txn_active = false;
    }

    /// Takes the entry to undo and opens the replay window.
    ///
    /// `None` when there is nothing to undo, or when a replay is already open.
    /// The caller applies [`HistoryEntry::edit`] and then hands it back to
    /// [`Self::finish_undo`], which is what closes the window.
    pub fn begin_undo(&mut self) -> Option<HistoryEntry> {
        if self.in_history_op {
            return None;
        }
        let entry = self.undo_stack.pop()?;
        self.in_history_op = true;
        Some(entry)
    }

    /// Closes the replay window opened by [`Self::begin_undo`], filing `entry`
    /// as redoable.
    pub fn finish_undo(&mut self, entry: HistoryEntry) {
        self.in_history_op = false;
        self.redo_stack.push(entry);
    }

    /// Takes the entry to redo and opens the replay window; see
    /// [`Self::begin_undo`].
    pub fn begin_redo(&mut self) -> Option<HistoryEntry> {
        if self.in_history_op {
            return None;
        }
        let entry = self.redo_stack.pop()?;
        self.in_history_op = true;
        Some(entry)
    }

    /// Closes the replay window opened by [`Self::begin_redo`], filing `entry`
    /// back onto the undo stack.
    pub fn finish_redo(&mut self, entry: HistoryEntry) {
        self.in_history_op = false;
        self.undo_stack.push(entry);
    }
}

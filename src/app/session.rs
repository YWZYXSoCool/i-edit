//! Picking up where the last run left off.
//!
//! Restoring a session is best effort throughout. A path that has since been
//! deleted or renamed is skipped, and a file that no longer reads leaves the
//! scratch buffer in place — the editor starting on an empty buffer is fine,
//! refusing to start is not.

use std::path::PathBuf;

use crate::Cursor;
use crate::app::App;
use crate::fs;

/// Directories re-expanded from the last run.
///
/// Rebuilding the tree reads a directory per expansion, so a session that left
/// hundreds open is cut short: the ones restored come first in the stored
/// order, the rest open collapsed, and expanding them by hand costs one read
/// each — the same as not having remembered them.
const RESTORE_EXPANDED_LIMIT: usize = 32;

impl App {
    /// Reopens what the last run was looking at: the folder in the tree, then
    /// the buffer, then where the cursor was in it.
    ///
    /// The panel toggles come back whether or not the session itself is
    /// restored: they describe how the editor is used, not what was open.
    pub(super) fn restore_session(&mut self) {
        self.file_tree_visible = self.storage.state().file_tree_visible;

        if !self.storage.config().restore_session {
            return;
        }

        let folder = self.storage.session().last_folder.clone();
        if let Some(folder) = folder {
            self.restore_folder(folder);
        }

        let file = self.storage.session().last_file.clone();
        if let Some(file) = file {
            self.restore_file(file);
        }
    }

    /// Roots the tree at `path` and re-expands the directories it left open.
    fn restore_folder(&mut self, path: PathBuf) {
        if !path.is_dir() {
            return;
        }

        self.file_tree_state.open_root(path.clone());
        self.file_tree_visible = true;
        self.storage
            .edit_session(|session| session.set_folder(path.clone()));
        self.storage.edit_state(|state| {
            state.touch_dir(path.clone());
            state.last_dir = Some(path);
        });

        // Read before the loop: the directory list borrows storage, which the
        // loop needs to hand to the tree.
        let expanded: Vec<PathBuf> = self
            .storage
            .session()
            .expanded_dirs()
            .iter()
            .take(RESTORE_EXPANDED_LIMIT)
            .cloned()
            .collect();

        for dir in expanded {
            self.file_tree_state.set_expanded(&dir, true);
        }
    }

    /// Reads `path` into the buffer and puts the cursor back where it was.
    fn restore_file(&mut self, path: PathBuf) {
        let saved = self
            .storage
            .session()
            .view(&path)
            .map(|view| (view.line, view.col, view.top));

        match fs::read_text_file(&path) {
            Ok(lines) => {
                self.editor_state.load_file(path.clone(), lines);

                if let Some((line, col, top)) = saved {
                    // The file may have shrunk or changed since; `clamp_cursor`
                    // puts the cursor back inside it and on a char boundary.
                    self.editor_state.text.cursor = Cursor { x: col, y: line };
                    self.editor_state.text.clamp_cursor();
                    self.editor_state.viewport_state.scroll_y = top;
                }
            }
            Err(err) => {
                self.message_box_state
                    .error(format!("cannot reopen: {err}"));
            }
        }
    }

    /// Records where the current buffer is, so a return to it starts there.
    pub(super) fn remember_current_view(&mut self) {
        let Some(path) = self.editor_state.path.clone() else {
            return;
        };

        let line = self.editor_state.text.cursor.y;
        let col = self.editor_state.text.cursor.x;
        let top = self.editor_state.viewport_state.scroll_y;

        self.storage
            .edit_session(|session| session.remember_view(path, line, col, top));
    }

    /// Persists the directories the tree expanded or collapsed this round.
    pub(super) fn apply_expansion_changes(&mut self) {
        if self.expansion_buf.capacity() == 0 {
            self.expansion_buf = Vec::with_capacity(8);
        }

        // Taken out of `self` so the loop below can borrow storage mutably;
        // put back afterwards so the capacity survives the round.
        let mut changes = core::mem::take(&mut self.expansion_buf);
        self.file_tree_state
            .take_expansion_changes_into(&mut changes);

        for (path, expanded) in changes.drain(..) {
            self.storage
                .edit_session(|session| session.set_expanded(path, expanded));
        }

        self.expansion_buf = changes;
    }
}

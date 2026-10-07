//! Multiple open buffers, presented as tabs above the editor.
//!
//! The editor has always held a single [`EditorState`]; this owns the *set* of
//! them and which one is current. Everything else in the shell keeps talking to
//! [`Tabs::active`] — the active buffer — so the rest of the code never learns
//! there is more than one. Opening a file focuses an existing tab for it or
//! pushes a new one; closing removes the current and keeps at least one (a
//! scratch) so the editor is never left with nothing to edit.

use std::path::Path;
use std::path::PathBuf;

use crate::storage::config::TabIndent;
use crate::widgets::EditorState;

/// The open buffers.
///
/// `buffers[active]` is the one the editor and language server are looking at;
/// the rest sit behind their tabs until focused.
pub struct Tabs {
    buffers: Vec<EditorState>,
    active: usize,
    /// What Tab inserts, copied into every buffer. Kept here as well as in the
    /// buffers so a tab opened after the setting changed still gets it.
    tab_indent: TabIndent,
}

impl Tabs {
    /// A fresh editor: one empty scratch buffer, active.
    pub fn new() -> Self {
        Self {
            buffers: vec![EditorState::new()],
            active: 0,
            tab_indent: TabIndent::default(),
        }
    }

    /// The buffer the editor edits and the language server colors.
    pub fn active(&self) -> &EditorState {
        &self.buffers[self.active]
    }

    /// Mutable form of [`Self::active`].
    pub fn active_mut(&mut self) -> &mut EditorState {
        &mut self.buffers[self.active]
    }

    /// How many tabs are open. Always at least one: closing the last tab
    /// leaves a scratch buffer behind.
    pub fn len(&self) -> usize {
        self.buffers.len()
    }

    /// Index of the active tab.
    pub fn active_index(&self) -> usize {
        self.active
    }

    /// Every buffer, in display order.
    pub fn buffers(&self) -> &[EditorState] {
        &self.buffers
    }

    /// Focus tab `i`.
    pub fn set_active(&mut self, i: usize) {
        if i < self.buffers.len() {
            self.active = i;
        }
    }

    /// The index of the tab holding `path`, if any.
    pub fn find(&self, path: &Path) -> Option<usize> {
        self.buffers
            .iter()
            .position(|buffer| buffer.path.as_deref() == Some(path))
    }

    /// Move the active tab by `delta`, wrapping around the ends.
    pub fn move_active_by(&mut self, delta: isize) {
        let count = self.buffers.len() as isize;
        if count <= 1 {
            return;
        }
        let next = (((self.active as isize) + delta) % count + count) % count;
        self.active = next as usize;
    }

    /// What the Tab key inserts, and into which buffers: every one of them,
    /// present and future.
    ///
    /// The setting is global — there is no per-file indentation — so it is
    /// pushed rather than fetched: a component handling a key only sees its own
    /// state and has nowhere to read storage from.
    pub fn set_tab_indent(&mut self, indent: TabIndent) {
        self.tab_indent = indent;

        for buffer in &mut self.buffers {
            buffer.tab_indent = indent;
        }
    }

    /// Open `path` with `lines`, either focusing the tab that already has it or
    /// adding a new one. A lone, untouched, pathless scratch at the active slot
    /// is replaced in place rather than spawning a second tab, so opening the
    /// first file does not leave an empty tab behind. Returns the focused index.
    pub fn focus_or_open(&mut self, path: PathBuf, lines: Vec<String>) -> usize {
        if let Some(i) = self.find(&path) {
            self.active = i;
            return i;
        }

        let replace_scratch =
            self.buffers[self.active].path.is_none() && !self.buffers[self.active].dirty;
        if replace_scratch {
            self.buffers[self.active].load_file(path, lines);
            return self.active;
        }

        let mut buffer = EditorState::new();
        buffer.tab_indent = self.tab_indent;
        buffer.load_file(path, lines);
        self.buffers.push(buffer);
        self.active = self.buffers.len() - 1;
        self.active
    }

    /// Closes every buffer whose file lives under `root` and holds no unsaved
    /// changes, returning the paths that went.
    ///
    /// A dirty buffer is kept: closing a tab throws the buffer away, and that
    /// is not a decision this sweep makes for the user — the confirm around a
    /// single tab is where it belongs. The editor is never left with nothing to
    /// edit, so a sweep that would empty it leaves a scratch buffer behind.
    pub fn close_under(&mut self, root: &Path) -> Vec<PathBuf> {
        let mut closed = Vec::new();

        self.buffers.retain(|buffer| {
            let Some(path) = buffer.path.as_deref() else {
                return true;
            };

            let closes = path.starts_with(root) && !buffer.dirty;
            if closes {
                closed.push(path.to_path_buf());
            }

            !closes
        });

        if self.buffers.is_empty() {
            let mut scratch = EditorState::new();
            scratch.tab_indent = self.tab_indent;
            self.buffers.push(scratch);
        }

        if self.active >= self.buffers.len() {
            self.active = self.buffers.len() - 1;
        }

        closed
    }

    /// Close the active tab, returning the index that went.
    ///
    /// Every tab can be closed — including the last one. What takes its place
    /// is a *new* scratch buffer: the editor is never left with nothing to
    /// edit, but it is left with nothing of what was closed.
    pub fn close_active(&mut self) -> usize {
        let removed = self.active;
        self.buffers.remove(self.active);

        if self.buffers.is_empty() {
            let mut scratch = EditorState::new();
            scratch.tab_indent = self.tab_indent;
            self.buffers.push(scratch);
        }

        if self.active >= self.buffers.len() {
            self.active = self.buffers.len() - 1;
        }

        removed
    }
}

impl Default for Tabs {
    fn default() -> Self {
        Self::new()
    }
}

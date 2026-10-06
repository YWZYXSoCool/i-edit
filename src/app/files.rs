//! Opening, saving and overwriting — the file operations the shell performs.
//!
//! Anything that could lose work asks first. Quitting or opening with a dirty
//! buffer stashes the action and opens a confirm; the answer comes back through
//! [`App::resolve_confirm`](crate::app::App::resolve_confirm). A dirty *scratch*
//! buffer is the exception: it is a throwaway pad with no path to save it to, so
//! it is dropped silently.

use std::path::{Path, PathBuf};

use crate::action::Action;
use crate::app::{App, Focus};
use crate::fs;
use crate::storage::state::PickerKind;
use crate::widgets::picker::PickerMode;
use crate::widgets::popup::ConfirmKind;

impl App {
    /// Quits, but asks first when a file-backed buffer holds unsaved changes.
    ///
    /// A dirty scratch buffer is a throwaway pad with no path to save it to,
    /// so quitting drops it silently instead of prompting.
    pub(super) fn request_quit(&mut self) -> bool {
        if self.editor_state.dirty && self.editor_state.path.is_some() {
            self.pending = Some(Action::Quit);
            self.popup_state.open_confirm(ConfirmKind::UnsavedChanges);
            false
        } else {
            true
        }
    }

    /// Opens `path`, but asks first when the buffer holds unsaved changes.
    pub(super) fn request_load_file(&mut self, path: PathBuf) {
        if self.editor_state.dirty {
            self.pending = Some(Action::LoadFile(path));
            self.popup_state.open_confirm(ConfirmKind::UnsavedChanges);
        } else {
            self.load_file_now(path);
        }
    }

    /// Reads `path` and replaces the buffer. The original buffer is only kept
    /// when the read fails.
    pub(super) fn load_file_now(&mut self, path: PathBuf) {
        self.popup_state.close();

        match fs::read_text_file(&path) {
            Ok(lines) => {
                // The buffer being replaced gets its position recorded first.
                self.remember_current_view();
                self.storage
                    .edit_session(|session| session.last_file = Some(path.clone()));
                self.storage.edit_state(|state| {
                    state.touch_file(path.clone());
                    state.last_dir = path.parent().map(Path::to_path_buf);
                    // The directory the file picker was browsing when it
                    // confirmed, so reopening it starts there.
                    if let Some(dir) = path.parent() {
                        state.set_picker_dir(PickerKind::OpenFile, dir.to_path_buf());
                    }
                });
                let opened = format!("opened: {}", path.display());
                self.editor_state.load_file(path, lines);
                // Loading returns focus to the editor and confirms the open,
                // the same way opening a folder surfaces its result.
                self.set_focus(Focus::Editor);
                self.message_box_state.success(opened);
            }
            Err(err) => {
                self.message_box_state.error(format!("cannot open: {err}"));
            }
        }
    }

    pub(super) fn open_folder(&mut self, path: PathBuf) {
        self.popup_state.close();

        if !path.is_dir() {
            self.message_box_state.error(format!(
                "cannot open: {} is not a directory",
                path.display()
            ));
            return;
        }

        self.file_tree_state.open_root(path.clone());
        self.file_tree_visible = true;
        self.message_box_state
            .success(format!("opened: {}", path.display()));

        // A new root throws away the old expansion set, so the stored one goes
        // with it; the tree reports the root itself, which is already expanded.
        self.storage
            .edit_session(|session| session.set_folder(path.clone()));
        self.storage.edit_state(|state| {
            state.touch_dir(path.clone());
            state.last_dir = Some(path.clone());
            state.set_picker_dir(PickerKind::OpenFolder, path);
        });
    }

    /// Saves to the buffer's own path, or asks for one when it has none.
    pub(super) fn save_current(&mut self) {
        match self.editor_state.path.clone() {
            Some(path) => {
                self.write_buffer(&path);
            }
            None => self.open_picker(PickerMode::Save),
        }
    }

    /// Writes the buffer to `path`. Returns whether the write succeeded.
    pub(super) fn write_buffer(&mut self, path: &Path) -> bool {
        match fs::write_text_file(path, &self.editor_state.text.lines) {
            Ok(()) => {
                self.editor_state.path = Some(path.to_path_buf());
                self.editor_state.mark_saved();
                self.storage.edit_state(|state| {
                    state.touch_file(path.to_path_buf());
                    state.last_dir = path.parent().map(Path::to_path_buf);
                });
                self.message_box_state
                    .success(format!("saved: {}", path.display()));
                true
            }
            Err(err) => {
                self.message_box_state.error(format!("save failed: {err}"));
                false
            }
        }
    }

    /// Confirmed picker target: write it, after an overwrite check if needed.
    pub(super) fn save_to(&mut self, path: PathBuf) -> bool {
        self.popup_state.close();

        if path.exists() {
            self.overwrite = Some(path);
            self.popup_state.open_confirm(ConfirmKind::Overwrite);
            return false;
        }

        if self.write_buffer(&path) {
            // Same for the save picker: it reopens in the directory the file
            // was written to.
            if let Some(dir) = path.parent() {
                let dir = dir.to_path_buf();
                self.storage
                    .edit_state(|state| state.set_picker_dir(PickerKind::SaveAs, dir));
            }

            match self.pending.take() {
                Some(continuation) => self.resume(continuation),
                None => false,
            }
        } else {
            // The continuation died with the failed write; the buffer stays
            // dirty so nothing is lost silently.
            self.pending = None;
            false
        }
    }

    pub(super) fn toggle_file_tree(&mut self) {
        self.file_tree_visible = !self.file_tree_visible;

        if !self.file_tree_visible {
            self.set_focus(Focus::Editor);
        }

        let visible = self.file_tree_visible;
        self.storage
            .edit_state(|state| state.file_tree_visible = visible);
    }
}

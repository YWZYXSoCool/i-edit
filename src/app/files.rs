//! Opening, saving and overwriting — the file operations the shell performs.
//!
//! Anything that could lose work asks first. Quitting or closing a tab with a
//! dirty buffer stashes the action and opens a confirm; the answer comes back
//! through
//! [`App::resolve_confirm`](crate::app::App::resolve_confirm). A dirty *scratch*
//! buffer is the exception: it is a throwaway pad with no path to save it to, so
//! it is dropped silently.
//!
//! Opening is not one of those: a file gets its own tab and the buffer that was
//! on screen keeps its own, so nothing is discarded on the way there.

use std::path::{Path, PathBuf};

use crate::action::Action;
use crate::app::{App, Focus};
use crate::fs;
use crate::storage::state::PickerKind;
use crate::widgets::file_tree::root_name;
use crate::widgets::picker::PickerMode;
use crate::widgets::popup::ConfirmKind;

impl App {
    /// Quits, but asks first when a file-backed buffer holds unsaved changes.
    ///
    /// A dirty scratch buffer is a throwaway pad with no path to save it to,
    /// so quitting drops it silently instead of prompting.
    pub(super) fn request_quit(&mut self) -> bool {
        if self.tabs.active().dirty && self.tabs.active().path.is_some() {
            self.pending = Some(Action::Quit);
            self.popup_state.open_confirm(ConfirmKind::UnsavedChanges);
            false
        } else {
            true
        }
    }

    /// Opens `path` in its own tab, without asking about unsaved changes.
    ///
    /// Nothing is lost on the way there: the file gets a tab of its own and the
    /// buffer that was on screen keeps its — dirty and still editable — so
    /// there is no work to be saved or discarded first.
    ///
    /// A file that already has a tab is only focused: it is not loaded again.
    pub(super) fn request_load_file(&mut self, path: PathBuf) {
        if let Some(open) = self.tabs.find(&path) {
            self.popup_state.close();
            self.switch_to_tab(open);
            self.set_focus(Focus::Editor);
            return;
        }

        self.load_file_now(path);
    }

    /// Tells the language server which buffer is on screen.
    ///
    /// Called after an open, after a save, and after a session restore. A
    /// document the server has already been given costs nothing to come back
    /// to — the client remembers it — so what this mostly does is hand over a
    /// file the server has never seen, and re-hand one whose buffer moved on
    /// while it was away. The first of those also starts a server; a save can
    /// rename the file into — or out of — a language the server handles.
    pub(super) fn sync_lsp(&mut self) {
        let Some(path) = self.tabs.active().path.clone() else {
            // A scratch buffer has no path, so no URI, so nothing to say.
            self.lsp.deactivate();
            return;
        };

        let version = self.tabs.active().version;
        if let Some(event) = self
            .lsp
            .open(&path, &self.tabs.active().text.lines, version)
        {
            self.apply_lsp_event(event);
        }
    }

    /// Starts the language server over.
    ///
    /// The way back from a server that died or was killed: the client treats a
    /// failure as final so that one death is not a message per keystroke, and
    /// this is the deliberate exception. The document is handed over again
    /// afterwards, because a new server knows nothing about it.
    pub(super) fn restart_lsp(&mut self) {
        if let Some(event) = self.lsp.restart() {
            self.apply_lsp_event(event);
            return;
        }

        self.sync_lsp();
        self.message_box_state.info("rust-analyzer: restarting");
    }

    /// Reads `path` into its own tab. A failed read leaves the tabs as they
    /// were.
    pub(super) fn load_file_now(&mut self, path: PathBuf) {
        self.popup_state.close();

        match fs::read_text_file(&path) {
            Ok(lines) => {
                self.remember_current_view();

                // The settings file is not a document being worked on: opening
                // it must not make it the file the next run restores, the
                // directory the pickers start in, or an entry in the recent
                // list.
                if !self.storage.is_settings_file(&path) {
                    self.storage
                        .edit_session(|session| session.last_file = Some(path.clone()));
                    self.storage.edit_state(|state| {
                        state.touch_file(path.clone());
                        state.last_dir = path.parent().map(Path::to_path_buf);
                        if let Some(dir) = path.parent() {
                            state.set_picker_dir(PickerKind::OpenFile, dir.to_path_buf());
                        }
                    });
                }

                self.tabs.focus_or_open(path, lines);
                self.after_tab_change();
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

    /// Closes the folder in the file tree, and the tabs that came from it.
    ///
    /// The panel stays visible — with no root it shows the hint that tells the
    /// user how to open one — but the session forgets the folder, so the next
    /// run does not reopen what was just closed.
    ///
    /// A buffer with unsaved changes is left open rather than discarded: this
    /// command is a sweep, not a decision about someone's edits.
    pub(super) fn close_folder(&mut self) {
        let Some(root) = self.file_tree_state.root().map(Path::to_path_buf) else {
            self.message_box_state.info("no folder is open");
            return;
        };

        self.popup_state.close();
        self.file_tree_state.close_root();
        self.storage.edit_session(|session| session.clear_folder());

        // The tree that had the keyboard is gone, so the keys belong to the
        // editor again.
        self.set_focus(Focus::Editor);

        let closed = self.tabs.close_under(&root);
        for path in &closed {
            self.lsp.forget(path);
        }

        // Whatever is left under the folder is the unsaved work that was kept.
        let kept = self
            .tabs
            .buffers()
            .iter()
            .filter(|buffer| {
                buffer.dirty
                    && buffer
                        .path
                        .as_deref()
                        .is_some_and(|path| path.starts_with(&root))
            })
            .count();

        self.after_tab_change();

        let mut text = format!("closed {}", root_name(&root));
        if !closed.is_empty() {
            text.push_str(&format!(" · {} closed", closed.len()));
        }

        if kept > 0 {
            self.message_box_state
                .warning(format!("{text} · {kept} kept, unsaved"));
        } else {
            self.message_box_state.success(text);
        }
    }

    /// Saves to the buffer's own path, or asks for one when it has none.
    pub(super) fn save_current(&mut self) {
        match self.tabs.active().path.clone() {
            Some(path) => {
                self.write_buffer(&path);
            }
            None => self.open_picker(PickerMode::Save),
        }
    }

    /// Writes the buffer to `path`. Returns whether the write succeeded.
    pub(super) fn write_buffer(&mut self, path: &Path) -> bool {
        match fs::write_text_file(path, &self.tabs.active().text.lines) {
            Ok(()) => {
                let buffer = self.tabs.active_mut();
                buffer.path = Some(path.to_path_buf());
                buffer.mark_saved();

                // Same reason as the open above: saving the settings is not
                // recent work worth remembering.
                let settings = self.storage.is_settings_file(path);
                if !settings {
                    self.storage.edit_state(|state| {
                        state.touch_file(path.to_path_buf());
                        state.last_dir = path.parent().map(Path::to_path_buf);
                    });
                }

                // A save can change what the file *is*: `save as x.rs` turns a
                // plain buffer into a Rust one, so the server is told again —
                // and the tab bar is told too, because a scratch buffer just
                // became a file the next run can reopen.
                self.after_tab_change();

                // Saving the settings file is how settings are applied, so the
                // write is what triggers the reread.
                if settings {
                    self.reload_settings();
                }

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

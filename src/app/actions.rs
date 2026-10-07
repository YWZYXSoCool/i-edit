//! Dispatching a queued action, and resuming one that had to wait.
//!
//! Two of these need a second look at the user before they can run: quitting or
//! closing a tab while the buffer is dirty. Both stash the action in
//! `pending`, ask, and come back through [`App::resolve_confirm`] — which is
//! why `resolve_confirm` and `resume` live next to the dispatch rather than
//! with the file operations they eventually trigger.

use std::env;
use std::path::{Path, PathBuf};

use crate::action::{Action, ConfirmChoice};
use crate::app::App;
use crate::storage::state::PickerKind;
use crate::widgets::picker::PickerMode;
use crate::widgets::popup::PopupKind;

impl App {
    /// Opens a popup, giving the picker kinds their initial directory.
    pub(super) fn open_popup(&mut self, kind: PopupKind) {
        match kind {
            PopupKind::OpenFile => self.open_picker(PickerMode::File),
            PopupKind::OpenFolder => self.open_picker(PickerMode::Folder),
            PopupKind::SaveAs => self.open_picker(PickerMode::Save),
            _ => self.popup_state.open(kind),
        }
    }

    pub(super) fn open_picker(&mut self, mode: PickerMode) {
        let start_dir = self.picker_start_dir(picker_kind(mode));
        let preset = self.tabs.active().path.clone();
        self.popup_state.open_picker(mode, start_dir, preset);
    }

    /// Initial directory for one picker: where *that* picker was last used,
    /// then the open folder, then the last directory anything used, then the
    /// process working directory.
    ///
    /// A remembered directory that no longer exists is skipped rather than
    /// shown: the picker would open on a listing that cannot be read and the
    /// user would have to type their way out of it. Every candidate is checked,
    /// because a stale one can be stored in any of them.
    fn picker_start_dir(&self, kind: PickerKind) -> PathBuf {
        let existing = |dir: Option<&PathBuf>| dir.filter(|dir| dir.is_dir()).cloned();

        existing(self.storage.state().picker_dir(kind))
            .or_else(|| self.file_tree_state.root().map(Path::to_path_buf))
            .or_else(|| existing(self.storage.state().last_dir.as_ref()))
            .or_else(|| env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("."))
    }

    /// Dismisses the open popup and aborts whatever it was going to resume.
    pub(super) fn cancel_popup(&mut self) {
        self.pending = None;
        self.overwrite = None;
        self.popup_state.close();
    }

    /// Carries out the answer to a confirm the shell asked for.
    pub(super) fn resolve_confirm(&mut self, choice: ConfirmChoice) -> bool {
        match choice {
            ConfirmChoice::Save => {
                let continuation = self.pending.take();
                let path = self.tabs.active().path.clone();
                self.popup_state.close();

                match path {
                    Some(path) => {
                        let written = self.write_buffer(&path);
                        match (written, continuation) {
                            (true, Some(continuation)) => self.resume(continuation),
                            _ => false,
                        }
                    }
                    None => {
                        // A scratch buffer needs a path before it can resume.
                        self.pending = continuation;
                        self.open_picker(PickerMode::Save);
                        false
                    }
                }
            }
            ConfirmChoice::Discard => {
                let continuation = self.pending.take();
                self.popup_state.close();

                match continuation {
                    Some(continuation) => self.resume(continuation),
                    None => false,
                }
            }
            ConfirmChoice::Overwrite => {
                let path = self.overwrite.take();
                self.popup_state.close();

                let written = path.as_deref().is_some_and(|path| self.write_buffer(path));
                // The save-as picker that led here gets the directory too,
                // whether or not the write went through: the user was there.
                if let Some(dir) = path.as_deref().and_then(Path::parent) {
                    let dir = dir.to_path_buf();
                    self.storage
                        .edit_state(|state| state.set_picker_dir(PickerKind::SaveAs, dir));
                }

                match (written, self.pending.take()) {
                    (true, Some(continuation)) => self.resume(continuation),
                    _ => false,
                }
            }
        }
    }

    /// Carries out `action` once the reason a confirm was shown is gone.
    ///
    /// Deliberately bypasses the dirty check: the user just saved or discarded
    /// the changes, so asking again would loop.
    pub(super) fn resume(&mut self, action: Action) -> bool {
        match action {
            Action::Quit => true,
            Action::LoadFile(path) => {
                self.load_file_now(path);
                false
            }
            Action::CloseTab => {
                self.close_active_tab();
                false
            }
            other => self.apply(other),
        }
    }
}

/// Which remembered start directory a picker mode uses.
///
/// Kept here rather than on [`PickerMode`] so `storage` stays independent of
/// the widgets: the shell is the only thing that knows about both.
fn picker_kind(mode: PickerMode) -> PickerKind {
    match mode {
        PickerMode::File => PickerKind::OpenFile,
        PickerMode::Folder => PickerKind::OpenFolder,
        PickerMode::Save => PickerKind::SaveAs,
    }
}

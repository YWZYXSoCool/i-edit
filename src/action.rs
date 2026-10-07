use std::path::PathBuf;

use crate::widgets::popup::PopupKind;

/// Requests a component makes of the app shell.
///
/// Anything that reaches past a component's own state goes through here: the
/// popup cannot decide to quit the editor on its own, it can only ask. That is
/// why the file variants carry a [`PathBuf`] — the picker names a file, the
/// shell decides whether opening it is safe yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Terminate the editor.
    Quit,
    /// Show a popup.
    OpenPopup(PopupKind),
    /// Dismiss whatever popup is open.
    ClosePopup,
    /// Open `kind`, or close anything already open.
    TogglePopup(PopupKind),
    /// Load `path` into the editor, replacing the current buffer.
    LoadFile(PathBuf),
    /// Open `path` as the root of the file tree.
    LoadFolder(PathBuf),
    /// Close the folder in the file tree, and the tabs that came from it.
    CloseFolder,
    /// Close the active tab.
    CloseTab,
    /// Write the buffer back to its path, or ask for one when it has none.
    Save,
    /// Open the save-as picker.
    SaveAs,
    /// Write the buffer to a confirmed picker target.
    SaveTo(PathBuf),
    /// Open the settings file in the editor, so the user can edit it by hand.
    OpenSettings,
    /// Show or hide the file tree panel.
    ToggleFileTree,
    /// Clear every notification message.
    ClearMessages,
    /// Throw the language server away and start a new one.
    RestartLsp,
    /// Answer to the confirm popup.
    ConfirmChoice(ConfirmChoice),
    NextTab,
    PrevTab,
}

/// What the user picked in the confirm popup.
///
/// The action the choice resumes stays in the shell; the popup only reports
/// which key was pressed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmChoice {
    /// Save the buffer, then continue with the pending action.
    Save,
    /// Drop unsaved changes and continue with the pending action.
    Discard,
    /// Overwrite the existing save-as target.
    Overwrite,
}

/// Outbox a component fills while handling an event.
///
/// [`crate::component::Component::handle_event`] returns nothing, so this is the
/// way back up to the shell. Every state that needs to ask for something owns
/// one; [`super::App`] drains all of them after each event.
#[derive(Debug, Default)]
pub struct Actions(Vec<Action>);

impl Actions {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn emit(&mut self, action: Action) {
        self.0.push(action);
    }

    #[inline]
    pub const fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Takes everything queued, oldest first, leaving the outbox empty.
    pub fn drain(&mut self) -> Vec<Action> {
        core::mem::take(&mut self.0)
    }

    /// Moves everything queued onto the end of `out`, oldest first, leaving the
    /// outbox empty but keeping its capacity.
    pub(crate) fn take_into(&mut self, out: &mut Vec<Action>) {
        out.append(&mut self.0)
    }
}

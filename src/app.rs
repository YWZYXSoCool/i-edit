//! The application shell.
//!
//! [`App`] owns the component state, decides the layout and routes events —
//! that is all. Every actual behaviour lives in the corresponding
//! [`Component`](crate::component::Component): [`Editor`] owns the buffer and
//! editing commands, [`Popup`] owns the overlay, [`StatusBar`] owns the
//! read-outs along the bottom, [`MessageBox`] owns the notification stack,
//! [`FileTree`] owns the folder view.
//!
//! The shell is also the only owner of [`Storage`]: components queue what they
//! cannot do themselves, and it is the shell that decides what of that is worth
//! remembering on disk.
//!
//! Because the shell is the only place that knows about *both* storage and the
//! widgets, the parts that need both live in their own files:
//!
//! | File        | What it decides                                        |
//! | ----------- | ------------------------------------------------------ |
//! | `session`   | what the last run was looking at, and where to pick up  |
//! | `files`     | opening, saving, overwriting and the confirms around it |
//! | `actions`   | how a queued [`Action`] is dispatched, and resumed      |
//! | `render`    | the layout                                             |

mod actions;
mod files;
mod render;
mod session;

#[cfg(test)]
mod tests;

use std::path::PathBuf;

use crate::Result;
use crate::action::Action;
use crate::component::Component;
use crate::shortcuts;
use crate::storage::Storage;
use crate::utils;
use crate::widgets::picker::PickerMode;
use crate::widgets::popup::PopupKind;
use crate::widgets::{
    Editor, EditorState, FileTree, FileTreeState, MessageBoxState, Popup, PopupState, StatusBar,
    StatusBarState,
};

use crossterm::event;
use log::{debug, info};
use ratatui::DefaultTerminal;

/// Where keyboard input goes while no popup is up.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Focus {
    #[default]
    Editor,
    FileTree,
}

pub struct App {
    storage: Storage,
    editor_state: EditorState,
    popup_state: PopupState,
    status_bar_state: StatusBarState,
    message_box_state: MessageBoxState,
    file_tree_state: FileTreeState,
    focus: Focus,
    file_tree_visible: bool,
    pending: Option<Action>,
    overwrite: Option<PathBuf>,
    /// Reused by [`Self::apply_actions`] so a steady stream of actions stops
    /// reallocating the queue on every event.
    actions_buf: Vec<Action>,
    /// Reused by [`Self::apply_expansion_changes`], same reason.
    expansion_buf: Vec<(PathBuf, bool)>,
    /// Whether a redraw is pending. Lets the loop draw only when something
    /// changed (an event, or data arriving on the async intake) instead of
    /// blocking on `event::read`, which would starve asynchronous producers.
    needs_redraw: bool,
}

impl App {
    /// An editor that remembers nothing: the storage has no root, so the
    /// session is kept in memory and never written. Tests use this.
    fn in_memory() -> Self {
        Self::new(Storage::default())
    }

    /// How long the loop waits for an event before yielding back to the top,
    /// giving the async intake a chance to deliver data. 50 ms ≈ 20 idle
    /// iterations per second, and a no-op draw is skipped entirely.
    const POLL: std::time::Duration = std::time::Duration::from_millis(50);

    /// Pulls pending data from asynchronous sources (a future LSP client, file
    /// watchers). Today it is a placeholder: the position is taken so the render
    /// loop already has an intake seam, and any data that arrives there will
    /// mark `needs_redraw`.
    fn drain_incoming(&mut self) {
        // Intentionally empty: see `Self::POLL` and the module docs.
    }

    /// An editor whose session comes from `storage` and is written back to it.
    pub fn new(storage: Storage) -> Self {
        Self {
            storage,
            editor_state: EditorState::new(),
            popup_state: PopupState::new(),
            status_bar_state: StatusBarState::new(),
            message_box_state: MessageBoxState::new(),
            file_tree_state: FileTreeState::new(),
            focus: Focus::Editor,
            file_tree_visible: true,
            pending: None,
            overwrite: None,
            actions_buf: Vec::new(),
            expansion_buf: Vec::new(),
            needs_redraw: true,
        }
    }
}

impl Default for App {
    fn default() -> Self {
        Self::in_memory()
    }
}

impl App {
    pub fn run(&mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        info!("i-edit started");

        self.restore_session();

        loop {
            // Async intake first: if a producer dropped new colors (or any other
            // data) this frame, redraw to show them.
            self.drain_incoming();

            if self.needs_redraw {
                terminal.draw(|frame| self.render(frame))?;
                self.needs_redraw = false;
            }

            // Non-blocking wait: a timeout returns control so the intake above
            // gets another look. No event means no redraw, so idle is cheap.
            if !event::poll(Self::POLL)? {
                continue;
            }

            let event = event::read()?;

            // Bracketed paste from the terminal (Ctrl+V / right-click in most
            // terminals) arrives as `Event::Paste`; `key_press` returns `None`
            // for it, so it would otherwise be dropped. Route it straight to the
            // editor when it is the focused component and no popup is open.
            if let event::Event::Paste(_) = &event {
                if self.focus == Focus::Editor && self.popup_state.kind.is_none() {
                    Component::handle_event(Editor, &event, &mut self.editor_state);
                    self.needs_redraw = true;
                }
                continue;
            }

            let Some(key) = utils::key_press(&event) else {
                continue;
            };

            Component::handle_event(
                StatusBar::new(&self.editor_state.text.lines),
                &event,
                &mut self.status_bar_state,
            );

            let mut quit = false;

            if self.popup_state.kind.is_none() {
                if let Some(action) = shortcuts::lookup(key) {
                    quit = self.apply(action);
                } else if key.code == shortcuts::QUIT_KEY {
                    // Esc priority: close popup (handled above) > leave the
                    // tree > quit the editor (with its own dirty check).
                    if self.focus == Focus::FileTree {
                        self.set_focus(Focus::Editor);
                    } else {
                        quit = self.apply(Action::Quit);
                    }
                } else if shortcuts::is_focus_key(key) && self.file_tree_visible {
                    let focus = match self.focus {
                        Focus::Editor => Focus::FileTree,
                        Focus::FileTree => Focus::Editor,
                    };
                    self.set_focus(focus);
                } else if self.focus == Focus::FileTree {
                    Component::handle_event(FileTree, &event, &mut self.file_tree_state);
                } else {
                    Component::handle_event(Editor, &event, &mut self.editor_state);
                }
            } else {
                Component::handle_event(Popup, &event, &mut self.popup_state);
            }

            // Components cannot touch anything outside their own state, so they
            // queue requests instead. The shell is the only one applying them.
            if quit || self.apply_actions() {
                // Last chance to record where the buffer was left: writing
                // only on a switch would lose the position of the file being
                // quit from.
                self.remember_current_view();
                break Ok(());
            }

            self.apply_expansion_changes();
            self.storage.tick();
            self.needs_redraw = true;
        }
    }

    /// Moves the keyboard focus.
    ///
    /// The tree keeps its selection index when the focus leaves, but only
    /// draws the highlight while it is the one receiving keys.
    pub(super) fn set_focus(&mut self, focus: Focus) {
        self.file_tree_state.set_focused(focus == Focus::FileTree);
        self.focus = focus;
    }

    /// Writes the session back to disk. Call this after the editor has run:
    /// a failed save is worth reporting, but never worth blocking exit over.
    pub fn persist(&mut self) -> Result<()> {
        self.storage.flush()
    }

    /// Drains every component outbox and applies what came out.
    ///
    /// Returns `true` once quitting has been requested.
    pub(super) fn apply_actions(&mut self) -> bool {
        self.actions_buf.clear();
        self.editor_state.take_actions_into(&mut self.actions_buf);
        self.popup_state.take_actions_into(&mut self.actions_buf);
        self.file_tree_state
            .take_actions_into(&mut self.actions_buf);

        // `pop` takes from the front of the reversed queue, so actions still
        // run in the order they were queued.
        self.actions_buf.reverse();
        while let Some(action) = self.actions_buf.pop() {
            if self.apply(action) {
                return true;
            }
        }

        false
    }

    /// Runs a single action. Returns `true` for quitting the editor.
    pub(super) fn apply(&mut self, action: Action) -> bool {
        debug!("action: {:?}", action);

        match action {
            Action::Quit => self.request_quit(),
            Action::OpenPopup(kind) => {
                self.open_popup(kind);
                false
            }
            Action::ClosePopup => {
                self.cancel_popup();
                false
            }
            Action::TogglePopup(kind) => {
                match self.popup_state.kind.toggle(kind) {
                    PopupKind::None => self.cancel_popup(),
                    opened => self.open_popup(opened),
                }
                false
            }
            Action::LoadFile(path) => {
                self.request_load_file(path);
                false
            }
            Action::LoadFolder(path) => {
                self.open_folder(path);
                false
            }
            Action::Save => {
                self.save_current();
                false
            }
            Action::SaveAs => {
                self.open_picker(PickerMode::Save);
                false
            }
            Action::SaveTo(path) => self.save_to(path),
            Action::ToggleFileTree => {
                self.toggle_file_tree();
                false
            }
            Action::ClearMessages => {
                self.message_box_state.clear();
                false
            }
            Action::ConfirmChoice(choice) => self.resolve_confirm(choice),
        }
    }

    /// Integration-test entry point for a single action; not part of the public API.
    #[doc(hidden)]
    pub fn apply_for_test(&mut self, action: Action) -> bool {
        self.apply(action)
    }
}

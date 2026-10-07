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
//! | `settings`  | the settings file: opening it, and applying what it says |
//! | `actions`   | how a queued [`Action`] is dispatched, and resumed      |
//! | `tabs`      | which buffers are open, and which one is current        |
//! | `render`    | the layout                                             |

mod actions;
mod files;
mod render;
mod session;
mod settings;
mod tabs;

#[cfg(test)]
mod tests;

use tabs::Tabs;

use std::path::PathBuf;

use crate::Result;
use crate::action::Action;
use crate::component::Component;
use crate::lsp::{LspClient, LspEvent};
use crate::shortcuts;
use crate::storage::Storage;
use crate::utils;
use crate::widgets::picker::PickerMode;
use crate::widgets::popup::{ConfirmKind, PopupKind};
use crate::widgets::{
    Editor, FileTree, FileTreeState, MessageBoxState, Popup, PopupState, StatusBar, StatusBarState,
};

use crossterm::event::{self};
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
    tabs: Tabs,
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
    /// The language server, if one is running. Its only appearance in the shell
    /// is here: everything it does is pulled by [`Self::drain_incoming`], so
    /// the server can be slow, absent or dead without the loop knowing.
    lsp: LspClient,
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

    /// Pulls pending data from asynchronous sources — today, the language
    /// server.
    ///
    /// Called at the top of every loop iteration and never blocking: whatever
    /// has arrived is applied, whatever has not is left for the next frame. The
    /// version is read once so a whole batch is judged against the same buffer.
    fn drain_incoming(&mut self) {
        let version = self.tabs.active().version;

        while let Some(event) = self.lsp.tick(&self.tabs.active().text.lines, version) {
            self.apply_lsp_event(event);
        }
    }

    /// Applies one language-server event.
    fn apply_lsp_event(&mut self, event: LspEvent) {
        match event {
            LspEvent::Tokens { version } => {
                // Offsets describe the text as it was when the request went
                // out. Against a buffer that has moved on they would slice the
                // wrong characters, so the batch is dropped instead of guessed
                // at — the next sync asks again.
                if version == self.tabs.active().version {
                    let rows = self.lsp.rows();
                    self.tabs.active_mut().set_semantic_tokens(rows);
                }
                self.needs_redraw = true;
            }
            LspEvent::Diagnostics => {
                self.tabs
                    .active_mut()
                    .set_diagnostics(self.lsp.diagnostic_rows());
                self.needs_redraw = true;
            }
            LspEvent::Progress => {
                // Copied out of the client rather than borrowed: the status bar
                // keeps its own text, so the read-out survives until the next
                // progress notification — or the `end` that clears it.
                let text = self.lsp.progress_text();
                self.status_bar_state.set_progress(text.as_deref());
                self.needs_redraw = true;
            }
            LspEvent::Notice(text) => {
                // Worth knowing, costs nothing to ignore: the coloring layer is
                // left exactly as it is.
                self.message_box_state.info(text);
                self.needs_redraw = true;
            }
            LspEvent::Stopped(reason) => {
                // The server is decoration: losing it costs colors and nothing
                // else, and it is reported once rather than per keystroke.
                self.tabs.active_mut().clear_semantic_tokens();
                self.tabs.active_mut().clear_diagnostics();
                self.status_bar_state.set_progress(None);
                self.message_box_state.warning(reason);
                self.needs_redraw = true;
            }
        }
    }

    /// An editor whose session comes from `storage` and is written back to it.
    pub fn new(storage: Storage) -> Self {
        let mut app = Self {
            storage,
            tabs: Tabs::new(),
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
            lsp: LspClient::new(),
        };

        // The settings are on disk before the first frame; everything that
        // reads them starts off with the stored value rather than a default.
        app.apply_settings();
        app
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
                    Component::handle_event(Editor, &event, self.tabs.active_mut());
                    self.needs_redraw = true;
                }
                continue;
            }

            let Some(key) = utils::key_press(&event) else {
                continue;
            };

            Component::handle_event(
                StatusBar::new(&self.tabs.active().text.lines),
                &event,
                &mut self.status_bar_state,
            );

            let mut quit = false;

            if self.popup_state.kind.is_none() {
                if let Some(action) = shortcuts::lookup(key) {
                    quit = self.apply(action);
                } else if key.code == shortcuts::QUIT_KEY {
                    // Esc priority: close popup (handled above) > leave the
                    // tree or tab bar > quit the editor (with its own dirty check).
                    match self.focus {
                        Focus::FileTree => self.set_focus(Focus::Editor),
                        Focus::Editor => quit = self.apply(Action::Quit),
                    }
                } else if shortcuts::is_focus_key(key) {
                    self.cycle_focus();
                } else if self.focus == Focus::FileTree {
                    Component::handle_event(FileTree, &event, &mut self.file_tree_state);
                } else {
                    Component::handle_event(Editor, &event, self.tabs.active_mut());
                }
            } else {
                Component::handle_event(Popup, &event, &mut self.popup_state);
            }

            // Components cannot touch anything outside their own state, so they
            // queue requests instead. The shell is the only one applying them.
            if quit || self.apply_actions() {
                // Stopped here rather than in `Drop`: this is the one place
                // that knows the loop is about to end, so the server's threads
                // are joined while the terminal still belongs to us.
                self.lsp.shutdown();
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

    /// Step the focus on by one panel: editor -> file tree -> editor.
    ///
    /// The file tree only joins the rotation while it is visible, because
    /// focusing a panel nobody can see would swallow keystrokes with nothing
    /// on screen to explain it; with the panel hidden the key is a no-op that
    /// leaves the editor in charge.
    pub(super) fn cycle_focus(&mut self) {
        let next = match self.focus {
            Focus::Editor if self.file_tree_visible => Focus::FileTree,
            // Nothing to step onto with the panel hidden, and a focus that is
            // already on a hidden tree has nowhere to go but back.
            _ => Focus::Editor,
        };
        self.set_focus(next);
    }

    /// Handles a key pressed while the tab bar has the focus.
    ///
    /// The arrows (and their vim spelling) step through the tabs and the
    /// editor follows along immediately, so browsing to the right file is a
    /// matter of holding one key; Enter hands the keys back to the buffer.
    /// Makes tab `i` active, leaving the focus wherever it is.
    /// Moves the active tab `delta` places, wrapping around the ends.
    /// Points the editor at tab `i`.
    ///
    /// The outgoing buffer records where it was left first, so stepping past a
    /// file and stepping back returns to the same line.
    fn switch_to_tab(&mut self, i: usize) {
        self.remember_current_view();
        self.tabs.set_active(i);
        self.after_tab_change();
    }

    /// Closes the active tab, asking first when it holds unsaved changes.
    ///
    /// Every tab can be closed, the last one included: what is left behind is
    /// a fresh scratch buffer, so the editor never has nothing to edit.
    ///
    /// A dirty *scratch* buffer is dropped silently — see
    /// [`Self::request_quit`] — because there is no path to save it to.
    pub(super) fn request_close_tab(&mut self) {
        let needs_confirm = self.tabs.active().dirty && self.tabs.active().path.is_some();
        if needs_confirm {
            self.pending = Some(Action::CloseTab);
            self.popup_state.open_confirm(ConfirmKind::UnsavedChanges);
            return;
        }
        self.close_active_tab();
    }

    /// Removes the active tab and follows whatever becomes active instead —
    /// a scratch buffer when the tab that went was the last one.
    pub(super) fn close_active_tab(&mut self) {
        let closed = self.tabs.active().path.clone();
        self.tabs.close_active();

        // The buffer is gone, so the document it was showing is gone with it:
        // the server is told to drop it instead of holding one no switch can
        // ever ask for again.
        if let Some(path) = closed {
            self.lsp.forget(&path);
        }

        // The tree may have been focused when the shortcut was pressed, but
        // the panel that asked for this is gone: keys belong to the editor.
        self.set_focus(Focus::Editor);
        self.after_tab_change();
    }

    /// Everything that follows the active buffer becoming another one.
    ///
    /// The language server is told which file is on screen — it holds every
    /// document it was given, so this is a lookup for a tab that has been
    /// looked at before, not a reopen; the session is told too, so a restart
    /// opens the file that was last looked at rather than the one that was
    /// last opened.
    fn after_tab_change(&mut self) {
        let current = self.tabs.active().path.clone();

        // The settings file is not a document being worked on, so it must not
        // become the file the next run comes back to.
        if !current
            .as_deref()
            .is_some_and(|path| self.storage.is_settings_file(path))
        {
            self.storage
                .edit_session(|session| session.last_file = current);
        }
        self.remember_tabs();
        self.sync_lsp();
        self.needs_redraw = true;
    }

    /// Records which buffers are open, and in what order.
    ///
    /// The tab bar is the one piece of session state the editor cannot
    /// reconstruct on its own — a file that is open is only known here — so
    /// it is written whenever the set changes, which is also what lets a
    /// crash mid-run still leave the last known bar behind. Scratch buffers
    /// have no path and are left out: they cannot be read back.
    fn remember_tabs(&mut self) {
        let open: Vec<PathBuf> = self
            .tabs
            .buffers()
            .iter()
            .filter_map(|buffer| buffer.path.clone())
            .collect();

        self.storage.edit_session(|session| session.set_tabs(open));
    }

    /// Writes the session back to disk, recording where the run ended first.
    ///
    /// The cursor and the tab bar are captured here as well as on the way:
    /// the file being quit from is the one whose position no switch ever
    /// recorded, and a tab opened as the last act of the run is the one no
    /// later change would have saved. Call this after the editor has run: a
    /// failed save is worth reporting, but never worth blocking exit over.
    pub fn persist(&mut self) -> Result<()> {
        self.remember_current_view();
        self.remember_tabs();
        self.storage.flush()
    }

    /// Drains every component outbox and applies what came out.
    ///
    /// Returns `true` once quitting has been requested.
    pub(super) fn apply_actions(&mut self) -> bool {
        self.actions_buf.clear();
        self.tabs
            .active_mut()
            .take_actions_into(&mut self.actions_buf);
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
            Action::CloseFolder => {
                self.close_folder();
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
            Action::OpenSettings => {
                self.open_settings();
                false
            }
            Action::ToggleFileTree => {
                self.toggle_file_tree();
                false
            }
            Action::ClearMessages => {
                self.message_box_state.clear();
                false
            }
            Action::RestartLsp => {
                self.restart_lsp();
                false
            }
            Action::CloseTab => {
                self.request_close_tab();
                false
            }
            Action::ConfirmChoice(choice) => self.resolve_confirm(choice),

            Action::NextTab => {
                self.next_tab();
                false
            }
            Action::PrevTab => {
                self.prev_tab();
                false
            }
        }
    }

    fn next_tab(&mut self) {
        self.step_tab(1);
    }

    fn prev_tab(&mut self) {
        self.step_tab(-1);
    }

    /// Steps the active tab `delta` places, wrapping around the ends.
    ///
    /// Goes through the same bookkeeping as [`Self::switch_to_tab`], which
    /// stepping used to skip: without it the buffer being left never records
    /// its cursor, the session is never told which tab is current, and the
    /// language server goes on coloring the file that was just left.
    fn step_tab(&mut self, delta: isize) {
        self.remember_current_view();
        self.tabs.move_active_by(delta);
        self.after_tab_change();
    }

    /// Integration-test entry point for a single action; not part of the public API.
    #[doc(hidden)]
    pub fn apply_for_test(&mut self, action: Action) -> bool {
        self.apply(action)
    }
}

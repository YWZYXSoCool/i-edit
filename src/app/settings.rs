//! The settings file: opening it for editing, and applying what it says.
//!
//! Settings are not edited through a form. The `settings` command loads the
//! file storage writes into a tab and the user edits it as text; saving it is
//! what applies it, because the write is what tells the shell to reread. That
//! keeps one source of truth on disk and costs nothing in the way of UI.
//!
//! Anything a setting controls therefore has to be pushed into the running
//! editor by [`App::apply_settings`]: the widgets hold a copy, since a
//! [`Component`](crate::component::Component) only ever sees its own state and
//! cannot reach storage while handling a key.

use crate::app::App;

impl App {
    /// Opens the settings file in a tab.
    ///
    /// Goes through the same dirty check as any other open: editing settings
    /// with unsaved changes in the buffer is not special enough to lose work
    /// over. A first run writes the file out with the defaults first, so there
    /// is always something to read.
    pub(super) fn open_settings(&mut self) {
        match self.storage.ensure_settings_file() {
            Some(path) => self.request_load_file(path),
            None => self
                .message_box_state
                .error("settings: no storage root to write them to"),
        }
    }

    /// Pushes the current settings into everything that reads them.
    ///
    /// Called on startup and after the file is saved, and silent in both cases:
    /// it is a copy of values the user already chose, not an event worth
    /// reporting.
    pub(super) fn apply_settings(&mut self) {
        let tab_indent = self.storage.config().tab_indent;
        self.tabs.set_tab_indent(tab_indent);
    }

    /// Rereads the file after it was written and applies the new settings.
    pub(super) fn reload_settings(&mut self) {
        self.storage.reload_config();
        self.apply_settings();

        let tab_indent = self.storage.config().tab_indent;
        self.message_box_state
            .success(format!("settings applied: Tab inserts {tab_indent}"));
    }
}

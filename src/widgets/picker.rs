//! File picker overlay: `open file` / `open folder` / `save as`.
//!
//! All three modes share one component: the list and the browsing keys are the
//! same everywhere, and only the overlay title, the footer hint and what a
//! confirmed row means change with the mode.
//!
//! The path input *is* the browsing state: its text names the directory being
//! listed. That is what makes "type a path to jump anywhere" free — every text
//! edit re-lists the directory, and a path that does not exist simply lists
//! nothing instead of raising an error.
//!
//! The picker never dismisses itself and never answers `Esc`: the popup host
//! owns dismissal. Choosing a row emits an [`Action`] and leaves the decision
//! to open, load or overwrite to the shell.
//!
//! The state and its key handling live here; everything that writes cells lives
//! in [`render`].

mod render;

#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};

use crate::action::{Action, Actions};
use crate::fs::{DirEntry, expand_path, list_dir};
use crate::widgets::InputState;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use render::with_separator;

/// Which overlay the picker is drawn as.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PickerMode {
    #[default]
    File,
    Folder,
    Save,
}

impl PickerMode {
    /// Title of the overlay; the mode is the only thing it depends on.
    pub(crate) const fn title(self) -> &'static str {
        match self {
            Self::File => "Open File",
            Self::Folder => "Open Folder",
            Self::Save => "Save As",
        }
    }

    pub(crate) const fn footer(self) -> &'static str {
        match self {
            Self::File => "Tab: complete   Enter: open   Esc: cancel",
            Self::Folder => {
                "Tab: complete   Enter: choose   Shift+Enter: this folder   Esc: cancel"
            }
            Self::Save => "Tab: switch   Enter: save   Ctrl+S: save   Esc: cancel",
        }
    }
}

/// Which save-mode field receives plain text keys.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum SaveFocus {
    #[default]
    Name,
    Path,
}

/// One line of the picker list.
#[derive(Debug, Clone)]
enum Row {
    /// The `..` row: walks to the parent directory.
    Parent,
    /// A real entry from [`list_dir`].
    Entry(DirEntry),
}

impl Row {
    /// Dotfiles are listed normally but drawn dim; they stay selectable.
    fn is_hidden(&self) -> bool {
        matches!(self, Row::Entry(entry) if entry.is_hidden)
    }
}

/// Everything the picker owns: the two inputs, the listed rows and the
/// requests on their way to the shell.
///
/// `Default` is an empty picker with no I/O, because `PopupState` derives
/// `Default` and must be able to sit unused until the shell opens it.
#[derive(Debug, Default)]
pub struct PickerState {
    mode: PickerMode,
    path_input: InputState,
    name_input: InputState,
    rows: Vec<Row>,
    highlighted: usize,
    scroll: usize,
    focus: SaveFocus,
    actions: Actions,
}

impl PickerState {
    /// `start_dir` is the initial directory (the shell picks it), `preset` is
    /// the current file: highlighted in `File` mode, used as the initial file
    /// name in `Save` mode.
    pub fn new(mode: PickerMode, start_dir: PathBuf, preset: Option<PathBuf>) -> Self {
        let mut state = Self {
            mode,
            ..Self::default()
        };

        state.path_input.set_text(start_dir.to_string_lossy());
        state.refresh();

        match mode {
            PickerMode::File => {
                state.path_input.is_editing = true;
                if let Some(preset) = preset {
                    state.highlight_preset(&preset);
                }
            }
            PickerMode::Folder => {
                state.path_input.is_editing = true;
            }
            PickerMode::Save => {
                // The current file is the natural save target, so its name is
                // what the name field starts with.
                if let Some(name) = preset.as_deref().and_then(Path::file_name) {
                    state.name_input.set_text(name.to_string_lossy());
                }
                state.set_save_focus(SaveFocus::Name);
            }
        }

        state
    }

    pub fn mode(&self) -> PickerMode {
        self.mode
    }

    /// The directory the picker is browsing: what the path field holds.
    ///
    /// Exposed for the shell's tests and for anything else that needs to know
    /// where the picker stands; the field itself stays private so the path can
    /// only be changed through the picker's own key handling.
    pub fn dir(&self) -> PathBuf {
        expand_path(self.path_input.text())
    }

    /// Takes everything this component has asked for since the last drain.
    pub fn take_actions(&mut self) -> Vec<Action> {
        self.actions.drain()
    }

    /// Moves everything this component has asked for onto the end of `out`.
    pub fn take_actions_into(&mut self, out: &mut Vec<Action>) {
        self.actions.take_into(out)
    }

    fn handle_key(&mut self, key: KeyEvent) {
        match self.mode {
            PickerMode::File | PickerMode::Folder => self.handle_browse_key(key),
            PickerMode::Save => self.handle_save_key(key),
        }
    }

    /// Keys shared by the file and folder modes: the list is the same, only
    /// the meaning of `Enter` on a directory differs.
    fn handle_browse_key(&mut self, key: KeyEvent) {
        // Checked before the plain-text branch below: `is_text_key` counts a
        // shifted key as text, so `Shift+Enter` would otherwise be handed to
        // the path input and swallowed there.
        if self.mode == PickerMode::Folder
            && key.code == KeyCode::Enter
            && key.modifiers.contains(KeyModifiers::SHIFT)
        {
            self.choose_current_dir();
            return;
        }

        match key.code {
            KeyCode::Up => self.select_previous(),
            KeyCode::Down => self.select_next(),
            KeyCode::Enter => self.activate_enter(),
            KeyCode::Right => self.enter_highlighted(),
            KeyCode::Left => self.go_parent(),
            KeyCode::Tab => self.complete_highlighted(),
            _ => {
                // Plain text keys edit the path; modified keys (Ctrl+..) are
                // not forwarded because `InputState` matches on `KeyCode`
                // alone and would insert them as characters.
                if is_text_key(&key) {
                    self.path_input.input(key);
                    self.refresh();
                }
            }
        }
    }

    fn handle_save_key(&mut self, key: KeyEvent) {
        if is_save_shortcut(&key) {
            self.confirm_save();
            return;
        }

        match key.code {
            KeyCode::Tab => self.toggle_save_focus(),
            _ => match self.focus {
                SaveFocus::Name => self.handle_name_key(key),
                SaveFocus::Path => self.handle_save_path_key(key),
            },
        }
    }

    fn handle_name_key(&mut self, key: KeyEvent) {
        if key.code == KeyCode::Enter {
            self.confirm_save();
        } else if is_text_key(&key) {
            self.name_input.input(key);
        }
    }

    /// Path/list focus in save mode: browsing behaves exactly like file mode,
    /// except that `Enter` on a file fills the name field instead of opening
    /// the file.
    fn handle_save_path_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Up => self.select_previous(),
            KeyCode::Down => self.select_next(),
            KeyCode::Enter => self.pick_for_save(),
            KeyCode::Right => self.enter_highlighted(),
            KeyCode::Left => self.go_parent(),
            _ => {
                if is_text_key(&key) {
                    self.path_input.input(key);
                    self.refresh();
                }
            }
        }
    }

    /// What `Enter` does to the highlighted row in file/folder mode.
    fn activate_enter(&mut self) {
        match self.highlighted_row() {
            Some(Row::Parent) => self.go_parent(),
            Some(Row::Entry(entry)) if entry.is_dir => {
                // The path is copied out before acting, so the row borrow ends
                // before the `&mut self` calls below.
                let path = entry.path.clone();
                match self.mode {
                    // Folder mode chooses the directory; file mode browses
                    // into it, because picking a folder for the *tree* is a
                    // different intent from navigating there.
                    PickerMode::Folder => self.actions.emit(Action::LoadFolder(path)),
                    _ => self.enter_dir(path),
                }
            }
            Some(Row::Entry(entry)) if self.mode == PickerMode::File => {
                let path = entry.path.clone();
                self.actions.emit(Action::LoadFile(path));
            }
            Some(Row::Entry(_)) => {}
            None => {}
        }
    }

    /// `Shift+Enter` in folder mode: choose the directory being listed.
    ///
    /// Enter always acts on the highlighted row, so picking the directory the
    /// picker is already in means walking up to its parent and choosing it
    /// from there. This key removes that round trip.
    ///
    /// The path is not checked here: a directory that cannot be opened is
    /// reported by the shell, which is the only place that knows what "opened"
    /// means.
    fn choose_current_dir(&mut self) {
        let dir = expand_path(self.path_input.text());
        self.actions.emit(Action::LoadFolder(dir));
    }

    /// What `Enter` does in save mode with the path/list focused.
    fn pick_for_save(&mut self) {
        match self.highlighted_row() {
            Some(Row::Parent) => self.go_parent(),
            Some(Row::Entry(entry)) if entry.is_dir => {
                let path = entry.path.clone();
                self.enter_dir(path);
            }
            Some(Row::Entry(entry)) => {
                let name = entry.name.clone();
                self.name_input.set_text(name);
                self.set_save_focus(SaveFocus::Name);
            }
            None => {}
        }
    }

    /// `Right`: enter the highlighted directory, or walk up for `..`.
    fn enter_highlighted(&mut self) {
        match self.highlighted_row() {
            Some(Row::Parent) => self.go_parent(),
            Some(Row::Entry(entry)) if entry.is_dir => {
                let path = entry.path.clone();
                self.enter_dir(path);
            }
            _ => {}
        }
    }

    /// `Left` and the `..` row: make the path input its own parent.
    fn go_parent(&mut self) {
        let current = expand_path(self.path_input.text());
        if let Some(parent) = current.parent() {
            // `Path::new("")` has no parent; an empty lexical parent would
            // blank the input, so it is not a move worth making.
            if !parent.as_os_str().is_empty() {
                self.enter_dir(parent.to_path_buf());
            }
        }
    }

    /// Makes `path` the listed directory: the input text, then the refresh.
    fn enter_dir(&mut self, path: PathBuf) {
        self.path_input.set_text(path.to_string_lossy());
        self.refresh();
    }

    /// `Tab`: copy the highlighted row into the input, ending a directory with
    /// a separator so the refreshed listing shows its contents right away.
    fn complete_highlighted(&mut self) {
        // The completed text is built while the rows are borrowed; the inputs
        // are only touched once that borrow is over.
        let text = match self.highlighted_row() {
            Some(Row::Parent) => {
                let current = expand_path(self.path_input.text());
                match current.parent() {
                    Some(parent) if !parent.as_os_str().is_empty() => with_separator(parent),
                    _ => return,
                }
            }
            Some(Row::Entry(entry)) if entry.is_dir => with_separator(&entry.path),
            Some(Row::Entry(entry)) => entry.path.to_string_lossy().into_owned(),
            None => return,
        };

        self.path_input.set_text(text);
        self.refresh();
    }

    fn select_previous(&mut self) {
        let len = self.rows.len();

        if len > 0 {
            self.highlighted = (self.highlighted + len - 1) % len;
        }
    }

    fn select_next(&mut self) {
        let len = self.rows.len();

        if len > 0 {
            self.highlighted = (self.highlighted + 1) % len;
        }
    }

    fn highlighted_row(&self) -> Option<&Row> {
        self.rows.get(self.highlighted)
    }

    /// `Ctrl+S` or `Enter` in the name field: hand the target to the shell.
    fn confirm_save(&mut self) {
        if self.name_input.text().trim().is_empty() {
            // An empty file name cannot be joined into a path, so confirming
            // it is a no-op rather than an error.
            return;
        }

        let dir = expand_path(self.path_input.text());
        self.actions
            .emit(Action::SaveTo(dir.join(self.name_input.text().trim())));
    }

    fn toggle_save_focus(&mut self) {
        let focus = match self.focus {
            SaveFocus::Name => SaveFocus::Path,
            SaveFocus::Path => SaveFocus::Name,
        };
        self.set_save_focus(focus);
    }

    /// The focused input is the yellow one with the cursor; this keeps that
    /// flag in one place so both inputs always agree with `focus`.
    fn set_save_focus(&mut self, focus: SaveFocus) {
        self.focus = focus;
        self.name_input.is_editing = focus == SaveFocus::Name;
        self.path_input.is_editing = focus == SaveFocus::Path;
    }

    /// Re-lists the directory the path input names.
    ///
    /// A missing or unreadable path is not an error here: showing an empty
    /// list lets the user keep editing the path, which is the whole point of
    /// the editable path box.
    fn refresh(&mut self) {
        let path = expand_path(self.path_input.text());

        // Reuse the row buffer: this runs on every keystroke in the path box.
        // A missing or unreadable directory stays an empty list, because
        // `list_dir` fails before anything is pushed.
        self.rows.clear();
        if let Ok(entries) = list_dir(&path) {
            if path.parent().is_some() {
                self.rows.push(Row::Parent);
            }
            self.rows.extend(entries.into_iter().map(Row::Entry));
        }

        // The old highlight pointed into the old listing; both reset to the
        // top so the picker never acts on a stale row.
        self.highlighted = 0;
        self.scroll = 0;
    }

    /// Puts the highlight on `preset` when the fresh listing contains it.
    fn highlight_preset(&mut self, preset: &Path) {
        if let Some(index) = self
            .rows
            .iter()
            .position(|row| matches!(row, Row::Entry(entry) if entry.path.as_path() == preset))
        {
            self.highlighted = index;
        }
    }

    fn has_entries(&self) -> bool {
        self.rows.iter().any(|row| matches!(row, Row::Entry(_)))
    }
}

/// The overlay: an opaque, bordered box with the path box on top, the listing
/// in the middle and the key hints at the bottom.
///
/// Drawing lives in [`render`]; the type is declared here so the module keeps
/// owning its public surface.
#[derive(Debug, Default, Clone, Copy)]
pub struct Picker;

/// `InputState` dispatches on `KeyCode` alone, so a modified key such as
/// `Ctrl+S` would be inserted as a plain `s`. Only unmodified keys and shifted
/// characters (which arrive as their final glyph) are safe to forward.
fn is_text_key(key: &KeyEvent) -> bool {
    key.modifiers == KeyModifiers::NONE || key.modifiers == KeyModifiers::SHIFT
}

fn is_save_shortcut(key: &KeyEvent) -> bool {
    key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('s' | 'S'))
}

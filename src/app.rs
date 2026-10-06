use std::env;
use std::path::{Path, PathBuf};

use crate::Result;
use crate::action::{Action, ConfirmChoice};
use crate::component::Component;
use crate::fs;
use crate::utils;
use crate::widgets::picker::PickerMode;
use crate::widgets::popup::{ConfirmKind, PopupKind};
use crate::widgets::{
    Editor, EditorState, FileTree, FileTreeState, MessageBox, MessageBoxState, Popup, PopupState,
    StatusBar, StatusBarState,
};

use crossterm::event::{self, KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Constraint, Layout};
use ratatui::{DefaultTerminal, Frame};

use log::info;

/// Key that quits the editor. Only reachable while no popup is open.
const QUIT_KEY: KeyCode = KeyCode::Esc;

/// Bounds of the file tree panel: a quarter of the window, kept readable.
const FILE_TREE_MIN_WIDTH: u16 = 20;
const FILE_TREE_MAX_WIDTH: u16 = 40;

/// Columns the editor never gives up to the tree.
const EDITOR_MIN_WIDTH: u16 = 10;

/// Where keyboard input goes while no popup is up.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum Focus {
    #[default]
    Editor,
    FileTree,
}

/// Application shell.
///
/// Holds component state, decides the layout and routes events — that is all.
/// Every actual behaviour lives in the corresponding [`Component`]:
/// [`Editor`] owns the buffer and editing commands, [`Popup`] owns the overlay,
/// [`StatusBar`] owns the read-outs along the bottom, [`MessageBox`] owns the
/// notification stack in the corner, [`FileTree`] owns the folder view.
#[derive(Debug)]
pub struct App {
    editor_state: EditorState,
    popup_state: PopupState,
    status_bar_state: StatusBarState,
    message_box_state: MessageBoxState,
    file_tree_state: FileTreeState,
    focus: Focus,
    file_tree_visible: bool,
    pending: Option<Action>,
    overwrite: Option<PathBuf>,
    last_dir: Option<PathBuf>,
    /// Reused by [`Self::apply_actions`] so a steady stream of actions stops
    /// reallocating the queue on every event.
    actions_buf: Vec<Action>,
}

impl App {
    fn new() -> Self {
        Self {
            editor_state: EditorState::new(),
            popup_state: PopupState::new(),
            status_bar_state: StatusBarState::new(),
            message_box_state: MessageBoxState::new(),
            file_tree_state: FileTreeState::new(),
            focus: Focus::Editor,
            file_tree_visible: true,
            pending: None,
            overwrite: None,
            last_dir: None,
            actions_buf: Vec::new(),
        }
    }
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

impl App {
    pub fn run(&mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        info!("i-edit started");

        loop {
            terminal.draw(|frame| self.render(frame))?;

            let event = event::read()?;

            let Some(key) = utils::key_press(&event) else {
                continue;
            };

            Component::handle_event(
                StatusBar::new(&self.editor_state.text.lines),
                &event,
                &mut self.status_bar_state,
            );

            let mut quit = false;

            if key.modifiers == (KeyModifiers::CONTROL | KeyModifiers::SHIFT)
                && key.code == KeyCode::Char('Q')
            {
                // Ctrl+Shift+Q toggles the command popup from anywhere.
                self.apply(Action::TogglePopup(PopupKind::Command));
            } else if self.popup_state.kind.is_none() {
                if let Some(action) = shortcut(key) {
                    quit = self.apply(action);
                } else if key.code == QUIT_KEY {
                    // Esc priority: close popup (handled above) > leave the
                    // tree > quit the editor (with its own dirty check).
                    if self.focus == Focus::FileTree {
                        self.focus = Focus::Editor;
                    } else {
                        quit = self.apply(Action::Quit);
                    }
                } else if key.code == KeyCode::Tab && self.file_tree_visible {
                    self.focus = match self.focus {
                        Focus::Editor => Focus::FileTree,
                        Focus::FileTree => Focus::Editor,
                    };
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
                break Ok(());
            }
        }
    }

    /// Drains every component outbox and applies what came out.
    ///
    /// Returns `true` once quitting has been requested.
    fn apply_actions(&mut self) -> bool {
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
    fn apply(&mut self, action: Action) -> bool {
        info!("action: {:?}", action);

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

    /// Opens a popup, giving the picker kinds their initial directory.
    fn open_popup(&mut self, kind: PopupKind) {
        match kind {
            PopupKind::OpenFile => self.open_picker(PickerMode::File),
            PopupKind::OpenFolder => self.open_picker(PickerMode::Folder),
            PopupKind::SaveAs => self.open_picker(PickerMode::Save),
            _ => self.popup_state.open(kind),
        }
    }

    fn open_picker(&mut self, mode: PickerMode) {
        let start_dir = self.picker_start_dir();
        let preset = self.editor_state.path.clone();
        self.popup_state.open_picker(mode, start_dir, preset);
    }

    /// Initial picker directory: the open folder wins, then the last one used,
    /// then the process working directory.
    fn picker_start_dir(&self) -> PathBuf {
        self.file_tree_state
            .root()
            .map(Path::to_path_buf)
            .or_else(|| self.last_dir.clone())
            .or_else(|| env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("."))
    }

    /// Dismisses the open popup and aborts whatever it was going to resume.
    fn cancel_popup(&mut self) {
        self.pending = None;
        self.overwrite = None;
        self.popup_state.close();
    }

    /// Quits, but asks first when a file-backed buffer holds unsaved changes.
    ///
    /// A dirty scratch buffer is a throwaway pad with no path to save it to,
    /// so quitting drops it silently instead of prompting.
    fn request_quit(&mut self) -> bool {
        if self.editor_state.dirty && self.editor_state.path.is_some() {
            self.pending = Some(Action::Quit);
            self.popup_state.open_confirm(ConfirmKind::UnsavedChanges);
            false
        } else {
            true
        }
    }

    /// Opens `path`, but asks first when the buffer holds unsaved changes.
    fn request_load_file(&mut self, path: PathBuf) {
        if self.editor_state.dirty {
            self.pending = Some(Action::LoadFile(path));
            self.popup_state.open_confirm(ConfirmKind::UnsavedChanges);
        } else {
            self.load_file_now(path);
        }
    }

    /// Reads `path` and replaces the buffer. The original buffer is only kept
    /// when the read fails.
    fn load_file_now(&mut self, path: PathBuf) {
        self.popup_state.close();

        match fs::read_text_file(&path) {
            Ok(lines) => {
                self.last_dir = path.parent().map(Path::to_path_buf);
                self.message_box_state
                    .success(format!("opened: {}", path.display()));
                self.editor_state.load_file(path, lines);
                self.focus = Focus::Editor;
            }
            Err(err) => {
                self.message_box_state.error(format!("cannot open: {err}"));
            }
        }
    }

    fn open_folder(&mut self, path: PathBuf) {
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
        self.last_dir = Some(path);
    }

    /// Saves to the buffer's own path, or asks for one when it has none.
    fn save_current(&mut self) {
        match self.editor_state.path.clone() {
            Some(path) => {
                self.write_buffer(&path);
            }
            None => self.open_picker(PickerMode::Save),
        }
    }

    /// Writes the buffer to `path`. Returns whether the write succeeded.
    fn write_buffer(&mut self, path: &Path) -> bool {
        match fs::write_text_file(path, &self.editor_state.text.lines) {
            Ok(()) => {
                self.editor_state.path = Some(path.to_path_buf());
                self.editor_state.mark_saved();
                self.last_dir = path.parent().map(Path::to_path_buf);
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
    fn save_to(&mut self, path: PathBuf) -> bool {
        self.popup_state.close();

        if path.exists() {
            self.overwrite = Some(path);
            self.popup_state.open_confirm(ConfirmKind::Overwrite);
            return false;
        }

        if self.write_buffer(&path) {
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

    fn resolve_confirm(&mut self, choice: ConfirmChoice) -> bool {
        match choice {
            ConfirmChoice::Save => {
                let continuation = self.pending.take();
                let path = self.editor_state.path.clone();
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

                let written = path.is_some_and(|path| self.write_buffer(&path));
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
    fn resume(&mut self, action: Action) -> bool {
        match action {
            Action::Quit => true,
            Action::LoadFile(path) => {
                self.load_file_now(path);
                false
            }
            other => self.apply(other),
        }
    }

    fn toggle_file_tree(&mut self) {
        self.file_tree_visible = !self.file_tree_visible;

        if !self.file_tree_visible {
            self.focus = Focus::Editor;
        }
    }

    fn render(&mut self, frame: &mut Frame) {
        let area = frame.area();

        let layout = Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]);
        let [main_area, status_bar_area] = layout.areas(area);

        if self.file_tree_visible {
            let tree_width = file_tree_width(area.width);
            let [editor_area, tree_area] =
                Layout::horizontal([Constraint::Fill(1), Constraint::Length(tree_width)])
                    .areas(main_area);

            Component::render(
                Editor,
                editor_area,
                frame.buffer_mut(),
                &mut self.editor_state,
            );
            Component::render(
                FileTree,
                tree_area,
                frame.buffer_mut(),
                &mut self.file_tree_state,
            );
        } else {
            Component::render(
                Editor,
                main_area,
                frame.buffer_mut(),
                &mut self.editor_state,
            );
        }

        self.status_bar_state.cursor = self.editor_state.text.cursor;

        let file_name = self
            .editor_state
            .path
            .as_deref()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str());

        Component::render(
            StatusBar::new(&self.editor_state.text.lines)
                .file_name(file_name)
                .dirty(self.editor_state.dirty),
            status_bar_area,
            frame.buffer_mut(),
            &mut self.status_bar_state,
        );

        // The cursor belongs to the editor only while typing goes there and no
        // popup covers it.
        if self.focus == Focus::Editor
            && self.popup_state.kind.is_none()
            && let Some(pos) = self.editor_state.cursor_screen_pos
        {
            frame.set_cursor_position(pos);
        }

        Component::render(Popup, area, frame.buffer_mut(), &mut self.popup_state);

        // Last, so notifications stay visible even over an open popup.
        Component::render(
            MessageBox,
            area,
            frame.buffer_mut(),
            &mut self.message_box_state,
        );
    }
}

/// Width of the tree panel: a quarter of the window, kept readable and never
/// allowed to squeeze the editor out.
fn file_tree_width(total_width: u16) -> u16 {
    let ideal = (total_width / 4).clamp(FILE_TREE_MIN_WIDTH, FILE_TREE_MAX_WIDTH);
    ideal.min(total_width.saturating_sub(EDITOR_MIN_WIDTH))
}

/// Single source of truth for the editor-wide key bindings.
///
/// Checked before any component sees the key: the editor inserts an arbitrary
/// `Char`, so Ctrl+S would otherwise type an `s`. Terminals disagree on how
/// they report shifted letters, so an uppercase letter without the `SHIFT`
/// flag (or a lowercase one with it) still counts as the shifted binding.
fn shortcut(key: KeyEvent) -> Option<Action> {
    use KeyCode::Char;

    if !key.modifiers.contains(KeyModifiers::CONTROL) {
        return None;
    }

    let shift = key.modifiers.contains(KeyModifiers::SHIFT);

    match key.code {
        Char('o') => Some(if shift {
            Action::OpenPopup(PopupKind::OpenFolder)
        } else {
            Action::OpenPopup(PopupKind::OpenFile)
        }),
        // A terminal that drops the SHIFT flag still sends the uppercase form.
        Char('O') => Some(Action::OpenPopup(PopupKind::OpenFolder)),
        Char('s') => Some(if shift { Action::SaveAs } else { Action::Save }),
        Char('S') => Some(Action::SaveAs),
        Char('b') | Char('B') => Some(Action::ToggleFileTree),
        Char('m') | Char('M') => Some(Action::ClearMessages),
        // Terminals that cannot tell Ctrl+M from Enter report it as the latter.
        KeyCode::Enter => Some(Action::ClearMessages),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{Action, App, shortcut};
    use crate::action::ConfirmChoice;
    use crate::component::Component;
    use crate::widgets::Popup;
    use crate::widgets::popup::PopupKind;

    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    use std::path::PathBuf;

    fn press(code: KeyCode) -> Event {
        Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    /// A unique, self-cleaning scratch directory for one test.
    fn scratch_dir(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("i-edit-app-test-{}-{name}", std::process::id()))
    }

    #[test]
    fn toggling_opens_then_closes_the_same_popup() {
        let mut app = App::default();

        assert!(!app.apply(Action::TogglePopup(PopupKind::Command)));
        assert_eq!(app.popup_state.kind, PopupKind::Command);

        assert!(!app.apply(Action::TogglePopup(PopupKind::Command)));
        assert!(app.popup_state.kind.is_none());
    }

    #[test]
    fn toggling_closes_whatever_popup_is_open() {
        let mut app = App::default();

        app.apply(Action::TogglePopup(PopupKind::Log));
        assert_eq!(app.popup_state.kind, PopupKind::Log);

        // Toggle means "close anything that is up", not "switch to this one".
        app.apply(Action::TogglePopup(PopupKind::Command));
        assert!(app.popup_state.kind.is_none());

        app.apply(Action::TogglePopup(PopupKind::Command));
        assert_eq!(app.popup_state.kind, PopupKind::Command);
    }

    #[test]
    fn quit_command_stops_the_app() {
        let mut app = App::default();
        app.apply(Action::TogglePopup(PopupKind::Command));

        for ch in "quit".chars() {
            Component::handle_event(Popup, &press(KeyCode::Char(ch)), &mut app.popup_state);
        }
        Component::handle_event(Popup, &press(KeyCode::Enter), &mut app.popup_state);

        assert!(app.apply_actions());
    }

    #[test]
    fn close_action_dismisses_the_popup() {
        let mut app = App::default();
        app.apply(Action::TogglePopup(PopupKind::Command));

        Component::handle_event(Popup, &press(KeyCode::Esc), &mut app.popup_state);

        assert!(!app.apply_actions());
        assert!(app.popup_state.kind.is_none());
    }

    #[test]
    fn actions_are_consumed_once() {
        let mut app = App::default();
        app.apply(Action::TogglePopup(PopupKind::Command));

        Component::handle_event(Popup, &press(KeyCode::Esc), &mut app.popup_state);

        assert!(!app.apply_actions());
        // Drained, so the second pass has nothing left to do.
        assert!(!app.apply_actions());
    }

    #[test]
    fn a_clean_buffer_quits_directly() {
        let mut app = App::default();

        assert!(app.apply(Action::Quit));
    }

    #[test]
    fn a_dirty_named_buffer_asks_before_quitting() {
        let dir = scratch_dir("quit-named");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("named.txt");
        std::fs::write(&path, "a\n").unwrap();

        let mut app = App::default();
        app.apply(Action::LoadFile(path.clone()));
        app.editor_state
            .handle_key(KeyCode::Char('x'), KeyModifiers::NONE);
        assert!(app.editor_state.dirty);

        assert!(!app.apply(Action::Quit));
        assert_eq!(app.popup_state.kind, PopupKind::Confirm);

        // Discarding the changes lets the quit through.
        assert!(app.apply(Action::ConfirmChoice(ConfirmChoice::Discard)));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_dirty_scratch_buffer_quits_without_asking() {
        let mut app = App::default();
        app.editor_state
            .handle_key(KeyCode::Char('x'), KeyModifiers::NONE);
        assert!(app.editor_state.dirty);
        assert!(app.editor_state.path.is_none());

        assert!(app.apply(Action::Quit));
        assert!(app.popup_state.kind.is_none());
    }

    #[test]
    fn toggling_the_file_tree_flips_visibility() {
        let mut app = App::default();
        assert!(app.file_tree_visible);

        app.apply(Action::ToggleFileTree);
        assert!(!app.file_tree_visible);

        app.apply(Action::ToggleFileTree);
        assert!(app.file_tree_visible);
    }

    #[test]
    fn loading_a_file_replaces_the_buffer() {
        let dir = scratch_dir("load");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("sample.txt");
        std::fs::write(&path, "hello\n").unwrap();

        let mut app = App::default();
        app.apply(Action::LoadFile(path.clone()));

        assert_eq!(app.editor_state.text.lines, vec!["hello"]);
        assert_eq!(app.editor_state.path.as_deref(), Some(path.as_path()));
        assert!(!app.editor_state.dirty);
        assert!(!app.message_box_state.is_empty());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_dirty_buffer_loads_after_discarding() {
        let dir = scratch_dir("discard");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("other.txt");
        std::fs::write(&path, "other\n").unwrap();

        let mut app = App::default();
        app.editor_state
            .handle_key(KeyCode::Char('x'), KeyModifiers::NONE);
        app.apply(Action::LoadFile(path.clone()));
        assert_eq!(app.popup_state.kind, PopupKind::Confirm);

        app.apply(Action::ConfirmChoice(ConfirmChoice::Discard));

        assert_eq!(app.editor_state.text.lines, vec!["other"]);
        assert_eq!(app.editor_state.path.as_deref(), Some(path.as_path()));
        assert!(!app.editor_state.dirty);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn save_writes_the_buffer_back() {
        let dir = scratch_dir("save");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("save.txt");
        std::fs::write(&path, "a\n").unwrap();

        let mut app = App::default();
        app.apply(Action::LoadFile(path.clone()));
        app.editor_state
            .handle_key(KeyCode::Char('b'), KeyModifiers::NONE);
        app.apply(Action::Save);

        assert!(!app.editor_state.dirty);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "ba\n");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn saving_without_a_path_opens_the_save_picker() {
        let mut app = App::default();
        app.editor_state
            .handle_key(KeyCode::Char('x'), KeyModifiers::NONE);

        app.apply(Action::Save);

        assert_eq!(app.popup_state.kind, PopupKind::SaveAs);
        assert_eq!(
            app.popup_state.picker.mode(),
            crate::widgets::picker::PickerMode::Save
        );
    }

    #[test]
    fn save_to_an_existing_file_asks_before_overwriting() {
        let dir = scratch_dir("overwrite");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("taken.txt");
        std::fs::write(&path, "old\n").unwrap();

        let mut app = App::default();
        app.editor_state
            .handle_key(KeyCode::Char('n'), KeyModifiers::NONE);

        assert!(!app.apply(Action::SaveTo(path.clone())));
        assert_eq!(app.popup_state.kind, PopupKind::Confirm);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "old\n");

        app.apply(Action::ConfirmChoice(ConfirmChoice::Overwrite));

        assert_eq!(std::fs::read_to_string(&path).unwrap(), "n\n");
        assert_eq!(app.editor_state.path.as_deref(), Some(path.as_path()));
        assert!(!app.editor_state.dirty);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn saving_a_scratch_buffer_resumes_a_pending_load() {
        let dir = scratch_dir("resume-load");
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("target.txt");
        std::fs::write(&target, "target\n").unwrap();
        let scratch = dir.join("scratch.txt");

        let mut app = App::default();
        app.editor_state
            .handle_key(KeyCode::Char('x'), KeyModifiers::NONE);

        // The dirty buffer turns the load into a confirm; saving the scratch
        // buffer must then carry the load out instead of dropping it.
        app.apply(Action::LoadFile(target.clone()));
        assert_eq!(app.popup_state.kind, PopupKind::Confirm);

        app.apply(Action::ConfirmChoice(ConfirmChoice::Save));
        assert_eq!(app.popup_state.kind, PopupKind::SaveAs);

        app.apply(Action::SaveTo(scratch.clone()));

        assert_eq!(std::fs::read_to_string(&scratch).unwrap(), "x\n");
        assert_eq!(app.editor_state.text.lines, vec!["target"]);
        assert_eq!(app.editor_state.path.as_deref(), Some(target.as_path()));
        assert!(!app.editor_state.dirty);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn opening_a_folder_roots_and_shows_the_tree() {
        let dir = scratch_dir("open-folder");
        std::fs::create_dir_all(&dir).unwrap();

        let mut app = App::default();
        app.apply(Action::ToggleFileTree);
        assert!(!app.file_tree_visible);

        app.apply(Action::LoadFolder(dir.clone()));

        assert!(app.file_tree_visible);
        assert_eq!(app.file_tree_state.root(), Some(dir.as_path()));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn ctrl_m_clears_the_message_box() {
        let mut app = App::default();
        app.message_box_state.success("opened: somewhere");
        assert!(!app.message_box_state.is_empty());

        for code in [KeyCode::Char('m'), KeyCode::Char('M'), KeyCode::Enter] {
            assert_eq!(
                shortcut(KeyEvent::new(code, KeyModifiers::CONTROL)),
                Some(Action::ClearMessages)
            );
        }

        app.apply(Action::ClearMessages);
        assert!(app.message_box_state.is_empty());
    }
}

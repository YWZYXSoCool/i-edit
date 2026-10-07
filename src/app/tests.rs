use super::{Action, App, Focus};

use crate::Cursor;
use crate::action::ConfirmChoice;
use crate::component::Component;
use crate::highlight::StyledRun;
use crate::lsp::LspEvent;
use crate::shortcuts;
use crate::storage::Storage;
use crate::storage::config::TabIndent;
use crate::widgets::Editor;
use crate::widgets::FileTree;
use crate::widgets::Popup;
use crate::widgets::popup::PopupKind;

use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use std::path::PathBuf;

fn press(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

/// A unique, self-cleaning scratch directory for one test.
fn scratch_dir(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("i-edit-app-test-{}-{name}", std::process::id()))
}

/// An app whose storage lives in `dir`, plus a file to open in it.
fn session_fixture(name: &str, contents: &str) -> (PathBuf, PathBuf, PathBuf) {
    let dir = scratch_dir(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let file = dir.join("notes.txt");
    std::fs::write(&file, contents).unwrap();

    (dir.clone(), dir.join("storage"), file)
}

#[test]
fn each_picker_starts_where_it_was_last_used() {
    let dir = scratch_dir("picker-dirs");
    let _ = std::fs::remove_dir_all(&dir);
    let project = dir.join("project");
    let src = project.join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("main.rs"), "fn main() {}\n").unwrap();
    let root = dir.join("storage");

    let mut app = App::new(Storage::open(root.clone()));
    app.apply(Action::LoadFolder(project.clone()));
    app.apply(Action::LoadFile(src.join("main.rs")));
    app.persist().unwrap();

    // A fresh editor, so both directories come from disk and not from the
    // session above.
    let mut reopened = App::new(Storage::open(root));

    reopened.apply(Action::OpenPopup(PopupKind::OpenFile));
    assert_eq!(reopened.popup_state.picker.dir(), src);
    reopened.apply(Action::ClosePopup);

    reopened.apply(Action::OpenPopup(PopupKind::OpenFolder));
    assert_eq!(reopened.popup_state.picker.dir(), project);

    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn using_one_picker_does_not_move_the_others() {
    let dir = scratch_dir("picker-separate");
    let _ = std::fs::remove_dir_all(&dir);
    let folder = dir.join("folder");
    let elsewhere = dir.join("elsewhere");
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::create_dir_all(&elsewhere).unwrap();
    std::fs::write(elsewhere.join("note.txt"), "x\n").unwrap();
    let root = dir.join("storage");

    let mut app = App::new(Storage::open(root));
    app.apply(Action::LoadFolder(folder.clone()));

    // Only the folder picker has been used, so opening a file starts there...
    app.apply(Action::OpenPopup(PopupKind::OpenFile));
    assert_eq!(app.popup_state.picker.dir(), folder);
    app.apply(Action::ClosePopup);

    // ...and only the file picker moves once a file has been opened.
    app.apply(Action::LoadFile(elsewhere.join("note.txt")));
    app.apply(Action::OpenPopup(PopupKind::OpenFile));
    assert_eq!(app.popup_state.picker.dir(), elsewhere);
    app.apply(Action::ClosePopup);

    app.apply(Action::OpenPopup(PopupKind::OpenFolder));
    assert_eq!(app.popup_state.picker.dir(), folder);

    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_remembered_directory_that_is_gone_is_not_offered() {
    let dir = scratch_dir("picker-stale");
    let _ = std::fs::remove_dir_all(&dir);
    let gone = dir.join("gone");
    std::fs::create_dir_all(&gone).unwrap();
    let file = gone.join("a.txt");
    std::fs::write(&file, "x\n").unwrap();
    let root = dir.join("storage");

    let mut app = App::new(Storage::open(root.clone()));
    app.apply(Action::LoadFile(file));
    app.persist().unwrap();
    std::fs::remove_dir_all(&gone).unwrap();

    let mut reopened = App::new(Storage::open(root));
    reopened.apply(Action::OpenPopup(PopupKind::OpenFile));

    // Falls through to the next candidate instead of opening the picker on a
    // listing that cannot be read.
    assert_ne!(reopened.popup_state.picker.dir(), gone);

    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn settings_opens_the_settings_file_in_a_tab() {
    let dir = scratch_dir("settings-open");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let root = dir.join("storage");

    let mut app = App::new(Storage::open(root.clone()));
    app.apply(Action::OpenSettings);

    // The command is what creates the file: a first run has none.
    let path = root.join("config");
    assert_eq!(app.tabs.active().path.as_deref(), Some(path.as_path()));
    assert!(path.exists());
    assert!(
        app.tabs
            .active()
            .text
            .lines
            .iter()
            .any(|line| line.contains("tab_indent"))
    );

    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn tab_inserts_what_the_settings_say() {
    let dir = scratch_dir("tab-indent");
    let _ = std::fs::remove_dir_all(&dir);
    let root = dir.join("storage");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("config"), "tab_indent = tab\n").unwrap();

    let mut app = App::new(Storage::open(root));

    // The setting is read at startup, before a key is ever pressed.
    assert_eq!(app.tabs.active().tab_indent, TabIndent::Tab);
    Component::handle_event(Editor, &press(KeyCode::Tab), app.tabs.active_mut());
    assert_eq!(app.tabs.active().text.lines[0], "\t");

    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn saving_the_settings_file_applies_it() {
    let dir = scratch_dir("settings-save");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let root = dir.join("storage");

    let mut app = App::new(Storage::open(root));
    app.apply(Action::OpenSettings);

    // What the user does in the buffer: change the value and save.
    app.tabs
        .active_mut()
        .text
        .load(vec![String::from("tab_indent = tab")]);
    app.apply(Action::Save);

    assert_eq!(app.storage.config().tab_indent, TabIndent::Tab);
    assert_eq!(app.tabs.active().tab_indent, TabIndent::Tab);

    // A tab opened after the change gets the new value too.
    let note = dir.join("note.txt");
    std::fs::write(&note, "x\n").unwrap();
    app.apply(Action::LoadFile(note));
    assert_eq!(app.tabs.active().tab_indent, TabIndent::Tab);

    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn opening_the_settings_leaves_the_session_alone() {
    let dir = scratch_dir("settings-session");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("notes.txt");
    std::fs::write(&file, "x\n").unwrap();
    let root = dir.join("storage");

    let mut app = App::new(Storage::open(root));
    app.apply(Action::LoadFile(file.clone()));
    app.apply(Action::OpenSettings);

    // The settings are not the work: the next run comes back to the file, and
    // the recent list stays a list of files.
    assert_eq!(
        app.storage.session().last_file.as_deref(),
        Some(file.as_path())
    );
    assert!(
        !app.storage
            .state()
            .recent_files()
            .iter()
            .any(|path| path.ends_with("config"))
    );

    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn closing_the_folder_takes_its_tabs_with_it() {
    let dir = scratch_dir("close-folder");
    let _ = std::fs::remove_dir_all(&dir);
    let workspace = dir.join("project");
    std::fs::create_dir_all(&workspace).unwrap();
    let inside = workspace.join("a.rs");
    std::fs::write(&inside, "fn main() {}\n").unwrap();
    let outside = dir.join("notes.txt");
    std::fs::write(&outside, "x\n").unwrap();
    let root = dir.join("storage");

    let mut app = App::new(Storage::open(root.clone()));
    app.apply(Action::LoadFolder(workspace.clone()));
    app.apply(Action::LoadFile(inside.clone()));
    app.apply(Action::LoadFile(outside.clone()));

    app.apply(Action::CloseFolder);

    // The tree is empty and the session forgot the folder, so a restart does
    // not come back to it.
    assert!(app.file_tree_state.root().is_none());
    assert!(app.storage.session().last_folder.is_none());

    // Only the file from outside the folder is still open.
    let open: Vec<&PathBuf> = app
        .tabs
        .buffers()
        .iter()
        .filter_map(|b| b.path.as_ref())
        .collect();
    assert_eq!(open, vec![&outside]);

    app.persist().unwrap();
    let reopened = App::new(Storage::open(root));
    assert!(reopened.file_tree_state.root().is_none());

    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn closing_the_folder_keeps_unsaved_buffers() {
    let dir = scratch_dir("close-folder-dirty");
    let _ = std::fs::remove_dir_all(&dir);
    let workspace = dir.join("project");
    std::fs::create_dir_all(&workspace).unwrap();
    let dirty = workspace.join("a.rs");
    let clean = workspace.join("b.rs");
    std::fs::write(&dirty, "fn a() {}\n").unwrap();
    std::fs::write(&clean, "fn b() {}\n").unwrap();

    let mut app = App::new(Storage::open(dir.join("storage")));
    app.apply(Action::LoadFolder(workspace.clone()));
    app.apply(Action::LoadFile(dirty.clone()));
    // a.rs is the active buffer here, so it is the one with unsaved work.
    app.tabs.active_mut().dirty = true;
    app.apply(Action::LoadFile(clean.clone()));

    app.apply(Action::CloseFolder);

    // The sweep is not a decision about someone's edits: the clean buffer
    // went, the dirty one stayed.
    let open: Vec<&PathBuf> = app
        .tabs
        .buffers()
        .iter()
        .filter_map(|b| b.path.as_ref())
        .collect();
    assert_eq!(open, vec![&dirty]);

    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn closing_a_folder_that_only_holds_the_last_tab_leaves_a_scratch_buffer() {
    let dir = scratch_dir("close-folder-empty");
    let _ = std::fs::remove_dir_all(&dir);
    let workspace = dir.join("project");
    std::fs::create_dir_all(&workspace).unwrap();
    let only = workspace.join("a.rs");
    std::fs::write(&only, "fn main() {}\n").unwrap();

    let mut app = App::new(Storage::open(dir.join("storage")));
    app.apply(Action::LoadFolder(workspace.clone()));
    app.apply(Action::LoadFile(only));
    app.apply(Action::CloseFolder);

    // The editor is never left with nothing to edit.
    assert_eq!(app.tabs.len(), 1);
    assert!(app.tabs.active().path.is_none());

    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn closing_with_no_folder_open_is_not_an_error() {
    let dir = scratch_dir("close-folder-none");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let mut app = App::new(Storage::open(dir.join("storage")));
    app.apply(Action::CloseFolder);

    assert!(app.file_tree_state.root().is_none());
    // A no-op says so instead of failing silently.
    assert!(!app.message_box_state.is_empty());

    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn saving_as_comes_back_to_the_directory_the_file_went_to() {
    let dir = scratch_dir("picker-save");
    let _ = std::fs::remove_dir_all(&dir);
    let target = dir.join("out");
    std::fs::create_dir_all(&target).unwrap();
    let root = dir.join("storage");

    let mut app = App::new(Storage::open(root.clone()));
    app.tabs
        .active_mut()
        .handle_key(KeyCode::Char('x'), KeyModifiers::NONE);
    app.apply(Action::SaveTo(target.join("new.txt")));
    app.persist().unwrap();

    let mut reopened = App::new(Storage::open(root));
    reopened.apply(Action::OpenPopup(PopupKind::SaveAs));
    assert_eq!(reopened.popup_state.picker.dir(), target);

    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_restart_reopens_the_file_and_where_the_cursor_was() {
    let (dir, root, file) = session_fixture("restore-file", "one\ntwo\nthree\n");

    let mut app = App::new(Storage::open(root.clone()));
    app.apply(Action::LoadFile(file.clone()));
    app.tabs.active_mut().text.cursor = Cursor { x: 3, y: 2 };
    app.remember_current_view();
    app.persist().unwrap();

    let mut reopened = App::new(Storage::open(root));
    reopened.restore_session();

    assert_eq!(
        reopened.tabs.active_mut().path.as_deref(),
        Some(file.as_path())
    );
    assert_eq!(
        reopened.tabs.active_mut().text.lines,
        ["one", "two", "three"]
    );
    assert_eq!(reopened.tabs.active_mut().text.cursor.y, 2);
    assert_eq!(reopened.tabs.active_mut().text.cursor.x, 3);
    // The file is back in the recent list, newest first.
    assert_eq!(reopened.storage.state().recent_files().first(), Some(&file));

    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_cursor_past_the_end_is_clamped_on_restore() {
    let (dir, root, file) = session_fixture("restore-clamp", "one\ntwo\nthree\n");

    let mut app = App::new(Storage::open(root.clone()));
    app.apply(Action::LoadFile(file.clone()));
    app.tabs.active_mut().text.cursor = Cursor { x: 3, y: 2 };
    app.remember_current_view();
    app.persist().unwrap();

    // The file shrank between runs: the remembered line is gone.
    std::fs::write(&file, "only\n").unwrap();

    let mut reopened = App::new(Storage::open(root));
    reopened.restore_session();

    assert_eq!(reopened.tabs.active_mut().text.cursor.y, 0);
    assert_eq!(reopened.tabs.active_mut().text.cursor.x, 3);

    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_restart_reopens_the_folder_in_the_tree() {
    let dir = scratch_dir("restore-folder");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("project/src")).unwrap();
    std::fs::write(dir.join("project/src/main.rs"), "fn main() {}\n").unwrap();
    let root = dir.join("storage");

    let mut app = App::new(Storage::open(root.clone()));
    app.apply(Action::LoadFolder(dir.join("project")));
    app.persist().unwrap();

    let mut reopened = App::new(Storage::open(root));
    reopened.restore_session();

    assert_eq!(
        reopened.file_tree_state.root(),
        Some(dir.join("project").as_path())
    );
    assert!(reopened.file_tree_visible);
    assert_eq!(
        reopened.storage.state().recent_dirs().first(),
        Some(&dir.join("project"))
    );

    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn the_panel_toggle_comes_back_even_without_the_session() {
    let dir = scratch_dir("restore-toggle");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let root = dir.join("storage");

    let mut app = App::new(Storage::open(root.clone()));
    app.apply(Action::ToggleFileTree);
    assert!(!app.file_tree_visible);

    // Session restoring off: the buffer and folder stay shut, the panel
    // preference still applies.
    app.storage
        .edit_config(|config| config.restore_session = false);
    app.persist().unwrap();

    let mut reopened = App::new(Storage::open(root));
    reopened.restore_session();

    assert!(!reopened.file_tree_visible);
    assert!(reopened.tabs.active_mut().path.is_none());

    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_file_that_no_longer_exists_leaves_the_scratch_buffer() {
    let (dir, root, file) = session_fixture("restore-missing", "one\n");

    let mut app = App::new(Storage::open(root.clone()));
    app.apply(Action::LoadFile(file.clone()));
    app.persist().unwrap();

    std::fs::remove_file(&file).unwrap();

    let mut reopened = App::new(Storage::open(root));
    reopened.restore_session();

    assert!(reopened.tabs.active_mut().path.is_none());
    assert_eq!(reopened.tabs.active_mut().text.lines, [""]);
    // A failed reopen is reported, not swallowed.
    assert!(!reopened.message_box_state.is_empty());

    std::fs::remove_dir_all(&dir).unwrap();
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
    app.tabs
        .active_mut()
        .handle_key(KeyCode::Char('x'), KeyModifiers::NONE);
    assert!(app.tabs.active_mut().dirty);

    assert!(!app.apply(Action::Quit));
    assert_eq!(app.popup_state.kind, PopupKind::Confirm);

    // Discarding the changes lets the quit through.
    assert!(app.apply(Action::ConfirmChoice(ConfirmChoice::Discard)));

    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_dirty_scratch_buffer_quits_without_asking() {
    let mut app = App::default();
    app.tabs
        .active_mut()
        .handle_key(KeyCode::Char('x'), KeyModifiers::NONE);
    assert!(app.tabs.active_mut().dirty);
    assert!(app.tabs.active_mut().path.is_none());

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
fn shift_tab_cycles_the_focus_between_the_panels() {
    let mut app = App::default();
    assert_eq!(app.focus, Focus::Editor);

    // The chord as a terminal reports it: crossterm maps both `ESC [ Z` and
    // Windows' shifted VK_TAB to `BackTab`, never to `Tab` + SHIFT.
    let shift_tab = KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT);
    assert!(shortcuts::is_focus_key(shift_tab));

    app.cycle_focus();
    assert_eq!(app.focus, Focus::FileTree);
    assert!(app.file_tree_state.is_focused());

    app.cycle_focus();
    assert_eq!(app.focus, Focus::Editor);
    assert!(!app.file_tree_state.is_focused());
}

#[test]
fn shift_tab_is_a_no_op_while_the_tree_is_hidden() {
    let mut app = App::default();
    app.apply(Action::ToggleFileTree);
    assert!(!app.file_tree_visible);

    // There is nothing to step onto, and the panel that took the keys last
    // must not be able to strand them.
    app.cycle_focus();
    assert_eq!(app.focus, Focus::Editor);
}

#[test]
fn hiding_the_tree_keeps_its_selection_without_the_highlight() {
    let dir = scratch_dir("tree-selection-hide");
    std::fs::create_dir_all(&dir).unwrap();

    let mut app = App::default();
    app.apply(Action::LoadFolder(dir.clone()));
    app.set_focus(Focus::FileTree);
    Component::handle_event(FileTree, &press(KeyCode::Down), &mut app.file_tree_state);
    assert_eq!(app.file_tree_state.selected_index(), 0);
    assert!(app.file_tree_state.is_focused());

    // Hiding the panel hands the keys back to the editor: the row is
    // remembered, the highlight is not.
    app.apply(Action::ToggleFileTree);

    assert!(!app.file_tree_visible);
    assert!(!app.file_tree_state.is_focused());
    assert_eq!(app.file_tree_state.selected_index(), 0);

    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn opening_a_file_from_the_tree_keeps_its_selection() {
    let (dir, _storage, file) = session_fixture("tree-selection-load", "hello\n");

    let mut app = App::default();
    app.apply(Action::LoadFolder(dir.clone()));
    app.set_focus(Focus::FileTree);
    Component::handle_event(FileTree, &press(KeyCode::Down), &mut app.file_tree_state);
    assert_eq!(app.file_tree_state.selected_index(), 1);

    // Loading moves focus to the editor; the tree remembers the row but stops
    // highlighting it.
    app.apply(Action::LoadFile(file));

    assert!(app.file_tree_state.is_focused());
    assert_eq!(app.file_tree_state.selected_index(), 1);

    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn loading_a_file_replaces_the_buffer() {
    let dir = scratch_dir("load");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("sample.txt");
    std::fs::write(&path, "hello\n").unwrap();

    let mut app = App::default();
    app.apply(Action::LoadFile(path.clone()));

    assert_eq!(app.tabs.active_mut().text.lines, vec!["hello"]);
    assert_eq!(app.tabs.active_mut().path.as_deref(), Some(path.as_path()));
    assert!(!app.tabs.active_mut().dirty);
    // The open is visible in the tab bar and the status bar, so it is not
    // announced in the message box as well: that one is kept for failures.

    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_dirty_buffer_opens_a_file_without_being_asked() {
    let dir = scratch_dir("open-dirty");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("other.txt");
    std::fs::write(&path, "other\n").unwrap();

    let mut app = App::default();
    app.tabs
        .active_mut()
        .handle_key(KeyCode::Char('x'), KeyModifiers::NONE);
    app.apply(Action::LoadFile(path.clone()));

    // The file gets its own tab and the dirty scratch keeps its own, so there
    // is nothing to save or discard first — and nothing to ask about.
    assert!(app.popup_state.kind.is_none());
    assert_eq!(app.tabs.len(), 2);
    assert_eq!(app.tabs.active_mut().text.lines, vec!["other"]);
    assert_eq!(app.tabs.active_mut().path.as_deref(), Some(path.as_path()));

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
    app.tabs
        .active_mut()
        .handle_key(KeyCode::Char('b'), KeyModifiers::NONE);
    app.apply(Action::Save);

    assert!(!app.tabs.active_mut().dirty);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "ba\n");

    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn saving_without_a_path_opens_the_save_picker() {
    let mut app = App::default();
    app.tabs
        .active_mut()
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
    app.tabs
        .active_mut()
        .handle_key(KeyCode::Char('n'), KeyModifiers::NONE);

    assert!(!app.apply(Action::SaveTo(path.clone())));
    assert_eq!(app.popup_state.kind, PopupKind::Confirm);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "old\n");

    app.apply(Action::ConfirmChoice(ConfirmChoice::Overwrite));

    assert_eq!(std::fs::read_to_string(&path).unwrap(), "n\n");
    assert_eq!(app.tabs.active_mut().path.as_deref(), Some(path.as_path()));
    assert!(!app.tabs.active_mut().dirty);

    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn saving_a_dirty_tab_resumes_the_close_that_asked() {
    let (dir, first, second) = tab_files("resume-close");

    let mut app = App::default();
    app.apply(Action::LoadFile(first.clone()));
    app.apply(Action::LoadFile(second.clone()));
    app.tabs
        .active_mut()
        .handle_key(KeyCode::Char('x'), KeyModifiers::NONE);
    assert!(app.tabs.active().dirty);

    app.apply(Action::CloseTab);
    assert_eq!(app.popup_state.kind, PopupKind::Confirm);

    // The answer carries the close out instead of dropping it: the file is
    // written, and only then does the tab go.
    app.apply(Action::ConfirmChoice(ConfirmChoice::Save));

    assert_eq!(app.tabs.len(), 1);
    assert_eq!(app.tabs.active().path.as_deref(), Some(first.as_path()));
    assert!(std::fs::read_to_string(&second).unwrap().starts_with('x'));

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

/// Plan §12: a server that dies must cost colors and nothing else — no panic,
/// no lost buffer, no repeated complaint.
#[test]
fn losing_the_language_server_costs_colors_and_nothing_else() {
    let mut app = App::default();
    app.tabs
        .active_mut()
        .handle_key(KeyCode::Char('a'), KeyModifiers::NONE);

    let rows = vec![vec![StyledRun {
        start: 0,
        end: 1,
        style: Style::default(),
    }]];
    app.tabs.active_mut().set_semantic_tokens(&rows);
    app.tabs.active_mut().set_diagnostics(&rows);
    assert!(
        app.tabs.active_mut().viewport_state.highlights.line_runs(0)[..]
            .iter()
            .any(|runs| !runs.is_empty())
    );

    app.apply_lsp_event(LspEvent::Stopped("rust-analyzer exited".to_string()));

    // Both layers the server owned are empty again...
    assert!(
        app.tabs.active_mut().viewport_state.highlights.line_runs(0)[..]
            .iter()
            .all(|runs| runs.is_empty())
    );
    // ...the buffer is untouched, typing still works...
    assert_eq!(app.tabs.active_mut().text.lines, ["a"]);
    app.tabs
        .active_mut()
        .handle_key(KeyCode::Char('b'), KeyModifiers::NONE);
    assert_eq!(app.tabs.active_mut().text.lines, ["ab"]);
    // ...and it was reported once.
    assert!(!app.message_box_state.is_empty());
    assert_eq!(app.status_bar_state.progress(), None);
}

#[test]
fn a_server_that_is_busy_is_shown_in_the_status_bar() {
    let mut app = App::default();

    app.apply_lsp_event(LspEvent::Progress);

    // The client is the source of the text; what the shell does with it is
    // only ever a copy into the status bar.
    assert_eq!(
        app.status_bar_state.progress(),
        app.lsp.progress_text().as_deref()
    );
}

#[test]
fn a_notice_is_reported_without_disturbing_the_colors() {
    let mut app = App::default();
    let rows = vec![vec![StyledRun {
        start: 0,
        end: 1,
        style: Style::default(),
    }]];
    app.tabs.active_mut().set_semantic_tokens(&rows);

    app.apply_lsp_event(LspEvent::Notice("too large".to_string()));

    assert!(!app.message_box_state.is_empty());
    assert!(
        app.tabs.active_mut().viewport_state.highlights.line_runs(0)[..]
            .iter()
            .any(|runs| !runs.is_empty())
    );
}

#[test]
fn ctrl_m_clears_the_message_box() {
    let mut app = App::default();
    app.message_box_state.success("opened: somewhere");
    assert!(!app.message_box_state.is_empty());

    for code in [KeyCode::Char('m'), KeyCode::Char('M'), KeyCode::Enter] {
        assert_eq!(
            shortcuts::lookup(KeyEvent::new(code, KeyModifiers::CONTROL)),
            Some(Action::ClearMessages)
        );
    }

    app.apply(Action::ClearMessages);
    assert!(app.message_box_state.is_empty());
}

/// Two files side by side in `dir`, so the tabs have something to hold.
fn tab_files(name: &str) -> (PathBuf, PathBuf, PathBuf) {
    let dir = scratch_dir(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let first = dir.join("first.txt");
    let second = dir.join("second.txt");
    std::fs::write(&first, "first\n").unwrap();
    std::fs::write(&second, "second\n").unwrap();

    (dir, first, second)
}

/// Renders the tab bar into one row and returns it as text, so these tests
/// read like what they are checking.
fn tab_bar(app: &App, width: u16) -> String {
    let area = Rect::new(0, 0, width, 1);
    let mut buf = Buffer::empty(area);
    app.render_tab_bar(area, &mut buf);

    (0..area.width).map(|x| buf[(x, 0)].symbol()).collect()
}

#[test]
fn opening_a_second_file_adds_a_tab() {
    let (dir, first, second) = tab_files("tabs-open");

    let mut app = App::default();
    app.apply(Action::LoadFile(first.clone()));
    // The scratch buffer becomes the first file rather than sitting beside it
    // as an empty tab.
    assert_eq!(app.tabs.len(), 1);

    app.apply(Action::LoadFile(second.clone()));

    assert_eq!(app.tabs.len(), 2);
    assert_eq!(app.tabs.active_index(), 1);
    assert_eq!(app.tabs.active().text.lines, vec!["second"]);

    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn opening_a_file_that_is_already_open_focuses_its_tab() {
    let (dir, first, second) = tab_files("tabs-refocus");

    let mut app = App::default();
    app.apply(Action::LoadFile(first.clone()));
    app.apply(Action::LoadFile(second.clone()));

    app.apply(Action::LoadFile(first.clone()));

    // Nothing is re-read: asking for a file that has a tab only focuses it,
    // so there is no second copy of the same buffer.
    assert_eq!(app.tabs.len(), 2);
    assert_eq!(app.tabs.active_index(), 0);
    assert_eq!(app.tabs.active().path.as_deref(), Some(first.as_path()));

    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn ctrl_w_closes_the_active_tab() {
    let (dir, first, second) = tab_files("tabs-close");

    let mut app = App::default();
    app.apply(Action::LoadFile(first.clone()));
    app.apply(Action::LoadFile(second.clone()));

    app.apply(Action::CloseTab);

    assert_eq!(app.tabs.len(), 1);
    assert_eq!(app.tabs.active().path.as_deref(), Some(first.as_path()));
    // The panel that was closed took the focus with it.
    assert_eq!(app.focus, Focus::Editor);

    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn closing_the_last_tab_leaves_a_scratch_buffer() {
    let (dir, first, _second) = tab_files("tabs-close-last");

    let mut app = App::new(Storage::open(dir.join("storage")));
    app.apply(Action::LoadFile(first.clone()));
    assert_eq!(app.tabs.len(), 1);

    app.apply(Action::CloseTab);

    // Every tab can be closed. The editor is never left with nothing to edit,
    // but what it is left with is a new, empty scratch — not the file.
    assert_eq!(app.tabs.len(), 1);
    assert!(app.tabs.active().path.is_none());
    assert!(!app.tabs.active().dirty);
    assert_eq!(app.tabs.active().text.lines, vec![String::new()]);
    assert!(app.popup_state.kind.is_none());
    // The buffer that went is gone from the session too, so a restart does not
    // come back to it.
    assert!(app.storage.session().tabs().is_empty());

    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_dirty_tab_asks_before_it_is_closed() {
    let (dir, first, second) = tab_files("tabs-close-dirty");

    let mut app = App::default();
    app.apply(Action::LoadFile(first.clone()));
    app.apply(Action::LoadFile(second.clone()));
    app.tabs
        .active_mut()
        .handle_key(KeyCode::Char('x'), KeyModifiers::NONE);
    assert!(app.tabs.active().dirty);

    app.apply(Action::CloseTab);

    // Nothing is lost while the question is unanswered.
    assert_eq!(app.popup_state.kind, PopupKind::Confirm);
    assert_eq!(app.tabs.len(), 2);

    app.apply(Action::ConfirmChoice(ConfirmChoice::Discard));

    assert_eq!(app.tabs.len(), 1);
    assert_eq!(app.tabs.active().path.as_deref(), Some(first.as_path()));

    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn the_tab_bar_names_the_open_buffers() {
    let (dir, first, second) = tab_files("tabs-render");

    let mut app = App::default();
    app.apply(Action::LoadFile(first.clone()));
    app.apply(Action::LoadFile(second.clone()));

    let row = tab_bar(&app, 40);

    assert!(row.contains("first"));
    assert!(row.contains("second"));
    // One pad drawn after the last tab, so the bar is one unbroken strip.
    assert_eq!(row.chars().count(), 40);

    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn tabs_that_do_not_fit_are_counted_instead_of_clipped() {
    let (dir, first, second) = tab_files("tabs-overflow");

    let mut app = App::default();
    app.apply(Action::LoadFile(first.clone()));
    app.apply(Action::LoadFile(second.clone()));

    // Exactly enough room for the icon, " first.txt " and the "+1" counter.
    let row = tab_bar(&app, 16);

    assert!(row.contains("first.txt"));
    assert!(!row.contains("second"));
    assert!(row.ends_with("+1"), "{row:?}");

    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_dirty_tab_is_marked_in_the_bar() {
    let (dir, first, _second) = tab_files("tabs-render-dirty");

    let mut app = App::default();
    app.apply(Action::LoadFile(first.clone()));
    app.tabs
        .active_mut()
        .handle_key(KeyCode::Char('x'), KeyModifiers::NONE);

    assert!(tab_bar(&app, 40).contains("first.txt*"));

    std::fs::remove_dir_all(&dir).unwrap();
}


#[test]
fn probe_fuzz_read() {
    let dir = scratch_dir("probe-fuzz");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let mut cases: Vec<String> = Vec::new();
    // Systematic: every combination of line ending for 1..3 lines, plus blanks.
    let endings = ["\n", "\r\n", "\r"];
    for e in endings {
        for n in 1..=3usize {
            let mut c = String::new();
            for i in 0..n {
                c.push_str(if i == 0 { "a" } else { "b" });
                c.push_str(e);
            }
            cases.push(c.clone());          // ends with terminator
            cases.push(format!("{c}{e}"));  // extra terminator => blank last line
            cases.push(c.trim_end_matches(['\r', '\n']).to_string()); // no terminator
        }
    }
    for extra in ["", " ", "\t", "\u{feff}", "\u{e9}", "你"] {
        cases.push(format!("a\n{extra}\n"));
        cases.push(format!("a\n{extra}"));
    }
    cases.push(String::new());
    cases.push("\n".to_string());
    cases.push("\n\n".to_string());
    cases.push("\r\n\r\n".to_string());

    let mut bad = 0;
    for (i, content) in cases.iter().enumerate() {
        let path = dir.join(format!("c{i}.txt"));
        std::fs::write(&path, content.as_bytes()).unwrap();
        let got = crate::fs::read_text_file(&path).unwrap();

        // Reference model (convention A): split on '\n', drop the final empty
        // piece produced by a trailing terminator; each piece loses one trailing
        // '\r'. Empty file -> one empty line.
        let mut expected: Vec<String> = Vec::new();
        if content.is_empty() {
            expected.push(String::new());
        } else {
            let mut pieces: Vec<&str> = content.split('\n').collect();
            if pieces.last() == Some(&"") {
                pieces.pop();
            }
            for piece in pieces {
                let piece = piece.strip_suffix('\r').unwrap_or(piece);
                expected.push(piece.to_string());
            }
            if expected.is_empty() {
                expected.push(String::new());
            }
        }

        if got != expected {
            bad += 1;
            println!("PROBE MISMATCH {content:?}: got {got:?} expected {expected:?}");
        }
    }
    println!("PROBE cases {} mismatches {}", cases.len(), bad);
    let _ = std::fs::remove_dir_all(&dir);
}

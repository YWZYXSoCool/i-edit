use super::{Action, App, Focus};
use crate::Cursor;
use crate::action::ConfirmChoice;
use crate::component::Component;
use crate::shortcuts;
use crate::storage::Storage;
use crate::widgets::FileTree;
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
fn saving_as_comes_back_to_the_directory_the_file_went_to() {
    let dir = scratch_dir("picker-save");
    let _ = std::fs::remove_dir_all(&dir);
    let target = dir.join("out");
    std::fs::create_dir_all(&target).unwrap();
    let root = dir.join("storage");

    let mut app = App::new(Storage::open(root.clone()));
    app.editor_state
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
    app.editor_state.text.cursor = Cursor { x: 3, y: 2 };
    app.remember_current_view();
    app.persist().unwrap();

    let mut reopened = App::new(Storage::open(root));
    reopened.restore_session();

    assert_eq!(reopened.editor_state.path.as_deref(), Some(file.as_path()));
    assert_eq!(reopened.editor_state.text.lines, ["one", "two", "three"]);
    assert_eq!(reopened.editor_state.text.cursor.y, 2);
    assert_eq!(reopened.editor_state.text.cursor.x, 3);
    // The file is back in the recent list, newest first.
    assert_eq!(reopened.storage.state().recent_files().first(), Some(&file));

    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_cursor_past_the_end_is_clamped_on_restore() {
    let (dir, root, file) = session_fixture("restore-clamp", "one\ntwo\nthree\n");

    let mut app = App::new(Storage::open(root.clone()));
    app.apply(Action::LoadFile(file.clone()));
    app.editor_state.text.cursor = Cursor { x: 3, y: 2 };
    app.remember_current_view();
    app.persist().unwrap();

    // The file shrank between runs: the remembered line is gone.
    std::fs::write(&file, "only\n").unwrap();

    let mut reopened = App::new(Storage::open(root));
    reopened.restore_session();

    assert_eq!(reopened.editor_state.text.cursor.y, 0);
    assert_eq!(reopened.editor_state.text.cursor.x, 3);

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
    assert!(reopened.editor_state.path.is_none());

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

    assert!(reopened.editor_state.path.is_none());
    assert_eq!(reopened.editor_state.text.lines, [""]);
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

    assert_eq!(app.focus, Focus::Editor);
    assert!(!app.file_tree_state.is_focused());
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
            shortcuts::lookup(KeyEvent::new(code, KeyModifiers::CONTROL)),
            Some(Action::ClearMessages)
        );
    }

    app.apply(Action::ClearMessages);
    assert!(app.message_box_state.is_empty());
}

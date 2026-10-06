use super::render::{clip_to_width, row_spans, with_separator};
use super::{Picker, PickerMode, PickerState, Row};
use crate::action::Action;
use crate::component::Component;
use crate::fs::DirEntry;
use crate::icon;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::text::Span;
use unicode_width::UnicodeWidthStr;

/// A unique directory that deletes itself, so tests can run in parallel
/// and leave nothing behind even when they panic.
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let unique = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "i-edit-picker-test-{}-{}",
            std::process::id(),
            unique
        ));

        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn press(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn press_shift(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::SHIFT))
}

fn ctrl_s() -> Event {
    Event::Key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL))
}

fn picker(mode: PickerMode, dir: &Path, preset: Option<PathBuf>) -> PickerState {
    let mut state = PickerState::new(mode, dir.to_path_buf(), preset);
    state.take_actions();
    state
}

fn key(state: &mut PickerState, event: Event) {
    Component::handle_event(Picker, &event, state);
}

fn type_text(state: &mut PickerState, text: &str) {
    for ch in text.chars() {
        key(state, press(KeyCode::Char(ch)));
    }
}

fn text_of(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn row_names(state: &PickerState) -> Vec<&str> {
    state
        .rows
        .iter()
        .filter_map(|row| match row {
            Row::Entry(entry) => Some(entry.name.as_str()),
            Row::Parent => None,
        })
        .collect()
}

/// Moves the highlight onto the entry named `name` with the real
/// navigation keys, so wrap-around is exercised as a side effect.
fn select(state: &mut PickerState, name: &str) {
    let index = state
        .rows
        .iter()
        .position(|row| matches!(row, Row::Entry(entry) if entry.name == name))
        .unwrap_or_else(|| panic!("no row named {name:?}"));

    for _ in 0..=state.rows.len() {
        if state.highlighted == index {
            return;
        }
        key(state, press(KeyCode::Down));
    }
    panic!("never reached {name:?}");
}

fn select_parent(state: &mut PickerState) {
    let index = state
        .rows
        .iter()
        .position(|row| matches!(row, Row::Parent))
        .expect("no parent row");

    for _ in 0..=state.rows.len() {
        if state.highlighted == index {
            return;
        }
        key(state, press(KeyCode::Down));
    }
    panic!("never reached the parent row");
}

fn render(state: &mut PickerState, area: Rect) -> Buffer {
    let mut buf = Buffer::empty(area);
    Component::render(Picker, area, &mut buf, state);
    buf
}

fn buffer_contains(buf: &Buffer, area: Rect, needle: &str) -> bool {
    (area.y..area.bottom()).any(|y| {
        let row: String = (area.x..area.right())
            .map(|x| buf[(x, y)].symbol())
            .collect();
        row.contains(needle)
    })
}

/// Whether any cell showing `symbol` uses `color` as its foreground.
fn any_fg_of_symbol(buf: &Buffer, area: Rect, symbol: &str, color: Color) -> bool {
    (area.y..area.bottom()).any(|y| {
        (area.x..area.right())
            .any(|x| buf[(x, y)].symbol() == symbol && buf[(x, y)].style().fg == Some(color))
    })
}

fn file_row(name: &str) -> Row {
    Row::Entry(DirEntry {
        path: PathBuf::new(),
        name: name.to_string(),
        is_dir: false,
        is_hidden: false,
    })
}

fn dir_row(name: &str) -> Row {
    Row::Entry(DirEntry {
        path: PathBuf::new(),
        name: name.to_string(),
        is_dir: true,
        is_hidden: false,
    })
}

fn span_texts<'a>(spans: &'a [Span<'a>]) -> Vec<&'a str> {
    spans.iter().map(|span| span.content.as_ref()).collect()
}

/// The visible text of one buffer row, advancing by each symbol's display
/// width so the placeholder cell behind a wide glyph is not duplicated.
fn visible_text(buf: &Buffer, area: Rect, y: u16) -> String {
    let mut text = String::new();
    let mut x = area.x;
    while x < area.right() {
        let symbol = buf[(x, y)].symbol();
        text.push_str(symbol);
        x = x.saturating_add(UnicodeWidthStr::width(symbol).max(1) as u16);
    }
    text
}

#[test]
fn entering_a_directory_refreshes_the_rows() {
    let dir = TempDir::new();
    let sub = dir.path().join("sub");
    fs::create_dir(&sub).unwrap();
    fs::write(sub.join("inner.txt"), "x").unwrap();

    let mut state = picker(PickerMode::File, dir.path(), None);
    assert!(!row_names(&state).contains(&"inner.txt"));

    select(&mut state, "sub");
    key(&mut state, press(KeyCode::Enter));

    assert_eq!(state.path_input.text(), text_of(&sub).as_str());
    assert!(row_names(&state).contains(&"inner.txt"));
    assert_eq!(state.highlighted, 0);
    assert!(state.take_actions().is_empty());
}

#[test]
fn enter_on_a_file_emits_load_file() {
    let dir = TempDir::new();
    let file = dir.path().join("a.txt");
    fs::write(&file, "x").unwrap();

    let mut state = picker(PickerMode::File, dir.path(), None);
    select(&mut state, "a.txt");
    key(&mut state, press(KeyCode::Enter));

    assert_eq!(state.take_actions(), vec![Action::LoadFile(file)]);
}

#[test]
fn shift_enter_chooses_the_directory_being_browsed() {
    let dir = TempDir::new();
    let sub = dir.path().join("sub");
    fs::create_dir(&sub).unwrap();

    let mut state = picker(PickerMode::Folder, dir.path(), None);
    // Walk into it first: the point of the key is that the directory the
    // picker is *in* can be chosen without going back up.
    select(&mut state, "sub");
    key(&mut state, press(KeyCode::Right));
    assert_eq!(state.path_input.text(), text_of(&sub).as_str());

    key(&mut state, press_shift(KeyCode::Enter));

    assert_eq!(state.take_actions(), vec![Action::LoadFolder(sub)]);
}

#[test]
fn shift_enter_uses_the_typed_path_not_the_highlight() {
    let dir = TempDir::new();
    let sub = dir.path().join("sub");
    fs::create_dir(&sub).unwrap();
    fs::write(sub.join("a.txt"), "x").unwrap();

    let mut state = picker(PickerMode::Folder, dir.path(), None);
    select(&mut state, "sub");
    key(&mut state, press(KeyCode::Right));
    // A file is highlighted: the key must still take the directory.
    select(&mut state, "a.txt");

    key(&mut state, press_shift(KeyCode::Enter));

    assert_eq!(state.take_actions(), vec![Action::LoadFolder(sub)]);
}

#[test]
fn shift_enter_is_only_in_folder_mode() {
    let dir = TempDir::new();
    fs::write(dir.path().join("a.txt"), "x").unwrap();

    // File mode has no "choose this directory": the key falls through to
    // the path input, which leaves the picker where it was.
    let mut state = picker(PickerMode::File, dir.path(), None);
    key(&mut state, press_shift(KeyCode::Enter));
    assert!(state.take_actions().is_empty());

    // Save mode owns Enter entirely and is not affected either.
    let mut state = picker(PickerMode::Save, dir.path(), None);
    key(&mut state, press_shift(KeyCode::Enter));
    assert!(state.take_actions().is_empty());
}

#[test]
fn the_folder_footer_mentions_the_key() {
    assert!(PickerMode::Folder.footer().contains("Shift+Enter"));
    assert!(!PickerMode::File.footer().contains("Shift+Enter"));
}

#[test]
fn folder_mode_picks_a_directory_and_walks_up_without_emitting() {
    let dir = TempDir::new();
    let sub = dir.path().join("sub");
    fs::create_dir(&sub).unwrap();

    let mut state = picker(PickerMode::Folder, dir.path(), None);
    select(&mut state, "sub");
    key(&mut state, press(KeyCode::Enter));
    assert_eq!(state.take_actions(), vec![Action::LoadFolder(sub.clone())]);

    // Right browses into the directory without choosing it.
    key(&mut state, press(KeyCode::Right));
    assert_eq!(state.path_input.text(), text_of(&sub).as_str());
    assert!(state.take_actions().is_empty());

    // `..` navigates to the parent and emits nothing.
    select_parent(&mut state);
    key(&mut state, press(KeyCode::Enter));
    assert_eq!(state.path_input.text(), text_of(dir.path()).as_str());
    assert!(state.take_actions().is_empty());
}

#[test]
fn folder_mode_enter_on_a_file_does_nothing() {
    let dir = TempDir::new();
    fs::write(dir.path().join("a.txt"), "x").unwrap();

    let mut state = picker(PickerMode::Folder, dir.path(), None);
    select(&mut state, "a.txt");
    key(&mut state, press(KeyCode::Enter));

    assert!(state.take_actions().is_empty());
}

#[test]
fn save_mode_confirms_with_ctrl_s_and_ignores_an_empty_name() {
    let dir = TempDir::new();

    // Empty name: confirming must not emit anything.
    let mut state = picker(PickerMode::Save, dir.path(), None);
    key(&mut state, ctrl_s());
    assert!(state.take_actions().is_empty());

    // The name is trimmed before it becomes a path component.
    type_text(&mut state, "  report.txt  ");
    key(&mut state, ctrl_s());
    assert_eq!(
        state.take_actions(),
        vec![Action::SaveTo(dir.path().join("report.txt"))]
    );
}

#[test]
fn save_mode_confirms_from_the_path_focus_too() {
    let dir = TempDir::new();

    let mut state = picker(PickerMode::Save, dir.path(), None);
    type_text(&mut state, "target.txt");
    key(&mut state, press(KeyCode::Tab));
    key(&mut state, ctrl_s());

    assert_eq!(
        state.take_actions(),
        vec![Action::SaveTo(dir.path().join("target.txt"))]
    );
}

#[test]
fn save_mode_enter_on_a_file_fills_the_name_field() {
    let dir = TempDir::new();
    let file = dir.path().join("chapter.txt");
    fs::write(&file, "x").unwrap();

    let mut state = picker(PickerMode::Save, dir.path(), None);
    key(&mut state, press(KeyCode::Tab));
    select(&mut state, "chapter.txt");
    key(&mut state, press(KeyCode::Enter));

    assert_eq!(state.name_input.text(), "chapter.txt");
    assert!(state.take_actions().is_empty());

    // Focus moved to the name field, where Enter confirms.
    key(&mut state, press(KeyCode::Enter));
    assert_eq!(state.take_actions(), vec![Action::SaveTo(file)]);
}

#[test]
fn save_mode_prefills_the_name_from_the_preset() {
    let dir = TempDir::new();
    let preset = dir.path().join("draft.md");

    let mut state = PickerState::new(PickerMode::Save, dir.path().to_path_buf(), Some(preset));

    assert_eq!(state.mode(), PickerMode::Save);
    assert_eq!(state.name_input.text(), "draft.md");
    assert!(state.take_actions().is_empty());
}

#[test]
fn file_mode_highlights_the_preset() {
    let dir = TempDir::new();
    let preset = dir.path().join("b.txt");
    fs::write(&preset, "x").unwrap();

    let state = PickerState::new(
        PickerMode::File,
        dir.path().to_path_buf(),
        Some(preset.clone()),
    );
    let index = state
        .rows
        .iter()
        .position(|row| matches!(row, Row::Entry(entry) if entry.path == preset))
        .unwrap();

    assert_eq!(state.highlighted, index);
}

#[test]
fn the_parent_row_is_listed_first() {
    let dir = TempDir::new();
    let mut state = picker(PickerMode::File, dir.path(), None);

    assert!(matches!(state.rows.first(), Some(Row::Parent)));

    let area = Rect::new(0, 0, 40, 12);
    let buf = render(&mut state, area);
    assert!(buffer_contains(&buf, area, ".."));
}

#[test]
fn hidden_entries_are_flagged_and_rendered_dim() {
    let dir = TempDir::new();
    fs::write(dir.path().join(".hidden"), "x").unwrap();

    let mut state = picker(PickerMode::File, dir.path(), None);
    let hidden = state
        .rows
        .iter()
        .find_map(|row| match row {
            Row::Entry(entry) if entry.name == ".hidden" => Some(entry),
            _ => None,
        })
        .expect("hidden file not listed");
    assert!(hidden.is_hidden);

    let area = Rect::new(0, 0, 60, 20);
    let buf = render(&mut state, area);

    assert!(buffer_contains(&buf, area, ".hidden"));
    assert!(any_fg_of_symbol(&buf, area, ".", Color::DarkGray));
}

#[test]
fn an_empty_directory_shows_the_empty_marker() {
    let dir = TempDir::new();
    let empty = dir.path().join("empty");
    fs::create_dir(&empty).unwrap();

    let mut state = picker(PickerMode::File, &empty, None);
    assert!(state.rows.iter().any(|row| matches!(row, Row::Parent)));

    let area = Rect::new(0, 0, 40, 12);
    let buf = render(&mut state, area);
    assert!(buffer_contains(&buf, area, "(empty)"));

    // A buffer too small for the overlay must stay panic-free.
    let tiny = Rect::new(0, 0, 4, 2);
    let mut tiny_buf = Buffer::empty(tiny);
    Component::render(Picker, tiny, &mut tiny_buf, &mut state);
}

#[test]
fn a_missing_directory_lists_nothing() {
    let dir = TempDir::new();
    let missing = dir.path().join("nope");

    let mut state = picker(PickerMode::File, &missing, None);
    assert!(state.rows.is_empty());

    key(&mut state, press(KeyCode::Enter));
    key(&mut state, press(KeyCode::Tab));
    key(&mut state, press(KeyCode::Right));
    assert!(state.take_actions().is_empty());

    let _ = render(&mut state, Rect::new(0, 0, 30, 10));
}

#[test]
fn the_highlight_wraps_around() {
    let dir = TempDir::new();
    fs::write(dir.path().join("a.txt"), "x").unwrap();
    fs::write(dir.path().join("b.txt"), "x").unwrap();

    let mut state = picker(PickerMode::File, dir.path(), None);
    let last = state.rows.len() - 1;

    key(&mut state, press(KeyCode::Up));
    assert_eq!(state.highlighted, last);

    key(&mut state, press(KeyCode::Down));
    assert_eq!(state.highlighted, 0);
}

#[test]
fn rendering_scrolls_to_keep_the_highlight_visible() {
    let dir = TempDir::new();
    for i in 0..20 {
        fs::write(dir.path().join(format!("f{i:02}.txt")), "x").unwrap();
    }

    let mut state = picker(PickerMode::File, dir.path(), None);
    let target = state
        .rows
        .iter()
        .position(|row| matches!(row, Row::Entry(entry) if entry.name == "f15.txt"))
        .unwrap();
    for _ in 0..target {
        key(&mut state, press(KeyCode::Down));
    }
    assert_eq!(state.highlighted, target);

    let area = Rect::new(0, 0, 40, 12);
    let buf = render(&mut state, area);

    assert!(buffer_contains(&buf, area, "f15.txt"));
    assert!(!buffer_contains(&buf, area, "f00.txt"));
}

#[test]
fn tab_completes_a_directory_with_a_separator() {
    let dir = TempDir::new();
    let sub = dir.path().join("sub");
    fs::create_dir(&sub).unwrap();
    fs::write(sub.join("inner.txt"), "x").unwrap();

    let mut state = picker(PickerMode::File, dir.path(), None);
    select(&mut state, "sub");
    key(&mut state, press(KeyCode::Tab));

    assert_eq!(state.path_input.text(), with_separator(&sub).as_str());
    assert!(row_names(&state).contains(&"inner.txt"));
    assert!(state.take_actions().is_empty());
}

#[test]
fn tab_completes_a_file_without_a_separator() {
    let dir = TempDir::new();
    let file = dir.path().join("a.txt");
    fs::write(&file, "x").unwrap();

    let mut state = picker(PickerMode::File, dir.path(), None);
    select(&mut state, "a.txt");
    key(&mut state, press(KeyCode::Tab));

    assert_eq!(state.path_input.text(), text_of(&file).as_str());
}

#[test]
fn left_walks_to_the_parent_directory() {
    let dir = TempDir::new();
    let sub = dir.path().join("sub");
    fs::create_dir(&sub).unwrap();

    let mut state = picker(PickerMode::File, &sub, None);
    key(&mut state, press(KeyCode::Left));

    assert_eq!(state.path_input.text(), text_of(dir.path()).as_str());
    assert!(state.take_actions().is_empty());
}

/// A name is clipped on character boundaries: a double-width glyph that
/// does not fit whole is dropped, never left hanging over the row's edge.
#[test]
fn clipping_stops_in_front_of_a_wide_glyph_that_does_not_fit() {
    assert_eq!(clip_to_width("中文名", 5), "中文");
    assert_eq!(clip_to_width("中文名", 3), "中");
    assert_eq!(clip_to_width("中文名", 1), "");
    // Zero-width marks ride along with the character before them.
    assert_eq!(clip_to_width("e\u{301}x", 1), "e\u{301}");
}

#[test]
fn row_spans_spend_the_width_in_order_and_drop_the_suffix_first() {
    let file = file_row("中文名字.txt");
    let spans = row_spans(&file, 8);
    assert_eq!(span_texts(&spans), [" ", " ", icon::FILE, " ", "中文"]);

    // A directory at a budget that just fits the name loses its `/` first.
    let dir = dir_row("中文名");
    let spans = row_spans(&dir, 10);
    assert_eq!(
        span_texts(&spans),
        [" ", icon::CHEVRON, icon::FOLDER, " ", "中文名"]
    );

    let spans = row_spans(&Row::Parent, 40);
    assert_eq!(
        span_texts(&spans),
        [" ", icon::CHEVRON, icon::FOLDER, " ", ".."]
    );
}

#[test]
fn a_wide_name_cut_at_the_right_edge_stays_whole() {
    let dir = TempDir::new();
    let name = "中文文件名.txt";
    fs::write(dir.path().join(name), "x").unwrap();

    let mut state = picker(PickerMode::File, dir.path(), None);
    select(&mut state, name);

    // The picker is a percentage of the area, so sweep a few widths: the
    // name may be shortened, but only on character boundaries.
    for width in 10..=30 {
        let area = Rect::new(0, 0, width, 10);
        let buf = render(&mut state, area);

        let row = (area.y..area.bottom())
            .map(|y| visible_text(&buf, area, y))
            .find(|row| row.contains('中'))
            .unwrap_or_else(|| panic!("name row missing at width {width}"));

        // Everything from the first glyph of the name on, up to the right
        // border: a whole-character prefix, never a split one.
        let visible = &row[row.find('中').unwrap()..];
        let visible = visible.split('│').next().unwrap().trim_end();
        assert!(
            name.starts_with(visible),
            "width {width} showed {visible:?}"
        );
    }

    // With enough room the whole name shows.
    let area = Rect::new(0, 0, 40, 12);
    let buf = render(&mut state, area);
    let row = (area.y..area.bottom())
        .map(|y| visible_text(&buf, area, y))
        .find(|row| row.contains('中'))
        .expect("name row missing at 40 columns");
    assert!(row.contains(name));
}

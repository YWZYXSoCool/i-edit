use std::path::PathBuf;

use super::{Editor, EditorState};
use crate::Cursor;
use crate::component::Component;

use crossterm::event::{Event, KeyCode, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier};

fn run(keys: &[KeyCode], state: &mut EditorState) {
    for code in keys {
        state.handle_key(*code, KeyModifiers::NONE);
    }
}

/// Renders into a 20x5 area: 2 columns for the vertical scrollbar, 6 for
/// the line-number gutter and 12 visible text columns.
fn render(state: &mut EditorState) -> Buffer {
    render_in(state, 20, 5)
}

/// Renders into an arbitrary area, e.g. wide enough for the scratch hint.
fn render_in(state: &mut EditorState, width: u16, height: u16) -> Buffer {
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);
    Component::render(Editor, area, &mut buf, state);
    buf
}

/// The buffer's rows joined by newlines, for substring checks.
fn text_of(buf: &Buffer) -> String {
    let area = buf.area();

    (0..area.height)
        .map(|y| {
            (0..area.width)
                .map(|x| buf[(x, y)].symbol())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn starts_with_a_single_empty_line() {
    let state = EditorState::new();
    assert_eq!(state.text.lines, vec![String::new()]);
    assert_eq!((state.text.cursor.x, state.text.cursor.y), (0, 0));
}

#[test]
fn an_empty_scratch_buffer_shows_the_vim_style_welcome() {
    let mut state = EditorState::new();

    // 13 rows: title + version + blank + 10 hint lines (Shift+Tab and Tab
    // are now separate bindings, so the block is one line taller than the
    // original nine-hint layout).
    let buf = render_in(&mut state, 64, 13);

    let text = text_of(&buf);
    assert!(text.contains(env!("CARGO_PKG_NAME")));
    assert!(text.contains(concat!("version ", env!("CARGO_PKG_VERSION"))));
    assert!(text.contains("open file"));
    assert!(text.contains("toggle file tree"));
    assert!(text.contains("quit"));

    // Centered block: title bright and bold at the top, hints dim, like
    // the file tree's placeholder.
    assert_eq!(buf[(17, 0)].symbol(), "i");
    assert_eq!(buf[(17, 0)].fg, Color::White);
    assert!(buf[(17, 0)].modifier.contains(Modifier::BOLD));
    assert_eq!(buf[(17, 3)].symbol(), "C");
    assert_eq!(buf[(17, 3)].fg, Color::DarkGray);
    // The last hint keeps its own row and the dim style, so every welcome
    // line is drawn, not only the first few. It is "Tab: insert 4 spaces".
    assert_eq!(buf[(17, 12)].symbol(), "T");
    assert_eq!(buf[(17, 12)].fg, Color::DarkGray);
}

#[test]
fn the_welcome_leaves_once_something_is_typed() {
    let mut state = EditorState::new();
    run(&[KeyCode::Char('x')], &mut state);

    let buf = render_in(&mut state, 64, 12);

    assert!(!text_of(&buf).contains("i-edit"));
}

#[test]
fn a_named_empty_buffer_shows_no_welcome() {
    let mut state = EditorState::new();
    state.load_file(PathBuf::from("empty.txt"), vec![String::new()]);

    let buf = render_in(&mut state, 64, 12);

    assert!(!text_of(&buf).contains("open file"));
}

#[test]
fn a_small_editor_drops_the_welcome_instead_of_clipping_it() {
    let mut state = EditorState::new();

    let buf = render(&mut state);

    assert!(!text_of(&buf).contains("i-edit"));
}

#[test]
fn typing_appends_to_the_current_line() {
    let mut state = EditorState::new();
    run(&[KeyCode::Char('a'), KeyCode::Char('b')], &mut state);

    assert_eq!(state.text.lines, vec![String::from("ab")]);
    assert_eq!(state.text.cursor.x, 2);
}

#[test]
fn enter_splits_the_line_at_the_cursor() {
    let mut state = EditorState::new();
    run(
        &[
            KeyCode::Char('a'),
            KeyCode::Char('b'),
            KeyCode::Left,
            KeyCode::Enter,
        ],
        &mut state,
    );

    assert_eq!(state.text.lines, vec![String::from("a"), String::from("b")]);
    assert_eq!((state.text.cursor.x, state.text.cursor.y), (0, 1));
}

#[test]
fn backspace_at_line_start_joins_lines() {
    let mut state = EditorState::new();
    run(
        &[
            KeyCode::Char('a'),
            KeyCode::Enter,
            KeyCode::Char('b'),
            KeyCode::Backspace,
            KeyCode::Backspace,
        ],
        &mut state,
    );

    assert_eq!(state.text.lines, vec![String::from("a")]);
    assert_eq!(state.text.cursor.x, 1);
}

#[test]
fn wide_chars_keep_the_cursor_on_char_boundaries() {
    let mut state = EditorState::new();
    run(
        &[KeyCode::Char('你'), KeyCode::Left, KeyCode::Delete],
        &mut state,
    );

    assert_eq!(state.text.lines, vec![String::new()]);
    assert_eq!(state.text.cursor.x, 0);
}

#[test]
fn vertical_movement_keeps_the_display_column() {
    let mut state = EditorState::new();
    state.text.lines = vec![String::from("你好ab"), String::from("x")];
    // End of "你好ab": byte 8, display column 6.
    state.text.cursor = Cursor { x: 8, y: 0 };

    run(&[KeyCode::Down], &mut state);

    // Column 6 does not exist on "x", so the cursor stops at its end.
    assert_eq!((state.text.cursor.x, state.text.cursor.y), (1, 1));
}

#[test]
fn paging_stays_safe_on_shorter_lines() {
    let mut state = EditorState::new();
    state.text.lines = (0..25)
        .map(|i| {
            if i == 0 {
                "a".repeat(30)
            } else {
                String::from("xy")
            }
        })
        .collect();
    state.text.cursor = Cursor { x: 30, y: 0 };

    run(&[KeyCode::PageDown], &mut state);
    assert_eq!((state.text.cursor.x, state.text.cursor.y), (2, 20));

    run(&[KeyCode::PageUp], &mut state);
    assert_eq!((state.text.cursor.x, state.text.cursor.y), (2, 0));
}

#[test]
fn typing_past_the_right_edge_scrolls_the_view() {
    let mut state = EditorState::new();
    state.text.lines = vec!["a".repeat(40)];
    state.viewport_state.width = 10;
    state.text.cursor = Cursor { x: 9, y: 0 };

    run(&[KeyCode::Right], &mut state);

    // Column 10 no longer fits in 10 columns starting at 0.
    assert_eq!(state.text.cursor.x, 10);
    assert_eq!(state.viewport_state.scroll_x, 1);

    run(&[KeyCode::Home], &mut state);

    assert_eq!(state.viewport_state.scroll_x, 0);
}

#[test]
fn cursor_screen_pos_accounts_for_horizontal_scroll() {
    let mut state = EditorState::new();
    state.text.lines = vec!["a".repeat(40)];
    state.viewport_state.scroll_x = 5;
    state.text.cursor = Cursor { x: 8, y: 0 };

    let pos = super::cursor_screen_pos(Rect::new(0, 0, 30, 5), &state);

    // Gutter width 6, plus display column 8 shifted left by 5.
    assert_eq!((pos.x, pos.y), (9, 0));
}

#[test]
fn an_overflowing_line_gets_a_horizontal_scrollbar() {
    let mut state = EditorState::new();
    state.text.lines = vec!["a".repeat(40)];

    let buf = render(&mut state);

    // The bar sits on the bottom row, aligned with the 12 text columns.
    assert_eq!(buf[(6, 4)].symbol(), "◄");
    assert_eq!(buf[(7, 4)].symbol(), "▬");
    assert_eq!(buf[(16, 4)].symbol(), "─");
    assert_eq!(buf[(17, 4)].symbol(), "►");
    // The reserved row shrinks the viewport, not the text itself.
    assert_eq!(state.viewport_state.height, 4);
    assert_eq!(buf[(6, 0)].symbol(), "a");
}

#[test]
fn short_lines_keep_the_bottom_row_for_text() {
    let mut state = EditorState::new();
    state.text.lines = (0..5).map(|i| format!("line {i}")).collect();

    let buf = render(&mut state);

    assert_eq!(state.viewport_state.height, 5);
    assert_eq!(buf[(6, 4)].symbol(), "l");
}

#[test]
fn the_cursor_line_number_is_highlighted() {
    let mut state = EditorState::new();
    state.text.lines = vec![String::from("a"), String::from("b")];
    state.text.cursor = Cursor { x: 0, y: 1 };

    let buf = render(&mut state);

    assert_eq!(buf[(2, 0)].fg, Color::DarkGray);
    assert_eq!(buf[(2, 1)].fg, Color::White);
}

#[test]
fn the_thumb_follows_the_horizontal_scroll() {
    let mut state = EditorState::new();
    state.text.lines = vec!["a".repeat(40)];
    state.viewport_state.scroll_x = 12;

    let buf = render(&mut state);

    assert_eq!(state.h_scrollbar_state.get_position(), 12);
    assert_eq!(buf[(7, 4)].symbol(), "─");
    assert_eq!(buf[(10, 4)].symbol(), "▬");
}

#[test]
fn rendering_pulls_a_stale_horizontal_offset_back() {
    let mut state = EditorState::new();
    state.text.lines = vec!["a".repeat(8)];
    state.viewport_state.scroll_x = 5;

    render(&mut state);

    // Eight columns fit in twelve; nothing should stay scrolled off.
    assert_eq!(state.viewport_state.scroll_x, 0);
}

#[test]
fn load_file_sets_the_path_and_resets_the_view() {
    let mut state = EditorState::new();
    state.dirty = true;
    state.viewport_state.scroll_x = 3;
    state.viewport_state.scroll_y = 2;
    state.viewport_state.curr_line = 1;
    state.scrollbar_state = state.scrollbar_state.position(4);
    state.h_scrollbar_state = state.h_scrollbar_state.position(5);
    state.cursor_screen_pos = Some(ratatui::layout::Position::new(1, 1));

    let path = PathBuf::from("notes.txt");
    state.load_file(path.clone(), vec![String::from("one"), String::from("two")]);

    assert_eq!(state.path, Some(path));
    assert!(!state.dirty);
    assert_eq!(
        state.text.lines,
        vec![String::from("one"), String::from("two")]
    );
    assert_eq!(state.viewport_state.scroll_x, 0);
    assert_eq!(state.viewport_state.scroll_y, 0);
    assert_eq!(state.viewport_state.curr_line, 0);
    assert_eq!(state.scrollbar_state.get_position(), 0);
    assert_eq!(state.h_scrollbar_state.get_position(), 0);
    assert_eq!(state.cursor_screen_pos, None);
}

#[test]
fn the_scrollbar_appears_on_the_first_render_without_interaction() {
    let mut state = EditorState::new();
    // A long document that overflows the viewport, loaded without any key
    // press. Before the render-time sync, `scrollbar_state` was left at its
    // default (`content_length` 0), so the thumb never drew until a key
    // forced `sync()` to run.
    state.load_file(
        PathBuf::from("long.txt"),
        (0..40).map(|i| format!("line {i}")).collect(),
    );

    let buf = render_in(&mut state, 64, 12);

    // The vertical scrollbar thumb must be drawn on the very first frame,
    // with no key press having forced `sync()` first.
    assert!(
        buf.content().iter().any(|cell| cell.symbol() == "█"),
        "vertical scrollbar thumb missing on first render"
    );
}

#[test]
fn typing_marks_the_buffer_dirty() {
    let mut state = EditorState::new();
    assert!(!state.dirty);

    run(&[KeyCode::Char('a')], &mut state);

    assert!(state.dirty);
}

#[test]
fn deleting_and_enter_also_mark_the_buffer_dirty() {
    let cases = [
        vec![KeyCode::Char('a'), KeyCode::Backspace],
        vec![KeyCode::Char('a'), KeyCode::Left, KeyCode::Delete],
        vec![KeyCode::Char('a'), KeyCode::Enter],
    ];

    for keys in cases {
        let mut state = EditorState::new();
        run(&keys, &mut state);
        assert!(state.dirty, "{keys:?} should have marked the buffer dirty");
    }
}

#[test]
fn movement_does_not_mark_the_buffer_dirty() {
    let mut state = EditorState::new();
    run(
        &[
            KeyCode::Left,
            KeyCode::Right,
            KeyCode::Up,
            KeyCode::Down,
            KeyCode::Home,
            KeyCode::End,
            KeyCode::PageUp,
            KeyCode::PageDown,
        ],
        &mut state,
    );

    assert!(!state.dirty);
}

#[test]
fn mark_saved_clears_the_dirty_flag() {
    let mut state = EditorState::new();
    run(&[KeyCode::Char('a')], &mut state);

    state.mark_saved();

    assert!(!state.dirty);
}

/// Presses `code` with `modifiers` so selection tests can use Shift etc.
fn press(code: KeyCode, modifiers: KeyModifiers, state: &mut EditorState) {
    state.handle_key(code, modifiers);
}

#[test]
fn shift_arrow_makes_a_selection_that_cut_removes() {
    let mut state = EditorState::new();
    run(
        &[KeyCode::Char('a'), KeyCode::Char('b'), KeyCode::Char('c')],
        &mut state,
    );
    // Cursor sits after "abc"; extending left by one char selects 'c'.
    press(KeyCode::Left, KeyModifiers::SHIFT, &mut state);
    // Cutting the selection removes 'c'.
    press(KeyCode::Char('x'), KeyModifiers::CONTROL, &mut state);
    assert_eq!(state.text.lines, vec![String::from("ab")]);
    assert!(state.dirty);
}

#[test]
fn plain_move_after_selection_collapses_it() {
    let mut state = EditorState::new();
    run(&[KeyCode::Char('a'), KeyCode::Char('b')], &mut state);
    press(KeyCode::Left, KeyModifiers::SHIFT, &mut state);
    // A plain arrow collapses the selection without editing the buffer.
    press(KeyCode::Right, KeyModifiers::NONE, &mut state);
    // Typing now inserts, not replaces.
    run(&[KeyCode::Char('x')], &mut state);
    assert_eq!(state.text.lines, vec![String::from("abx")]);
}

#[test]
fn select_all_then_type_replaces_the_whole_buffer() {
    let mut state = EditorState::new();
    state.text.lines = vec![String::from("one"), String::from("two")];
    state.text.cursor = Cursor { x: 0, y: 0 };
    press(KeyCode::Char('a'), KeyModifiers::CONTROL, &mut state);
    run(&[KeyCode::Char('z')], &mut state);
    assert_eq!(state.text.lines, vec![String::from("z")]);
}

#[test]
fn ctrl_v_pastes_what_was_copied() {
    let mut state = EditorState::new();
    run(&[KeyCode::Char('a'), KeyCode::Char('b')], &mut state);
    // Select the whole line with Shift+Home (char selection, no trailing \n).
    press(KeyCode::Home, KeyModifiers::SHIFT, &mut state);
    press(KeyCode::Char('c'), KeyModifiers::CONTROL, &mut state);
    // Collapse the selection (plain End) so the next Enter appends a line
    // instead of replacing the still-highlighted text.
    run(&[KeyCode::End], &mut state);
    run(&[KeyCode::Enter], &mut state);
    press(KeyCode::Char('v'), KeyModifiers::CONTROL, &mut state);
    assert_eq!(
        state.text.lines,
        vec![String::from("ab"), String::from("ab")]
    );
}

#[test]
fn external_paste_inserts_at_the_cursor() {
    let mut state = EditorState::new();
    run(&[KeyCode::Char('a'), KeyCode::Char('b')], &mut state);
    run(&[KeyCode::Home], &mut state);
    Component::handle_event(Editor, &Event::Paste("XY".into()), &mut state);
    assert_eq!(state.text.lines, vec![String::from("XYab")]);
}

#[test]
fn load_file_clears_any_selection() {
    let mut state = EditorState::new();
    run(&[KeyCode::Char('a'), KeyCode::Char('b')], &mut state);
    press(KeyCode::Left, KeyModifiers::SHIFT, &mut state);
    state.load_file(PathBuf::from("fresh.txt"), vec![String::from("hi")]);
    // No selection remains; typing inserts rather than replaces.
    run(&[KeyCode::Char('!')], &mut state);
    assert_eq!(state.text.lines, vec![String::from("!hi")]);
}

// ----- Undo / redo ----------------------------------------------------

/// Ctrl+Z / Ctrl+Y / Ctrl+Shift+Z, the three bindings every editor answers to.
const UNDO: (KeyCode, KeyModifiers) = (KeyCode::Char('z'), KeyModifiers::CONTROL);
const REDO: (KeyCode, KeyModifiers) = (KeyCode::Char('y'), KeyModifiers::CONTROL);

fn editor_with(lines: &[&str]) -> EditorState {
    let mut state = EditorState::new();
    state.load_file(
        PathBuf::from("notes.txt"),
        lines.iter().map(|l| l.to_string()).collect(),
    );
    state
}

/// Types every key in `codes` with no modifiers.
fn hit(key: (KeyCode, KeyModifiers), state: &mut EditorState) {
    press(key.0, key.1, state);
}

/// `Cursor` compares by its parts; every other test here does the same.
fn pos(cursor: Cursor) -> (usize, usize) {
    (cursor.x, cursor.y)
}

#[test]
fn undo_steps_back_over_a_whole_typing_run() {
    let mut state = EditorState::new();
    run(
        &[KeyCode::Char('a'), KeyCode::Char('b'), KeyCode::Char('c')],
        &mut state,
    );
    assert_eq!(state.text.lines, vec![String::from("abc")]);

    // Coalesced: one Ctrl+Z takes the whole burst, not one letter.
    hit(UNDO, &mut state);
    assert_eq!(state.text.lines, vec![String::new()]);
    assert_eq!(pos(state.text.cursor), (0, 0));

    hit(REDO, &mut state);
    assert_eq!(state.text.lines, vec![String::from("abc")]);
    assert_eq!(pos(state.text.cursor), (3, 0));
}

#[test]
fn ctrl_shift_z_also_redoes() {
    let mut state = EditorState::new();
    run(&[KeyCode::Char('x')], &mut state);
    hit(UNDO, &mut state);
    assert_eq!(state.text.lines, vec![String::new()]);

    press(
        KeyCode::Char('z'),
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        &mut state,
    );
    assert_eq!(state.text.lines, vec![String::from("x")]);
}

#[test]
fn typing_after_undo_discards_the_redo_chain() {
    let mut state = EditorState::new();
    run(&[KeyCode::Char('a'), KeyCode::Char('b')], &mut state);
    hit(UNDO, &mut state);
    hit((KeyCode::Char('z'), KeyModifiers::NONE), &mut state);

    // The one remaining edit was typed after the undo, so the discarded
    // branch cannot be resurrected.
    hit(REDO, &mut state);
    assert_eq!(state.text.lines, vec![String::from("z")]);
}

#[test]
fn enter_ends_a_typing_run_but_replaces_a_selection_in_one_step() {
    let mut state = editor_with(&["ab"]);
    press(KeyCode::End, KeyModifiers::NONE, &mut state);
    run(&[KeyCode::Enter, KeyCode::Char('c')], &mut state);
    assert_eq!(
        state.text.lines,
        vec![String::from("ab"), String::from("c")]
    );

    hit(UNDO, &mut state);
    assert_eq!(state.text.lines, vec![String::from("ab"), String::new()]);
    hit(UNDO, &mut state);
    assert_eq!(state.text.lines, vec![String::from("ab")]);
    assert_eq!(pos(state.text.cursor), (2, 0));
}

#[test]
fn deleting_a_selection_and_typing_is_a_single_step() {
    let mut state = editor_with(&["abcd"]);
    // Select "abc" with char granularity, then replace it with 'X'.
    for _ in 0..3 {
        press(KeyCode::Right, KeyModifiers::SHIFT, &mut state);
    }
    hit((KeyCode::Char('X'), KeyModifiers::NONE), &mut state);
    assert_eq!(state.text.lines, vec![String::from("Xd")]);

    hit(UNDO, &mut state);
    assert_eq!(state.text.lines, vec![String::from("abcd")]);
}

#[test]
fn undo_rejoins_a_deleted_line() {
    let mut state = editor_with(&["one", "two"]);
    press(KeyCode::Down, KeyModifiers::NONE, &mut state);
    press(KeyCode::Home, KeyModifiers::NONE, &mut state);
    hit((KeyCode::Backspace, KeyModifiers::NONE), &mut state);
    assert_eq!(state.text.lines, vec![String::from("onetwo")]);

    hit(UNDO, &mut state);
    assert_eq!(
        state.text.lines,
        vec![String::from("one"), String::from("two")]
    );
    assert_eq!(pos(state.text.cursor), (0, 1));
}

#[test]
fn cut_a_whole_line_can_be_undone() {
    let mut state = editor_with(&["keep", "drop"]);
    press(KeyCode::Down, KeyModifiers::NONE, &mut state);
    press(KeyCode::Char('x'), KeyModifiers::CONTROL, &mut state);
    assert_eq!(state.text.lines, vec![String::from("keep")]);

    hit(UNDO, &mut state);
    assert_eq!(
        state.text.lines,
        vec![String::from("keep"), String::from("drop")]
    );
}

#[test]
fn an_external_paste_is_one_undo_step() {
    let mut state = editor_with(&["ab"]);
    // A loaded buffer starts at {0,0}: the paste lands in front of "ab".
    Component::handle_event(Editor, &Event::Paste("x\ny".into()), &mut state);
    assert_eq!(
        state.text.lines,
        vec![String::from("x"), String::from("yab")]
    );

    hit(UNDO, &mut state);
    assert_eq!(state.text.lines, vec![String::from("ab")]);
}

#[test]
fn undo_lands_back_on_the_saved_state_and_clears_dirty() {
    let mut state = editor_with(&["a"]);
    assert!(!state.dirty);

    hit((KeyCode::Char('b'), KeyModifiers::NONE), &mut state);
    assert!(state.dirty);
    state.mark_saved();
    assert!(!state.dirty);

    // End first: a keystroke far from the previous one starts its own entry,
    // so undoing lands exactly on what was written to disk.
    press(KeyCode::End, KeyModifiers::NONE, &mut state);
    hit((KeyCode::Char('c'), KeyModifiers::NONE), &mut state);
    assert_eq!(state.text.lines, vec![String::from("bac")]);
    assert!(state.dirty);

    hit(UNDO, &mut state);
    assert!(!state.dirty);
    assert_eq!(state.text.lines, vec![String::from("ba")]);

    hit(REDO, &mut state);
    assert!(state.dirty);
    assert_eq!(state.text.lines, vec![String::from("bac")]);
}

#[test]
fn a_char_folded_into_a_saved_run_keeps_the_buffer_dirty() {
    let mut state = editor_with(&["a"]);
    hit((KeyCode::Char('b'), KeyModifiers::NONE), &mut state);
    state.mark_saved();

    // 'c' coalesces into the run that was saved, so no position left in the
    // history holds the file's contents — including after undoing them both.
    hit((KeyCode::Char('c'), KeyModifiers::NONE), &mut state);
    assert_eq!(state.text.lines, vec![String::from("bca")]);
    assert!(state.dirty);

    hit(UNDO, &mut state);
    assert_eq!(state.text.lines, vec![String::from("a")]);
    assert!(state.dirty);
}

#[test]
fn undo_history_does_not_follow_across_files() {
    let mut state = editor_with(&["one"]);
    hit((KeyCode::Char('!'), KeyModifiers::NONE), &mut state);
    assert_eq!(state.text.lines, vec![String::from("!one")]);

    state.load_file(PathBuf::from("other.txt"), vec![String::from("two")]);
    hit(UNDO, &mut state);
    assert_eq!(state.text.lines, vec![String::from("two")]);
}

#[test]
fn undo_and_redo_at_the_ends_do_nothing() {
    let mut state = editor_with(&["a"]);
    // Nothing to undo yet.
    hit(UNDO, &mut state);
    assert_eq!(state.text.lines, vec![String::from("a")]);
    hit(REDO, &mut state);
    assert_eq!(state.text.lines, vec![String::from("a")]);

    // The cursor sits at {0,0} after loading, so this types "b" *in front of*
    // the existing "a".
    hit((KeyCode::Char('b'), KeyModifiers::NONE), &mut state);
    hit(UNDO, &mut state);
    // The redo chain is one deep; a second redo must not duplicate text.
    hit(REDO, &mut state);
    hit(REDO, &mut state);
    assert_eq!(state.text.lines, vec![String::from("ba")]);
}

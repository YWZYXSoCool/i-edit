use super::{Edit, Selection, SelectionMode, TextState};
use crate::Cursor;

/// `Cursor` compares by its parts; every other test in the crate does the same.
fn pos(cursor: Cursor) -> (usize, usize) {
    (cursor.x, cursor.y)
}

// ----- Edit: the undo unit -------------------------------------------

/// Undoes `f` immediately and then redoes it, checking the buffer and cursor
/// come back exactly where they were before and after the edit.
fn round_trip<F>(state: &mut TextState, f: F)
where
    F: Fn(&mut TextState) -> Edit,
{
    let before_lines = state.lines.clone();
    let before = pos(state.cursor);

    let edit = f(state);
    let after_lines = state.lines.clone();
    let after = pos(state.cursor);

    edit.undo(state);
    assert_eq!(
        state.lines, before_lines,
        "undo did not restore the original buffer"
    );
    assert_eq!(
        pos(state.cursor),
        before,
        "undo did not restore the original cursor"
    );

    edit.redo(state);
    assert_eq!(state.lines, after_lines, "redo did not reapply the edit");
    assert_eq!(pos(state.cursor), after, "redo did not restore the cursor");
}

#[test]
fn insert_char_round_trips() {
    let mut state = TextState::default();
    state.load(vec![String::from("ab")]);
    state.cursor = Cursor { x: 1, y: 0 };

    round_trip(&mut state, |s| s.insert_char('X'));
}

#[test]
fn a_multibyte_insert_round_trips() {
    let mut state = TextState::default();
    state.load(vec![String::from("你好")]);
    state.cursor = Cursor { x: 6, y: 0 };

    round_trip(&mut state, |s| s.insert_char('中'));
    assert_eq!(state.lines, vec![String::from("你好中")]);
}

#[test]
fn a_multiline_paste_round_trips() {
    let mut state = TextState::default();
    state.load(vec![String::from("ab"), String::from("cd")]);
    state.cursor = Cursor { x: 1, y: 0 };

    round_trip(&mut state, |s| s.insert_str("X\nY"));
}

#[test]
fn a_selection_delete_round_trips() {
    let mut state = TextState::default();
    state.load(vec![
        String::from("one"),
        String::from("two"),
        String::from("three"),
    ]);
    let selection = Selection {
        anchor: Cursor { x: 1, y: 0 },
        mode: SelectionMode::Char,
    };
    state.cursor = Cursor { x: 2, y: 1 };

    round_trip(&mut state, |s| s.delete_selection(&selection));
}

#[test]
fn a_line_selection_delete_round_trips() {
    let mut state = TextState::default();
    state.load(vec![
        String::from("one"),
        String::from("two"),
        String::from("three"),
    ]);
    let selection = Selection {
        anchor: Cursor { x: 0, y: 1 },
        mode: SelectionMode::Line,
    };
    state.cursor = Cursor { x: 0, y: 2 };

    round_trip(&mut state, |s| s.delete_selection(&selection));
}

#[test]
fn consecutive_typing_coalesces_and_multibyte_counts_as_one_key() {
    let mut state = TextState::default();
    state.load(vec![String::from("中")]);

    let mut run = state.insert_char('文');
    let second = state.insert_char('字');
    assert!(run.try_absorb(&second), "CJK typing should coalesce");

    // The whole run is one undo step, so reverting clears both characters.
    run.undo(&mut state);
    assert_eq!(state.lines, vec![String::from("中")]);
}

#[test]
fn a_line_split_never_coalesces_with_the_run_before_it() {
    let mut state = TextState::default();
    state.load(vec![String::from("ab")]);
    state.cursor = Cursor { x: 1, y: 0 };

    let mut newline = state.insert_new_line();
    let next = state.insert_char('c');

    assert!(!newline.try_absorb(&next));
}

#[test]
fn load_replaces_the_buffer_and_resets_the_cursor() {
    let mut state = TextState {
        cursor: Cursor { x: 3, y: 2 },
        ..Default::default()
    };

    state.load(vec![String::from("one"), String::from("two")]);

    assert_eq!(state.lines, vec![String::from("one"), String::from("two")]);
    assert_eq!((state.cursor.x, state.cursor.y), (0, 0));
}

#[test]
fn loading_an_empty_file_keeps_one_empty_line() {
    let mut state = TextState::default();

    state.load(Vec::new());

    assert_eq!(state.lines, vec![String::new()]);
    assert_eq!((state.cursor.x, state.cursor.y), (0, 0));
}

#[test]
fn load_leaves_no_cursor_past_the_new_content() {
    // A position that cannot exist in the shorter buffer loaded below.
    let mut state = TextState {
        cursor: Cursor { x: 40, y: 7 },
        ..Default::default()
    };

    state.load(vec![String::from("tiny")]);

    assert_eq!((state.cursor.x, state.cursor.y), (0, 0));
    assert_eq!(state.display_col(), 0);
}

#[test]
fn selected_text_char_single_line() {
    let mut state = TextState::default();
    state.lines = vec![String::from("hello")];
    state.cursor = Cursor { x: 5, y: 0 };
    let mut sel = Selection::default();
    sel.anchor = Cursor { x: 1, y: 0 };
    sel.mode = SelectionMode::Char;
    assert_eq!(state.selected_text(&sel), "ello");
}

#[test]
fn selected_text_char_anchor_after_cursor() {
    let mut state = TextState::default();
    state.lines = vec![String::from("hello")];
    state.cursor = Cursor { x: 1, y: 0 };
    let mut sel = Selection::default();
    sel.anchor = Cursor { x: 5, y: 0 };
    sel.mode = SelectionMode::Char;
    // Ordered so the anchor being "ahead" still yields the same span.
    assert_eq!(state.selected_text(&sel), "ello");
}

#[test]
fn selected_text_multiline_char() {
    let mut state = TextState::default();
    state.lines = vec![String::from("abc"), String::from("de"), String::from("f")];
    state.cursor = Cursor { x: 1, y: 2 };
    let mut sel = Selection::default();
    sel.anchor = Cursor { x: 1, y: 0 };
    sel.mode = SelectionMode::Char;
    assert_eq!(state.selected_text(&sel), "bc\nde\nf");
}

#[test]
fn selected_text_line_mode_full_rows() {
    let mut state = TextState::default();
    state.lines = vec![String::from("abc"), String::from("de"), String::from("f")];
    state.cursor = Cursor { x: 0, y: 2 };
    let mut sel = Selection::default();
    sel.anchor = Cursor { x: 0, y: 0 };
    sel.mode = SelectionMode::Line;
    assert_eq!(state.selected_text(&sel), "abc\nde\nf\n");
}

#[test]
fn delete_selection_single_line() {
    let mut state = TextState::default();
    state.lines = vec![String::from("hello")];
    state.cursor = Cursor { x: 5, y: 0 };
    let mut sel = Selection::default();
    sel.anchor = Cursor { x: 1, y: 0 };
    sel.mode = SelectionMode::Char;
    state.delete_selection(&sel);
    assert_eq!(state.lines, vec![String::from("h")]);
    assert_eq!((state.cursor.x, state.cursor.y), (1, 0));
}

#[test]
fn delete_selection_multiline() {
    let mut state = TextState::default();
    state.lines = vec![String::from("abc"), String::from("de"), String::from("f")];
    state.cursor = Cursor { x: 1, y: 2 };
    let mut sel = Selection::default();
    sel.anchor = Cursor { x: 1, y: 0 };
    sel.mode = SelectionMode::Char;
    state.delete_selection(&sel);
    assert_eq!(state.lines, vec![String::from("a")]);
    assert_eq!((state.cursor.x, state.cursor.y), (1, 0));
}

#[test]
fn insert_str_multiline() {
    let mut state = TextState::default();
    state.lines = vec![String::from("ab")];
    state.cursor = Cursor { x: 1, y: 0 };
    state.insert_str("X\nY");
    assert_eq!(state.lines, vec![String::from("aX"), String::from("Yb")]);
    assert_eq!((state.cursor.x, state.cursor.y), (1, 1));
}

#[test]
fn insert_str_empty_is_noop() {
    let mut state = TextState::default();
    state.lines = vec![String::from("ab")];
    state.insert_str("");
    assert_eq!(state.lines, vec![String::from("ab")]);
    assert_eq!((state.cursor.x, state.cursor.y), (0, 0));
}

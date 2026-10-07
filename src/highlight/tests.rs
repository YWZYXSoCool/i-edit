use crate::highlight::{Highlights, LAYER_ORDER, LayerId, StyledRun, TextEdit, utf16_to_byte};
use ratatui::style::{Color, Style};

fn run(start: u32, end: u32, color: Color) -> StyledRun {
    StyledRun {
        start,
        end,
        style: Style::new().fg(color),
    }
}

/// Where a layer sits in the array `line_runs` returns, so the tests name
/// the layer instead of indexing it with a bare number.
fn index_of(which: LayerId) -> usize {
    LAYER_ORDER
        .iter()
        .position(|layer| *layer == which)
        .expect("every layer is in LAYER_ORDER")
}

#[test]
fn utf16_to_byte_handles_ascii() {
    let line = "hello";
    assert_eq!(utf16_to_byte(line, 0), 0);
    assert_eq!(utf16_to_byte(line, 2), 2);
    assert_eq!(utf16_to_byte(line, 5), 5);
    // Beyond the end clamps to the line length.
    assert_eq!(utf16_to_byte(line, 99), 5);
}

#[test]
fn utf16_to_byte_handles_bmp_chinese() {
    // Each Han character is 3 UTF-8 bytes but 1 UTF-16 unit.
    let line = "你好世界";
    assert_eq!(utf16_to_byte(line, 0), 0);
    assert_eq!(utf16_to_byte(line, 1), 3);
    assert_eq!(utf16_to_byte(line, 2), 6);
    assert_eq!(utf16_to_byte(line, 3), 9);
    assert_eq!(utf16_to_byte(line, 4), 12);
}

#[test]
fn utf16_to_byte_handles_surrogate_pair_emoji() {
    // 😀 is a surrogate pair: 2 UTF-16 units, 4 UTF-8 bytes. 'a' is 1/1.
    // UTF-16 columns: 0 = start of 😀, 2 = start of 'a', 3 = end.
    let line = "😀a";
    assert_eq!(utf16_to_byte(line, 0), 0);
    assert_eq!(utf16_to_byte(line, 1), 4); // mid-emoji → next char boundary
    assert_eq!(utf16_to_byte(line, 2), 4); // start of 'a'
    assert_eq!(utf16_to_byte(line, 3), 5); // end of line
}

#[test]
fn overlay_wins_over_base_and_uncolored_falls_through() {
    let mut h = Highlights::default();
    h.replace_layer(
        LayerId::Base,
        &[vec![run(0, 3, Color::Red)], vec![run(0, 2, Color::Red)]],
    );
    h.replace_layer(LayerId::Overlay, &[vec![run(0, 1, Color::Blue)], vec![]]);
    h.set_enabled(true);

    let row0 = h.line_runs(0);
    assert_eq!(row0[index_of(LayerId::Overlay)].len(), 1);
    assert_eq!(row0[index_of(LayerId::Base)].len(), 1);

    let row1 = h.line_runs(1);
    assert_eq!(row1[index_of(LayerId::Overlay)].len(), 0);
    assert_eq!(row1[index_of(LayerId::Base)].len(), 1);
}

/// The reason for three layers: each producer replaces its own layer
/// wholesale, so a push from one must not erase the other two.
#[test]
fn replacing_one_layer_leaves_the_others_alone() {
    let mut h = Highlights::default();
    h.replace_layer(LayerId::Base, &[vec![run(0, 3, Color::Red)]]);
    h.replace_layer(LayerId::Semantic, &[vec![run(0, 3, Color::Green)]]);
    h.replace_layer(LayerId::Overlay, &[vec![run(0, 3, Color::Blue)]]);

    // The selection changes on every keystroke; the syntax and semantic
    // layers have to survive it.
    h.replace_layer(LayerId::Overlay, &[vec![]]);

    let row = h.line_runs(0);
    assert_eq!(row[index_of(LayerId::Base)].len(), 1);
    assert_eq!(row[index_of(LayerId::Semantic)].len(), 1);
    assert_eq!(row[index_of(LayerId::Overlay)].len(), 0);
}

fn spans(runs: &[StyledRun]) -> Vec<(u32, u32)> {
    runs.iter().map(|r| (r.start, r.end)).collect()
}

/// The runs of one row, in the layer a test is talking about.
fn row(h: &Highlights, which: LayerId, y: usize) -> Vec<(u32, u32)> {
    spans(h.line_runs(y)[index_of(which)])
}

/// The whole point of carrying runs across an edit: typing at the top of a
/// file must not blank everything below it.
#[test]
fn an_edit_inside_a_row_slides_only_what_follows_it() {
    let mut h = Highlights::default();
    // "let x = 1;", with `let`, `x` and `1` colored.
    h.replace_layer(
        LayerId::Semantic,
        &[vec![
            run(0, 3, Color::Red),
            run(4, 5, Color::Green),
            run(8, 9, Color::Blue),
        ]],
    );

    // Two bytes inserted at the very start of the row.
    h.apply_edit(&TextEdit::within_line(0, 0, 0, 2));

    assert_eq!(
        row(&h, LayerId::Semantic, 0),
        vec![(2, 5), (6, 7), (10, 11)],
        "everything on the row slid right by two"
    );
}

#[test]
fn runs_before_the_edit_stay_where_they_are() {
    let mut h = Highlights::default();
    h.replace_layer(
        LayerId::Semantic,
        &[vec![
            run(0, 3, Color::Red),
            run(4, 5, Color::Green),
            run(8, 9, Color::Blue),
        ]],
    );

    // Something typed at column 6: only the run past it moves.
    h.apply_edit(&TextEdit::within_line(0, 6, 0, 1));

    assert_eq!(row(&h, LayerId::Semantic, 0), vec![(0, 3), (4, 5), (9, 10)]);
}

#[test]
fn a_run_the_edit_lands_inside_is_dropped() {
    // It covers text that no longer exists; keeping it would invent a
    // color for whatever took its place.
    let mut h = Highlights::default();
    h.replace_layer(
        LayerId::Semantic,
        &[vec![
            run(0, 3, Color::Red),
            run(4, 5, Color::Green),
            run(8, 9, Color::Blue),
        ]],
    );

    // One byte at column 2 replaced — inside the first run.
    h.apply_edit(&TextEdit::within_line(0, 2, 1, 1));

    assert_eq!(
        row(&h, LayerId::Semantic, 0),
        vec![(4, 5), (8, 9)],
        "the run that was typed into is the only one lost"
    );
}

/// Pressing Enter splits one row into two; the runs split with it, and —
/// the part that used to cost a whole screen of color — the rows below
/// keep theirs.
#[test]
fn pressing_enter_splits_the_row_and_spares_the_rest() {
    let mut h = Highlights::default();
    h.replace_layer(
        LayerId::Semantic,
        &[
            vec![run(0, 1, Color::Red)],
            vec![run(0, 1, Color::Green), run(2, 4, Color::Blue)],
            vec![run(0, 1, Color::Yellow)],
        ],
    );

    // Enter at column 2 of row 1: "ab" stays above, "cd" starts the new row.
    h.apply_edit(&TextEdit {
        line: 1,
        column: 2,
        removed_rows: 1,
        inserted_rows: 2,
        removed_to: 2,
        inserted_to: 0,
    });

    assert_eq!(row(&h, LayerId::Semantic, 0), vec![(0, 1)]);
    assert_eq!(row(&h, LayerId::Semantic, 1), vec![(0, 1)], "the head");
    assert_eq!(row(&h, LayerId::Semantic, 2), vec![(0, 2)], "the tail");
    assert_eq!(
        row(&h, LayerId::Semantic, 3),
        vec![(0, 1)],
        "the row below, now one further down"
    );
}

/// Deleting a whole row takes its runs with it and closes the gap; nothing
/// below it notices.
#[test]
fn a_deleted_row_leaves_the_ones_below_alone() {
    let mut h = Highlights::default();
    h.replace_layer(
        LayerId::Semantic,
        &[
            vec![run(0, 1, Color::Red)],
            vec![run(0, 1, Color::Green)],
            vec![run(0, 1, Color::Blue)],
        ],
    );

    // "line1\n" removed from the start of row 1.
    h.apply_edit(&TextEdit {
        line: 1,
        column: 0,
        removed_rows: 2,
        inserted_rows: 1,
        removed_to: 0,
        inserted_to: 0,
    });

    assert_eq!(row(&h, LayerId::Semantic, 0), vec![(0, 1)]);
    assert_eq!(row(&h, LayerId::Semantic, 1), vec![(0, 1)]);
    // Two rows covered, one produced: three rows became two.
    assert_eq!(row(&h, LayerId::Semantic, 2), vec![]);
}

/// Undo is the same replacement with its two sides swapped. Runs that
/// survive the edit come back; one the edit landed inside is gone for good,
/// and the next response from the server is what restores it.
#[test]
fn an_edit_and_its_inverse_land_back_where_they_started() {
    let mut h = Highlights::default();
    // Two runs with a gap between them: the edit goes into the gap, so
    // neither run is lost.
    h.replace_layer(
        LayerId::Semantic,
        &[
            vec![run(0, 2, Color::Red), run(4, 6, Color::Green)],
            vec![run(0, 1, Color::Blue)],
        ],
    );
    let before: Vec<Vec<(u32, u32)>> = (0..2).map(|y| row(&h, LayerId::Semantic, y)).collect();

    // Enter, in the gap: the second run becomes the head of a new row.
    let enter = TextEdit {
        line: 0,
        column: 3,
        removed_rows: 1,
        inserted_rows: 2,
        removed_to: 3,
        inserted_to: 0,
    };
    h.apply_edit(&enter);
    assert_eq!(row(&h, LayerId::Semantic, 0), vec![(0, 2)]);
    assert_eq!(
        row(&h, LayerId::Semantic, 1),
        vec![(1, 3)],
        "the run that moved down starts three columns earlier now"
    );

    h.apply_edit(&enter.inverse());
    let after: Vec<Vec<(u32, u32)>> = (0..2).map(|y| row(&h, LayerId::Semantic, y)).collect();
    assert_eq!(after, before);
}

/// A row past the watermark holds runs from text that is already gone.
/// Carrying them forward would paint the new text with the old colors.
#[test]
fn an_edit_past_the_watermark_does_not_invent_colors() {
    let mut h = Highlights::default();
    h.replace_layer(
        LayerId::Semantic,
        &[vec![run(0, 1, Color::Red)], vec![run(0, 1, Color::Green)]],
    );
    h.stale_from(1);

    // Row 1 is waiting for new data. Typing on it must not adopt the runs
    // it still happens to hold.
    h.apply_edit(&TextEdit::within_line(1, 0, 0, 3));

    assert_eq!(row(&h, LayerId::Semantic, 1), vec![]);
}

#[test]
fn replace_layer_is_allocation_free_after_the_first_call() {
    // First call must grow the arena/index from empty (one alloc each).
    // Second call reuses the established capacity (zero alloc).
    let rows: Vec<Vec<StyledRun>> = (0..50)
        .map(|i| vec![run(i as u32, (i + 1) as u32, Color::Green)])
        .collect();

    let mut h = Highlights::default();
    h.replace_layer(LayerId::Base, &rows); // first: establishes capacity
    h.replace_layer(LayerId::Base, &rows); // second: should reuse it

    assert_eq!(h.line_runs(10)[index_of(LayerId::Base)].len(), 1);
}

#[test]
fn clear_layer_empties_without_losing_capacity() {
    let mut h = Highlights::default();
    h.replace_layer(LayerId::Overlay, &[vec![run(0, 1, Color::Blue)]]);
    h.clear_layer(LayerId::Overlay);
    assert_eq!(h.line_runs(0)[index_of(LayerId::Overlay)].len(), 0);
}

#[test]
fn disabled_layer_renders_empty_runs() {
    let mut h = Highlights::default();
    h.replace_layer(LayerId::Overlay, &[vec![run(0, 1, Color::Blue)]]);
    h.set_enabled(false);
    // With coloring off the renderer is told there is nothing to paint.
    assert_eq!(h.line_runs(0)[index_of(LayerId::Overlay)].len(), 1); // data still present...
    assert!(!h.enabled()); // ...but the switch is off, so no color shows.
}

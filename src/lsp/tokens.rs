//! Decoding the `semanticTokens` stream into per-row runs.
//!
//! The wire format is a flat `Vec<u32>`, five numbers per token, and it is
//! *relative*: each token is an offset from the one before it. That saves bytes
//! and costs correctness if the rule is misread, and there is one rule that
//! bites:
//!
//! ```text
//! deltaLine != 0  →  deltaStartChar is from the start of the line
//! deltaLine == 0  →  deltaStartChar is from the start of the previous token
//! ```
//!
//! Treating the second case as if it were the first puts the first token of
//! each line in the right place and slides every token after it to the right.
//! The result is not "no colors", it is plausible-looking wrong colors, which
//! is why there is a test for it.

use crate::highlight::{StyledRun, utf16_to_byte};
use crate::lsp::theme::StyleTable;

/// Cap on runs per row. A pathological line (thousands of tokens) would
/// otherwise cost memory and per-frame walking for no visible gain; past the
/// cap the rest of the row simply stays plain. The text itself is never
/// affected.
const MAX_RUNS_PER_LINE: usize = 512;

/// Numbers per token on the wire: `deltaLine`, `deltaStartChar`, `length`,
/// `tokenType`, `tokenModifiers`.
const FIELDS_PER_TOKEN: usize = 5;

/// Fills `rows` — one entry per line of `lines` — with the decoded tokens.
///
/// `rows` is resized in place and its inner buffers are cleared rather than
/// replaced, so a steady stream of responses settles into zero allocations.
pub fn decode(data: &[u32], lines: &[String], table: &StyleTable, rows: &mut Vec<Vec<StyledRun>>) {
    // Every row exists even when empty: the coloring layer indexes rows by
    // absolute line number, so a missing row would shift all the ones below.
    rows.resize(lines.len(), Vec::new());
    for row in rows.iter_mut() {
        row.clear();
    }

    let mut line = 0usize;
    let mut start16 = 0usize;

    // Whole groups only: a trailing partial group is not a token, and silently
    // dropping it keeps one malformed response from shifting every later one.
    let (tokens, _partial) = data.as_chunks::<FIELDS_PER_TOKEN>();

    for token in tokens {
        let [delta_line, delta_start, length, token_type, modifiers] = *token;

        line += delta_line as usize;
        start16 = if delta_line == 0 {
            start16 + delta_start as usize
        } else {
            delta_start as usize
        };

        // A zero-length token (the server uses them for e.g. injected code
        // boundaries) covers nothing; keeping it would produce a run that can
        // never be entered.
        if length == 0 {
            continue;
        }

        // The server may be describing a version of the file with more lines
        // than the buffer holds. Stop instead of indexing past the end.
        if line >= lines.len() {
            break;
        }

        if rows[line].len() >= MAX_RUNS_PER_LINE {
            continue;
        }

        let text = &lines[line];
        let start = utf16_to_byte(text, start16);
        let end = utf16_to_byte(text, start16 + length as usize);
        if start >= end {
            continue;
        }

        rows[line].push(StyledRun {
            start: start as u32,
            end: end as u32,
            style: table.style(token_type, modifiers),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::{MAX_RUNS_PER_LINE, decode};
    use crate::highlight::StyledRun;
    use crate::lsp::protocol::{REAL_LEGEND, legend_for_test};
    use crate::lsp::theme::StyleTable;
    use ratatui::style::{Color, Style};

    /// A table whose token type *n* is whatever the legend names, so tests can
    /// tell tokens apart without depending on the real palette.
    fn table(types: &[&str]) -> StyleTable {
        StyleTable::build(&legend_for_test(types, &[]))
    }

    fn lines(text: &str) -> Vec<String> {
        text.split('\n').map(str::to_string).collect()
    }

    fn spans(row: &[StyledRun]) -> Vec<(u32, u32)> {
        row.iter().map(|r| (r.start, r.end)).collect()
    }

    #[test]
    fn one_span_per_row_for_every_line() {
        let lines = lines("a\nb\nc");
        let mut rows: Vec<Vec<StyledRun>> = Vec::new();
        decode(&[], &lines, &table(&[]), &mut rows);

        assert_eq!(rows.len(), 3);
        assert!(rows.iter().all(|r| r.is_empty()));
    }

    #[test]
    fn tokens_on_one_line_are_relative_to_the_previous_token() {
        // "let x = 1;" — `let` [0,3), `x` [4,5), `1` [8,9)
        let lines = lines("let x = 1;");
        let mut rows = Vec::new();
        let table = table(&["keyword", "variable", "number"]);

        decode(
            &[
                0, 0, 3, 0, 0, // let
                0, 4, 1, 1, 0, // x: 0 + 4 = 4
                0, 4, 1, 2, 0, // 1: 4 + 4 = 8
            ],
            &lines,
            &table,
            &mut rows,
        );

        assert_eq!(spans(&rows[0]), vec![(0, 3), (4, 5), (8, 9)]);
        assert_eq!(rows[0][0].style.fg, Some(Color::LightMagenta)); // keyword
        assert_eq!(rows[0][1].style.fg, None); // variable → plain
        assert_eq!(rows[0][2].style.fg, Some(Color::LightYellow)); // number
    }

    #[test]
    fn a_new_line_resets_the_column_to_the_line_start() {
        // The case that breaks if deltaStart is always read as relative.
        let lines = lines("aa\nbb");
        let mut rows = Vec::new();
        let table = table(&["variable"]);

        decode(
            &[
                0, 0, 2, 0, 0, // line 0: [0,2)
                1, 1, 1, 0, 0, // line 1: delta != 0, so start = 1, not 2+1
            ],
            &lines,
            &table,
            &mut rows,
        );

        assert_eq!(spans(&rows[0]), vec![(0, 2)]);
        assert_eq!(spans(&rows[1]), vec![(1, 2)]);
    }

    #[test]
    fn a_token_at_column_zero_is_kept() {
        let lines = lines("abc");
        let mut rows = Vec::new();
        decode(&[0, 0, 3, 0, 0], &lines, &table(&["keyword"]), &mut rows);

        assert_eq!(spans(&rows[0]), vec![(0, 3)]);
    }

    #[test]
    fn zero_length_tokens_are_dropped() {
        let lines = lines("abc");
        let mut rows = Vec::new();
        decode(&[0, 0, 0, 0, 0], &lines, &table(&["keyword"]), &mut rows);

        assert!(rows[0].is_empty());
    }

    #[test]
    fn utf16_columns_are_converted_to_bytes() {
        // 你好 is 6 bytes but 2 UTF-16 units; token covers the second char.
        let lines = lines("你好世界");
        let mut rows = Vec::new();
        decode(&[0, 1, 1, 0, 0], &lines, &table(&["keyword"]), &mut rows);

        assert_eq!(spans(&rows[0]), vec![(3, 6)]);
    }

    #[test]
    fn a_token_past_the_end_of_the_file_is_ignored() {
        let lines = lines("a\nb");
        let mut rows = Vec::new();
        // Line 5 does not exist: stop rather than indexing past the end.
        decode(&[5, 0, 1, 0, 0], &lines, &table(&["keyword"]), &mut rows);

        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|r| r.is_empty()));
    }

    #[test]
    fn a_trailing_partial_group_is_ignored() {
        let lines = lines("abc");
        let mut rows = Vec::new();
        // Only 3 of the 5 numbers; the group is incomplete, not a token.
        decode(&[0, 0, 3], &lines, &table(&["keyword"]), &mut rows);

        assert!(rows[0].is_empty());
    }

    #[test]
    fn a_pathological_line_is_capped_without_losing_rows() {
        let lines = lines("a a a a");
        let mut rows = Vec::new();
        let mut data = Vec::new();
        for _ in 0..(MAX_RUNS_PER_LINE * 2) {
            data.extend_from_slice(&[0, 0, 1, 0, 0]);
        }

        decode(&data, &lines, &table(&["keyword"]), &mut rows);

        assert_eq!(rows[0].len(), MAX_RUNS_PER_LINE);
        assert_eq!(rows.len(), 1);
    }

    #[test]
    fn repeated_decodes_reuse_the_row_capacity() {
        let lines = lines("a\nb\nc");
        let mut rows: Vec<Vec<StyledRun>> = Vec::new();
        let table = table(&["keyword"]);

        decode(&[0, 0, 1, 0, 0], &lines, &table, &mut rows);
        let capacity = rows[0].capacity();

        for _ in 0..4 {
            decode(&[0, 0, 1, 0, 0], &lines, &table, &mut rows);
        }

        // No reallocation after the first pass: the buffer is reused as-is.
        assert_eq!(rows[0].capacity(), capacity);
        assert_eq!(spans(&rows[0]), vec![(0, 1)]);
    }

    #[test]
    fn rows_shrink_when_the_file_shrinks() {
        let mut rows: Vec<Vec<StyledRun>> = Vec::new();
        let table = table(&["keyword"]);

        decode(&[0, 0, 1, 0, 0], &lines("a\nb\nc"), &table, &mut rows);
        assert_eq!(rows.len(), 3);

        decode(&[0, 0, 1, 0, 0], &lines("a"), &table, &mut rows);
        assert_eq!(rows.len(), 1);
    }

    /// A real `semanticTokens/full` response, captured from rust-analyzer
    /// 1.99.0 for the first six lines of `src/main.rs`.
    ///
    /// This is the test the hand-written cases cannot replace: it is the only
    /// one where the data was produced by a server rather than by someone
    /// reading the spec, so it catches a misread of the wire format that the
    /// hand-written cases would repeat.
    #[test]
    fn decodes_a_real_rust_analyzer_response() {
        const DATA: &[u32] = &[
            0, 0, 3, 6, 0, // use
            0, 4, 3, 9, 0, // std
            0, 3, 2, 11, 0, // ::
            0, 2, 3, 9, 0, // env
            1, 0, 3, 6, 0, 0, 4, 3, 9, 0, 0, 3, 2, 11, 0, 0, 2, 2, 9, 0, 0, 2, 2, 11, 0, 0, 2, 6,
            9, 0, // line 1: use std::io::stdout
            2, 0, 3, 6, 0, 0, 4, 9, 9, 0, 0, 9, 2, 11, 0, 0, 2, 5, 9, 0, 0, 5, 2, 11, 0, 0, 2, 20,
            15, 0, // line 3: use crossterm::event::EnableBracketedPaste
            1, 0, 3, 6, 0, 0, 4, 9, 9, 0, 0, 9, 2, 11, 0, 0, 2, 7, 9,
            0, // line 4: use crossterm::execute
            1, 0, 3, 6, 0, 0, 4, 6, 9, 0, 0, 6, 2, 11, 0, 0, 2, 6, 15,
            0, // line 5: use i_edit::Result
        ];

        let table = StyleTable::build(&legend_for_test(REAL_LEGEND, &[]));
        let lines = lines(concat!(
            "use std::env;\n",
            "use std::io::stdout;\n",
            "\n",
            "use crossterm::event::EnableBracketedPaste;\n",
            "use crossterm::execute;\n",
            "use i_edit::Result;",
        ));

        let mut rows = Vec::new();
        decode(DATA, &lines, &table, &mut rows);

        // `use std::env;` — and the spans are byte offsets, so they slice the
        // source back out exactly.
        assert_eq!(spans(&rows[0]), vec![(0, 3), (4, 7), (7, 9), (9, 12)]);
        assert_eq!(&lines[0][0..3], "use");
        assert_eq!(&lines[0][4..7], "std");
        assert_eq!(&lines[0][7..9], "::");
        assert_eq!(&lines[0][9..12], "env");

        // keyword, namespace, operator, namespace.
        assert_eq!(rows[0][0].style.fg, Some(Color::LightMagenta));
        assert_eq!(rows[0][1].style.fg, Some(Color::LightCyan));
        assert_eq!(rows[0][2].style.fg, Some(Color::Gray));
        assert_eq!(rows[0][3].style.fg, Some(Color::LightCyan));

        // An empty line has nothing on it — and the row still exists.
        assert_eq!(rows.len(), 6);
        assert!(rows[2].is_empty());

        // `EnableBracketedPaste` is resolved as a struct.
        let last = rows[3].last().expect("tokens on line 3");
        assert_eq!(
            &lines[3][last.start as usize..last.end as usize],
            "EnableBracketedPaste"
        );
        assert_eq!(last.style.fg, Some(Color::LightCyan));
    }

    #[test]
    fn styles_come_from_the_type_and_the_modifier_bits() {
        let table = StyleTable::build(&legend_for_test(&["variable"], &["declaration"]));
        let lines = lines("a b");
        let mut rows = Vec::new();

        decode(
            &[
                0, 0, 1, 0, 1, // modifier bit 0 set → bold
                0, 2, 1, 0, 0,
            ],
            &lines,
            &table,
            &mut rows,
        );

        assert_eq!(
            rows[0][0].style,
            Style::new().add_modifier(ratatui::style::Modifier::BOLD)
        );
        assert_eq!(rows[0][1].style, Style::new());
    }
}

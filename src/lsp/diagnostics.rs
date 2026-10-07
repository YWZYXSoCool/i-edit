//! Turning `publishDiagnostics` into per-row underlines.
//!
//! Two things make this more than "convert a range to a run":
//!
//! - **A range may span lines.** The coloring model is per row — a run cannot
//!   straddle a newline the way a token cannot — so a multi-line range is cut
//!   into one run per line: the first starts at `startChar`, the last ends at
//!   `endChar`, and the ones between cover the whole line.
//! - **A range may be empty.** An error at the end of a line has
//!   `start == end`, and that is meaningful: it marks a position, not a span.
//!   The renderer already paints a zero-length run on the one cell at that
//!   position, so it is kept as-is rather than dropped.
//!
//! `severity` only ever becomes an underline, never a foreground: the code
//! under a diagnostic keeps its semantic color, and the underline rides on top.

use crate::highlight::{StyledRun, utf16_to_byte};
use crate::lsp::protocol::Diagnostic;
use crate::lsp::theme::style_for_severity;

/// Fills `rows` — one entry per line of `lines` — with one run per diagnostic.
///
/// `rows` is resized in place and its inner buffers cleared rather than
/// replaced, so a steady stream of notifications settles into zero allocations.
pub fn decode(items: &[Diagnostic], lines: &[String], rows: &mut Vec<Vec<StyledRun>>) {
    // Every row exists even when empty: the coloring layer indexes rows by
    // absolute line number, so a missing row would shift all the ones below.
    rows.resize(lines.len(), Vec::new());
    for row in rows.iter_mut() {
        row.clear();
    }

    for item in items {
        // A range that ends before it starts is a server bug; skipping it
        // keeps one bad item from looping over the whole file.
        if item.end_line < item.start_line {
            continue;
        }

        let style = style_for_severity(item.severity);
        // A range may run past the end of the buffer — the server is
        // describing the file it has, which is not always the file we have.
        let last = item.end_line.min(lines.len().saturating_sub(1));

        let covered = rows
            .iter_mut()
            .enumerate()
            .take(last + 1)
            .skip(item.start_line);

        for (line, row) in covered {
            let text = &lines[line];

            let start16 = if line == item.start_line {
                item.start_char
            } else {
                0
            };
            // Any line but the last is covered whole, and `utf16_to_byte`
            // clamps an out-of-range column to the line length.
            let end16 = if line == item.end_line {
                item.end_char
            } else {
                usize::MAX
            };

            let start = utf16_to_byte(text, start16);
            let end = utf16_to_byte(text, end16);
            if start > end {
                continue;
            }

            row.push(StyledRun {
                start: start as u32,
                end: end as u32,
                style,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::decode;
    use crate::highlight::StyledRun;
    use crate::lsp::protocol::{Diagnostic, Severity};
    use ratatui::style::{Color, Modifier, Style};

    fn diagnostic(
        start_line: usize,
        start_char: usize,
        end_line: usize,
        end_char: usize,
    ) -> Diagnostic {
        Diagnostic {
            start_line,
            end_line,
            start_char,
            end_char,
            severity: Some(Severity::Error),
        }
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
        decode(&[], &lines, &mut rows);

        assert_eq!(rows.len(), 3);
        assert!(rows.iter().all(|r| r.is_empty()));
    }

    #[test]
    fn a_range_becomes_one_run() {
        let lines = lines("let x = ;");
        let mut rows = Vec::new();
        decode(&[diagnostic(0, 4, 0, 5)], &lines, &mut rows);

        assert_eq!(spans(&rows[0]), vec![(4, 5)]);
    }

    #[test]
    fn a_multi_line_range_is_cut_per_line() {
        // Rows 0..2: first starts at 2, last ends at 3, the middle is whole.
        let lines = lines("abcdef\nghijkl\nmnopqr");
        let mut rows = Vec::new();
        decode(&[diagnostic(0, 2, 2, 3)], &lines, &mut rows);

        assert_eq!(spans(&rows[0]), vec![(2, 6)]);
        assert_eq!(spans(&rows[1]), vec![(0, 6)]);
        assert_eq!(spans(&rows[2]), vec![(0, 3)]);
    }

    #[test]
    fn an_empty_range_marks_a_position() {
        // An error at the end of the line: `start == end` is kept, not dropped.
        let lines = lines("abc");
        let mut rows = Vec::new();
        decode(&[diagnostic(0, 3, 0, 3)], &lines, &mut rows);

        assert_eq!(spans(&rows[0]), vec![(3, 3)]);
    }

    #[test]
    fn utf16_columns_are_converted_to_bytes() {
        // 你好 is 6 bytes but 2 UTF-16 units.
        let lines = lines("你好世界");
        let mut rows = Vec::new();
        decode(&[diagnostic(0, 1, 0, 3)], &lines, &mut rows);

        assert_eq!(spans(&rows[0]), vec![(3, 9)]);
    }

    #[test]
    fn a_range_past_the_end_of_the_file_is_clipped() {
        let lines = lines("ab");
        let mut rows = Vec::new();
        decode(&[diagnostic(0, 0, 9, 4)], &lines, &mut rows);

        assert_eq!(rows.len(), 1);
        assert_eq!(spans(&rows[0]), vec![(0, 2)]);
    }

    #[test]
    fn a_range_that_ends_before_it_starts_is_skipped() {
        let lines = lines("ab\ncd");
        let mut rows = Vec::new();
        decode(&[diagnostic(1, 2, 0, 1)], &lines, &mut rows);

        assert!(rows.iter().all(|r| r.is_empty()));
    }

    #[test]
    fn severity_becomes_an_underline_color_and_no_foreground() {
        let items: Vec<Diagnostic> = [
            Some(Severity::Error),
            Some(Severity::Warning),
            Some(Severity::Information),
            Some(Severity::Hint),
            None, // unset → the loudest one
        ]
        .into_iter()
        .enumerate()
        .map(|(i, severity)| Diagnostic {
            start_line: 0,
            end_line: 0,
            start_char: i,
            end_char: i + 1,
            severity,
        })
        .collect();

        let lines = lines("abcde");
        let mut rows = Vec::new();
        decode(&items, &lines, &mut rows);

        let colors: Vec<Option<Color>> = rows[0].iter().map(|r| r.style.underline_color).collect();
        assert_eq!(
            colors,
            [
                Some(Color::LightRed),
                Some(Color::LightYellow),
                Some(Color::LightBlue),
                Some(Color::Gray),
                Some(Color::LightRed),
            ]
        );

        // The point of it: no foreground, so the semantic color shows through.
        for run in &rows[0] {
            assert_eq!(run.style.fg, None);
            assert!(run.style.add_modifier.contains(Modifier::UNDERLINED));
        }
    }

    #[test]
    fn repeated_decodes_reuse_the_row_capacity() {
        let lines = lines("a\nb\nc");
        let mut rows: Vec<Vec<StyledRun>> = Vec::new();
        let items = [diagnostic(0, 0, 0, 1)];

        decode(&items, &lines, &mut rows);
        let capacity = rows[0].capacity();

        for _ in 0..4 {
            decode(&items, &lines, &mut rows);
        }

        assert_eq!(rows[0].capacity(), capacity);
    }

    /// What the renderer needs to be true for the whole design to work: a
    /// diagnostic run and a semantic run over the same byte must combine, not
    /// replace each other — hence underline-only, no foreground.
    #[test]
    fn a_diagnostic_style_merges_with_a_token_style() {
        let lines = lines("abc");
        let mut rows = Vec::new();
        decode(&[diagnostic(0, 0, 0, 3)], &lines, &mut rows);

        let underlined = rows[0][0].style;
        let colored = Style::new().fg(Color::LightMagenta);

        let merged = colored.patch(underlined);
        assert_eq!(merged.fg, Some(Color::LightMagenta));
        assert_eq!(merged.underline_color, Some(Color::LightRed));
        assert!(merged.add_modifier.contains(Modifier::UNDERLINED));
    }
}

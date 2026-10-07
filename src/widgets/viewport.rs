use core::fmt::Write;

use crate::fixed_buf::FixedBuf;
use crate::highlight::{Highlights, LAYER_COUNT, StyledRun};
use crate::utils;

use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::StatefulWidget;
use unicode_width::UnicodeWidthChar;

pub const LINE_NUMBER_MIN_WIDTH: usize = 3;
pub const LINE_NUMBER_SUFFIX: &str = " | ";

/// Line numbers are dimmed unless they belong to the line the cursor is on.
const LINE_NUMBER_STYLE: Style = Style::new().fg(Color::DarkGray);
const ACTIVE_LINE_NUMBER_STYLE: Style = Style::new().fg(Color::White);

/// Columns taken by the line-number gutter for a document of `line_count` lines.
pub fn gutter_width(line_count: usize) -> usize {
    utils::digit_count(line_count).max(LINE_NUMBER_MIN_WIDTH) + LINE_NUMBER_SUFFIX.len()
}

#[derive(Debug, Default)]
pub struct ViewportState {
    pub curr_line: usize,

    /// Text columns scrolled off to the left.
    pub scroll_x: usize,
    pub scroll_y: usize,
    /// Text columns that fit next to the gutter. The owning widget sets this
    /// during render; until then horizontal scrolling stays put.
    pub width: usize,
    pub height: usize,

    /// Per-row coloring. Owned here so the renderer can read it during
    /// `render` through the `&mut ViewportState` it already holds, with no new
    /// borrow. Off by default, so the editor behaves exactly as before until a
    /// producer feeds it data.
    pub highlights: Highlights,
}

pub struct Viewport<'a> {
    lines: &'a [String],
    active_line: Option<usize>,
}

impl<'a> Viewport<'a> {
    pub fn new(lines: &'a [String]) -> Self {
        Self {
            lines,
            active_line: None,
        }
    }

    /// Highlights the line number of `line`, e.g. the line under the cursor.
    pub fn active_line(mut self, line: usize) -> Self {
        self.active_line = Some(line);
        self
    }
}

impl StatefulWidget for Viewport<'_> {
    type State = ViewportState;

    fn render(
        self,
        area: ratatui::prelude::Rect,
        buf: &mut ratatui::prelude::Buffer,
        state: &mut Self::State,
    ) {
        let line_number_width = utils::digit_count(self.lines.len()).max(LINE_NUMBER_MIN_WIDTH);
        let content_offset_x = gutter_width(self.lines.len());

        let visible_lines = area.height as usize;
        let start_line = state.scroll_y;
        let end_line = (start_line + visible_lines).min(self.lines.len());

        // Text columns that fit to the right of the gutter.
        let text_width = area.width as i32 - content_offset_x as i32;

        for relative_y in 0..(end_line - start_line) {
            let absolute_y = start_line + relative_y;
            let line = &self.lines[absolute_y];

            let mut line_num_buf = FixedBuf::<32>::new();
            write!(
                &mut line_num_buf,
                "{:>width$}{LINE_NUMBER_SUFFIX}",
                absolute_y + 1,
                width = line_number_width
            )
            .unwrap();

            for (col, byte) in line_num_buf.as_str().bytes().enumerate() {
                if col >= area.width as usize {
                    continue;
                }

                let style = if self.active_line == Some(absolute_y) {
                    ACTIVE_LINE_NUMBER_STYLE
                } else {
                    LINE_NUMBER_STYLE
                };

                buf[(area.x + col as u16, area.y + relative_y as u16)]
                    .set_char(byte as char)
                    .set_style(style);
            }

            // Coloring is pulled per row. When it is off we hand the renderer
            // empty slices, so every text cell keeps the default style — the
            // byte-for-byte output of the pre-coloring editor.
            let empty: [&[StyledRun]; LAYER_COUNT] = [&[]; LAYER_COUNT];
            let layers = if state.highlights.enabled() {
                state.highlights.line_runs(absolute_y)
            } else {
                empty
            };

            // One cursor per layer, walking in lockstep with the characters.
            // Each advances past runs it has moved beyond. The cursors track
            // byte positions, never display columns, so horizontal scrolling
            // never shifts a color off its character.
            let mut cursors = [0usize; LAYER_COUNT];
            let mut col_offset = -(state.scroll_x as i32);
            let mut last_drawn: Option<i32> = None;

            for (byte_idx, ch) in line.char_indices() {
                let char_width = UnicodeWidthChar::width(ch).unwrap_or(0) as i32;

                for layer in 0..LAYER_COUNT {
                    while cursors[layer] < layers[layer].len()
                        && run_exhausted(&layers[layer][cursors[layer]], byte_idx)
                    {
                        cursors[layer] += 1;
                    }
                }

                let style = resolve(layers, cursors, byte_idx);

                if col_offset >= 0 && col_offset + char_width <= text_width {
                    let x = area.x + content_offset_x as u16 + col_offset as u16;
                    let y = area.y + relative_y as u16;

                    let is_whitespace = ch.is_whitespace();
                    let written_char = if is_whitespace { '·' } else { ch };
                    let style = if is_whitespace {
                        style.add_modifier(Modifier::DIM)
                    } else {
                        style
                    };
                    buf[(x, y)].set_char(written_char).set_style(style);

                    if char_width == 2 {
                        // A wide char's second cell must carry the same style,
                        // or CJK comments/strings show a broken color block.
                        buf[(x + 1, y)].set_char(' ').set_style(style);
                    }
                    last_drawn = Some(x as i32 + char_width - 1);
                }

                col_offset += char_width;
            }

            // A run that reaches the line end (e.g. a current-line background or
            // a diagnostic background) must keep painting to the visible row
            // end, otherwise the highlight snaps off at the last character. Only
            // rows whose trailing run carries a background are touched; plain
            // text pays nothing.
            if let Some(bg) = trailing_background(layers, line.len()) {
                let fill_from = match last_drawn {
                    Some(end) => end + 1,
                    // An empty row with a trailing (zero-length) background run
                    // still gets the full highlight; a row scrolled entirely off
                    // to the left is left alone.
                    None if line.is_empty() => content_offset_x as i32,
                    None => continue,
                };
                let fill_to = content_offset_x as i32 + text_width;
                for x in fill_from..fill_to {
                    if x < 0 {
                        continue;
                    }
                    let x = x as u16;
                    buf[(x, area.y + relative_y as u16)].set_style(bg);
                }
            }
        }
    }
}

/// The style for one byte: **each attribute comes from the highest-priority
/// layer that sets it**, not from one winning layer wholesale.
///
/// That distinction is the reason the layers can coexist. A diagnostic in the
/// lowest layer sets only an underline; the semantic layer above it sets only a
/// foreground; the selection sets only a background. Picking one layer's whole
/// `Style` would make two of the three invisible. Resolving per attribute lets
/// all three annotate the same character at once.
///
/// Allocation-free: a `Style` and a few comparisons on the stack, per character.
fn resolve(
    layers: [&[StyledRun]; LAYER_COUNT],
    cursors: [usize; LAYER_COUNT],
    byte_idx: usize,
) -> Style {
    let mut style = Style::default();

    // `LAYER_ORDER` is ascending priority, so walk it backwards: the first
    // layer to set an attribute owns it.
    for layer in (0..LAYER_COUNT).rev() {
        let Some(run) = layers[layer]
            .get(cursors[layer])
            .filter(|r| (r.start as usize) <= byte_idx)
        else {
            continue;
        };

        // An unset attribute is not "set to default", it is "not claimed" — so
        // a lower layer may still supply it.
        if style.fg.is_none() {
            style.fg = run.style.fg;
        }
        if style.bg.is_none() {
            style.bg = run.style.bg;
        }
        if style.underline_color.is_none() {
            style.underline_color = run.style.underline_color;
        }
        // Modifiers are additive across layers on purpose: a comment's italic
        // and a diagnostic's underline are independent facts about the same
        // character, and neither cancels the other.
        style.add_modifier |= run.style.add_modifier;
        style.sub_modifier |= run.style.sub_modifier;
    }

    style
}

/// Whether `run` no longer covers `byte_idx`.
///
/// A normal run `[start, end)` covers `byte_idx` while `byte_idx < end`; once
/// we reach `end` it is spent. A zero-length run `[p, p)` is a decoration at a
/// single position (a diagnostic at a line end, say) and covers only the cell
/// that starts exactly at `p` — so it is spent as soon as `byte_idx > p`. That
/// keeps it from swallowing the following character.
fn run_exhausted(run: &StyledRun, byte_idx: usize) -> bool {
    let start = run.start as usize;
    let end = run.end as usize;
    if start == end {
        byte_idx > start
    } else {
        byte_idx >= end
    }
}

/// The style whose background should extend to the visible row end, if a run
/// reaches the line end (`end == line_len`, or a zero-length run at `line_len`).
/// Higher-priority layers win, so the search runs from the top layer down.
fn trailing_background(layers: [&[StyledRun]; LAYER_COUNT], line_len: usize) -> Option<Style> {
    let candidate = |run: &StyledRun| -> Option<Style> {
        let reaches_end = (run.end as usize) == line_len
            || (run.start as usize) == (run.end as usize) && (run.start as usize) == line_len;
        // `Style::bg` is `Option<Color>`; `None` is "no background", which means
        // the run contributes no trailing fill.
        if reaches_end && run.style.bg.is_some() {
            Some(run.style)
        } else {
            None
        }
    };

    for layer in (0..LAYER_COUNT).rev() {
        for run in layers[layer].iter().rev() {
            if let Some(style) = candidate(run) {
                return Some(style);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{StyledRun, Viewport, ViewportState, gutter_width};
    use crate::highlight::LayerId;

    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::style::{Color, Style};
    use ratatui::widgets::StatefulWidget;

    fn render(lines: &[String], state: &mut ViewportState, width: u16) -> Buffer {
        let area = Rect::new(0, 0, width, lines.len() as u16);
        let mut buf = Buffer::empty(area);
        StatefulWidget::render(Viewport::new(lines), area, &mut buf, state);
        buf
    }

    #[test]
    fn scroll_x_shifts_the_text_and_leaves_the_gutter_in_place() {
        let lines = vec![String::from("0123456789")];
        let mut state = ViewportState {
            scroll_x: 4,
            ..Default::default()
        };

        let buf = render(&lines, &mut state, 12);

        // "  1 | " stays put; the text starts at display column 4.
        assert_eq!(buf[(2, 0)].symbol(), "1");
        assert_eq!(buf[(6, 0)].symbol(), "4");
        assert_eq!(buf[(11, 0)].symbol(), "9");
    }

    #[test]
    fn a_wide_char_cut_by_the_left_edge_is_hidden() {
        let lines = vec![String::from("你好ab")];
        let mut state = ViewportState {
            scroll_x: 1,
            ..Default::default()
        };

        let buf = render(&lines, &mut state, 14);

        // "你" spans columns 0..2, so scrolling by 1 clips it entirely.
        assert_eq!(buf[(6, 0)].symbol(), " ");
        assert_eq!(buf[(7, 0)].symbol(), "好");
    }

    #[test]
    fn only_the_active_line_number_is_white() {
        let lines = vec![String::from("a"), String::from("b")];
        let area = Rect::new(0, 0, 12, 2);
        let mut buf = Buffer::empty(area);
        let viewport = Viewport::new(&lines).active_line(1);

        StatefulWidget::render(viewport, area, &mut buf, &mut ViewportState::default());

        assert_eq!(buf[(2, 0)].fg, Color::DarkGray);
        assert_eq!(buf[(2, 1)].fg, Color::White);
    }

    // --- Coloring (Phase 1) -------------------------------------------------

    fn colored_state(rows: &[Vec<StyledRun>], enabled: bool) -> ViewportState {
        let mut state = ViewportState::default();
        state.highlights.replace_layer(LayerId::Overlay, rows);
        state.highlights.set_enabled(enabled);
        state
    }

    fn run_at(start: u32, end: u32, color: Color) -> StyledRun {
        StyledRun {
            start,
            end,
            style: Style::new().fg(color),
        }
    }

    #[test]
    fn overlay_paints_a_single_cell() {
        // Line "abc": color bytes [0,1) (the 'a').
        let lines = vec![String::from("abc")];
        let mut state = colored_state(&[vec![run_at(0, 1, Color::Red)]], true);
        let buf = render(&lines, &mut state, 12);

        let content = gutter_width(lines.len()) as u16;
        assert_eq!(buf[(content, 0)].symbol(), "a");
        assert_eq!(buf[(content, 0)].fg, Color::Red);
        // The rest stays default.
        assert_eq!(buf[(content + 1, 0)].fg, Color::Reset);
    }

    #[test]
    fn overlay_takes_priority_over_base() {
        let lines = vec![String::from("abc")];
        let mut state = ViewportState::default();
        state
            .highlights
            .replace_layer(LayerId::Base, &[vec![run_at(0, 3, Color::Red)]]);
        state
            .highlights
            .replace_layer(LayerId::Overlay, &[vec![run_at(0, 1, Color::Blue)]]);
        state.highlights.set_enabled(true);
        let buf = render(&lines, &mut state, 12);

        let content = gutter_width(lines.len()) as u16;
        // Overlay covers byte [0,1) → Blue wins on 'a'.
        assert_eq!(buf[(content, 0)].fg, Color::Blue);
        // Base shows through where overlay does not → 'b' Red.
        assert_eq!(buf[(content + 1, 0)].fg, Color::Red);
    }

    /// The three layers, resolved in one pass: each producer gets its own
    /// color where it covers a byte, and the higher-priority one wins.
    #[test]
    fn semantic_beats_syntax_and_overlay_beats_semantic() {
        let lines = vec![String::from("abcdef")];
        let mut state = ViewportState::default();
        // Syntax covers the whole line; semantic and overlay cover narrower
        // prefixes, so all three are visible in one row.
        state
            .highlights
            .replace_layer(LayerId::Base, &[vec![run_at(0, 6, Color::Red)]]);
        state
            .highlights
            .replace_layer(LayerId::Semantic, &[vec![run_at(0, 4, Color::Green)]]);
        state
            .highlights
            .replace_layer(LayerId::Overlay, &[vec![run_at(0, 2, Color::Blue)]]);
        state.highlights.set_enabled(true);
        let buf = render(&lines, &mut state, 12);

        let content = gutter_width(lines.len()) as u16;
        assert_eq!(buf[(content, 0)].fg, Color::Blue); // 'a': overlay
        assert_eq!(buf[(content + 1, 0)].fg, Color::Blue); // 'b': overlay
        assert_eq!(buf[(content + 2, 0)].fg, Color::Green); // 'c': semantic
        assert_eq!(buf[(content + 3, 0)].fg, Color::Green); // 'd': semantic
        assert_eq!(buf[(content + 4, 0)].fg, Color::Red); // 'e': syntax
        assert_eq!(buf[(content + 5, 0)].fg, Color::Red); // 'f': syntax
    }

    /// The gap between an edit and the server's answer is why the syntax layer
    /// exists: with only the semantic layer covering a byte, the rest is plain.
    #[test]
    fn uncovered_bytes_fall_through_to_the_default() {
        let lines = vec![String::from("abcdef")];
        let mut state = ViewportState::default();
        state
            .highlights
            .replace_layer(LayerId::Semantic, &[vec![run_at(2, 4, Color::Green)]]);
        state.highlights.set_enabled(true);
        let buf = render(&lines, &mut state, 12);

        let content = gutter_width(lines.len()) as u16;
        assert_eq!(buf[(content, 0)].fg, Color::Reset); // 'a'
        assert_eq!(buf[(content + 2, 0)].fg, Color::Green); // 'c'
        assert_eq!(buf[(content + 5, 0)].fg, Color::Reset); // 'f'
    }

    /// The whole reason resolution is per attribute: a diagnostic in the lowest
    /// layer and a token color in the one above it must both be visible on the
    /// same character. With "highest layer wins wholesale" the underline — the
    /// more urgent of the two — would be the one that disappears.
    #[test]
    fn a_diagnostic_underline_and_a_token_color_both_show() {
        let lines = vec![String::from("abc")];
        let mut state = ViewportState::default();
        state.highlights.replace_layer(
            LayerId::Base,
            &[vec![StyledRun {
                start: 0,
                end: 2,
                style: Style::new()
                    .underline_color(Color::LightRed)
                    .add_modifier(ratatui::style::Modifier::UNDERLINED),
            }]],
        );
        state
            .highlights
            .replace_layer(LayerId::Semantic, &[vec![run_at(0, 3, Color::LightBlue)]]);
        state.highlights.set_enabled(true);
        let buf = render(&lines, &mut state, 12);

        let content = gutter_width(lines.len()) as u16;
        // 'a' and 'b': the token's foreground and the diagnostic's underline.
        assert_eq!(buf[(content, 0)].fg, Color::LightBlue);
        assert_eq!(buf[(content, 0)].underline_color, Color::LightRed);
        assert_eq!(buf[(content + 1, 0)].fg, Color::LightBlue);
        assert_eq!(buf[(content + 1, 0)].underline_color, Color::LightRed);
        // 'c' is outside the diagnostic: colored, not underlined.
        assert_eq!(buf[(content + 2, 0)].fg, Color::LightBlue);
        assert_eq!(buf[(content + 2, 0)].underline_color, Color::Reset);
    }

    #[test]
    fn scroll_x_keeps_color_locked_to_its_character() {
        // 10-char line, color byte [3,4) ('d'); scroll 2 columns left.
        let lines = vec![String::from("abcdefghij")];
        let mut state = colored_state(&[vec![run_at(3, 4, Color::Red)]], true);
        state.scroll_x = 2;
        let buf = render(&lines, &mut state, 12);

        let content = gutter_width(lines.len()) as u16;
        // 'd' (byte 3, display col 3) lands at visible display col 1.
        assert_eq!(buf[(content + 1, 0)].symbol(), "d");
        assert_eq!(buf[(content + 1, 0)].fg, Color::Red);
        // 'c' (display col 2) is the first visible char, uncolored.
        assert_eq!(buf[(content, 0)].symbol(), "c");
        assert_eq!(buf[(content, 0)].fg, Color::Reset);
    }

    #[test]
    fn wide_char_second_cell_carries_the_same_style() {
        // "你x": color byte [0,3) (the '你', width 2).
        let lines = vec![String::from("你x")];
        let mut state = colored_state(&[vec![run_at(0, 3, Color::Red)]], true);
        let buf = render(&lines, &mut state, 12);

        let content = gutter_width(lines.len()) as u16;
        // First cell of '你' and its trailing cell both red, else the block breaks.
        assert_eq!(buf[(content, 0)].fg, Color::Red);
        assert_eq!(buf[(content + 1, 0)].fg, Color::Red);
        // 'x' (byte 3) is untouched.
        assert_eq!(buf[(content + 2, 0)].fg, Color::Reset);
    }

    #[test]
    fn trailing_background_fills_to_the_row_end() {
        // Full-line background on "abc".
        let lines = vec![String::from("abc")];
        let mut state = ViewportState::default();
        state.highlights.replace_layer(
            LayerId::Overlay,
            &[vec![StyledRun {
                start: 0,
                end: 3,
                style: Style::new().bg(Color::Blue),
            }]],
        );
        state.highlights.set_enabled(true);
        let buf = render(&lines, &mut state, 20);

        let content = gutter_width(lines.len()) as u16;
        // Every cell from the line start to the visible row end carries the bg.
        for x in content..20 {
            assert_eq!(buf[(x, 0)].bg, Color::Blue, "cell {x} missing bg");
        }
    }

    #[test]
    fn zero_length_run_colors_one_cell_without_swallowing_the_next() {
        // A zero-length run at byte 1 (between 'a' and 'b') colors only 'b'.
        let lines = vec![String::from("ab")];
        let mut state = colored_state(&[vec![run_at(1, 1, Color::Blue)]], true);
        let buf = render(&lines, &mut state, 12);

        let content = gutter_width(lines.len()) as u16;
        assert_eq!(buf[(content, 0)].fg, Color::Reset); // 'a' untouched
        assert_eq!(buf[(content + 1, 0)].fg, Color::Blue); // 'b' colored
    }

    #[test]
    fn disabled_render_leaves_text_cells_default() {
        // Data is present but coloring is off: identical to the old editor.
        let lines = vec![String::from("abc")];
        let mut state = colored_state(&[vec![run_at(0, 3, Color::Red)]], false);
        let buf = render(&lines, &mut state, 12);

        let content = gutter_width(lines.len()) as u16;
        for x in content..12 {
            assert_eq!(buf[(x, 0)].fg, Color::Reset);
            assert_eq!(buf[(x, 0)].bg, Color::Reset);
        }
    }
}

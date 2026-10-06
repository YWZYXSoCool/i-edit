use core::fmt::Write;

use crate::fixed_buf::FixedBuf;
use crate::utils;

use ratatui::style::{Color, Style};
use ratatui::widgets::StatefulWidget;

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

            // Column offsets are relative to the text start and shifted left by
            // `scroll_x`. A wide char cut by either edge is dropped instead of
            // half-drawn, leaving its cells blank.
            let mut col_offset = -(state.scroll_x as i32);
            for ch in line.chars() {
                let char_width = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0) as i32;

                if col_offset >= 0 && col_offset + char_width <= text_width {
                    let x = area.x + content_offset_x as u16 + col_offset as u16;
                    buf[(x, area.y + relative_y as u16)].set_char(ch);
                    if char_width == 2 {
                        buf[(x + 1, area.y + relative_y as u16)].set_char(' ');
                    }
                }

                col_offset += char_width;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Viewport, ViewportState};

    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::style::Color;
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
}

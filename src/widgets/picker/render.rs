//! Drawing the picker overlay.
//!
//! Everything here reads [`PickerState`](super::PickerState) and writes cells;
//! none of it decides anything. The layout is one bordered box: the path input
//! on top (plus the name input in save mode), the listing filling the middle and
//! the key hints along the bottom.
//!
//! Rows are drawn span by span against a shrinking width budget rather than
//! assembled and clipped as a whole. Budgeting that way is column-for-column the
//! same as clipping the assembled row, but it keeps the borrow of the row alive
//! instead of forcing an owned `String` per row per frame.

use std::path::Path;

use crate::component::Component;
use crate::icon;
use crate::widgets::Input;

use crossterm::event::Event;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Margin, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Widget};
use unicode_width::UnicodeWidthChar;

use super::{Picker, PickerMode, PickerState, Row};

/// Rows of one bordered input box: a line of text between two border rows.
const INPUT_HEIGHT: u16 = 3;

/// Border and title colour, matching the log panel.
const BORDER_STYLE: Style = Style::new().fg(Color::White);

/// Highlight of the row under the cursor: inverted across the whole row.
const HIGHLIGHT_STYLE: Style = Style::new().fg(Color::Black).bg(Color::White);

/// Style of dotfiles and of the `(empty)` marker.
const HIDDEN_STYLE: Style = Style::new().fg(Color::DarkGray);

/// Style of the footer hints.
const DIM_STYLE: Style = Style::new().fg(Color::Gray);

/// Shown in the save-mode name field while it is empty.
const NAME_PLACEHOLDER: &str = "File name:";

impl Component for Picker {
    type State = PickerState;

    fn handle_event(self, event: &Event, state: &mut Self::State) {
        let Some(key) = crate::utils::key_press(event) else {
            return;
        };

        state.handle_key(key);
    }

    fn render(self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
        render_picker(area, buf, state);
    }
}

fn render_picker(area: Rect, buf: &mut Buffer, state: &mut PickerState) {
    let area = area.centered(Constraint::Percentage(80), Constraint::Percentage(80));
    if area.width == 0 || area.height == 0 {
        return;
    }

    // Opaque: the editor must not show through the picker.
    Clear.render(area, buf);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(BORDER_STYLE)
        .title(Line::from(vec![
            Span::raw(icon::FOLDER),
            Span::raw(" "),
            Span::raw(state.mode.title()),
        ]));
    block.render(area, buf);

    let inner = area.inner(Margin {
        horizontal: 1,
        vertical: 1,
    });
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    match state.mode {
        PickerMode::File | PickerMode::Folder => {
            let [path_area, list_area, footer_area] = Layout::vertical([
                Constraint::Length(INPUT_HEIGHT),
                Constraint::Fill(1),
                Constraint::Length(1),
            ])
            .areas(inner);

            Component::render(Input::new(), path_area, buf, &mut state.path_input);
            render_list(list_area, buf, state);
            render_footer(footer_area, buf, state.mode.footer());
        }
        PickerMode::Save => {
            let [name_area, path_area, list_area, footer_area] = Layout::vertical([
                Constraint::Length(INPUT_HEIGHT),
                Constraint::Length(INPUT_HEIGHT),
                Constraint::Fill(1),
                Constraint::Length(1),
            ])
            .areas(inner);

            Component::render(
                Input::new().placeholder(NAME_PLACEHOLDER),
                name_area,
                buf,
                &mut state.name_input,
            );
            Component::render(Input::new(), path_area, buf, &mut state.path_input);
            render_list(list_area, buf, state);
            render_footer(footer_area, buf, PickerMode::Save.footer());
        }
    }
}

fn render_list(area: Rect, buf: &mut Buffer, state: &mut PickerState) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let visible = area.height as usize;

    // Keep the highlighted row inside the viewport, moving the offset only
    // when the highlight actually walked off an edge.
    if state.highlighted < state.scroll {
        state.scroll = state.highlighted;
    } else if state.highlighted >= state.scroll + visible {
        state.scroll = state.highlighted + 1 - visible;
    }
    state.scroll = state.scroll.min(state.rows.len().saturating_sub(visible));

    for (offset, row) in state
        .rows
        .iter()
        .skip(state.scroll)
        .take(visible)
        .enumerate()
    {
        let row_area = Rect {
            y: area.y.saturating_add(offset as u16),
            height: 1,
            ..area
        };

        let selected = state.scroll + offset == state.highlighted;
        let style = if selected {
            HIGHLIGHT_STYLE
        } else if row.is_hidden() {
            HIDDEN_STYLE
        } else {
            Style::default()
        };

        // Invert the whole row, not just the glyphs: a highlight that stops at
        // the end of the name looks like a rendering bug.
        buf.set_style(row_area, style);

        let mut spans = row_spans(row, area.width as usize);
        // The spans borrow the row and carry its style, so every cell the text
        // writes keeps the style of the old joined-and-clipped span.
        for span in &mut spans {
            span.style = style;
        }
        Line::from(spans).render(row_area, buf);
    }

    if !state.has_entries() {
        // `..` may be listed above it, but the directory itself is empty.
        let index = state.rows.len().saturating_sub(state.scroll);
        if index < visible {
            let row_area = Rect {
                y: area.y.saturating_add(index as u16),
                height: 1,
                ..area
            };
            Line::from(Span::styled("(empty)", HIDDEN_STYLE)).render(row_area, buf);
        }
    }
}

fn render_footer(area: Rect, buf: &mut Buffer, hint: &str) {
    // The footer is the first thing to go when the overlay is too short.
    if area.width == 0 || area.height == 0 {
        return;
    }

    Line::from(Span::styled(hint, DIM_STYLE)).render(area, buf);
}

/// One row of the list as borrowed, already clipped spans.
///
/// The spans come in rendered order — padding, marker, glyph, name and the
/// trailing `/` of a directory — and each one only gets the width the previous
/// ones leave free. Budgeting that way is column-for-column the same as
/// clipping the whole assembled row, so the suffix is the first thing dropped
/// when the space runs out.
pub(crate) fn row_spans(row: &Row, max_width: usize) -> Vec<Span<'_>> {
    let (marker, glyph, name, suffix) = match row {
        Row::Parent => (icon::CHEVRON, icon::FOLDER, "..", ""),
        Row::Entry(entry) if entry.is_dir => {
            (icon::CHEVRON, icon::FOLDER, entry.name.as_str(), "/")
        }
        Row::Entry(entry) => (" ", icon::FILE, entry.name.as_str(), ""),
    };

    let mut spans = Vec::with_capacity(6);
    let mut used = 0;

    for text in [" ", marker, glyph, " ", name, suffix] {
        let clipped = clip_to_width(text, max_width - used);
        used += display_width(clipped);
        // A truncated span means the row ends there: the old whole-row clip
        // stopped at the same column and dropped whatever followed.
        let truncated = clipped.len() < text.len();
        if !clipped.is_empty() {
            spans.push(Span::raw(clipped));
        }
        if truncated {
            break;
        }
    }

    spans
}

/// Display width of `text`, the sum [`clip_to_width`] budgets with.
fn display_width(text: &str) -> usize {
    text.chars()
        .map(|ch| UnicodeWidthChar::width(ch).unwrap_or(0))
        .sum()
}

/// Truncates `text` to `max_width` display columns.
///
/// File names are arbitrary Unicode, so a byte-based cut could split a
/// character or leave a double-width glyph hanging over the row's edge. The
/// returned slice is on a char boundary and borrows from `text`.
pub(crate) fn clip_to_width(text: &str, max_width: usize) -> &str {
    let mut width = 0;
    let mut end = 0;

    for ch in text.chars() {
        let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if width + ch_width > max_width {
            break;
        }
        width += ch_width;
        end += ch.len_utf8();
    }

    &text[..end]
}

/// Renders `path` for the input box, ending in a separator when it names a
/// directory (unless it already ends in one, so the drive root stays `C:\`).
pub(crate) fn with_separator(path: &Path) -> String {
    let mut text = path.to_string_lossy().into_owned();
    if !text.ends_with(std::path::MAIN_SEPARATOR) && !text.ends_with('/') {
        text.push(std::path::MAIN_SEPARATOR);
    }
    text
}

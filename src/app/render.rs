//! The layout: a row of tabs along the top, a status bar along the bottom, and
//! between them either the editor alone or the editor beside the file tree.
//!
//! Drawing order matters at the end: the popup covers the editor, and
//! notifications are drawn last of all so they stay visible even over an open
//! popup.

use std::path::Path;

use crate::app::{App, Focus};
use crate::component::Component;
use crate::icon;
use crate::widgets::{Editor, EditorState, FileTree, MessageBox, Popup, StatusBar};

use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;
use unicode_width::UnicodeWidthStr;

/// Height of the tab bar: it names the buffers, nothing more.
const TAB_BAR_HEIGHT: u16 = 1;

/// Style for a tab nobody is looking at.
const TAB_STYLE: Style = Style::new().fg(Color::DarkGray);

/// Style for the tab the editor is showing: a quiet mark, because the keys
/// are somewhere else.
const ACTIVE_TAB_STYLE: Style = Style::new().fg(Color::White).bg(Color::DarkGray);

/// Bounds of the file tree panel: a quarter of the window, kept readable.
const FILE_TREE_MIN_WIDTH: u16 = 20;
const FILE_TREE_MAX_WIDTH: u16 = 40;

/// Columns the editor never gives up to the tree.
const EDITOR_MIN_WIDTH: u16 = 10;

impl App {
    pub(super) fn render(&mut self, frame: &mut Frame) {
        let area = frame.area();

        let layout = Layout::vertical([
            Constraint::Length(TAB_BAR_HEIGHT),
            Constraint::Fill(1),
            Constraint::Length(1),
        ]);
        let [tab_bar_area, main_area, status_bar_area] = layout.areas(area);

        self.render_tab_bar(tab_bar_area, frame.buffer_mut());

        if self.file_tree_visible {
            let tree_width = file_tree_width(area.width);
            let [editor_area, tree_area] =
                Layout::horizontal([Constraint::Fill(1), Constraint::Length(tree_width)])
                    .areas(main_area);

            Component::render(
                Editor,
                editor_area,
                frame.buffer_mut(),
                self.tabs.active_mut(),
            );
            Component::render(
                FileTree,
                tree_area,
                frame.buffer_mut(),
                &mut self.file_tree_state,
            );
        } else {
            Component::render(
                Editor,
                main_area,
                frame.buffer_mut(),
                self.tabs.active_mut(),
            );
        }

        self.status_bar_state.cursor = self.tabs.active().text.cursor;

        Component::render(
            StatusBar::new(&self.tabs.active().text.lines).dirty(self.tabs.active().dirty),
            status_bar_area,
            frame.buffer_mut(),
            &mut self.status_bar_state,
        );

        // The cursor belongs to the editor only while typing goes there and no
        // popup covers it.
        if self.focus == Focus::Editor
            && self.popup_state.kind.is_none()
            && let Some(pos) = self.tabs.active().cursor_screen_pos
        {
            frame.set_cursor_position(pos);
        }

        Component::render(Popup, area, frame.buffer_mut(), &mut self.popup_state);

        // Last, so notifications stay visible even over an open popup.
        Component::render(
            MessageBox,
            area,
            frame.buffer_mut(),
            &mut self.message_box_state,
        );
    }

    /// Draws the names of the open buffers along the top.
    ///
    /// Tabs are laid out left to right and stop when the row is full; the ones
    /// that did not fit are counted rather than silently dropped, so a crowded
    /// bar still says how much it is hiding. The remainder of the row is
    /// padded so the bar reads as one continuous strip.
    pub(super) fn render_tab_bar(&self, area: Rect, buf: &mut Buffer) {
        if area.is_empty() {
            return;
        }

        let active = self.tabs.active_index();
        let count = self.tabs.len();
        let width = area.width as usize;

        let mut spans: Vec<Span<'static>> = Vec::with_capacity(count + 1);
        let mut used = 0;

        for (i, buffer) in self.tabs.buffers().iter().enumerate() {
            let label = tab_label(buffer);
            let label_width = UnicodeWidthStr::width(label.as_str());

            // This tab may only take its own width if what follows it still
            // fits: one column here costs a whole file name further right.
            let hidden = count - 1 - i;
            let after = overflow_width(hidden);
            if used + label_width + after > width {
                let overflow = overflow_label(count - i);
                let marker_width = UnicodeWidthStr::width(overflow.as_str());
                if used + marker_width <= width {
                    spans.push(Span::styled(overflow, TAB_STYLE));
                    used += marker_width;
                }
                break;
            }

            let style = match i == active {
                false => TAB_STYLE,
                true => ACTIVE_TAB_STYLE,
            };

            spans.push(Span::styled(label, style));
            used += label_width;
        }

        if used < width {
            spans.push(Span::styled(" ".repeat(width - used), TAB_STYLE));
        }

        Line::from(spans).render(area, buf);
    }
}

/// The `" +N"` shown once every tab that fits has been drawn.
fn overflow_label(hidden: usize) -> String {
    format!(" +{hidden}")
}

/// Columns [`overflow_label`] needs for `hidden` tabs, `0` when there would be
/// nothing to count. The digits decide, so this cannot be one small constant.
fn overflow_width(hidden: usize) -> usize {
    if hidden == 0 {
        0
    } else {
        UnicodeWidthStr::width(overflow_label(hidden).as_str())
    }
}

/// What one tab reads: the file's icon and name, or `[scratch]`, with a `*`
/// for unsaved changes and a space either side to keep the tabs apart.
fn tab_label(buffer: &EditorState) -> String {
    let name = buffer
        .path
        .as_deref()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .unwrap_or("[scratch]");

    let mark = if buffer.dirty { "*" } else { "" };
    format!(" {} {name}{mark} ", icon::icon_for(name))
}

/// Width of the tree panel: a quarter of the window, kept readable and never
/// allowed to squeeze the editor out.
fn file_tree_width(total_width: u16) -> u16 {
    let ideal = (total_width / 6).clamp(FILE_TREE_MIN_WIDTH, FILE_TREE_MAX_WIDTH);
    ideal.min(total_width.saturating_sub(EDITOR_MIN_WIDTH))
}

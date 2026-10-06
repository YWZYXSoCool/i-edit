//! The layout: one status bar along the bottom, and above it either the editor
//! alone or the editor beside the file tree.
//!
//! Drawing order matters at the end: the popup covers the editor, and
//! notifications are drawn last of all so they stay visible even over an open
//! popup.

use std::path::Path;

use crate::app::{App, Focus};
use crate::component::Component;
use crate::widgets::{Editor, FileTree, MessageBox, Popup, StatusBar};

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout};

/// Bounds of the file tree panel: a quarter of the window, kept readable.
const FILE_TREE_MIN_WIDTH: u16 = 20;
const FILE_TREE_MAX_WIDTH: u16 = 40;

/// Columns the editor never gives up to the tree.
const EDITOR_MIN_WIDTH: u16 = 10;

impl App {
    pub(super) fn render(&mut self, frame: &mut Frame) {
        let area = frame.area();

        let layout = Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]);
        let [main_area, status_bar_area] = layout.areas(area);

        if self.file_tree_visible {
            let tree_width = file_tree_width(area.width);
            let [editor_area, tree_area] =
                Layout::horizontal([Constraint::Fill(1), Constraint::Length(tree_width)])
                    .areas(main_area);

            Component::render(
                Editor,
                editor_area,
                frame.buffer_mut(),
                &mut self.editor_state,
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
                &mut self.editor_state,
            );
        }

        self.status_bar_state.cursor = self.editor_state.text.cursor;

        let file_name = self
            .editor_state
            .path
            .as_deref()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str());

        Component::render(
            StatusBar::new(&self.editor_state.text.lines)
                .file_name(file_name)
                .dirty(self.editor_state.dirty),
            status_bar_area,
            frame.buffer_mut(),
            &mut self.status_bar_state,
        );

        // The cursor belongs to the editor only while typing goes there and no
        // popup covers it.
        if self.focus == Focus::Editor
            && self.popup_state.kind.is_none()
            && let Some(pos) = self.editor_state.cursor_screen_pos
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
}

/// Width of the tree panel: a quarter of the window, kept readable and never
/// allowed to squeeze the editor out.
fn file_tree_width(total_width: u16) -> u16 {
    let ideal = (total_width / 6).clamp(FILE_TREE_MIN_WIDTH, FILE_TREE_MAX_WIDTH);
    ideal.min(total_width.saturating_sub(EDITOR_MIN_WIDTH))
}

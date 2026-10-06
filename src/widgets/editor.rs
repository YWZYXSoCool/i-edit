use std::path::PathBuf;

use crate::action::{Action, Actions};
use crate::component::Component;
use crate::text::TextState;
use crate::widgets::viewport::gutter_width;
use crate::widgets::{Viewport, ViewportState};

use crossterm::event::{Event, KeyCode, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::{Offset, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, StatefulWidget, Widget,
};
use unicode_width::UnicodeWidthStr;

/// Lines jumped by PageUp / PageDown.
const PAGE_SIZE: usize = 20;

/// Welcome drawn while the scratch buffer is empty, in the spirit of vim's
/// splash: what the editor is, then the keys that get you started. Typing the
/// first character replaces it.
const WELCOME_LINES: &[&str] = &[
    env!("CARGO_PKG_NAME"),
    concat!("version ", env!("CARGO_PKG_VERSION")),
    "",
    "Ctrl+O        open file",
    "Ctrl+Shift+O  open folder",
    "Ctrl+S        save",
    "Ctrl+Shift+S  save as",
    "Ctrl+B        toggle file tree",
    "Ctrl+Shift+Q  commands",
    "Esc           quit",
];

/// The welcome's first line stands out; the hints stay dim, like the file
/// tree's placeholder.
const TITLE_STYLE: Style = Style::new().fg(Color::White).add_modifier(Modifier::BOLD);
const HINT_STYLE: Style = Style::new().fg(Color::DarkGray);

/// The text area: a scrolling, multi-line view over a [`TextState`].
#[derive(Debug, Default)]
pub struct Editor;

impl Component for Editor {
    type State = EditorState;

    fn handle_event(self, event: &Event, state: &mut Self::State) {
        let Some(key) = crate::utils::key_press(event) else {
            return;
        };

        state.handle_key(key.code, key.modifiers);
    }

    fn render(self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
        let gutter = gutter_width(state.text.lines.len());

        // Calculate content area (excluding scrollbar space)
        let content_area = Rect {
            width: area.width.saturating_sub(2), // Reserve 2 columns for scrollbar
            ..area
        };

        // How much text fits next to the gutter; drives horizontal scrolling.
        let text_width = (content_area.width as usize).saturating_sub(gutter);
        state.viewport_state.width = text_width;

        let longest_line = state
            .text
            .lines
            .iter()
            .map(|line| UnicodeWidthStr::width(line.as_str()))
            .max()
            .unwrap_or(0);

        // Pull a stale offset back once the line shrinks: only keep scrolled
        // what cannot fit, plus one column for a cursor past the line end.
        let max_scroll_x = longest_line.saturating_add(1).saturating_sub(text_width);
        state.viewport_state.scroll_x = state.viewport_state.scroll_x.min(max_scroll_x);

        // The horizontal scrollbar takes the bottom row, but only while some
        // line is actually wider than the view.
        let needs_h_scrollbar = text_width > 0 && area.height >= 2 && longest_line > text_width;

        let content_area = Rect {
            height: content_area
                .height
                .saturating_sub(u16::from(needs_h_scrollbar)),
            ..content_area
        };
        state.viewport_state.height = content_area.height as usize;

        // Render the viewport widget (content without scrollbars)
        let viewport = Viewport::new(&state.text.lines).active_line(state.text.cursor.y);
        StatefulWidget::render(viewport, content_area, buf, &mut state.viewport_state);

        // An empty scratch buffer is a blank page: greet the user the way vim
        // does instead of leaving it bare.
        if is_empty_scratch(state)
            && let Some(area) = welcome_area(content_area)
        {
            Paragraph::new(welcome_text()).render(area, buf);
        }

        // Render the vertical scrollbar in the remaining columns
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .track_symbol(Some("│"))
            .thumb_symbol("█");
        let scrollbar_area = Rect {
            x: content_area.right(),
            width: 2,
            ..content_area
        };
        StatefulWidget::render(scrollbar, scrollbar_area, buf, &mut state.scrollbar_state);

        // Render the horizontal scrollbar under the columns it scrolls
        if needs_h_scrollbar {
            state.h_scrollbar_state = state
                .h_scrollbar_state
                .content_length(longest_line.saturating_sub(text_width))
                .position(state.viewport_state.scroll_x);

            let h_scrollbar = Scrollbar::new(ScrollbarOrientation::HorizontalBottom)
                .track_symbol(Some("─"))
                .thumb_symbol("▬");
            let h_scrollbar_area = Rect {
                x: content_area.x + gutter as u16,
                y: area.bottom() - 1,
                width: text_width as u16,
                height: 1,
            };
            StatefulWidget::render(
                h_scrollbar,
                h_scrollbar_area,
                buf,
                &mut state.h_scrollbar_state,
            );
        }

        state.cursor_screen_pos = Some(cursor_screen_pos(area, state));
    }
}

/// True while this is the never-saved startup buffer with nothing typed into it.
fn is_empty_scratch(state: &EditorState) -> bool {
    state.path.is_none() && state.text.lines.iter().all(String::is_empty)
}

/// The welcome content with its first line emphasized.
fn welcome_text() -> Vec<Line<'static>> {
    WELCOME_LINES
        .iter()
        .enumerate()
        .map(|(index, line)| {
            let style = if index == 0 { TITLE_STYLE } else { HINT_STYLE };
            Line::from(Span::styled(*line, style))
        })
        .collect()
}

/// Where the welcome block goes: centered, or `None` when it does not fit.
fn welcome_area(area: Rect) -> Option<Rect> {
    let width = WELCOME_LINES
        .iter()
        .map(|line| UnicodeWidthStr::width(*line))
        .max()
        .unwrap_or(0) as u16;

    centered_block(area, width, WELCOME_LINES.len() as u16)
}

/// A `width` x `height` rect centered in `area`, or `None` when it does not fit.
fn centered_block(area: Rect, width: u16, height: u16) -> Option<Rect> {
    if width == 0 || height == 0 || width > area.width || height > area.height {
        return None;
    }

    Some(Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    })
}

/// Everything [`Editor`] needs to draw and edit itself.
///
/// The text lives in [`TextState`], shared with [`Input`](crate::widgets::Input);
/// what is left here is the view: how the viewport is scrolled, where the
/// terminal cursor goes and which [`Action`]s the editor has queued.
#[derive(Debug)]
pub struct EditorState {
    /// The document being edited.
    pub text: TextState,
    /// Path this buffer was loaded from or last saved to; `None` for a scratch
    /// buffer that has never been saved.
    pub path: Option<PathBuf>,
    /// Whether the buffer holds edits not yet written to disk.
    pub dirty: bool,
    pub(crate) viewport_state: ViewportState,
    pub(crate) scrollbar_state: ScrollbarState,
    pub(crate) h_scrollbar_state: ScrollbarState,
    pub(crate) cursor_screen_pos: Option<Position>,
    actions: Actions,
}

impl EditorState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Takes everything this component has asked for since the last drain.
    ///
    /// Nothing the editor does today needs the shell, but the channel is here
    /// so commands like "save the buffer" have somewhere to go.
    pub fn take_actions(&mut self) -> Vec<Action> {
        self.actions.drain()
    }

    /// Replaces the buffer with a freshly loaded file and resets the view.
    pub fn load_file(&mut self, path: PathBuf, lines: Vec<String>) {
        self.text.load(lines);
        self.path = Some(path);
        self.dirty = false;
        self.viewport_state.scroll_x = 0;
        self.viewport_state.scroll_y = 0;
        self.viewport_state.curr_line = 0;
        self.scrollbar_state = ScrollbarState::default();
        self.h_scrollbar_state = ScrollbarState::default();
        self.cursor_screen_pos = None;
    }

    /// Called after a successful write to disk.
    pub fn mark_saved(&mut self) {
        self.dirty = false;
    }

    pub fn handle_key(&mut self, code: KeyCode, modifiers: KeyModifiers) {
        // Editing keys flip the dirty flag; movement keys must leave it alone.
        let mut edited = false;

        match (modifiers, code) {
            (_, KeyCode::Char(c)) => {
                self.text.insert_char(c);
                edited = true;
            }
            (KeyModifiers::NONE, KeyCode::Backspace) => {
                self.text.delete_backward();
                edited = true;
            }
            (KeyModifiers::NONE, KeyCode::Delete) => {
                self.text.delete_forward();
                edited = true;
            }
            (KeyModifiers::NONE, KeyCode::Enter) => {
                self.text.insert_new_line();
                edited = true;
            }
            (KeyModifiers::NONE, KeyCode::Up) => self.text.move_up(),
            (KeyModifiers::NONE, KeyCode::Down) => self.text.move_down(),
            (KeyModifiers::NONE, KeyCode::Left) => self.text.move_left(),
            (KeyModifiers::NONE, KeyCode::Right) => self.text.move_right(),
            (KeyModifiers::NONE, KeyCode::Home) => self.text.move_to_line_start(),
            (KeyModifiers::NONE, KeyCode::End) => self.text.move_to_line_end(),
            (KeyModifiers::CONTROL, KeyCode::Left) => self.text.move_word_left(),
            (KeyModifiers::CONTROL, KeyCode::Right) => self.text.move_word_right(),
            (KeyModifiers::CONTROL, KeyCode::Up) => self.text.move_to_text_start(),
            (KeyModifiers::CONTROL, KeyCode::Down) => self.text.move_to_text_end(),
            (KeyModifiers::NONE, KeyCode::PageUp) => self.text.move_page_up(PAGE_SIZE),
            (KeyModifiers::NONE, KeyCode::PageDown) => self.text.move_page_down(PAGE_SIZE),
            (KeyModifiers::CONTROL, KeyCode::Home) => self.text.move_to_text_start(),
            (KeyModifiers::CONTROL, KeyCode::End) => self.text.move_to_text_end(),
            _ => {}
        }

        if edited {
            self.dirty = true;
        }

        self.sync();
    }

    fn sync(&mut self) {
        self.text.clamp_cursor();
        self.ensure_cursor_visible();
        self.update_scrollbar_state();
        self.viewport_state.curr_line = self.text.cursor.y;
    }

    /// Scrolls the viewport just enough to keep the cursor inside it.
    pub fn ensure_cursor_visible(&mut self) {
        let y = self.text.cursor.y;
        let visible_height = self.viewport_state.height;

        if y < self.viewport_state.scroll_y {
            self.viewport_state.scroll_y = y;
        } else if y >= self.viewport_state.scroll_y + visible_height {
            self.viewport_state.scroll_y = y - visible_height + 1;
        }

        // Before the first render the width is unknown; don't scroll on guesses.
        if self.viewport_state.width == 0 {
            return;
        }

        let x = self.text.display_col();

        if x < self.viewport_state.scroll_x {
            self.viewport_state.scroll_x = x;
        } else if x >= self.viewport_state.scroll_x + self.viewport_state.width {
            self.viewport_state.scroll_x = x - self.viewport_state.width + 1;
        }
    }

    pub fn update_scrollbar_state(&mut self) {
        let total_lines = self.text.lines.len();

        self.scrollbar_state = self
            .scrollbar_state
            .content_length(total_lines.saturating_sub(self.viewport_state.height))
            .position(self.viewport_state.scroll_y);
    }

    pub fn viewport_state(mut self, viewport_state: ViewportState) -> Self {
        self.viewport_state = viewport_state;
        self
    }

    pub fn scrollbar_state(mut self, scrollbar_state: ScrollbarState) -> Self {
        self.scrollbar_state = scrollbar_state;
        self
    }
}

impl Default for EditorState {
    fn default() -> Self {
        Self {
            text: TextState::default(),
            path: None,
            dirty: false,
            viewport_state: ViewportState::default(),
            scrollbar_state: ScrollbarState::default(),
            h_scrollbar_state: ScrollbarState::default(),
            cursor_screen_pos: None,
            actions: Actions::new(),
        }
    }
}

/// Where the terminal cursor should sit, accounting for the gutter and scroll.
fn cursor_screen_pos(area: Rect, state: &EditorState) -> Position {
    let content_offset_x = gutter_width(state.text.lines.len());
    let relative_x = state
        .text
        .display_col()
        .saturating_sub(state.viewport_state.scroll_x);
    let relative_y = state
        .text
        .cursor
        .y
        .saturating_sub(state.viewport_state.scroll_y);

    area.as_position()
        + Offset::new(
            relative_x as i32 + content_offset_x as i32,
            relative_y as i32,
        )
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{Editor, EditorState};
    use crate::Cursor;
    use crate::component::Component;

    use crossterm::event::{KeyCode, KeyModifiers};
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

        let buf = render_in(&mut state, 64, 12);

        let text = text_of(&buf);
        assert!(text.contains(env!("CARGO_PKG_NAME")));
        assert!(text.contains(concat!("version ", env!("CARGO_PKG_VERSION"))));
        assert!(text.contains("open file"));
        assert!(text.contains("toggle file tree"));
        assert!(text.contains("quit"));

        // Centered block: title bright and bold at the top, hints dim, like
        // the file tree's placeholder.
        assert_eq!(buf[(16, 1)].symbol(), "i");
        assert_eq!(buf[(16, 1)].fg, Color::White);
        assert!(buf[(16, 1)].modifier.contains(Modifier::BOLD));
        assert_eq!(buf[(16, 4)].symbol(), "C");
        assert_eq!(buf[(16, 4)].fg, Color::DarkGray);
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
}

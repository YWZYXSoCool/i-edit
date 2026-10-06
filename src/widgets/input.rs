use std::borrow::Cow;

use crate::Cursor;
use crate::component::Component;
use crate::text::TextState;

use crossterm::event::{Event, KeyCode, KeyEvent};
use ratatui::{
    prelude::*,
    widgets::{Block, Borders, Widget},
};

/// State of an [`Input`]: one line of text and where the cursor sits in it.
///
/// Editing is delegated to [`TextState`], so the box treats non-ASCII text the
/// same way the editor does.
#[derive(Debug, Clone, Default)]
pub struct InputState {
    /// Holds exactly one line: this box is single-line by construction, its key
    /// map never inserts a line break.
    pub(crate) text: TextState,
    pub is_editing: bool,
}

impl InputState {
    /// The text in the box.
    pub fn text(&self) -> &str {
        &self.text.lines[0]
    }

    /// Byte offset of the cursor in [`text`](Self::text), always on a char
    /// boundary.
    pub fn cursor_position(&self) -> usize {
        self.text.cursor.x
    }

    /// Replaces the text and puts the cursor at its end.
    ///
    /// Reuses the single line buffer instead of replacing the whole `Vec`.
    pub fn set_text(&mut self, text: impl Into<String>) {
        if self.text.lines.is_empty() {
            self.text.lines.push(String::new());
        }

        self.text.lines[0] = text.into();
        self.text.lines.truncate(1);
        self.text.cursor = Cursor {
            x: self.text.lines[0].len(),
            y: 0,
        };
    }

    /// Forgets the text and resets the cursor.
    ///
    /// Keeps the line buffer allocated; the end state matches
    /// [`TextState::default`].
    pub fn clear(&mut self) {
        self.text.lines.truncate(1);

        if self.text.lines.is_empty() {
            self.text.lines.push(String::new());
        }

        self.text.lines[0].clear();
        self.text.cursor = Cursor::default();
    }

    /// Takes the text, leaving the box empty.
    pub fn take_text(&mut self) -> String {
        let text = core::mem::take(&mut self.text.lines[0]);
        self.text.cursor = Cursor::default();
        text
    }

    /// Applies one key press.
    ///
    /// Single-line rules: Enter is ignored (the owner decides what submitting
    /// means) and Up/Down collapse to the ends of the line.
    pub fn input(&mut self, ev: KeyEvent) {
        match ev.code {
            KeyCode::Char(c) => {
                self.text.insert_char(c);
            }
            KeyCode::Backspace => {
                self.text.delete_backward();
            }
            KeyCode::Delete => {
                self.text.delete_forward();
            }
            KeyCode::Up | KeyCode::Home => self.text.move_to_text_start(),
            KeyCode::Down | KeyCode::End => self.text.move_to_text_end(),
            KeyCode::Left => self.text.move_left(),
            KeyCode::Right => self.text.move_right(),
            _ => {}
        }
    }
}

pub struct Input<'a> {
    placeholder: Option<&'a str>,
    max_length: usize,
    password_mode: bool,
    style: Style,
    focus_style: Style,
    cursor_style: Style,
}

impl Default for Input<'_> {
    fn default() -> Self {
        Self::new()
    }
}

impl<'a> Input<'a> {
    pub fn new() -> Self {
        Self {
            placeholder: None,
            max_length: usize::MAX,
            password_mode: false,
            style: Style::default().fg(Color::Gray),
            focus_style: Style::default().fg(Color::White),
            cursor_style: Style::default().bg(Color::White).fg(Color::Black),
        }
    }

    pub fn placeholder(mut self, placeholder: &'a str) -> Self {
        self.placeholder = Some(placeholder);
        self
    }

    pub fn max_length(mut self, max_length: usize) -> Self {
        self.max_length = max_length;
        self
    }

    pub fn password_mode(mut self, enabled: bool) -> Self {
        self.password_mode = enabled;
        self
    }

    pub fn style(mut self, style: Style) -> Self {
        self.style = style;
        self
    }

    pub fn focus_style(mut self, style: Style) -> Self {
        self.focus_style = style;
        self
    }

    pub fn cursor_style(mut self, style: Style) -> Self {
        self.cursor_style = style;
        self
    }

    /// The text as shown, plus where the cursor sits in *that* string.
    ///
    /// Password mode replaces the text with one `*` per char, so the cursor
    /// offset has to move from bytes to chars along with it. The offset comes
    /// in as bytes because that is what [`TextState`] stores.
    fn display<'t>(&'t self, text: &'t str, cursor_x: usize) -> (Cow<'t, str>, usize) {
        if self.password_mode && !text.is_empty() {
            (
                Cow::Owned("*".repeat(text.chars().count())),
                text[..cursor_x].chars().count(),
            )
        } else {
            (Cow::Borrowed(text), cursor_x)
        }
    }
}

impl Component for Input<'_> {
    type State = InputState;

    fn handle_event(self, event: &Event, state: &mut Self::State) {
        let Some(key) = crate::utils::key_press(event) else {
            return;
        };

        state.input(key);
    }

    fn render(self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
        let current_style = if state.is_editing {
            self.focus_style
        } else {
            self.style
        };

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(current_style);

        block.render(area, buf);

        let inner_area = area.inner(Margin {
            horizontal: 1,
            vertical: 1,
        });

        let text = state.text();
        let cursor_x = state.text.cursor.clamped_to(text).x;
        let (display_text, cursor_pos) = self.display(text, cursor_x);

        if !display_text.is_empty() {
            let (_, after_cursor) = display_text.split_at(cursor_pos);

            let before_span = Span::styled(&display_text[..cursor_pos], current_style);

            let cursor_char = if cursor_pos < display_text.len() {
                after_cursor.chars().next().unwrap_or(' ')
            } else {
                ' '
            };
            let mut cursor_buf = [0u8; 4];
            let cursor_text: &str = cursor_char.encode_utf8(&mut cursor_buf);
            let cursor_span = Span::styled(cursor_text, self.cursor_style);

            let mut after_chars = after_cursor.chars();
            after_chars.next();
            let after_span = Span::styled(after_chars.as_str(), current_style);

            let line = Line::from(vec![before_span, cursor_span, after_span]);
            line.render(inner_area, buf);
        } else if state.is_editing {
            let cursor_span = Span::styled(" ", self.cursor_style);
            cursor_span.render(inner_area, buf);
        } else if let Some(placeholder) = self.placeholder {
            let placeholder_span = Span::styled(placeholder, Style::default().fg(Color::DarkGray));
            placeholder_span.render(inner_area, buf);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Input, InputState};
    use crate::component::Component;

    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;

    fn press(code: KeyCode) -> Event {
        Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn type_keys(state: &mut InputState, keys: &[KeyCode]) {
        for code in keys {
            Component::handle_event(Input::new(), &press(*code), state);
        }
    }

    fn render(input: Input<'_>, state: &mut InputState) -> Buffer {
        let area = Rect::new(0, 0, 12, 3);
        let mut buf = Buffer::empty(area);
        Component::render(input, area, &mut buf, state);
        buf
    }

    #[test]
    fn wide_chars_are_inserted_and_crossed_whole() {
        let mut state = InputState::default();
        type_keys(&mut state, &[KeyCode::Char('你'), KeyCode::Char('好')]);
        assert_eq!(state.text(), "你好");
        assert_eq!(state.cursor_position(), 6);

        type_keys(&mut state, &[KeyCode::Left]);
        assert_eq!(state.cursor_position(), 3);

        type_keys(&mut state, &[KeyCode::Char('a')]);
        assert_eq!(state.text(), "你a好");
        assert_eq!(state.cursor_position(), 4);
    }

    #[test]
    fn backspace_removes_a_whole_wide_char() {
        let mut state = InputState::default();
        type_keys(&mut state, &[KeyCode::Char('你'), KeyCode::Backspace]);

        assert_eq!(state.text(), "");
        assert_eq!(state.cursor_position(), 0);
    }

    #[test]
    fn delete_removes_the_wide_char_under_the_cursor() {
        let mut state = InputState::default();
        type_keys(
            &mut state,
            &[KeyCode::Char('你'), KeyCode::Left, KeyCode::Delete],
        );

        assert_eq!(state.text(), "");
        assert_eq!(state.cursor_position(), 0);
    }

    #[test]
    fn home_and_end_jump_over_wide_text() {
        let mut state = InputState::default();
        type_keys(&mut state, &[KeyCode::Char('你'), KeyCode::Char('a')]);

        type_keys(&mut state, &[KeyCode::Home]);
        assert_eq!(state.cursor_position(), 0);

        type_keys(&mut state, &[KeyCode::Right]);
        assert_eq!(state.cursor_position(), 3);

        type_keys(&mut state, &[KeyCode::End]);
        assert_eq!(state.cursor_position(), 4);
    }

    #[test]
    fn rendering_wide_text_does_not_panic() {
        let mut state = InputState::default();
        type_keys(&mut state, &[KeyCode::Char('你')]);

        let buf = render(Input::new(), &mut state);

        assert_eq!(buf[(1, 1)].symbol(), "你");
    }

    #[test]
    fn password_mode_masks_one_star_per_char() {
        let mut state = InputState::default();
        type_keys(&mut state, &[KeyCode::Char('你'), KeyCode::Char('a')]);

        let buf = render(Input::new().password_mode(true), &mut state);

        assert_eq!(buf[(1, 1)].symbol(), "*");
        assert_eq!(buf[(2, 1)].symbol(), "*");
    }
}

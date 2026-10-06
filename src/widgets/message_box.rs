use crate::component::Component;
use crate::icon;

use crossterm::event::Event;
use ratatui::buffer::Buffer;
use ratatui::layout::{Margin, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Widget};
use unicode_width::UnicodeWidthStr;

/// Messages kept in the stack at once. Pushing past the cap drops the oldest
/// one, so a misbehaving producer cannot grow the box without bound.
const MAX_MESSAGES: usize = 4;

/// Rows one message occupies: a line of text between two border rows.
const MESSAGE_HEIGHT: u16 = 3;

/// Blank columns between each border and the text.
const PADDING: u16 = 1;

/// What a message is about: picks its icon and its colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageKind {
    Info,
    Success,
    Warning,
    Error,
}

impl MessageKind {
    /// Glyph drawn in front of the text.
    pub const fn icon(self) -> &'static str {
        match self {
            Self::Info => icon::INFO,
            Self::Success => icon::SUCCESS,
            Self::Warning => icon::WARNING,
            Self::Error => icon::ERROR,
        }
    }

    /// Colour of the icon and the border.
    ///
    /// The text itself stays white: coloured text on an unknown terminal
    /// background is the first thing that becomes unreadable.
    pub const fn color(self) -> Color {
        match self {
            Self::Info => Color::Cyan,
            Self::Success => Color::Green,
            Self::Warning => Color::Yellow,
            Self::Error => Color::Red,
        }
    }
}

/// One entry in the box: a single line of text and how urgent it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub kind: MessageKind,
    pub text: String,
}

impl Message {
    pub fn new(kind: MessageKind, text: impl Into<String>) -> Self {
        Self {
            kind,
            text: text.into(),
        }
    }
}

/// The stack of messages drawn in the top-right corner.
///
/// Newest first: pushing puts the new message in the corner itself and every
/// older one moves a box down, so the stack grows away from the corner. The
/// whole stack lives in this single state — the app shell owns exactly one of
/// them, not one widget per message.
#[derive(Debug, Default)]
pub struct MessageBoxState {
    messages: Vec<Message>,
}

impl MessageBoxState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Puts `message` on top of the stack, dropping the oldest one at the cap.
    pub fn push(&mut self, message: Message) {
        self.messages.insert(0, message);
        self.messages.truncate(MAX_MESSAGES);
    }

    /// Pushes an informational message.
    pub fn info(&mut self, text: impl Into<String>) {
        self.push(Message::new(MessageKind::Info, text));
    }

    /// Pushes a success message.
    pub fn success(&mut self, text: impl Into<String>) {
        self.push(Message::new(MessageKind::Success, text));
    }

    /// Pushes a warning message.
    pub fn warning(&mut self, text: impl Into<String>) {
        self.push(Message::new(MessageKind::Warning, text));
    }

    /// Pushes an error message.
    pub fn error(&mut self, text: impl Into<String>) {
        self.push(Message::new(MessageKind::Error, text));
    }

    /// Removes the newest message, if any, and returns it.
    pub fn dismiss(&mut self) -> Option<Message> {
        if self.messages.is_empty() {
            None
        } else {
            Some(self.messages.remove(0))
        }
    }

    /// Removes every message.
    pub fn clear(&mut self) {
        self.messages.clear();
    }

    /// The messages, newest first.
    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    pub fn len(&self) -> usize {
        self.messages.len()
    }

    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }
}

/// Corner overlay for the [`MessageBoxState`] stack.
///
/// Each message gets a bordered box hugging its text, right-aligned to the
/// corner; the newest sits in the corner itself. Only the boxes that fit in
/// `area.height` are drawn, so the stack never spills out of the overlay.
#[derive(Debug, Default, Clone, Copy)]
pub struct MessageBox;

impl Component for MessageBox {
    type State = MessageBoxState;

    /// The box never reacts to input: it is a notification, not a prompt.
    /// Whatever key was pressed belongs to the component underneath it.
    fn handle_event(self, _event: &Event, _state: &mut Self::State) {}

    fn render(self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
        if state.is_empty() || area.height < MESSAGE_HEIGHT || area.width < 2 {
            return;
        }

        // Every box uses the widest message width, so the stack lines up into
        // a single column instead of a ragged edge.
        let width = state
            .messages()
            .iter()
            .map(message_width)
            .max()
            .unwrap_or(0)
            .min(area.width);

        // The corner belongs to the top of the stack: when there is no room
        // for everything, the newest messages win.
        let visible = state.len().min((area.height / MESSAGE_HEIGHT) as usize);

        for (row, message) in state.messages().iter().take(visible).enumerate() {
            let box_area = Rect {
                x: area.right() - width,
                y: area.y + row as u16 * MESSAGE_HEIGHT,
                width,
                height: MESSAGE_HEIGHT,
            };

            self.render_message(message, box_area, buf);
        }
    }
}

impl MessageBox {
    fn render_message(self, message: &Message, area: Rect, buf: &mut Buffer) {
        // Opaque: the editor must not show through the box.
        Clear.render(area, buf);

        let color = message.kind.color();
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::new().fg(color))
            .render(area, buf);

        let inner = area.inner(Margin {
            horizontal: 1,
            vertical: 1,
        });
        let text_area = inner.inner(Margin {
            horizontal: PADDING,
            vertical: 0,
        });

        Line::from(vec![
            Span::styled(message.kind.icon(), Style::new().fg(color)),
            Span::raw(" "),
            Span::styled(message.text.as_str(), Style::new().fg(Color::White)),
        ])
        .render(text_area, buf);
    }
}

/// Columns one box needs for `message`: two borders, the padding on each side,
/// the icon, the space after it and the text.
///
/// The text is measured in display columns, so CJK and other wide characters
/// do not push the right border out by a column.
fn message_width(message: &Message) -> u16 {
    let content = UnicodeWidthStr::width(message.kind.icon())
        + 1
        + UnicodeWidthStr::width(message.text.as_str());

    u16::try_from(content + 2 * PADDING as usize + 2).unwrap_or(u16::MAX)
}

#[cfg(test)]
mod tests {
    use super::{MAX_MESSAGES, Message, MessageBox, MessageBoxState, MessageKind, PADDING};
    use crate::component::Component;
    use crate::icon;

    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::style::Color;

    fn press(code: KeyCode) -> Event {
        Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    /// Pushes the `(kind, text)` pairs oldest first, exactly as listed.
    fn stack(entries: &[(MessageKind, &str)]) -> MessageBoxState {
        let mut state = MessageBoxState::new();

        for (kind, text) in entries {
            state.push(Message::new(*kind, *text));
        }

        state
    }

    fn render(state: &mut MessageBoxState, area: Rect) -> Buffer {
        let mut buf = Buffer::empty(area);
        Component::render(MessageBox, area, &mut buf, state);
        buf
    }

    /// Width of a single-box stack: two borders, two padding columns, the icon,
    /// the space after it and `text` (ASCII in these tests).
    fn expected_width(text: &str) -> u16 {
        2 + 2 * PADDING + 1 + 1 + text.len() as u16
    }

    #[test]
    fn pushing_puts_the_newest_message_on_top() {
        let state = stack(&[(MessageKind::Info, "first"), (MessageKind::Error, "second")]);

        assert_eq!(state.messages()[0].text, "second");
        assert_eq!(state.messages()[1].text, "first");
    }

    #[test]
    fn the_stack_drops_its_oldest_message_at_the_cap() {
        let mut state = MessageBoxState::new();

        for i in 0..MAX_MESSAGES + 1 {
            state.info(format!("message {i}"));
        }

        assert_eq!(state.len(), MAX_MESSAGES);
        assert_eq!(state.messages()[0].text, format!("message {MAX_MESSAGES}"));
        assert!(state.messages().iter().all(|m| m.text != "message 0"));
    }

    #[test]
    fn dismiss_takes_messages_off_the_top() {
        let mut state = stack(&[(MessageKind::Info, "first"), (MessageKind::Error, "second")]);

        assert_eq!(state.dismiss().unwrap().text, "second");
        assert_eq!(state.dismiss().unwrap().text, "first");
        assert!(state.dismiss().is_none());
        assert!(state.is_empty());
    }

    #[test]
    fn clear_empties_the_stack() {
        let mut state = stack(&[(MessageKind::Info, "first"), (MessageKind::Error, "second")]);

        state.clear();

        assert!(state.is_empty());
    }

    #[test]
    fn every_kind_has_its_own_icon_and_colour() {
        let kinds = [
            MessageKind::Info,
            MessageKind::Success,
            MessageKind::Warning,
            MessageKind::Error,
        ];

        for (i, kind) in kinds.iter().enumerate() {
            for other in &kinds[i + 1..] {
                assert_ne!(kind.icon(), other.icon());
                assert_ne!(kind.color(), other.color());
            }
        }
    }

    #[test]
    fn keys_leave_the_stack_alone() {
        let mut state = stack(&[(MessageKind::Info, "hi")]);

        Component::handle_event(MessageBox, &press(KeyCode::Esc), &mut state);
        Component::handle_event(MessageBox, &press(KeyCode::Char('x')), &mut state);

        assert_eq!(state.len(), 1);
    }

    #[test]
    fn the_newest_message_sits_in_the_top_right_corner() {
        let mut state = stack(&[(MessageKind::Info, "hi")]);
        let area = Rect::new(0, 0, 20, 6);

        let buf = render(&mut state, area);

        let x = area.width - expected_width("hi");
        assert_eq!(buf[(x, 0)].symbol(), "┌");
        assert_eq!(buf[(area.width - 1, 0)].symbol(), "┐");
        assert_eq!(buf[(x, 2)].symbol(), "└");
        assert_eq!(buf[(area.width - 1, 2)].symbol(), "┘");

        // Icon and text sit inside the padding, the icon in the kind's colour.
        assert_eq!(buf[(x + 2, 1)].symbol(), icon::INFO);
        assert_eq!(buf[(x + 2, 1)].style().fg, Some(Color::Cyan));
        assert_eq!(buf[(x + 4, 1)].symbol(), "h");
        assert_eq!(buf[(x + 4, 1)].style().fg, Some(Color::White));
    }

    #[test]
    fn older_messages_stack_downwards() {
        let mut state = stack(&[(MessageKind::Info, "hi"), (MessageKind::Error, "boom")]);
        let area = Rect::new(0, 0, 30, 8);

        let buf = render(&mut state, area);

        // All boxes share the widest width; the error is the newest.
        let x = area.width - expected_width("boom");
        assert_eq!(buf[(x + 2, 1)].symbol(), icon::ERROR);
        assert_eq!(buf[(x + 2, 1)].style().fg, Some(Color::Red));
        // The info box follows three rows below it.
        assert_eq!(buf[(x, 3)].symbol(), "┌");
        assert_eq!(buf[(area.width - 1, 3)].symbol(), "┐");
        assert_eq!(buf[(x + 2, 4)].symbol(), icon::INFO);
        assert_eq!(buf[(x + 2, 4)].style().fg, Some(Color::Cyan));
    }

    #[test]
    fn only_the_newest_messages_that_fit_are_drawn() {
        let mut state = stack(&[
            (MessageKind::Info, "one"),
            (MessageKind::Warning, "two"),
            (MessageKind::Error, "three"),
        ]);
        // Two boxes fit in eight rows; the oldest one must not be drawn.
        let area = Rect::new(0, 0, 20, 8);

        let buf = render(&mut state, area);

        assert_eq!(state.len(), 3);
        assert_eq!(buf[(area.width - 1, 0)].symbol(), "┐");
        assert_eq!(buf[(area.width - 1, 3)].symbol(), "┐");
        assert_eq!(buf[(area.width - 1, 6)].symbol(), " ");
    }

    #[test]
    fn long_messages_are_clipped_to_the_area() {
        let long = "a".repeat(100);
        let mut state = stack(&[(MessageKind::Warning, long.as_str())]);
        let area = Rect::new(0, 0, 20, 3);

        let buf = render(&mut state, area);

        // The box spans the whole width and its corners survive the clip.
        assert_eq!(buf[(0, 0)].symbol(), "┌");
        assert_eq!(buf[(0, 0)].style().fg, Some(Color::Yellow));
        assert_eq!(buf[(area.width - 1, 0)].symbol(), "┐");
        assert_eq!(buf[(0, 2)].symbol(), "└");
        assert_eq!(buf[(area.width - 1, 2)].symbol(), "┘");
    }

    #[test]
    fn an_empty_stack_draws_nothing() {
        let mut state = MessageBoxState::new();
        let area = Rect::new(0, 0, 20, 6);

        let buf = render(&mut state, area);

        assert_eq!(buf[(area.width - 1, 0)].symbol(), " ");
    }

    #[test]
    fn a_too_small_area_draws_nothing() {
        let mut state = stack(&[(MessageKind::Info, "hi")]);
        // Fewer rows than a single box needs.
        let area = Rect::new(0, 0, 20, 2);

        let buf = render(&mut state, area);

        assert_eq!(buf[(area.width - 1, 0)].symbol(), " ");
    }
}

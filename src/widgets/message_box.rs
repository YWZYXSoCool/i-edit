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
                y: area.bottom() - (row + 2) as u16 * MESSAGE_HEIGHT,
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

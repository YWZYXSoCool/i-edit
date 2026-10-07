use core::fmt::Write;

use crate::Cursor;
use crate::component::Component;
use crate::fixed_buf::FixedBuf;
use crate::icon;

use crossterm::event::{Event, KeyCode, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;
use unicode_width::UnicodeWidthStr;

/// Capacity of the scratch buffer used to format the last keystroke.
///
/// Must be large enough for the longest `KeyModifiers + KeyCode` pair, e.g.
/// `CONTROL+SHIFT+ALT+Backspace`. Overflowing writes fail instead of panicking.
const KEY_INFO_CAPACITY: usize = 32;

/// Capacity of the scratch buffer used to format the cursor position.
///
/// The longest result is `"Ln " + <line> + ", Col " + <column>`, and both
/// numbers are `usize`: 3 + 20 + 7 + 20 = 50 bytes, comfortably inside 64.
/// Overflowing writes fail instead of panicking.
const POSITION_INFO_CAPACITY: usize = 64;

/// Style applied to both halves of the bar.
const STATUS_STYLE: Style = Style::new().fg(Color::White).bg(Color::DarkGray);

/// Everything [`StatusBar`] needs to draw itself.
#[derive(Debug, Clone)]
pub struct StatusBarState {
    pub cursor: Cursor,
    pub last_key_modifiers: KeyModifiers,
    pub last_key_code: Option<KeyCode>,
    /// What a background worker is doing, e.g. `"Indexing 42%"`. `None` draws
    /// nothing.
    ///
    /// Owned rather than borrowed because it outlives the event that carried
    /// it: the read-out stays until the next one, or until the worker reports
    /// it is done. The string is reused in place, so a long run of progress
    /// updates does not allocate per update.
    progress: Option<String>,
}

// `KeyModifiers` has no `Default`, so hand-roll it.
impl Default for StatusBarState {
    fn default() -> Self {
        Self {
            cursor: Cursor::default(),
            last_key_modifiers: KeyModifiers::NONE,
            last_key_code: None,
            progress: None,
        }
    }
}

impl StatusBarState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets or clears the background-work read-out.
    ///
    /// The text is copied into the state's own buffer, which is reused when one
    /// is already there: progress arrives many times a second while a project
    /// is being indexed.
    pub fn set_progress(&mut self, text: Option<&str>) {
        match (text, &mut self.progress) {
            (None, _) => self.progress = None,
            (Some(text), Some(held)) => {
                held.clear();
                held.push_str(text);
            }
            (Some(text), None) => self.progress = Some(text.to_string()),
        }
    }

    /// The read-out as drawn, if there is one.
    pub fn progress(&self) -> Option<&str> {
        self.progress.as_deref()
    }

    /// Records a key press.
    ///
    /// Only modified keys are stored: bare keys would flood the bar with,
    /// e.g. every letter typed into the buffer.
    pub fn record_key(&mut self, modifiers: KeyModifiers, code: KeyCode) {
        if modifiers != KeyModifiers::NONE {
            self.last_key_modifiers = modifiers;
            self.last_key_code = Some(code);
        }
    }

    /// Drops the recorded key, e.g. when the editor regains focus.
    pub fn clear_key(&mut self) {
        self.last_key_modifiers = KeyModifiers::NONE;
        self.last_key_code = None;
    }

    /// Formats the recorded key as `MODIFIERS+Key`, or the bare `Key`.
    ///
    /// Returns an empty string when no modified key has been pressed yet.
    /// Overlong results are truncated rather than causing a panic.
    pub fn key_info(&self) -> FixedBuf<KEY_INFO_CAPACITY> {
        let mut buf = FixedBuf::new();

        if let Some(key_code) = self.last_key_code {
            if self.last_key_modifiers != KeyModifiers::NONE {
                let _ = write!(&mut buf, "{}+{}", self.last_key_modifiers, key_code);
            } else {
                let _ = write!(&mut buf, "{}", key_code);
            }
        }

        buf
    }
}

/// One line tall bar: buffer identity and keystroke hint on the left, cursor
/// position on the right.
#[derive(Debug)]
pub struct StatusBar<'a> {
    lines: &'a [String],
    dirty: bool,
    left_style: Style,
    right_style: Style,
}

impl<'a> StatusBar<'a> {
    pub fn new(lines: &'a [String]) -> Self {
        Self {
            lines,
            dirty: false,
            left_style: STATUS_STYLE,
            right_style: STATUS_STYLE,
        }
    }

    /// Marks the buffer as having unsaved changes, rendered as `[+]`.
    pub fn dirty(mut self, dirty: bool) -> Self {
        self.dirty = dirty;
        self
    }

    pub fn left_style(mut self, style: Style) -> Self {
        self.left_style = style;
        self
    }

    pub fn right_style(mut self, style: Style) -> Self {
        self.right_style = style;
        self
    }

    /// Builds the left half: buffer identity, dirty marker, then the recorded
    /// key.
    ///
    /// The identity always comes first so the file being edited stays visible
    /// no matter which key was pressed last. `key_info` is a caller-owned
    /// scratch buffer, so every span borrows from `self` or from it.
    fn left_line<'b>(
        &'b self,
        state: &'b StatusBarState,
        key_info: &'b FixedBuf<KEY_INFO_CAPACITY>,
    ) -> Line<'b> {
        let style = self.left_style;

        let mut spans = vec![];

        if self.dirty {
            spans.push(Span::styled("[+] ", style));
        }

        if !key_info.as_str().is_empty() {
            spans.push(Span::styled(icon::KEYBOARD, style));
            spans.push(Span::styled(" ", style));
            spans.push(Span::styled(key_info.as_str(), style));
            spans.push(Span::styled(" ", style));
        }

        if let Some(progress) = state.progress() {
            spans.push(Span::styled(progress, style));
            spans.push(Span::styled(" ", style));
        }

        Line::from(spans)
    }

    /// Formats the cursor position, both 1-based.
    ///
    /// The column counts display columns, not bytes, so a CJK or other wide
    /// character advances it by exactly one.
    fn position_info(&self, state: &StatusBarState) -> FixedBuf<POSITION_INFO_CAPACITY> {
        let cursor = &state.cursor;
        let line = self.lines.get(cursor.y).map_or("", |line| line.as_str());
        let display_col = cursor.clamped_to(line).display_col(line);

        let mut buf = FixedBuf::new();
        let _ = write!(&mut buf, "Ln {}, Col {}", cursor.y + 1, display_col + 1);

        buf
    }
}

impl Default for StatusBar<'_> {
    fn default() -> Self {
        Self {
            lines: &[],
            dirty: false,
            left_style: STATUS_STYLE,
            right_style: STATUS_STYLE,
        }
    }
}

impl Component for StatusBar<'_> {
    type State = StatusBarState;

    fn handle_event(self, event: &Event, state: &mut Self::State) {
        let Some(key) = crate::utils::key_press(event) else {
            return;
        };

        state.record_key(key.modifiers, key.code);
    }

    fn render(self, area: Rect, buf: &mut Buffer, state: &mut Self::State) {
        if area.is_empty() {
            return;
        }

        let position = self.position_info(state);
        // The cursor icon plus the space after it are one column each; every
        // icon is single-width, as `icon.rs` pins down with a test.
        let right_width = UnicodeWidthStr::width(position.as_str()) + 2;

        let layout =
            Layout::horizontal([Constraint::Fill(1), Constraint::Length(right_width as u16)]);
        let [left_area, right_area] = layout.areas(area);

        let key_info = state.key_info();
        self.left_line(state, &key_info).render(left_area, buf);

        let right_style = self.right_style;
        Line::from(vec![
            Span::styled(icon::CURSOR, right_style),
            Span::styled(" ", right_style),
            Span::styled(position.as_str(), right_style),
        ])
        .render(right_area, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::{POSITION_INFO_CAPACITY, StatusBar, StatusBarState};
    use crate::Cursor;
    use crate::fixed_buf::FixedBuf;

    fn position(lines: &[String], x: usize, y: usize) -> FixedBuf<POSITION_INFO_CAPACITY> {
        let state = StatusBarState {
            cursor: Cursor { x, y },
            ..Default::default()
        };

        StatusBar::new(lines).position_info(&state)
    }

    /// Rebuilds the left half as one string by concatenating the spans of the
    /// line that `render` draws.
    fn left_info(bar: StatusBar<'_>, state: &StatusBarState) -> String {
        let key_info = state.key_info();

        bar.left_line(state, &key_info)
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    #[test]
    fn ascii_column_is_byte_offset_plus_one() {
        let lines = vec![String::from("hello")];
        assert_eq!(position(&lines, 0, 0), "Ln 1, Col 1");
        assert_eq!(position(&lines, 5, 0), "Ln 1, Col 6");
    }

    #[test]
    fn wide_char_counts_as_one_column() {
        // Each of U+4F60 / U+597D occupies 2 columns but 3 bytes.
        let lines = vec![String::from("你好ab")];
        assert_eq!(position(&lines, 6, 0), "Ln 1, Col 5");
        assert_eq!(position(&lines, 8, 0), "Ln 1, Col 7");
    }

    #[test]
    fn out_of_range_offset_is_clamped() {
        let lines = vec![String::from("你好ab")];
        // x lands inside a 3-byte char; must clamp instead of panicking.
        assert_eq!(position(&lines, 1, 0), "Ln 1, Col 1");
        assert_eq!(position(&lines, usize::MAX, 0), "Ln 1, Col 7");
    }

    #[test]
    fn missing_line_reports_first_column() {
        let lines: Vec<String> = vec![];
        assert_eq!(position(&lines, 3, 7), "Ln 8, Col 1");
    }

    /// Guards the capacity derivation noted in the allocation plan's risk
    /// list: `"Ln " + <line> + ", Col " + <column>` is at most
    /// 3 + 20 + 7 + 20 = 50 bytes, so the 64-byte buffer never truncates.
    #[test]
    fn a_max_length_line_number_fits_the_position_buffer() {
        let digits = usize::MAX.to_string().len();
        assert!(POSITION_INFO_CAPACITY >= "Ln ".len() + digits + ", Col ".len() + digits);

        // `y = usize::MAX` would overflow the existing `cursor.y + 1` before
        // formatting, so the largest line stays one short of it; an extreme
        // `x` shows the clamped column cannot threaten the buffer either.
        let lines: Vec<String> = vec![];
        let info = position(&lines, usize::MAX, usize::MAX - 1);

        assert_eq!(info.as_str(), format!("Ln {}, Col 1", usize::MAX));
    }

    #[test]
    fn a_scratch_buffer_shows_no_name_or_dirty_marker() {
        let info = left_info(StatusBar::new(&[]), &StatusBarState::default());
        assert!(!info.contains("[+]"));
    }

    #[test]
    fn clearing_the_progress_removes_the_read_out() {
        let mut state = StatusBarState::default();
        state.set_progress(Some("Indexing 42%"));
        state.set_progress(None);

        assert_eq!(state.progress(), None);
        assert!(!left_info(StatusBar::new(&[]), &state).contains("Indexing"));
    }

    #[test]
    fn repeated_progress_reuses_one_buffer() {
        // The read-out changes many times a second while a project indexes;
        // each update must not be a new allocation.
        let mut state = StatusBarState::default();
        state.set_progress(Some("Indexing 0%"));

        let capacity = state.progress().map(str::len);
        for i in 0..50 {
            let text = format!("Indexing {i}%");
            state.set_progress(Some(&text));
            assert_eq!(state.progress(), Some(text.as_str()));
        }

        assert!(capacity.is_some());
    }
}

//! File picker overlay: `open file` / `open folder` / `save as`.
//!
//! All three modes share one component: the list and the browsing keys are the
//! same everywhere, and only the overlay title, the footer hint and what a
//! confirmed row means change with the mode.
//!
//! The path input *is* the browsing state: its text names the directory being
//! listed. That is what makes "type a path to jump anywhere" free — every text
//! edit re-lists the directory, and a path that does not exist simply lists
//! nothing instead of raising an error.
//!
//! The picker never dismisses itself and never answers `Esc`: the popup host
//! owns dismissal. Choosing a row emits an [`Action`] and leaves the decision
//! to open, load or overwrite to the shell.

use std::path::{Path, PathBuf};

use crate::action::{Action, Actions};
use crate::component::Component;
use crate::fs::{DirEntry, expand_path, list_dir};
use crate::icon;
use crate::widgets::{Input, InputState};

use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Margin, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Widget};
use unicode_width::UnicodeWidthChar;

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

/// Which overlay the picker is drawn as.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PickerMode {
    #[default]
    File,
    Folder,
    Save,
}

impl PickerMode {
    /// Title of the overlay; the mode is the only thing it depends on.
    const fn title(self) -> &'static str {
        match self {
            Self::File => "Open File",
            Self::Folder => "Open Folder",
            Self::Save => "Save As",
        }
    }

    const fn footer(self) -> &'static str {
        match self {
            Self::File => "Tab: complete   Enter: open   Esc: cancel",
            Self::Folder => "Tab: complete   Enter: choose   Esc: cancel",
            Self::Save => "Tab: switch   Enter: save   Ctrl+S: save   Esc: cancel",
        }
    }
}

/// Which save-mode field receives plain text keys.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum SaveFocus {
    #[default]
    Name,
    Path,
}

/// One line of the picker list.
#[derive(Debug, Clone)]
enum Row {
    /// The `..` row: walks to the parent directory.
    Parent,
    /// A real entry from [`list_dir`].
    Entry(DirEntry),
}

impl Row {
    /// Dotfiles are listed normally but drawn dim; they stay selectable.
    fn is_hidden(&self) -> bool {
        matches!(self, Row::Entry(entry) if entry.is_hidden)
    }
}

/// Everything the picker owns: the two inputs, the listed rows and the
/// requests on their way to the shell.
///
/// `Default` is an empty picker with no I/O, because `PopupState` derives
/// `Default` and must be able to sit unused until the shell opens it.
#[derive(Debug, Default)]
pub struct PickerState {
    mode: PickerMode,
    path_input: InputState,
    name_input: InputState,
    rows: Vec<Row>,
    highlighted: usize,
    scroll: usize,
    focus: SaveFocus,
    actions: Actions,
}

impl PickerState {
    /// `start_dir` is the initial directory (the shell picks it), `preset` is
    /// the current file: highlighted in `File` mode, used as the initial file
    /// name in `Save` mode.
    pub fn new(mode: PickerMode, start_dir: PathBuf, preset: Option<PathBuf>) -> Self {
        let mut state = Self {
            mode,
            ..Self::default()
        };

        state.path_input.set_text(start_dir.to_string_lossy());
        state.refresh();

        match mode {
            PickerMode::File => {
                state.path_input.is_editing = true;
                if let Some(preset) = preset {
                    state.highlight_preset(&preset);
                }
            }
            PickerMode::Folder => {
                state.path_input.is_editing = true;
            }
            PickerMode::Save => {
                // The current file is the natural save target, so its name is
                // what the name field starts with.
                if let Some(name) = preset.as_deref().and_then(Path::file_name) {
                    state.name_input.set_text(name.to_string_lossy());
                }
                state.set_save_focus(SaveFocus::Name);
            }
        }

        state
    }

    pub fn mode(&self) -> PickerMode {
        self.mode
    }

    /// Takes everything this component has asked for since the last drain.
    pub fn take_actions(&mut self) -> Vec<Action> {
        self.actions.drain()
    }

    fn handle_key(&mut self, key: KeyEvent) {
        match self.mode {
            PickerMode::File | PickerMode::Folder => self.handle_browse_key(key),
            PickerMode::Save => self.handle_save_key(key),
        }
    }

    /// Keys shared by the file and folder modes: the list is the same, only
    /// the meaning of `Enter` on a directory differs.
    fn handle_browse_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Up => self.select_previous(),
            KeyCode::Down => self.select_next(),
            KeyCode::Enter => self.activate_enter(),
            KeyCode::Right => self.enter_highlighted(),
            KeyCode::Left => self.go_parent(),
            KeyCode::Tab => self.complete_highlighted(),
            _ => {
                // Plain text keys edit the path; modified keys (Ctrl+..) are
                // not forwarded because `InputState` matches on `KeyCode`
                // alone and would insert them as characters.
                if is_text_key(&key) {
                    self.path_input.input(key);
                    self.refresh();
                }
            }
        }
    }

    fn handle_save_key(&mut self, key: KeyEvent) {
        if is_save_shortcut(&key) {
            self.confirm_save();
            return;
        }

        match key.code {
            KeyCode::Tab => self.toggle_save_focus(),
            _ => match self.focus {
                SaveFocus::Name => self.handle_name_key(key),
                SaveFocus::Path => self.handle_save_path_key(key),
            },
        }
    }

    fn handle_name_key(&mut self, key: KeyEvent) {
        if key.code == KeyCode::Enter {
            self.confirm_save();
        } else if is_text_key(&key) {
            self.name_input.input(key);
        }
    }

    /// Path/list focus in save mode: browsing behaves exactly like file mode,
    /// except that `Enter` on a file fills the name field instead of opening
    /// the file.
    fn handle_save_path_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Up => self.select_previous(),
            KeyCode::Down => self.select_next(),
            KeyCode::Enter => self.pick_for_save(),
            KeyCode::Right => self.enter_highlighted(),
            KeyCode::Left => self.go_parent(),
            _ => {
                if is_text_key(&key) {
                    self.path_input.input(key);
                    self.refresh();
                }
            }
        }
    }

    /// What `Enter` does to the highlighted row in file/folder mode.
    fn activate_enter(&mut self) {
        match self.highlighted_row() {
            Some(Row::Parent) => self.go_parent(),
            Some(Row::Entry(entry)) if entry.is_dir => match self.mode {
                // Folder mode chooses the directory; file mode browses into
                // it, because picking a folder for the *tree* is a different
                // intent from navigating there.
                PickerMode::Folder => self.actions.emit(Action::LoadFolder(entry.path)),
                _ => self.enter_dir(entry.path),
            },
            Some(Row::Entry(entry)) if self.mode == PickerMode::File => {
                self.actions.emit(Action::LoadFile(entry.path));
            }
            Some(Row::Entry(_)) => {}
            None => {}
        }
    }

    /// What `Enter` does in save mode with the path/list focused.
    fn pick_for_save(&mut self) {
        match self.highlighted_row() {
            Some(Row::Parent) => self.go_parent(),
            Some(Row::Entry(entry)) if entry.is_dir => self.enter_dir(entry.path),
            Some(Row::Entry(entry)) => {
                self.name_input.set_text(entry.name);
                self.set_save_focus(SaveFocus::Name);
            }
            None => {}
        }
    }

    /// `Right`: enter the highlighted directory, or walk up for `..`.
    fn enter_highlighted(&mut self) {
        match self.highlighted_row() {
            Some(Row::Parent) => self.go_parent(),
            Some(Row::Entry(entry)) if entry.is_dir => self.enter_dir(entry.path),
            _ => {}
        }
    }

    /// `Left` and the `..` row: make the path input its own parent.
    fn go_parent(&mut self) {
        let current = expand_path(self.path_input.text());
        if let Some(parent) = current.parent() {
            // `Path::new("")` has no parent; an empty lexical parent would
            // blank the input, so it is not a move worth making.
            if !parent.as_os_str().is_empty() {
                self.enter_dir(parent.to_path_buf());
            }
        }
    }

    /// Makes `path` the listed directory: the input text, then the refresh.
    fn enter_dir(&mut self, path: PathBuf) {
        self.path_input.set_text(path.to_string_lossy());
        self.refresh();
    }

    /// `Tab`: copy the highlighted row into the input, ending a directory with
    /// a separator so the refreshed listing shows its contents right away.
    fn complete_highlighted(&mut self) {
        let Some(row) = self.highlighted_row() else {
            return;
        };

        let text = match row {
            Row::Parent => {
                let current = expand_path(self.path_input.text());
                match current.parent() {
                    Some(parent) if !parent.as_os_str().is_empty() => with_separator(parent),
                    _ => return,
                }
            }
            Row::Entry(entry) if entry.is_dir => with_separator(&entry.path),
            Row::Entry(entry) => entry.path.to_string_lossy().into_owned(),
        };

        self.path_input.set_text(text);
        self.refresh();
    }

    fn select_previous(&mut self) {
        let len = self.rows.len();

        if len > 0 {
            self.highlighted = (self.highlighted + len - 1) % len;
        }
    }

    fn select_next(&mut self) {
        let len = self.rows.len();

        if len > 0 {
            self.highlighted = (self.highlighted + 1) % len;
        }
    }

    fn highlighted_row(&self) -> Option<Row> {
        self.rows.get(self.highlighted).cloned()
    }

    /// `Ctrl+S` or `Enter` in the name field: hand the target to the shell.
    fn confirm_save(&mut self) {
        let name = self.name_input.text().trim().to_owned();
        if name.is_empty() {
            // An empty file name cannot be joined into a path, so confirming
            // it is a no-op rather than an error.
            return;
        }

        let dir = expand_path(self.path_input.text());
        self.actions.emit(Action::SaveTo(dir.join(name)));
    }

    fn toggle_save_focus(&mut self) {
        let focus = match self.focus {
            SaveFocus::Name => SaveFocus::Path,
            SaveFocus::Path => SaveFocus::Name,
        };
        self.set_save_focus(focus);
    }

    /// The focused input is the yellow one with the cursor; this keeps that
    /// flag in one place so both inputs always agree with `focus`.
    fn set_save_focus(&mut self, focus: SaveFocus) {
        self.focus = focus;
        self.name_input.is_editing = focus == SaveFocus::Name;
        self.path_input.is_editing = focus == SaveFocus::Path;
    }

    /// Re-lists the directory the path input names.
    ///
    /// A missing or unreadable path is not an error here: showing an empty
    /// list lets the user keep editing the path, which is the whole point of
    /// the editable path box.
    fn refresh(&mut self) {
        let path = expand_path(self.path_input.text());

        self.rows = match list_dir(&path) {
            Ok(entries) => {
                let mut rows = Vec::with_capacity(entries.len() + 1);
                if path.parent().is_some() {
                    rows.push(Row::Parent);
                }
                rows.extend(entries.into_iter().map(Row::Entry));
                rows
            }
            Err(_) => Vec::new(),
        };

        // The old highlight pointed into the old listing; both reset to the
        // top so the picker never acts on a stale row.
        self.highlighted = 0;
        self.scroll = 0;
    }

    /// Puts the highlight on `preset` when the fresh listing contains it.
    fn highlight_preset(&mut self, preset: &Path) {
        if let Some(index) = self
            .rows
            .iter()
            .position(|row| matches!(row, Row::Entry(entry) if entry.path.as_path() == preset))
        {
            self.highlighted = index;
        }
    }

    fn has_entries(&self) -> bool {
        self.rows.iter().any(|row| matches!(row, Row::Entry(_)))
    }
}

/// The overlay: an opaque, bordered box with the path box on top, the listing
/// in the middle and the key hints at the bottom.
#[derive(Debug, Default, Clone, Copy)]
pub struct Picker;

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
fn row_spans(row: &Row, max_width: usize) -> Vec<Span<'_>> {
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
fn clip_to_width(text: &str, max_width: usize) -> &str {
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
fn with_separator(path: &Path) -> String {
    let mut text = path.to_string_lossy().into_owned();
    if !text.ends_with(std::path::MAIN_SEPARATOR) && !text.ends_with('/') {
        text.push(std::path::MAIN_SEPARATOR);
    }
    text
}

/// `InputState` dispatches on `KeyCode` alone, so a modified key such as
/// `Ctrl+S` would be inserted as a plain `s`. Only unmodified keys and shifted
/// characters (which arrive as their final glyph) are safe to forward.
fn is_text_key(key: &KeyEvent) -> bool {
    key.modifiers == KeyModifiers::NONE || key.modifiers == KeyModifiers::SHIFT
}

fn is_save_shortcut(key: &KeyEvent) -> bool {
    key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('s' | 'S'))
}

#[cfg(test)]
mod tests {
    use super::{Picker, PickerMode, PickerState, Row, clip_to_width, row_spans, with_separator};
    use crate::action::Action;
    use crate::component::Component;
    use crate::fs::DirEntry;
    use crate::icon;

    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};

    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::style::Color;
    use ratatui::text::Span;
    use unicode_width::UnicodeWidthStr;

    /// A unique directory that deletes itself, so tests can run in parallel
    /// and leave nothing behind even when they panic.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let unique = NEXT.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "i-edit-picker-test-{}-{}",
                std::process::id(),
                unique
            ));

            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn press(code: KeyCode) -> Event {
        Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn ctrl_s() -> Event {
        Event::Key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL))
    }

    fn picker(mode: PickerMode, dir: &Path, preset: Option<PathBuf>) -> PickerState {
        let mut state = PickerState::new(mode, dir.to_path_buf(), preset);
        state.take_actions();
        state
    }

    fn key(state: &mut PickerState, event: Event) {
        Component::handle_event(Picker, &event, state);
    }

    fn type_text(state: &mut PickerState, text: &str) {
        for ch in text.chars() {
            key(state, press(KeyCode::Char(ch)));
        }
    }

    fn text_of(path: &Path) -> String {
        path.to_string_lossy().into_owned()
    }

    fn row_names(state: &PickerState) -> Vec<&str> {
        state
            .rows
            .iter()
            .filter_map(|row| match row {
                Row::Entry(entry) => Some(entry.name.as_str()),
                Row::Parent => None,
            })
            .collect()
    }

    /// Moves the highlight onto the entry named `name` with the real
    /// navigation keys, so wrap-around is exercised as a side effect.
    fn select(state: &mut PickerState, name: &str) {
        let index = state
            .rows
            .iter()
            .position(|row| matches!(row, Row::Entry(entry) if entry.name == name))
            .unwrap_or_else(|| panic!("no row named {name:?}"));

        for _ in 0..=state.rows.len() {
            if state.highlighted == index {
                return;
            }
            key(state, press(KeyCode::Down));
        }
        panic!("never reached {name:?}");
    }

    fn select_parent(state: &mut PickerState) {
        let index = state
            .rows
            .iter()
            .position(|row| matches!(row, Row::Parent))
            .expect("no parent row");

        for _ in 0..=state.rows.len() {
            if state.highlighted == index {
                return;
            }
            key(state, press(KeyCode::Down));
        }
        panic!("never reached the parent row");
    }

    fn render(state: &mut PickerState, area: Rect) -> Buffer {
        let mut buf = Buffer::empty(area);
        Component::render(Picker, area, &mut buf, state);
        buf
    }

    fn buffer_contains(buf: &Buffer, area: Rect, needle: &str) -> bool {
        (area.y..area.bottom()).any(|y| {
            let row: String = (area.x..area.right())
                .map(|x| buf[(x, y)].symbol())
                .collect();
            row.contains(needle)
        })
    }

    /// Whether any cell showing `symbol` uses `color` as its foreground.
    fn any_fg_of_symbol(buf: &Buffer, area: Rect, symbol: &str, color: Color) -> bool {
        (area.y..area.bottom()).any(|y| {
            (area.x..area.right())
                .any(|x| buf[(x, y)].symbol() == symbol && buf[(x, y)].style().fg == Some(color))
        })
    }

    fn file_row(name: &str) -> Row {
        Row::Entry(DirEntry {
            path: PathBuf::new(),
            name: name.to_string(),
            is_dir: false,
            is_hidden: false,
        })
    }

    fn dir_row(name: &str) -> Row {
        Row::Entry(DirEntry {
            path: PathBuf::new(),
            name: name.to_string(),
            is_dir: true,
            is_hidden: false,
        })
    }

    fn span_texts<'a>(spans: &'a [Span<'a>]) -> Vec<&'a str> {
        spans.iter().map(|span| span.content.as_ref()).collect()
    }

    /// The visible text of one buffer row, advancing by each symbol's display
    /// width so the placeholder cell behind a wide glyph is not duplicated.
    fn visible_text(buf: &Buffer, area: Rect, y: u16) -> String {
        let mut text = String::new();
        let mut x = area.x;
        while x < area.right() {
            let symbol = buf[(x, y)].symbol();
            text.push_str(symbol);
            x = x.saturating_add(UnicodeWidthStr::width(symbol).max(1) as u16);
        }
        text
    }

    #[test]
    fn entering_a_directory_refreshes_the_rows() {
        let dir = TempDir::new();
        let sub = dir.path().join("sub");
        fs::create_dir(&sub).unwrap();
        fs::write(sub.join("inner.txt"), "x").unwrap();

        let mut state = picker(PickerMode::File, dir.path(), None);
        assert!(!row_names(&state).contains(&"inner.txt"));

        select(&mut state, "sub");
        key(&mut state, press(KeyCode::Enter));

        assert_eq!(state.path_input.text(), text_of(&sub).as_str());
        assert!(row_names(&state).contains(&"inner.txt"));
        assert_eq!(state.highlighted, 0);
        assert!(state.take_actions().is_empty());
    }

    #[test]
    fn enter_on_a_file_emits_load_file() {
        let dir = TempDir::new();
        let file = dir.path().join("a.txt");
        fs::write(&file, "x").unwrap();

        let mut state = picker(PickerMode::File, dir.path(), None);
        select(&mut state, "a.txt");
        key(&mut state, press(KeyCode::Enter));

        assert_eq!(state.take_actions(), vec![Action::LoadFile(file)]);
    }

    #[test]
    fn folder_mode_picks_a_directory_and_walks_up_without_emitting() {
        let dir = TempDir::new();
        let sub = dir.path().join("sub");
        fs::create_dir(&sub).unwrap();

        let mut state = picker(PickerMode::Folder, dir.path(), None);
        select(&mut state, "sub");
        key(&mut state, press(KeyCode::Enter));
        assert_eq!(state.take_actions(), vec![Action::LoadFolder(sub.clone())]);

        // Right browses into the directory without choosing it.
        key(&mut state, press(KeyCode::Right));
        assert_eq!(state.path_input.text(), text_of(&sub).as_str());
        assert!(state.take_actions().is_empty());

        // `..` navigates to the parent and emits nothing.
        select_parent(&mut state);
        key(&mut state, press(KeyCode::Enter));
        assert_eq!(state.path_input.text(), text_of(dir.path()).as_str());
        assert!(state.take_actions().is_empty());
    }

    #[test]
    fn folder_mode_enter_on_a_file_does_nothing() {
        let dir = TempDir::new();
        fs::write(dir.path().join("a.txt"), "x").unwrap();

        let mut state = picker(PickerMode::Folder, dir.path(), None);
        select(&mut state, "a.txt");
        key(&mut state, press(KeyCode::Enter));

        assert!(state.take_actions().is_empty());
    }

    #[test]
    fn save_mode_confirms_with_ctrl_s_and_ignores_an_empty_name() {
        let dir = TempDir::new();

        // Empty name: confirming must not emit anything.
        let mut state = picker(PickerMode::Save, dir.path(), None);
        key(&mut state, ctrl_s());
        assert!(state.take_actions().is_empty());

        // The name is trimmed before it becomes a path component.
        type_text(&mut state, "  report.txt  ");
        key(&mut state, ctrl_s());
        assert_eq!(
            state.take_actions(),
            vec![Action::SaveTo(dir.path().join("report.txt"))]
        );
    }

    #[test]
    fn save_mode_confirms_from_the_path_focus_too() {
        let dir = TempDir::new();

        let mut state = picker(PickerMode::Save, dir.path(), None);
        type_text(&mut state, "target.txt");
        key(&mut state, press(KeyCode::Tab));
        key(&mut state, ctrl_s());

        assert_eq!(
            state.take_actions(),
            vec![Action::SaveTo(dir.path().join("target.txt"))]
        );
    }

    #[test]
    fn save_mode_enter_on_a_file_fills_the_name_field() {
        let dir = TempDir::new();
        let file = dir.path().join("chapter.txt");
        fs::write(&file, "x").unwrap();

        let mut state = picker(PickerMode::Save, dir.path(), None);
        key(&mut state, press(KeyCode::Tab));
        select(&mut state, "chapter.txt");
        key(&mut state, press(KeyCode::Enter));

        assert_eq!(state.name_input.text(), "chapter.txt");
        assert!(state.take_actions().is_empty());

        // Focus moved to the name field, where Enter confirms.
        key(&mut state, press(KeyCode::Enter));
        assert_eq!(state.take_actions(), vec![Action::SaveTo(file)]);
    }

    #[test]
    fn save_mode_prefills_the_name_from_the_preset() {
        let dir = TempDir::new();
        let preset = dir.path().join("draft.md");

        let mut state = PickerState::new(PickerMode::Save, dir.path().to_path_buf(), Some(preset));

        assert_eq!(state.mode(), PickerMode::Save);
        assert_eq!(state.name_input.text(), "draft.md");
        assert!(state.take_actions().is_empty());
    }

    #[test]
    fn file_mode_highlights_the_preset() {
        let dir = TempDir::new();
        let preset = dir.path().join("b.txt");
        fs::write(&preset, "x").unwrap();

        let state = PickerState::new(
            PickerMode::File,
            dir.path().to_path_buf(),
            Some(preset.clone()),
        );
        let index = state
            .rows
            .iter()
            .position(|row| matches!(row, Row::Entry(entry) if entry.path == preset))
            .unwrap();

        assert_eq!(state.highlighted, index);
    }

    #[test]
    fn the_parent_row_is_listed_first() {
        let dir = TempDir::new();
        let mut state = picker(PickerMode::File, dir.path(), None);

        assert!(matches!(state.rows.first(), Some(Row::Parent)));

        let area = Rect::new(0, 0, 40, 12);
        let buf = render(&mut state, area);
        assert!(buffer_contains(&buf, area, ".."));
    }

    #[test]
    fn hidden_entries_are_flagged_and_rendered_dim() {
        let dir = TempDir::new();
        fs::write(dir.path().join(".hidden"), "x").unwrap();

        let mut state = picker(PickerMode::File, dir.path(), None);
        let hidden = state
            .rows
            .iter()
            .find_map(|row| match row {
                Row::Entry(entry) if entry.name == ".hidden" => Some(entry),
                _ => None,
            })
            .expect("hidden file not listed");
        assert!(hidden.is_hidden);

        let area = Rect::new(0, 0, 60, 20);
        let buf = render(&mut state, area);

        assert!(buffer_contains(&buf, area, ".hidden"));
        assert!(any_fg_of_symbol(&buf, area, ".", Color::DarkGray));
    }

    #[test]
    fn an_empty_directory_shows_the_empty_marker() {
        let dir = TempDir::new();
        let empty = dir.path().join("empty");
        fs::create_dir(&empty).unwrap();

        let mut state = picker(PickerMode::File, &empty, None);
        assert!(state.rows.iter().any(|row| matches!(row, Row::Parent)));

        let area = Rect::new(0, 0, 40, 12);
        let buf = render(&mut state, area);
        assert!(buffer_contains(&buf, area, "(empty)"));

        // A buffer too small for the overlay must stay panic-free.
        let tiny = Rect::new(0, 0, 4, 2);
        let mut tiny_buf = Buffer::empty(tiny);
        Component::render(Picker, tiny, &mut tiny_buf, &mut state);
    }

    #[test]
    fn a_missing_directory_lists_nothing() {
        let dir = TempDir::new();
        let missing = dir.path().join("nope");

        let mut state = picker(PickerMode::File, &missing, None);
        assert!(state.rows.is_empty());

        key(&mut state, press(KeyCode::Enter));
        key(&mut state, press(KeyCode::Tab));
        key(&mut state, press(KeyCode::Right));
        assert!(state.take_actions().is_empty());

        let _ = render(&mut state, Rect::new(0, 0, 30, 10));
    }

    #[test]
    fn the_highlight_wraps_around() {
        let dir = TempDir::new();
        fs::write(dir.path().join("a.txt"), "x").unwrap();
        fs::write(dir.path().join("b.txt"), "x").unwrap();

        let mut state = picker(PickerMode::File, dir.path(), None);
        let last = state.rows.len() - 1;

        key(&mut state, press(KeyCode::Up));
        assert_eq!(state.highlighted, last);

        key(&mut state, press(KeyCode::Down));
        assert_eq!(state.highlighted, 0);
    }

    #[test]
    fn rendering_scrolls_to_keep_the_highlight_visible() {
        let dir = TempDir::new();
        for i in 0..20 {
            fs::write(dir.path().join(format!("f{i:02}.txt")), "x").unwrap();
        }

        let mut state = picker(PickerMode::File, dir.path(), None);
        let target = state
            .rows
            .iter()
            .position(|row| matches!(row, Row::Entry(entry) if entry.name == "f15.txt"))
            .unwrap();
        for _ in 0..target {
            key(&mut state, press(KeyCode::Down));
        }
        assert_eq!(state.highlighted, target);

        let area = Rect::new(0, 0, 40, 12);
        let buf = render(&mut state, area);

        assert!(buffer_contains(&buf, area, "f15.txt"));
        assert!(!buffer_contains(&buf, area, "f00.txt"));
    }

    #[test]
    fn tab_completes_a_directory_with_a_separator() {
        let dir = TempDir::new();
        let sub = dir.path().join("sub");
        fs::create_dir(&sub).unwrap();
        fs::write(sub.join("inner.txt"), "x").unwrap();

        let mut state = picker(PickerMode::File, dir.path(), None);
        select(&mut state, "sub");
        key(&mut state, press(KeyCode::Tab));

        assert_eq!(state.path_input.text(), with_separator(&sub).as_str());
        assert!(row_names(&state).contains(&"inner.txt"));
        assert!(state.take_actions().is_empty());
    }

    #[test]
    fn tab_completes_a_file_without_a_separator() {
        let dir = TempDir::new();
        let file = dir.path().join("a.txt");
        fs::write(&file, "x").unwrap();

        let mut state = picker(PickerMode::File, dir.path(), None);
        select(&mut state, "a.txt");
        key(&mut state, press(KeyCode::Tab));

        assert_eq!(state.path_input.text(), text_of(&file).as_str());
    }

    #[test]
    fn left_walks_to_the_parent_directory() {
        let dir = TempDir::new();
        let sub = dir.path().join("sub");
        fs::create_dir(&sub).unwrap();

        let mut state = picker(PickerMode::File, &sub, None);
        key(&mut state, press(KeyCode::Left));

        assert_eq!(state.path_input.text(), text_of(dir.path()).as_str());
        assert!(state.take_actions().is_empty());
    }

    /// A name is clipped on character boundaries: a double-width glyph that
    /// does not fit whole is dropped, never left hanging over the row's edge.
    #[test]
    fn clipping_stops_in_front_of_a_wide_glyph_that_does_not_fit() {
        assert_eq!(clip_to_width("中文名", 5), "中文");
        assert_eq!(clip_to_width("中文名", 3), "中");
        assert_eq!(clip_to_width("中文名", 1), "");
        // Zero-width marks ride along with the character before them.
        assert_eq!(clip_to_width("e\u{301}x", 1), "e\u{301}");
    }

    #[test]
    fn row_spans_spend_the_width_in_order_and_drop_the_suffix_first() {
        let file = file_row("中文名字.txt");
        let spans = row_spans(&file, 8);
        assert_eq!(span_texts(&spans), [" ", " ", icon::FILE, " ", "中文"]);

        // A directory at a budget that just fits the name loses its `/` first.
        let dir = dir_row("中文名");
        let spans = row_spans(&dir, 10);
        assert_eq!(
            span_texts(&spans),
            [" ", icon::CHEVRON, icon::FOLDER, " ", "中文名"]
        );

        let spans = row_spans(&Row::Parent, 40);
        assert_eq!(
            span_texts(&spans),
            [" ", icon::CHEVRON, icon::FOLDER, " ", ".."]
        );
    }

    #[test]
    fn a_wide_name_cut_at_the_right_edge_stays_whole() {
        let dir = TempDir::new();
        let name = "中文文件名.txt";
        fs::write(dir.path().join(name), "x").unwrap();

        let mut state = picker(PickerMode::File, dir.path(), None);
        select(&mut state, name);

        // The picker is a percentage of the area, so sweep a few widths: the
        // name may be shortened, but only on character boundaries.
        for width in 10..=30 {
            let area = Rect::new(0, 0, width, 10);
            let buf = render(&mut state, area);

            let row = (area.y..area.bottom())
                .map(|y| visible_text(&buf, area, y))
                .find(|row| row.contains('中'))
                .unwrap_or_else(|| panic!("name row missing at width {width}"));

            // Everything from the first glyph of the name on, up to the right
            // border: a whole-character prefix, never a split one.
            let visible = &row[row.find('中').unwrap()..];
            let visible = visible.split('│').next().unwrap().trim_end();
            assert!(
                name.starts_with(visible),
                "width {width} showed {visible:?}"
            );
        }

        // With enough room the whole name shows.
        let area = Rect::new(0, 0, 40, 12);
        let buf = render(&mut state, area);
        let row = (area.y..area.bottom())
            .map(|y| visible_text(&buf, area, y))
            .find(|row| row.contains('中'))
            .expect("name row missing at 40 columns");
        assert!(row.contains(name));
    }
}

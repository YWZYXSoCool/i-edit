//! The text area: a scrolling, multi-line view over a [`TextState`].
//!
//! The state behind it is split across three files, each with one job:
//!
//! * [`motion`] — cursor movements, decoupled from the keys that trigger them;
//! * [`history`] — the undo/redo stacks and the transactions that feed them;
//! * [`overlay`] — the spans a selection covers on the visible rows;
//! * [`welcome`] — the greeting drawn in an empty scratch buffer.
//!
//! What is left here is the view: how the viewport is scrolled, where the
//! terminal cursor goes, what a key press does to the buffer and which
//! [`Action`]s the editor has queued.

mod history;
mod motion;
mod overlay;
mod welcome;

#[cfg(test)]
mod tests;

use std::path::PathBuf;

use crate::Cursor;
use crate::action::{Action, Actions};
use crate::clipboard::{Clipboard, ClipboardBackendKind};
use crate::component::Component;
use crate::highlight::{LayerId, StyledRun, TextEdit};
use crate::storage::config::TabIndent;
use crate::text::{Edit, Selection, SelectionMode, TextState};
use crate::widgets::viewport::gutter_width;
use crate::widgets::{Viewport, ViewportState};

use crossterm::event::{Event, KeyCode, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::{Offset, Position, Rect};
use ratatui::text::Span;
use ratatui::widgets::{Scrollbar, ScrollbarOrientation, ScrollbarState, StatefulWidget, Widget};
use unicode_width::UnicodeWidthStr;

use history::History;
use motion::Motion;

/// The text area: a scrolling, multi-line view over a [`TextState`].
#[derive(Debug, Default)]
pub struct Editor;

impl Component for Editor {
    type State = EditorState;

    fn handle_event(self, event: &Event, state: &mut Self::State) {
        match event {
            // Terminal bracketed paste delivers the system clipboard contents
            // straight to the buffer.
            Event::Paste(text) => state.paste_text(text),
            _ => {
                let Some(key) = crate::utils::key_press(event) else {
                    return;
                };
                state.handle_key(key.code, key.modifiers);
            }
        }
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

        // The viewport dimensions are only known now, at render time, so the
        // vertical scrollbar's content length and thumb position must be
        // recomputed here. Without this, the first render (and any later render
        // after a resize or a freshly loaded file) would draw the scrollbar from
        // the stale default state until the next key press updates it.
        state.update_scrollbar_state();

        // Rebuild the selection overlay only when it changed (a selection was
        // made, extended, collapsed, or the cursor moved inside one). With no
        // selection the overlay is cleared so the editor renders byte-for-byte as
        // it did before this feature existed.
        if state.selection_dirty {
            if state.selection.is_active() {
                state.recompute_selection_overlay();
            } else {
                // Only the overlay is emptied. The coloring switch is left
                // alone because it is not the selection's to decide: the lower
                // layers may hold semantic tokens and diagnostic underlines,
                // and turning it off here would blank them on every cursor
                // move.
                state
                    .viewport_state
                    .highlights
                    .clear_layer(LayerId::Overlay);
            }
            state.selection_dirty = false;
        }

        // Render the viewport widget (content without scrollbars)
        let viewport = Viewport::new(&state.text.lines).active_line(state.text.cursor.y);
        StatefulWidget::render(viewport, content_area, buf, &mut state.viewport_state);

        // An empty scratch buffer is a blank page: greet the user the way vim
        // does instead of leaving it bare. The hints come from the shortcut
        // registry, so they always match the real bindings.
        if welcome::is_empty_scratch(state.path.as_deref(), &state.text.lines)
            && let Some(area) = welcome::welcome_area(content_area)
        {
            // One borrowed span per line: `Paragraph` would need an owned
            // `Text`, which clones every line on every frame.
            for (index, line) in welcome::welcome_lines().iter().enumerate() {
                let style = if index == 0 {
                    welcome::TITLE_STYLE
                } else {
                    welcome::HINT_STYLE
                };
                let row = Rect {
                    y: area.y + index as u16,
                    height: 1,
                    ..area
                };
                Span::styled(line.as_str(), style).render(row, buf);
            }
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
    /// Monotonic document version, bumped on every edit. Asynchronous color
    /// producers (a future LSP) stamp their responses with the version they
    /// were computed against; a mismatch means the data is stale and is dropped.
    pub version: u64,
    pub(crate) viewport_state: ViewportState,
    pub(crate) scrollbar_state: ScrollbarState,
    pub(crate) h_scrollbar_state: ScrollbarState,
    pub(crate) cursor_screen_pos: Option<Position>,
    /// The active text selection; `mode == None` means no selection.
    selection: Selection,
    /// What the Tab key inserts. A copy of the `tab_indent` setting, pushed in
    /// by the shell: a component handling a key sees only its own state, so it
    /// cannot ask storage what the user chose.
    pub tab_indent: TabIndent,
    /// Clipboard: mirrors to the system and to an internal register.
    clipboard: Clipboard,
    /// Whether the selection overlay must be recomputed before the next render.
    selection_dirty: bool,
    /// Reused buffer for [`Self::recompute_selection_overlay`], so steady-state
    /// recomputation allocates nothing beyond the first time.
    selection_rows: Vec<Vec<StyledRun>>,
    actions: Actions,
    history: History,
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

    /// Moves everything this component has asked for onto the end of `out`.
    pub fn take_actions_into(&mut self, out: &mut Vec<Action>) {
        self.actions.take_into(out)
    }

    /// Replaces the buffer with a freshly loaded file and resets the view.
    pub fn load_file(&mut self, path: PathBuf, lines: Vec<String>) {
        self.text.load(lines);
        // A freshly loaded buffer has no coloring yet; rebuild the index to the
        // new line count so stale runs from the previous file cannot leak in.
        self.viewport_state.highlights.reset(self.text.lines.len());
        // The text was replaced wholesale, so the buffer has moved on: an
        // in-flight color response was computed against the *old* text and is
        // now stale, and the language server must be told this version. Without
        // the bump, reloading a file that had never been edited would look
        // identical to the server's copy of it.
        self.version += 1;
        self.path = Some(path);
        self.dirty = false;
        // A new file is a clean slate: drop any history so undo cannot reach
        // into the previous buffer, and mark the (empty) history as saved.
        self.history.reset();
        self.viewport_state.scroll_x = 0;
        self.viewport_state.scroll_y = 0;
        self.viewport_state.curr_line = 0;
        self.scrollbar_state = ScrollbarState::default();
        self.h_scrollbar_state = ScrollbarState::default();
        self.cursor_screen_pos = None;
        // A freshly loaded file has no selection and no stale overlay.
        self.selection_clear();
    }

    /// Called after a successful write to disk.
    pub fn mark_saved(&mut self) {
        self.dirty = false;
        // Anchor the saved point to the current position in history, so an undo
        // that returns the buffer to this state clears `dirty` again.
        self.history.mark_saved();
    }

    pub fn handle_key(&mut self, code: KeyCode, modifiers: KeyModifiers) {
        use KeyModifiers as M;

        // 1) Copy / cut / paste / select-all. These must be intercepted before the
        //    character-insert fallback, which would otherwise swallow Ctrl+C / X /
        //    V / A as literal input.
        if modifiers.contains(M::CONTROL) {
            match code {
                KeyCode::Char('c') | KeyCode::Char('C') => {
                    self.copy();
                    return;
                }
                KeyCode::Char('x') | KeyCode::Char('X') => {
                    self.cut();
                    return;
                }
                KeyCode::Char('v') | KeyCode::Char('V') => {
                    self.paste();
                    return;
                }
                KeyCode::Char('a') | KeyCode::Char('A') => {
                    self.select_all();
                    return;
                }
                KeyCode::Char('z') | KeyCode::Char('Z') => {
                    // Ctrl+Z undoes; the widely expected Ctrl+Shift+Z redoes.
                    if modifiers.contains(M::SHIFT) {
                        self.redo();
                    } else {
                        self.undo();
                    }
                    return;
                }
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    self.redo();
                    return;
                }
                _ => {}
            }
        }
        // Shift+Insert pastes (xterm convention).
        if !modifiers.contains(M::CONTROL)
            && modifiers.contains(M::SHIFT)
            && code == KeyCode::Insert
        {
            self.paste();
            return;
        }

        // 2) Movement, or selection extension when Shift is held.
        if let Some(motion) = Motion::from(modifiers, code) {
            if modifiers.contains(M::SHIFT) {
                if !self.selection.is_active() {
                    self.selection_begin(SelectionMode::Char);
                }
                motion.apply(&mut self.text);
            } else {
                if self.selection.is_active() {
                    self.selection_clear();
                }
                motion.apply(&mut self.text);
            }
            self.after_move();
            return;
        }

        // 3) Editing. A live selection is replaced by whatever is typed. Every
        // edit is recorded; a single key press becomes one undo entry.
        let had_selection = self.selection.is_active();
        self.history.begin_txn();
        match (modifiers, code) {
            (_, KeyCode::Char(c)) => {
                if had_selection {
                    let e = self.text.delete_selection(&self.selection);
                    self.record(e);
                    self.selection_clear();
                }
                let e = self.text.insert_char(c);
                self.record(e);
            }
            (M::NONE, KeyCode::Enter) => {
                if had_selection {
                    let e = self.text.delete_selection(&self.selection);
                    self.record(e);
                    self.selection_clear();
                }
                let e = self.text.insert_new_line();
                self.record(e);
            }
            (M::NONE, KeyCode::Tab) => {
                // Tab inserts indentation instead of moving focus; focus cycling
                // is Shift+Tab (handled by the shell). A live selection is
                // replaced by the indentation, like any other typed text.
                if had_selection {
                    let e = self.text.delete_selection(&self.selection);
                    self.record(e);
                    self.selection_clear();
                }
                let e = self.text.insert_str(self.tab_indent.text());
                self.record(e);
            }
            (M::NONE, KeyCode::Backspace) => {
                if had_selection {
                    let e = self.text.delete_selection(&self.selection);
                    self.record(e);
                    self.selection_clear();
                } else {
                    let e = self.text.delete_backward();
                    self.record(e);
                }
            }
            (M::NONE, KeyCode::Delete) => {
                if had_selection {
                    let e = self.text.delete_selection(&self.selection);
                    self.record(e);
                    self.selection_clear();
                } else {
                    let e = self.text.delete_forward();
                    self.record(e);
                }
            }
            (M::NONE, KeyCode::Esc) => {
                self.selection_clear();
            }
            _ => {}
        }
        let changed = self.history.end_txn();

        if changed {
            self.after_edit();
        } else {
            // An unhandled key (or Esc) leaves the document untouched.
            self.sync();
        }
    }

    fn selection_begin(&mut self, mode: SelectionMode) {
        self.selection.anchor = self.text.cursor;
        self.selection.mode = mode;
        self.selection_dirty = true;
    }

    fn selection_clear(&mut self) {
        if self.selection.is_active() {
            self.selection.mode = SelectionMode::None;
            self.selection_dirty = true;
        }
    }

    /// Records an edit on the undo stack and carries the coloring across it.
    ///
    /// Every command that changes the buffer hands its [`Edit`] here, which is
    /// what lets the coloring layer move instead of restart: it sees each
    /// replacement as it happens, so the runs on screen describe the text that
    /// is on screen. A key press that replaces a selection records twice — a
    /// delete and an insert — and two moves in a row are as exact as one.
    fn record(&mut self, edit: Edit) {
        let change = text_edit(&edit);
        self.viewport_state.highlights.apply_edit(&change);
        self.history.record(edit);
    }

    /// Bookkeeping after a key press that changed the buffer: the document
    /// moved on, the selection has to be recomputed and the view resynced.
    fn after_edit(&mut self) {
        self.dirty = true;
        self.version += 1;
        self.selection_dirty = true;
        self.sync();
    }

    /// Paints the whole document with the colors a language server produced.
    ///
    /// Semantic tokens go into the [`Semantic`](LayerId::Semantic) layer: above
    /// the syntax layer, because they know more about the code, and below the
    /// overlay, because the selection must still win. `rows` covers every line
    /// of the buffer — a response is always for the whole document, so this is
    /// a wholesale replace, never a merge.
    pub(crate) fn set_semantic_tokens(&mut self, rows: &[Vec<StyledRun>]) {
        self.viewport_state
            .highlights
            .replace_layer(LayerId::Semantic, rows);
        // The switch is global to the coloring layer; a producer that has
        // something to paint is what turns it on.
        self.viewport_state.highlights.set_enabled(true);
    }

    /// Drops the colors a language server produced, e.g. after it went away.
    ///
    /// Turning the switch off is left to the caller's judgement: a selection
    /// may still be painted, and clearing one layer must not blank the other.
    pub(crate) fn clear_semantic_tokens(&mut self) {
        self.viewport_state
            .highlights
            .clear_layer(LayerId::Semantic);
    }

    /// Paints the underlines a language server reported.
    ///
    /// Diagnostics go into the [`Base`](LayerId::Base) layer — the lowest — and
    /// that is deliberate, not a compromise: they only ever set an underline
    /// color, never a foreground, so the semantic layer's colors show through
    /// them instead of being replaced by them. An empty `rows` clears them,
    /// which is how a fixed error stops being underlined.
    pub(crate) fn set_diagnostics(&mut self, rows: &[Vec<StyledRun>]) {
        self.viewport_state
            .highlights
            .replace_layer(LayerId::Base, rows);
        self.viewport_state.highlights.set_enabled(true);
    }

    /// Drops the underlines a language server reported.
    pub(crate) fn clear_diagnostics(&mut self) {
        self.viewport_state.highlights.clear_layer(LayerId::Base);
    }

    /// Records a pure movement / selection extension. The document is unchanged,
    /// so `dirty` is left alone; only the selection overlay and view are updated.
    fn after_move(&mut self) {
        self.selection_dirty = true;
        self.sync();
    }

    /// Reverts the most recent edit.
    pub fn undo(&mut self) {
        let Some(entry) = self.history.begin_undo() else {
            return;
        };
        // Undoing runs the recorded replacement backwards, so that is the
        // direction the coloring moves too.
        let undone = text_edit(&entry.edit).inverse();
        entry.edit.undo(&mut self.text);
        self.history.finish_undo(entry);
        self.viewport_state.highlights.apply_edit(&undone);
        self.after_history();
    }

    /// Re-applies the most recently undone edit.
    pub fn redo(&mut self) {
        let Some(entry) = self.history.begin_redo() else {
            return;
        };
        let redone = text_edit(&entry.edit);
        entry.edit.redo(&mut self.text);
        self.history.finish_redo(entry);
        self.viewport_state.highlights.apply_edit(&redone);
        self.after_history();
    }

    /// Bookkeeping shared by `undo`/`redo`: the document changed, so the view
    /// refreshes, the selection collapses, and `dirty` is recomputed against
    /// the last write — undoing back onto the saved state clears it again.
    fn after_history(&mut self) {
        self.version += 1;
        self.selection_clear();
        self.dirty = self.history.is_dirty();
        self.sync();
    }

    fn copy(&mut self) {
        let text = self.current_selection_or_line();
        self.clipboard.copy(&text);
        // Copying does not mutate the buffer; the selection stays so it can be
        // pasted again.
    }

    fn cut(&mut self) {
        let text = self.current_selection_or_line();
        self.clipboard.copy(&text);

        self.history.begin_txn();
        if self.selection.is_active() {
            let e = self.text.delete_selection(&self.selection);
            self.record(e);
        } else {
            // No selection: cut the current line (VS Code behaviour). The removed
            // span reaches into the neighbouring newline so the row disappears
            // instead of leaving a blank one behind; on the last line the newline
            // in front of it is taken instead.
            let y = self.text.cursor.y;
            let edit = if self.text.lines.len() == 1 {
                let end = Cursor {
                    x: self.text.lines[y].len(),
                    y,
                };
                self.text.replace(Cursor { x: 0, y }, end, "")
            } else if y + 1 < self.text.lines.len() {
                self.text
                    .replace(Cursor { x: 0, y }, Cursor { x: 0, y: y + 1 }, "")
            } else {
                let prev = Cursor {
                    x: self.text.lines[y - 1].len(),
                    y: y - 1,
                };
                let end = Cursor {
                    x: self.text.lines[y].len(),
                    y,
                };
                self.text.replace(prev, end, "")
            };
            self.record(edit);
        }
        self.history.end_txn();
        self.text.clamp_cursor();

        self.after_edit();
        self.selection_clear();
    }

    fn paste(&mut self) {
        let text = self.clipboard.paste_text();
        self.insert_recorded(&text);
    }

    /// Inserts text arriving from an external source (terminal bracketed paste).
    /// Does not touch the internal register, so a later Ctrl+V still pastes what
    /// *we* copied. An active selection is replaced, matching `paste`.
    fn paste_text(&mut self, text: &str) {
        self.insert_recorded(text);
    }

    /// Shared insertion path for paste / bracketed paste: an active selection is
    /// replaced, and the whole thing ("delete selection, insert") is one undo
    /// entry.
    fn insert_recorded(&mut self, text: &str) {
        self.history.begin_txn();
        if self.selection.is_active() {
            let e = self.text.delete_selection(&self.selection);
            self.record(e);
        }
        let e = self.text.insert_str(text);
        self.record(e);
        self.history.end_txn();

        self.after_edit();
        self.selection_clear();
    }

    fn select_all(&mut self) {
        let last = self.text.lines.len().saturating_sub(1);
        self.selection.anchor = Cursor { x: 0, y: 0 };
        self.text.cursor = Cursor {
            x: self.text.lines[last].len(),
            y: last,
        };
        self.selection.mode = SelectionMode::Line;
        self.after_move();
    }

    /// The text to copy or cut: the selection when active, else the current line
    /// with its trailing newline.
    fn current_selection_or_line(&self) -> String {
        if self.selection.is_active() {
            self.text.selected_text(&self.selection)
        } else {
            let line = &self.text.lines[self.text.cursor.y];
            let mut s = line.clone();
            s.push('\n');
            s
        }
    }

    /// Refreshes the selection overlay for the visible rows and hands it to the
    /// highlight stack. The spans themselves are computed by [`overlay`].
    fn recompute_selection_overlay(&mut self) {
        overlay::rebuild(
            &self.text.lines,
            &self.selection,
            self.text.cursor,
            self.viewport_state.scroll_y,
            self.viewport_state.height,
            &mut self.selection_rows,
        );

        self.viewport_state
            .highlights
            .replace_layer(LayerId::Overlay, &self.selection_rows);
        self.viewport_state.highlights.set_enabled(true);
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
            version: 0,
            tab_indent: TabIndent::default(),
            viewport_state: ViewportState::default(),
            scrollbar_state: ScrollbarState::default(),
            h_scrollbar_state: ScrollbarState::default(),
            cursor_screen_pos: None,
            selection: Selection::default(),
            clipboard: Clipboard::new(ClipboardBackendKind::Auto, Box::new(std::io::stdout())),
            selection_dirty: false,
            selection_rows: Vec::new(),
            actions: Actions::new(),
            history: History::new(),
        }
    }
}

/// The span an [`Edit`] replaced, in the terms the coloring layer speaks.
///
/// An [`Edit`] carries the two strings; what the coloring needs is the shape of
/// the hole they made — which rows it covered, which rows filled it, and how
/// far the surviving text on the first and last of them slid.
fn text_edit(edit: &Edit) -> TextEdit {
    // One newline means two rows are involved: the one the text starts on and
    // the one it ends on.
    let rows = |text: &str| text.matches('\n').count() + 1;
    // Where a side ended on its last row: everything after its final newline,
    // or — when it never left the first row — however far past `column` it
    // reached.
    let end = |text: &str, rows: usize| {
        if rows == 1 {
            edit.start.x + text.len()
        } else {
            text.rsplit('\n').next().unwrap_or("").len()
        }
    };

    let removed_rows = rows(&edit.removed);
    let inserted_rows = rows(&edit.inserted);

    TextEdit {
        line: edit.start.y,
        column: edit.start.x,
        removed_rows,
        inserted_rows,
        removed_to: end(&edit.removed, removed_rows),
        inserted_to: end(&edit.inserted, inserted_rows),
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

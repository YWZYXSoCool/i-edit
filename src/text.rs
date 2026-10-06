//! The text-editing core shared by [`Editor`](crate::widgets::Editor) and
//! [`Input`](crate::widgets::Input).
//!
//! The two widgets differ in which keys they claim and in how much of the text
//! they show, but the text under them behaves the same. Keeping it here means
//! cursor movement and every editing command exist once — including the
//! char-boundary arithmetic that non-ASCII text needs.
//!
//! Three pieces live here, each in its own file:
//!
//! * [`TextState`] — the buffer and its cursor (this file);
//! * [`Edit`] — the reversible unit [`TextState`] hands back, and the undo
//!   history is made of ([`edit`]);
//! * [`Selection`] — an anchor that reads against the live cursor
//!   ([`selection`]).

pub mod edit;
pub mod selection;

#[cfg(test)]
mod tests;

pub use edit::Edit;
pub use selection::{Selection, SelectionMode};

use crate::Cursor;
use crate::utils;

/// A document plus the cursor that edits it.
///
/// Always holds at least one line. `cursor.x` is a byte offset into the current
/// line, and every command here keeps it on a char boundary: slicing the line
/// for display and editing the `String` can never panic. `lines` is public so
/// widgets can read the text they draw; editing should go through the commands.
#[derive(Debug, Clone)]
pub struct TextState {
    pub lines: Vec<String>,
    pub(crate) cursor: Cursor,
}

impl Default for TextState {
    fn default() -> Self {
        Self {
            lines: vec![String::new()],
            cursor: Cursor::default(),
        }
    }
}

impl TextState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Display column of the cursor on the current line.
    pub fn display_col(&self) -> usize {
        self.cursor.display_col(&self.lines[self.cursor.y])
    }

    /// Replaces the whole buffer with `lines` and resets the cursor.
    ///
    /// `lines` may come straight from a file read; an empty vector still leaves
    /// the buffer with the single empty line every command assumes.
    pub fn load(&mut self, lines: Vec<String>) {
        self.lines = if lines.is_empty() {
            vec![String::new()]
        } else {
            lines
        };
        self.cursor = Cursor::default();
        self.clamp_cursor();
    }

    /// Keeps the cursor inside the buffer and on a char boundary.
    ///
    /// Needed after `lines` was written to from outside the commands below.
    pub fn clamp_cursor(&mut self) {
        if self.lines.is_empty() {
            self.lines.push(String::new());
        }

        self.cursor.y = self.cursor.y.min(self.lines.len() - 1);
        self.cursor = self.cursor.clamped_to(&self.lines[self.cursor.y]);
    }

    /// Inserts `c` at the cursor and steps over all of its bytes.
    pub fn insert_char(&mut self, c: char) -> Edit {
        let at = self.cursor;
        self.replace_owned(at, at, c.to_string())
    }

    /// Removes the char before the cursor; at a line start, joins the previous
    /// line.
    pub fn delete_backward(&mut self) -> Edit {
        let (x, y) = (self.cursor.x, self.cursor.y);

        if x > 0 {
            let prev = prev_char_boundary(&self.lines[y], x);
            return self.replace(Cursor { x: prev, y }, self.cursor, "");
        } else if y > 0 {
            // At a line start the edit removes the newline joining this line to
            // the previous one: the span runs from the previous line's end to
            // the cursor, and `replace` merges the two lines.
            let start = Cursor {
                x: self.lines[y - 1].len(),
                y: y - 1,
            };
            return self.replace(start, self.cursor, "");
        }

        Edit::identity(self.cursor)
    }

    /// Removes the char under the cursor; at a line end, pulls the next line up.
    pub fn delete_forward(&mut self) -> Edit {
        let (x, y) = (self.cursor.x, self.cursor.y);

        if x < self.lines[y].len() {
            let next = next_char_boundary(&self.lines[y], x);
            return self.replace(self.cursor, Cursor { x: next, y }, "");
        } else if y < self.lines.len() - 1 {
            return self.replace(self.cursor, Cursor { x: 0, y: y + 1 }, "");
        }

        Edit::identity(self.cursor)
    }

    /// Splits the current line at the cursor and steps onto the new line.
    pub fn insert_new_line(&mut self) -> Edit {
        let at = self.cursor;
        self.replace(at, at, "\n")
    }

    /// Moves one char left, wrapping to the end of the previous line.
    pub fn move_left(&mut self) {
        if self.cursor.x > 0 {
            self.cursor.x = prev_char_boundary(&self.lines[self.cursor.y], self.cursor.x);
        } else if self.cursor.y > 0 {
            self.cursor.y -= 1;
            self.cursor.x = self.lines[self.cursor.y].len();
        }
    }

    /// Moves one char right, wrapping to the start of the next line.
    pub fn move_right(&mut self) {
        if self.cursor.x < self.lines[self.cursor.y].len() {
            self.cursor.x = next_char_boundary(&self.lines[self.cursor.y], self.cursor.x);
        } else if self.cursor.y < self.lines.len() - 1 {
            self.cursor.y += 1;
            self.cursor.x = 0;
        }
    }

    /// Moves to line `y`, landing on the display column the cursor has now.
    fn move_to_line(&mut self, y: usize) {
        let display_col = self.display_col();
        self.cursor.y = y;
        self.cursor.x = utils::find_best_char_position(&self.lines[y], display_col);
    }

    /// Moves one line up, keeping the display column.
    pub fn move_up(&mut self) {
        if self.cursor.y > 0 {
            self.move_to_line(self.cursor.y - 1);
        }
    }

    /// Moves one line down, keeping the display column.
    pub fn move_down(&mut self) {
        if self.cursor.y + 1 < self.lines.len() {
            self.move_to_line(self.cursor.y + 1);
        }
    }

    /// Jumps `page_size` lines up, keeping the display column.
    pub fn move_page_up(&mut self, page_size: usize) {
        let y = self.cursor.y.saturating_sub(page_size);
        self.move_to_line(y);
    }

    /// Jumps `page_size` lines down, keeping the display column.
    pub fn move_page_down(&mut self, page_size: usize) {
        let y = (self.cursor.y + page_size).min(self.lines.len() - 1);
        self.move_to_line(y);
    }

    /// Moves to the start of the current line.
    pub fn move_to_line_start(&mut self) {
        self.cursor.x = 0;
    }

    /// Moves to the end of the current line.
    pub fn move_to_line_end(&mut self) {
        self.cursor.x = self.lines[self.cursor.y].len();
    }

    /// Moves to the very start of the text.
    pub fn move_to_text_start(&mut self) {
        self.cursor = Cursor::default();
    }

    /// Moves to the very end of the text.
    pub fn move_to_text_end(&mut self) {
        self.cursor.y = self.lines.len() - 1;
        self.cursor.x = self.lines[self.cursor.y].len();
    }

    /// Steps left to the start of the previous word, skipping whitespace.
    pub fn move_word_left(&mut self) {
        if self.cursor.x == 0 {
            if self.cursor.y > 0 {
                self.cursor.y -= 1;
                self.cursor.x = self.lines[self.cursor.y].len();
            }
            return;
        }

        let line = &self.lines[self.cursor.y];
        let mut pos = self.cursor.x;

        while let Some((idx, c)) = line[..pos].char_indices().next_back() {
            if !c.is_whitespace() {
                break;
            }
            pos = idx;
        }

        while let Some((idx, c)) = line[..pos].char_indices().next_back() {
            if !c.is_alphanumeric() && c != '_' {
                break;
            }
            pos = idx;
        }

        self.cursor.x = pos;
    }

    /// Steps right to the start of the next word, skipping whitespace.
    pub fn move_word_right(&mut self) {
        let line = &self.lines[self.cursor.y];

        if self.cursor.x >= line.len() {
            if self.cursor.y < self.lines.len() - 1 {
                self.cursor.y += 1;
                self.cursor.x = 0;
            }
            return;
        }

        let mut pos = self.cursor.x;

        while let Some(c) = line[pos..].chars().next() {
            if !c.is_alphanumeric() && c != '_' {
                break;
            }
            pos += c.len_utf8();
        }

        while let Some(c) = line[pos..].chars().next() {
            if c.is_whitespace() {
                pos += c.len_utf8();
            } else {
                break;
            }
        }

        self.cursor.x = pos.min(line.len());
    }

    pub fn replace(&mut self, start: Cursor, end: Cursor, new_text: &str) -> Edit {
        self.replace_owned(start, end, new_text.to_string())
    }

    /// As [`Self::replace`], taking ownership of the text to write. A caller that
    /// already holds a `String` (an encoded `char`, a building result) hands it
    /// over instead of paying for a second copy.
    pub fn replace_owned(&mut self, start: Cursor, end: Cursor, new_text: String) -> Edit {
        let before = self.cursor;
        let (start, end) = if (start.y, start.x) <= (end.y, end.x) {
            (start, end)
        } else {
            (end, start)
        };
        let removed = self.text_in_range(start, end);
        let after = self.apply_splice(start, &removed, &new_text);
        self.cursor = after;
        self.clamp_cursor();
        let edit = Edit::new(start, removed, new_text, before, self.cursor);
        edit.chainable_if_atomic()
    }

    pub(crate) fn apply_splice(&mut self, start: Cursor, removed: &str, inserted: &str) -> Cursor {
        let end = extend(start, removed);
        let end_y = end.y.min(self.lines.len() - 1);
        let end_x = end.x.min(self.lines[end_y].len());

        // Fast path: a span inside a single line that stays a single line. This
        // is every keystroke, so it edits the line in place rather than
        // rebuilding it — the shaping below would cost several allocations per
        // character typed.
        if start.y == end_y && !inserted.contains('\n') {
            let line = &mut self.lines[start.y];
            let from = start.x.min(line.len());
            let to = end_x.max(from);
            line.replace_range(from..to, inserted);
            return Cursor {
                y: start.y,
                x: from + inserted.len(),
            };
        }

        let head = self.lines[start.y][..start.x.min(self.lines[start.y].len())].to_string();
        let tail = self.lines[end_y][end_x..].to_string();
        let merged = format!("{head}{inserted}{tail}");
        let new_lines: Vec<String> = merged.split('\n').map(|s| s.to_string()).collect();
        self.lines.splice(start.y..=end_y, new_lines);

        extend(start, inserted)
    }

    fn text_in_range(&self, start: Cursor, end: Cursor) -> String {
        let (sy, sx, ey, ex) = (start.y, start.x, end.y, end.x);
        let sx = sx.min(self.lines[sy].len());
        let mut out = String::new();
        if sy == ey {
            out.push_str(&self.lines[sy][sx..ex.min(self.lines[sy].len())]);
        } else {
            out.push_str(&self.lines[sy][sx..]);
            for y in (sy + 1)..ey {
                out.push('\n');
                out.push_str(&self.lines[y]);
            }
            out.push('\n');
            out.push_str(&self.lines[ey][..ex.min(self.lines[ey].len())]);
        }
        out
    }
}

// ----- Selections --------------------------------------------------------
//
// `Selection` itself lives in `selection.rs`; what is left here is the part
// that needs the buffer: reading the selected text out and deleting it.

impl TextState {
    /// The text covered by `sel`, as it should land on the clipboard.
    ///
    /// In [`SelectionMode::Char`] rows between the ends are sliced by byte
    /// offset and a `\n` is inserted between them; a single-row selection gets
    /// no trailing newline. In [`SelectionMode::Line`] every covered row is
    /// emitted in full, each followed by `\n`.
    pub fn selected_text(&self, sel: &Selection) -> String {
        if !sel.is_active() {
            return String::new();
        }
        let (sy, sx, ey, ex) = sel.ordered(self.cursor);
        let mut out = String::new();

        match sel.mode {
            SelectionMode::Char => {
                for y in sy..=ey {
                    let line = &self.lines[y];
                    let (a, b) = if sy == ey {
                        (sx, ex)
                    } else if y == sy {
                        (sx, line.len())
                    } else if y == ey {
                        (0, ex)
                    } else {
                        (0, line.len())
                    };
                    let a = a.min(line.len());
                    let b = b.min(line.len());
                    out.push_str(&line[a..b]);
                    if y != ey {
                        out.push('\n');
                    }
                }
            }
            SelectionMode::Line => {
                for y in sy..=ey {
                    out.push_str(&self.lines[y]);
                    out.push('\n');
                }
            }
            SelectionMode::None => {}
        }
        out
    }

    /// Deletes the selection, leaving the cursor at the start of the removed
    /// span. Returns an identity edit when the selection is inactive.
    pub fn delete_selection(&mut self, sel: &Selection) -> Edit {
        if !sel.is_active() {
            return Edit::identity(self.cursor);
        }
        let (sy, sx, ey, ex) = sel.ordered(self.cursor);

        let start = Cursor { x: sx, y: sy };
        // Char mode removes the precise byte span. Line mode removes the whole
        // rows: the span runs to the start of the line *after* the selection
        // (so the rows below slide up). The last block of the buffer has no
        // following line, so there it collapses to a single empty line instead.
        let end = match sel.mode {
            SelectionMode::Line => {
                if ey + 1 < self.lines.len() {
                    Cursor { x: 0, y: ey + 1 }
                } else {
                    Cursor {
                        x: self.lines[ey].len(),
                        y: ey,
                    }
                }
            }
            _ => Cursor { x: ex, y: ey },
        };
        let edit = self.replace(start, end, "");
        self.clamp_cursor();
        edit
    }

    /// Inserts `text` at the cursor, splitting on `\n` so multi-line pastes
    /// become multiple lines. Lands the cursor at the end of the inserted text.
    pub fn insert_str(&mut self, text: &str) -> Edit {
        let at = self.cursor;
        self.replace(at, at, text)
    }
}

/// Where a cursor starting at `start` ends up after `text` is laid down there —
/// i.e. the position one past `text`'s last byte.
///
/// Written without collecting the split pieces: this runs on every keystroke,
/// and the `Vec` it would need is exactly the allocation we are trying to avoid.
fn extend(start: Cursor, text: &str) -> Cursor {
    match text.rfind('\n') {
        None => Cursor {
            y: start.y,
            x: start.x + text.len(),
        },
        Some(last_break) => Cursor {
            y: start.y + text.bytes().filter(|b| *b == b'\n').count(),
            x: text.len() - last_break - 1,
        },
    }
}

/// Byte offset where the char before `x` starts, `0` if there is none.
///
/// `x` is assumed to sit on a char boundary, as [`TextState`] keeps it.
fn prev_char_boundary(line: &str, x: usize) -> usize {
    line[..x]
        .char_indices()
        .next_back()
        .map_or(0, |(idx, _)| idx)
}

/// Byte offset where the char after `x` starts, `line.len()` at the end.
///
/// `x` is assumed to sit on a char boundary, as [`TextState`] keeps it.
fn next_char_boundary(line: &str, x: usize) -> usize {
    line[x..]
        .chars()
        .next()
        .map_or(line.len(), |c| x + c.len_utf8())
}

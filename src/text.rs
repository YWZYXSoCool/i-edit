//! The text-editing core shared by [`Editor`](crate::widgets::Editor) and
//! [`Input`](crate::widgets::Input).
//!
//! The two widgets differ in which keys they claim and in how much of the text
//! they show, but the text under them behaves the same. Keeping it here means
//! cursor movement and every editing command exist once — including the
//! char-boundary arithmetic that non-ASCII text needs.

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
    pub fn insert_char(&mut self, c: char) {
        let x = self.cursor.x;
        self.lines[self.cursor.y].insert(x, c);
        self.cursor.x += c.len_utf8();
    }

    /// Removes the char before the cursor; at a line start, joins the previous
    /// line.
    pub fn delete_backward(&mut self) {
        let (x, y) = (self.cursor.x, self.cursor.y);

        if x > 0 {
            let prev = prev_char_boundary(&self.lines[y], x);
            self.lines[y].remove(prev);
            self.cursor.x = prev;
        } else if y > 0 {
            let removed = self.lines.remove(y);
            self.cursor.y = y - 1;
            let join_at = self.lines[self.cursor.y].len();
            self.lines[self.cursor.y].push_str(&removed);
            self.cursor.x = join_at;
        }
    }

    /// Removes the char under the cursor; at a line end, pulls the next line up.
    pub fn delete_forward(&mut self) {
        let (x, y) = (self.cursor.x, self.cursor.y);

        if x < self.lines[y].len() {
            let next = next_char_boundary(&self.lines[y], x);
            self.lines[y].drain(x..next);
        } else if y < self.lines.len() - 1 {
            let next_line = self.lines.remove(y + 1);
            self.lines[y].push_str(&next_line);
        }
    }

    /// Splits the current line at the cursor and steps onto the new line.
    pub fn insert_new_line(&mut self) {
        let new_line = self.lines[self.cursor.y].split_off(self.cursor.x);

        self.cursor.x = 0;
        self.cursor.y += 1;
        self.lines.insert(self.cursor.y, new_line);
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
        let chars: Vec<(usize, char)> = line.char_indices().collect();
        let mut pos = self.cursor.x;

        while pos > 0 {
            let prev_char = chars.iter().rev().find(|&&(i, _)| i < pos);
            if let Some(&(_, c)) = prev_char {
                if c.is_whitespace() {
                    pos = prev_char.map(|&(i, _)| i).unwrap_or(0);
                } else {
                    break;
                }
            } else {
                break;
            }
        }

        while pos > 0 {
            let prev_char = chars.iter().rev().find(|&&(i, _)| i < pos);
            if let Some(&(i, c)) = prev_char {
                if !c.is_alphanumeric() && c != '_' {
                    break;
                }
                pos = i;
            } else {
                break;
            }
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

        let chars: Vec<(usize, char)> = line.char_indices().collect();
        let mut pos = self.cursor.x;

        while pos < line.len() {
            let current_char = chars.iter().find(|&&(i, _)| i == pos);
            if let Some(&(_, c)) = current_char {
                if !c.is_alphanumeric() && c != '_' {
                    break;
                }
                pos += c.len_utf8();
            } else {
                break;
            }
        }

        while pos < line.len() {
            let current_char = chars.iter().find(|&&(i, _)| i == pos);
            if let Some(&(_, c)) = current_char {
                if c.is_whitespace() {
                    pos += c.len_utf8();
                } else {
                    break;
                }
            } else {
                break;
            }
        }

        self.cursor.x = pos.min(line.len());
    }
}

/// Byte offset where the char before `x` starts, `0` if there is none.
///
/// `x` is assumed to sit on a char boundary, as [`TextState`] keeps it.
fn prev_char_boundary(line: &str, x: usize) -> usize {
    let mut prev = 0;

    for (idx, _) in line.char_indices() {
        if idx >= x {
            break;
        }
        prev = idx;
    }

    prev
}

/// Byte offset where the char after `x` starts, `line.len()` at the end.
///
/// `x` is assumed to sit on a char boundary, as [`TextState`] keeps it.
fn next_char_boundary(line: &str, x: usize) -> usize {
    line.char_indices()
        .find(|&(idx, _)| idx > x)
        .map_or(line.len(), |(idx, _)| idx)
}

#[cfg(test)]
mod tests {
    use super::TextState;
    use crate::Cursor;

    #[test]
    fn load_replaces_the_buffer_and_resets_the_cursor() {
        let mut state = TextState {
            cursor: Cursor { x: 3, y: 2 },
            ..Default::default()
        };

        state.load(vec![String::from("one"), String::from("two")]);

        assert_eq!(state.lines, vec![String::from("one"), String::from("two")]);
        assert_eq!((state.cursor.x, state.cursor.y), (0, 0));
    }

    #[test]
    fn loading_an_empty_file_keeps_one_empty_line() {
        let mut state = TextState::default();

        state.load(Vec::new());

        assert_eq!(state.lines, vec![String::new()]);
        assert_eq!((state.cursor.x, state.cursor.y), (0, 0));
    }

    #[test]
    fn load_leaves_no_cursor_past_the_new_content() {
        // A position that cannot exist in the shorter buffer loaded below.
        let mut state = TextState {
            cursor: Cursor { x: 40, y: 7 },
            ..Default::default()
        };

        state.load(vec![String::from("tiny")]);

        assert_eq!((state.cursor.x, state.cursor.y), (0, 0));
        assert_eq!(state.display_col(), 0);
    }
}

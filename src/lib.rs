pub mod action;
pub mod app;
pub mod clipboard;
pub mod component;
pub mod error;
pub mod fixed_buf;
pub mod fs;
pub mod highlight;
pub mod icon;
pub mod lsp;
pub mod shortcuts;
pub mod storage;
pub mod text;
pub mod utils;
pub mod widgets;

pub type Result<T> = std::result::Result<T, crate::error::Error>;

#[derive(Debug, Clone, Copy, Default)]
pub struct Cursor {
    pub x: usize,
    pub y: usize,
}

impl Cursor {
    pub fn display_col(&self, line: &str) -> usize {
        line[..self.x]
            .chars()
            .map(|c| unicode_width::UnicodeWidthChar::width(c).unwrap_or(0))
            .sum()
    }

    /// Returns a copy clamped into `line`.
    ///
    /// `x` is a byte offset; this keeps it inside the line and on a char
    /// boundary so slicing for [`Cursor::display_col`] cannot panic.
    pub fn clamped_to(&self, line: &str) -> Self {
        let mut x = self.x.min(line.len());

        while x > 0 && !line.is_char_boundary(x) {
            x -= 1;
        }

        Self { x, ..*self }
    }
}

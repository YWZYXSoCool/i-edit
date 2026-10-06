//! Cursor movements, decoupled from the keys that trigger them.
//!
//! A [`Motion`] is the *what*; the key binding that produced it is the *how*.
//! Keeping them apart means `Shift` needs no cases of its own: extending a
//! selection is just "move, but keep the selection anchor", and the movement
//! itself is identical.

use crate::text::TextState;

use crossterm::event::{KeyCode, KeyModifiers};

/// Lines jumped by PageUp / PageDown.
pub const PAGE_SIZE: usize = 20;

/// A logical cursor movement.
#[derive(Debug, Clone, Copy)]
pub enum Motion {
    Left,
    Right,
    Up,
    Down,
    LineStart,
    LineEnd,
    TextStart,
    TextEnd,
    WordLeft,
    WordRight,
    PageUp,
    PageDown,
}

impl Motion {
    /// Maps a key event to a motion, or `None` if the key is not a movement key.
    pub fn from(modifiers: KeyModifiers, code: KeyCode) -> Option<Motion> {
        use KeyModifiers as M;
        let ctrl = modifiers.contains(M::CONTROL);
        match (ctrl, code) {
            (false, KeyCode::Left) => Some(Motion::Left),
            (false, KeyCode::Right) => Some(Motion::Right),
            (false, KeyCode::Up) => Some(Motion::Up),
            (false, KeyCode::Down) => Some(Motion::Down),
            (false, KeyCode::Home) => Some(Motion::LineStart),
            (false, KeyCode::End) => Some(Motion::LineEnd),
            (false, KeyCode::PageUp) => Some(Motion::PageUp),
            (false, KeyCode::PageDown) => Some(Motion::PageDown),
            (true, KeyCode::Left) => Some(Motion::WordLeft),
            (true, KeyCode::Right) => Some(Motion::WordRight),
            (true, KeyCode::Home) => Some(Motion::TextStart),
            (true, KeyCode::End) => Some(Motion::TextEnd),
            (true, KeyCode::Up) => Some(Motion::TextStart),
            (true, KeyCode::Down) => Some(Motion::TextEnd),
            _ => None,
        }
    }

    /// Applies the motion to `text`.
    pub fn apply(self, text: &mut TextState) {
        match self {
            Motion::Left => text.move_left(),
            Motion::Right => text.move_right(),
            Motion::Up => text.move_up(),
            Motion::Down => text.move_down(),
            Motion::LineStart => text.move_to_line_start(),
            Motion::LineEnd => text.move_to_line_end(),
            Motion::TextStart => text.move_to_text_start(),
            Motion::TextEnd => text.move_to_text_end(),
            Motion::WordLeft => text.move_word_left(),
            Motion::WordRight => text.move_word_right(),
            Motion::PageUp => text.move_page_up(PAGE_SIZE),
            Motion::PageDown => text.move_page_down(PAGE_SIZE),
        }
    }
}

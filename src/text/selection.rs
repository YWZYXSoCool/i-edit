//! Selections: an anchor plus the live cursor.
//!
//! A selection is deliberately *not* stored as two positions. The moving end is
//! always [`TextState::cursor`](crate::text::TextState::cursor) itself, so the
//! two can never drift apart; the anchor is the only extra state. Geometry is
//! derived on demand by [`Selection::ordered`], which is also the only place
//! that cares which end came first.

use crate::Cursor;

/// The two granularities a selection can have.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SelectionMode {
    /// No selection.
    #[default]
    None,
    /// Character-level: the precise byte span between the anchor and the cursor.
    Char,
    /// Line-level: every whole line from the anchor's line through the cursor's.
    Line,
}

/// A selection: a fixed anchor plus an active end (the live [`TextState`](crate::text::TextState) cursor).
///
/// The active end is *not* stored — it is `TextState.cursor`, so the two can
/// never drift apart. The anchor and the cursor are sorted into an ordered span
/// only when geometry is needed; the direction is never baked in here.
#[derive(Debug, Clone, Copy, Default)]
pub struct Selection {
    pub anchor: Cursor,
    pub mode: SelectionMode,
}

impl Selection {
    /// Whether a real selection is in effect (i.e. `mode != None`).
    pub fn is_active(&self) -> bool {
        self.mode != SelectionMode::None
    }

    /// Returns the ordered `(start_y, start_x, end_y, end_x)` covering the
    /// selection given the current `cursor`.
    ///
    /// For [`SelectionMode::Char`] the two ends are compared and sorted so
    /// `(start, ..) <= (.. , end)`. For [`SelectionMode::Line`] the span is the
    /// whole rows from the topmost anchor/cursor line through the bottommost;
    /// `start_x`/`end_x` are both `0` and the caller treats every included row
    /// as full-width.
    pub fn ordered(&self, cursor: Cursor) -> (usize, usize, usize, usize) {
        match self.mode {
            SelectionMode::None => (cursor.y, cursor.x, cursor.y, cursor.x),
            SelectionMode::Char => {
                let (ay, ax) = (self.anchor.y, self.anchor.x);
                if (ay, ax) <= (cursor.y, cursor.x) {
                    (ay, ax, cursor.y, cursor.x)
                } else {
                    (cursor.y, cursor.x, ay, ax)
                }
            }
            SelectionMode::Line => {
                let top = self.anchor.y.min(cursor.y);
                let bottom = self.anchor.y.max(cursor.y);
                (top, 0, bottom, 0)
            }
        }
    }
}

//! The selection overlay: the spans of the visible rows that are selected.
//!
//! The overlay is a pure function of the buffer, the selection and the current
//! scroll — it decides no state of its own, which is why it lives apart from
//! [`EditorState`](super::EditorState). Being pure also makes it the only part
//! of the selection that is worth testing on its own.
//!
//! Selection is drawn with a reversed (inverse-video) style: it reads on any
//! terminal theme and never clashes with syntax foreground colors.

use crate::Cursor;
use crate::highlight::StyledRun;
use crate::text::{Selection, SelectionMode};

use ratatui::style::{Modifier, Style};

/// Fills `rows` — one entry per document line — with the spans `selection`
/// covers.
///
/// Only the visible band is filled: a selection that runs off screen still
/// costs nothing, because the viewport never asks about those rows. `rows` is
/// resized in place rather than reallocated, so a steady-state recomputation on
/// every cursor move allocates nothing beyond the first time.
pub fn rebuild(
    lines: &[String],
    selection: &Selection,
    cursor: Cursor,
    scroll_y: usize,
    height: usize,
    rows: &mut Vec<Vec<StyledRun>>,
) {
    let total = lines.len();
    let style = Style::new().add_modifier(Modifier::REVERSED);

    rows.clear();
    rows.resize(total, Vec::new());

    if !selection.is_active() {
        return;
    }

    let (sy, sx, ey, ex) = selection.ordered(cursor);
    let y_lo = scroll_y;
    let y_hi = (scroll_y + height).min(total);

    for y in y_lo..y_hi {
        if y < sy || y > ey {
            continue;
        }
        let line = &lines[y];
        let runs = &mut rows[y];
        match selection.mode {
            SelectionMode::Char => {
                let (a, b) = if sy == ey {
                    (sx, ex)
                } else if y == sy {
                    (sx, line.len())
                } else if y == ey {
                    (0, ex)
                } else {
                    (0, line.len())
                };
                if a < b {
                    runs.push(StyledRun {
                        start: a as u32,
                        end: b as u32,
                        style,
                    });
                }
            }
            SelectionMode::Line => {
                runs.push(StyledRun {
                    start: 0,
                    end: line.len() as u32,
                    style,
                });
            }
            SelectionMode::None => {}
        }
    }
}

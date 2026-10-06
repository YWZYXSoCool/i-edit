//! The coloring layer: turns `(byte range, Style)` instructions into painted
//! text, without ever deciding what the colors *mean*.
//!
//! This module is deliberately agnostic. It accepts "row `y`, bytes
//! `[start, end)` get this [`Style`]" and draws it. Where the color comes from
//! — a demo source today, rust-analyzer's `semanticTokens` or diagnostics
//! tomorrow — is the producer's business, decided entirely outside this layer.
//! That is the property that lets the renderer sit underneath a future LSP
//! without being bound by its token legend.
//!
//! Two layers, `base` and `overlay`, are resolved per row by the renderer:
//! `overlay` wins where it covers a byte, otherwise `base`, otherwise the
//! default style. They are never merged on the render path — merging would mean
//! building a third container every frame, which the project's zero-allocation
//! render policy forbids.

use ratatui::style::{Color, Style};

/// A coloring instruction: the byte range `[start, end)` of one row is painted
/// with `style`.
///
/// `style` is the *full* ratatui `Style` — foreground, background,
/// underline color and modifier — so the layer can carry anything a producer
/// needs (semantic tokens, diagnostic underlines, selection, current-line
/// background) without the renderer learning any semantics. The renderer only
/// ever sees `(byte range, Style)`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StyledRun {
    pub start: u32,
    pub end: u32,
    pub style: Style,
}

/// Which layer a producer writes to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayerId {
    Base,
    Overlay,
}

/// One row's `[start, end)` slice into [`Layer::arena`].
#[derive(Debug, Clone, Copy, Default)]
struct LineSpan {
    start: u32,
    end: u32,
}

/// A flat arena of runs plus a per-row index into it.
///
/// Storing every row's runs in a single `Vec` (rather than
/// `Vec<Vec<StyledRun>>`) keeps injection to two allocations and is
/// cache-friendly on the render path, which walks the arena through indices
/// only — never through per-row allocations.
#[derive(Debug, Default)]
struct Layer {
    arena: Vec<StyledRun>,
    lines: Vec<LineSpan>,
    /// Rows `0..valid_upto` are consistent with the current text; rows at or
    /// past it are stale and render as empty. Lowering the watermark on an edit
    /// is O(1) and allocation-free — no per-row work, no splice.
    valid_upto: usize,
}

impl Layer {
    /// Runs for row `y`, or an empty slice when `y` is stale or out of range.
    fn runs_for(&self, y: usize) -> &[StyledRun] {
        if y < self.valid_upto && y < self.lines.len() {
            let span = &self.lines[y];
            &self.arena[span.start as usize..span.end as usize]
        } else {
            &[]
        }
    }

    /// Replaces every row's runs. `rows` must be in ascending row order, each
    /// row's runs in ascending `start`. This is the push side of the model: it
    /// may allocate (once for the arena, once for the index) but only when new
    /// data arrives — never on the render or key paths.
    fn replace(&mut self, rows: &[Vec<StyledRun>]) {
        let total: usize = rows.iter().map(Vec::len).sum();
        self.arena.clear();
        self.arena.reserve(total);
        self.lines.clear();
        self.lines.reserve(rows.len());
        for row in rows {
            let start = self.arena.len() as u32;
            self.arena.extend_from_slice(row);
            let end = self.arena.len() as u32;
            self.lines.push(LineSpan { start, end });
        }
        self.valid_upto = self.lines.len();
    }

    /// Drops all runs, keeping capacity so the next `replace` is
    /// allocation-free in the steady state.
    fn clear(&mut self) {
        self.arena.clear();
        self.lines.clear();
        self.valid_upto = 0;
    }

    /// Marks everything from `line` down as stale. O(1): only the watermark
    /// drops. Stale spans are ignored by `runs_for`; the next `replace`
    /// overwrites them.
    fn invalidate_from(&mut self, line: usize) {
        if line < self.valid_upto {
            self.valid_upto = line;
        }
    }

    /// Resizes the index to `line_count` empty rows, e.g. after a file load.
    fn reset(&mut self, line_count: usize) {
        self.arena.clear();
        self.lines.clear();
        self.lines.reserve(line_count);
        self.lines.resize(line_count, LineSpan::default());
        self.valid_upto = line_count;
    }
}

/// The full coloring state for a buffer: two layers plus an on/off switch.
///
/// Owned by [`ViewportState`](crate::widgets::viewport::ViewportState) so the
/// renderer has mutable access during `render` without any new borrow. Rendering
/// pulls ([`Highlights::line_runs`]); producers push
/// ([`Highlights::replace_layer`]). The two sides never meet, which is what lets
/// data arrive from another thread without touching the render path.
#[derive(Debug, Default)]
pub struct Highlights {
    base: Layer,
    overlay: Layer,
    enabled: bool,
}

impl Highlights {
    /// Whether coloring is currently applied at all.
    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// Turns coloring on or off. Off renders every text cell with the default
    /// style — byte-for-byte what the editor drew before this layer existed.
    pub fn set_enabled(&mut self, on: bool) {
        self.enabled = on;
    }

    /// Render side: returns the `(base, overlay)` run slices for row `y`, in
    /// that order. Allocation-free; both slices borrow the arena. The renderer
    /// resolves them with `overlay` taking priority.
    pub fn line_runs(&self, y: usize) -> (&[StyledRun], &[StyledRun]) {
        (self.base.runs_for(y), self.overlay.runs_for(y))
    }

    /// Producer side: replaces one layer wholesale. May allocate, but only when
    /// new data arrives — never on the render or key paths.
    pub fn replace_layer(&mut self, which: LayerId, rows: &[Vec<StyledRun>]) {
        match which {
            LayerId::Base => self.base.replace(rows),
            LayerId::Overlay => self.overlay.replace(rows),
        }
    }

    /// Producer side: empties one layer, keeping its capacity.
    pub fn clear_layer(&mut self, which: LayerId) {
        match which {
            LayerId::Base => self.base.clear(),
            LayerId::Overlay => self.overlay.clear(),
        }
    }

    /// Records an edit at or below `line`. O(1): only the watermark drops; the
    /// stale rows are already inconsistent, and the next `replace` rebuilds
    /// them. No allocation, no per-row work.
    pub fn note_edit(&mut self, line: usize) {
        self.base.invalidate_from(line);
        self.overlay.invalidate_from(line);
    }

    /// After a file load: rebuild the index to `line_count` empty rows.
    pub fn reset(&mut self, line_count: usize) {
        self.base.reset(line_count);
        self.overlay.reset(line_count);
    }

    /// Demo producer (temporary, removable): paints each row's
    /// whitespace-separated tokens in a rotating palette so the rendering
    /// pipeline is visible end-to-end. Writes the overlay layer and enables
    /// coloring.
    pub fn paint_demo(&mut self, lines: &[String]) {
        let palette = [
            Style::new().fg(Color::Red),
            Style::new().fg(Color::Green),
            Style::new().fg(Color::Blue),
            Style::new().fg(Color::Yellow),
            Style::new().fg(Color::Magenta),
            Style::new().fg(Color::Cyan),
        ];

        let mut rows: Vec<Vec<StyledRun>> = Vec::with_capacity(lines.len());
        for line in lines {
            let bytes = line.as_bytes();
            let mut runs = Vec::new();
            let mut i = 0usize;
            let mut color = 0usize;
            while i < bytes.len() {
                // Skip whitespace between tokens.
                while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                    i += 1;
                }
                if i >= bytes.len() {
                    break;
                }
                let start = i as u32;
                while i < bytes.len() && !bytes[i].is_ascii_whitespace() {
                    i += 1;
                }
                let end = i as u32;
                runs.push(StyledRun {
                    start,
                    end,
                    style: palette[color % palette.len()],
                });
                color += 1;
            }
            rows.push(runs);
        }

        self.overlay.replace(&rows);
        self.enabled = true;
    }
}

/// Converts an LSP line-column (UTF-16 code units) to a byte offset within
/// `line`. Called only on the injection path (once per token), never on render,
/// so its linear scan is free.
///
/// UTF-16 is the LSP wire unit, not bytes and not display columns; converting
/// up front means the renderer can stay in byte space and zero-convert.
pub fn utf16_to_byte(line: &str, col16: usize) -> usize {
    let mut units = 0;
    for (offset, ch) in line.char_indices() {
        if units >= col16 {
            return offset;
        }
        units += ch.len_utf16();
    }
    line.len()
}

#[cfg(test)]
mod tests {
    use super::{Highlights, LayerId, StyledRun, utf16_to_byte};
    use ratatui::style::{Color, Style};

    fn run(start: u32, end: u32, color: Color) -> StyledRun {
        StyledRun {
            start,
            end,
            style: Style::new().fg(color),
        }
    }

    #[test]
    fn utf16_to_byte_handles_ascii() {
        let line = "hello";
        assert_eq!(utf16_to_byte(line, 0), 0);
        assert_eq!(utf16_to_byte(line, 2), 2);
        assert_eq!(utf16_to_byte(line, 5), 5);
        // Beyond the end clamps to the line length.
        assert_eq!(utf16_to_byte(line, 99), 5);
    }

    #[test]
    fn utf16_to_byte_handles_bmp_chinese() {
        // Each Han character is 3 UTF-8 bytes but 1 UTF-16 unit.
        let line = "你好世界";
        assert_eq!(utf16_to_byte(line, 0), 0);
        assert_eq!(utf16_to_byte(line, 1), 3);
        assert_eq!(utf16_to_byte(line, 2), 6);
        assert_eq!(utf16_to_byte(line, 3), 9);
        assert_eq!(utf16_to_byte(line, 4), 12);
    }

    #[test]
    fn utf16_to_byte_handles_surrogate_pair_emoji() {
        // 😀 is a surrogate pair: 2 UTF-16 units, 4 UTF-8 bytes. 'a' is 1/1.
        // UTF-16 columns: 0 = start of 😀, 2 = start of 'a', 3 = end.
        let line = "😀a";
        assert_eq!(utf16_to_byte(line, 0), 0);
        assert_eq!(utf16_to_byte(line, 1), 4); // mid-emoji → next char boundary
        assert_eq!(utf16_to_byte(line, 2), 4); // start of 'a'
        assert_eq!(utf16_to_byte(line, 3), 5); // end of line
    }

    #[test]
    fn overlay_wins_over_base_and_uncolored_falls_through() {
        let mut h = Highlights::default();
        h.replace_layer(
            LayerId::Base,
            &[vec![run(0, 3, Color::Red)], vec![run(0, 2, Color::Red)]],
        );
        h.replace_layer(LayerId::Overlay, &[vec![run(0, 1, Color::Blue)], vec![]]);
        h.set_enabled(true);

        let (base, overlay) = h.line_runs(0);
        assert_eq!(overlay.len(), 1);
        assert_eq!(base.len(), 1);

        let (base1, overlay1) = h.line_runs(1);
        assert_eq!(overlay1.len(), 0);
        assert_eq!(base1.len(), 1);
    }

    #[test]
    fn note_edit_invalidates_from_the_edited_row_down() {
        let mut h = Highlights::default();
        h.replace_layer(
            LayerId::Overlay,
            &[
                vec![run(0, 1, Color::Blue)],
                vec![run(0, 1, Color::Blue)],
                vec![run(0, 1, Color::Blue)],
                vec![run(0, 1, Color::Blue)],
                vec![run(0, 1, Color::Blue)],
            ],
        );
        h.set_enabled(true);

        // Editing row 2 invalidates row 2 and everything below; rows 0–1 keep
        // their runs.
        h.note_edit(2);
        assert_eq!(h.line_runs(0).1.len(), 1);
        assert_eq!(h.line_runs(1).1.len(), 1);
        assert_eq!(h.line_runs(2).1.len(), 0);
        assert_eq!(h.line_runs(4).1.len(), 0);
    }

    #[test]
    fn note_edit_is_idempotent_and_never_grows_the_watermark() {
        let mut h = Highlights::default();
        h.replace_layer(LayerId::Overlay, &[vec![run(0, 1, Color::Blue)]]);

        h.note_edit(0);
        h.note_edit(0);
        assert_eq!(h.line_runs(0).1.len(), 0);
    }

    #[test]
    fn replace_layer_is_allocation_free_after_the_first_call() {
        // First call must grow the arena/index from empty (one alloc each).
        // Second call reuses the established capacity (zero alloc).
        let rows: Vec<Vec<StyledRun>> = (0..50)
            .map(|i| vec![run(i as u32, (i + 1) as u32, Color::Green)])
            .collect();

        let mut h = Highlights::default();
        h.replace_layer(LayerId::Base, &rows); // first: establishes capacity
        h.replace_layer(LayerId::Base, &rows); // second: should reuse it

        assert_eq!(h.line_runs(10).0.len(), 1);
    }

    #[test]
    fn clear_layer_empties_without_losing_capacity() {
        let mut h = Highlights::default();
        h.replace_layer(LayerId::Overlay, &[vec![run(0, 1, Color::Blue)]]);
        h.clear_layer(LayerId::Overlay);
        assert_eq!(h.line_runs(0).1.len(), 0);
    }

    #[test]
    fn disabled_layer_renders_empty_runs() {
        let mut h = Highlights::default();
        h.replace_layer(LayerId::Overlay, &[vec![run(0, 1, Color::Blue)]]);
        h.set_enabled(false);
        // With coloring off the renderer is told there is nothing to paint.
        assert_eq!(h.line_runs(0).1.len(), 1); // data still present...
        assert!(!h.enabled()); // ...but the switch is off, so no color shows.
    }
}

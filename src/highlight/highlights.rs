use ratatui::style::{Color, Style};

use crate::highlight::edit::TextEdit;
use crate::highlight::layer::{LAYER_COUNT, LAYER_ORDER, Layer, LayerId};
use crate::highlight::run::StyledRun;

/// The full coloring state for a buffer: three layers plus an on/off switch.
///
/// Owned by [`ViewportState`](crate::widgets::viewport::ViewportState) so the
/// renderer has mutable access during `render` without any new borrow. Rendering
/// pulls ([`Highlights::line_runs`]); producers push
/// ([`Highlights::replace_layer`]). The two sides never meet, which is what lets
/// data arrive from another thread without touching the render path.
#[derive(Debug, Default)]
pub struct Highlights {
    base: Layer,
    semantic: Layer,
    overlay: Layer,
    enabled: bool,
}

impl Highlights {
    /// The layer a producer addresses. The single place the layer list is
    /// spelled out, so adding a fourth layer touches one match.
    fn layer_mut(&mut self, which: LayerId) -> &mut Layer {
        match which {
            LayerId::Base => &mut self.base,
            LayerId::Semantic => &mut self.semantic,
            LayerId::Overlay => &mut self.overlay,
        }
    }

    /// Whether coloring is currently applied at all.
    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// Turns coloring on or off. Off renders every text cell with the default
    /// style — byte-for-byte what the editor drew before this layer existed.
    pub fn set_enabled(&mut self, on: bool) {
        self.enabled = on;
    }

    /// Render side: the runs for row `y`, one slice per layer, in
    /// [`LAYER_ORDER`] — ascending priority, so the renderer resolves them by
    /// walking backwards. Allocation-free; every slice borrows its arena.
    pub fn line_runs(&self, y: usize) -> [&[StyledRun]; LAYER_COUNT] {
        [
            self.base.runs_for(y),
            self.semantic.runs_for(y),
            self.overlay.runs_for(y),
        ]
    }

    /// Producer side: replaces one layer wholesale. May allocate, but only when
    /// new data arrives — never on the render or key paths.
    pub fn replace_layer(&mut self, which: LayerId, rows: &[Vec<StyledRun>]) {
        self.layer_mut(which).replace(rows);
    }

    /// Producer side: empties one layer, keeping its capacity.
    pub fn clear_layer(&mut self, which: LayerId) {
        self.layer_mut(which).clear();
    }

    /// Records an edit. Every layer moves its runs across it, so what was
    /// already colored stays colored and only what the change touched goes
    /// blank until the next response.
    pub fn apply_edit(&mut self, edit: &TextEdit) {
        for which in LAYER_ORDER {
            self.layer_mut(which).apply_edit(edit);
        }
    }

    /// After a file load: rebuild every layer's index to `line_count` empty
    /// rows.
    pub fn reset(&mut self, line_count: usize) {
        for which in LAYER_ORDER {
            self.layer_mut(which).reset(line_count);
        }
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

#[cfg(test)]
impl Highlights {
    /// Marks everything from `line` down as no longer describing the text — the
    /// state a layer is in while it waits for fresh data. Only tests can put a
    /// layer there; in the editor it happens by itself.
    pub(crate) fn stale_from(&mut self, line: usize) {
        for which in LAYER_ORDER {
            self.layer_mut(which).invalidate_from(line);
        }
    }
}

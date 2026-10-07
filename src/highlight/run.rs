use ratatui::style::Style;

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

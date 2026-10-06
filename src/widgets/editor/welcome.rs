//! The vim-style greeting shown in an empty scratch buffer.
//!
//! An empty, never-saved buffer is a blank page: it greets the user the way vim
//! does instead of leaving it bare. The hints come from the shortcut registry,
//! so they always match the real bindings rather than drifting from them.

use std::path::Path;

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use unicode_width::UnicodeWidthStr;

/// The welcome's first line stands out; the hints stay dim, like the file
/// tree's placeholder.
pub const TITLE_STYLE: Style = Style::new().fg(Color::White).add_modifier(Modifier::BOLD);
pub const HINT_STYLE: Style = Style::new().fg(Color::DarkGray);

/// True while this is the never-saved startup buffer with nothing typed into it.
///
/// A *named* buffer that happens to be empty is a real empty file, not a
/// scratch pad, so it gets no greeting.
pub fn is_empty_scratch(path: Option<&Path>, lines: &[String]) -> bool {
    path.is_none() && lines.iter().all(String::is_empty)
}

/// The welcome text: the editor identity, then every shortcut as
/// `key  description`. Built from the shortcut registry so the hints never
/// drift from the actual bindings.
pub fn welcome_lines() -> Vec<String> {
    let hints = crate::shortcuts::welcome_lines();
    let label_width = hints
        .iter()
        .map(|(label, _)| label.chars().count())
        .max()
        .unwrap_or(0);

    let mut lines: Vec<String> = Vec::with_capacity(hints.len() + 3);
    lines.push(env!("CARGO_PKG_NAME").to_string());
    lines.push(format!("version {}", env!("CARGO_PKG_VERSION")));
    lines.push(String::new());
    for (label, description) in hints {
        lines.push(format!("{label:<label_width$}  {description}"));
    }
    lines
}

/// Where the welcome block goes: centered, or `None` when it does not fit.
pub fn welcome_area(area: Rect) -> Option<Rect> {
    let lines = welcome_lines();
    let width = lines
        .iter()
        .map(|line| UnicodeWidthStr::width(line.as_str()))
        .max()
        .unwrap_or(0) as u16;

    centered_block(area, width, lines.len() as u16)
}

/// A `width` x `height` rect centered in `area`, or `None` when it does not fit.
fn centered_block(area: Rect, width: u16, height: u16) -> Option<Rect> {
    if width == 0 || height == 0 || width > area.width || height > area.height {
        return None;
    }

    Some(Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    })
}

//! Nerd Font glyphs used by the UI.
//!
//! Each constant is one glyph from the [Nerd Fonts cheat sheet], named after
//! the place the UI uses it; its doc comment records the cheat-sheet entry and
//! codepoint so the glyph can be looked up. All of them sit in the Private Use
//! Area and measure one column wide, so prefixing a string with an icon moves
//! the rest along by exactly one column.
//!
//! A terminal without a Nerd Font shows replacement boxes instead. Nothing
//! relies on that not happening: every icon has text next to it that says the
//! same thing, so the icons are decoration.
//!
//! [Nerd Fonts cheat sheet]: https://www.nerdfonts.com/cheat-sheet

/// `nf-fa-terminal` (U+F120) — prefix of the command box placeholder.
pub const COMMAND: &str = "\u{f120}";

/// `nf-fa-keyboard` (U+F11C) — prefix of the last-key read-out.
pub const KEYBOARD: &str = "\u{f11c}";

/// `nf-fa-location_arrow` (U+F124) — prefix of the cursor-position read-out.
pub const CURSOR: &str = "\u{f124}";

/// `nf-fa-list` (U+F03A) — log panel title.
pub const LOG: &str = "\u{f03a}";

/// `nf-fa-chevron_right` (U+F054) — marks the highlighted command suggestion.
pub const CHEVRON: &str = "\u{f054}";

/// `nf-fa-circle_info` (U+F05A) — prefix of an informational message.
pub const INFO: &str = "\u{f05a}";

/// `nf-fa-circle_check` (U+F058) — prefix of a success message.
pub const SUCCESS: &str = "\u{f058}";

/// `nf-fa-triangle_exclamation` (U+F071) — prefix of a warning message.
pub const WARNING: &str = "\u{f071}";

/// `nf-fa-circle_xmark` (U+F057) — prefix of an error message.
pub const ERROR: &str = "\u{f057}";

/// `nf-fa-file` (U+F15B) — prefix of a file in the file picker and file tree.
pub const FILE: &str = "\u{f15b}";

/// `nf-fa-folder` (U+F07B) — prefix of a collapsed folder in the file tree.
pub const FOLDER: &str = "\u{f07b}";

/// `nf-fa-folder_open` (U+F07C) — prefix of an expanded folder in the file tree.
pub const FOLDER_OPEN: &str = "\u{f07c}";

/// `nf-fa-chevron_down` (U+F078) — marks an expanded folder in the file tree.
pub const CHEVRON_DOWN: &str = "\u{f078}";

#[cfg(test)]
mod tests {
    use super::{
        CHEVRON, CHEVRON_DOWN, COMMAND, CURSOR, ERROR, FILE, FOLDER, FOLDER_OPEN, INFO, KEYBOARD,
        LOG, SUCCESS, WARNING,
    };

    use unicode_width::UnicodeWidthStr;

    /// The layout adds icons without measuring them: that is only safe while
    /// each one stays single-width.
    #[test]
    fn every_icon_is_one_column_wide() {
        for icon in [
            COMMAND,
            KEYBOARD,
            CURSOR,
            LOG,
            CHEVRON,
            INFO,
            SUCCESS,
            WARNING,
            ERROR,
            FILE,
            FOLDER,
            FOLDER_OPEN,
            CHEVRON_DOWN,
        ] {
            assert_eq!(UnicodeWidthStr::width(icon), 1, "unexpected width");
        }
    }
}

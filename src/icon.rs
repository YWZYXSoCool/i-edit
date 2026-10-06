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

use std::path::Path;
use std::sync::LazyLock;

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

/// `nf-dev-rust` (U+E7A8)
pub const DEV_RUST: &str = "\u{e7a8}";

/// `nf-custom-toml` (U+E6B2)
pub const CUSTOM_TOML: &str = "\u{e6b2}";

/// `nf-seti-markdown` (U+E609)
pub const SETI_MARKDOWN: &str = "\u{e609}";

/// `nf-seti-git_ignore` (U+E65D)
pub const SETI_GIT_IGNORE: &str = "\u{e65d}";

/// A file-name rule, precompiled from a template so lookups do no string
/// parsing.
enum Rule {
    /// Exact full file name, e.g. `Cargo.toml`.
    Exact(&'static str),
    /// File extension, e.g. `rs` (from `*.rs`), matched via `Path::extension`.
    Ext(&'static str),
}

/// Compile a template (`Cargo.toml`, `*.rs`, …) into a [`Rule`] at startup.
fn compile(template: &'static str) -> Rule {
    match template.strip_prefix('*') {
        Some(suffix) => Rule::Ext(suffix.strip_prefix('.').unwrap_or(suffix)),
        None => Rule::Exact(template),
    }
}

/// Icon selection rules for file names, in priority order.
///
/// Written as templates — `Cargo.toml`/`Cargo.lock` are exact names, `*.rs`,
/// `*.toml`, `*.md` are extension wildcards — then precompiled into [`Rule`]s so
/// matching is a plain comparison, never string chopping. Exact-name rules are
/// listed before wildcard rules so they always win.
static ICON_RULES: LazyLock<Vec<(Rule, &'static str)>> = LazyLock::new(|| {
    [
        ("Cargo.toml", CUSTOM_TOML),
        ("Cargo.lock", CUSTOM_TOML),
        (".gitattributes", SETI_GIT_IGNORE),
        (".gitignore", SETI_GIT_IGNORE),
        ("*.rs", DEV_RUST),
        ("*.toml", CUSTOM_TOML),
        ("*.md", SETI_MARKDOWN),
    ]
    .into_iter()
    .map(|(template, icon)| (compile(template), icon))
    .collect()
});

/// Pick the icon for a file name, falling back to [`FILE`] when nothing matches.
pub fn icon_for(name: &str) -> &'static str {
    for (rule, icon) in ICON_RULES.iter() {
        if rule_matches(rule, name) {
            return icon;
        }
    }
    FILE
}

/// Match a precompiled [`Rule`] against a file name. No template parsing here.
fn rule_matches(rule: &Rule, name: &str) -> bool {
    match rule {
        Rule::Exact(exact) => *exact == name,
        Rule::Ext(ext) => Path::new(name).extension().and_then(|e| e.to_str()) == Some(ext),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CHEVRON, CHEVRON_DOWN, COMMAND, CURSOR, CUSTOM_TOML, DEV_RUST, ERROR, FILE, FOLDER,
        FOLDER_OPEN, INFO, KEYBOARD, LOG, SETI_MARKDOWN, SUCCESS, WARNING, icon_for,
    };

    use unicode_width::UnicodeWidthStr;

    /// The layout adds icons without measuring them: that is only safe while
    /// each one stays single-width.
    /// The old `ends_with` matcher misclassified dotfiles and was order-dependent;
    /// these pin the template behaviour so that cannot regress.
    #[test]
    fn icon_for_matches_templates() {
        assert_eq!(icon_for("main.rs"), DEV_RUST);
        assert_eq!(icon_for("Cargo.toml"), CUSTOM_TOML);
        assert_eq!(icon_for("Cargo.lock"), CUSTOM_TOML);
        assert_eq!(icon_for("foo.toml"), CUSTOM_TOML);
        assert_eq!(icon_for("README.md"), SETI_MARKDOWN);
        // Extension, not trailing suffix: a dotfile is never a Rust file...
        assert_eq!(icon_for(".rs"), FILE);
        // ...and a double extension is not mistaken for its inner one.
        assert_eq!(icon_for("notes.md.bak"), FILE);
        assert_eq!(icon_for("anything"), FILE);
    }

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

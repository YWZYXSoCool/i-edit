//! User settings: the small set of switches that outlive the session and are
//! meant to be edited by hand.
//!
//! The `settings` command opens the file this module writes, so the values here
//! are part of the editor's interface: each one needs a name a person can guess
//! and a value that is obvious, and the file has to say what it accepts.
//! Anything unparseable falls back to its default rather than being an error —
//! a half-typed setting must not stop the editor from starting.

use std::str::FromStr;

use crate::storage::codec::Document;

/// Reopen the last buffer and its cursor on startup.
const DEFAULT_RESTORE_SESSION: bool = true;

/// Write the directory cache to disk on shutdown.
const DEFAULT_PERSIST_CACHE: bool = true;

/// What the Tab key inserts when the setting is missing or unreadable.
const DEFAULT_TAB_INDENT: TabIndent = TabIndent::Spaces;

/// How many columns [`TabIndent::Spaces`] inserts.
pub const TAB_SPACES_WIDTH: usize = 4;

/// Comment block written above the settings themselves.
///
/// The settings file is edited by hand, so it has to explain itself: a bare
/// `tab_indent = spaces` tells nobody that `tab` is the other answer. Every
/// line starts with `#`, which the parser skips, so the block can be deleted
/// or moved without breaking the file.
pub const SETTINGS_HELP: &[&str] = &[
    "#",
    "# Settings. Save the file to apply: i-edit rereads it after every write.",
    "# An unknown value, or a missing setting, keeps the default.",
    "#",
    "# tab_indent = spaces | tab",
    "#   What the Tab key inserts: four spaces, or one tab character.",
    "#",
];

/// What the Tab key inserts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TabIndent {
    /// A run of [`TAB_SPACES_WIDTH`] spaces. The default: it keeps a file
    /// looking the same in every editor and terminal.
    #[default]
    Spaces,
    /// A single tab character.
    Tab,
}

impl TabIndent {
    /// The text inserted when the key is pressed.
    pub fn text(self) -> &'static str {
        match self {
            Self::Spaces => "    ",
            Self::Tab => "\t",
        }
    }

    /// How this value is spelled in the settings file.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Spaces => "spaces",
            Self::Tab => "tab",
        }
    }
}

impl std::fmt::Display for TabIndent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for TabIndent {
    /// A value that is neither `spaces` nor `tab`.
    type Err = UnknownSetting;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "spaces" => Ok(Self::Spaces),
            "tab" | "tabs" => Ok(Self::Tab),
            _ => Err(UnknownSetting),
        }
    }
}

/// A settings value the editor does not recognise.
///
/// Carries nothing: it only has to exist so [`FromStr`] can refuse, and the
/// caller then falls back to the default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnknownSetting;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// Restore the buffer, cursor, and expanded directories of the last run.
    pub restore_session: bool,
    /// Keep the directory cache between runs. Off means it stays in memory
    /// only, which is the right answer for a workspace on a slow or removable
    /// disk.
    pub persist_cache: bool,
    /// What pressing Tab in the editor inserts.
    pub tab_indent: TabIndent,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            restore_session: DEFAULT_RESTORE_SESSION,
            persist_cache: DEFAULT_PERSIST_CACHE,
            tab_indent: DEFAULT_TAB_INDENT,
        }
    }
}

impl Config {
    pub(crate) fn from_doc(doc: &Document) -> Self {
        Self {
            restore_session: flag(doc.get("restore_session"), DEFAULT_RESTORE_SESSION),
            persist_cache: flag(doc.get("persist_cache"), DEFAULT_PERSIST_CACHE),
            tab_indent: doc.get_parsed("tab_indent").unwrap_or(DEFAULT_TAB_INDENT),
        }
    }

    pub(crate) fn to_doc(&self) -> Document {
        let mut doc = Document::new();
        doc.set("restore_session", bool_word(self.restore_session));
        doc.set("persist_cache", bool_word(self.persist_cache));
        doc.set("tab_indent", self.tab_indent.as_str());
        doc
    }
}

/// Anything other than an explicit `true` / `false` keeps the default, so a
/// typo degrades instead of flipping the setting.
pub(crate) fn flag(value: Option<&str>, default: bool) -> bool {
    match value {
        Some("true") => true,
        Some("false") => false,
        _ => default,
    }
}

pub(crate) fn bool_word(value: bool) -> &'static str {
    if value { "true" } else { "false" }
}

#[cfg(test)]
mod tests {
    use super::{Config, Document, TAB_SPACES_WIDTH, TabIndent};

    #[test]
    fn spaces_are_four_wide() {
        assert_eq!(TabIndent::Spaces.text().len(), TAB_SPACES_WIDTH);
    }

    #[test]
    fn a_tab_is_one_character() {
        assert_eq!(TabIndent::Tab.text(), "\t");
    }

    #[test]
    fn the_default_config_round_trips() {
        let config = Config::default();

        assert_eq!(Config::from_doc(&config.to_doc()), config);
    }

    #[test]
    fn tab_indent_reads_from_the_file() {
        // Case and surrounding spaces are forgiven: this is a hand-edited file.
        for value in ["tab", "TAB", "tabs", " tab "] {
            let doc = Document::parse(&[format!("tab_indent = {value}")]);

            assert_eq!(
                Config::from_doc(&doc).tab_indent,
                TabIndent::Tab,
                "{value:?}"
            );
        }
    }

    #[test]
    fn tab_indent_survives_a_write() {
        let config = Config {
            tab_indent: TabIndent::Tab,
            ..Config::default()
        };

        assert_eq!(
            Config::from_doc(&config.to_doc()).tab_indent,
            TabIndent::Tab
        );
    }

    #[test]
    fn an_unknown_tab_indent_keeps_the_default() {
        for value in ["", "4", "space", "spaces;", "tabs and spaces"] {
            let doc = Document::parse(&[format!("tab_indent = {value}")]);

            assert_eq!(
                Config::from_doc(&doc).tab_indent,
                TabIndent::Spaces,
                "{value:?}"
            );
        }
    }

    #[test]
    fn the_help_block_is_comments_only() {
        // It is written into the file verbatim, so anything that is not a
        // comment would come back as a setting.
        for line in super::SETTINGS_HELP {
            assert!(line.starts_with('#'), "{line:?}");
        }
    }
}

//! User settings: the small set of switches that outlive the session and are
//! meant to be edited by hand.

use crate::storage::codec::Document;

/// Reopen the last buffer and its cursor on startup.
const DEFAULT_RESTORE_SESSION: bool = true;

/// Write the directory cache to disk on shutdown.
const DEFAULT_PERSIST_CACHE: bool = true;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// Restore the buffer, cursor, and expanded directories of the last run.
    pub restore_session: bool,
    /// Keep the directory cache between runs. Off means it stays in memory
    /// only, which is the right answer for a workspace on a slow or removable
    /// disk.
    pub persist_cache: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            restore_session: DEFAULT_RESTORE_SESSION,
            persist_cache: DEFAULT_PERSIST_CACHE,
        }
    }
}

impl Config {
    pub(crate) fn from_doc(doc: &Document) -> Self {
        Self {
            restore_session: flag(doc.get("restore_session"), DEFAULT_RESTORE_SESSION),
            persist_cache: flag(doc.get("persist_cache"), DEFAULT_PERSIST_CACHE),
        }
    }

    pub(crate) fn to_doc(&self) -> Document {
        let mut doc = Document::new();
        doc.set("restore_session", bool_word(self.restore_session));
        doc.set("persist_cache", bool_word(self.persist_cache));
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

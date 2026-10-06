//! One line-oriented `key = value` file, as the on-disk shape of every
//! storage section.
//!
//! The format is deliberately not JSON or TOML: the data here is a handful of
//! scalars plus lists of paths, and a flat file with one entry per line can be
//! read and written without a dependency, hand-edited, and parsed tolerantly —
//! an unknown key or a broken line is skipped instead of failing the read.
//!
//! Values are raw to the end of the line. A value containing a line break is
//! therefore unrepresentable and [`Document::set`] / [`Document::push`] refuse
//! it, which keeps a read-back byte-exact for every value that is stored.
//! Paths are the only values that could ever hold one, and a path with an
//! embedded newline is dropped rather than silently corrupted.
//!
//! Repeated keys are legal and keep their order, which is how lists are
//! stored: one `file = …` line per recent file.

use std::str::FromStr;

/// First line of every file, so a future format change can be told apart.
pub const MAGIC: &str = "# i-edit";

/// Bumped whenever the meaning of a key changes.
pub const FORMAT_VERSION: u32 = 1;

/// One section's contents, in file order.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Document {
    entries: Vec<(String, String)>,
}

impl Document {
    pub fn new() -> Self {
        Self::default()
    }

    /// Parses `lines`, skipping comments, blanks, and anything without ` = `.
    ///
    /// A malformed line costs that one entry; the rest of the file still
    /// loads. That is what lets an older or newer editor, or a hand edit,
    /// leave a section half-understandable instead of losing all of it.
    pub fn parse(lines: &[String]) -> Self {
        let mut entries = Vec::new();

        for line in lines {
            // Only a CR from a CRLF file is dropped. Nothing else is trimmed:
            // a path may end in a space, and a value that survives a write
            // must come back byte for byte.
            let line = line.strip_suffix('\r').unwrap_or(line);
            if line.is_empty() || line.starts_with('#') {
                continue;
            }

            let Some((key, value)) = line.split_once('=') else {
                continue;
            };

            let key = key.trim_end();
            if key.is_empty() {
                continue;
            }

            // One optional space after `=`, so values keep any further
            // whitespace — a trailing space in a path is real.
            let value = value.strip_prefix(' ').unwrap_or(value);
            entries.push((key.to_string(), value.to_string()));
        }

        Self { entries }
    }

    /// The first value stored under `key`.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.entries
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, value)| value.as_str())
    }

    /// `get`, parsed. A value that does not parse counts as absent, so a
    /// hand-edited section falls back to the default instead of failing.
    pub fn get_parsed<T: FromStr>(&self, key: &str) -> Option<T> {
        self.get(key)?.parse().ok()
    }

    /// `get`, falling back to `default` when the key is missing or empty.
    pub fn get_or<'a>(&'a self, key: &str, default: &'a str) -> &'a str {
        match self.get(key) {
            Some(value) if !value.is_empty() => value,
            _ => default,
        }
    }

    /// Every value stored under `key`, in file order.
    pub fn all<'a>(&'a self, key: &'a str) -> impl Iterator<Item = &'a str> + 'a {
        self.entries
            .iter()
            .filter(move |(k, _)| k == key)
            .map(|(_, value)| value.as_str())
    }

    /// Stores `key` once, replacing any previous value.
    ///
    /// Returns `false` and stores nothing when `value` holds a line break.
    pub fn set(&mut self, key: &str, value: impl Into<String>) -> bool {
        let value = value.into();
        if !representable(&value) {
            return false;
        }

        self.entries.retain(|(k, _)| k != key);
        self.entries.push((key.to_string(), value));
        true
    }

    /// Adds another value under `key`, keeping the existing ones.
    ///
    /// Returns `false` and stores nothing when `value` holds a line break.
    pub fn push(&mut self, key: &str, value: impl Into<String>) -> bool {
        let value = value.into();
        if !representable(&value) {
            return false;
        }

        self.entries.push((key.to_string(), value));
        true
    }

    /// Renders the section as lines, ready for
    /// [`write_text_file`](crate::fs::write_text_file).
    pub fn lines(&self, section: &str) -> Vec<String> {
        let mut lines = Vec::with_capacity(self.entries.len() + 1);
        lines.push(format!("{MAGIC} {section} v{FORMAT_VERSION}"));

        for (key, value) in &self.entries {
            lines.push(format!("{key} = {value}"));
        }

        lines
    }
}

/// Whether `value` can survive a write and read unchanged.
fn representable(value: &str) -> bool {
    !value.contains(['\n', '\r'])
}

#[cfg(test)]
mod tests {
    use super::Document;

    fn lines(text: &str) -> Vec<String> {
        text.lines().map(str::to_string).collect()
    }

    #[test]
    fn round_trips_scalars_and_lists() {
        let mut doc = Document::new();
        assert!(doc.set("last_dir", "/tmp/work"));
        assert!(doc.push("file", "/tmp/a.rs"));
        assert!(doc.push("file", "/tmp/b.rs"));

        let rendered = doc.lines("state");
        let parsed = Document::parse(&rendered);

        assert_eq!(parsed.get("last_dir"), Some("/tmp/work"));
        assert_eq!(
            parsed.all("file").collect::<Vec<_>>(),
            ["/tmp/a.rs", "/tmp/b.rs"]
        );
        assert_eq!(parsed, doc);
    }

    #[test]
    fn skipped_lines_do_not_lose_the_rest() {
        let doc = Document::parse(&lines(
            "# i-edit state v1\nnot a pair\nbroken =\n = value\nflag = true\n",
        ));

        assert_eq!(doc.get("flag"), Some("true"));
        // Stored, but empty: `get_or` is what turns an empty value into a
        // default.
        assert_eq!(doc.get("broken"), Some(""));
    }

    #[test]
    fn values_keep_their_spaces_apart_from_one_leading_one() {
        let doc = Document::parse(&lines("path =  /a b/c  \n"));
        assert_eq!(doc.get("path"), Some(" /a b/c  "));
    }

    #[test]
    fn unrepresentable_values_are_refused() {
        let mut doc = Document::new();
        assert!(!doc.set("path", "/a\nb"));
        assert!(doc.set("path", "/a\\nb"));
        assert_eq!(doc.get("path"), Some("/a\\nb"));
    }
}

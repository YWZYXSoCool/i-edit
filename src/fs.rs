//! File-system helpers for opening, saving, and browsing files.
//!
//! The editor represents a document as a `Vec<String>` of lines; this module is
//! the only place that translates between that representation and the bytes on
//! disk. Files are UTF-8 text only, read line by line with either line ending,
//! and written back with `\n` and a trailing newline.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

/// Refuse to open files larger than this.
pub const MAX_FILE_SIZE: u64 = 10 * 1024 * 1024;

/// How many leading bytes are inspected when deciding whether a file is binary.
const BINARY_SNIFF_LEN: usize = 8192;

/// One entry of a directory listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    pub path: PathBuf,
    pub name: String,
    pub is_dir: bool,
    pub is_hidden: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum FsError {
    #[error("{0}")]
    Io(#[from] std::io::Error),

    #[error("not a text file")]
    NotText,

    #[error("file too large ({0} bytes, limit 10 MiB)")]
    TooLarge(u64),
}

/// Reads `path` into lines, one at a time, so the whole file is never held in
/// memory alongside the resulting lines.
///
/// A trailing newline is not part of the document, so removing exactly one of
/// them makes read -> write -> read a round trip. The result always holds at
/// least one line, even for an empty file, because the editor never deals with
/// a buffer that has no lines.
pub fn read_text_file(path: &Path) -> Result<Vec<String>, FsError> {
    let len = std::fs::metadata(path)?.len();
    if len > MAX_FILE_SIZE {
        return Err(FsError::TooLarge(len));
    }

    let mut reader = BufReader::new(std::fs::File::open(path)?);
    let mut lines = Vec::new();
    // Bytes read so far, so the sniff window only covers the leading bytes.
    let mut sniffed = 0usize;

    loop {
        let mut buf = Vec::new();
        if reader.read_until(b'\n', &mut buf)? == 0 {
            break;
        }

        // A NUL early in the file is the cheap binary tell; stopping after the
        // sniff window keeps this O(1) in the file size.
        let window = BINARY_SNIFF_LEN.saturating_sub(sniffed);
        if buf[..window.min(buf.len())].contains(&0) {
            return Err(FsError::NotText);
        }
        sniffed += buf.len();

        // Trim the line ending in place, so the buffer read above can be moved
        // into the result instead of allocated a second time.
        if buf.last() == Some(&b'\n') {
            buf.pop();
        }
        if buf.last() == Some(&b'\r') {
            buf.pop();
        }

        // The byte-order mark is metadata and only ever leads the first line.
        if lines.is_empty() && buf.starts_with("\u{feff}".as_bytes()) {
            buf.drain(.."\u{feff}".len());
        }

        let line = String::from_utf8(buf).map_err(|_| FsError::NotText)?;
        lines.push(line);
    }

    // The editor never deals with a buffer that has no lines.
    if lines.is_empty() {
        lines.push(String::new());
    }

    Ok(lines)
}

/// Writes `lines` as UTF-8 text with `\n` separators and a trailing newline.
pub fn write_text_file(path: &Path, lines: &[String]) -> Result<(), FsError> {
    // One allocation, sized exactly: every line's bytes plus one newline each.
    // An empty slice still writes a lone "\n", exactly as the old
    // `join`-plus-`push` did, so `max(1)` keeps that case realloc-free too.
    let capacity: usize = lines.iter().map(|line| line.len() + 1).sum();
    let mut content = String::with_capacity(capacity.max(1));
    if lines.is_empty() {
        content.push('\n');
    }
    for line in lines {
        content.push_str(line);
        content.push('\n');
    }

    std::fs::write(path, content)?;
    Ok(())
}

/// Lists the entries in `path`, directories first.
///
/// Entries that disappear or cannot be stat'ed mid-listing are skipped: a stale
/// entry should not make the rest of the directory unreadable. Symlinks are
/// listed as whatever they point at.
pub fn list_dir(path: &Path) -> Result<Vec<DirEntry>, FsError> {
    let mut entries = Vec::new();

    for entry in std::fs::read_dir(path)? {
        let Ok(entry) = entry else { continue };
        let path = entry.path();
        // The free function follows symlinks; `DirEntry::metadata` would not.
        let Ok(metadata) = std::fs::metadata(&path) else {
            continue;
        };

        let name = entry.file_name().to_string_lossy().into_owned();
        let is_hidden = is_hidden_name(&name);
        entries.push(DirEntry {
            path,
            name,
            is_dir: metadata.is_dir(),
            is_hidden,
        });
    }

    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| lowercase_chars(&a.name).cmp(lowercase_chars(&b.name)))
            .then_with(|| a.name.cmp(&b.name))
    });

    Ok(entries)
}

/// Case-insensitive iteration over `name`, allocating nothing.
///
/// Folding character by character differs from `str::to_lowercase` only for the
/// rare context-sensitive mappings, such as a Greek capital sigma at the end of
/// a word (folded to `ς` as a whole string, to `σ` here). That cannot change
/// how real file names sort, and comparing folded characters is still a total
/// order.
fn lowercase_chars(name: &str) -> impl Iterator<Item = char> + '_ {
    name.chars().flat_map(char::to_lowercase)
}

/// Whether `name` is a dotfile. Windows hidden attributes are deliberately not
/// consulted, so the flag means the same thing on every platform.
pub fn is_hidden_name(name: &str) -> bool {
    name.starts_with('.')
}

/// Expands `~` and resolves relative paths against the current directory.
///
/// The input may come straight from a text field, so it is trimmed first. When
/// a base cannot be determined (no home variable, no current directory) the
/// input is returned verbatim rather than guessed at.
pub fn expand_path(input: &str) -> PathBuf {
    let input = input.trim();

    if input.is_empty() {
        return PathBuf::new();
    }

    if let Some(rest) = input
        .strip_prefix("~/")
        .or_else(|| input.strip_prefix("~\\"))
    {
        return match home_dir() {
            Some(home) => home.join(rest),
            None => PathBuf::from(input),
        };
    }

    if input == "~" {
        return home_dir().unwrap_or_else(|| PathBuf::from(input));
    }

    let path = Path::new(input);
    if path.is_absolute() {
        return path.to_path_buf();
    }

    std::env::current_dir()
        .map(|cwd| cwd.join(path))
        .unwrap_or_else(|_| path.to_path_buf())
}

/// `HOME` first so Unix shells and Git Bash work the same way; `USERPROFILE`
/// covers plain Windows environments where `HOME` is unset.
fn home_dir() -> Option<PathBuf> {
    ["HOME", "USERPROFILE"]
        .into_iter()
        .find_map(|key| std::env::var_os(key).filter(|value| !value.is_empty()))
        .map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::{
        DirEntry, FsError, MAX_FILE_SIZE, expand_path, home_dir, is_hidden_name, list_dir,
        read_text_file, write_text_file,
    };

    /// A unique directory that deletes itself, so tests can run in parallel and
    /// leave nothing behind even when they panic.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let unique = NEXT.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "i-edit-fs-test-{}-{}",
                std::process::id(),
                unique
            ));

            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn reads_lines_and_strips_one_trailing_newline() {
        let dir = TempDir::new();
        let cases: [(&str, &[&str]); 10] = [
            ("", &[""]),
            ("only line", &["only line"]),
            ("a\n", &["a"]),
            ("a\n\n", &["a", ""]),
            ("a\nb", &["a", "b"]),
            ("a\nb\n", &["a", "b"]),
            ("a\r\nb\r\n", &["a", "b"]),
            ("a\r\n\r\n", &["a", ""]),
            ("a\rb\n", &["a\rb"]),
            ("\u{feff}a\n", &["a"]),
        ];

        for (i, (input, expected)) in cases.into_iter().enumerate() {
            let path = dir.path().join(format!("case-{i}.txt"));
            fs::write(&path, input).unwrap();

            assert_eq!(read_text_file(&path).unwrap(), expected, "input: {input:?}");
        }
    }

    #[test]
    fn rejects_binary_and_invalid_utf8() {
        let dir = TempDir::new();

        let nul = dir.path().join("nul.bin");
        fs::write(&nul, b"text\0more").unwrap();
        assert!(matches!(read_text_file(&nul), Err(FsError::NotText)));

        let invalid = dir.path().join("invalid.bin");
        fs::write(&invalid, [0xff, 0xff]).unwrap();
        assert!(matches!(read_text_file(&invalid), Err(FsError::NotText)));
    }

    #[test]
    fn rejects_files_over_the_size_limit() {
        let dir = TempDir::new();
        let path = dir.path().join("big.txt");

        // Sparse file: setting the length costs no disk space.
        let file = fs::File::create(&path).unwrap();
        file.set_len(MAX_FILE_SIZE + 1).unwrap();
        drop(file);

        assert!(matches!(
            read_text_file(&path),
            Err(FsError::TooLarge(len)) if len == MAX_FILE_SIZE + 1
        ));
    }

    #[test]
    fn write_then_read_round_trips() {
        let dir = TempDir::new();
        let path = dir.path().join("round-trip.txt");

        let lines = vec!["first".to_string(), String::new(), "third".to_string()];
        write_text_file(&path, &lines).unwrap();

        assert_eq!(fs::read_to_string(&path).unwrap(), "first\n\nthird\n");
        assert_eq!(read_text_file(&path).unwrap(), lines);
    }

    #[test]
    fn write_text_file_keeps_the_join_based_bytes_on_edge_cases() {
        let dir = TempDir::new();
        let cases: [(&[&str], &str); 4] = [
            // The old `join`-plus-`push` version wrote a lone newline even for
            // an empty slice.
            (&[], "\n"),
            (&[""], "\n"),
            (&["a"], "a\n"),
            (&["a", "", "b"], "a\n\nb\n"),
        ];

        for (i, (lines, expected)) in cases.into_iter().enumerate() {
            let path = dir.path().join(format!("write-{i}.txt"));
            let lines: Vec<String> = lines.iter().map(|line| line.to_string()).collect();
            write_text_file(&path, &lines).unwrap();

            assert_eq!(
                fs::read_to_string(&path).unwrap(),
                expected,
                "lines: {lines:?}"
            );
        }
    }

    #[test]
    fn list_dir_sorts_dirs_first_and_flags_hidden_files() {
        let dir = TempDir::new();
        fs::create_dir(dir.path().join("zebra")).unwrap();
        fs::create_dir(dir.path().join("Alpha")).unwrap();
        for name in [".hidden", "Alpha.txt", "beta.txt"] {
            fs::write(dir.path().join(name), "x").unwrap();
        }

        let entries = list_dir(dir.path()).unwrap();
        let names: Vec<&str> = entries.iter().map(|entry| entry.name.as_str()).collect();
        // Directories first, both groups case-insensitively by name.
        assert_eq!(
            names,
            ["Alpha", "zebra", ".hidden", "Alpha.txt", "beta.txt"]
        );

        let entry =
            |name: &str| -> &DirEntry { entries.iter().find(|entry| entry.name == name).unwrap() };
        assert!(entry("Alpha").is_dir);
        assert_eq!(entry("Alpha").path, dir.path().join("Alpha"));
        assert!(!entry("Alpha.txt").is_dir);
        assert!(entry(".hidden").is_hidden);
        assert!(!entry("beta.txt").is_hidden);
        assert!(!is_hidden_name("visible.txt"));
    }

    #[test]
    fn list_dir_sorts_mixed_case_names_case_insensitively() {
        let dir = TempDir::new();
        for name in ["delta.txt", "Bravo.txt", "CHARLIE.txt", "alpha.txt"] {
            fs::write(dir.path().join(name), "x").unwrap();
        }

        let entries = list_dir(dir.path()).unwrap();
        let names: Vec<&str> = entries.iter().map(|entry| entry.name.as_str()).collect();
        // A case-sensitive byte order would have put "Bravo.txt" first.
        assert_eq!(
            names,
            ["alpha.txt", "Bravo.txt", "CHARLIE.txt", "delta.txt"]
        );
    }

    #[test]
    fn expand_path_handles_empty_relative_absolute_and_home() {
        assert_eq!(expand_path(""), PathBuf::new());
        assert_eq!(expand_path("   "), PathBuf::new());

        let dir = TempDir::new();
        let absolute = dir.path().to_string_lossy();
        assert_eq!(expand_path(&format!("  {absolute}  ")), dir.path());

        let relative = PathBuf::from("some/relative/file.txt");
        assert_eq!(
            expand_path("some/relative/file.txt"),
            std::env::current_dir().unwrap().join(relative)
        );

        match home_dir() {
            Some(home) => {
                assert_eq!(expand_path("~"), home);
                assert_eq!(expand_path("~/sub"), home.join("sub"));
                assert_eq!(expand_path("~\\sub"), home.join("sub"));
            }
            None => {
                assert_eq!(expand_path("~"), PathBuf::from("~"));
                assert_eq!(expand_path("~/sub"), PathBuf::from("~/sub"));
            }
        }
    }
}

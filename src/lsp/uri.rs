//! `file://` URI construction.
//!
//! LSP identifies documents by URI, not by path, and the two are not
//! interchangeable: a Windows path like `C:\Users\me\a.rs` is written
//! `file:///c%3A/Users/me/a.rs`. Getting this wrong is silent — the server
//! accepts the URI, finds no file behind it and returns an empty token set, so
//! the editor simply looks uncolored with nothing in the log.
//!
//! Only the path → URI direction is implemented. That is the only direction the
//! client needs: every URI the server sends back is either ignored (progress,
//! diagnostics in v1) or already known by the time it arrives. A decoder would
//! be dead code, and untested dead code at that.

use std::path::Path;

const HEX: &[u8; 16] = b"0123456789ABCDEF";

/// Builds the `file://` URI for `path`.
///
/// The result is percent-encoded per RFC 3986: `/` separates segments and is
/// preserved, everything outside the unreserved set is escaped byte-by-byte
/// (so non-ASCII paths are escaped as their UTF-8 bytes, which is what a URI
/// means by them).
pub fn path_to_uri(path: &Path) -> String {
    let raw = path.to_string_lossy().replace('\\', "/");
    // The Windows verbatim prefix (`\\?\C:\...`) carries no meaning in a URI.
    let raw = raw.strip_prefix("//?/").unwrap_or(raw.as_str());
    // A trailing separator addresses nothing; drop it so `C:\dir\` and
    // `C:\dir` are the same document.
    let raw = trim_trailing_slashes(raw);

    let mut out = String::with_capacity(raw.len() + 16);
    out.push_str("file://");

    // UNC: `//server/share/...` → authority `server`, then the share path.
    if let Some(rest) = raw.strip_prefix("//") {
        let (authority, rest) = match rest.find('/') {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, ""),
        };
        encode(authority, &mut out);
        if !rest.starts_with('/') {
            out.push('/');
        }
        encode(rest, &mut out);
        return out;
    }

    // A drive letter becomes a single lower-cased path segment with the colon
    // escaped. An escaped colon is not decoration: an unescaped one makes the
    // URI parse as if `c` were a scheme.
    let bytes = raw.as_bytes();
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        out.push('/');
        out.push((bytes[0] as char).to_ascii_lowercase());
        out.push_str("%3A");
        encode(&raw[2..], &mut out);
        return out;
    }

    if !raw.starts_with('/') {
        out.push('/');
    }
    encode(raw, &mut out);
    out
}

fn trim_trailing_slashes(s: &str) -> &str {
    let trimmed = s.trim_end_matches('/');
    // `C:/` and `/` are roots; trimming them would lose the path entirely.
    if trimmed.is_empty() || trimmed.ends_with(':') {
        s
    } else {
        trimmed
    }
}

/// Escapes everything except `/` and the RFC 3986 unreserved characters.
fn encode(s: &str, out: &mut String) {
    for b in s.bytes() {
        match b {
            b'/' => out.push('/'),
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char)
            }
            _ => {
                out.push('%');
                out.push(HEX[(b >> 4) as usize] as char);
                out.push(HEX[(b & 0xf) as usize] as char);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::path_to_uri;
    use std::path::{Path, PathBuf};

    fn uri(s: &str) -> String {
        path_to_uri(Path::new(s))
    }

    #[test]
    fn drive_letter_is_lowercased_and_its_colon_escaped() {
        // Both cases must land on the same URI: the server compares URIs as
        // strings, so an inconsistent case would open the file twice.
        assert_eq!(uri(r"C:\Users\me\a.rs"), "file:///c%3A/Users/me/a.rs");
        assert_eq!(uri(r"c:\Users\me\a.rs"), "file:///c%3A/Users/me/a.rs");
        // Forward slashes are already accepted by `Path`.
        assert_eq!(
            uri("E:/i-edit/src/main.rs"),
            "file:///e%3A/i-edit/src/main.rs"
        );
    }

    #[test]
    fn drive_root_keeps_its_slash() {
        assert_eq!(uri(r"C:\"), "file:///c%3A/");
        assert_eq!(uri("C:/"), "file:///c%3A/");
    }

    #[test]
    fn spaces_are_escaped() {
        assert_eq!(
            uri(r"C:\dir with space\a.rs"),
            "file:///c%3A/dir%20with%20space/a.rs"
        );
    }

    #[test]
    fn non_ascii_is_escaped_as_utf8_bytes() {
        // 中文 is E4 B8 AD E6 96 87 in UTF-8.
        assert_eq!(uri(r"C:\中文\a.rs"), "file:///c%3A/%E4%B8%AD%E6%96%87/a.rs");
    }

    #[test]
    fn trailing_separator_is_dropped() {
        assert_eq!(uri(r"C:\dir\"), "file:///c%3A/dir");
        assert_eq!(uri(r"C:\dir"), "file:///c%3A/dir");
    }

    #[test]
    fn unc_becomes_an_authority() {
        assert_eq!(uri(r"\\server\share\a.rs"), "file://server/share/a.rs");
    }

    #[test]
    fn posix_paths_get_a_leading_slash() {
        assert_eq!(uri("/home/me/a.rs"), "file:///home/me/a.rs");
    }

    #[test]
    fn verbatim_prefix_is_stripped() {
        assert_eq!(uri(r"\\?\C:\dir\a.rs"), "file:///c%3A/dir/a.rs");
    }

    #[test]
    fn round_trips_through_a_real_temp_dir() {
        // Whatever the platform, the URI must name the same file when read
        // back by stripping the scheme and unescaping.
        let dir = std::env::temp_dir().join("i-edit-uri-test");
        let path: PathBuf = dir.join("a b.rs");
        let out = path_to_uri(&path);
        assert!(out.starts_with("file:///"), "unexpected: {out}");
        assert!(!out.contains('\\'), "backslash leaked into {out}");
        assert!(out.ends_with("a%20b.rs"), "unexpected: {out}");
    }
}

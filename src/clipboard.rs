//! System-level clipboard with an internal fallback.
//!
//! Copy / cut write to the **system** clipboard (so other applications can paste)
//! and mirror the text into an in-memory register that survives even when no
//! system backend is available. Paste prefers the system clipboard and falls back
//! to the internal register when the backend cannot be read (e.g. a remote
//! terminal whose OSC 52 readback is unsupported).
//!
//! Three backends are provided behind a single [`ClipboardBackend`] trait:
//! - [`NativeBackend`] wraps `arboard` and talks to the OS clipboard directly
//!   (gated behind the `system-clipboard` feature, on by default).
//! - [`Osc52Backend`] emits the OSC 52 escape sequence, for SSH / tmux sessions.
//! - [`NullBackend`] does nothing; the internal register is the only store.
//!
//! [`Clipboard::new`] picks a backend from a [`ClipboardBackendKind`]; `Auto`
//! tries native first, then OSC 52, then null.

use std::io::{self, Write};

/// Which clipboard backend to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ClipboardBackendKind {
    /// Native on a local desktop, else OSC 52, else the internal register.
    #[default]
    Auto,
    /// Always the OS-native clipboard.
    Native,
    /// Always the OSC 52 escape sequence.
    Osc52,
    /// Only the in-memory register; never touches the system.
    InternalOnly,
}

/// A system clipboard backend: read from and write to the OS clipboard.
pub trait ClipboardBackend {
    /// Reads the system clipboard, or `None` when reading is unsupported or
    /// currently unavailable (the caller then falls back to its own register).
    fn read(&self) -> Option<String>;
    /// Writes `text` to the system clipboard, returning `true` on success.
    fn write(&mut self, text: &str) -> bool;
}

/// No-op backend: the [`Clipboard`] manager's internal register is the only
/// store. Used for `InternalOnly` and as the final fallback in `Auto`.
pub struct NullBackend;

impl ClipboardBackend for NullBackend {
    fn read(&self) -> Option<String> {
        None
    }
    fn write(&mut self, _text: &str) -> bool {
        false
    }
}

/// OSC 52 backend: base64-encodes `text` and writes the
/// `\x1b]52;c;<base64>\x07` sequence to the supplied writer (typically stdout).
///
/// Reading is not supported by most terminals, so [`ClipboardBackend::read`]
/// always returns `None`; the manager falls back to its internal register.
pub struct Osc52Backend<W: Write + Send> {
    writer: W,
}

impl<W: Write + Send> Osc52Backend<W> {
    pub fn new(writer: W) -> Self {
        Self { writer }
    }
}

impl<W: Write + Send> ClipboardBackend for Osc52Backend<W> {
    fn read(&self) -> Option<String> {
        None
    }

    fn write(&mut self, text: &str) -> bool {
        use base64::Engine;
        let encoded = base64::engine::general_purpose::STANDARD.encode(text.as_bytes());
        // BEL (`\x07`) terminates the sequence; most terminals also accept ST
        // (`\x1b\\`), but BEL is the broadly supported choice.
        let seq = format!("\x1b]52;c;{encoded}\x07");
        let writer = &mut self.writer;
        writer.write_all(seq.as_bytes()).is_ok() && writer.flush().is_ok()
    }
}

/// Native OS clipboard backend, used on local desktops. Gated behind the
/// `system-clipboard` feature; without it this type does not exist and `Auto`
/// skips straight to OSC 52.
#[cfg(feature = "system-clipboard")]
pub struct NativeBackend {
    inner: std::sync::Mutex<arboard::Clipboard>,
}

#[cfg(feature = "system-clipboard")]
impl NativeBackend {
    /// Opens the OS clipboard. Returns `Err` when the platform clipboard cannot
    /// be initialised (headless CI, a missing display server, …).
    fn open() -> io::Result<Self> {
        Ok(Self {
            inner: std::sync::Mutex::new(arboard::Clipboard::new().map_err(io::Error::other)?),
        })
    }
}

#[cfg(feature = "system-clipboard")]
impl ClipboardBackend for NativeBackend {
    fn read(&self) -> Option<String> {
        self.inner.lock().ok()?.get_text().ok()
    }

    fn write(&mut self, text: &str) -> bool {
        match self.inner.lock() {
            Ok(mut clip) => clip.set_text(text.to_owned()).is_ok(),
            Err(_) => false,
        }
    }
}

/// The clipboard the editor uses: an internal register plus a system backend.
///
/// `read`/`write` failures on the backend never surface as errors here — they
/// simply leave the internal register as the authoritative store.
pub struct Clipboard {
    register: String,
    backend: Box<dyn ClipboardBackend>,
}

impl std::fmt::Debug for Clipboard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The backend is opaque and intentionally not inspected; report the
        // register length so `EditorState` can still derive `Debug`.
        f.debug_struct("Clipboard")
            .field("register_len", &self.register.len())
            .finish()
    }
}

impl Clipboard {
    /// Builds a clipboard with the given backend kind. `writer` is used only by
    /// the OSC 52 backend (e.g. stdout); other backends ignore it.
    pub fn new(kind: ClipboardBackendKind, writer: Box<dyn Write + Send>) -> Self {
        let backend: Box<dyn ClipboardBackend> = match kind {
            ClipboardBackendKind::InternalOnly => Box::new(NullBackend),
            ClipboardBackendKind::Osc52 => Box::new(Osc52Backend::new(writer)),
            ClipboardBackendKind::Native => match Self::make_native() {
                Some(b) => b,
                None => Box::new(NullBackend),
            },
            ClipboardBackendKind::Auto => match Self::make_native() {
                Some(b) => b,
                None => Box::new(Osc52Backend::new(writer)),
            },
        };
        Self {
            register: String::new(),
            backend,
        }
    }

    /// Tries to open the native backend. Returns `None` when the feature is off
    /// or the platform clipboard cannot be initialised.
    #[cfg(feature = "system-clipboard")]
    fn make_native() -> Option<Box<dyn ClipboardBackend>> {
        NativeBackend::open()
            .ok()
            .map(|b| Box::new(b) as Box<dyn ClipboardBackend>)
    }

    #[cfg(not(feature = "system-clipboard"))]
    fn make_native() -> Option<Box<dyn ClipboardBackend>> {
        None
    }

    /// Copies `text`: mirrors it into the internal register (always available)
    /// and writes it to the system clipboard when a backend is present.
    pub fn copy(&mut self, text: &str) {
        self.register.clear();
        self.register.push_str(text);
        let _ = self.backend.write(text);
    }

    /// Cuts `text`: identical to [`Self::copy`] — the caller removes the source.
    pub fn cut(&mut self, text: &str) {
        self.copy(text);
    }

    /// Returns the text to paste: the system clipboard when it holds something,
    /// otherwise the internal register.
    pub fn paste_text(&self) -> String {
        self.backend
            .read()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| self.register.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{self, Write};
    use std::sync::{Arc, Mutex};

    /// A backend whose stored value round-trips, for exercising the manager.
    struct MockBackend {
        stored: Option<String>,
        /// When `Some`, `read` returns this instead of the stored value — lets a
        /// test simulate "the system clipboard holds something else".
        read_returns: Option<Option<String>>,
    }

    impl ClipboardBackend for MockBackend {
        fn read(&self) -> Option<String> {
            match &self.read_returns {
                Some(v) => v.clone(),
                None => self.stored.clone(),
            }
        }
        fn write(&mut self, text: &str) -> bool {
            self.stored = Some(text.to_owned());
            true
        }
    }

    /// A `Write` that records into a shared `Vec`, so the test can inspect what
    /// the OSC 52 backend emitted.
    struct Spy(Arc<Mutex<Vec<u8>>>);

    impl Write for Spy {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn copy_writes_register_and_backend_and_reads_system_first() {
        let mut clip = Clipboard {
            register: String::new(),
            backend: Box::new(MockBackend {
                stored: None,
                read_returns: None,
            }),
        };
        clip.copy("hello");
        // System backend round-trips, so the read wins.
        assert_eq!(clip.paste_text(), "hello");

        // When the system clipboard holds a *different* value, it must win over
        // the internal register we just copied into.
        let mut clip = Clipboard {
            register: String::new(),
            backend: Box::new(MockBackend {
                stored: Some("sys".into()),
                read_returns: Some(Some("sys".into())),
            }),
        };
        clip.copy("mine");
        assert_eq!(clip.paste_text(), "sys");
    }

    #[test]
    fn read_failure_falls_back_to_register() {
        let mut clip = Clipboard {
            register: String::new(),
            backend: Box::new(MockBackend {
                stored: None,
                read_returns: Some(None),
            }),
        };
        clip.copy("fallback");
        assert_eq!(clip.paste_text(), "fallback");
    }

    #[test]
    fn internal_only_uses_the_register_only() {
        let mut clip = Clipboard::new(ClipboardBackendKind::InternalOnly, Box::new(io::sink()));
        clip.copy("x");
        assert_eq!(clip.paste_text(), "x");
    }

    #[test]
    fn osc52_backend_emits_the_escape_sequence_and_never_reads() {
        let recorded = Arc::new(Mutex::new(Vec::new()));
        let mut backend = Osc52Backend::new(Spy(recorded.clone()));
        assert!(backend.write("hi"));
        assert_eq!(backend.read(), None);

        let bytes = recorded.lock().unwrap().clone();
        let text = String::from_utf8(bytes).unwrap();
        assert!(
            text.starts_with("\x1b]52;c;"),
            "must start with the OSC 52 header"
        );
        assert!(text.ends_with('\x07'), "must terminate with BEL");
        assert!(text.contains("aGk="), "base64 of 'hi' is aGk=");
    }

    #[test]
    fn null_backend_is_silent() {
        let mut backend = NullBackend;
        assert_eq!(backend.read(), None);
        assert!(!backend.write("anything"));
    }
}

//! Everything the editor remembers between runs, and everything it caches
//! within one.
//!
//! Four sections, each its own file under [`paths::root`]:
//!
//! | Section   | File         | Lives for    | Written when                    |
//! | --------- | ------------ | ------------ | ------------------------------- |
//! | `Config`  | `config`     | forever      | a setting changes               |
//! | `State`   | `state`      | forever      | the MRU or a toggle changes     |
//! | `Session` | `session`    | one run      | the buffer or cursor moves      |
//! | `Cache`   | `cache/dirs` | until stale  | listings change (if enabled)    |
//!
//! The split is what makes a bad file survivable: each section parses on its
//! own and falls back to its default, so a corrupt cache cannot cost the
//! recent-files list.
//!
//! Reading never fails. A missing, unreadable, or half-broken section logs a
//! warning and yields defaults — the editor starting up with no history is
//! fine, the editor refusing to start is not. Writing is the opposite: it
//! reports [`crate::Error`] so the caller can tell the user the state was not
//! saved, and never takes the editor down with it.
//!
//! Writes are debounced ([`FLUSH_DEBOUNCE`]) and atomic, so editing settings
//! in a burst costs one write, and a crash mid-write leaves the previous file.
//! [`Storage::tick`] is what the main loop calls; it does the waiting.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::Result;
use crate::fs;

pub mod cache;
pub mod codec;
pub mod config;
pub mod paths;
pub mod session;
pub mod state;

#[cfg(test)]
mod tests;

pub use cache::Cache;
pub use config::Config;
pub use session::Session;
pub use state::State;

/// How long a section waits after its last change before `tick` writes it.
/// Long enough to swallow a burst of edits, short enough that a crash costs
/// seconds of history.
const FLUSH_DEBOUNCE: Duration = Duration::from_millis(800);

/// One file under the storage root.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    Config,
    State,
    Session,
    Cache,
}

impl Section {
    const ALL: [Self; 4] = [Self::Config, Self::State, Self::Session, Self::Cache];

    fn bit(self) -> u8 {
        1 << self as u8
    }

    /// Path relative to the root.
    fn relative_path(self) -> &'static str {
        match self {
            Self::Config => "config",
            Self::State => "state",
            Self::Session => "session",
            Self::Cache => "cache/dirs",
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Config => "config",
            Self::State => "state",
            Self::Session => "session",
            Self::Cache => "cache",
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("{0}")]
    Io(#[from] std::io::Error),

    /// Raised through [`crate::fs`], which owns the text read and write the
    /// sections are stored with.
    #[error("{0}")]
    Fs(#[from] fs::FsError),
}

/// The editor's memory, on disk.
#[derive(Debug, Default)]
pub struct Storage {
    /// `None` when no root could be determined: storage then keeps everything
    /// in memory and silently never persists.
    root: Option<PathBuf>,
    config: Config,
    state: State,
    session: Session,
    cache: Cache,
    /// Bit set of [`Section::bit`] for sections waiting to be written.
    dirty: u8,
    /// When the first of them became dirty, for the debounce.
    dirty_since: Option<Instant>,
}

impl Storage {
    /// Loads every section from the platform root, or runs in memory when
    /// there is none.
    pub fn load() -> Self {
        match paths::root() {
            Some(root) => Self::open(root),
            None => {
                log::warn!(
                    "storage: no root ({} unset, no home directory); state will not persist",
                    paths::ENV_ROOT
                );
                Self::default()
            }
        }
    }

    /// Loads every section from `root`, which tests own outright.
    pub fn open(root: PathBuf) -> Self {
        let mut storage = Self {
            root: Some(root),
            ..Self::default()
        };
        storage.read_all();
        storage
    }

    pub fn root(&self) -> Option<&Path> {
        self.root.as_deref()
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    pub fn state(&self) -> &State {
        &self.state
    }

    pub fn session(&self) -> &Session {
        &self.session
    }

    pub fn cache(&self) -> &Cache {
        &self.cache
    }

    /// Changes the config and schedules it for writing.
    ///
    /// Editing goes through a closure so the dirty flag cannot be forgotten:
    /// handing out `&mut Config` would leave the caller free to change the
    /// settings behind storage's back.
    pub fn edit_config(&mut self, edit: impl FnOnce(&mut Config)) {
        edit(&mut self.config);
        self.mark_dirty(Section::Config);
    }

    /// Changes the durable state and schedules it for writing.
    pub fn edit_state(&mut self, edit: impl FnOnce(&mut State)) {
        edit(&mut self.state);
        self.mark_dirty(Section::State);
    }

    /// Changes the session and schedules it for writing.
    pub fn edit_session(&mut self, edit: impl FnOnce(&mut Session)) {
        edit(&mut self.session);
        self.mark_dirty(Section::Session);
    }

    /// Changes the cache and schedules it for writing.
    ///
    /// `persist_cache` is consulted at write time, so turning it off mid-run
    /// stops the writes without touching the cache itself.
    pub fn edit_cache(&mut self, edit: impl FnOnce(&mut Cache)) {
        edit(&mut self.cache);
        self.mark_dirty(Section::Cache);
    }

    /// Writes what has changed once it has been dirty long enough.
    ///
    /// Called from the main loop's idle path. A failure is logged, not
    /// returned: a failed background save must not end the editor, and the
    /// section stays dirty for the next attempt.
    pub fn tick(&mut self) {
        if self.dirty == 0 {
            return;
        }

        let Some(since) = self.dirty_since else {
            return;
        };

        if since.elapsed() < FLUSH_DEBOUNCE {
            return;
        }

        if let Err(err) = self.flush() {
            log::warn!("storage: background flush failed: {err}");
        }
    }

    /// Writes every dirty section now. Call this before exiting.
    pub fn flush(&mut self) -> Result<()> {
        let Some(root) = self.root.clone() else {
            self.dirty = 0;
            self.dirty_since = None;
            return Ok(());
        };

        for section in Section::ALL {
            if self.dirty & section.bit() == 0 {
                continue;
            }

            if section == Section::Cache && !self.config.persist_cache {
                self.dirty &= !section.bit();
                continue;
            }

            self.write_section(&root, section)?;
            self.dirty &= !section.bit();
        }

        self.dirty_since = None;
        Ok(())
    }

    /// Whether anything is waiting to be written.
    pub fn is_dirty(&self) -> bool {
        self.dirty != 0
    }

    fn mark_dirty(&mut self, section: Section) {
        if self.dirty == 0 {
            self.dirty_since = Some(Instant::now());
        }
        self.dirty |= section.bit();
    }

    fn read_all(&mut self) {
        let Some(root) = self.root.clone() else {
            return;
        };

        self.config = Config::from_doc(&codec::Document::parse(&read_lines(
            &root.join(Section::Config.relative_path()),
        )));
        self.state = State::from_doc(&codec::Document::parse(&read_lines(
            &root.join(Section::State.relative_path()),
        )));
        self.session = Session::from_doc(&codec::Document::parse(&read_lines(
            &root.join(Section::Session.relative_path()),
        )));
        self.cache = Cache::from_lines(&read_lines(&root.join(Section::Cache.relative_path())));

        // Freshly loaded state is already on disk.
        self.dirty = 0;
        self.dirty_since = None;
    }

    fn write_section(&self, root: &Path, section: Section) -> Result<()> {
        let path = root.join(section.relative_path());

        let lines = match section {
            Section::Config => self.config.to_doc().lines(section.name()),
            Section::State => self.state.to_doc().lines(section.name()),
            Section::Session => self.session.to_doc().lines(section.name()),
            Section::Cache => self.cache.to_lines(),
        };

        write_atomic(&path, &lines)
    }
}

/// Reads a section, returning no lines when it is missing or unreadable.
///
/// "File not found" is the normal case on a first run, so it is quiet;
/// anything else is worth a warning because the user's history is at stake.
fn read_lines(path: &Path) -> Vec<String> {
    match fs::read_text_file(path) {
        Ok(lines) => lines,
        Err(err) => {
            let missing =
                matches!(&err, fs::FsError::Io(io) if io.kind() == std::io::ErrorKind::NotFound);
            if !missing {
                log::warn!("storage: cannot read {}: {err}", path.display());
            }
            Vec::new()
        }
    }
}

/// Writes `lines` to `path` so that `path` is either the old file or the new
/// one, never a half-written mix.
///
/// The temp file sits beside the target, so the final step is a rename within
/// one directory. Windows refuses to rename over an existing file, so that
/// case removes the target first: the window where `path` is missing is the
/// price of replacing it, and a reader that loses the race finds a missing
/// section, which is a case storage already handles.
fn write_atomic(path: &Path, lines: &[String]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let temp = PathBuf::from(format!("{}.tmp", path.display()));
    fs::write_text_file(&temp, lines)?;

    if let Err(err) = std::fs::rename(&temp, path) {
        // Either the rename is unsupported here or the target exists; both are
        // handled by removing the target and trying once more.
        let _ = std::fs::remove_file(path);
        std::fs::rename(&temp, path).map_err(|_| err)?;
    }

    Ok(())
}

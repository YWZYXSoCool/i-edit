//! Session state: what the editor was looking at when it was last closed.
//!
//! Everything here is per-path and best effort — a path that no longer exists
//! is simply never matched again, and stale entries are pushed out by the
//! caps instead of being validated on load.

use std::path::{Path, PathBuf};

use crate::storage::codec::Document;

/// Cursor positions remembered per file.
pub const MAX_VIEWS: usize = 200;

/// Expanded directories remembered for the file tree.
pub const MAX_EXPANDED_DIRS: usize = 500;

/// Tabs reopened on startup.
///
/// Restoring one means reading its file, so a session that left hundreds open
/// is cut short rather than turning the start into a hundred reads: the tabs
/// that come back are the first ones in stored order, the rest are left for
/// the user to open again.
pub const MAX_TABS: usize = 32;

/// Where a buffer was left: cursor line, byte column, and the top line shown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileView {
    pub path: PathBuf,
    /// Cursor line index.
    pub line: usize,
    /// Cursor byte offset within the line.
    pub col: usize,
    /// First line drawn, so the view does not jump to the top.
    pub top: usize,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Session {
    /// Buffer to reopen, and of the tabs below the one that was current;
    /// `None` when the last run ended on a scratch buffer.
    pub last_file: Option<PathBuf>,
    /// Folder to reopen as the file tree root.
    pub last_folder: Option<PathBuf>,
    /// The file-backed buffers that were open, in tab-bar order. Scratch
    /// buffers have no path to read back, so they are not in here.
    tabs: Vec<PathBuf>,
    /// Most recently viewed first.
    views: Vec<FileView>,
    /// Directories left expanded in the file tree.
    expanded: Vec<PathBuf>,
}

impl Session {
    pub(crate) fn from_doc(doc: &Document) -> Self {
        let mut session = Self {
            last_file: doc.get("last_file").map(PathBuf::from),
            last_folder: doc.get("folder").map(PathBuf::from),
            tabs: Vec::new(),
            views: Vec::new(),
            expanded: Vec::new(),
        };

        // `line col top <path>`: the numbers first so the path, which may
        // contain spaces, is the remainder of the line.
        for value in doc.all("view") {
            let mut parts = value.splitn(4, ' ');
            let (Some(line), Some(col), Some(top), Some(path)) =
                (parts.next(), parts.next(), parts.next(), parts.next())
            else {
                continue;
            };

            let (Ok(line), Ok(col), Ok(top)) = (line.parse(), col.parse(), top.parse()) else {
                continue;
            };

            session.views.push(FileView {
                path: PathBuf::from(path),
                line,
                col,
                top,
            });
            if session.views.len() >= MAX_VIEWS {
                break;
            }
        }

        for path in doc.all("expanded").take(MAX_EXPANDED_DIRS) {
            session.expanded.push(PathBuf::from(path));
        }

        // One `tab` line per open buffer, in the order the bar showed them.
        for path in doc.all("tab").take(MAX_TABS) {
            session.tabs.push(PathBuf::from(path));
        }

        session
    }

    pub(crate) fn to_doc(&self) -> Document {
        let mut doc = Document::new();

        if let Some(path) = &self.last_file {
            doc.set("last_file", path.display().to_string());
        }
        if let Some(path) = &self.last_folder {
            doc.set("folder", path.display().to_string());
        }

        for view in &self.views {
            // Dropped when the path cannot be represented; see `codec`.
            doc.push(
                "view",
                format!(
                    "{} {} {} {}",
                    view.line,
                    view.col,
                    view.top,
                    view.path.display()
                ),
            );
        }

        for path in &self.expanded {
            doc.push("expanded", path.display().to_string());
        }

        for path in &self.tabs {
            doc.push("tab", path.display().to_string());
        }

        doc
    }

    /// The tabs to reopen on the next run, in the order they were left in.
    pub fn tabs(&self) -> &[PathBuf] {
        &self.tabs
    }

    /// Records which buffers are open, replacing whatever was remembered.
    ///
    /// Duplicates collapse — two tabs cannot hold the same file — and the
    /// list is cut at [`MAX_TABS`], so a hand-edited or very old session
    /// cannot ask the next run to read hundreds of files.
    pub fn set_tabs(&mut self, tabs: impl IntoIterator<Item = PathBuf>) {
        self.tabs.clear();

        for path in tabs {
            if self.tabs.contains(&path) {
                continue;
            }
            self.tabs.push(path);
        }

        self.tabs.truncate(MAX_TABS);
    }

    /// Where `path` was left, if it was left anywhere.
    pub fn view(&self, path: &Path) -> Option<&FileView> {
        self.views.iter().find(|view| view.path == path)
    }

    /// Records where `path` is now, moving it to the front and evicting the
    /// least recently used entry past the cap.
    pub fn remember_view(&mut self, path: PathBuf, line: usize, col: usize, top: usize) {
        self.views.retain(|view| view.path != path);
        self.views.insert(
            0,
            FileView {
                path,
                line,
                col,
                top,
            },
        );
        self.views.truncate(MAX_VIEWS);
    }

    pub fn is_expanded(&self, path: &Path) -> bool {
        self.expanded.iter().any(|dir| dir == path)
    }

    /// Points the session at `folder` and drops the expansion set with it.
    ///
    /// The set belongs to one root: the tree clears its own cache when the
    /// root changes, so carrying the old directories over would only restore
    /// paths that are no longer in the tree.
    pub fn set_folder(&mut self, folder: PathBuf) {
        self.last_folder = Some(folder);
        self.expanded.clear();
    }

    /// Forgets the folder and the expansion set that belonged to it.
    ///
    /// The counterpart of [`Self::set_folder`]: without it the next run would
    /// reopen a folder the user has just closed.
    pub fn clear_folder(&mut self) {
        self.last_folder = None;
        self.expanded.clear();
    }

    pub fn set_expanded(&mut self, path: PathBuf, expanded: bool) {
        if expanded {
            if !self.is_expanded(&path) {
                self.expanded.insert(0, path);
                self.expanded.truncate(MAX_EXPANDED_DIRS);
            }
        } else {
            self.expanded.retain(|dir| dir != &path);
        }
    }

    pub fn expanded_dirs(&self) -> &[PathBuf] {
        &self.expanded
    }
}

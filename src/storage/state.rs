//! Durable editor state: what the UI looked like and where the user has been.
//!
//! Distinct from [`Session`](super::Session), which is per-file view state:
//! this is the single set of toggles and most-recently-used lists that the
//! shell reads on its own behalf.

use std::path::{Path, PathBuf};

use crate::storage::codec::Document;
use crate::storage::config::{bool_word, flag};

/// Default of [`State::file_tree_visible`]; matches `App::new`.
const DEFAULT_FILE_TREE_VISIBLE: bool = true;

/// Recent files kept.
pub const MAX_RECENT_FILES: usize = 20;

/// Recent directories kept.
pub const MAX_RECENT_DIRS: usize = 10;

/// Which picker a start directory belongs to.
///
/// The three overlays are used for different things and land in different
/// places, so each remembers its own: opening a file from `src` must not move
/// where "open folder" starts, and neither should move "save as".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickerKind {
    /// Ctrl+O: opens a file.
    OpenFile,
    /// Ctrl+Shift+O: roots the file tree.
    OpenFolder,
    /// Ctrl+Shift+S: names a new file.
    SaveAs,
}

impl PickerKind {
    const ALL: [Self; 3] = [Self::OpenFile, Self::OpenFolder, Self::SaveAs];

    fn key(self) -> &'static str {
        match self {
            Self::OpenFile => "open_file_dir",
            Self::OpenFolder => "open_folder_dir",
            Self::SaveAs => "save_as_dir",
        }
    }

    /// Index into [`State::picker_dirs`].
    fn index(self) -> usize {
        match self {
            Self::OpenFile => 0,
            Self::OpenFolder => 1,
            Self::SaveAs => 2,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct State {
    /// Whether the file tree panel was showing.
    pub file_tree_visible: bool,
    /// Directory the last open/save happened in; the picker starts here.
    pub last_dir: Option<PathBuf>,
    /// Where each picker was last used, indexed by [`PickerKind::index`].
    picker_dirs: [Option<PathBuf>; 3],
    /// Most recently opened files first.
    recent_files: Vec<PathBuf>,
    /// Most recently opened directories first.
    recent_dirs: Vec<PathBuf>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            file_tree_visible: DEFAULT_FILE_TREE_VISIBLE,
            last_dir: None,
            picker_dirs: [None, None, None],
            recent_files: Vec::new(),
            recent_dirs: Vec::new(),
        }
    }
}

impl State {
    pub(crate) fn from_doc(doc: &Document) -> Self {
        let mut picker_dirs = [None, None, None];
        for kind in PickerKind::ALL {
            picker_dirs[kind.index()] = doc.get(kind.key()).map(PathBuf::from);
        }

        Self {
            file_tree_visible: flag(doc.get("file_tree_visible"), DEFAULT_FILE_TREE_VISIBLE),
            last_dir: doc.get("last_dir").map(PathBuf::from),
            picker_dirs,
            // `take` so a hand-edited file cannot grow the lists past the cap.
            recent_files: doc
                .all("file")
                .take(MAX_RECENT_FILES)
                .map(PathBuf::from)
                .collect(),
            recent_dirs: doc
                .all("dir")
                .take(MAX_RECENT_DIRS)
                .map(PathBuf::from)
                .collect(),
        }
    }

    pub(crate) fn to_doc(&self) -> Document {
        let mut doc = Document::new();
        doc.set("file_tree_visible", bool_word(self.file_tree_visible));

        if let Some(dir) = &self.last_dir {
            doc.set("last_dir", dir.display().to_string());
        }

        for kind in PickerKind::ALL {
            if let Some(dir) = &self.picker_dirs[kind.index()] {
                doc.set(kind.key(), dir.display().to_string());
            }
        }

        for path in &self.recent_files {
            doc.push("file", path.display().to_string());
        }
        for path in &self.recent_dirs {
            doc.push("dir", path.display().to_string());
        }

        doc
    }

    pub fn recent_files(&self) -> &[PathBuf] {
        &self.recent_files
    }

    pub fn recent_dirs(&self) -> &[PathBuf] {
        &self.recent_dirs
    }

    /// Where `kind` was last used, if it has been used at all.
    pub fn picker_dir(&self, kind: PickerKind) -> Option<&PathBuf> {
        self.picker_dirs[kind.index()].as_ref()
    }

    /// Remembers `dir` as where `kind` was last used.
    pub fn set_picker_dir(&mut self, kind: PickerKind, dir: PathBuf) {
        self.picker_dirs[kind.index()] = Some(dir);
    }

    /// Moves `path` to the front of the recent files, evicting the oldest.
    pub fn touch_file(&mut self, path: PathBuf) {
        push_recent(&mut self.recent_files, path, MAX_RECENT_FILES);
    }

    /// Moves `path` to the front of the recent directories, evicting the oldest.
    pub fn touch_dir(&mut self, path: PathBuf) {
        push_recent(&mut self.recent_dirs, path, MAX_RECENT_DIRS);
    }
}

/// Most-recently-used insert: no duplicates, newest first, hard cap.
fn push_recent(list: &mut Vec<PathBuf>, path: PathBuf, cap: usize) {
    list.retain(|entry| entry != &path);
    list.insert(0, path);
    list.truncate(cap);
}

/// Whether `path` is in `list`; kept here so callers do not reach into the
/// field.
pub fn contains(list: &[PathBuf], path: &Path) -> bool {
    list.iter().any(|entry| entry == path)
}

//! Tests for the storage facade.
//!
//! Every test owns a [`TempDir`] root and never touches the real one, so
//! nothing here can cost the user their history.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::SystemTime;

use super::paths::ENV_ROOT;
use super::{Cache, Section, Storage};
use crate::fs::DirEntry;
use crate::storage::config::TabIndent;

/// A unique directory that deletes itself, so tests run in parallel and leave
/// nothing behind even when they panic.
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let unique = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "i-edit-storage-test-{}-{}",
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

fn dir_entry(path: &str, is_dir: bool) -> DirEntry {
    let path = PathBuf::from(path);
    let name = path.file_name().unwrap().to_string_lossy().into_owned();
    DirEntry {
        is_hidden: crate::fs::is_hidden_name(&name),
        path,
        name,
        is_dir,
    }
}

#[test]
fn a_missing_root_loads_defaults_and_stays_clean() {
    let dir = TempDir::new();
    let storage = Storage::open(dir.path().to_path_buf());

    assert!(!storage.is_dirty());
    assert!(storage.config().restore_session);
    assert!(storage.state().file_tree_visible);
    assert!(storage.state().recent_files().is_empty());
    assert!(storage.session().last_file.is_none());
    assert!(storage.cache().is_empty());
}

#[test]
fn state_and_session_survive_a_restart() {
    let dir = TempDir::new();

    {
        let mut storage = Storage::open(dir.path().to_path_buf());
        storage.edit_state(|state| {
            state.file_tree_visible = false;
            state.last_dir = Some(PathBuf::from("/work"));
            state.touch_file(PathBuf::from("/work/a.rs"));
            state.touch_dir(PathBuf::from("/work"));
        });
        storage.edit_session(|session| {
            session.last_file = Some(PathBuf::from("/work/a.rs"));
            session.set_folder(PathBuf::from("/work"));
            session.remember_view(PathBuf::from("/work/a.rs"), 12, 4, 3);
            session.set_expanded(PathBuf::from("/work/src"), true);
        });
        storage.edit_config(|config| config.restore_session = false);

        assert!(storage.is_dirty());
        storage.flush().unwrap();
        assert!(!storage.is_dirty());
    }

    let storage = Storage::open(dir.path().to_path_buf());
    assert!(!storage.state().file_tree_visible);
    assert_eq!(
        storage.state().last_dir.as_deref(),
        Some(Path::new("/work"))
    );
    assert_eq!(
        storage.state().recent_files(),
        [PathBuf::from("/work/a.rs")]
    );
    assert_eq!(storage.state().recent_dirs(), [PathBuf::from("/work")]);
    assert_eq!(
        storage.session().last_file.as_deref(),
        Some(Path::new("/work/a.rs"))
    );
    assert_eq!(
        storage.session().last_folder.as_deref(),
        Some(Path::new("/work"))
    );

    let view = storage.session().view(Path::new("/work/a.rs")).unwrap();
    assert_eq!((view.line, view.col, view.top), (12, 4, 3));
    assert!(storage.session().is_expanded(Path::new("/work/src")));
    assert!(!storage.config().restore_session);
    assert!(!storage.is_dirty());
}

#[test]
fn each_section_writes_only_its_own_file() {
    let dir = TempDir::new();

    let mut storage = Storage::open(dir.path().to_path_buf());
    storage.edit_config(|config| config.persist_cache = false);
    storage.flush().unwrap();

    assert!(dir.path().join("config").exists());
    assert!(!dir.path().join("state").exists());
    assert!(!dir.path().join("session").exists());
    assert!(!dir.path().join("cache/dirs").exists());
}

#[test]
fn a_corrupt_section_falls_back_without_touching_the_others() {
    let dir = TempDir::new();

    {
        let mut storage = Storage::open(dir.path().to_path_buf());
        storage.edit_state(|state| state.touch_file(PathBuf::from("/kept.rs")));
        storage.flush().unwrap();
    }

    // A truncated write, a hand edit gone wrong, a newer editor's file: all
    // three look like this, and none of them may lose the rest.
    fs::write(dir.path().join("state"), "\u{0}not a state file").unwrap();
    fs::create_dir_all(dir.path().join("cache")).unwrap();
    fs::write(dir.path().join("cache/dirs"), "dir without a path\n").unwrap();

    let storage = Storage::open(dir.path().to_path_buf());
    // The corrupt section keeps its default...
    assert!(storage.state().recent_files().is_empty());
    assert!(storage.cache().is_empty());
    // ...and nothing else was involved.
    assert!(storage.config().restore_session);
}

#[test]
fn flush_replaces_an_existing_file() {
    let dir = TempDir::new();
    let path = dir.path().join("state");

    let mut storage = Storage::open(dir.path().to_path_buf());
    storage.edit_state(|state| state.touch_file(PathBuf::from("/first.rs")));
    storage.flush().unwrap();

    storage.edit_state(|state| state.touch_file(PathBuf::from("/second.rs")));
    storage.flush().unwrap();

    // No temp file left behind, and the second write won.
    assert!(!dir.path().join("state.tmp").exists());
    assert_eq!(
        Storage::open(dir.path().to_path_buf())
            .state()
            .recent_files(),
        [PathBuf::from("/second.rs"), PathBuf::from("/first.rs")]
    );
    assert!(path.exists());
}

#[test]
fn changing_the_folder_drops_the_expansion_set_with_it() {
    let mut session = super::Session::default();
    session.set_expanded(PathBuf::from("/work/src"), true);
    assert!(session.is_expanded(Path::new("/work/src")));

    // The directories belong to one root; keeping them across a change would
    // only restore paths the new tree does not contain.
    session.set_folder(PathBuf::from("/other"));
    assert_eq!(session.last_folder.as_deref(), Some(Path::new("/other")));
    assert!(!session.is_expanded(Path::new("/work/src")));
}

#[test]
fn recent_lists_cap_themselves_and_move_to_the_front() {
    let dir = TempDir::new();
    let mut storage = Storage::open(dir.path().to_path_buf());

    storage.edit_state(|state| {
        for i in 0..super::state::MAX_RECENT_FILES + 5 {
            state.touch_file(PathBuf::from(format!("/f{i}.rs")));
        }
        state.touch_file(PathBuf::from("/f3.rs"));
    });

    assert_eq!(
        storage.state().recent_files().len(),
        super::state::MAX_RECENT_FILES
    );
    assert_eq!(storage.state().recent_files()[0], PathBuf::from("/f3.rs"));
    // Pushed out by the five extra files.
    assert!(
        !storage
            .state()
            .recent_files()
            .contains(&PathBuf::from("/f4.rs"))
    );
}

#[test]
fn cache_hits_only_while_the_mtime_matches() {
    let cached = PathBuf::from("/proj/src");
    let read_at = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000);
    let changed_at = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(2_000);

    let mut cache = Cache::default();
    cache.insert(
        cached.clone(),
        read_at,
        vec![dir_entry("/proj/src/a.rs", false)],
    );

    assert_eq!(cache.get(&cached, read_at).map(<[_]>::len), Some(1));
    // A directory edited since the read is a miss, not a stale hit.
    assert!(cache.get(&cached, changed_at).is_none());
    assert!(cache.get(Path::new("/proj/other"), read_at).is_none());

    // Round-tripped through its own file format, mtimes included.
    let restored = Cache::from_lines(&cache.to_lines());
    assert_eq!(restored.get(&cached, read_at).map(<[_]>::len), Some(1));
    assert!(restored.get(&cached, changed_at).is_none());
}

#[test]
fn cache_stays_under_its_cap() {
    let mut cache = Cache::default();
    for i in 0..super::cache::MAX_CACHED_DIRS + 20 {
        cache.insert(
            PathBuf::from(format!("/d{i}")),
            SystemTime::UNIX_EPOCH,
            Vec::new(),
        );
    }

    assert_eq!(cache.len(), super::cache::MAX_CACHED_DIRS);
}

#[test]
fn the_cache_section_is_skipped_when_persistence_is_off() {
    let dir = TempDir::new();

    let mut storage = Storage::open(dir.path().to_path_buf());
    storage.edit_config(|config| config.persist_cache = false);
    storage.edit_cache(|cache| {
        cache.insert(PathBuf::from("/proj"), SystemTime::now(), Vec::new());
    });
    storage.flush().unwrap();

    assert!(!dir.path().join("cache/dirs").exists());
    assert!(!storage.is_dirty());
}

#[test]
fn without_a_root_nothing_is_written_and_nothing_fails() {
    let mut storage = Storage::default();
    storage.edit_state(|state| state.touch_file(PathBuf::from("/a.rs")));

    // No root: the edit is remembered in memory and the flush is a no-op.
    assert!(storage.is_dirty());
    storage.flush().unwrap();
    assert!(!storage.is_dirty());
    assert_eq!(storage.state().recent_files().len(), 1);
}

/// Documents the override without depending on the environment: the name is
/// what `load` reads, and a root passed to `open` wins over it either way.
#[test]
fn the_root_override_is_named_for_the_environment() {
    assert_eq!(ENV_ROOT, "I_EDIT_HOME");
}

#[test]
fn section_paths_stay_where_the_module_doc_says() {
    assert_eq!(Section::Config.relative_path(), "config");
    assert_eq!(Section::State.relative_path(), "state");
    assert_eq!(Section::Session.relative_path(), "session");
    assert_eq!(Section::Cache.relative_path(), "cache/dirs");
}

#[test]
fn the_settings_file_appears_with_its_defaults_and_explains_itself() {
    let dir = TempDir::new();
    let mut storage = Storage::open(dir.path().to_path_buf());

    let path = storage.ensure_settings_file().expect("no storage root");
    assert_eq!(path, dir.path().join("config"));
    assert!(storage.is_settings_file(&path));

    let contents = fs::read_to_string(&path).unwrap();
    assert!(contents.contains("tab_indent = spaces"));
    // The file is meant to be edited by hand, so it has to say what it takes.
    assert!(contents.contains("# tab_indent = spaces | tab"));

    // A second call is what happens on every `settings` command: the file is
    // already there and the user's edits must survive it.
    fs::write(&path, "tab_indent = tab\n").unwrap();
    storage.ensure_settings_file();
    assert_eq!(fs::read_to_string(&path).unwrap(), "tab_indent = tab\n");
}

#[test]
fn rereading_the_settings_takes_the_hand_edit() {
    let dir = TempDir::new();
    let mut storage = Storage::open(dir.path().to_path_buf());
    let path = storage.ensure_settings_file().expect("no storage root");

    assert_eq!(storage.config().tab_indent, TabIndent::Spaces);

    fs::write(&path, "tab_indent = tab\n").unwrap();
    storage.reload_config();

    assert_eq!(storage.config().tab_indent, TabIndent::Tab);
    assert!(!storage.is_dirty());
}

#[test]
fn without_a_root_there_is_no_settings_file_to_open() {
    let mut storage = Storage::default();

    assert!(storage.ensure_settings_file().is_none());
    assert!(!storage.is_settings_file(Path::new("config")));
}

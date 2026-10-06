//! Regenerable caches, kept so a restart does not pay for work already done.
//!
//! The only cache today is directory listings: opening a folder in the file
//! tree reads it, and re-reading it on every startup is the one I/O cost worth
//! avoiding. Every entry carries the `mtime` it was read at, so a stale cache
//! can never show a listing that has changed — a miss just re-reads.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::fs::DirEntry;

/// Directories held. Beyond this the oldest-looking entry is dropped; the
/// cache is an optimisation, so an inexact eviction costs a re-read at worst.
pub const MAX_CACHED_DIRS: usize = 256;

/// One cached listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedDir {
    /// `mtime` of the directory when it was read.
    pub modified: SystemTime,
    pub entries: Vec<DirEntry>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Cache {
    dirs: HashMap<PathBuf, CachedDir>,
}

impl Cache {
    /// The listing for `path`, if it is cached and `modified` still matches.
    ///
    /// Passing the caller's `mtime` keeps the stat call — which it needs
    /// anyway — out of this module, and makes a stale entry indistinguishable
    /// from a missing one.
    pub fn get(&self, path: &Path, modified: SystemTime) -> Option<&[DirEntry]> {
        let cached = self.dirs.get(path)?;
        (cached.modified == modified).then_some(cached.entries.as_slice())
    }

    /// Caches a listing, dropping an entry to stay under the cap.
    pub fn insert(&mut self, path: PathBuf, modified: SystemTime, entries: Vec<DirEntry>) {
        if !self.dirs.contains_key(&path) && self.dirs.len() >= MAX_CACHED_DIRS {
            // HashMap order is unspecified, so this is not really LRU; the
            // cap exists to bound memory, not to be clever.
            let victim = self.dirs.keys().next().cloned();
            if let Some(victim) = victim {
                self.dirs.remove(&victim);
            }
        }

        self.dirs.insert(path, CachedDir { modified, entries });
    }

    pub fn len(&self) -> usize {
        self.dirs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.dirs.is_empty()
    }

    pub fn clear(&mut self) {
        self.dirs.clear();
    }

    /// Parses the `dir` / `entry` pairs written by [`Self::to_lines`].
    ///
    /// Entries follow the `dir` line they belong to, so the two keys are read
    /// as a stream rather than looked up independently.
    pub(crate) fn from_lines(lines: &[String]) -> Self {
        let mut dirs: HashMap<PathBuf, CachedDir> = HashMap::new();
        let mut current: Option<PathBuf> = None;

        for line in lines {
            let line = line.trim_end();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }

            if let Some(rest) = line.strip_prefix("dir ") {
                let Some((nanos, path)) = rest.split_once(' ') else {
                    current = None;
                    continue;
                };
                // An unparsable timestamp means an unparsable entry: without
                // it the listing could never be validated, so it is dropped.
                let Some(nanos) = nanos.parse().ok() else {
                    current = None;
                    continue;
                };
                let path = PathBuf::from(path);
                current = Some(path.clone());
                dirs.insert(
                    path,
                    CachedDir {
                        modified: nanos_to_time(Some(nanos)),
                        entries: Vec::new(),
                    },
                );
            } else if let Some(rest) = line.strip_prefix("entry ")
                && let Some(dir) = &current
            {
                let Some((is_dir, path)) = rest.split_once(' ') else {
                    continue;
                };
                let path = PathBuf::from(path);
                let Some(name) = path.file_name().map(|n| n.to_string_lossy().into_owned()) else {
                    continue;
                };

                if let Some(cached) = dirs.get_mut(dir) {
                    cached.entries.push(DirEntry {
                        is_dir: is_dir == "1",
                        is_hidden: crate::fs::is_hidden_name(&name),
                        name,
                        path,
                    });
                }
            }

            if dirs.len() >= MAX_CACHED_DIRS {
                break;
            }
        }

        Self { dirs }
    }

    /// Renders the cache, directories sorted so the file is reproducible.
    pub(crate) fn to_lines(&self) -> Vec<String> {
        let mut paths: Vec<&PathBuf> = self.dirs.keys().collect();
        paths.sort();

        let mut lines = Vec::with_capacity(self.dirs.len() * 4);
        lines.push(format!(
            "{} cache v{}",
            crate::storage::codec::MAGIC,
            crate::storage::codec::FORMAT_VERSION
        ));

        for path in paths {
            let cached = &self.dirs[path];
            lines.push(format!(
                "dir {} {}",
                time_to_nanos(cached.modified),
                path.display()
            ));

            for entry in &cached.entries {
                lines.push(format!(
                    "entry {} {}",
                    u8::from(entry.is_dir),
                    entry.path.display()
                ));
            }
        }

        lines
    }
}

/// Nanoseconds since the epoch, or `0` when the clock is before it.
fn time_to_nanos(time: SystemTime) -> u128 {
    time.duration_since(SystemTime::UNIX_EPOCH)
        .map(|dur| dur.as_nanos())
        .unwrap_or(0)
}

fn nanos_to_time(nanos: Option<u128>) -> SystemTime {
    match nanos {
        Some(nanos) => SystemTime::UNIX_EPOCH + std::time::Duration::from_nanos(nanos as u64),
        // Never matches a real `mtime`, so the entry simply misses.
        None => SystemTime::UNIX_EPOCH,
    }
}

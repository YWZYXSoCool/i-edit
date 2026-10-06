//! Where the editor keeps its files.
//!
//! Every path goes through [`root`]: `I_EDIT_HOME` when it is set, otherwise
//! the platform's per-user config directory. `None` means no directory could
//! be determined, which is not an error — storage then runs in memory and
//! simply never persists.

use std::path::PathBuf;

/// Overrides the platform lookup; also what tests set to get a private root.
pub const ENV_ROOT: &str = "I_EDIT_HOME";

/// Directory name under the platform config directory.
pub const APP_NAME: &str = "i-edit";

/// Root of every file storage writes.
///
/// `I_EDIT_HOME` first, so tests and a portable install can point anywhere.
/// Then `%APPDATA%` on Windows, `$XDG_CONFIG_HOME` (or `~/.config`) elsewhere.
pub fn root() -> Option<PathBuf> {
    env_path(ENV_ROOT).or_else(platform_root)
}

/// `<root>/cache`. Caches live beside the rest: they are one root to back up
/// and one root to delete.
pub fn cache_dir(root: &std::path::Path) -> PathBuf {
    root.join("cache")
}

fn platform_root() -> Option<PathBuf> {
    if cfg!(windows) {
        env_path("APPDATA").map(|base| base.join(APP_NAME))
    } else {
        env_path("XDG_CONFIG_HOME")
            .or_else(|| crate::fs::home_dir().map(|home| home.join(".config")))
            .map(|base| base.join(APP_NAME))
    }
}

fn env_path(key: &str) -> Option<PathBuf> {
    std::env::var_os(key)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::{APP_NAME, cache_dir, root};

    #[test]
    fn cache_dir_sits_under_the_root() {
        assert_eq!(
            cache_dir(std::path::Path::new("/r")),
            std::path::PathBuf::from("/r/cache")
        );
    }

    #[test]
    fn a_root_is_found_in_this_environment() {
        // Either the override or a platform directory; what matters is that a
        // personal IDE always has somewhere to put its state.
        assert!(
            root().is_some(),
            "no storage root: {APP_NAME} has nowhere to persist"
        );
    }
}

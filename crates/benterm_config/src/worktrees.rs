//! The `[worktrees]` section: where the worktrees of tasks are put.

use std::path::{Path, PathBuf};

use toml::Table;

use crate::read::Reader;

/// Where the worktrees of tasks are put.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorktreeSettings {
    /// The directory the worktrees go under, each at
    /// `<repository>/<branch>`. A relative path, or one that starts with
    /// `~/`, is relative to the home directory. A task keeps the place it
    /// was made at when this changes.
    pub directory: PathBuf,
}

impl WorktreeSettings {
    pub(crate) const KEYS: &'static [&'static str] = &["directory"];

    pub(crate) fn read(r: &mut Reader, table: Option<&Table>) -> Self {
        let d = Self::default();
        let path = "worktrees";
        r.unknown_keys(table, path, Self::KEYS);
        Self {
            directory: r.path(table, path, "directory", d.directory),
        }
    }

    /// The directory as an absolute path, for the home directory `home`.
    /// `None` when the directory is below a home directory that is not
    /// known.
    pub fn root(&self, home: Option<&Path>) -> Option<PathBuf> {
        if self.directory.is_absolute() {
            return Some(self.directory.clone());
        }
        let home = home.filter(|home| home.is_absolute())?;
        let below = self.directory.strip_prefix("~").unwrap_or(&self.directory);
        Some(home.join(below))
    }
}

impl Default for WorktreeSettings {
    fn default() -> Self {
        Self {
            // Not below `Library/Application Support`: a space in the
            // path of a checkout trips up scripts that do not quote it.
            directory: PathBuf::from(".local/share/benterm/worktrees"),
        }
    }
}

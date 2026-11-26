//! The `[pty_log]` section: recording what the PTY sends to the terminal
//! core, so that it can be replayed later.
//!
//! This crate only reads the values. The app hands them to `benterm_log`,
//! which refuses a directory outside the home directory.

use std::path::PathBuf;

use toml::Table;

use crate::read::Reader;

/// Whether to record PTY logs and where. Off by default.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PtyLogSettings {
    pub enabled: bool,
    /// The directory for the logs. A relative path is relative to the home
    /// directory, and the directory must be inside the home directory.
    pub dir: PathBuf,
}

impl PtyLogSettings {
    pub(crate) const KEYS: &'static [&'static str] = &["enabled", "dir"];

    pub(crate) fn read(r: &mut Reader, table: Option<&Table>) -> Self {
        let d = Self::default();
        let path = "pty_log";
        r.unknown_keys(table, path, Self::KEYS);
        Self {
            enabled: r.boolean(table, path, "enabled", d.enabled),
            dir: r.path(table, path, "dir", d.dir),
        }
    }
}

impl Default for PtyLogSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            // The same default as `benterm_log::RecordingConfig`.
            dir: PathBuf::from("Library/Application Support/benterm/pty-logs"),
        }
    }
}

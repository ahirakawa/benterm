//! The `[session]` section: what is kept of the session for the next
//! start, besides its workspaces, tabs and panes.

use toml::Table;

use crate::read::Reader;

/// How many rows of a pane can be kept: from none to as many as a pane
/// holds.
pub const SCROLLBACK_LINES_RANGE: (usize, usize) = (0, 10_000);

/// What is kept of each pane.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionSettings {
    /// How many rows of what a pane showed, the last ones of its
    /// scrollback and its screen together, are saved and shown again at
    /// the next start. With 0 nothing is saved and nothing shown again.
    ///
    /// What is saved is what was on the screen, also a password or a
    /// token that a program printed.
    pub scrollback_lines: usize,
}

impl SessionSettings {
    pub(crate) const KEYS: &'static [&'static str] = &["scrollback_lines"];

    pub(crate) fn read(r: &mut Reader, table: Option<&Table>) -> Self {
        let d = Self::default();
        let path = "session";
        r.unknown_keys(table, path, Self::KEYS);
        Self {
            scrollback_lines: r.count(
                table,
                path,
                "scrollback_lines",
                d.scrollback_lines,
                SCROLLBACK_LINES_RANGE,
            ),
        }
    }
}

impl Default for SessionSettings {
    fn default() -> Self {
        Self {
            scrollback_lines: 2000,
        }
    }
}

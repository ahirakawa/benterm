//! The `[ipc]` section: who may use the socket API.

use toml::Table;

use crate::read::Reader;

/// How the socket API checks the requests it takes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IpcSettings {
    /// Whether every request must carry a credential the app gave out.
    /// Panes of the window get one for the whole API, and agents started
    /// with `agent run` one for their own pane only, unless started with
    /// `--control`. A program outside Benterm, or a `benterm` older than
    /// the app, is refused. Off lets every request through as before, for
    /// a client that cannot yet send a credential. Read when the app
    /// starts and when the file is saved.
    pub restricted: bool,
}

impl IpcSettings {
    pub(crate) const KEYS: &'static [&'static str] = &["restricted"];

    pub(crate) fn read(r: &mut Reader, table: Option<&Table>) -> Self {
        let d = Self::default();
        let path = "ipc";
        r.unknown_keys(table, path, Self::KEYS);
        Self {
            restricted: r.boolean(table, path, "restricted", d.restricted),
        }
    }
}

impl Default for IpcSettings {
    fn default() -> Self {
        Self { restricted: true }
    }
}

//! The `[stall]` section: when an agent at work is marked as stalled.
//!
//! One time serves every agent for now. A time for each role can come in
//! a table of its own under this section (`[stall.roles]`), with this one
//! as the time of an agent whose role has none.

use std::time::Duration;

use toml::Table;

use crate::read::Reader;

/// The shortest and the longest time, in seconds.
pub const STALL_SECONDS_RANGE: (usize, usize) = (1, 86_400);

/// When an agent at work is marked as stalled.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StallSettings {
    /// How long an agent may stay at work with neither its screen nor
    /// the files of its directory changing before it is marked.
    pub after_seconds: usize,
}

impl StallSettings {
    pub(crate) const KEYS: &'static [&'static str] = &["after_seconds"];

    pub(crate) fn read(r: &mut Reader, table: Option<&Table>) -> Self {
        let d = Self::default();
        let path = "stall";
        r.unknown_keys(table, path, Self::KEYS);
        Self {
            after_seconds: r.count(
                table,
                path,
                "after_seconds",
                d.after_seconds,
                STALL_SECONDS_RANGE,
            ),
        }
    }

    /// [`after_seconds`](Self::after_seconds) as a duration.
    pub fn threshold(&self) -> Duration {
        Duration::from_secs(self.after_seconds as u64)
    }
}

impl Default for StallSettings {
    fn default() -> Self {
        Self { after_seconds: 600 }
    }
}

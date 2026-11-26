//! Deciding what a newly read file means for the config in use.

use crate::file::decode;
use crate::read::{Loaded, Warning, parse};
use std::io;

/// What a reload tells the application.
// Events come once per save, so the size of `Applied` does not matter.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, PartialEq)]
pub enum ConfigEvent {
    /// The file is valid TOML. Use this config and show these warnings.
    Applied(Loaded),
    /// The file could not be used as a whole, so the previous config stays.
    Rejected(Warning),
}

/// Holds the config in use and changes it only for a file that is valid
/// TOML. A file caught halfway through a save is usually not valid TOML, so
/// it leaves the previous config in place. A missing file also leaves it in
/// place, because some editors delete the file for a moment while saving.
#[derive(Clone, Debug)]
pub struct Reloader {
    current: Loaded,
    /// The last rejection reported since a file was applied.
    rejected: Option<Warning>,
}

impl Reloader {
    pub fn new(current: Loaded) -> Self {
        Self {
            current,
            rejected: None,
        }
    }

    /// The config in use and the warnings it was read with.
    pub fn current(&self) -> &Loaded {
        &self.current
    }

    /// Takes the result of reading the file again. Returns an event when the
    /// application has something new to show: a different config or set of
    /// warnings, or a rejection not already reported.
    pub fn update(&mut self, read: io::Result<Vec<u8>>) -> Option<ConfigEvent> {
        let parsed = match decode(read) {
            Ok(None) => return None,
            Ok(Some(text)) => parse(&text),
            Err(w) => Err(w),
        };
        match parsed {
            Ok(loaded) => {
                self.rejected = None;
                if loaded == self.current {
                    return None;
                }
                self.current = loaded.clone();
                Some(ConfigEvent::Applied(loaded))
            }
            Err(w) => {
                if self.rejected.as_ref() == Some(&w) {
                    return None;
                }
                self.rejected = Some(w.clone());
                Some(ConfigEvent::Rejected(w))
            }
        }
    }
}

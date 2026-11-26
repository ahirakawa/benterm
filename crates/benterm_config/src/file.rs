//! Where the file lives and how it is read from disk.

use crate::read::{Loaded, Problem, Warning, load};
use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};

/// The config file path for the current environment. See [`config_path_from`].
pub fn config_path() -> Option<PathBuf> {
    config_path_from(
        std::env::var_os("XDG_CONFIG_HOME").as_deref(),
        std::env::var_os("HOME").as_deref(),
    )
}

/// `$XDG_CONFIG_HOME/benterm/config.toml`, or
/// `$HOME/.config/benterm/config.toml` when `XDG_CONFIG_HOME` is unset,
/// empty or relative. `None` when neither gives an absolute directory.
pub fn config_path_from(xdg_config_home: Option<&OsStr>, home: Option<&OsStr>) -> Option<PathBuf> {
    fn absolute(d: Option<&OsStr>) -> Option<&Path> {
        d.map(Path::new).filter(|p| p.is_absolute())
    }
    let dir = match absolute(xdg_config_home) {
        Some(xdg) => xdg.to_path_buf(),
        None => absolute(home)?.join(".config"),
    };
    Some(dir.join("benterm").join("config.toml"))
}

/// Reads the file at `path`. A missing file gives the defaults without
/// warnings; a file that cannot be read, or is not UTF-8 TOML, gives the
/// defaults and one warning.
pub fn load_file(path: &Path) -> Loaded {
    let text = match decode(std::fs::read(path)) {
        Ok(Some(text)) => text,
        Ok(None) => return Loaded::default(),
        Err(w) => {
            return Loaded {
                warnings: vec![w],
                ..Loaded::default()
            };
        }
    };
    load(&text)
}

/// The text of a file read from disk: `None` if the file does not exist,
/// and a [`Problem::Unreadable`] warning if it cannot be read as UTF-8.
pub(crate) fn decode(read: io::Result<Vec<u8>>) -> Result<Option<String>, Warning> {
    let unreadable = |message: String| Warning {
        key: String::new(),
        problem: Problem::Unreadable(message),
    };
    match read {
        Ok(bytes) => String::from_utf8(bytes)
            .map(Some)
            .map_err(|e| unreadable(e.to_string())),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(unreadable(e.to_string())),
    }
}

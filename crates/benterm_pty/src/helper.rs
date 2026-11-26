//! The helper program each child runs first (see `helper/main.rs`).
//!
//! It is built with this crate and embedded in it, and written to a file
//! of the user's own when a session starts and the file is not there.

use std::ffi::CString;
use std::fs;
use std::io::{self, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

static HELPER: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/benterm-pty-helper"));

/// The path of the helper, in [`private_dir`](crate::private_dir).
///
/// The file is checked each time: a helper that is missing, or that is
/// not the one built into this app, is written again before it is run.
pub fn path() -> io::Result<CString> {
    // One at a time, so that two sessions starting together do not both
    // write the file.
    static INSTALLING: Mutex<()> = Mutex::new(());
    let _installing = INSTALLING.lock().unwrap_or_else(|e| e.into_inner());
    let path = install(&crate::private_dir::private_dir()?)?;
    CString::new(path.as_os_str().as_bytes())
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))
}

/// Writes the helper into `dir` unless it is already there, and returns
/// its path. The name carries a hash of the contents, so apps built with
/// different helpers do not share a file.
fn install(dir: &Path) -> io::Result<PathBuf> {
    let path = dir.join(format!("benterm-pty-helper-{:016x}", fnv1a(HELPER)));
    if is_installed(&path) {
        return Ok(path);
    }
    // Written under a name of this process and renamed into place, so no
    // process ever runs a partly written helper.
    let partial = dir.join(format!(
        "benterm-pty-helper-{:016x}.{}",
        fnv1a(HELPER),
        std::process::id()
    ));
    // One left by a process that had this id before.
    let _ = fs::remove_file(&partial);
    let written = (|| {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o700)
            .open(&partial)?;
        file.write_all(HELPER)?;
        drop(file);
        fs::rename(&partial, &path)
    })();
    if written.is_err() {
        let _ = fs::remove_file(&partial);
    }
    written.map(|()| path)
}

/// Whether `path` is the helper itself: a file, not a link to one, that
/// holds what was built into this app and that the user may run.
fn is_installed(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o100 != 0)
        && fs::read(path).is_ok_and(|bytes| bytes == HELPER)
}

/// 64-bit FNV-1a.
fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, &byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

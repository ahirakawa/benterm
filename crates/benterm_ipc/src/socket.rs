//! Where the socket lives and how a pane's shell finds it.

use std::ffi::OsStr;
use std::io::{self, ErrorKind};
use std::os::unix::fs::FileTypeExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};

/// The environment variable that tells a pane's shell where the socket of
/// the app it runs in is.
pub const SOCKET_ENV: &str = "BENTERM_SOCKET";

/// The environment variable that tells a pane's shell which pane it is: the
/// pane id as a decimal number.
pub const PANE_ID_ENV: &str = "BENTERM_PANE_ID";

/// The environment variable that tells a spawned agent its own name.
pub const AGENT_NAME_ENV: &str = "BENTERM_AGENT_NAME";

/// The environment variable that tells a spawned agent the pane id of the
/// pane that started it, when it has a parent.
pub const PARENT_PANE_ID_ENV: &str = "BENTERM_PARENT_PANE_ID";

/// The environment variable that tells a spawned agent where its file-based
/// mailbox directory lives, used as a sandbox fallback when the socket is
/// unreachable.
pub const MAILBOX_DIR_ENV: &str = "BENTERM_MAILBOX_DIR";

/// The environment variable that carries a pane's opaque IPC credential for
/// own-pane hooks and reports.
pub const PANE_TOKEN_ENV: &str = "BENTERM_PANE_TOKEN";

/// The environment variable that carries an opaque IPC credential with full
/// control permissions.
pub const CONTROL_TOKEN_ENV: &str = "BENTERM_CONTROL_TOKEN";

/// The directory the app and its commands keep their state in:
/// `$XDG_STATE_HOME/benterm`, or `~/.local/state/benterm` when
/// `XDG_STATE_HOME` is not set to an absolute path. `None` when neither
/// directory is known.
pub fn state_dir(xdg_state_home: Option<&OsStr>, home: Option<&OsStr>) -> Option<PathBuf> {
    let state = xdg_state_home
        .map(Path::new)
        .filter(|path| path.is_absolute())
        .map(Path::to_path_buf)
        .or_else(|| {
            home.filter(|home| !home.is_empty())
                .map(|home| Path::new(home).join(".local/state"))
        })?;
    Some(state.join("benterm"))
}

/// The socket path for the app with process id `pid`.
///
/// It is `$XDG_STATE_HOME/benterm/<pid>.sock`, or
/// `~/.local/state/benterm/<pid>.sock` when `XDG_STATE_HOME` is not set to
/// an absolute path. The process id keeps two running apps, such as a
/// development build inside the installed one, on separate sockets.
/// Returns `None` when neither directory is known.
pub fn socket_path(
    xdg_state_home: Option<&OsStr>,
    home: Option<&OsStr>,
    pid: u32,
) -> Option<PathBuf> {
    Some(state_dir(xdg_state_home, home)?.join(format!("{pid}.sock")))
}

/// The socket path for this process, from the environment.
pub fn default_socket_path() -> Option<PathBuf> {
    socket_path(
        std::env::var_os("XDG_STATE_HOME").as_deref(),
        std::env::var_os("HOME").as_deref(),
        std::process::id(),
    )
}

/// Removes the sockets that apps which ended without cleaning up left in
/// `directory`. Returns the paths it removed.
///
/// A socket is removed only when its name is `<pid>.sock`, nobody listens
/// on it, and no process with that id is running. The last check keeps the
/// socket of an app that is starting and not listening yet, or that is too
/// busy to take a connection. Files that are not sockets, and other names,
/// are left alone. A missing directory has nothing to clean up.
pub fn remove_dead_sockets(directory: &Path) -> io::Result<Vec<PathBuf>> {
    let entries = match std::fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut removed = Vec::new();
    for entry in entries {
        let entry = entry?;
        let Some(pid) = socket_pid(&entry.file_name()) else {
            continue;
        };
        let path = entry.path();
        // An entry that cannot be checked or removed is skipped: the
        // others can still be cleaned up.
        if is_dead_socket(&path, pid) && std::fs::remove_file(&path).is_ok() {
            removed.push(path);
        }
    }
    Ok(removed)
}

/// The process id in a socket name of the form `<pid>.sock`.
fn socket_pid(name: &OsStr) -> Option<u32> {
    let digits = name.to_str()?.strip_suffix(".sock")?;
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

fn is_dead_socket(path: &Path, pid: u32) -> bool {
    // Not following a symbolic link keeps the check on the entry itself.
    let is_socket =
        std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_socket());
    is_socket
        && matches!(UnixStream::connect(path), Err(error) if error.kind() == ErrorKind::ConnectionRefused)
        && !process_exists(pid)
}

fn process_exists(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    if pid <= 0 {
        // 0 and below name process groups, not one process.
        return true;
    }
    // SAFETY: signal 0 only checks whether the process exists; nothing is
    // sent.
    if unsafe { libc::kill(pid, 0) } == 0 {
        return true;
    }
    // EPERM means it exists and belongs to someone else.
    io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
}

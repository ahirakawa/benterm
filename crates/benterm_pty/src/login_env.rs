//! The environment of the user's shell, for an app opened from the
//! Finder.
//!
//! An app the Finder or the Dock opens gets the short environment of
//! launchd, without what the user's shell files add to `PATH`. So the app
//! runs the user's shell once, as a login and interactive shell, which
//! reads the same files a terminal's shell does, `.zshrc` included, and
//! takes the environment it prints.

use std::ffi::OsString;
use std::fmt;
use std::io::{self, Read};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// How long the shell may take before the app goes on with its own
/// environment.
pub const CAPTURE_TIMEOUT: Duration = Duration::from_secs(5);

/// Names whose values belong to the shell that printed them, not to the
/// user's settings.
const DROPPED: &[&str] = &["PWD", "OLDPWD", "SHLVL", "_"];

/// Why the shell's environment could not be taken.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaptureError {
    /// The shell did not print its environment within this time.
    TimedOut(Duration),
    /// The shell could not be started, or ended without printing it.
    Failed(String),
}

impl fmt::Display for CaptureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TimedOut(after) => write!(
                f,
                "the login shell did not print its environment within {} seconds",
                after.as_secs_f32()
            ),
            Self::Failed(why) => write!(f, "{why}"),
        }
    }
}

/// Whether the app was started from a terminal, whose shell has already
/// given the app its environment. Every shell sets `SHLVL`, and launchd
/// does not.
pub fn started_from_terminal(vars: impl IntoIterator<Item = (OsString, OsString)>) -> bool {
    vars.into_iter().any(|(name, _)| name == "SHLVL")
}

/// Runs `shell` as a login and interactive shell and returns the
/// environment it prints, without the names of [`DROPPED`]. Gives up
/// after `timeout`, and stops the shell and what it started.
///
/// What the shell files print, before or after, is passed over: the
/// environment is what `env -0` prints between two markers. It is taken
/// as soon as the second marker is read, without waiting for the output
/// to close, which a program the shell files leave running may hold open.
pub fn capture(shell: &Path, timeout: Duration) -> Result<Vec<(OsString, OsString)>, CaptureError> {
    let marker = marker();
    let command = format!("printf '%s' {marker}; /usr/bin/env -0; printf '%s' {marker}");
    let mut child = Command::new(shell);
    child
        .args(shell_args(shell))
        .arg(command)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if let Some(home) = std::env::var_os("HOME") {
        child.current_dir(home);
    }
    // SAFETY: setsid is async-signal-safe. A session of its own has no
    // terminal for an interactive shell to take, and puts the shell and
    // what it starts in one process group to stop together.
    unsafe {
        child.pre_exec(|| match libc::setsid() {
            -1 => Err(io::Error::last_os_error()),
            _ => Ok(()),
        });
    }
    let mut child = child.spawn().map_err(|error| {
        CaptureError::Failed(format!(
            "cannot start the login shell {}: {error}",
            shell.display()
        ))
    })?;
    let pid = child.id() as libc::pid_t;
    let mut stdout = child.stdout.take().expect("stdout is piped");
    let shown = shell.display().to_string();

    let (sender, result) = mpsc::channel();
    // The thread answers before it reaps the shell, so the process group
    // is still there to stop for as long as no answer has come.
    let reader = thread::Builder::new()
        .name("benterm-login-env".into())
        .spawn(move || {
            let mut output = Vec::new();
            let mut chunk = [0; 8192];
            let answer = loop {
                match stdout.read(&mut chunk) {
                    Ok(0) | Err(_) => break None,
                    Ok(n) => output.extend_from_slice(&chunk[..n]),
                }
                if let Some(env) = parse(&output, marker.as_bytes()) {
                    break Some(env);
                }
            };
            drop(stdout);
            let answer = answer.ok_or_else(|| {
                CaptureError::Failed(format!(
                    "the login shell {shown} ended without printing its environment"
                ))
            });
            let _ = sender.send(answer);
            let _ = child.wait();
        });
    if let Err(error) = reader {
        // SAFETY: the child has not been reaped: nothing else waits for it.
        unsafe { libc::killpg(pid, libc::SIGKILL) };
        return Err(CaptureError::Failed(format!(
            "cannot read the login shell: {error}"
        )));
    }
    match result.recv_timeout(timeout) {
        Ok(answer) => answer,
        Err(_) => {
            // SAFETY: no answer has come, so the shell is not reaped and
            // its pid is still the id of its process group.
            unsafe { libc::killpg(pid, libc::SIGKILL) };
            Err(CaptureError::TimedOut(timeout))
        }
    }
}

/// A word the shell files are not going to print.
fn marker() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_nanos());
    format!("benterm-env-{}-{nanos}", std::process::id())
}

/// The arguments before the command, for the shell at `shell`. csh and
/// tcsh take `-l` only alone, so they read their `.cshrc` as an
/// interactive shell that is not a login shell.
fn shell_args(shell: &Path) -> &'static [&'static str] {
    let name = shell.file_name().map(OsStrExt::as_bytes);
    match name {
        Some(b"csh" | b"tcsh") => &["-i", "-c"],
        _ => &["-l", "-i", "-c"],
    }
}

/// The variables `env -0` printed between the two `marker`s of `output`.
/// `None` until both markers are there.
fn parse(output: &[u8], marker: &[u8]) -> Option<Vec<(OsString, OsString)>> {
    let find = |bytes: &[u8]| bytes.windows(marker.len()).position(|w| w == marker);
    let start = find(output)? + marker.len();
    let body = &output[start..];
    let body = &body[..find(body)?];
    let env = body
        .split(|&byte| byte == 0)
        .filter_map(|entry| {
            let equals = entry.iter().position(|&byte| byte == b'=')?;
            let (name, value) = (&entry[..equals], &entry[equals + 1..]);
            let dropped = DROPPED.iter().any(|d| d.as_bytes() == name);
            (!name.is_empty() && !dropped).then(|| {
                (
                    OsString::from(std::ffi::OsStr::from_bytes(name)),
                    OsString::from(std::ffi::OsStr::from_bytes(value)),
                )
            })
        })
        .collect();
    Some(env)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pairs(env: &[(OsString, OsString)]) -> Vec<(&str, &str)> {
        env.iter()
            .map(|(name, value)| (name.to_str().unwrap(), value.to_str().unwrap()))
            .collect()
    }

    #[test]
    fn takes_only_the_variables_between_the_markers() {
        let output = b"hello\n\x1b[1m% \x1b[0mMARKA=1\0B=two\0MARK% bye\n";
        let env = parse(output, b"MARK").unwrap();
        assert_eq!(pairs(&env), [("A", "1"), ("B", "two")]);
    }

    #[test]
    fn a_value_with_line_breaks_or_equal_signs_is_one_variable() {
        let output = b"MARKA=x=y\0B=line 1\nC=not a name\n\0D=\0MARK";
        let env = parse(output, b"MARK").unwrap();
        assert_eq!(
            pairs(&env),
            [("A", "x=y"), ("B", "line 1\nC=not a name\n"), ("D", "")]
        );
    }

    #[test]
    fn the_shells_own_names_are_dropped() {
        let output = b"MARKPWD=/x\0OLDPWD=/y\0SHLVL=1\0_=/usr/bin/env\0PATH=/bin\0MARK";
        let env = parse(output, b"MARK").unwrap();
        assert_eq!(pairs(&env), [("PATH", "/bin")]);
    }

    #[test]
    fn nothing_is_taken_until_the_second_marker() {
        assert_eq!(parse(b"hello", b"MARK"), None);
        assert_eq!(parse(b"hello MARKA=1\0", b"MARK"), None);
    }

    #[test]
    fn a_terminal_is_told_by_shlvl() {
        let vars = |names: &[&str]| -> Vec<(OsString, OsString)> {
            names.iter().map(|n| (n.into(), "1".into())).collect()
        };
        assert!(started_from_terminal(vars(&["PATH", "SHLVL"])));
        assert!(!started_from_terminal(vars(&["PATH", "HOME"])));
    }

    #[test]
    fn csh_cannot_be_a_login_shell_with_a_command() {
        assert_eq!(shell_args(Path::new("/bin/zsh")), ["-l", "-i", "-c"]);
        assert_eq!(
            shell_args(Path::new("/opt/homebrew/bin/fish")),
            ["-l", "-i", "-c"]
        );
        assert_eq!(shell_args(Path::new("/bin/bash")), ["-l", "-i", "-c"]);
        assert_eq!(shell_args(Path::new("/bin/tcsh")), ["-i", "-c"]);
        assert_eq!(shell_args(Path::new("/bin/csh")), ["-i", "-c"]);
    }
}

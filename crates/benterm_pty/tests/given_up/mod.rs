//! A made-up shell that never prints its environment, and the check
//! that `capture` gave up on it at the timeout.

use std::fs::File;
use std::os::fd::{FromRawFd, OwnedFd};
use std::path::PathBuf;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use benterm_pty::login_env::{CaptureError, capture};

/// A made-up shell, standing in for `$SHELL`, that runs
/// `before_starting`, prints something else, runs `before_waiting`, and
/// then waits for a job it started, which runs `sleep` for a minute.
/// The shell or the job leaves `ended` behind if it ends by itself. What
/// it prints to its error output is kept in `errors`.
pub struct NeverEnding {
    dir: tempfile::TempDir,
}

impl NeverEnding {
    pub fn new(before_starting: &str, before_waiting: &str) -> Self {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let at = dir.path().display();
        let body = format!(
            "#!/bin/sh\n\
             exec 2>'{at}/errors'\n\
             {before_starting}\n\
             echo hello\n\
             (sleep 60; touch '{at}/ended') &\n\
             {before_waiting}\n\
             wait\n\
             touch '{at}/ended'\n"
        );
        let shell = dir.path().join("zsh");
        std::fs::write(&shell, body).unwrap();
        std::fs::set_permissions(&shell, std::fs::Permissions::from_mode(0o755)).unwrap();
        Self { dir }
    }

    pub fn path(&self) -> PathBuf {
        self.dir.path().join("zsh")
    }
}

/// One `capture` at a time, so a shell does not inherit the pipe of
/// another test's shell.
static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

/// Takes the environment of `shell`, and checks that `capture` gave up
/// when `timeout` had passed and did not wait for the shell to end: it
/// stopped the shell and what the shell started.
///
/// The shell and all it starts inherit the writing end of a pipe, so the
/// pipe reads as ended once none of them is left. That holds however far
/// the shell got before it was stopped: a loaded machine may not run it
/// at all before the timeout. The shell and its job would end by
/// themselves only after a minute, so a `capture` that waited for them,
/// or stopped the shell but not the job, would leave `ended` behind.
///
/// How long after the timeout `capture` came back is not checked. That
/// is how soon the system ran the test again, which a loaded machine can
/// put off for seconds.
///
/// Every shell started while the pipe is open inherits it, so a test
/// binary that uses this must start no shell other than through this.
pub fn assert_given_up(shell: &NeverEnding, timeout: Duration) {
    let _alone = ONE_AT_A_TIME.lock().unwrap_or_else(PoisonError::into_inner);
    let (mut reading, writing) = inherited_pipe();

    let started = Instant::now();
    let result = capture(&shell.path(), timeout);
    let took = started.elapsed();
    drop(writing);

    assert_eq!(result, Err(CaptureError::TimedOut(timeout)));
    assert!(took >= timeout, "{took:?}");
    // Waits until no process holds the writing end.
    std::io::copy(&mut reading, &mut std::io::sink()).unwrap();
    let errors = std::fs::read_to_string(shell.dir.path().join("errors")).unwrap_or_default();
    assert!(
        !shell.dir.path().join("ended").exists(),
        "the shell or its job ended by itself, so it was not stopped: {errors}"
    );
}

/// A pipe whose writing end the processes started from now on inherit,
/// and whose reading end they do not.
fn inherited_pipe() -> (File, OwnedFd) {
    let mut ends = [0; 2];
    // SAFETY: `ends` has room for the two descriptors pipe writes.
    assert_eq!(unsafe { libc::pipe(ends.as_mut_ptr()) }, 0);
    // SAFETY: pipe opened both, and nothing else owns them.
    let (reading, writing) =
        unsafe { (OwnedFd::from_raw_fd(ends[0]), OwnedFd::from_raw_fd(ends[1])) };
    // SAFETY: the reading end is open.
    assert_eq!(
        unsafe { libc::fcntl(ends[0], libc::F_SETFD, libc::FD_CLOEXEC) },
        0
    );
    (File::from(reading), writing)
}

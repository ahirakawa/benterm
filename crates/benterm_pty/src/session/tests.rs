//! Failures while [`PtySession::spawn`] starts its threads.
//!
//! Each test runs again in a process of its own, where no other test
//! opens descriptors, so the number of open descriptors tells what the
//! failure left behind.

use std::cell::Cell;
use std::process::Command;

use super::*;

thread_local! {
    /// The name of the thread whose start fails on this thread.
    static FAIL: Cell<Option<&'static str>> = const { Cell::new(None) };
    /// The pid of the child the last `spawn` on this thread started.
    static PID: Cell<Option<libc::pid_t>> = const { Cell::new(None) };
}

/// Whether starting the thread `name` fails.
pub(super) fn fails(name: &str) -> bool {
    FAIL.get() == Some(name)
}

/// Notes the pid of the child `spawn` has started.
pub(super) fn spawned(pid: libc::pid_t) {
    PID.set(Some(pid));
}

/// Set in the process a test runs again in.
const ALONE: &str = "BENTERM_PTY_TEST_ALONE";

/// Whether this is the process of its own the test `name` runs in.
/// If not, runs the test in one and checks that it passed there.
fn alone(name: &str) -> bool {
    if env::var_os(ALONE).is_some() {
        return true;
    }
    let status = Command::new(env::current_exe().unwrap())
        .args([name, "--exact", "--test-threads=1", "--nocapture"])
        .env(ALONE, "1")
        .status()
        .unwrap();
    assert!(status.success(), "{name} failed in a process of its own");
    false
}

fn open_descriptors() -> usize {
    // Reading the directory opens one descriptor, the same each time.
    std::fs::read_dir("/dev/fd").unwrap().count()
}

/// Starts a child with the start of `thread` failing; returns the
/// child's pid and whether `spawn` failed.
fn spawn_failing(thread: &'static str) -> (libc::pid_t, bool) {
    FAIL.set(Some(thread));
    let result = PtySession::spawn(PtyConfig::command("/bin/sleep", ["30"]));
    FAIL.set(None);
    let pid = PID.take().expect("the child was started");
    (pid, result.is_err())
}

fn exists(pid: libc::pid_t) -> bool {
    let ret = unsafe { libc::kill(pid, 0) };
    ret == 0 || io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
}

/// Starts and ends one session, so that what the first start sets up
/// for good, such as the helper file, is not counted.
fn warm_up() {
    let (session, _events) =
        PtySession::spawn(PtyConfig::command("/usr/bin/true", [""; 0])).expect("spawn a session");
    session.shutdown();
}

#[test]
fn a_failed_start_of_the_waiter_kills_and_reaps_the_child() {
    if !alone("session::tests::a_failed_start_of_the_waiter_kills_and_reaps_the_child") {
        return;
    }
    warm_up();
    let before = open_descriptors();
    let (pid, failed) = spawn_failing(WAITER_THREAD);
    assert!(failed, "spawn returns the error");
    assert!(!exists(pid), "the child is reaped");
    assert_eq!(open_descriptors(), before, "no descriptor is left open");
}

#[test]
fn a_failed_start_of_the_reader_kills_the_child_and_the_waiter_reaps_it() {
    if !alone(
        "session::tests::a_failed_start_of_the_reader_kills_the_child_and_the_waiter_reaps_it",
    ) {
        return;
    }
    warm_up();
    let before = open_descriptors();
    let (pid, failed) = spawn_failing(READER_THREAD);
    assert!(failed, "spawn returns the error");
    assert!(!exists(pid), "the child is reaped");
    assert_eq!(open_descriptors(), before, "no descriptor is left open");
}

#[test]
fn the_waiter_writing_after_the_reader_is_gone_raises_no_sigpipe() {
    if !alone("session::tests::the_waiter_writing_after_the_reader_is_gone_raises_no_sigpipe") {
        return;
    }
    // Rust programs ignore SIGPIPE; a program that does not would die.
    unsafe { libc::signal(libc::SIGPIPE, libc::SIG_DFL) };
    let (wake_read, wake_write) = sys::pipe().unwrap();
    drop(wake_read);
    let error = (&wake_write).write_all(&[1]).unwrap_err();
    assert_eq!(error.raw_os_error(), Some(libc::EPIPE));
}

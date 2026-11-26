//! Starting a session while another thread of the app is setting up a
//! system library must not kill the child.
//!
//! In the window tests, children were sometimes killed with SIGKILL before
//! they ran their program. The crash reports showed the cause: after
//! fork(2), libSystem's fork handler in the child set up libnotify, whose
//! one-time initialization another thread of the parent was in the middle
//! of. The child found the half-done state and crashed. That
//! initialization happens once per process, so each try runs in a fresh
//! copy of this test binary.

mod common;

use std::env;
use std::os::unix::process::ExitStatusExt;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::Duration;

use benterm_pty::{PtyConfig, PtySession};
use common::collect_to_exit;

const TEST: &str = "a_child_started_while_libnotify_starts_up_runs_its_program";
/// Set in the copies of this binary that do one try.
const ONE_TRY: &str = "BENTERM_PTY_FORK_RACE_TRY";
const TRIES: usize = 50;
const SPAWNERS: usize = 8;

unsafe extern "C" {
    /// libnotify, part of libSystem. The first call sets up its globals.
    fn notify_post(name: *const libc::c_char) -> u32;
}

/// One try: several threads start sessions one after another while
/// another thread makes the process's first libnotify call.
fn one_try() {
    let stop = Arc::new(AtomicBool::new(false));
    let start = Arc::new(Barrier::new(SPAWNERS + 1));
    let spawners: Vec<_> = (0..SPAWNERS)
        .map(|_| {
            let (start, stop) = (Arc::clone(&start), Arc::clone(&stop));
            thread::spawn(move || {
                start.wait();
                let mut sessions = Vec::new();
                while !stop.load(Ordering::SeqCst) {
                    sessions.push(
                        PtySession::spawn(PtyConfig::command("/usr/bin/true", [""; 0])).unwrap(),
                    );
                }
                sessions
                    .into_iter()
                    .map(|(_session, events)| collect_to_exit(&events).exit.unwrap())
                    .collect::<Vec<_>>()
            })
        })
        .collect();
    start.wait();
    thread::sleep(Duration::from_millis(2));
    unsafe { notify_post(c"org.benterm.test.fork-race".as_ptr()) };
    thread::sleep(Duration::from_millis(2));
    stop.store(true, Ordering::SeqCst);
    for spawner in spawners {
        for status in spawner.join().unwrap() {
            assert!(
                status.success(),
                "the child did not run its program: {status:?} (signal {:?})",
                status.signal()
            );
        }
    }
}

#[test]
fn a_child_started_while_libnotify_starts_up_runs_its_program() {
    if env::var_os(ONE_TRY).is_some() {
        one_try();
        return;
    }
    let me = env::current_exe().unwrap();
    for i in 0..TRIES {
        let output = Command::new(&me)
            .args([TEST, "--exact", "--nocapture"])
            .env(ONE_TRY, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "try {i} failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

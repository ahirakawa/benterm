//! Many sessions started and shut down side by side. Each
//! child must end only by the SIGHUP of its own session's shutdown.

mod common;

use std::os::unix::process::ExitStatusExt;
use std::thread;

use benterm_pty::{PtyConfig, PtySession};
use common::{Collected, collect_to_exit, collect_until, within};

const THREADS: usize = 8;
const ROUNDS: usize = 25;

/// Starts a child that prints `ready` and then waits, waits for `ready`,
/// and shuts the session down. Returns how the child ended.
fn one_round() -> Option<i32> {
    let (session, events) = PtySession::spawn(PtyConfig::command(
        "/bin/sh",
        ["-c", "printf ready; sleep 100"],
    ))
    .unwrap();
    let mut collected = Collected::default();
    collect_until(&events, &mut collected, |c| c.text().contains("ready"));
    assert!(
        collected.exit.is_none(),
        "the child ended before its shutdown: {:?}",
        collected.exit
    );
    within("shutdown", move || session.shutdown());
    collect_to_exit(&events).exit.unwrap().signal()
}

#[test]
fn parallel_sessions_end_only_by_their_own_shutdown() {
    let workers: Vec<_> = (0..THREADS)
        .map(|_| thread::spawn(|| (0..ROUNDS).map(|_| one_round()).collect::<Vec<_>>()))
        .collect();
    for worker in workers {
        for signal in worker.join().unwrap() {
            assert_eq!(signal, Some(libc::SIGHUP));
        }
    }
}

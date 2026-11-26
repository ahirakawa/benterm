mod common;

use std::os::unix::process::ExitStatusExt;

use benterm_pty::{PtyConfig, PtySession};
use common::{collect_to_exit, within};

fn spawn_sh(script: &str) -> (PtySession, std::sync::mpsc::Receiver<benterm_pty::PtyEvent>) {
    PtySession::spawn(PtyConfig::command("/bin/sh", ["-c", script])).unwrap()
}

#[test]
fn shutdown_hangs_up_a_running_child() {
    let (session, events) = spawn_sh("sleep 100");
    within("shutdown", move || session.shutdown());
    let collected = collect_to_exit(&events);
    assert_eq!(collected.exit.unwrap().signal(), Some(libc::SIGHUP));
}

#[test]
fn drop_shuts_down_a_running_child() {
    let (session, events) = spawn_sh("sleep 100");
    within("drop", move || drop(session));
    let collected = collect_to_exit(&events);
    assert_eq!(collected.exit.unwrap().signal(), Some(libc::SIGHUP));
}

#[test]
fn shutdown_kills_a_child_that_ignores_hangup() {
    // `ready` tells us the trap is in place before we hang up.
    let (session, events) = spawn_sh("trap '' HUP; echo ready; sleep 100");
    let mut collected = common::Collected::default();
    common::collect_until(&events, &mut collected, |c| c.text().contains("ready"));
    within("shutdown", move || session.shutdown());
    let collected = collect_to_exit(&events);
    assert_eq!(collected.exit.unwrap().signal(), Some(libc::SIGKILL));
}

#[test]
fn shutdown_after_exit_returns_the_status() {
    let (session, events) = spawn_sh("exit 3");
    let collected = collect_to_exit(&events);
    assert_eq!(collected.exit.unwrap().code(), Some(3));
    within("shutdown", move || session.shutdown());
}

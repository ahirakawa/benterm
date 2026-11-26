//! A child that is stopped, as by SIGSTOP or Ctrl+Z, has not exited: the
//! session goes on answering, and reaps the child only when it ends.

mod common;

use std::process::Command;
use std::time::{Duration, Instant};

use benterm_pty::{PtyConfig, PtySession};
use common::{Collected, TIMEOUT, collect_to_exit, collect_until, within};

fn signal(pid: u32, signal: libc::c_int) {
    assert_eq!(unsafe { libc::kill(pid as libc::pid_t, signal) }, 0);
}

/// Waits until `ps` tells that `pid` is stopped.
fn wait_stopped(pid: u32) {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let out = Command::new("/bin/ps")
            .args(["-o", "stat=", "-p", &pid.to_string()])
            .output()
            .unwrap();
        if String::from_utf8_lossy(&out.stdout)
            .trim_start()
            .starts_with('T')
        {
            return;
        }
        assert!(Instant::now() < deadline, "{pid} did not stop");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Kills the child if the test fails, so that it does not stay stopped
/// after the test binary has ended.
struct KillOnPanic(u32);

impl Drop for KillOnPanic {
    fn drop(&mut self) {
        if std::thread::panicking() {
            unsafe { libc::kill(self.0 as libc::pid_t, libc::SIGKILL) };
        }
    }
}

#[test]
fn a_stopped_child_does_not_stop_the_session() {
    let (session, events) = PtySession::spawn(PtyConfig::command("/bin/sleep", ["30"])).unwrap();
    let pid = session.pid();
    let _guard = KillOnPanic(pid);
    signal(pid, libc::SIGSTOP);
    wait_stopped(pid);

    // Asked again and again for a while: a waiter that took the stop for
    // an exit would hold the session's lock from the first microseconds.
    let session = within("foreground_process while stopped", move || {
        let until = Instant::now() + Duration::from_millis(500);
        while Instant::now() < until {
            let front = session.foreground_process().unwrap();
            assert_eq!(front.pid, pid);
            assert!(session.working_directory().is_some(), "taken as reaped");
        }
        session
    });

    signal(pid, libc::SIGCONT);
    signal(pid, libc::SIGTERM);
    let collected = within("the exit event", move || collect_to_exit(&events));
    use std::os::unix::process::ExitStatusExt;
    assert_eq!(collected.exit.unwrap().signal(), Some(libc::SIGTERM));
    assert_eq!(
        unsafe { libc::kill(pid as libc::pid_t, 0) },
        -1,
        "the child was not reaped"
    );
    drop(session);
}

/// Waits until the program in front of `session` was started as `name`.
fn wait_front(session: &PtySession, name: &str) {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let front = session.foreground_process();
        if front.is_some_and(|f| f.args.first().is_some_and(|a| a == name)) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "{name} did not come to the front"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn ctrl_z_stops_a_job_of_the_shell_and_fg_brings_it_back() {
    for (shell, args) in [
        ("/bin/bash", &["--norc", "--noprofile", "-i"][..]),
        ("/bin/zsh", &["-f", "-i"][..]),
    ] {
        let (session, events) =
            PtySession::spawn(PtyConfig::command(shell, args.iter().copied())).unwrap();
        let _guard = KillOnPanic(session.pid());
        let mut collected = Collected::default();
        let mut wait_output = |mark: &str| {
            collect_until(&events, &mut collected, |c| c.text().contains(mark));
        };
        let type_line = |line: &str| session.write(format!("{line}\n").as_bytes()).unwrap();

        type_line("sleep 100");
        wait_front(&session, "sleep");
        session.write(b"\x1a").unwrap();
        type_line("echo stopped-$((40 + 2))");
        wait_output("stopped-42");
        wait_front(&session, shell);

        type_line("fg");
        wait_front(&session, "sleep");
        session.write(b"\x03").unwrap();
        type_line("echo back-$((40 + 3))");
        wait_output("back-43");
        wait_front(&session, shell);

        type_line("exit");
        let collected = within("the exit event", move || collect_to_exit(&events));
        assert_eq!(collected.exit.unwrap().code(), Some(0), "{shell}");
    }
}

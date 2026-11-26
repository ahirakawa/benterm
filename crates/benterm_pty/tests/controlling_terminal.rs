//! The child gets the PTY as its controlling terminal and nothing else
//! from the app.

mod common;

use std::os::fd::{FromRawFd, OwnedFd};
use std::os::unix::process::ExitStatusExt;
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use benterm_pty::{PtyConfig, PtySession};
use common::{Collected, TIMEOUT, collect_to_exit, collect_until};

fn sh(script: &str) -> PtyConfig {
    PtyConfig::command("/bin/sh", ["-c", script])
}

#[test]
fn the_terminal_is_the_controlling_terminal_of_the_child() {
    // /dev/tty opens only for a process with a controlling terminal.
    let (_session, events) = PtySession::spawn(sh("exec 3</dev/tty && echo has-tty")).unwrap();
    let collected = collect_to_exit(&events);
    assert!(
        collected.text().contains("has-tty"),
        "{:?}",
        collected.text()
    );
}

#[test]
fn ctrl_c_interrupts_the_child() {
    let (session, events) = PtySession::spawn(sh("echo ready; exec sleep 100")).unwrap();
    let mut collected = Collected::default();
    collect_until(&events, &mut collected, |c| c.text().contains("ready"));
    session.write(b"\x03").unwrap();
    let collected = collect_to_exit(&events);
    assert_eq!(collected.exit.unwrap().signal(), Some(libc::SIGINT));
}

#[test]
fn the_child_inherits_only_the_terminal() {
    // A pipe without close-on-exec, as other code in the app may make.
    let mut fds = [-1; 2];
    assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
    // Moved up to 100 or more: the shell saves a descriptor it redirects
    // on the lowest free one from 10, and `2>/dev/null` below would find
    // that copy open if the pipe were on 10.
    let fds = fds.map(|fd| unsafe {
        let high = libc::fcntl(fd, libc::F_DUPFD, 100);
        assert!(high >= 100);
        libc::close(fd);
        high
    });
    let _pipe = fds.map(|fd| unsafe { OwnedFd::from_raw_fd(fd) });
    // `: <&N` fails unless descriptor N is open in the shell.
    let script = format!(
        "for fd in {} {}; do if (: <&$fd) 2>/dev/null; then echo inherited $fd; fi; done; echo checked",
        fds[0], fds[1]
    );
    let (_session, events) = PtySession::spawn(sh(&script)).unwrap();
    let collected = collect_to_exit(&events);
    assert_eq!(collected.text().trim(), "checked");
}

/// Programs that do not open their terminal again themselves. Only bash
/// did, so the tests above, which run `/bin/sh`, passed without the
/// child getting the terminal from `spawn`.
fn direct_programs() -> [PtyConfig; 3] {
    [
        PtyConfig::command("/bin/zsh", ["-c", "sleep 100"]),
        PtyConfig::command("/bin/dash", ["-c", "sleep 100"]),
        PtyConfig::command("/bin/sleep", ["100"]),
    ]
}

/// Waits until `pid`'s terminal has `pid`'s group in the foreground,
/// that is, until the terminal is the controlling terminal of the
/// session `pid` leads. Returns whether that happened within `TIMEOUT`.
fn leads_its_terminal(pid: u32) -> bool {
    let deadline = Instant::now() + TIMEOUT;
    while Instant::now() < deadline {
        let ps = Command::new("ps")
            .args(["-o", "tpgid=", "-p", &pid.to_string()])
            .output()
            .unwrap();
        if String::from_utf8_lossy(&ps.stdout).trim() == pid.to_string() {
            return true;
        }
        thread::sleep(Duration::from_millis(20));
    }
    false
}

#[test]
fn a_program_started_directly_gets_the_terminal_as_its_controlling_terminal() {
    for config in direct_programs() {
        let program = config.program.clone();
        let (session, _events) = PtySession::spawn(config).unwrap();
        assert!(
            leads_its_terminal(session.pid()),
            "{program:?} has no controlling terminal"
        );
    }
}

#[test]
fn ctrl_c_stops_a_program_started_directly() {
    for config in direct_programs() {
        let program = config.program.clone();
        let (session, events) = PtySession::spawn(config).unwrap();
        // Typed before the child has the terminal, ^C would reach no one.
        leads_its_terminal(session.pid());
        session.write(b"\x03").unwrap();
        let exit = collect_to_exit(&events).exit.unwrap();
        assert_eq!(exit.signal(), Some(libc::SIGINT), "{program:?}: {exit:?}");
    }
}

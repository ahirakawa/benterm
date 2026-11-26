//! The shell does not inherit the variables of the terminal Benterm was
//! started from.
//!
//! This file holds a single test because it changes the environment of
//! the test process, which other tests running at the same time would see.

mod common;

use benterm_pty::{PtyConfig, PtySession};
use common::collect_to_exit;

#[test]
fn a_shell_started_from_herdr_sees_benterm_and_not_herdr() {
    let outer = [
        ("HC_TERMINAL_ID", "outer-terminal"),
        ("HC_TERMINAL_SOCKET", "/tmp/outer.sock"),
        ("HERDR_ENV", "1"),
        ("GHOSTTY_RESOURCES_DIR", "/tmp/ghostty"),
        ("TERM_PROGRAM", "ghostty"),
        ("TERM", "xterm-ghostty"),
        ("BENTERM_PANE_ID", "7"),
    ];
    for (name, value) in outer {
        // SAFETY: this is the only test in this binary, and no other
        // thread reads or writes the environment while it runs.
        unsafe { std::env::set_var(name, value) };
    }

    let (_session, events) =
        PtySession::spawn(PtyConfig::command("/usr/bin/env", [""; 0])).unwrap();
    let collected = collect_to_exit(&events);
    let text = collected.text();
    let lines: Vec<&str> = text
        .lines()
        .map(|line| line.trim_end_matches('\r'))
        .collect();

    for name in [
        "HC_TERMINAL_ID",
        "HC_TERMINAL_SOCKET",
        "HERDR_ENV",
        "GHOSTTY_RESOURCES_DIR",
    ] {
        let prefix = format!("{name}=");
        assert!(
            !lines.iter().any(|line| line.starts_with(&prefix)),
            "{name} reached the child: {lines:?}"
        );
    }
    for expected in [
        "TERM=xterm-256color",
        "COLORTERM=truecolor",
        "TERM_PROGRAM=Benterm",
        "BENTERM_PANE_ID=7",
    ] {
        assert!(
            lines.contains(&expected),
            "{expected} is missing: {lines:?}"
        );
    }
    assert!(
        lines
            .iter()
            .any(|line| line.starts_with("TERM_PROGRAM_VERSION=")),
        "TERM_PROGRAM_VERSION is missing: {lines:?}"
    );
    assert!(collected.exit.unwrap().success());
}

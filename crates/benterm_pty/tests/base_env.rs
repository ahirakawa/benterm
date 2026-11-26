//! A child made from the environment a login shell printed gets that one,
//! not the app's, and still without the names of other terminals and
//! agents.

mod common;

use std::sync::Arc;

use benterm_pty::{PtyConfig, PtySession};
use common::collect_to_exit;

#[test]
fn a_child_gets_the_given_environment_without_other_terminals_and_agents() {
    let base = [
        ("PATH", "/tmp/benterm-test-bin:/usr/bin:/bin"),
        ("ONLY_IN_THE_SHELL", "yes"),
        ("HC_TERMINAL_ID", "outer-terminal"),
        ("CLAUDECODE", "1"),
    ];
    let config = PtyConfig {
        base_env: Some(Arc::new(
            base.iter()
                .map(|(name, value)| (name.into(), value.into()))
                .collect(),
        )),
        ..PtyConfig::command("/usr/bin/env", [""; 0])
    };
    // cargo sets this for the test process, so the app has it.
    assert!(std::env::var_os("CARGO_MANIFEST_DIR").is_some());

    let (_session, events) = PtySession::spawn(config).unwrap();
    let text = collect_to_exit(&events).text();
    let lines: Vec<&str> = text.lines().map(|l| l.trim_end_matches('\r')).collect();

    for expected in [
        "PATH=/tmp/benterm-test-bin:/usr/bin:/bin",
        "ONLY_IN_THE_SHELL=yes",
        "TERM_PROGRAM=Benterm",
    ] {
        assert!(
            lines.contains(&expected),
            "{expected} is missing: {lines:?}"
        );
    }
    for name in ["HC_TERMINAL_ID", "CLAUDECODE", "CARGO_MANIFEST_DIR"] {
        let prefix = format!("{name}=");
        assert!(
            !lines.iter().any(|line| line.starts_with(&prefix)),
            "{name} reached the child: {lines:?}"
        );
    }
}

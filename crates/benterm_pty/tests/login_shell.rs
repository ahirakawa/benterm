//! Runs the user's real login shell.

mod common;

use benterm_pty::{PtyConfig, PtySession};
use common::{Collected, collect_to_exit, collect_until};

#[test]
fn login_shell_runs_echo_and_exits() {
    let (session, events) = PtySession::spawn(PtyConfig::login_shell()).unwrap();
    let mut collected = Collected::default();

    // The terminal echoes what we type, so look for text that only the
    // shell's expansion produces.
    session
        .write(b"echo \"benterm-$((40 + 2)) argv0=$0\"\n")
        .unwrap();
    collect_until(&events, &mut collected, |c| c.text().contains("benterm-42"));
    let text = collected.text();
    assert!(text.contains("benterm-42"), "{text:?}");
    assert!(text.contains("argv0=-"), "not a login shell: {text:?}");

    session.write(b"exit\n").unwrap();
    let rest = collect_to_exit(&events);
    assert!(rest.exit.unwrap().success());
}

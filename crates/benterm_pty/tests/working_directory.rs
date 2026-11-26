//! The session knows the directory its shell is in.

mod common;

use std::path::Path;
use std::sync::mpsc::Receiver;

use benterm_pty::{PtyConfig, PtyEvent, PtySession};
use common::{Collected, TestDir, collect_to_exit, collect_until};

/// The shells of the tests, started without the user's startup files:
/// (name, program, arguments).
const SHELLS: &[(&str, &str, &[&str])] = &[
    ("zsh", "/bin/zsh", &["-f", "-i"]),
    ("bash", "/bin/bash", &["--noprofile", "--norc", "-i"]),
];

fn spawn_in(program: &str, args: &[&str], cwd: &Path) -> (PtySession, Receiver<PtyEvent>) {
    let config = PtyConfig {
        cwd: Some(cwd.to_owned()),
        ..PtyConfig::command(program, args.iter().copied())
    };
    PtySession::spawn(config).unwrap()
}

/// Types `command` and waits until the shell has run it. The mark is
/// written by the shell's arithmetic, so the echo of the typed line does
/// not match it. The shell writes the mark before its next prompt.
fn run(
    session: &PtySession,
    events: &Receiver<PtyEvent>,
    collected: &mut Collected,
    command: &str,
) {
    let seen = collected.text().matches("ran-42").count();
    session
        .write(format!("{command}; echo ran-$((40 + 2))\n").as_bytes())
        .unwrap();
    collect_until(events, collected, |c| {
        c.text().matches("ran-42").count() > seen
    });
    assert!(collected.exit.is_none(), "{:?}", collected.text());
}

#[test]
fn a_shell_starts_in_the_directory_it_was_given() {
    for (name, program, args) in SHELLS {
        let dir = TestDir::new(&format!("starts-{name}"));
        let (session, events) = spawn_in(program, args, &dir.0);
        let mut collected = Collected::default();
        run(&session, &events, &mut collected, "true");
        assert_eq!(session.working_directory(), Some(dir.0.clone()), "{name}");
    }
}

#[test]
fn cd_changes_the_working_directory_before_the_next_prompt() {
    for (name, program, args) in SHELLS {
        let dir = TestDir::new(&format!("cd-{name}"));
        let first = dir.sub("first");
        // A name with a space and with characters outside ASCII.
        let second = dir.sub("second 二つ目");
        let (session, events) = spawn_in(program, args, &dir.0);
        let mut collected = Collected::default();

        run(&session, &events, &mut collected, "cd first");
        assert_eq!(session.working_directory(), Some(first), "{name}");

        run(&session, &events, &mut collected, "cd '../second 二つ目'");
        assert_eq!(session.working_directory(), Some(second), "{name}");

        run(&session, &events, &mut collected, "cd ..");
        assert_eq!(session.working_directory(), Some(dir.0.clone()), "{name}");
    }
}

#[test]
fn a_symbolic_link_is_reported_as_the_directory_it_names() {
    for (name, program, args) in SHELLS {
        let dir = TestDir::new(&format!("link-{name}"));
        let real = dir.sub("real");
        std::os::unix::fs::symlink(&real, dir.0.join("link")).unwrap();
        let (session, events) = spawn_in(program, args, &dir.0);
        let mut collected = Collected::default();

        run(&session, &events, &mut collected, "cd link");
        assert_eq!(session.working_directory(), Some(real), "{name}");
    }
}

#[test]
fn a_session_whose_shell_has_exited_has_no_working_directory() {
    let dir = TestDir::new("exited");
    let (session, events) = spawn_in("/bin/sh", &["-c", "true"], &dir.0);
    let collected = collect_to_exit(&events);
    assert!(collected.exit.unwrap().success());
    assert_eq!(session.working_directory(), None);
}

/// The user's own login shell, with its startup files.
#[test]
fn cd_in_the_login_shell_changes_the_working_directory() {
    let dir = TestDir::new("login");
    let sub = dir.sub("sub");
    let config = PtyConfig {
        cwd: Some(dir.0.clone()),
        env: vec![("BENTERM_TEST_DIR".into(), sub.clone().into())],
        ..PtyConfig::login_shell()
    };
    let (session, events) = PtySession::spawn(config).unwrap();
    let mut collected = Collected::default();

    // Startup files may change the directory, so go there by name.
    run(
        &session,
        &events,
        &mut collected,
        "cd \"$BENTERM_TEST_DIR\"",
    );
    assert_eq!(session.working_directory(), Some(sub));

    run(&session, &events, &mut collected, "cd ..");
    assert_eq!(session.working_directory(), Some(dir.0.clone()));
}

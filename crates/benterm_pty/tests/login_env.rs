//! Taking the environment of the user's shell: the shells here are
//! made-up scripts that stand in for `$SHELL`.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use benterm_pty::login_env::{CAPTURE_TIMEOUT, CaptureError, capture};

/// A made-up shell running `body`. It is called as `shell -l -i -c
/// <command>`, so the command is `$4`.
fn shell(dir: &Path, body: &str) -> PathBuf {
    let path = dir.join("zsh");
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

fn value<'a>(env: &'a [(std::ffi::OsString, std::ffi::OsString)], name: &str) -> Option<&'a str> {
    env.iter()
        .find(|(n, _)| n == name)
        .map(|(_, value)| value.to_str().unwrap())
}

#[test]
fn the_environment_the_shell_files_set_is_taken_past_what_they_print() {
    let dir = tempfile::tempdir().unwrap();
    let zsh = shell(
        dir.path(),
        r#"echo hello; printf '%% '
PATH="/tmp/benterm-test-bin:$PATH"; export PATH
MULTI='line 1
line 2=x'; export MULTI
eval "$4"
echo bye"#,
    );

    let env = capture(&zsh, CAPTURE_TIMEOUT).unwrap();

    let path = value(&env, "PATH").unwrap();
    assert!(path.starts_with("/tmp/benterm-test-bin:"), "{path}");
    assert_eq!(value(&env, "MULTI"), Some("line 1\nline 2=x"));
    for dropped in ["PWD", "OLDPWD", "SHLVL", "_"] {
        assert_eq!(value(&env, dropped), None, "{dropped}");
    }
}

#[test]
fn the_environment_is_taken_without_waiting_for_what_the_shell_left_running() {
    let dir = tempfile::tempdir().unwrap();
    // An agent the shell files start keeps the output open after the
    // shell has ended.
    let zsh = shell(dir.path(), r#"sleep 100 & eval "$4""#);

    let started = Instant::now();
    let env = capture(&zsh, CAPTURE_TIMEOUT).unwrap();

    assert!(started.elapsed() < Duration::from_secs(2));
    assert!(value(&env, "PATH").is_some());
}

#[test]
fn a_shell_that_ends_without_the_environment_fails() {
    let dir = tempfile::tempdir().unwrap();
    let zsh = shell(dir.path(), "echo hello; exit 3");

    let result = capture(&zsh, CAPTURE_TIMEOUT);

    assert!(matches!(result, Err(CaptureError::Failed(_))), "{result:?}");
}

#[test]
fn a_shell_that_is_not_there_fails() {
    let result = capture(Path::new("/nowhere/zsh"), CAPTURE_TIMEOUT);

    assert!(matches!(result, Err(CaptureError::Failed(_))), "{result:?}");
}

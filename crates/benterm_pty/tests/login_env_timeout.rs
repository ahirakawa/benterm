//! Taking the environment of a made-up shell that never prints it, so
//! that `capture` gives up on it at the timeout. Apart from the other
//! tests of `capture`, because the shells here inherit a pipe that the
//! shells of those tests would hold open too.

use std::time::Duration;

use benterm_pty::login_env::CAPTURE_TIMEOUT;
use given_up::{NeverEnding, assert_given_up};

mod given_up;

#[test]
fn a_shell_that_never_ends_is_given_up_after_the_timeout() {
    let zsh = NeverEnding::new("", "");

    assert_eq!(CAPTURE_TIMEOUT, Duration::from_secs(5));
    assert_given_up(&zsh, CAPTURE_TIMEOUT);
}

#[test]
fn a_shell_stopped_before_it_runs_anything_is_given_up_after_the_timeout() {
    // The shell stops itself at once, as a loaded machine may not run it
    // before the timeout, so it is stopped before it has printed anything
    // or started its job. It lets itself go on a minute later, so a
    // `capture` that does not stop it fails instead of hanging.
    let zsh = NeverEnding::new("(sleep 60; kill -CONT $$) & kill -STOP $$", "");

    assert_given_up(&zsh, Duration::from_secs(1));
}

//! Taking the environment of the user's shell when the app is not run
//! at the moment the timeout passes, as on a loaded machine. Alone in
//! its own test binary, because it stops the whole process.

use std::time::Duration;

use given_up::{NeverEnding, assert_given_up};

mod given_up;

#[test]
fn a_shell_given_up_on_late_is_still_not_waited_for() {
    let timeout = Duration::from_secs(1);
    // The shell stops the test process at once and lets it go on 4
    // seconds later, so the timeout passes while the process is stopped.
    // The test process cannot stop the shell before it is let go on.
    let zsh = NeverEnding::new(
        "",
        "app=$PPID; kill -STOP $app; (sleep 4; kill -CONT $app) &",
    );

    assert_given_up(&zsh, timeout);
}

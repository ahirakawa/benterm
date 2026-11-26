//! A terminal tells which program is in front of it.

use std::ffi::OsString;
use std::time::{Duration, Instant};

use benterm_core::GridSize;
use benterm_terminal::{ForegroundProcess, PtyConfig, Terminal, TerminalConfig};

const TIMEOUT: Duration = Duration::from_secs(10);

fn start(program: &str, args: &[&str]) -> Terminal {
    let config = TerminalConfig {
        pty: PtyConfig::command(program, args.iter().copied()),
        size: GridSize::new(24, 80),
        scrollback_limit: 100,
        colors: None,
        log: None,
    };
    Terminal::spawn(config, || {}).unwrap()
}

fn wait_for(what: &str, done: impl Fn() -> bool) {
    let deadline = Instant::now() + TIMEOUT;
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn the_started_program_is_in_front_while_it_runs() {
    let terminal = start("/bin/sleep", &["100"]);
    // The program has its terminal and its arguments once it has started.
    let want = vec![OsString::from("/bin/sleep"), OsString::from("100")];
    wait_for("the program to start", || {
        let front: Option<ForegroundProcess> = terminal.foreground_process();
        front.is_some_and(|front| front.args == want)
    });
}

#[test]
fn nothing_is_in_front_once_the_program_has_exited() {
    let terminal = start("/bin/sh", &["-c", "exit 0"]);
    wait_for("the exit", || terminal.foreground_process().is_none());
}

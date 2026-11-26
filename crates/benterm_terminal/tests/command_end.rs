//! A program on the PTY tells the end of a command with OSC 133 ; D, as
//! a shell does after each command line, and the terminal keeps the
//! exit code.

use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use benterm_core::GridSize;
use benterm_terminal::{CommandEnd, PtyConfig, Screen, Terminal, TerminalConfig};

const TIMEOUT: Duration = Duration::from_secs(10);

struct Harness {
    terminal: Terminal,
    wakes: Receiver<()>,
    screen: Screen,
}

fn sh(script: &str) -> Harness {
    let (wake_tx, wakes) = mpsc::channel();
    let config = TerminalConfig {
        pty: PtyConfig::command("/bin/sh", ["-c", script]),
        size: GridSize::new(6, 40),
        scrollback_limit: 100,
        colors: None,
        log: None,
    };
    let waker = move || {
        let _ = wake_tx.send(());
    };
    Harness {
        terminal: Terminal::spawn(config, waker).unwrap(),
        wakes,
        screen: Screen::default(),
    }
}

impl Harness {
    /// Waits for the program to print `marker`, which it prints after
    /// everything else.
    fn wait_for(&mut self, marker: &str) {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            self.terminal.sync(&mut self.screen);
            if self
                .screen
                .rows
                .iter()
                .any(|row| row.text().contains(marker))
            {
                return;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            assert!(self.wakes.recv_timeout(left).is_ok(), "no {marker:?}");
        }
    }
}

fn end(exit_code: Option<i32>) -> CommandEnd {
    CommandEnd { exit_code }
}

#[test]
fn the_exit_code_of_the_last_command_can_be_read() {
    let mut h = sh(r"printf '\033]133;D;1\007done'; sleep 5");
    h.wait_for("done");
    assert_eq!(h.terminal.last_command_end(), Some(end(Some(1))));
}

#[test]
fn an_end_without_an_exit_code_has_an_unknown_one() {
    let mut h = sh(r"printf '\033]133;D\007done'; sleep 5");
    h.wait_for("done");
    assert_eq!(h.terminal.last_command_end(), Some(end(None)));
}

#[test]
fn before_any_end_there_is_no_last_command() {
    let mut h = sh(r"printf 'done'; sleep 5");
    h.wait_for("done");
    assert_eq!(h.terminal.last_command_end(), None);
    assert!(h.terminal.take_command_ends().is_empty());
}

#[test]
fn every_end_is_taken_once_in_order_and_the_last_one_stays() {
    let mut h = sh(r"printf '\033]133;D;1\007\033]133;C\007\033]133;D;0\007done'; sleep 5");
    h.wait_for("done");
    assert_eq!(h.terminal.take_command_ends(), [end(Some(1)), end(Some(0))]);
    assert!(h.terminal.take_command_ends().is_empty());
    assert_eq!(h.terminal.last_command_end(), Some(end(Some(0))));
}

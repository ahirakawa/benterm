//! A program on the PTY tells what it is doing with OSC 7501, and the
//! terminal keeps it until the program ends or a prompt starts.

use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use benterm_core::{BlockKind, GridSize, ProgramState};
use benterm_terminal::{PtyConfig, Screen, Terminal, TerminalConfig};

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
    fn wait_until(&mut self, what: &str, done: impl Fn(&Screen) -> bool) {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            self.terminal.sync(&mut self.screen);
            if done(&self.screen) {
                return;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            assert!(self.wakes.recv_timeout(left).is_ok(), "no {what}");
        }
    }

    /// Waits for the program to print `marker`, which it prints after
    /// everything else.
    fn wait_for(&mut self, marker: &str) {
        self.wait_until(marker, |screen| {
            screen.rows.iter().any(|row| row.text().contains(marker))
        });
    }

    fn wait_for_exit(&mut self) {
        self.wait_until("exit", |screen| screen.exit.is_some());
    }

    /// The state the program itself is in, as it told it.
    fn state(&self) -> Option<ProgramState> {
        self.terminal
            .program_status()
            .map(|record| record.status.state)
    }
}

/// A script that prints the reports with the bodies `bodies`, then `then`,
/// then "ok", and stays.
fn reports(bodies: &[&str], then: &str) -> String {
    let mut script = String::new();
    for body in bodies {
        script.push_str(&format!(r"printf '\033]7501;{body}\033\\'; "));
    }
    format!("{script}{then} printf ok; sleep 30")
}

#[test]
fn the_terminal_keeps_what_the_program_told_last() {
    let mut h = sh(&reports(
        &[
            "state=working:app=terraform",
            "state=blocked:kind=question:app=terraform",
        ],
        "",
    ));
    assert_eq!(h.state(), None);
    h.wait_for("ok");
    let record = h.terminal.program_status().unwrap();
    assert_eq!(record.status.state, ProgramState::Blocked);
    assert_eq!(record.status.kind, Some(BlockKind::Question));
    assert_eq!(record.status.app.as_deref(), Some("terraform"));
}

#[test]
fn every_report_of_the_program_itself_gets_a_higher_serial() {
    let mut h = sh(&format!(
        "{} read line; {}",
        reports(&["state=working"], "").replace("sleep 30", ""),
        reports(&["state=working:id=part", "state=working"], "printf k;")
    ));
    h.wait_for("ok");
    let first = h.terminal.program_status().unwrap();
    h.terminal.write(b"\n".to_vec()).unwrap();
    h.wait_for("kok");
    let second = h.terminal.program_status().unwrap();
    assert_eq!(first.status, second.status);
    assert!(second.serial > first.serial);
}

#[test]
fn a_part_of_the_program_does_not_change_what_the_program_itself_told() {
    let mut h = sh(&reports(
        &["state=idle", "state=blocked:kind=permission:id=worker"],
        "",
    ));
    h.wait_for("ok");
    assert_eq!(h.state(), Some(ProgramState::Idle));
}

#[test]
fn the_start_of_a_prompt_drops_working_and_blocked_and_keeps_the_rest() {
    for (state, kept) in [
        ("working", false),
        ("blocked", false),
        ("idle", true),
        ("done", true),
        ("error", true),
    ] {
        let body = format!("state={state}");
        let mut h = sh(&reports(&[&body], r"printf '\033]133;A\007';"));
        h.wait_for("ok");
        assert_eq!(h.state().is_some(), kept, "{state}");
    }
}

#[test]
fn the_end_of_a_command_drops_working_and_blocked_too() {
    let mut h = sh(&reports(&["state=working"], r"printf '\033]133;D;0\007';"));
    h.wait_for("ok");
    assert_eq!(h.state(), None);
    let mut h = sh(&reports(&["state=done"], r"printf '\033]133;D;0\007';"));
    h.wait_for("ok");
    assert_eq!(h.state(), Some(ProgramState::Done));
}

#[test]
fn a_report_after_the_prompt_started_is_kept() {
    let mut h = sh(&format!(
        r"printf '\033]7501;state=working\007\033]133;A\007'; {}",
        reports(&["state=working:progress=1"], "")
    ));
    h.wait_for("ok");
    assert_eq!(
        h.terminal.program_status().unwrap().status.progress,
        Some(1)
    );
}

#[test]
fn the_end_of_the_program_drops_working_and_blocked_and_keeps_done() {
    let mut h = sh(r"printf '\033]7501;state=working\033\\'");
    h.wait_for_exit();
    assert_eq!(h.state(), None);
    let mut h = sh(r"printf '\033]7501;state=done\033\\'");
    h.wait_for_exit();
    assert_eq!(h.state(), Some(ProgramState::Done));
}

#[test]
fn clear_removes_what_the_program_told() {
    let mut h = sh(&reports(&["state=done", "state=clear"], ""));
    h.wait_for("ok");
    assert_eq!(h.state(), None);
}

#[test]
fn the_program_gets_the_answer_to_its_query() {
    // The first eight bytes of the answer, as numbers.
    let mut h = sh(
        r"stty raw -echo; printf '\033]7501;?\007'; dd bs=1 count=8 2>/dev/null | od -An -tu1 | tr -d ' \n'; printf ok; sleep 30",
    );
    h.wait_for("ok");
    // ESC ] 7 5 0 1 ; ?
    h.wait_for("2793555348495963ok");
}

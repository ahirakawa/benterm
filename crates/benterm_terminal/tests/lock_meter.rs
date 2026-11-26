//! A lock meter counts the locks the UI side takes of a terminal's
//! state, and how long it waited for them. The worker's locks are not
//! counted.

use std::time::Duration;

use benterm_core::GridSize;
use benterm_terminal::{LockCount, LockMeter, PtyConfig, Screen, Terminal, TerminalConfig};

fn terminal() -> Terminal {
    let config = TerminalConfig {
        pty: PtyConfig::command("/bin/sh", ["-c", "sleep 10"]),
        size: GridSize::new(6, 40),
        scrollback_limit: 100,
        colors: None,
        log: None,
    };
    Terminal::spawn(config, || {}).unwrap()
}

#[test]
fn a_new_meter_has_counted_nothing() {
    assert_eq!(
        LockMeter::new().count(),
        LockCount {
            locks: 0,
            waited: Duration::ZERO
        }
    );
}

#[test]
fn every_call_that_reads_the_state_is_one_lock() {
    let mut terminal = terminal();
    let meter = LockMeter::new();
    terminal.meter_locks(meter.clone());
    let mut screen = Screen::default();
    // What the view does when the terminal wakes it.
    terminal.sync(&mut screen);
    terminal.take_command_ends();
    terminal.program_status();
    terminal.take_notifications();
    assert_eq!(meter.count().locks, 4);
}

#[test]
fn a_lock_nobody_holds_is_waited_for_briefly() {
    let mut terminal = terminal();
    let meter = LockMeter::new();
    terminal.meter_locks(meter.clone());
    terminal.changes();
    // Far above what an uncontended lock takes, and far below a hang.
    assert!(meter.count().waited < Duration::from_secs(1));
}

#[test]
fn counts_are_subtracted_to_count_a_stretch() {
    let before = LockCount {
        locks: 3,
        waited: Duration::from_micros(5),
    };
    let after = LockCount {
        locks: 7,
        waited: Duration::from_micros(12),
    };
    assert_eq!(
        after - before,
        LockCount {
            locks: 4,
            waited: Duration::from_micros(7)
        }
    );
}

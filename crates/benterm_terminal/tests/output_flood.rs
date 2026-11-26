//! Output that takes the core long to read must not keep the UI
//! waiting for the terminal's state for as long as the reading takes.

use std::sync::mpsc;
use std::time::{Duration, Instant};

use benterm_core::GridSize;
use benterm_terminal::{PtyConfig, Screen, Terminal, TerminalConfig};

const TIMEOUT: Duration = Duration::from_secs(120);

#[test]
fn the_ui_gets_the_lock_while_a_flood_of_repeats_is_read() {
    // Each of the 2,000 repeats writes a screen of characters, which is
    // the most a repeat is let to write.
    let script =
        r#"awk 'BEGIN { for (i = 0; i < 2000; i++) printf "a\033[65535b"; printf "\r\ndone" }'"#;
    let config = TerminalConfig {
        pty: PtyConfig::command("/bin/sh", ["-c", script]),
        size: GridSize::new(50, 200),
        scrollback_limit: 1000,
        colors: None,
        log: None,
    };
    let (wake_tx, wakes) = mpsc::channel();
    let terminal = Terminal::spawn(config, move || {
        let _ = wake_tx.send(());
    })
    .unwrap();

    // Like a UI that also draws for reasons of its own: it asks for the
    // state every few milliseconds, whether the terminal woke it or not.
    let mut screen = Screen::default();
    let mut longest = Duration::ZERO;
    let started = Instant::now();
    loop {
        let asked = Instant::now();
        terminal.sync(&mut screen);
        longest = longest.max(asked.elapsed());
        if screen.rows.iter().any(|row| row.text().starts_with("done")) {
            break;
        }
        assert!(started.elapsed() < TIMEOUT, "the output was not read");
        let _ = wakes.recv_timeout(Duration::from_millis(2));
    }
    let total = started.elapsed();

    // Reading it all takes far longer than one hold of the lock may.
    // The bound is loose, for a slow machine and an unoptimized build.
    assert!(
        longest < Duration::from_millis(500).min(total / 4),
        "the UI waited {longest:?} of the {total:?} it took to read the output"
    );
}

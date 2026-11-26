//! A terminal running real programs on a PTY.

use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use benterm_core::{Damage, GridSize, ReportedColors, Rgb};
use benterm_terminal::{
    Notification, PtyConfig, Screen, SyncOutcome, Terminal, TerminalConfig, WindowSize,
};

const TIMEOUT: Duration = Duration::from_secs(10);

struct Harness {
    terminal: Terminal,
    wakes: Receiver<()>,
    screen: Screen,
}

fn sh(script: &str) -> Harness {
    start(PtyConfig::command("/bin/sh", ["-c", script]))
}

fn start(pty: PtyConfig) -> Harness {
    let (wake_tx, wakes) = mpsc::channel();
    let config = TerminalConfig {
        pty,
        size: GridSize::new(6, 40),
        scrollback_limit: 50_000,
        colors: None,
        log: None,
    };
    let waker = move || {
        let _ = wake_tx.send(());
    };
    let terminal = Terminal::spawn(config, waker).unwrap();
    Harness {
        terminal,
        wakes,
        screen: Screen::default(),
    }
}

impl Harness {
    /// Redraws after every wake, like the UI, until `done` holds.
    fn wait_until(&mut self, what: &str, done: impl Fn(&Screen) -> bool) {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            self.terminal.sync(&mut self.screen);
            if done(&self.screen) {
                return;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if self.wakes.recv_timeout(left).is_err() {
                panic!("{what}: not before the timeout; screen: {:?}", self.text());
            }
        }
    }

    fn text(&self) -> String {
        let rows: Vec<String> = self.screen.rows.iter().map(|row| row.text()).collect();
        rows.join("\n")
    }
}

fn shows(text: &'static str) -> impl Fn(&Screen) -> bool {
    move |screen| screen.rows.iter().any(|row| row.text().contains(text))
}

/// Runs `f` on another thread and panics if it does not return in time.
fn within<T: Send + 'static>(what: &str, f: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || drop(tx.send(f())));
    rx.recv_timeout(TIMEOUT)
        .unwrap_or_else(|_| panic!("{what} did not return within {TIMEOUT:?}"))
}

#[test]
fn output_reaches_the_screen() {
    let mut harness = sh("printf 'hello\\r\\nworld'; sleep 100");
    harness.wait_until("output", shows("world"));
    assert_eq!(harness.screen.size, GridSize::new(6, 40));
    assert_eq!(harness.screen.rows.len(), 6);
    assert_eq!(harness.screen.rows[0].text().trim_end(), "hello");
    assert_eq!(harness.screen.rows[1].text().trim_end(), "world");
    assert_eq!(
        (harness.screen.cursor.row, harness.screen.cursor.col),
        (1, 5)
    );
}

/// The UI caches what it painted per row, so `sync` tells it which rows
/// it replaced.
#[test]
fn sync_reports_what_it_copied() {
    let mut harness = sh("sleep 100");
    assert_eq!(
        harness.terminal.sync(&mut harness.screen),
        Some(Damage::Full)
    );
    assert_eq!(harness.terminal.sync(&mut harness.screen), None);
}

#[test]
fn sync_reports_single_rows_for_small_changes() {
    let mut harness = sh("printf one; sleep 1; printf '\\033[3;1Hthree'; sleep 100");
    harness.wait_until("first output", shows("one"));
    let deadline = Instant::now() + TIMEOUT;
    let mut damaged = Vec::new();
    while !shows("three")(&harness.screen) {
        assert!(Instant::now() < deadline);
        let _ = harness.wakes.recv_timeout(TIMEOUT);
        match harness.terminal.sync(&mut harness.screen) {
            Some(Damage::Rows(rows)) => damaged.extend(rows),
            Some(Damage::Full) => panic!("a small change must not replace every row"),
            None => {}
        }
    }
    assert!(damaged.contains(&2));
    assert!(!damaged.contains(&1));
}

#[test]
fn the_exit_status_is_reported_after_the_output() {
    let mut harness = sh("printf bye; exit 3");
    harness.wait_until("exit", |screen| screen.exit.is_some());
    assert_eq!(harness.screen.exit.unwrap().code(), Some(3));
    assert!(shows("bye")(&harness.screen));
}

#[test]
fn key_input_reaches_the_child() {
    let mut harness = start(PtyConfig::command("/bin/cat", [""; 0]));
    harness.terminal.write(b"typed\n".to_vec()).unwrap();
    harness.wait_until("echo from cat", |screen| {
        screen
            .rows
            .iter()
            .filter(|r| r.text().contains("typed"))
            .count()
            == 2
    });
}

/// The program asks where the cursor is; the core's answer has to travel
/// back through the PTY for the program to print it.
#[test]
fn replies_of_the_core_are_written_to_the_pty() {
    let mut harness = sh("stty -icanon -echo; printf 'ab\\033[6n'; \
         x=$(dd bs=1 count=6 2>/dev/null); printf ' reply:%s' \"${x#?}\"; sleep 100");
    harness.wait_until("reply", shows("reply:[1;3R"));
}

/// Colors changed while the program runs answer its later queries, so that
/// a program that adapts to the background sees a reloaded theme.
#[test]
fn reported_colors_can_change_while_the_program_runs() {
    let mut harness = sh("stty -icanon -echo; sleep 0.5; printf '\\033]11;?\\007'; \
         x=$(dd bs=1 count=24 2>/dev/null); printf ' reply:%s' \"${x#??}\"; sleep 100");
    harness.terminal.set_reported_colors(ReportedColors {
        foreground: Rgb(4, 5, 6),
        background: Rgb(1, 2, 3),
        cursor: Rgb(7, 8, 9),
        palette: [Rgb::default(); 256],
    });
    harness.wait_until("reply", shows("reply:11;rgb:0101/0202/0303"));
}

#[test]
fn the_title_follows_the_program() {
    let mut harness = sh("printf '\\033]2;my title\\007'; sleep 100");
    harness.wait_until("title", |screen| screen.title.is_some());
    assert_eq!(harness.screen.title.as_deref(), Some("my title"));
}

/// The notifications of the program are taken once each, in order.
#[test]
fn the_notifications_of_the_program_are_handed_on_in_order() {
    let mut harness =
        sh("printf '\\033]9;build done\\007\\033]777;notify;T;B\\007'; printf end; sleep 100");
    harness.wait_until("end", shows("end"));
    assert_eq!(
        harness.terminal.take_notifications(),
        vec![
            Notification {
                title: None,
                body: "build done".into()
            },
            Notification {
                title: Some("T".into()),
                body: "B".into()
            },
        ]
    );
    assert!(harness.terminal.take_notifications().is_empty());
}

#[test]
fn resize_reaches_the_core_and_the_child() {
    let mut harness = sh("sleep 0.5; stty size; sleep 100");
    harness.terminal.resize(GridSize::new(10, 30), (300, 200));
    harness.wait_until("stty size", shows("10 30"));
    assert_eq!(harness.screen.size, GridSize::new(10, 30));
    assert_eq!(harness.screen.rows.len(), 10);
}

#[test]
fn no_line_of_a_large_output_is_lost() {
    let mut harness = sh("seq 1 20000");
    harness.wait_until("exit", |screen| screen.exit.is_some());
    let rows = harness.screen.size.rows as usize;
    // 20000 lines and the empty line the cursor is on.
    assert_eq!(harness.screen.scrollback_len + rows, 20001);
    let history = harness
        .terminal
        .scrollback_rows(0..harness.screen.scrollback_len);
    let lines = history.iter().chain(&harness.screen.rows);
    for (index, row) in lines.take(20000).enumerate() {
        assert_eq!(row.text().trim_end(), (index + 1).to_string());
    }
}

/// `sleep` never reads its input, so the kernel buffer fills up. Queuing
/// input must still return at once, the screen must keep updating, and
/// dropping the terminal must not hang on the blocked write.
#[test]
fn a_child_that_does_not_read_blocks_neither_input_nor_shutdown() {
    let mut harness = sh("stty -echo; printf ready; sleep 1; printf later; sleep 100");
    harness.wait_until("ready", shows("ready"));
    let terminal = harness.terminal;
    let terminal = within("write", move || {
        let _ = terminal.write(vec![b'x'; 1 << 20]);
        terminal
    });
    harness.terminal = terminal;
    harness.wait_until("later output", shows("later"));
    within("drop", move || drop(harness.terminal));
}

#[test]
fn dropping_a_terminal_ends_its_worker() {
    let harness = sh("sleep 100");
    let wakes = harness.wakes;
    within("drop", move || drop(harness.terminal));
    // The worker owned the waker, so the channel closes once it has ended.
    let closed = within("worker", move || wakes.iter().count());
    assert!(closed <= 1, "at most the wake for the exit");
}

#[test]
fn text_is_the_screen_without_trailing_blanks() {
    let mut h = sh("printf 'one  \\n\\n  three\\n'; sleep 100");
    h.wait_until("the output", shows("three"));
    assert_eq!(h.terminal.text(None), "one\n\n  three");
}

#[test]
fn text_has_blanks_where_the_screen_hides_its_characters() {
    let mut h = sh("printf 'ok \\033[8mrun this\\033[28m end\\n'; sleep 100");
    h.wait_until("the output", shows("end"));
    assert_eq!(h.terminal.text(None), "ok          end");
}

#[test]
fn text_has_blanks_where_the_ink_has_the_color_of_the_background() {
    let mut h = sh("printf 'ok \\033[38;2;1;2;3mrun this\\033[0m end\\n'; sleep 100");
    h.terminal.set_reported_colors(ReportedColors {
        foreground: Rgb(4, 5, 6),
        background: Rgb(1, 2, 3),
        cursor: Rgb(7, 8, 9),
        palette: [Rgb::default(); 256],
    });
    h.wait_until("the output", shows("end"));
    assert_eq!(h.terminal.text(None), "ok          end");
}

#[test]
fn text_of_a_blank_screen_is_empty() {
    let mut h = sh("sleep 100");
    h.wait_until("the first sync", |screen| !screen.rows.is_empty());
    assert_eq!(h.terminal.text(None), "");
    assert_eq!(h.terminal.text(Some(3)), "");
}

#[test]
fn text_with_a_number_takes_the_last_lines_of_scrollback_and_screen() {
    // Ten lines on a screen of six rows: the first ones scroll off.
    let mut h = sh("for i in 1 2 3 4 5 6 7 8 9 10; do echo line-$i; done; sleep 100");
    h.wait_until("the output", shows("line-10"));
    assert_eq!(h.terminal.text(Some(2)), "line-9\nline-10");
    assert_eq!(h.terminal.text(Some(0)), "");
    let all = h.terminal.text(Some(1000));
    assert!(all.starts_with("line-1\nline-2\n"), "{all:?}");
    assert!(all.ends_with("line-9\nline-10"), "{all:?}");
    // Without a number, only what is on the screen.
    let screen = h.terminal.text(None);
    assert!(screen.starts_with("line-6\n"), "{screen:?}");
}

#[test]
fn text_joins_a_line_that_wrapped_and_keeps_its_blanks() {
    // 40 columns: the 50 characters wrap after "b" and a blank.
    let line = format!("{} b {}", "a".repeat(37), "c".repeat(10));
    let mut h = sh(&format!("echo '{line}'; echo end; sleep 100"));
    h.wait_until("the output", shows("end"));
    assert_eq!(h.terminal.text(None), format!("{line}\nend"));
    // Lines are counted as rows: the last two are the wrapped rest and "end".
    assert_eq!(h.terminal.text(Some(2)), format!("{}\nend", "c".repeat(10)));
}

#[test]
fn text_does_not_count_blank_rows_at_the_end_of_the_scrollback() {
    // The line and blank rows scroll off a screen of six rows, and the
    // screen is left blank.
    let mut h = sh("printf 'marker\\n\\n\\n\\n\\n\\n\\n\\n\\n'; sleep 100");
    h.wait_until("the line to scroll off", |screen| {
        screen.scrollback_len >= 3
    });
    assert_eq!(h.terminal.text(None), "");
    assert_eq!(h.terminal.text(Some(1)), "marker");
    assert_eq!(h.terminal.text(Some(5)), "marker");
}

#[test]
fn write_tells_when_the_program_no_longer_takes_input() {
    let mut h = sh("exit 0");
    assert!(
        h.terminal.write(Vec::new()).is_ok(),
        "a running program takes input"
    );
    h.wait_until("the exit", |screen| screen.exit.is_some());
    // The writer thread ends a moment after the program.
    let deadline = Instant::now() + TIMEOUT;
    while h.terminal.write(b"x".to_vec()).is_ok() {
        assert!(Instant::now() < deadline, "input is still taken");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Prints the rows, the columns and the size in pixels the kernel has for
/// the terminal. `stty` shows no pixels, so the program asks with the
/// TIOCGWINSZ of macOS itself.
const SHOWS_ITS_WINDOW_SIZE: &str = r#"/usr/bin/perl -e '$w = "\0" x 8; ioctl(STDIN, 0x40087468, $w) or die; print "size=", join(",", unpack("S4", $w)), ";\n"'; sleep 100"#;

/// The grid of the config decides the rows and columns, and the size in
/// pixels of the PTY config is kept. A program then has, from its start,
/// the whole size its first frame would set, and that frame changes
/// nothing.
#[test]
fn the_program_starts_with_the_grid_and_the_pixel_size_of_the_config() {
    let pty = PtyConfig {
        size: WindowSize {
            rows: 1,
            cols: 1,
            pixel_width: 320,
            pixel_height: 114,
        },
        ..PtyConfig::command("/bin/sh", ["-c", SHOWS_ITS_WINDOW_SIZE])
    };
    let mut harness = start(pty);
    harness.wait_until("the window size", shows("size=6,40,320,114;"));
}

#[test]
fn sync_if_ready_defers_during_synchronized_update_and_preserves_damage() {
    let script = r#"stty -echo; printf before; while IFS= read -r line; do eval "$line"; done"#;
    let mut harness = sh(script);
    harness.wait_until("initial screen", shows("before"));

    // Start a synchronized update and modify two separate rows without ending it.
    harness
        .terminal
        .write(b"printf '\\033[?2026h\\033[1;1Hrow0\\033[3;1Hrow2'\n".to_vec())
        .unwrap();
    let deadline = Instant::now() + TIMEOUT;
    while !harness.terminal.text(None).contains("row2") {
        assert!(Instant::now() < deadline, "core did not receive update");
        let _ = harness.wakes.recv_timeout(Duration::from_millis(50));
    }

    // Core has the new rows, but sync_if_ready(false) defers without touching
    // the screen or consuming damage.
    assert_eq!(
        harness.terminal.sync_if_ready(&mut harness.screen, false),
        SyncOutcome::Deferred
    );
    assert!(shows("before")(&harness.screen));
    assert!(!shows("row0")(&harness.screen));
    assert!(!shows("row2")(&harness.screen));
    let latest_rows = harness.terminal.screen_rows();
    assert!(latest_rows[0].text().contains("row0"));
    assert!(latest_rows[2].text().contains("row2"));

    // Ending the synchronized update flushes all accumulated row damage.
    harness
        .terminal
        .write(b"printf '\\033[?2026l'\n".to_vec())
        .unwrap();
    let deadline = Instant::now() + TIMEOUT;
    loop {
        if let SyncOutcome::Updated(_) = harness.terminal.sync_if_ready(&mut harness.screen, false)
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "sync did not complete after 2026l"
        );
        let _ = harness.wakes.recv_timeout(Duration::from_millis(50));
    }
    assert!(harness.screen.rows[0].text().contains("row0"));
    assert!(harness.screen.rows[2].text().contains("row2"));
}

#[test]
fn sync_if_ready_with_force_updates_during_synchronized_update() {
    let script = r#"stty -echo; printf before; while IFS= read -r line; do eval "$line"; done"#;
    let mut harness = sh(script);
    harness.wait_until("initial screen", shows("before"));

    harness
        .terminal
        .write(b"printf '\\033[?2026h\\033[1;1Hforced'\n".to_vec())
        .unwrap();
    let deadline = Instant::now() + TIMEOUT;
    while !harness.terminal.text(None).contains("forced") {
        assert!(Instant::now() < deadline, "core did not receive update");
        let _ = harness.wakes.recv_timeout(Duration::from_millis(50));
    }

    assert_eq!(
        harness.terminal.sync_if_ready(&mut harness.screen, false),
        SyncOutcome::Deferred
    );
    assert!(shows("before")(&harness.screen));

    assert!(matches!(
        harness.terminal.sync_if_ready(&mut harness.screen, true),
        SyncOutcome::Updated(_)
    ));
    assert!(shows("forced")(&harness.screen));
}

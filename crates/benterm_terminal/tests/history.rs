//! What a terminal showed is taken from it as rows, and a terminal
//! that starts shows it above the output of its program.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use benterm_core::{AlacrittyCore, GridSize, Row, TerminalCore, history_bytes};
use benterm_log::{LogReader, RecordingConfig, replay, screen_text};
use benterm_terminal::{PtyConfig, Recorder, Screen, Terminal, TerminalConfig};

const TIMEOUT: Duration = Duration::from_secs(10);
const SIZE: GridSize = GridSize::new(6, 20);

struct Harness {
    terminal: Terminal,
    wakes: Receiver<()>,
    screen: Screen,
}

fn config(script: &str, log: Option<Recorder>) -> TerminalConfig {
    TerminalConfig {
        pty: PtyConfig::command("/bin/sh", ["-c", script]),
        size: SIZE,
        scrollback_limit: 1_000,
        colors: None,
        log,
    }
}

/// Runs `script` on a terminal that feeds its output at once.
fn sh(script: &str) -> Harness {
    let (wake_tx, wakes) = mpsc::channel();
    let waker = move || {
        let _ = wake_tx.send(());
    };
    Harness {
        terminal: Terminal::spawn(config(script, None), waker).unwrap(),
        wakes,
        screen: Screen::default(),
    }
}

/// Runs `script` on a terminal that holds its output back.
fn held(script: &str, log: Option<Recorder>) -> Harness {
    let (wake_tx, wakes) = mpsc::channel();
    let waker = move || {
        let _ = wake_tx.send(());
    };
    Harness {
        terminal: Terminal::spawn_held(config(script, log), waker).unwrap(),
        wakes,
        screen: Screen::default(),
    }
}

impl Harness {
    fn wait_for(&mut self, text: &str) {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            self.terminal.sync(&mut self.screen);
            if self.screen.rows.iter().any(|row| row.text().contains(text)) {
                return;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if self.wakes.recv_timeout(left).is_err() {
                panic!(
                    "{text:?} not before the timeout; screen: {:?}",
                    self.texts()
                );
            }
        }
    }

    /// The rows of the screen that are not blank.
    fn texts(&self) -> Vec<String> {
        let texts = self.screen.rows.iter();
        texts
            .map(|row| row.text().trim_end().to_owned())
            .filter(|text| !text.is_empty())
            .collect()
    }
}

fn texts(rows: &[Row]) -> Vec<String> {
    rows.iter()
        .map(|row| row.text().trim_end().to_owned())
        .collect()
}

/// Long enough for the output of a program that has started to arrive.
const A_MOMENT: Duration = Duration::from_millis(300);

#[test]
fn a_held_terminal_feeds_no_output_until_it_is_released() {
    let mut harness = held("printf prompt; sleep 100", None);
    std::thread::sleep(A_MOMENT);
    harness.terminal.sync(&mut harness.screen);
    assert_eq!(harness.texts(), [""; 0]);

    harness.terminal.release(Vec::new());
    harness.wait_for("prompt");
    assert_eq!(harness.texts(), ["prompt"]);
}

#[test]
fn history_is_shown_above_the_output_that_was_held_back() {
    let mut harness = held("printf prompt; sleep 100", None);
    // The output has arrived by now, and still comes second.
    std::thread::sleep(A_MOMENT);
    harness
        .terminal
        .release(b"\x1b[31mold\x1b[0m\r\nolder\r\n".to_vec());
    harness.wait_for("prompt");
    assert_eq!(harness.texts(), ["old", "older", "prompt"]);
}

#[test]
fn bytes_that_are_not_history_are_not_shown() {
    let mut harness = held("printf prompt; sleep 100", None);
    // A title, and a question the core would answer to the program.
    harness
        .terminal
        .release(b"old\x1b]0;title\x07\x1b[6n\r\n".to_vec());
    harness.wait_for("prompt");
    assert_eq!(harness.texts(), ["prompt"]);
    assert_eq!(harness.screen.title, None);
}

#[test]
fn a_held_terminal_that_is_never_released_can_be_dropped() {
    let harness = held("printf prompt; sleep 100", None);
    let (done_tx, done) = mpsc::channel();
    std::thread::spawn(move || {
        drop(harness);
        let _ = done_tx.send(());
    });
    done.recv_timeout(TIMEOUT)
        .expect("dropping the terminal returns");
}

#[test]
fn history_is_the_scrollback_and_the_screen_without_the_blank_rows_below() {
    let mut harness = sh("seq 1 8; printf end; sleep 100");
    harness.wait_for("end");
    let rows = harness.terminal.history(100).unwrap();
    assert_eq!(
        texts(&rows),
        ["1", "2", "3", "4", "5", "6", "7", "8", "end"]
    );

    let mut harness = sh("printf 'a\\r\\n\\r\\nb\\r\\n'; sleep 100");
    harness.wait_for("b");
    assert_eq!(
        texts(&harness.terminal.history(100).unwrap()),
        ["a", "", "b"]
    );
}

#[test]
fn there_is_no_history_to_take_before_the_terminal_is_released() {
    let mut harness = held("printf prompt; sleep 100", None);
    // Nothing yet is not the same as nothing: what the terminal is
    // about to show must not be replaced by an empty screen.
    assert_eq!(harness.terminal.history(100), None);
    harness.terminal.release(b"old\r\n".to_vec());
    harness.wait_for("prompt");
    assert_eq!(
        texts(&harness.terminal.history(100).unwrap()),
        ["old", "prompt"]
    );
}

#[test]
fn history_is_at_most_as_many_rows_as_asked_for_and_they_are_the_last() {
    let mut harness = sh("seq 1 30; printf end; sleep 100");
    harness.wait_for("end");
    let rows = harness.terminal.history(3).unwrap();
    assert_eq!(texts(&rows), ["29", "30", "end"]);
    assert_eq!(harness.terminal.history(0).unwrap(), []);
}

#[test]
fn there_is_no_history_to_take_while_the_alternate_screen_is_shown() {
    let mut harness = sh("seq 1 8; printf '\\033[?1049hfull'; read line; \
         printf '\\033[?1049lback'; sleep 100");
    harness.wait_for("full");
    assert_eq!(harness.terminal.history(100), None);

    harness.terminal.write(b"\n".to_vec()).unwrap();
    harness.wait_for("back");
    let rows = harness.terminal.history(100).unwrap();
    assert_eq!(texts(&rows).first().map(String::as_str), Some("1"));
}

#[test]
fn the_count_of_changes_goes_up_with_output_and_not_without() {
    let mut harness = sh("printf one; read line; printf two; sleep 100");
    harness.wait_for("one");
    let first = harness.terminal.changes();
    std::thread::sleep(A_MOMENT);
    assert_eq!(harness.terminal.changes(), first);

    harness.terminal.write(b"\n".to_vec()).unwrap();
    harness.wait_for("two");
    assert!(harness.terminal.changes() > first);
}

/// The test the issue asks for: what one terminal showed, colors and
/// all, is what the next one shows above its own output.
#[test]
fn what_a_terminal_showed_is_shown_again_by_the_next_one() {
    let mut first = sh("printf '\\033[1;32mgreen\\033[0m plain\\r\\n'; seq 1 12; \
         printf '日本語 \\033[48;5;17mback\\033[0m\\r\\n'; \
         printf 'a line that is longer than the terminal is wide'; sleep 100");
    first.wait_for("wide");
    let showed = first.terminal.history(100).unwrap();
    assert!(
        showed.len() > usize::from(SIZE.rows),
        "some of it scrolled off"
    );

    let mut second = held("printf prompt; sleep 100", None);
    second.terminal.release(history_bytes(&showed));
    second.wait_for("prompt");
    let shows = second.terminal.history(100).unwrap();
    let (again, own) = shows.split_at(showed.len());
    assert_eq!(again, showed);
    assert_eq!(texts(own), ["prompt"]);
}

#[test]
fn history_is_in_the_log_so_that_replaying_it_gives_the_same_screen() {
    let home = tempfile::tempdir().unwrap();
    let recording = RecordingConfig {
        enabled: true,
        dir: PathBuf::from("logs"),
    };
    let recorder = recording.start(home.path(), SIZE).unwrap().unwrap();
    let path = recorder.path().to_path_buf();
    let mut harness = held("printf prompt; sleep 100", Some(recorder));
    harness.terminal.release(b"old\r\n".to_vec());
    harness.wait_for("prompt");

    let file = std::io::BufReader::new(std::fs::File::open(path).unwrap());
    let log = LogReader::new(file).unwrap();
    let mut core = AlacrittyCore::new(log.initial_size(), 1_000);
    replay(log, &mut core).unwrap();
    let replayed = screen_text(&core.snapshot());
    assert!(replayed.starts_with("old\nprompt\n"), "{replayed:?}");
}

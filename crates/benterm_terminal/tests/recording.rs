//! A terminal given a recorder logs what its core is fed, and replaying the
//! log gives the screen the terminal showed.

use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use benterm_core::{AlacrittyCore, GridSize, TerminalCore};
use benterm_log::{LogReader, Record, RecordingConfig, replay, screen_text};
use benterm_terminal::{PtyConfig, Screen, Terminal, TerminalConfig};

const TIMEOUT: Duration = Duration::from_secs(10);
const SIZE: GridSize = GridSize::new(6, 40);
const SCROLLBACK_LIMIT: usize = 1_000;

struct Harness {
    terminal: Terminal,
    wakes: Receiver<()>,
    screen: Screen,
}

impl Harness {
    /// Runs `script` on a terminal that records to a log in `home`.
    fn start(script: &str, home: &Path) -> (Self, PathBuf) {
        let recording = RecordingConfig {
            enabled: true,
            dir: PathBuf::from("logs"),
        };
        let recorder = recording.start(home, SIZE).unwrap().unwrap();
        let path = recorder.path().to_path_buf();
        let (wake_tx, wakes) = mpsc::channel();
        let config = TerminalConfig {
            pty: PtyConfig::command("/bin/sh", ["-c", script]),
            size: SIZE,
            scrollback_limit: SCROLLBACK_LIMIT,
            colors: None,
            log: Some(recorder),
        };
        let terminal = Terminal::spawn(config, move || {
            let _ = wake_tx.send(());
        })
        .unwrap();
        let harness = Self {
            terminal,
            wakes,
            screen: Screen::default(),
        };
        (harness, path)
    }

    fn wait_for(&mut self, text: &str) {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            self.terminal.sync(&mut self.screen);
            if self.screen.rows.iter().any(|row| row.text().contains(text)) {
                return;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if self.wakes.recv_timeout(left).is_err() {
                panic!("{text:?} not before the timeout; screen: {}", self.text());
            }
        }
    }

    /// The screen in the form `screen_text` gives.
    fn text(&self) -> String {
        let mut text = String::new();
        for row in &self.screen.rows {
            text.push_str(row.text().trim_end_matches(' '));
            text.push('\n');
        }
        text
    }
}

fn read_log(path: &Path) -> (GridSize, Vec<Record>) {
    let log = LogReader::new(BufReader::new(File::open(path).unwrap())).unwrap();
    let size = log.initial_size();
    (size, log.map(Result::unwrap).collect())
}

fn replayed_screen(path: &Path) -> String {
    let log = LogReader::new(BufReader::new(File::open(path).unwrap())).unwrap();
    let mut core = AlacrittyCore::new(log.initial_size(), SCROLLBACK_LIMIT);
    replay(log, &mut core).unwrap();
    screen_text(&core.snapshot())
}

#[test]
fn output_and_resizes_are_logged_in_the_order_the_core_got_them() {
    let home = tempfile::tempdir().unwrap();
    let (mut harness, path) = Harness::start(
        "printf before; read line; printf 'after %s' \"$(stty size)\"; sleep 100",
        home.path(),
    );
    harness.wait_for("before");
    harness.terminal.resize(GridSize::new(8, 30), (300, 200));
    harness.terminal.write(b"\n".to_vec()).unwrap();
    harness.wait_for("after 8 30");

    let (initial, records) = read_log(&path);
    assert_eq!(initial, SIZE);
    let output_up_to = |end: usize| -> String {
        records[..end]
            .iter()
            .filter_map(|record| match record {
                Record::Output(bytes) => Some(String::from_utf8_lossy(bytes).into_owned()),
                Record::Resize(_) => None,
            })
            .collect()
    };
    let resizes: Vec<usize> = (0..records.len())
        .filter(|&i| matches!(records[i], Record::Resize(_)))
        .collect();
    assert_eq!(resizes.len(), 1, "{records:?}");
    let resize = resizes[0];
    assert_eq!(records[resize], Record::Resize(GridSize::new(8, 30)));
    assert!(output_up_to(resize).contains("before"));
    assert!(!output_up_to(resize).contains("after"));
    assert!(output_up_to(records.len()).contains("after 8 30"));
}

#[test]
fn replaying_the_log_gives_the_screen_the_terminal_showed() {
    let home = tempfile::tempdir().unwrap();
    let (mut harness, path) = Harness::start(
        "printf '\\033[31mred\\033[0m\\r\\n'; seq 1 20; read line; \
         printf '\\033[2J\\033[Hcleared'; seq 1 3; printf done; sleep 100",
        home.path(),
    );
    harness.wait_for("20");
    harness.terminal.resize(GridSize::new(4, 20), (200, 100));
    harness.terminal.write(b"\n".to_vec()).unwrap();
    harness.wait_for("done");

    assert_eq!(replayed_screen(&path), harness.text());
}

#[test]
fn a_terminal_without_a_recorder_writes_no_log() {
    let home = tempfile::tempdir().unwrap();
    let (wake_tx, wakes) = mpsc::channel();
    let config = TerminalConfig {
        pty: PtyConfig::command("/bin/sh", ["-c", "printf hello; sleep 100"]),
        size: SIZE,
        scrollback_limit: SCROLLBACK_LIMIT,
        colors: None,
        log: None,
    };
    let terminal = Terminal::spawn(config, move || {
        let _ = wake_tx.send(());
    })
    .unwrap();
    let mut harness = Harness {
        terminal,
        wakes,
        screen: Screen::default(),
    };
    harness.wait_for("hello");
    assert_eq!(std::fs::read_dir(home.path()).unwrap().count(), 0);
}

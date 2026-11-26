//! The app records a terminal's PTY log only when the settings ask for it,
//! and `benterm-replay` shows the screen the app showed.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use benterm_config::PtyLogSettings;
use benterm_core::GridSize;
use benterm_log::{LogReader, Record, RecordingConfig, RecordingError};
use benterm_terminal::PtyConfig;
use benterm_ui::recording::start_recorder;
use benterm_ui::settings::ConfigFeed;
use benterm_ui::theme::Theme;
use benterm_ui::{
    BentermWindow, FirstPane, SpawnTerminal, StartedTerminal, TerminalView, open_started_window,
};
use gpui::{TestAppContext, VisualTestContext, WindowHandle, px, size};

const SIZE: GridSize = GridSize::new(24, 80);

fn enabled(dir: impl Into<PathBuf>) -> PtyLogSettings {
    PtyLogSettings {
        enabled: true,
        dir: dir.into(),
    }
}

fn files_in(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries.map(|entry| entry.unwrap().path()).collect()
}

#[test]
fn the_default_directory_is_the_one_benterm_log_uses() {
    assert_eq!(
        PtyLogSettings::default().dir,
        RecordingConfig::default().dir
    );
}

#[test]
fn nothing_is_recorded_by_default() {
    let home = tempfile::tempdir().unwrap();
    let recorder = start_recorder(&PtyLogSettings::default(), Some(home.path()), SIZE).unwrap();
    assert!(recorder.is_none());
    assert_eq!(files_in(home.path()), Vec::<PathBuf>::new());
}

#[test]
fn nothing_is_recorded_without_a_home_unless_enabled() {
    let recorder = start_recorder(&PtyLogSettings::default(), None, SIZE).unwrap();
    assert!(recorder.is_none());
    assert!(start_recorder(&enabled("logs"), None, SIZE).is_err());
}

#[test]
fn an_enabled_recording_writes_a_log_in_the_directory_under_home() {
    let home = tempfile::tempdir().unwrap();
    let recorder = start_recorder(&enabled("logs"), Some(home.path()), SIZE)
        .unwrap()
        .unwrap();
    let dir = std::fs::canonicalize(home.path().join("logs")).unwrap();
    assert_eq!(files_in(&dir), vec![recorder.path().to_path_buf()]);
}

#[test]
fn a_directory_outside_home_is_refused_and_left_alone() {
    let home = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let target = outside.path().join("logs");
    for dir in [target.clone(), PathBuf::from("../logs")] {
        let result = start_recorder(&enabled(dir), Some(home.path()), SIZE);
        assert!(matches!(result, Err(RecordingError::OutsideHome(_))));
    }
    assert!(!target.exists());
    assert_eq!(files_in(home.path()), Vec::<PathBuf>::new());
}

/// Opens a window whose terminal runs `script` and records as `settings`
/// ask, with `home` as the home directory.
fn open(
    script: &str,
    settings: &PtyLogSettings,
    home: &Path,
    cx: &mut TestAppContext,
) -> WindowHandle<BentermWindow> {
    cx.executor().allow_parking();
    let theme = Theme::default();
    let pty = PtyConfig::command("/bin/sh", ["-c", script]);
    let first = FirstPane::start_with(|_| {
        StartedTerminal::spawn_recorded(pty.clone(), &theme.terminal, settings, Some(home))
    })
    .expect("the terminal starts");
    let spawn: SpawnTerminal =
        std::rc::Rc::new(move |_, theme, _, _| StartedTerminal::spawn(pty.clone(), theme));
    cx.update(|cx| open_started_window(first, spawn, ConfigFeed::defaults(), cx))
        .unwrap()
}

fn wait_until(
    window: WindowHandle<BentermWindow>,
    cx: &mut VisualTestContext,
    done: impl Fn(&TerminalView) -> bool,
) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        cx.run_until_parked();
        let terminal = window
            .update(cx, |root, _, cx| root.terminal(cx))
            .expect("the window is open");
        if terminal.read_with(cx, |view, _| done(view)) {
            return;
        }
        assert!(Instant::now() < deadline, "not before the timeout");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn shows(text: &'static str) -> impl Fn(&TerminalView) -> bool {
    move |view| {
        view.screen()
            .rows
            .iter()
            .any(|row| row.text().contains(text))
    }
}

/// The screen in the form `benterm-replay` prints it.
fn screen_text(view: &TerminalView) -> String {
    let mut text = String::new();
    for row in &view.screen().rows {
        text.push_str(row.text().trim_end_matches(' '));
        text.push('\n');
    }
    text
}

fn benterm_replay(log: &Path) -> String {
    let output = Command::new(env!("CARGO"))
        .args([
            "run",
            "--quiet",
            "-p",
            "benterm_log",
            "--bin",
            "benterm-replay",
            "--",
        ])
        .arg(log)
        .output()
        .expect("cargo runs");
    assert!(
        output.status.success(),
        "benterm-replay failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

#[gpui::test]
fn benterm_replay_shows_the_screen_the_app_showed(cx: &mut TestAppContext) {
    let home = tempfile::tempdir().unwrap();
    let window = open(
        "printf '\\033[1mbefore\\033[0m\\r\\n'; read line; stty size; seq 1 40; printf done; sleep 100",
        &enabled("logs"),
        home.path(),
        cx,
    );
    let mut cx = VisualTestContext::from_window(window.into(), cx);
    wait_until(window, &mut cx, shows("before"));
    cx.simulate_resize(size(px(500.), px(300.)));
    cx.run_until_parked();
    cx.simulate_keystrokes("enter");
    wait_until(window, &mut cx, shows("done"));

    let terminal = window
        .update(&mut cx, |root, _, cx| root.terminal(cx))
        .unwrap();
    let (shown, shown_size) =
        terminal.read_with(&cx, |view, _| (screen_text(view), view.screen().size));
    let logs = files_in(&home.path().join("logs"));
    assert_eq!(logs.len(), 1, "{logs:?}");

    // The window's resize reached the log, not only the output.
    let reader = LogReader::new(std::fs::File::open(&logs[0]).unwrap()).unwrap();
    let records: Vec<Record> = reader.map(Result::unwrap).collect();
    assert_ne!(shown_size, SIZE);
    assert!(records.contains(&Record::Resize(shown_size)), "{records:?}");

    assert_eq!(benterm_replay(&logs[0]), shown);
}

#[gpui::test]
fn the_app_writes_no_log_when_recording_is_off(cx: &mut TestAppContext) {
    let home = tempfile::tempdir().unwrap();
    let window = open(
        "printf hello; sleep 100",
        &PtyLogSettings::default(),
        home.path(),
        cx,
    );
    let mut cx = VisualTestContext::from_window(window.into(), cx);
    wait_until(window, &mut cx, shows("hello"));
    assert_eq!(files_in(home.path()), Vec::<PathBuf>::new());
}

/// A terminal started at the size of its pane is logged from that size
/// on, so a replay begins with the grid the program began with.
#[test]
fn the_log_begins_with_the_size_the_terminal_starts_with() {
    let home = tempfile::tempdir().unwrap();
    let pty = PtyConfig {
        size: benterm_terminal::WindowSize::new(30, 100),
        ..PtyConfig::command("/bin/sh", ["-c", "stty size; sleep 100"])
    };
    let theme = Theme::default();
    let started =
        StartedTerminal::spawn_recorded(pty, &theme.terminal, &enabled("logs"), Some(home.path()))
            .expect("the terminal starts");
    // Dropping the terminal ends the program and finishes the log.
    drop(started);

    let logs = files_in(&home.path().join("logs"));
    assert_eq!(logs.len(), 1, "{logs:?}");
    let reader = LogReader::new(std::fs::File::open(&logs[0]).unwrap()).unwrap();
    assert_eq!(reader.initial_size(), GridSize::new(30, 100));
}

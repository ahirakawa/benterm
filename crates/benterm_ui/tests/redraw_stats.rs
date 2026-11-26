//! Counting the redraws that the output of terminals asks for, which
//! `cargo xtask redraws` reads. Nothing is counted, and no file is
//! written, unless the app is asked to.
//!
//! Each terminal runs a script that prints one letter for each file
//! the test makes, and nothing else, so that it wakes its view exactly
//! as often as the test says.

use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use benterm_terminal::PtyConfig;
use benterm_ui::redraw_stats::{self, RedrawCounts};
use benterm_ui::settings::ConfigFeed;
use benterm_ui::theme::Theme;
use benterm_ui::{BentermWindow, StartedTerminal, TerminalView, bind_keys, ipc};
use gpui::{AppContext, Entity, TestAppContext, VisualTestContext, WindowHandle};

const TIMEOUT: Duration = Duration::from_secs(10);

/// How many letters each script prints.
const LETTERS: usize = 3;

/// A script that waits for `go1`, `go2` and `go3` in `dir` and prints a
/// letter after each.
fn printer(dir: &Path) -> PtyConfig {
    let script = format!(
        "for n in 1 2 3; do while [ ! -e '{dir}/go'$n ]; do sleep 0.01; done; printf x; done; \
         sleep 100",
        dir = dir.display()
    );
    PtyConfig::command("/bin/sh", ["-c", script.as_str()])
}

struct Benterm {
    window: WindowHandle<BentermWindow>,
    cx: VisualTestContext,
    /// The pane's script waits for its files here.
    shown: tempfile::TempDir,
    /// A terminal whose view is drawn nowhere, and the directory its
    /// script waits for files in.
    hidden: Option<(Entity<TerminalView>, tempfile::TempDir)>,
}

/// Opens a window whose only pane runs [`printer`], after starting the
/// counting with `stats` as `BENTERM_REDRAW_STATS` would.
fn open(stats: Option<PathBuf>, cx: &mut TestAppContext) -> Benterm {
    cx.executor().allow_parking();
    cx.update(bind_keys);
    cx.update(|cx| redraw_stats::start(stats, cx));
    let shown = tempfile::tempdir().unwrap();
    let start: ipc::StartTerminal = Rc::new(StartedTerminal::spawn);
    let window = cx
        .update(|cx| {
            ipc::open_session_window(
                printer(shown.path()),
                start,
                None,
                None,
                ConfigFeed::defaults(),
                cx,
            )
        })
        .unwrap();
    let cx = VisualTestContext::from_window(window.into(), cx);
    let benterm = Benterm {
        window,
        cx,
        shown,
        hidden: None,
    };
    benterm.cx.run_until_parked();
    benterm
}

impl Benterm {
    /// Starts a terminal running [`printer`] whose view is in no pane.
    fn start_hidden(&mut self) {
        let dir = tempfile::tempdir().unwrap();
        let theme = Theme::default();
        let started = StartedTerminal::spawn(printer(dir.path()), &theme.terminal).unwrap();
        let view = self
            .window
            .update(&mut self.cx, |_, window, cx| {
                cx.new(|cx| TerminalView::new(started, theme, window, cx))
            })
            .unwrap();
        self.hidden = Some((view, dir));
    }

    fn counts(&mut self) -> Option<RedrawCounts> {
        self.cx.update(|_, cx| redraw_stats::counts(cx))
    }

    fn wait_until(&mut self, what: &str, mut done: impl FnMut(&mut Self) -> bool) {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            self.cx.run_until_parked();
            if done(self) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {what}; counts: {:?}",
                self.counts()
            );
            std::thread::sleep(Duration::from_millis(10));
            self.cx.executor().advance_clock(Duration::from_millis(10));
        }
    }

    /// Lets the script waiting in `dir` print its letters one by one,
    /// each after the wake of the one before was counted.
    fn print_letters(&mut self, dir: &Path) {
        for n in 1..=LETTERS {
            let before = self.redraws();
            fs::write(dir.join(format!("go{n}")), "").unwrap();
            self.wait_until("the wake of a letter", |benterm| {
                benterm.redraws() == before + 1
            });
        }
    }

    fn redraws(&mut self) -> u64 {
        self.counts()
            .map_or(0, |counts| counts.visible_redraws + counts.hidden_redraws)
    }

    fn shown_dir(&self) -> PathBuf {
        self.shown.path().to_path_buf()
    }

    fn hidden_dir(&self) -> PathBuf {
        self.hidden.as_ref().unwrap().1.path().to_path_buf()
    }
}

#[gpui::test]
fn the_wakes_of_a_terminal_drawn_nowhere_count_as_hidden(cx: &mut TestAppContext) {
    let stats = tempfile::tempdir().unwrap();
    let mut benterm = open(Some(stats.path().join("redraws")), cx);
    benterm.start_hidden();
    let dir = benterm.hidden_dir();
    benterm.print_letters(&dir);
    let counts = benterm.counts().unwrap();
    assert_eq!(counts.hidden_redraws, 3);
    assert_eq!(counts.visible_redraws, 0);
}

#[gpui::test]
fn the_wakes_of_the_pane_on_view_count_as_visible(cx: &mut TestAppContext) {
    let stats = tempfile::tempdir().unwrap();
    let mut benterm = open(Some(stats.path().join("redraws")), cx);
    benterm.wait_until("a frame", |benterm| benterm.counts().unwrap().frames > 0);
    let dir = benterm.shown_dir();
    benterm.print_letters(&dir);
    let counts = benterm.counts().unwrap();
    assert_eq!(counts.visible_redraws, 3);
    assert_eq!(counts.hidden_redraws, 0);
}

#[gpui::test]
fn each_wake_takes_the_state_lock_four_times(cx: &mut TestAppContext) {
    // The four lock acquisitions read the screen, command endings,
    // OSC 7501 updates, and notifications.
    let stats = tempfile::tempdir().unwrap();
    let mut benterm = open(Some(stats.path().join("redraws")), cx);
    benterm.start_hidden();
    let dir = benterm.hidden_dir();
    benterm.print_letters(&dir);
    let counts = benterm.counts().unwrap();
    assert_eq!(counts.hidden_redraws, 3);
    assert_eq!(counts.wake_locks, 4 * 3);
}

#[gpui::test]
fn a_frame_drawn_is_counted(cx: &mut TestAppContext) {
    let stats = tempfile::tempdir().unwrap();
    let mut benterm = open(Some(stats.path().join("redraws")), cx);
    let before = benterm.counts().unwrap().frames;
    let dir = benterm.shown_dir();
    benterm.print_letters(&dir);
    // Each letter on view is drawn.
    assert!(benterm.counts().unwrap().frames >= before + 3);
}

#[gpui::test]
fn the_counts_are_written_to_the_file_from_time_to_time(cx: &mut TestAppContext) {
    let stats = tempfile::tempdir().unwrap();
    let file = stats.path().join("redraws");
    let mut benterm = open(Some(file.clone()), cx);
    let dir = benterm.shown_dir();
    benterm.print_letters(&dir);
    benterm
        .cx
        .executor()
        .advance_clock(redraw_stats::WRITE_INTERVAL);
    benterm.wait_until("the file", |_| file.exists());
    let written = redraw_stats::parse(&fs::read_to_string(&file).unwrap()).unwrap();
    assert_eq!(written.counts.visible_redraws, 3);
}

#[gpui::test]
fn nothing_is_counted_or_written_unless_asked(cx: &mut TestAppContext) {
    let mut benterm = open(None, cx);
    benterm.start_hidden();
    let hidden = benterm.hidden_dir();
    let shown = benterm.shown_dir();
    for n in 1..=LETTERS {
        fs::write(hidden.join(format!("go{n}")), "").unwrap();
        fs::write(shown.join(format!("go{n}")), "").unwrap();
    }
    let (view, _) = benterm.hidden.as_ref().unwrap();
    let view = view.clone();
    benterm.wait_until("the letters", |benterm| {
        let text = benterm.cx.update(|_, cx| view.read(cx).text(None));
        text.contains("xxx")
    });
    benterm
        .cx
        .executor()
        .advance_clock(redraw_stats::WRITE_INTERVAL * 3);
    benterm.cx.run_until_parked();
    assert_eq!(benterm.counts(), None);
}

#[test]
fn only_a_path_in_the_variable_asks_for_counting() {
    assert_eq!(redraw_stats::path_from(None), None);
    assert_eq!(redraw_stats::path_from(Some("".into())), None);
    assert_eq!(
        redraw_stats::path_from(Some("/tmp/redraws".into())),
        Some(PathBuf::from("/tmp/redraws"))
    );
}

#[test]
fn the_file_holds_one_count_on_a_line() {
    let counts = RedrawCounts {
        frames: 120,
        visible_redraws: 30,
        hidden_redraws: 900,
        wake_locks: 3720,
        wake_lock_wait: Duration::from_micros(1500),
    };
    let text = redraw_stats::file_text(&counts, Duration::from_millis(2500));
    assert_eq!(
        text,
        "elapsed_ms 2500\nframes 120\nvisible_redraws 30\nhidden_redraws 900\n\
         wake_locks 3720\nwake_lock_wait_us 1500\n"
    );
    let read = redraw_stats::parse(&text).unwrap();
    assert_eq!(read.counts, counts);
    assert_eq!(read.elapsed, Duration::from_millis(2500));
}

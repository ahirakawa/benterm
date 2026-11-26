//! Counts what the output of terminals costs the window: the redraws
//! the terminal views ask for, split by whether the view was on view,
//! the frames the window draws, and the locks of the terminal's state
//! taken for each wake. `cargo xtask redraws` reads the counts.
//!
//! Nothing is counted unless [`ENV`] names a file. Then the counts go
//! to that file every [`WRITE_INTERVAL`] and when the app quits.

use std::cell::Cell;
use std::ffi::OsString;
use std::fmt::Write as _;
use std::fs;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail};
use benterm_terminal::LockCount;
use gpui::{App, Global};

/// The environment variable that names the file to write the counts
/// to. Unset or empty, nothing is counted.
pub const ENV: &str = "BENTERM_REDRAW_STATS";

/// How often the counts are written to the file.
pub const WRITE_INTERVAL: Duration = Duration::from_millis(250);

/// What has been counted since the counting started.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RedrawCounts {
    /// Frames the window drew.
    pub frames: u64,
    /// Wakes of a terminal whose view was drawn in the last frame. Each
    /// asks for a redraw.
    pub visible_redraws: u64,
    /// Wakes of a terminal whose view was not drawn in the last frame,
    /// such as one in a tab that is not selected or behind the window.
    /// Each asks for a redraw too.
    pub hidden_redraws: u64,
    /// Locks of the terminal's state taken while handling the wakes.
    pub wake_locks: u64,
    /// How long those locks were waited for in all.
    pub wake_lock_wait: Duration,
}

/// The counts and how long they were counted for, as the file has them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Written {
    /// From the start of the counting to the writing of the file.
    pub elapsed: Duration,
    pub counts: RedrawCounts,
}

/// The counting, shared by the window and every terminal view.
pub(crate) struct RedrawStats {
    counts: Cell<RedrawCounts>,
}

impl RedrawStats {
    fn update(&self, change: impl FnOnce(&mut RedrawCounts)) {
        let mut counts = self.counts.get();
        change(&mut counts);
        self.counts.set(counts);
    }

    /// The frames drawn so far, which numbers the frame being drawn.
    pub(crate) fn frames(&self) -> u64 {
        self.counts.get().frames
    }

    /// A terminal woke its view, which then asked for a redraw. `locks`
    /// were taken of its state meanwhile.
    pub(crate) fn wake(&self, visible: bool, locks: LockCount) {
        self.update(|counts| {
            if visible {
                counts.visible_redraws += 1;
            } else {
                counts.hidden_redraws += 1;
            }
            counts.wake_locks += locks.locks;
            counts.wake_lock_wait += locks.waited;
        });
    }
}

struct Installed(Rc<RedrawStats>);

impl Global for Installed {}

/// The file the value of [`ENV`] names, if it names one.
pub fn path_from(value: Option<OsString>) -> Option<PathBuf> {
    value.filter(|value| !value.is_empty()).map(PathBuf::from)
}

/// Starts counting, and writing the counts to `path`, if there is a
/// path. Without one this does nothing, and nothing is counted. Called
/// before the window opens, so that its terminals count too.
pub fn start(path: Option<PathBuf>, cx: &mut App) {
    let Some(path) = path else {
        return;
    };
    let stats = Rc::new(RedrawStats {
        counts: Cell::default(),
    });
    cx.set_global(Installed(Rc::clone(&stats)));
    let started = Instant::now();
    let write = {
        let stats = Rc::clone(&stats);
        move || {
            let text = file_text(&stats.counts.get(), started.elapsed());
            if let Err(error) = write_whole(&path, &text) {
                eprintln!(
                    "benterm: cannot write the redraw counts to {}: {error}",
                    path.display()
                );
            }
        }
    };
    cx.on_app_quit({
        let write = write.clone();
        move |_| {
            write();
            async {}
        }
    })
    .detach();
    cx.spawn(async move |cx| {
        loop {
            cx.background_executor().timer(WRITE_INTERVAL).await;
            write();
        }
    })
    .detach();
}

/// Writes `text` to a file beside `path` and renames it to `path`, so
/// that a reader never finds half of it.
fn write_whole(path: &std::path::Path, text: &str) -> std::io::Result<()> {
    let mut partial = path.as_os_str().to_owned();
    partial.push(".partial");
    fs::write(&partial, text)?;
    fs::rename(&partial, path)
}

/// The counting, if it was started.
pub(crate) fn installed(cx: &App) -> Option<Rc<RedrawStats>> {
    cx.try_global::<Installed>()
        .map(|installed| Rc::clone(&installed.0))
}

/// What has been counted so far, if the counting was started.
pub fn counts(cx: &App) -> Option<RedrawCounts> {
    cx.try_global::<Installed>()
        .map(|installed| installed.0.counts.get())
}

/// The window drew a frame.
pub(crate) fn frame_drawn(cx: &App) {
    if let Some(installed) = cx.try_global::<Installed>() {
        installed.0.update(|counts| counts.frames += 1);
    }
}

/// The file: a name and a number on each line.
pub fn file_text(counts: &RedrawCounts, elapsed: Duration) -> String {
    let mut text = String::new();
    for (name, value) in [
        ("elapsed_ms", elapsed.as_millis() as u64),
        ("frames", counts.frames),
        ("visible_redraws", counts.visible_redraws),
        ("hidden_redraws", counts.hidden_redraws),
        ("wake_locks", counts.wake_locks),
        (
            "wake_lock_wait_us",
            counts.wake_lock_wait.as_micros() as u64,
        ),
    ] {
        writeln!(text, "{name} {value}").expect("writing to a string does not fail");
    }
    text
}

/// Reads what [`file_text`] wrote.
pub fn parse(text: &str) -> Result<Written> {
    let mut written = Written::default();
    for line in text.lines() {
        let (name, value) = line
            .split_once(' ')
            .with_context(|| format!("not NAME VALUE: {line:?}"))?;
        let value: u64 = value
            .parse()
            .with_context(|| format!("not a number: {line:?}"))?;
        let counts = &mut written.counts;
        match name {
            "elapsed_ms" => written.elapsed = Duration::from_millis(value),
            "frames" => counts.frames = value,
            "visible_redraws" => counts.visible_redraws = value,
            "hidden_redraws" => counts.hidden_redraws = value,
            "wake_locks" => counts.wake_locks = value,
            "wake_lock_wait_us" => counts.wake_lock_wait = Duration::from_micros(value),
            _ => bail!("unknown count: {line:?}"),
        }
    }
    Ok(written)
}

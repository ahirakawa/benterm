//! The made-up program that stands in for a busy agent: it writes lines
//! at a fixed rate until its terminal goes away. `cargo xtask flood`
//! runs it.

use std::fmt;
use std::fs;
use std::io::{self, Write};
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

/// The bytes of a line, without its newline. Short enough for the
/// narrowest pane of the measurement not to wrap it: a line that wrapped
/// in one app and not in the other would be more rows to scroll there.
pub const LINE_WIDTH: usize = 24;

/// The highest rate, in lines a second, the writer takes.
pub const MAX_RATE: u32 = 100_000;

const NANOS_PER_SEC: u128 = 1_000_000_000;

/// When each line is due: line `n` at `n / rate` seconds after the start.
/// The times are counted from the start and not from the line before, so
/// a late line does not make the next ones late.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Schedule {
    rate: u32,
}

impl Schedule {
    /// A schedule of `rate` lines a second. `rate` is not 0.
    pub fn new(rate: u32) -> Self {
        assert!(rate > 0, "a rate of no lines has no schedule");
        Self { rate }
    }

    /// How long after the start line `number` is due.
    pub fn due(&self, number: u64) -> Duration {
        let nanos = u128::from(number) * NANOS_PER_SEC / u128::from(self.rate);
        Duration::new(
            (nanos / NANOS_PER_SEC) as u64,
            (nanos % NANOS_PER_SEC) as u32,
        )
    }

    /// The line to write when `next` is the first one not written and
    /// `elapsed` has passed since the start, and how many lines were
    /// skipped to get there. A writer that fell behind goes on with the
    /// line that is due now: writing the missed ones at once would make
    /// the rate jump.
    pub fn advance(&self, next: u64, elapsed: Duration) -> (u64, u64) {
        let due_now = (elapsed.as_nanos() * u128::from(self.rate) / NANOS_PER_SEC) as u64;
        if due_now > next {
            (due_now, due_now - next)
        } else {
            (next, 0)
        }
    }
}

/// Line `number` of pane `pane`: both numbers, then letters that differ
/// from line to line, so a screen that stopped moving shows.
pub fn line(pane: u32, number: u64) -> String {
    let mut line = format!("{:02} {:010} ", pane % 100, number % 10_000_000_000);
    let letters = (0..LINE_WIDTH - line.len())
        .map(|column| char::from(b'a' + ((number + column as u64) % 26) as u8));
    line.extend(letters);
    line
}

/// How far a writer has come.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Progress {
    /// Lines written.
    pub written: u64,
    /// Lines that were due while the terminal did not take any, and were
    /// left out.
    pub skipped: u64,
}

impl fmt::Display for Progress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "{} {}", self.written, self.skipped)
    }
}

/// How many writers left a file in `dir`, and what they wrote together.
/// A file is named after the writer's process and holds its progress as
/// of the last second.
pub fn read_progress(dir: &Path) -> io::Result<(usize, Progress)> {
    let mut writers = 0;
    let mut total = Progress::default();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        if entry.file_name().to_string_lossy().parse::<u32>().is_err() {
            continue;
        }
        writers += 1;
        let text = fs::read_to_string(entry.path())?;
        let mut numbers = text
            .split_whitespace()
            .map(|n| n.parse::<u64>().unwrap_or(0));
        total.written += numbers.next().unwrap_or(0);
        total.skipped += numbers.next().unwrap_or(0);
    }
    Ok((writers, total))
}

/// Writes lines to standard output at `rate` a second until writing
/// fails, which is how the end of the terminal shows. With `started_dir`,
/// first makes an empty file there named after this process, so whoever
/// started ten of these can count the ones that run, and once a second
/// writes its [`Progress`] to it.
pub fn run(rate: u32, pane: u32, started_dir: Option<&Path>) -> io::Result<()> {
    let file = started_dir.map(|dir| dir.join(std::process::id().to_string()));
    if let Some(file) = &file {
        fs::write(file, "")?;
    }
    let schedule = Schedule::new(rate);
    let start = Instant::now();
    let mut out = io::stdout().lock();
    let mut next = 0;
    let mut progress = Progress::default();
    let mut told = 0;
    loop {
        let (number, skipped) = schedule.advance(next, start.elapsed());
        progress.skipped += skipped;
        let seconds = start.elapsed().as_secs();
        if let Some(file) = &file
            && seconds > told
        {
            told = seconds;
            // Whoever counts may have removed the directory by now.
            let _ = fs::write(file, progress.to_string());
        }
        if let Some(wait) = schedule.due(number).checked_sub(start.elapsed()) {
            thread::sleep(wait);
        }
        if writeln!(out, "{}", line(pane, number))
            .and_then(|()| out.flush())
            .is_err()
        {
            return Ok(());
        }
        progress.written += 1;
        next = number + 1;
    }
}

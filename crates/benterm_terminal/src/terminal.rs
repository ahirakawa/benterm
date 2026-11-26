use crate::program_records::{ProgramRecord, ProgramRecords};
use std::collections::VecDeque;
use std::fmt;
use std::io;
use std::ops::Range;
use std::path::PathBuf;
use std::process::ExitStatus;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use benterm_core::{
    AlacrittyCore, Cell, Damage, GridSize, ReportedColors, Row, RowEnd, TerminalCore,
    TerminalEvent, is_history,
};
use benterm_log::Recorder;
use benterm_pty::{ForegroundProcess, PtyConfig, PtyEvent, PtySession, PtyWriter, WindowSize};
use parking_lot::{Condvar, FairMutex, FairMutexGuard, Mutex};

use crate::lock_meter::LockMeter;
use crate::screen::Screen;

/// The worker feeds at most this many bytes per lock hold, so that a
/// flood of output cannot keep the UI waiting for long.
const FEED_BATCH: usize = 1 << 20;

/// The worker gives the lock up once it has held it for this long.
/// Bytes alone do not bound the hold: some take the core far longer to
/// read than others, a repeat (`CSI n b`) most of all.
const FEED_TIME: Duration = Duration::from_millis(8);

/// The worker feeds this many bytes between two looks at the clock, so
/// that a hold ends at most this many bytes after its time is up.
const FEED_SLICE: usize = 1 << 10;

/// The most bytes that may be waiting to be written to a terminal's
/// program at once (4 MiB). Large enough for normal pastes and agent
/// messages, while bounding memory when a child stops reading its input.
pub const WRITE_QUEUE_LIMIT: usize = 4 << 20;

/// Why [`Terminal::write`] did not queue the bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WriteError {
    /// Too many bytes are already waiting to be written to the program,
    /// usually because it is not reading its input.
    Full,
    /// Nothing takes input any more: the writer thread has ended, which
    /// happens only after the program has exited.
    Closed,
}

impl fmt::Display for WriteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Full => f.write_str("terminal write queue is full"),
            Self::Closed => f.write_str("terminal write queue is closed"),
        }
    }
}

impl std::error::Error for WriteError {}

/// Where the writer thread writes bytes queued for the program.
trait WriteSink: Send + 'static {
    fn write(&mut self, bytes: &[u8]) -> io::Result<()>;
}

impl WriteSink for PtyWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        PtyWriter::write(self, bytes)
    }
}

/// Bounded queue of bytes waiting to be written to the program.
///
/// Counts every byte accepted and not yet finished writing (both queued
/// chunks and the chunk currently in `WriteSink::write`).
struct WriteQueue {
    limit: usize,
    state: Mutex<WriteQueueState>,
    wake: Condvar,
}

struct WriteQueueState {
    chunks: VecDeque<Vec<u8>>,
    /// Bytes accepted and not yet finished writing.
    pending: usize,
    /// Live [`WriteSender`] handles.
    senders: usize,
    /// Set once the writer thread stops taking writes.
    closed: bool,
}

struct WriteSender {
    queue: Arc<WriteQueue>,
}

struct WriteReceiver {
    queue: Arc<WriteQueue>,
}

fn write_queue(limit: usize) -> (WriteSender, WriteReceiver) {
    let queue = Arc::new(WriteQueue {
        limit,
        state: Mutex::new(WriteQueueState {
            chunks: VecDeque::new(),
            pending: 0,
            senders: 1,
            closed: false,
        }),
        wake: Condvar::new(),
    });
    (
        WriteSender {
            queue: Arc::clone(&queue),
        },
        WriteReceiver { queue },
    )
}

impl WriteSender {
    fn send(&self, bytes: Vec<u8>) -> Result<(), WriteError> {
        let mut state = self.queue.state.lock();
        if state.closed {
            return Err(WriteError::Closed);
        }
        if bytes.is_empty() {
            return Ok(());
        }
        let Some(next) = state.pending.checked_add(bytes.len()) else {
            return Err(WriteError::Full);
        };
        if next > self.queue.limit {
            return Err(WriteError::Full);
        }
        state.pending = next;
        state.chunks.push_back(bytes);
        self.queue.wake.notify_one();
        Ok(())
    }
}

impl Clone for WriteSender {
    fn clone(&self) -> Self {
        let mut state = self.queue.state.lock();
        state.senders += 1;
        drop(state);
        Self {
            queue: Arc::clone(&self.queue),
        }
    }
}

impl Drop for WriteSender {
    fn drop(&mut self) {
        let mut state = self.queue.state.lock();
        state.senders -= 1;
        if state.senders == 0 {
            self.queue.wake.notify_one();
        }
    }
}

impl WriteReceiver {
    fn recv(&self) -> Option<Vec<u8>> {
        let mut state = self.queue.state.lock();
        loop {
            if let Some(bytes) = state.chunks.pop_front() {
                return Some(bytes);
            }
            if state.senders == 0 {
                return None;
            }
            self.queue.wake.wait(&mut state);
        }
    }

    fn finish_write(&self, len: usize) {
        let mut state = self.queue.state.lock();
        state.pending = state.pending.saturating_sub(len);
    }

    fn close(&self) {
        let mut state = self.queue.state.lock();
        state.closed = true;
        state.chunks.clear();
        state.pending = 0;
    }
}

impl Drop for WriteReceiver {
    fn drop(&mut self) {
        self.close();
    }
}

/// What to run and how the terminal starts out.
pub struct TerminalConfig {
    /// The program. The rows and columns of its `size` are overwritten
    /// with `size` below; the size in pixels is kept.
    pub pty: PtyConfig,
    pub size: GridSize,
    /// The maximum number of rows kept above the screen.
    pub scrollback_limit: usize,
    /// The colors given to programs that query them; `None` leaves the
    /// queries unanswered.
    pub colors: Option<ReportedColors>,
    /// Where to log what the core is fed, if anywhere. The recorder must
    /// have been started with `size` as its initial size.
    pub log: Option<Recorder>,
}

/// A program running on a PTY together with the core that interprets its
/// output.
///
/// Three threads work for a terminal. The PTY's reader thread delivers
/// output. The worker thread feeds it to the core. The writer thread is
/// the only one that writes to the PTY, because that write blocks while
/// the program does not read its input. No method here blocks on the
/// program.
///
/// Dropping the terminal hangs up the program and joins the worker.
pub struct Terminal {
    shared: Arc<Shared>,
    /// `None` only while dropping.
    pty: Option<PtySession>,
    to_writer: WriteSender,
    /// Hands a worker that holds the output back the history, which
    /// lets the output through. `None` while dropping.
    held: Option<Sender<Vec<u8>>>,
    worker: Option<JoinHandle<()>>,
    /// Counts the locks the methods here take; see
    /// [`meter_locks`](Self::meter_locks).
    meter: Option<LockMeter>,
    /// Disconnects when the writer thread ends, for the tests to see an
    /// end that nothing joins.
    #[cfg(test)]
    writer_ended: Option<Receiver<()>>,
}

struct Shared {
    /// A fair mutex: under a flood of output the worker takes the lock
    /// again and again, and the UI must still get its turn.
    state: FairMutex<State>,
}

/// A notification the program asked for with OSC 9 or OSC 777.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notification {
    /// `None` for OSC 9, which carries only a body.
    pub title: Option<String>,
    pub body: String,
}

/// The end of a command, as the shell told it with OSC 133 ; D.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandEnd {
    /// `None` when the shell did not tell it.
    pub exit_code: Option<i32>,
}

/// The outcome of [`Terminal::sync_if_ready`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncOutcome {
    /// Nothing changed since the screen was last synced.
    Unchanged,
    /// The screen was updated with these damaged rows.
    Updated(Damage),
    /// The program is in the middle of a synchronized update (mode 2026) and
    /// has not exited, so the screen was left unchanged and the core's damage
    /// was preserved for the next sync.
    Deferred,
}

struct State {
    core: AlacrittyCore,
    title: Option<String>,
    /// The notifications not yet taken, oldest first.
    notifications: Vec<Notification>,
    /// The ends of commands not yet taken, oldest first.
    command_ends: Vec<CommandEnd>,
    /// The end of the last command, taken or not.
    last_command_end: Option<CommandEnd>,
    /// What the programs told of themselves with OSC 7501.
    programs: ProgramRecords,
    exit: Option<ExitStatus>,
    /// Gets every output and resize the core gets, in the same order,
    /// because both happen under this lock. Dropped after a failed write.
    log: Option<Recorder>,
    /// The output is still held back: what the terminal is to show
    /// first has not been fed yet.
    held: bool,
    /// Bumped whenever anything a `Screen` shows may have changed. Starts
    /// at 1, above a new `Screen`'s 0.
    generation: u64,
}

impl Terminal {
    /// Starts the program. `waker` is called from the worker thread after
    /// every change to what [`sync`](Self::sync) would report; it must
    /// return quickly and should only schedule a redraw.
    pub fn spawn(config: TerminalConfig, waker: impl Fn() + Send + 'static) -> io::Result<Self> {
        let terminal = Self::spawn_held(config, waker)?;
        terminal.release(Vec::new());
        Ok(terminal)
    }

    /// Starts the program like [`spawn`](Self::spawn), but feeds none of
    /// its output to the core until [`release`](Self::release) is
    /// called. The program runs meanwhile, and what it writes waits.
    ///
    /// This is how a terminal shows what an earlier one showed above
    /// the first output of its program, whenever that output arrives.
    pub fn spawn_held(
        config: TerminalConfig,
        waker: impl Fn() + Send + 'static,
    ) -> io::Result<Self> {
        let mut core = AlacrittyCore::new(config.size, config.scrollback_limit);
        if let Some(colors) = config.colors {
            core.set_reported_colors(colors);
        }
        let size = core.size();
        let pty_config = PtyConfig {
            size: WindowSize {
                rows: size.rows,
                cols: size.cols,
                ..config.pty.size
            },
            ..config.pty
        };
        let (pty, events) = PtySession::spawn(pty_config)?;
        let shared = Arc::new(Shared {
            state: FairMutex::new(State {
                core,
                title: None,
                notifications: Vec::new(),
                command_ends: Vec::new(),
                programs: ProgramRecords::default(),
                last_command_end: None,
                exit: None,
                log: config.log,
                held: true,
                generation: 1,
            }),
        });

        let (to_writer, writes) = write_queue(WRITE_QUEUE_LIMIT);
        let mut writer = pty.writer();
        #[cfg(test)]
        let (writer_ending, writer_ended) = mpsc::channel::<()>();
        // Not joined: see `Drop`.
        thread::Builder::new()
            .name("benterm-pty-writer".into())
            .spawn(move || {
                run_writer(&mut writer, &writes);
                #[cfg(test)]
                {
                    // The receiver goes first, so that `write` fails once
                    // the test sees the end.
                    drop((writer, writes));
                    drop(writer_ending);
                }
            })?;
        let (held, release) = mpsc::channel::<Vec<u8>>();
        let worker = {
            let shared = Arc::clone(&shared);
            let to_writer = to_writer.clone();
            thread::Builder::new()
                .name("benterm-terminal-worker".into())
                .spawn(move || {
                    // Ends without history when the terminal is dropped
                    // before it is released.
                    if let Ok(history) = release.recv()
                        && show_history(&shared, &history)
                    {
                        waker();
                    }
                    drop(release);
                    run_worker(&shared, &events, &to_writer, &waker);
                })?
        };

        Ok(Self {
            shared,
            pty: Some(pty),
            to_writer,
            held: Some(held),
            worker: Some(worker),
            meter: None,
            #[cfg(test)]
            writer_ended: Some(writer_ended),
        })
    }

    /// Has `meter` count every lock of the state the methods of the
    /// terminal take from now on, and how long each waited. The worker's
    /// locks are not counted.
    pub fn meter_locks(&mut self, meter: LockMeter) {
        self.meter = Some(meter);
    }

    /// Locks the state, through the meter if there is one.
    fn state(&self) -> FairMutexGuard<'_, State> {
        match &self.meter {
            None => self.shared.state.lock(),
            Some(meter) => meter.time(|| self.shared.state.lock()),
        }
    }

    /// Shows `history` and then lets the output of the program through,
    /// for a terminal started with [`spawn_held`](Self::spawn_held).
    /// Only the first call does anything.
    ///
    /// `history` is what [`benterm_core::history_bytes`] wrote for the
    /// rows of an earlier terminal. Bytes that are not such history are
    /// not shown: they could make the core answer the program or change
    /// a mode.
    pub fn release(&self, history: Vec<u8>) {
        if let Some(held) = &self.held {
            // Fails when the worker has been released already.
            let _ = held.send(history);
        }
    }

    /// What the terminal shows, to be shown again after a restart: the
    /// last `limit` rows of the scrollback and the screen together,
    /// oldest first, without the blank rows below the last row that has
    /// something on it.
    ///
    /// `None` while the alternate screen is shown: it is the screen of a
    /// program that will not be there after a restart, and the rows
    /// under it cannot be read meanwhile. What was taken before is the
    /// history to keep. `None` too before a terminal started with
    /// [`spawn_held`](Self::spawn_held) has shown what it was released
    /// with: its empty screen is not what it is about to show.
    pub fn history(&self, limit: usize) -> Option<Vec<Row>> {
        let state = self.state();
        let core = &state.core;
        if state.held || core.modes().alternate_screen {
            return None;
        }
        let above = core.scrollback_len();
        let mut rows = core.scrollback_rows(above.saturating_sub(limit)..above);
        rows.append(&mut core.screen_rows(0..core.size().rows));
        drop(state);
        let blank = Cell::default();
        let used = rows
            .iter()
            .rposition(|row| row.cells.iter().any(|cell| *cell != blank));
        rows.truncate(used.map_or(0, |last| last + 1));
        rows.drain(..rows.len().saturating_sub(limit));
        Some(rows)
    }

    /// A number that goes up whenever what [`history`](Self::history)
    /// gives may have changed.
    pub fn changes(&self) -> u64 {
        self.state().generation
    }

    /// Queues `bytes` as input for the program. Returns at once
    /// even if the program is not reading.
    ///
    /// Returns [`WriteError::Full`] when queuing `bytes` would take the
    /// unwritten bytes past [`WRITE_QUEUE_LIMIT`], or [`WriteError::Closed`]
    /// when the writer thread has ended (after the program has exited).
    pub fn write(&self, bytes: Vec<u8>) -> Result<(), WriteError> {
        self.to_writer.send(bytes)
    }

    /// Resizes the core and tells the program.
    ///
    /// Both happen under the lock, the core first, so output the program
    /// writes for the new size is never fed to a core of the old size.
    pub fn resize(&self, size: GridSize, pixels: (u16, u16)) {
        let mut state = self.state();
        state.core.resize(size);
        state.record(|log| log.resize(size));
        state.generation += 1;
        let size = state.core.size();
        if let Some(pty) = &self.pty {
            // Fails only for a closed descriptor, which `pty` rules out.
            let _ = pty.resize(WindowSize {
                rows: size.rows,
                cols: size.cols,
                pixel_width: pixels.0,
                pixel_height: pixels.1,
            });
        }
    }

    /// Brings `screen` up to date. Returns the rows it replaced, or
    /// `None` if nothing at all changed.
    ///
    /// Only rows that changed since the previous call are copied, so a
    /// terminal must always be synced into the same `Screen`.
    pub fn sync(&self, screen: &mut Screen) -> Option<Damage> {
        match self.sync_if_ready(screen, true) {
            SyncOutcome::Updated(damage) => Some(damage),
            SyncOutcome::Unchanged | SyncOutcome::Deferred => None,
        }
    }

    /// Like [`sync`](Self::sync), but defers updating `screen` while the
    /// program is inside a synchronized update (DEC private mode 2026) and has
    /// not exited, unless `force` is true.
    ///
    /// When deferred, the core's accumulated damage is not consumed so that
    /// all rows changed during the synchronized update are copied once it ends
    /// or `force` is passed.
    pub fn sync_if_ready(&self, screen: &mut Screen, force: bool) -> SyncOutcome {
        let mut state = self.state();
        if state.generation == screen.generation {
            return SyncOutcome::Unchanged;
        }
        if !force && state.core.modes().synchronized_update && state.exit.is_none() {
            return SyncOutcome::Deferred;
        }
        SyncOutcome::Updated(Self::apply_sync(&mut state, screen))
    }

    fn apply_sync(state: &mut State, screen: &mut Screen) -> Damage {
        let size = state.core.size();
        let damage = match state.core.take_damage() {
            Damage::Rows(rows) if screen.size == size => {
                for &row in &rows {
                    if let Some(fresh) = state.core.screen_rows(row..row + 1).pop() {
                        screen.rows[usize::from(row)] = fresh;
                    }
                }
                Damage::Rows(rows)
            }
            _ => {
                screen.rows = state.core.screen_rows(0..size.rows);
                Damage::Full
            }
        };
        screen.size = size;
        screen.cursor = state.core.cursor();
        screen.modes = state.core.modes();
        screen.scrollback_len = state.core.scrollback_len();
        screen.scrollback_evicted = state.core.scrollback_evicted();
        if screen.title != state.title {
            screen.title.clone_from(&state.title);
        }
        screen.exit = state.exit;
        screen.generation = state.generation;
        damage
    }

    /// The latest screen rows directly from the core, even while a
    /// synchronized update (mode 2026) is deferring [`Screen`] updates.
    pub fn screen_rows(&self) -> Vec<Row> {
        let state = self.state();
        state.core.screen_rows(0..state.core.size().rows)
    }

    /// The notifications the program asked for since the last call,
    /// oldest first. Each is given once. The waker is called after each
    /// one arrives.
    pub fn take_notifications(&self) -> Vec<Notification> {
        std::mem::take(&mut self.state().notifications)
    }

    /// The ends of commands the shell told since the last call, oldest
    /// first. Each is given once. The waker is called after each one
    /// arrives.
    pub fn take_command_ends(&self) -> Vec<CommandEnd> {
        std::mem::take(&mut self.state().command_ends)
    }

    /// The end of the last command the shell told, `None` before the
    /// first. The shell tells it only with a shell integration that
    /// prints OSC 133 ; D.
    pub fn last_command_end(&self) -> Option<CommandEnd> {
        self.state().last_command_end
    }

    /// What the program in the terminal told of itself last with OSC
    /// 7501, `None` when it told nothing or took it back. Parts of a
    /// program that report under an id of their own are left out.
    ///
    /// What a program was working on or blocked by is dropped when the
    /// program of the terminal ends, and when the shell tells the start
    /// of a prompt (OSC 133 ; A) or the end of a command (OSC 133 ; D),
    /// since the shell integration of Benterm tells only the end. What it
    /// finished, or failed at, stays. The waker is called after each
    /// change.
    pub fn program_status(&self) -> Option<ProgramRecord> {
        self.state().programs.root().cloned()
    }

    /// Rows above the screen, oldest first, like
    /// [`TerminalCore::scrollback_rows`].
    pub fn scrollback_rows(&self, range: Range<usize>) -> Vec<Row> {
        self.state().core.scrollback_rows(range)
    }

    /// The directory of the program the terminal started, usually a
    /// shell, with every symbolic link resolved. `None` once it has
    /// exited.
    ///
    /// The kernel is asked on each call, and the call waits for its
    /// answer. So a shell's `cd` shows here before the shell writes its
    /// next prompt. A program the shell runs in the foreground does not
    /// change this, whatever directory it moves to; restoring a session
    /// starts the shell again, so this is the directory to save. The
    /// kernel reports a removed directory as if it were still there, so
    /// the caller checks the directory before using it.
    pub fn working_directory(&self) -> Option<PathBuf> {
        self.pty.as_ref()?.working_directory()
    }

    /// The process of the program the terminal started. `None` once it
    /// has exited.
    pub fn process_id(&self) -> Option<u32> {
        Some(self.pty.as_ref()?.pid())
    }

    /// The directory of the program in front, the one that reads the
    /// keyboard: that of the started program while it is in front, as a
    /// shell at its prompt is, else that of the first process of the
    /// shell's foreground job. `None` if that process does not tell (see
    /// `PtySession::foreground_working_directory`). Asked and answered
    /// like [`working_directory`](Self::working_directory).
    pub fn foreground_working_directory(&self) -> Option<PathBuf> {
        self.pty.as_ref()?.foreground_working_directory()
    }

    /// The program in front, the one that reads the keyboard, with the
    /// arguments it was started with. `None` if its first process does
    /// not tell, or once nothing holds the terminal (see
    /// `PtySession::foreground_process`). Asked and answered like
    /// [`working_directory`](Self::working_directory).
    pub fn foreground_process(&self) -> Option<ForegroundProcess> {
        self.pty.as_ref()?.foreground_process()
    }

    /// Where a terminal opened beside this one starts: where the user
    /// is typing now.
    ///
    /// That is the directory of the program in front, so that a shell
    /// started from the shell is followed. If that program does not tell
    /// or its directory is gone, it is the directory of the program the
    /// terminal started, and `None` if that is gone too.
    pub fn directory_for_new_terminal(&self) -> Option<PathBuf> {
        let exists = |directory: &PathBuf| directory.is_dir();
        self.foreground_working_directory()
            .filter(exists)
            .or_else(|| self.working_directory().filter(exists))
    }

    /// The text the terminal shows, for a program that reads the pane.
    ///
    /// Without `lines` it is the screen. With a number it is the last
    /// rows of the scrollback and the screen together. Blank rows at the
    /// bottom are left out before the rows are counted, and blanks at the
    /// end of a line are dropped. A row that wraps is joined to the next
    /// one, so a long line comes out as one line.
    ///
    /// The rows are copied under one lock, so the scrollback and the
    /// screen belong to the same moment. What the screen does not show,
    /// as text in the color of its background, is blanks (see
    /// `Row::text_with`).
    pub fn text(&self, lines: Option<usize>) -> String {
        let (rows, colors) = {
            let state = self.state();
            let core = &state.core;
            let mut rows = core.screen_rows(0..core.size().rows);
            let shown = last_with_text(&rows);
            rows.truncate(shown);
            if let Some(lines) = lines {
                let mut history = core.scrollback_len();
                // Under a blank screen the blank rows go on into the
                // scrollback, where they are looked for a screen at a
                // time.
                while rows.is_empty() && history > 0 {
                    let start = history.saturating_sub(usize::from(core.size().rows).max(1));
                    history = start + last_with_text(&core.scrollback_rows(start..history));
                    if history > start {
                        break;
                    }
                }
                let above = lines.saturating_sub(rows.len()).min(history);
                rows.drain(..rows.len().saturating_sub(lines));
                if above > 0 {
                    let mut all = core.scrollback_rows(history - above..history);
                    all.append(&mut rows);
                    rows = all;
                }
            }
            (rows, core.reported_colors())
        };
        rows_text(&rows, colors.as_ref())
    }

    /// Answers later color queries of the program with `colors`.
    pub fn set_reported_colors(&self, colors: ReportedColors) {
        self.state().core.set_reported_colors(colors);
    }
}

/// How many of `rows` are left when the blank ones at the end are taken
/// off.
fn last_with_text(rows: &[Row]) -> usize {
    rows.iter()
        .rposition(|row| !is_blank(row))
        .map_or(0, |last| last + 1)
}

fn is_blank(row: &Row) -> bool {
    row.cells
        .iter()
        .all(|cell| cell.ch == ' ' && cell.zero_width.is_empty())
}

/// The rows as lines of text; see [`Terminal::text`].
fn rows_text(rows: &[Row], colors: Option<&ReportedColors>) -> String {
    let mut text = String::new();
    for (index, row) in rows.iter().enumerate() {
        let last = index + 1 == rows.len();
        let piece = match row.end {
            // The blank left before a wide character that went on to the
            // next row is not part of the text.
            RowEnd::WrappedBeforeWide => Row {
                cells: row.cells[..row.cells.len().saturating_sub(1)].to_vec(),
                end: row.end,
            }
            .text_with(colors),
            _ => row.text_with(colors),
        };
        if row.end.wraps() && !last {
            text.push_str(&piece);
            continue;
        }
        text.push_str(piece.trim_end_matches(' '));
        if !last {
            text.push('\n');
        }
    }
    text
}

impl Drop for Terminal {
    fn drop(&mut self) {
        self.take_shutdown()();
    }
}

impl Terminal {
    /// Transfers PTY and worker shutdown to the caller. Run the returned job
    /// exactly once, including when the terminal has not released its history.
    pub fn take_shutdown(&mut self) -> impl FnOnce() + Send + 'static {
        let pty = self.pty.take();
        let held = self.held.take();
        let worker = self.worker.take();
        move || {
            // Hangs up the program and joins the PTY's threads. The
            // exit event is delivered before the event channel closes, which
            // ends the worker.
            drop(pty);
            // A worker that still holds the output back goes
            // on to find the channel closed.
            drop(held);
            let _ = worker.map(JoinHandle::join);
            // The writer thread is left to end by itself. It ends when the
            // last sender is dropped or a write fails. If a background job
            // of the program still holds the terminal and does not read, a write can stay blocked until
            // that job ends; joining would hang.
        }
    }
}

impl State {
    /// Writes to the log, if there is one. A failed write stops the
    /// recording of this terminal only; the terminal keeps running.
    fn record(&mut self, write: impl FnOnce(&mut Recorder) -> io::Result<()>) {
        let Some(log) = &mut self.log else { return };
        if let Err(error) = write(log) {
            eprintln!(
                "benterm: stopped recording to {}: {error}",
                log.path().display()
            );
            self.log = None;
        }
    }
}

/// Feeds `history` to the core, before any output of the program.
///
/// Returns whether anything was fed.
fn show_history(shared: &Shared, history: &[u8]) -> bool {
    let mut state = shared.state.lock();
    state.held = false;
    if history.is_empty() || !is_history(history) {
        return false;
    }
    state.core.feed(history);
    // The log is what the core was fed, so that replaying it gives the
    // same screen.
    state.record(|log| log.output(history));
    state.generation += 1;
    true
}

/// Feeds `bytes[from..]` to the core a slice at a time, for as long as
/// `in_time` holds, and at least one slice. Returns where it stopped.
fn feed_for(state: &mut State, bytes: &[u8], from: usize, in_time: impl Fn() -> bool) -> usize {
    let mut to = from;
    while to < bytes.len() && (to == from || in_time()) {
        let end = bytes.len().min(to + FEED_SLICE);
        state.core.feed(&bytes[to..end]);
        to = end;
    }
    // The log is what the core was fed, in the order it was fed.
    state.record(|log| log.output(&bytes[from..to]));
    to
}

/// Body of the worker thread, once it feeds the output.
fn run_worker(
    shared: &Shared,
    events: &Receiver<PtyEvent>,
    to_writer: &WriteSender,
    waker: &dyn Fn(),
) {
    // Output that was not fed in full when the time of a lock hold was
    // up, and how many of its bytes were fed.
    let mut unfed: Option<(Vec<u8>, usize)> = None;
    loop {
        // The loop ends when the channel closes, which the
        // PTY session does right after the exit event.
        let mut next = None;
        if unfed.is_none() {
            match events.recv() {
                Ok(first) => next = Some(first),
                Err(_) => break,
            }
        }
        let mut state = shared.state.lock();
        let held_since = Instant::now();
        // Feed the output that has already arrived, up to the byte and time
        // limits for one lock acquisition.
        let mut fed = 0;
        let mut exited = None;
        loop {
            match next.take() {
                Some(PtyEvent::Output(bytes)) => unfed = Some((bytes, 0)),
                Some(PtyEvent::Exited(status)) => exited = Some(status),
                None => {}
            }
            if let Some((bytes, from)) = unfed.take() {
                let to = feed_for(&mut state, &bytes, from, || {
                    held_since.elapsed() < FEED_TIME
                });
                fed += to - from;
                if to < bytes.len() {
                    unfed = Some((bytes, to));
                    break;
                }
            }
            if fed >= FEED_BATCH || held_since.elapsed() >= FEED_TIME {
                break;
            }
            next = events.try_recv().ok();
            if next.is_none() {
                break;
            }
        }
        for event in state.core.take_events() {
            match event {
                TerminalEvent::TitleChanged(title) => state.title = Some(title),
                TerminalEvent::TitleReset => state.title = None,
                TerminalEvent::Notification { title, body } => {
                    state.notifications.push(Notification { title, body });
                }
                TerminalEvent::CommandFinished { exit_code } => {
                    let end = CommandEnd { exit_code };
                    state.command_ends.push(end);
                    state.last_command_end = Some(end);
                    state.programs.end_run();
                }
                TerminalEvent::PromptStarted => state.programs.end_run(),
                TerminalEvent::ProgramStatus(status) => state.programs.report(status),
                TerminalEvent::ProgramStatusCleared { id } => {
                    state.programs.clear(id.as_deref());
                }
            }
        }
        // After the events of the last output, which comes before the
        // exit: a program that tells what it is doing and ends at once
        // is not working any more.
        if let Some(status) = exited {
            state.exit = Some(status);
            state.programs.end_run();
        }
        let replies = state.core.take_pty_replies();
        if !replies.is_empty() {
            // `WriteSender::send` never blocks on I/O, so sending under
            // the lock keeps replies in query order without stalling the
            // UI. If the queue is full or closed, the reply is dropped.
            let _ = to_writer.send(replies);
        }
        state.generation += 1;
        drop(state);
        waker();
    }
}

/// Body of the writer thread.
fn run_writer(writer: &mut impl WriteSink, writes: &WriteReceiver) {
    while let Some(bytes) = writes.recv() {
        let len = bytes.len();
        if writer.write(&bytes).is_err() {
            // The program is gone; nothing can be delivered any more.
            writes.close();
            return;
        }
        writes.finish_write(len);
    }
}

#[cfg(test)]
mod tests {
    //! The writer thread is not joined, so these tests watch for its end
    //! through `Terminal::writer_ended`.

    use std::sync::mpsc::RecvTimeoutError;
    use std::time::{Duration, Instant};

    use super::*;

    /// Only an upper bound: each wait returns as soon as what it waits
    /// for has happened.
    const TIMEOUT: Duration = Duration::from_secs(10);

    fn start(args: &[&str], held: bool) -> (Terminal, Receiver<()>) {
        let config = TerminalConfig {
            pty: PtyConfig::command("/bin/sh", args),
            size: GridSize::new(6, 40),
            scrollback_limit: 100,
            colors: None,
            log: None,
        };
        let (wake_tx, wakes) = mpsc::channel();
        let waker = move || {
            let _ = wake_tx.send(());
        };
        let terminal = if held {
            Terminal::spawn_held(config, waker)
        } else {
            Terminal::spawn(config, waker)
        };
        (terminal.unwrap(), wakes)
    }

    /// Waits until `done` holds for what the terminal shows.
    fn wait_until(terminal: &Terminal, wakes: &Receiver<()>, done: impl Fn(&Screen) -> bool) {
        let deadline = Instant::now() + TIMEOUT;
        let mut screen = Screen::default();
        loop {
            terminal.sync(&mut screen);
            if done(&screen) {
                return;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            assert!(
                wakes.recv_timeout(left).is_ok(),
                "not before the timeout; screen: {:?}",
                terminal.text(None)
            );
        }
    }

    fn wait_for_writer_end(ended: &Receiver<()>) {
        match ended.recv_timeout(TIMEOUT) {
            Err(RecvTimeoutError::Disconnected) => {}
            other => panic!("the writer thread has not ended: {other:?}"),
        }
    }

    #[test]
    fn the_writer_thread_ends_after_the_terminal_is_dropped() {
        let (mut terminal, wakes) = start(&[], false);
        let ended = terminal.writer_ended.take().unwrap();
        assert_eq!(terminal.write(b"x".to_vec()), Ok(()));
        // The writer has written: the shell echoes what it read.
        wait_until(&terminal, &wakes, |screen| {
            screen.rows.iter().any(|row| row.text().contains('x'))
        });
        drop(terminal);
        wait_for_writer_end(&ended);
    }

    #[test]
    fn the_writer_thread_ends_after_a_held_terminal_is_dropped_unreleased() {
        let (mut terminal, _wakes) = start(&[], true);
        let ended = terminal.writer_ended.take().unwrap();
        drop(terminal);
        wait_for_writer_end(&ended);
    }

    #[test]
    fn the_writer_thread_ends_when_a_write_fails_after_the_program_exits() {
        let (mut terminal, wakes) = start(&["-c", "exit 0"], false);
        let ended = terminal.writer_ended.take().unwrap();
        wait_until(&terminal, &wakes, |screen| screen.exit.is_some());
        // Queued while the writer still waits; its write then fails.
        assert_eq!(terminal.write(b"x".to_vec()), Ok(()));
        wait_for_writer_end(&ended);
        assert_eq!(terminal.write(b"y".to_vec()), Err(WriteError::Closed));
    }

    /// Creates a terminal with a replaceable write sink and queue limit,
    /// without starting a real PTY or child program.
    fn with_writer(mut writer: impl WriteSink, limit: usize) -> Terminal {
        let shared = Arc::new(Shared {
            state: FairMutex::new(State {
                core: AlacrittyCore::new(GridSize::new(6, 40), 100),
                title: None,
                notifications: Vec::new(),
                command_ends: Vec::new(),
                programs: ProgramRecords::default(),
                last_command_end: None,
                exit: None,
                log: None,
                held: false,
                generation: 1,
            }),
        });
        let (to_writer, writes) = write_queue(limit);
        let (writer_ending, writer_ended) = mpsc::channel::<()>();
        thread::Builder::new()
            .name("benterm-test-writer".into())
            .spawn(move || {
                run_writer(&mut writer, &writes);
                drop((writer, writes));
                drop(writer_ending);
            })
            .unwrap();
        Terminal {
            shared,
            pty: None,
            to_writer,
            held: None,
            worker: None,
            meter: None,
            writer_ended: Some(writer_ended),
        }
    }

    /// A sink that blocks on a gate until released, recording every byte
    /// written once it is allowed to proceed.
    struct GateWriter {
        entered: Sender<usize>,
        release: Receiver<()>,
        written: Arc<parking_lot::Mutex<Vec<u8>>>,
        written_tx: Sender<usize>,
        fail: bool,
    }

    impl WriteSink for GateWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
            let _ = self.entered.send(bytes.len());
            // Wait until the test allows this write to finish (or closes the gate).
            let _ = self.release.recv();
            if self.fail {
                return Err(io::Error::new(io::ErrorKind::BrokenPipe, "closed"));
            }
            let mut written = self.written.lock();
            written.extend_from_slice(bytes);
            let total = written.len();
            drop(written);
            let _ = self.written_tx.send(total);
            Ok(())
        }
    }

    #[test]
    fn writes_past_the_limit_are_refused_until_the_sink_reads() {
        let (entered_tx, entered) = mpsc::channel();
        let (release_tx, release) = mpsc::channel();
        let (written_tx, written_rx) = mpsc::channel();
        let written = Arc::new(parking_lot::Mutex::new(Vec::new()));
        let sink = GateWriter {
            entered: entered_tx,
            release,
            written: Arc::clone(&written),
            written_tx,
            fail: false,
        };
        let mut terminal = with_writer(sink, 10);
        let ended = terminal.writer_ended.take().unwrap();

        // First write enters the sink and blocks there; its 6 bytes are
        // not yet finished writing.
        assert_eq!(terminal.write(b"abcdef".to_vec()), Ok(()));
        assert_eq!(entered.recv_timeout(TIMEOUT), Ok(6));

        // 6 + 5 > 10: refused without entering the queue.
        assert_eq!(terminal.write(b"12345".to_vec()), Err(WriteError::Full));
        // 6 + 4 == 10: fits exactly.
        assert_eq!(terminal.write(b"ghij".to_vec()), Ok(()));
        // 10 + 1 > 10: refused.
        assert_eq!(terminal.write(b"!".to_vec()), Err(WriteError::Full));

        // Let the sink finish writing the first chunk (6 bytes). Once the
        // second chunk (4 bytes) enters the sink, the first 6 bytes have
        // been accounted as finished, so 6 bytes of room are free again.
        release_tx.send(()).unwrap();
        assert_eq!(written_rx.recv_timeout(TIMEOUT), Ok(6));
        assert_eq!(entered.recv_timeout(TIMEOUT), Ok(4));

        // 4 + 6 == 10: now that the sink is reading, a new write succeeds.
        assert_eq!(terminal.write(b"klmnop".to_vec()), Ok(()));
        // 10 + 1 > 10: still bounded while the second chunk is in flight.
        assert_eq!(terminal.write(b"?".to_vec()), Err(WriteError::Full));

        // Drain the remaining chunks.
        release_tx.send(()).unwrap();
        assert_eq!(written_rx.recv_timeout(TIMEOUT), Ok(10));
        assert_eq!(entered.recv_timeout(TIMEOUT), Ok(6));
        release_tx.send(()).unwrap();
        assert_eq!(written_rx.recv_timeout(TIMEOUT), Ok(16));

        assert_eq!(*written.lock(), b"abcdefghijklmnop");
        drop(terminal);
        wait_for_writer_end(&ended);
    }

    #[test]
    fn a_single_write_larger_than_the_limit_is_refused() {
        let (entered_tx, _entered) = mpsc::channel();
        let (release_tx, release) = mpsc::channel();
        let (written_tx, written_rx) = mpsc::channel();
        let written = Arc::new(parking_lot::Mutex::new(Vec::new()));
        let sink = GateWriter {
            entered: entered_tx,
            release,
            written: Arc::clone(&written),
            written_tx,
            fail: false,
        };
        let mut terminal = with_writer(sink, 10);
        let ended = terminal.writer_ended.take().unwrap();

        assert_eq!(terminal.write(vec![b'x'; 11]), Err(WriteError::Full));
        // The rejected write did not consume capacity.
        assert_eq!(terminal.write(vec![b'y'; 10]), Ok(()));
        release_tx.send(()).unwrap();
        assert_eq!(written_rx.recv_timeout(TIMEOUT), Ok(10));
        assert_eq!(*written.lock(), vec![b'y'; 10]);

        drop(terminal);
        wait_for_writer_end(&ended);
    }

    #[test]
    fn writes_after_the_writer_closes_fail_as_closed_not_full() {
        let (entered_tx, entered) = mpsc::channel();
        let (release_tx, release) = mpsc::channel();
        let (written_tx, _written_rx) = mpsc::channel();
        let written = Arc::new(parking_lot::Mutex::new(Vec::new()));
        let sink = GateWriter {
            entered: entered_tx,
            release,
            written,
            written_tx,
            fail: true,
        };
        let mut terminal = with_writer(sink, 10);
        let ended = terminal.writer_ended.take().unwrap();

        assert_eq!(terminal.write(b"abcd".to_vec()), Ok(()));
        assert_eq!(entered.recv_timeout(TIMEOUT), Ok(4));
        release_tx.send(()).unwrap();
        wait_for_writer_end(&ended);

        assert_eq!(terminal.write(b"x".to_vec()), Err(WriteError::Closed));
        // Even a write larger than the limit reports Closed once the
        // writer is gone.
        assert_eq!(terminal.write(vec![b'x'; 100]), Err(WriteError::Closed));
    }
}

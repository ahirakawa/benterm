use std::ops::Range;

use crate::event::TerminalEvent;
use crate::screen::{Cursor, Damage, GridSize, Modes, ReportedColors, Row, Snapshot};

/// A terminal emulation core: bytes from the PTY go in, screen state comes
/// out.
///
/// The core never performs I/O. Replies to the program (answers to device
/// queries and the like) and events for the UI are queued inside the core and
/// handed over by [`take_pty_replies`](Self::take_pty_replies) and
/// [`take_events`](Self::take_events).
///
/// Two cores fed the same bytes and the same resizes must produce equal
/// [`Snapshot`]s. Anything that depends on wall-clock time therefore stays out
/// of the core.
pub trait TerminalCore {
    /// Processes bytes written by the program running in the terminal.
    ///
    /// Any byte sequence is accepted, including invalid UTF-8 and escape
    /// sequences split across calls.
    fn feed(&mut self, bytes: &[u8]);

    /// Changes the grid size. Dimensions below [`GridSize::MIN`] are raised
    /// to it.
    fn resize(&mut self, size: GridSize);

    fn size(&self) -> GridSize;

    /// The visible screen, the cursor and the modes as a comparable value.
    fn snapshot(&self) -> Snapshot;

    /// Which screen rows may have changed since the last call, and forgets
    /// them. A renderer that keeps its own copy of the screen re-reads only
    /// these rows with [`screen_rows`](Self::screen_rows) instead of taking a
    /// [`snapshot`](Self::snapshot) for every frame.
    ///
    /// Damage is about cells only. The cursor, the modes and the scrollback
    /// length are cheap to read and are not tracked.
    fn take_damage(&mut self) -> Damage;

    /// Rows of the screen, top to bottom; equal to the same range of
    /// `snapshot().rows`. The range is clamped to the rows that exist.
    fn screen_rows(&self, range: Range<u16>) -> Vec<Row>;

    /// The cursor. Equal to `snapshot().cursor` but cheap.
    fn cursor(&self) -> Cursor;

    /// Number of rows that have scrolled off the top of the screen.
    fn scrollback_len(&self) -> usize;

    /// Cumulative number of rows dropped from the oldest end of the scrollback,
    /// either because the scrollback reached its limit or because it was
    /// cleared.
    fn scrollback_evicted(&self) -> u64;

    /// Rows of the scrollback, oldest first. Index 0 is the oldest row and
    /// `scrollback_len() - 1` is the row just above the screen. The range is
    /// clamped to the rows that exist.
    fn scrollback_rows(&self, range: Range<usize>) -> Vec<Row>;

    /// The modes that input handling depends on. Equal to `snapshot().modes`
    /// but cheap.
    fn modes(&self) -> Modes;

    /// Removes and returns the events produced since the last call, in the
    /// order they occurred.
    fn take_events(&mut self) -> Vec<TerminalEvent>;

    /// Removes and returns the bytes the terminal wants to send back to the
    /// program, such as answers to device status queries. The caller writes
    /// them to the PTY.
    fn take_pty_replies(&mut self) -> Vec<u8>;

    /// Sets the colors given to programs that ask for them (OSC 4, 10, 11
    /// and 12). The answers go out with the other PTY replies, in the order
    /// of the queries. Until this is called, color queries get no answer.
    ///
    /// The colors come from the theme, which the core does not know. They do
    /// not affect the screen state.
    fn set_reported_colors(&mut self, colors: ReportedColors);
}

use std::collections::HashMap;
use std::ops::Range;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::{Dimensions, Scroll as AlacrittyScroll};
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::{Cell as AlacrittyCell, Flags};
use alacritty_terminal::term::{Config, Term, TermDamage, TermMode};
use alacritty_terminal::vte::ansi::{
    Color as AlacrittyColor, CursorShape as AlacrittyCursorShape, Handler, NamedColor, Processor,
    Rgb as AlacrittyRgb, Timeout,
};
use unicode_width::UnicodeWidthChar;

use crate::core::TerminalCore;
use crate::event::TerminalEvent;
use crate::scan::{OscLimit, Scanner, Stop, TITLE_CHARS, cut_to_chars};
use crate::screen::{
    Attributes, Cell, CellWidth, Color, Cursor, CursorShape, Damage, GridSize, KeyboardFlags,
    Modes, MouseEncoding, MouseTracking, ReportedColors, Rgb, Row, RowEnd, Snapshot, Underline,
};

/// The control character that cancels an escape sequence.
const CAN: u8 = 0x18;

/// The most combining characters one cell keeps. alacritty adds every
/// one it reads to the cell, and each copy of the cell for the UI copies
/// them all. No writing system stacks nearly this many on one character.
const ZERO_WIDTH_CHARS: usize = 32;

/// The browser rejects addresses longer than this (see its `is_page_url`).
const MAX_HYPERLINK_BYTES: usize = 8192;

type Hyperlinks = HashMap<*const u8, Arc<str>>;

/// [`TerminalCore`] backed by `alacritty_terminal`.
pub struct AlacrittyCore {
    term: Term<Outbox>,
    parser: Processor<NoSyncTimeout>,
    scanner: Scanner,
    osc_limit: OscLimit,
    outbox: Outbox,
    /// The start of a multi-byte character whose remaining bytes have not
    /// arrived yet. At most three bytes.
    utf8_carry: Vec<u8>,
    size: GridSize,
    /// Maximum number of scrollback rows configured for this core.
    scrollback_limit: usize,
    /// Cumulative number of rows evicted from the oldest end of scrollback.
    scrollback_evicted: u64,
    /// Scrollback length of the primary screen while the alternate screen is
    /// active, so a full reset (`ESC c`) on the alternate screen still counts
    /// the cleared primary scrollback rows.
    primary_scrollback_len: usize,
    /// Current vertical scrolling region (`0..rows` by default), used when
    /// `scrollback_limit` is too small for `display_offset` to stay unclamped.
    scroll_region: Range<usize>,
    /// Something was fed or resized since the last `take_damage`.
    touched: bool,
}

impl AlacrittyCore {
    /// The colors last given with
    /// [`set_reported_colors`](TerminalCore::set_reported_colors), which
    /// are the colors the screen is drawn in.
    pub fn reported_colors(&self) -> Option<ReportedColors> {
        self.outbox.lock().colors.clone()
    }

    /// `scrollback_limit` counts rows above the screen.
    pub fn new(size: GridSize, scrollback_limit: usize) -> Self {
        let size = size.at_least_min();
        let config = Config {
            scrolling_history: scrollback_limit,
            // Claude Code and Codex need it to tell shift-enter from enter.
            kitty_keyboard: true,
            ..Config::default()
        };
        let outbox = Outbox::default();
        Self {
            term: Term::new(config, &Dims(size), outbox.clone()),
            parser: Processor::new(),
            scanner: Scanner::new(scrollback_limit > 0),
            osc_limit: OscLimit::default(),
            outbox,
            utf8_carry: Vec::new(),
            size,
            scrollback_limit,
            scrollback_evicted: 0,
            primary_scrollback_len: 0,
            scroll_region: 0..usize::from(size.rows),
            touched: true,
        }
    }

    /// Number of screen rows from the top through the last non-blank row,
    /// which is how many rows `alacritty_terminal` scrolls into scrollback on
    /// `CSI 2 J`.
    fn non_blank_screen_prefix(&self) -> usize {
        let grid = self.term.grid();
        let rows = usize::from(self.size.rows);
        let cols = usize::from(self.size.cols);
        for row in (0..rows).rev() {
            let line = Line(row as i32);
            let has_content = (0..cols).any(|col| {
                let cell = &grid[line][Column(col)];
                cell.c != ' ' || cell.zerowidth().is_some()
            });
            if has_content {
                return row + 1;
            }
        }
        0
    }

    /// Feeds `bytes` like [`advance_unsplit`](Self::advance_unsplit),
    /// and keeps the combining characters in them from piling up on a
    /// cell.
    ///
    /// The bytes are fed up to the end of each run of combining
    /// characters, where the cell alacritty added the run to is still the
    /// one before the cursor, and that cell is cut. If the run was not
    /// text but part of a sequence, no cell took it and none is cut.
    fn advance_chunk(&mut self, bytes: &[u8]) {
        if bytes.is_ascii() {
            self.advance_unsplit(bytes);
            return;
        }
        let mut fed = 0;
        for end in zero_width_run_ends(bytes) {
            self.advance_unsplit(&bytes[fed..end]);
            self.cut_zero_width();
            fed = end;
        }
        self.advance_unsplit(&bytes[fed..]);
    }

    /// Cuts the combining characters of the cell that alacritty adds
    /// them to (the one before the cursor) to [`ZERO_WIDTH_CHARS`].
    fn cut_zero_width(&mut self) {
        let grid = self.term.grid_mut();
        let line = grid.cursor.point.line;
        // Mirrors how alacritty picks the cell.
        let mut column = grid.cursor.point.column;
        if !grid.cursor.input_needs_wrap {
            column.0 = column.0.saturating_sub(1);
        }
        if grid[line][column].flags.contains(Flags::WIDE_CHAR_SPACER) {
            column.0 = column.0.saturating_sub(1);
        }
        let cell = &mut grid[line][column];
        let Some(marks) = cell.zerowidth().filter(|m| m.len() > ZERO_WIDTH_CHARS) else {
            return;
        };
        // A cell cannot give up its marks, so it is built again.
        let mut cut = AlacrittyCell {
            c: cell.c,
            fg: cell.fg,
            bg: cell.bg,
            flags: cell.flags,
            extra: None,
        };
        cut.set_underline_color(cell.underline_color());
        cut.set_hyperlink(cell.hyperlink());
        for &mark in &marks[..ZERO_WIDTH_CHARS] {
            cut.push_zerowidth(mark);
        }
        *cell = cut;
    }

    /// Feeds `bytes` (containing no scrollback clear, screen switch, or
    /// multi-line CSI scroll) and adds any scrollback evictions to
    /// `self.scrollback_evicted`.
    fn advance_unsplit(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        if self.scrollback_limit == 0 || self.term.mode().contains(TermMode::ALT_SCREEN) {
            self.parser.advance(&mut self.term, bytes);
            return;
        }
        if self.scrollback_limit >= 2 {
            // A slice of at most `scrollback_limit - 1` bytes with no multi-line
            // CSI scroll can scroll at most `scrollback_limit - 1` lines, so
            // `display_offset` (starting at 1) can never clamp at `scrollback_limit`.
            let max_step = self.scrollback_limit - 1;
            let mut remaining = bytes;
            while !remaining.is_empty() {
                let mut take = remaining.len().min(max_step);
                let tail = incomplete_utf8_tail(&remaining[..take]);
                if tail < take {
                    take -= tail;
                } else {
                    take = next_utf8_boundary(remaining, take);
                }
                let (chunk, rest) = remaining.split_at(take);
                remaining = rest;
                let before_hist = self.term.grid().history_size();
                if before_hist == 0 {
                    self.parser.advance(&mut self.term, chunk);
                } else {
                    self.term
                        .grid_mut()
                        .scroll_display(AlacrittyScroll::Delta(1));
                    self.parser.advance(&mut self.term, chunk);
                    let pushed = self.term.grid().display_offset().saturating_sub(1);
                    self.term.grid_mut().scroll_display(AlacrittyScroll::Bottom);
                    self.scrollback_evicted +=
                        (before_hist + pushed).saturating_sub(self.scrollback_limit) as u64;
                }
            }
            return;
        }
        // `scrollback_limit == 1`: `display_offset` cannot exceed 1, so step
        // one character at a time and check whether a single-line scroll
        // occurred at the bottom of a top-anchored scroll region.
        let mut remaining = bytes;
        let mut prev_esc = false;
        while !remaining.is_empty() {
            let take = next_utf8_boundary(remaining, 1);
            let (chunk, rest) = remaining.split_at(take);
            remaining = rest;
            let before_hist = self.term.grid().history_size();
            if before_hist == 0 {
                self.parser.advance(&mut self.term, chunk);
            } else {
                let before_cursor = self.term.grid().cursor.point;
                let before_wrap = self.term.grid().cursor.input_needs_wrap;
                let line_wrap = self.term.mode().contains(TermMode::LINE_WRAP);
                self.parser.advance(&mut self.term, chunk);
                let after_cursor = self.term.grid().cursor.point;
                if self.scroll_region.start == 0
                    && (before_cursor.line.0 as usize) + 1 == self.scroll_region.end
                {
                    let is_linefeed = matches!(chunk, [0x0a..=0x0c])
                        || (prev_esc && matches!(chunk, [b'D' | b'E']));
                    let wrapped = line_wrap
                        && (before_wrap
                            || before_cursor.column.0 + 1 == usize::from(self.size.cols))
                        && after_cursor.column.0 < before_cursor.column.0;
                    if is_linefeed || wrapped {
                        self.scrollback_evicted += 1;
                    }
                }
            }
            prev_esc = chunk == [0x1b];
        }
    }

    /// Feeds `bytes` to the parsers, except for a multi-byte character cut
    /// off at the end, which is kept for the next call.
    ///
    /// vte 0.15 mishandles a character split across two calls: when invalid
    /// UTF-8 follows shortly after it, the bytes in between are skipped.
    /// Never handing vte a split character keeps the result independent of
    /// how the input is cut.
    fn feed_whole_chars(&mut self, bytes: &[u8]) {
        let (mut rest, tail) = bytes.split_at(bytes.len() - incomplete_utf8_tail(bytes));
        self.utf8_carry.extend_from_slice(tail);

        while !rest.is_empty() {
            let (read, stop) = self.scanner.scan(rest);
            match stop {
                Some(Stop::OverflowingPush) => {
                    // CAN in place of the push's final byte cancels it.
                    self.advance_chunk(&rest[..read - 1]);
                    self.advance_chunk(&[CAN]);
                }
                Some(Stop::Event(event)) => {
                    self.advance_chunk(&rest[..read]);
                    self.outbox.lock().events.push(event);
                }
                Some(Stop::Reply(reply)) => {
                    // After the bytes before it, whose answers are
                    // queued as they are parsed.
                    self.advance_chunk(&rest[..read]);
                    self.outbox.lock().pty_replies.extend_from_slice(reply);
                }
                Some(Stop::FullReset) => {
                    self.advance_chunk(&rest[..read - 1]);
                    let was_alt = self.term.mode().contains(TermMode::ALT_SCREEN);
                    let before_hist = if was_alt {
                        self.primary_scrollback_len
                    } else {
                        self.term.grid().history_size()
                    };
                    self.parser.advance(&mut self.term, &rest[read - 1..read]);
                    let after_hist = self.term.grid().history_size();
                    self.scrollback_evicted += before_hist.saturating_sub(after_hist) as u64;
                    self.primary_scrollback_len = 0;
                    self.scroll_region = 0..usize::from(self.size.rows);
                    self.outbox
                        .lock()
                        .events
                        .push(TerminalEvent::ProgramStatusCleared { id: None });
                }
                Some(Stop::ClearScrollback) => {
                    self.advance_chunk(&rest[..read - 1]);
                    let before_alt = self.term.mode().contains(TermMode::ALT_SCREEN);
                    let before_hist = self.term.grid().history_size();
                    self.parser.advance(&mut self.term, &rest[read - 1..read]);
                    if !before_alt {
                        let after_hist = self.term.grid().history_size();
                        self.scrollback_evicted += before_hist.saturating_sub(after_hist) as u64;
                    }
                }
                Some(Stop::ClearScreen) => {
                    self.advance_chunk(&rest[..read - 1]);
                    let alt = self.term.mode().contains(TermMode::ALT_SCREEN);
                    if alt || self.scrollback_limit == 0 {
                        self.parser.advance(&mut self.term, &rest[read - 1..read]);
                    } else {
                        let before_hist = self.term.grid().history_size();
                        let fallback_pushed = self.non_blank_screen_prefix();
                        let pushed = if self.scrollback_limit >= 2 && before_hist > 0 {
                            self.term
                                .grid_mut()
                                .scroll_display(AlacrittyScroll::Delta(1));
                            self.parser.advance(&mut self.term, &rest[read - 1..read]);
                            let after_offset = self.term.grid().display_offset();
                            self.term.grid_mut().scroll_display(AlacrittyScroll::Bottom);
                            if after_offset < self.scrollback_limit {
                                after_offset.saturating_sub(1)
                            } else {
                                fallback_pushed
                            }
                        } else {
                            self.parser.advance(&mut self.term, &rest[read - 1..read]);
                            fallback_pushed
                        };
                        self.scrollback_evicted +=
                            (before_hist + pushed).saturating_sub(self.scrollback_limit) as u64;
                    }
                }
                Some(Stop::ScrollLines { delete, count }) => {
                    self.advance_chunk(&rest[..read - 1]);
                    let alt = self.term.mode().contains(TermMode::ALT_SCREEN);
                    if alt || self.scrollback_limit == 0 {
                        self.parser.advance(&mut self.term, &rest[read - 1..read]);
                    } else {
                        let before_hist = self.term.grid().history_size();
                        let cursor_line = self.term.grid().cursor.point.line.0 as usize;
                        let fallback_pushed = if self.scroll_region.start == 0
                            && (!delete
                                || (cursor_line == 0 && self.scroll_region.contains(&cursor_line)))
                        {
                            count.min(self.scroll_region.end)
                        } else {
                            0
                        };
                        let pushed = if self.scrollback_limit >= 2 && before_hist > 0 {
                            self.term
                                .grid_mut()
                                .scroll_display(AlacrittyScroll::Delta(1));
                            self.parser.advance(&mut self.term, &rest[read - 1..read]);
                            let after_offset = self.term.grid().display_offset();
                            self.term.grid_mut().scroll_display(AlacrittyScroll::Bottom);
                            if after_offset < self.scrollback_limit {
                                after_offset.saturating_sub(1)
                            } else {
                                fallback_pushed
                            }
                        } else {
                            self.parser.advance(&mut self.term, &rest[read - 1..read]);
                            fallback_pushed
                        };
                        self.scrollback_evicted +=
                            (before_hist + pushed).saturating_sub(self.scrollback_limit) as u64;
                    }
                }
                Some(Stop::ScrollingRegion { top, bottom }) => {
                    self.advance_chunk(&rest[..read - 1]);
                    self.parser.advance(&mut self.term, &rest[read - 1..read]);
                    let rows = usize::from(self.size.rows);
                    let start = top.saturating_sub(1);
                    let end = bottom.unwrap_or(rows).min(rows);
                    if start < end {
                        self.scroll_region = start..end;
                    }
                }
                Some(Stop::AltScreen) => {
                    self.advance_chunk(&rest[..read - 1]);
                    let before_alt = self.term.mode().contains(TermMode::ALT_SCREEN);
                    let before_hist = self.term.grid().history_size();
                    self.parser.advance(&mut self.term, &rest[read - 1..read]);
                    let after_alt = self.term.mode().contains(TermMode::ALT_SCREEN);
                    if !before_alt && after_alt {
                        self.primary_scrollback_len = before_hist;
                        self.scroll_region = 0..usize::from(self.size.rows);
                    } else if before_alt && !after_alt {
                        self.primary_scrollback_len = 0;
                        self.scroll_region = 0..usize::from(self.size.rows);
                    }
                }
                Some(Stop::Repeat { count }) => {
                    // More of one character than the screen has cells
                    // shows nothing more, and takes alacritty as long as
                    // writing each of them does.
                    let cells = usize::from(self.size.rows) * usize::from(self.size.cols);
                    if count > cells {
                        // CAN in place of the final byte cancels the
                        // repeat. It does not change which character a
                        // repeat writes.
                        self.advance_chunk(&rest[..read - 1]);
                        self.advance_chunk(&[CAN]);
                        self.advance_chunk(format!("\x1b[{cells}b").as_bytes());
                    } else {
                        self.advance_chunk(&rest[..read]);
                    }
                    // The character may be a combining one.
                    self.cut_zero_width();
                }
                Some(Stop::LongTitle(title)) => {
                    self.advance_chunk(&rest[..read]);
                    // alacritty now holds the whole title, and puts a
                    // copy of it on its stack at each `CSI 22 t`. The
                    // event it sent was cut to the same text and is sent
                    // again by setting the title.
                    let mut pending = self.outbox.lock();
                    if matches!(pending.events.last(), Some(TerminalEvent::TitleChanged(_))) {
                        pending.events.pop();
                    }
                    drop(pending);
                    self.term.set_title(Some(title));
                }
                None => self.advance_chunk(&rest[..read]),
            }
            rest = &rest[read..];
        }
    }

    /// `rows` and, for each of them, the row above it if that row wraps.
    ///
    /// Writing over a wide character at the start of a row takes away the
    /// blank left before it at the end of the row above, which changes how
    /// that row ends. alacritty does not damage the row above for it.
    fn with_rows_above_that_wrap(&self, rows: Vec<u16>) -> Vec<u16> {
        let last_col = Column(usize::from(self.size.cols) - 1);
        let wraps = |row: u16| {
            self.term.grid()[Line(i32::from(row))][last_col]
                .flags
                .contains(Flags::WRAPLINE)
        };
        let mut all = Vec::with_capacity(rows.len());
        for row in rows {
            if let Some(above) = row.checked_sub(1)
                && all.last() != Some(&above)
                && wraps(above)
            {
                all.push(above);
            }
            all.push(row);
        }
        all
    }

    fn row(&self, line: Line, links: &mut Hyperlinks) -> Row {
        let grid = self.term.grid();
        let mut cells: Vec<Cell> = (0..usize::from(self.size.cols))
            .map(|col| convert_cell(&grid[line][Column(col)], links))
            .collect();
        blank_broken_wide_chars(&mut cells);
        // alacritty marks a wrap on the last cell of the row.
        let last = grid[line][Column(usize::from(self.size.cols) - 1)].flags;
        let end = if !last.contains(Flags::WRAPLINE) {
            RowEnd::LineEnd
        } else if last.contains(Flags::LEADING_WIDE_CHAR_SPACER) {
            RowEnd::WrappedBeforeWide
        } else {
            RowEnd::Wrapped
        };
        Row { cells, end }
    }
}

impl TerminalCore for AlacrittyCore {
    fn feed(&mut self, mut bytes: &[u8]) {
        self.touched = true;
        while !bytes.is_empty() {
            // What an OSC has beyond its limit reaches neither parser.
            let (keep, skip) = self.osc_limit.split(bytes);
            let kept = &bytes[..keep];
            if self.utf8_carry.is_empty() {
                self.feed_whole_chars(kept);
            } else if !kept.is_empty() {
                let mut joined = std::mem::take(&mut self.utf8_carry);
                joined.extend_from_slice(kept);
                self.feed_whole_chars(&joined);
            }
            bytes = &bytes[keep + skip..];
        }
    }

    fn resize(&mut self, size: GridSize) {
        self.touched = true;
        self.size = size.at_least_min();
        self.term.resize(Dims(self.size));
        self.scroll_region = 0..usize::from(self.size.rows);
    }

    fn size(&self) -> GridSize {
        self.size
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            size: self.size,
            rows: self.screen_rows(0..self.size.rows),
            cursor: self.cursor(),
            modes: self.modes(),
            scrollback_len: self.scrollback_len(),
        }
    }

    fn take_damage(&mut self) -> Damage {
        // alacritty always reports the cursor's row, even when nothing
        // happened, so an untouched core is answered without asking it.
        if !std::mem::take(&mut self.touched) {
            return Damage::Rows(Vec::new());
        }
        let damage = match self.term.damage() {
            TermDamage::Full => Damage::Full,
            // Lines come in ascending order, one entry per line.
            TermDamage::Partial(lines) => {
                let rows: Vec<u16> = lines.map(|l| l.line as u16).collect();
                Damage::Rows(self.with_rows_above_that_wrap(rows))
            }
        };
        self.term.reset_damage();
        damage
    }

    fn screen_rows(&self, range: Range<u16>) -> Vec<Row> {
        let end = range.end.min(self.size.rows);
        let start = range.start.min(end);
        let mut links = Hyperlinks::new();
        (start..end)
            .map(|row| self.row(Line(i32::from(row)), &mut links))
            .collect()
    }

    fn cursor(&self) -> Cursor {
        let point = self.term.grid().cursor.point;
        let style = self.term.cursor_style();
        Cursor {
            row: point.line.0 as u16,
            col: point.column.0 as u16,
            shape: match style.shape {
                AlacrittyCursorShape::Underline => CursorShape::Underline,
                AlacrittyCursorShape::Beam => CursorShape::Bar,
                // The remaining shapes only occur in alacritty's vi mode,
                // which this core never enters.
                _ => CursorShape::Block,
            },
            blinking: style.blinking,
            visible: self.term.mode().contains(TermMode::SHOW_CURSOR),
        }
    }

    fn scrollback_len(&self) -> usize {
        self.term.grid().history_size()
    }

    fn scrollback_evicted(&self) -> u64 {
        self.scrollback_evicted
    }

    fn scrollback_rows(&self, range: Range<usize>) -> Vec<Row> {
        let len = self.scrollback_len();
        let end = range.end.min(len);
        let start = range.start.min(end);
        // alacritty numbers history lines -1, -2, ... going up from the
        // screen, so the oldest row is line `-len`.
        let mut links = Hyperlinks::new();
        (start..end)
            .map(|index| self.row(Line(index as i32 - len as i32), &mut links))
            .collect()
    }

    fn modes(&self) -> Modes {
        let mode = self.term.mode();
        let mouse_tracking = if mode.contains(TermMode::MOUSE_MOTION) {
            MouseTracking::Motion
        } else if mode.contains(TermMode::MOUSE_DRAG) {
            MouseTracking::Drag
        } else if mode.contains(TermMode::MOUSE_REPORT_CLICK) {
            MouseTracking::Click
        } else {
            MouseTracking::Off
        };
        let mouse_encoding = if mode.contains(TermMode::SGR_MOUSE) {
            MouseEncoding::Sgr
        } else if mode.contains(TermMode::UTF8_MOUSE) {
            MouseEncoding::Utf8
        } else {
            MouseEncoding::Default
        };
        Modes {
            bracketed_paste: mode.contains(TermMode::BRACKETED_PASTE),
            app_cursor_keys: mode.contains(TermMode::APP_CURSOR),
            mouse_tracking,
            mouse_encoding,
            alternate_screen: mode.contains(TermMode::ALT_SCREEN),
            synchronized_update: self.scanner.synchronized_update(),
            keyboard: KeyboardFlags {
                disambiguate: mode.contains(TermMode::DISAMBIGUATE_ESC_CODES),
                report_event_types: mode.contains(TermMode::REPORT_EVENT_TYPES),
                report_alternate_keys: mode.contains(TermMode::REPORT_ALTERNATE_KEYS),
                report_all_keys_as_escapes: mode.contains(TermMode::REPORT_ALL_KEYS_AS_ESC),
                report_associated_text: mode.contains(TermMode::REPORT_ASSOCIATED_TEXT),
            },
        }
    }

    fn set_reported_colors(&mut self, colors: ReportedColors) {
        self.outbox.lock().colors = Some(colors);
    }

    fn take_events(&mut self) -> Vec<TerminalEvent> {
        std::mem::take(&mut self.outbox.lock().events)
    }

    fn take_pty_replies(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.outbox.lock().pty_replies)
    }
}

/// Length of the unfinished multi-byte character at the end of `bytes`, or
/// zero if `bytes` ends at a character boundary or in invalid UTF-8.
fn incomplete_utf8_tail(bytes: &[u8]) -> usize {
    (1..=bytes.len().min(3))
        .find(|len| {
            let tail = &bytes[bytes.len() - len..];
            std::str::from_utf8(tail)
                .is_err_and(|error| error.valid_up_to() == 0 && error.error_len().is_none())
        })
        .unwrap_or(0)
}

/// Smallest index `>= at` in `bytes` that does not split a UTF-8 continuation
/// sequence.
fn next_utf8_boundary(bytes: &[u8], at: usize) -> usize {
    let mut index = at.min(bytes.len());
    while index < bytes.len() && (bytes[index] & 0xc0) == 0x80 {
        index += 1;
    }
    index
}

/// Blanks any half of a wide character whose other half is missing.
///
/// alacritty leaves one half in place when the other is deleted or pushed
/// out of the row. Callers are promised that the halves come as a pair.
fn blank_broken_wide_chars(cells: &mut [Cell]) {
    for index in 0..cells.len() {
        let width_of = |index: usize| cells.get(index).map(|cell| cell.width);
        let paired = match cells[index].width {
            CellWidth::Narrow => true,
            CellWidth::Wide => width_of(index + 1) == Some(CellWidth::WideSpacer),
            CellWidth::WideSpacer => {
                index.checked_sub(1).and_then(width_of) == Some(CellWidth::Wide)
            }
        };
        if !paired {
            let cell = &mut cells[index];
            cell.ch = ' ';
            cell.zero_width.clear();
            cell.width = CellWidth::Narrow;
        }
    }
}

fn convert_cell(cell: &AlacrittyCell, links: &mut Hyperlinks) -> Cell {
    let flags = cell.flags;
    let width = if flags.contains(Flags::WIDE_CHAR) {
        CellWidth::Wide
    } else if flags.contains(Flags::WIDE_CHAR_SPACER) {
        CellWidth::WideSpacer
    } else {
        CellWidth::Narrow
    };
    let underline = if flags.contains(Flags::UNDERCURL) {
        Underline::Curly
    } else if flags.contains(Flags::DOTTED_UNDERLINE) {
        Underline::Dotted
    } else if flags.contains(Flags::DASHED_UNDERLINE) {
        Underline::Dashed
    } else if flags.contains(Flags::DOUBLE_UNDERLINE) {
        Underline::Double
    } else if flags.contains(Flags::UNDERLINE) {
        Underline::Single
    } else {
        Underline::None
    };
    Cell {
        // alacritty marks the start of a tab stop jump with a tab character
        // so that copied text keeps it. On screen it is a blank.
        ch: if cell.c == '\t' { ' ' } else { cell.c },
        zero_width: cell.zerowidth().map(<[char]>::to_vec).unwrap_or_default(),
        width,
        fg: convert_color(cell.fg),
        bg: convert_color(cell.bg),
        attrs: Attributes {
            bold: flags.contains(Flags::BOLD),
            dim: flags.contains(Flags::DIM),
            italic: flags.contains(Flags::ITALIC),
            underline,
            inverse: flags.contains(Flags::INVERSE),
            hidden: flags.contains(Flags::HIDDEN),
            strikeout: flags.contains(Flags::STRIKEOUT),
        },
        hyperlink: cell.hyperlink().and_then(|link| {
            let uri = link.uri();
            // Alacritty shares each hyperlink across its cells. The grid
            // keeps those addresses alive throughout this conversion.
            (uri.len() <= MAX_HYPERLINK_BYTES).then(|| {
                links
                    .entry(uri.as_ptr())
                    .or_insert_with(|| Arc::from(uri))
                    .clone()
            })
        }),
    }
}

fn convert_color(color: AlacrittyColor) -> Color {
    match color {
        AlacrittyColor::Spec(rgb) => Color::Rgb(rgb.r, rgb.g, rgb.b),
        AlacrittyColor::Indexed(index) => Color::Indexed(index),
        // Named colors 0-15 are the ANSI palette. The rest (foreground,
        // background and their variants) mean "no explicit color".
        AlacrittyColor::Named(named) => match u8::try_from(named as usize) {
            Ok(index @ 0..=15) => Color::Indexed(index),
            _ => Color::Default,
        },
    }
}

/// Where each run of combining characters in `bytes` ends, as offsets
/// into `bytes`. A run longer than [`ZERO_WIDTH_CHARS`] ends each time it
/// has that many, so that a cell never holds more than twice the limit
/// before it is cut.
fn zero_width_run_ends(bytes: &[u8]) -> Vec<usize> {
    let mut ends = Vec::new();
    let mut offset = 0;
    let mut run = 0;
    for chunk in bytes.utf8_chunks() {
        for c in chunk.valid().chars() {
            // The width alacritty goes by.
            let zero_width = !c.is_ascii() && c.width() == Some(0);
            if !zero_width && run > 0 {
                ends.push(offset);
                run = 0;
            }
            offset += c.len_utf8();
            if zero_width {
                run += 1;
                if run == ZERO_WIDTH_CHARS {
                    ends.push(offset);
                    run = 0;
                }
            }
        }
        // Invalid bytes end a run as another character does.
        if run > 0 {
            ends.push(offset);
            run = 0;
        }
        offset += chunk.invalid().len();
    }
    ends
}

/// Collects what alacritty's `Term` emits while parsing, until the owner of
/// the core takes it.
///
/// `Term` only hands its listener a shared reference, hence the mutex. The
/// lock is never contended: both ends live in the same `AlacrittyCore`.
#[derive(Clone, Default)]
struct Outbox(Arc<Mutex<Pending>>);

#[derive(Default)]
struct Pending {
    events: Vec<TerminalEvent>,
    pty_replies: Vec<u8>,
    colors: Option<ReportedColors>,
}

impl ReportedColors {
    /// The color at alacritty's color index: the palette, then the named
    /// colors.
    fn at(&self, index: usize) -> Option<Rgb> {
        match index {
            0..=255 => Some(self.palette[index]),
            _ if index == NamedColor::Foreground as usize => Some(self.foreground),
            _ if index == NamedColor::Background as usize => Some(self.background),
            _ if index == NamedColor::Cursor as usize => Some(self.cursor),
            _ => None,
        }
    }
}

impl Outbox {
    fn lock(&self) -> MutexGuard<'_, Pending> {
        // A poisoned lock still holds consistent data: every critical
        // section is a single push or take.
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl EventListener for Outbox {
    fn send_event(&self, event: Event) {
        let mut pending = self.lock();
        match event {
            Event::Title(title) => {
                // The title goes to the tab's label and to notifications,
                // which leave out what they do not draw.
                let title = cut_to_chars(crate::notification::shown(&title), TITLE_CHARS);
                pending.events.push(TerminalEvent::TitleChanged(title));
            }
            Event::ResetTitle => pending.events.push(TerminalEvent::TitleReset),
            Event::PtyWrite(text) => pending.pty_replies.extend_from_slice(text.as_bytes()),
            // Answered right here rather than after parsing, so that the
            // answer keeps its place among the other replies.
            Event::ColorRequest(index, format) => {
                let color = pending.colors.as_ref().and_then(|colors| colors.at(index));
                if let Some(Rgb(r, g, b)) = color {
                    let answer = format(AlacrittyRgb { r, g, b });
                    pending.pty_replies.extend_from_slice(answer.as_bytes());
                }
            }
            // Text-area-size queries need the cell size in pixels, which the
            // core does not know; clipboard access and the bell are not part
            // of `TerminalCore` yet.
            _ => {}
        }
    }
}

/// Makes the parser apply synchronized updates (mode 2026) immediately.
///
/// The stock handler buffers the bytes of a synchronized update until it ends
/// or 150 ms pass, which would make the screen state depend on the clock.
#[derive(Default)]
struct NoSyncTimeout;

impl Timeout for NoSyncTimeout {
    fn set_timeout(&mut self, _duration: Duration) {}

    fn clear_timeout(&mut self) {}

    fn pending_timeout(&self) -> bool {
        false
    }
}

struct Dims(GridSize);

impl Dimensions for Dims {
    fn total_lines(&self) -> usize {
        self.screen_lines()
    }

    fn screen_lines(&self) -> usize {
        usize::from(self.0.rows)
    }

    fn columns(&self) -> usize {
        usize::from(self.0.cols)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::OSC_BYTES;

    #[test]
    fn an_osc_that_does_not_end_is_read_up_to_the_limit() {
        let mut core = AlacrittyCore::new(GridSize::new(4, 20), 100);
        core.feed(b"\x1b]8;;");
        let piece = vec![b'u'; OSC_BYTES / 4];
        for _ in 0..8 {
            core.feed(&piece);
        }
        core.feed(b"\x07x");

        // Inspect what reached alacritty before conversion drops overlong URLs.
        let cell = &core.term.grid()[Line(0)][Column(0)];
        assert_eq!(cell.c, 'x');
        let link = cell.hyperlink().unwrap();
        assert!((OSC_BYTES - 16..=OSC_BYTES).contains(&link.uri().len()));
    }
}

//! What is selected in a terminal, and the text that a copy takes.
//!
//! Lines are numbered as in [`viewport`](crate::viewport): from the
//! oldest scrollback row (0) to the last screen row. While the scrollback
//! grows, a line keeps its number, so a selection can reach from the
//! scrollback onto the screen and stay put while output arrives.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::ops::{Range, RangeInclusive};
use std::time::Duration;

use benterm_core::{CellWidth, Damage, Modes, MouseTracking, ReportedColors, Row, RowEnd};

/// A cell: its line and its column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Point {
    pub line: usize,
    pub col: u16,
}

/// Which half of a cell the pointer is over.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Side {
    Left,
    Right,
}

/// Where the pointer is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Hit {
    pub point: Point,
    pub side: Side,
}

impl Hit {
    /// The edge between cells nearest to the pointer, as a column from 0
    /// to the number of columns.
    fn edge(self) -> Point {
        let col = match self.side {
            Side::Left => self.point.col,
            Side::Right => self.point.col.saturating_add(1),
        };
        Point { col, ..self.point }
    }
}

/// The cell under a pointer that is `x` cells right of the left edge of
/// the grid and `y` lines below the top of line `first_line`. A pointer
/// outside the grid is taken to the nearest of its `cols` columns and
/// `line_count` lines.
pub fn hit(x: f32, y: f32, first_line: usize, cols: u16, line_count: usize) -> Hit {
    let last_line = line_count.saturating_sub(1) as f64;
    let line = (first_line as f64 + f64::from(y).floor()).clamp(0.0, last_line) as usize;
    let last_col = cols.saturating_sub(1);
    let (col, side) = if x < 0.0 {
        (0, Side::Left)
    } else if x >= f32::from(cols) {
        (last_col, Side::Right)
    } else {
        let col = (x.floor() as u16).min(last_col);
        let side = if x - f32::from(col) < 0.5 {
            Side::Left
        } else {
            Side::Right
        };
        (col, side)
    };
    Hit {
        point: Point { line, col },
        side,
    }
}

/// What a selection grows by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Unit {
    /// Cells, from a drag.
    Cell,
    /// Whole words, from a double click.
    Word,
    /// Whole lines, from a triple click.
    Line,
}

impl Unit {
    /// The unit of a press that is the `count`th click in a row.
    pub fn for_clicks(count: usize) -> Self {
        match count {
            0 | 1 => Self::Cell,
            2 => Self::Word,
            _ => Self::Line,
        }
    }
}

/// A selection: where the press was, where the pointer is now, and the
/// unit the selection grows by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    anchor: Hit,
    head: Hit,
    unit: Unit,
}

impl Selection {
    /// A selection started by a press at `at`.
    pub fn new(at: Hit, unit: Unit) -> Self {
        Self {
            anchor: at,
            head: at,
            unit,
        }
    }

    /// Moves the free end to `to`.
    pub fn extend_to(&mut self, to: Hit) {
        self.head = to;
    }

    /// The lines the selection touches.
    pub fn lines(&self) -> RangeInclusive<usize> {
        let (a, b) = (self.anchor.point.line, self.head.point.line);
        a.min(b)..=a.max(b)
    }

    /// Shifts the selection up by `evicted` lines after that many oldest
    /// scrollback rows were dropped. Returns `false` (leaving the selection
    /// unchanged) if any selected line was evicted.
    pub fn shift_up(&mut self, evicted: usize) -> bool {
        if self.anchor.point.line < evicted || self.head.point.line < evicted {
            return false;
        }
        self.anchor.point.line -= evicted;
        self.head.point.line -= evicted;
        true
    }

    /// The cells selected, or `None` if there are none. `row` gives the
    /// row of a line; it is asked only for the lines of the two ends.
    pub fn span(&self, row: impl Fn(usize) -> Option<Row>) -> Option<Span> {
        let (first, last) = if self.head.point < self.anchor.point {
            (self.head, self.anchor)
        } else {
            (self.anchor, self.head)
        };
        let first_row = row(first.point.line)?;
        let last_row = if last.point.line == first.point.line {
            first_row.clone()
        } else {
            row(last.point.line)?
        };
        let (start, end) = match self.unit {
            Unit::Cell => {
                let (a, b) = (self.anchor.edge(), self.head.edge());
                let (start, end) = if b < a { (b, a) } else { (a, b) };
                let start_row = if start.line == first.point.line {
                    &first_row
                } else {
                    &last_row
                };
                let end_row = if end.line == last.point.line {
                    &last_row
                } else {
                    &first_row
                };
                (
                    Point {
                        col: start_of_char(start_row, start.col),
                        ..start
                    },
                    Point {
                        col: end_of_char(end_row, end.col),
                        ..end
                    },
                )
            }
            Unit::Word => (
                Point {
                    col: word(&first_row, first.point.col).start,
                    ..first.point
                },
                Point {
                    col: word(&last_row, last.point.col).end,
                    ..last.point
                },
            ),
            Unit::Line => (
                Point {
                    col: 0,
                    ..first.point
                },
                Point {
                    col: columns(&last_row),
                    ..last.point
                },
            ),
        };
        (start < end).then_some(Span { start, end })
    }
}

/// Selected cells in reading order: from `start` up to, not including,
/// `end`. A column here is an edge between cells, from 0 to the number
/// of columns, and never the middle of a wide character.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: Point,
    pub end: Point,
}

impl Span {
    /// The selected columns of `line` in a grid of `cols` columns; empty
    /// if none are.
    pub fn columns(&self, line: usize, cols: u16) -> Range<u16> {
        if line < self.start.line || line > self.end.line {
            return 0..0;
        }
        let start = if line == self.start.line {
            self.start.col
        } else {
            0
        };
        let end = if line == self.end.line {
            self.end.col
        } else {
            cols
        };
        start.min(end)..end
    }

    /// The selected text. `rows` are the rows of the lines from
    /// `start.line` to `end.line`.
    ///
    /// A row that wraps is joined to the next one as it is, so a long
    /// line comes out as one line. After a row that ends its line, a
    /// newline follows, and blanks are dropped from the end of the row
    /// when nothing but blanks follows them.
    pub fn text(&self, rows: &[Row]) -> String {
        self.text_with(rows, None)
    }

    /// Like [`text`](Self::text). With the `colors` the screen is drawn
    /// in, text in the color of its background is copied as blanks, as
    /// `Row::text_with` reads it.
    pub fn text_with(&self, rows: &[Row], colors: Option<&ReportedColors>) -> String {
        let mut text = String::new();
        for (offset, row) in rows.iter().enumerate() {
            let line = self.start.line + offset;
            let range = self.columns(line, columns(row));
            // The blank left before a wide character that went on to the
            // next row is not part of the text.
            let len = match row.end {
                RowEnd::WrappedBeforeWide => row.cells.len().saturating_sub(1),
                _ => row.cells.len(),
            };
            let (start, end) = (
                usize::from(range.start).min(len),
                usize::from(range.end).min(len),
            );
            let mut piece = Row {
                cells: row.cells[start..end].to_vec(),
                end: row.end,
            }
            .text_with(colors);
            let last = offset + 1 == rows.len();
            // Every row but the last is selected to its end, so a row
            // that wraps goes on in the next selected row.
            if row.end.wraps() && !last {
                text.push_str(&piece);
                continue;
            }
            if row.cells[end..].iter().all(is_blank) {
                piece.truncate(piece.trim_end_matches(' ').len());
            }
            text.push_str(&piece);
            if !last {
                text.push('\n');
            }
        }
        text
    }
}

fn columns(row: &Row) -> u16 {
    u16::try_from(row.cells.len()).unwrap_or(u16::MAX)
}

fn is_blank(cell: &benterm_core::Cell) -> bool {
    cell.ch == ' ' && cell.zero_width.is_empty()
}

fn is_spacer(row: &Row, col: u16) -> bool {
    row.cells
        .get(usize::from(col))
        .is_some_and(|cell| cell.width == CellWidth::WideSpacer)
}

/// `edge` as the start of a selection: moved left onto the start of a
/// wide character it would cut.
fn start_of_char(row: &Row, edge: u16) -> u16 {
    if is_spacer(row, edge) { edge - 1 } else { edge }
}

/// `edge` as the end of a selection: moved right past the end of a wide
/// character it would cut.
fn end_of_char(row: &Row, edge: u16) -> u16 {
    if is_spacer(row, edge) { edge + 1 } else { edge }
}

/// Characters that end a word, besides blanks. The rest of a path or a
/// URL stays one word.
const SEPARATORS: &[char] = &[
    '"', '\'', '`', '|', '│', '(', ')', '[', ']', '{', '}', '<', '>', ',', ';',
];

#[derive(PartialEq)]
enum Class {
    Blank,
    Separator,
    Word,
}

fn class(row: &Row, col: usize) -> Class {
    // The second half of a wide character belongs with the first.
    let col = if row.cells[col].width == CellWidth::WideSpacer {
        col.saturating_sub(1)
    } else {
        col
    };
    let ch = row.cells[col].ch;
    if ch.is_whitespace() {
        Class::Blank
    } else if SEPARATORS.contains(&ch) {
        Class::Separator
    } else {
        Class::Word
    }
}

/// The columns of the word at `col`: the run of word characters or of
/// blanks around it, or the one separator.
fn word(row: &Row, col: u16) -> Range<u16> {
    let len = row.cells.len();
    let col = usize::from(col).min(len.saturating_sub(1));
    if len == 0 {
        return 0..0;
    }
    let here = class(row, col);
    let (mut start, mut end) = (col, col + 1);
    if here == Class::Separator {
        return start as u16..end as u16;
    }
    while start > 0 && class(row, start - 1) == here {
        start -= 1;
    }
    while end < len && class(row, end) == here {
        end += 1;
    }
    let start = start_of_char(row, start as u16);
    let end = end_of_char(row, end as u16);
    start..end
}

/// Whether a drag selects: always, unless the program asked for mouse
/// reports, when only a drag with shift held does.
pub fn selects_on_drag(modes: Modes, shift: bool) -> bool {
    shift || modes.mouse_tracking == MouseTracking::Off
}

/// How often a selection drag held outside the pane scrolls the viewport.
pub const DRAG_SCROLL_INTERVAL: Duration = Duration::from_millis(50);

/// Maximum number of lines scrolled per [`DRAG_SCROLL_INTERVAL`] tick.
pub const MAX_DRAG_SCROLL_LINES: i32 = 5;

/// Lines to scroll on one auto-scroll tick for a pointer at `pointer_y`
/// relative to a pane spanning `pane_top..pane_bottom` with row height
/// `line_height`.
///
/// Positive values scroll up toward older scrollback lines, negative values
/// scroll down toward the bottom, and zero means the pointer is inside the
/// pane vertically.
pub fn drag_scroll_lines(pointer_y: f32, pane_top: f32, pane_bottom: f32, line_height: f32) -> i32 {
    if line_height <= 0.0 || pane_bottom <= pane_top {
        return 0;
    }
    let (direction, distance) = if pointer_y < pane_top {
        (1, pane_top - pointer_y)
    } else if pointer_y > pane_bottom {
        (-1, pointer_y - pane_bottom)
    } else {
        return 0;
    };
    let lines = (distance / line_height)
        .ceil()
        .clamp(1.0, MAX_DRAG_SCROLL_LINES as f32) as i32;
    direction * lines
}

/// What happened to the terminal between two looks at it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    /// The grid changed size, and the lines may have been rewrapped.
    pub resized: bool,
    pub old_scrollback: usize,
    pub new_scrollback: usize,
    /// How many oldest scrollback rows were dropped (by reaching the
    /// scrollback limit or clearing the scrollback) since the last look.
    pub evicted: usize,
    /// The screen rows that may have changed.
    pub damage: Damage,
}

/// Whether `change` certainly left `lines` (numbered before `change`) as
/// they were, possibly shifted up by `change.evicted`.
///
/// Rows that were already in the scrollback never change unless they are
/// evicted, cleared, hidden by the alternate screen, or rewrapped by a
/// resize. A row on the screen changes only if it is damaged or the screen
/// scrolled.
pub fn untouched(lines: RangeInclusive<usize>, change: &Change) -> bool {
    if change.resized || *lines.start() < change.evicted {
        return false;
    }
    let shifted = (*lines.start() - change.evicted)..=(*lines.end() - change.evicted);
    if *lines.end() < change.old_scrollback && *shifted.end() < change.new_scrollback {
        return true;
    }
    if change.new_scrollback != change.old_scrollback.saturating_sub(change.evicted) {
        return false;
    }
    match &change.damage {
        Damage::Full => false,
        Damage::Rows(rows) if rows.is_empty() => true,
        Damage::Rows(rows) => !rows
            .iter()
            .any(|&row| shifted.contains(&(change.new_scrollback + usize::from(row)))),
    }
}

/// The most lines whose text is compared to tell whether a selection may
/// stay. A longer selection is dropped whenever its lines may have
/// changed.
pub const MAX_CHECKED_LINES: usize = 1000;

/// A summary of the text of `rows`, to tell whether lines still hold
/// what they held.
pub fn fingerprint(rows: &[Row]) -> u64 {
    let mut hasher = DefaultHasher::new();
    rows.len().hash(&mut hasher);
    for row in rows {
        row.text().hash(&mut hasher);
    }
    hasher.finish()
}

/// Whether a selection over `lines` (numbered before `change`) may stay
/// after `change`. `then` is the [`fingerprint`] of the lines when the
/// selection was made, if it was taken; `rows` gives the rows of the
/// shifted lines now, and is asked only when `change` may have touched
/// them.
pub fn survives(
    lines: RangeInclusive<usize>,
    change: &Change,
    then: Option<u64>,
    rows: impl FnOnce(RangeInclusive<usize>) -> Vec<Row>,
) -> bool {
    if change.resized && change.evicted > 0 {
        return false;
    }
    if !change.resized && *lines.start() < change.evicted {
        return false;
    }
    if untouched(lines.clone(), change) {
        return true;
    }
    let shifted = if change.resized {
        lines
    } else {
        (*lines.start() - change.evicted)..=(*lines.end() - change.evicted)
    };
    match then {
        Some(then) if shifted.clone().count() <= MAX_CHECKED_LINES => {
            fingerprint(&rows(shifted)) == then
        }
        _ => false,
    }
}

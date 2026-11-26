//! Copying a line that the terminal wrapped onto several rows gives one
//! line, without a newline where it wrapped.

use benterm_core::{AlacrittyCore, Cell, GridSize, Row, RowEnd, TerminalCore};
use benterm_ui::selection::{Hit, Point, Selection, Side, Unit};
use proptest::prelude::*;

/// Every row of a terminal of `cols` columns and one screen row after
/// printing `text`, the scrollback first. With one screen row, the rows
/// are exactly those the text takes.
fn printed(cols: u16, text: &str) -> Vec<Row> {
    let mut core = AlacrittyCore::new(GridSize::new(1, cols), 10_000);
    core.feed(text.as_bytes());
    all_rows(&core)
}

fn all_rows(core: &impl TerminalCore) -> Vec<Row> {
    let mut rows = core.scrollback_rows(0..core.scrollback_len());
    rows.extend(core.snapshot().rows);
    rows
}

fn at(line: usize, col: u16, side: Side) -> Hit {
    Hit {
        point: Point { line, col },
        side,
    }
}

/// The text a drag from `from` to `to` copies.
fn dragged(rows: &[Row], from: Hit, to: Hit) -> String {
    let mut selection = Selection::new(from, Unit::Cell);
    selection.extend_to(to);
    let span = selection
        .span(|line| rows.get(line).cloned())
        .expect("something is selected");
    span.text(&rows[span.start.line..=span.end.line])
}

/// The text of a triple click on the first row dragged to the last one.
fn everything(rows: &[Row]) -> String {
    let mut selection = Selection::new(at(0, 0, Side::Left), Unit::Line);
    selection.extend_to(at(rows.len() - 1, 0, Side::Left));
    let span = selection
        .span(|line| rows.get(line).cloned())
        .expect("whole lines are selected");
    span.text(&rows[span.start.line..=span.end.line])
}

#[test]
fn a_wrapped_line_is_copied_as_one_line() {
    let rows = printed(4, "abcdefghij");
    assert_eq!(rows.len(), 3);
    assert_eq!(everything(&rows), "abcdefghij");
}

#[test]
fn a_blank_where_the_line_wrapped_is_kept() {
    let rows = printed(4, "abc defg");
    assert_eq!(everything(&rows), "abc defg");
}

#[test]
fn the_blank_left_before_a_wide_character_is_not_copied() {
    let rows = printed(3, "ab日本");
    assert_eq!(rows[0].end, RowEnd::WrappedBeforeWide);
    assert_eq!(everything(&rows), "ab日本");
}

#[test]
fn wrapped_and_unwrapped_rows_mix() {
    let rows = printed(4, "abcdef\r\ngh\r\nijklmnop");
    assert_eq!(everything(&rows), "abcdef\ngh\nijklmnop");
}

#[test]
fn a_drag_from_the_middle_of_a_wrapped_line_joins_its_rows() {
    let rows = printed(4, "abcdefghij");
    let text = dragged(&rows, at(0, 2, Side::Left), at(1, 1, Side::Right));
    assert_eq!(text, "cdef");
}

#[test]
fn a_drag_that_ends_on_a_wrapped_row_has_no_newline_at_the_end() {
    let rows = printed(4, "abcdefgh\r\nxy");
    let text = dragged(&rows, at(0, 0, Side::Left), at(0, 3, Side::Right));
    assert_eq!(text, "abcd");
}

/// Lines of narrow and wide characters, without blanks at the end, which
/// a copy drops.
fn lines() -> impl Strategy<Value = Vec<String>> {
    prop::collection::vec("([a-c 日本]{0,15}[a-c日本])?", 1..5)
}

/// A row of `cols` letters ending as `end`. A row that ends before a
/// wide character has the blank such a row has in its last cell.
fn row(letters: &[char], end: RowEnd) -> Row {
    let mut cells: Vec<Cell> = letters
        .iter()
        .map(|&ch| Cell {
            ch,
            ..Cell::default()
        })
        .collect();
    if end == RowEnd::WrappedBeforeWide {
        *cells.last_mut().unwrap() = Cell::default();
    }
    Row { cells, end }
}

fn row_end() -> impl Strategy<Value = RowEnd> {
    prop_oneof![
        Just(RowEnd::LineEnd),
        Just(RowEnd::Wrapped),
        Just(RowEnd::WrappedBeforeWide),
    ]
}

proptest! {
    /// A newline goes after every row that ends its line, and only there:
    /// the rows of a line are joined as they are, without the blank left
    /// before a wide character.
    #[test]
    fn a_newline_comes_exactly_where_a_line_ends(
        rows in prop::collection::vec(
            (prop::collection::vec(prop::char::range('a', 'z'), 4), row_end()),
            1..8,
        ),
    ) {
        let rows: Vec<Row> = rows.iter().map(|(letters, end)| row(letters, *end)).collect();
        let mut expected = String::new();
        for (index, row) in rows.iter().enumerate() {
            let text = row.text();
            expected.push_str(match row.end {
                RowEnd::WrappedBeforeWide => &text[..text.len() - 1],
                _ => &text,
            });
            if !row.end.wraps() && index + 1 < rows.len() {
                expected.push('\n');
            }
        }
        // The last row's own end does not matter: nothing follows it.
        let copied = everything(&rows);
        let expected = expected.trim_end_matches(' ');
        prop_assert_eq!(copied, expected);
    }

    /// Whatever lines a program prints and however narrow the terminal,
    /// copying them all gives the lines back.
    #[test]
    fn copying_printed_lines_gives_them_back(lines in lines(), cols in 2u16..9) {
        let rows = printed(cols, &lines.join("\r\n"));
        prop_assert_eq!(everything(&rows), lines.join("\n"));
    }

    /// The same holds after the terminal is resized and its lines are
    /// rewrapped.
    ///
    /// The output ends with a newline, as a shell's does before the
    /// prompt. With the cursor waiting to wrap at the end of a row,
    /// alacritty rewraps that row onto an empty one, which is not about
    /// copying. Empty rows below the output are left out of the
    /// comparison.
    #[test]
    fn copying_rewrapped_lines_gives_them_back(
        lines in lines(),
        cols in 2u16..9,
        new_cols in 2u16..9,
    ) {
        let mut core = AlacrittyCore::new(GridSize::new(1, cols), 10_000);
        core.feed(format!("{}\r\n", lines.join("\r\n")).as_bytes());
        core.resize(GridSize::new(1, new_cols));
        let copied = everything(&all_rows(&core));
        let expected = lines.join("\n");
        prop_assert_eq!(copied.trim_end_matches('\n'), expected.trim_end_matches('\n'));
    }
}

//! Whether a row ends a line of text or wraps onto the next row.

use benterm_core::{AlacrittyCore, CellWidth, Damage, GridSize, Row, RowEnd, TerminalCore};
use proptest::prelude::*;

fn ends(rows: &[Row]) -> Vec<RowEnd> {
    rows.iter().map(|row| row.end).collect()
}

/// Every row of `core`, the scrollback first.
fn all_rows(core: &impl TerminalCore) -> Vec<Row> {
    let mut rows = core.scrollback_rows(0..core.scrollback_len());
    rows.extend(core.snapshot().rows);
    rows
}

#[test]
fn a_line_longer_than_the_row_wraps() {
    let mut core = AlacrittyCore::new(GridSize::new(3, 4), 0);
    core.feed(b"abcdefg");
    assert_eq!(
        ends(&core.snapshot().rows),
        [RowEnd::Wrapped, RowEnd::LineEnd, RowEnd::LineEnd]
    );
}

#[test]
fn a_line_that_just_fills_the_row_does_not_wrap() {
    let mut core = AlacrittyCore::new(GridSize::new(3, 4), 0);
    core.feed(b"abcd\r\nef");
    assert_eq!(ends(&core.snapshot().rows), [RowEnd::LineEnd; 3]);
}

#[test]
fn a_newline_ends_the_line() {
    let mut core = AlacrittyCore::new(GridSize::new(3, 4), 0);
    core.feed(b"ab\r\ncd");
    assert_eq!(ends(&core.snapshot().rows), [RowEnd::LineEnd; 3]);
}

#[test]
fn a_wide_character_that_does_not_fit_leaves_a_blank_and_wraps() {
    let mut core = AlacrittyCore::new(GridSize::new(2, 3), 0);
    core.feed("ab日".as_bytes());
    let rows = core.snapshot().rows;
    assert_eq!(ends(&rows), [RowEnd::WrappedBeforeWide, RowEnd::LineEnd]);
    let last = &rows[0].cells[2];
    assert_eq!((last.ch, last.width), (' ', CellWidth::Narrow));
    assert_eq!(rows[1].cells[0].ch, '日');
}

#[test]
fn rows_in_the_scrollback_keep_their_wrap() {
    let mut core = AlacrittyCore::new(GridSize::new(2, 4), 10);
    core.feed(b"abcdefgh\r\nij\r\nkl");
    assert_eq!(
        ends(&core.scrollback_rows(0..core.scrollback_len())),
        [RowEnd::Wrapped, RowEnd::LineEnd]
    );
}

#[test]
fn erasing_a_wrapped_row_ends_its_line() {
    let mut core = AlacrittyCore::new(GridSize::new(3, 4), 0);
    core.feed(b"abcdefg\x1b[1;1H\x1b[2K");
    assert_eq!(ends(&core.snapshot().rows)[0], RowEnd::LineEnd);
}

/// Lines of narrow and wide characters without control characters or
/// blanks at the end.
fn lines() -> impl Strategy<Value = Vec<String>> {
    prop::collection::vec("[a-c 日本]{0,15}[a-c日本]|", 1..5)
}

proptest! {
    /// Printing lines one after another, a row wraps exactly when its
    /// line has not ended yet, and a line is cut only where it no longer
    /// fits.
    #[test]
    fn rows_wrap_exactly_where_a_line_goes_on(
        lines in lines(),
        cols in 2u16..9,
    ) {
        let mut core = AlacrittyCore::new(GridSize::new(2, cols), 1000);
        core.feed(lines.join("\r\n").as_bytes());
        let rows = all_rows(&core);

        let mut index = 0;
        for line in &lines {
            // The rows the line takes, all but the last one wrapped.
            let mut width = 0usize;
            let mut taken = 1;
            for ch in line.chars() {
                let w = if ch.is_ascii() { 1 } else { 2 };
                if width + w > usize::from(cols) {
                    let expected = if width < usize::from(cols) {
                        RowEnd::WrappedBeforeWide
                    } else {
                        RowEnd::Wrapped
                    };
                    prop_assert_eq!(rows[index + taken - 1].end, expected);
                    taken += 1;
                    width = 0;
                }
                width += w;
            }
            prop_assert_eq!(rows[index + taken - 1].end, RowEnd::LineEnd);
            index += taken;
        }
    }
}

/// Overwriting the wide character that did not fit takes away the blank
/// before it, which changes how the row above ends. That row is damaged,
/// so a renderer that reads only damaged rows sees the change.
#[test]
fn overwriting_a_wide_character_at_the_start_damages_the_row_above() {
    let mut core = AlacrittyCore::new(GridSize::new(2, 3), 0);
    core.feed("ab日".as_bytes());
    core.take_damage();
    core.feed(b"\x1b[2;1Hx");
    assert_eq!(core.screen_rows(0..1)[0].end, RowEnd::Wrapped);
    assert_eq!(core.take_damage(), Damage::Rows(vec![0, 1]));
}

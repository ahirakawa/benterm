//! What a core showed is written as bytes, and a new core fed those
//! bytes shows the same.
//!
//! The checks are written against `TerminalCore`, so a second
//! implementation can be put through them.

use benterm_core::{
    AlacrittyCore, Cell, Color, GridSize, Modes, Row, RowEnd, TerminalCore, Underline,
    history_bytes, history_bytes_within, is_history,
};
use proptest::collection::vec;
use proptest::prelude::*;

/// Enough for every test here to lose no row.
const SCROLLBACK_LIMIT: usize = 10_000;

fn new_core(size: GridSize) -> AlacrittyCore {
    AlacrittyCore::new(size, SCROLLBACK_LIMIT)
}

/// The rows of the scrollback and the screen, down to the last one that
/// is not blank. The line of that row is taken to end there: no row
/// that it could go on into is left.
fn shown(core: &impl TerminalCore) -> Vec<Row> {
    let mut rows = core.scrollback_rows(0..core.scrollback_len());
    rows.extend(core.snapshot().rows);
    let blank = Cell::default();
    let used = rows
        .iter()
        .rposition(|row| row.cells.iter().any(|cell| *cell != blank));
    rows.truncate(used.map_or(0, |last| last + 1));
    if let Some(last) = rows.last_mut() {
        last.end = RowEnd::LineEnd;
    }
    rows
}

/// A new core of `size` that was fed what `core` shows.
fn shown_again(core: &impl TerminalCore, size: GridSize) -> AlacrittyCore {
    let mut again = new_core(size);
    again.feed(&history_bytes(&shown(core)));
    again
}

/// Feeds `output` to a core of `size` and checks that a new core of the
/// same size, fed what the first shows, shows the same rows.
fn assert_shown_again(size: GridSize, output: &str) {
    let mut core = new_core(size);
    core.feed(output.as_bytes());
    let again = shown_again(&core, size);
    assert_eq!(shown(&again), shown(&core), "after {output:?}");
}

#[test]
fn nothing_shown_is_no_bytes() {
    let core = new_core(GridSize::new(3, 8));
    assert_eq!(history_bytes(&shown(&core)), b"");
}

#[test]
fn lines_are_shown_again() {
    assert_shown_again(GridSize::new(4, 8), "ab\r\n\r\n  cd\r\nef");
}

#[test]
fn rows_of_the_scrollback_are_shown_again_above_the_screen() {
    let size = GridSize::new(2, 8);
    let mut core = new_core(size);
    core.feed(b"1\r\n2\r\n3\r\n4\r\n5");
    let again = shown_again(&core, size);
    assert_eq!(shown(&again), shown(&core));
    assert!(again.scrollback_len() >= 3);
}

#[test]
fn a_line_that_just_fills_the_row_ends_there() {
    assert_shown_again(GridSize::new(3, 4), "abcd\r\nef");
}

#[test]
fn a_line_that_wraps_is_shown_as_one_line_again() {
    let size = GridSize::new(3, 4);
    let mut core = new_core(size);
    core.feed(b"abcdefghij\r\nkl");
    let again = shown_again(&core, size);
    assert_eq!(shown(&again), shown(&core));
    assert_eq!(again.scrollback_rows(0..1)[0].end, RowEnd::Wrapped);
}

#[test]
fn blanks_at_the_end_of_a_row_that_wraps_are_kept() {
    assert_shown_again(GridSize::new(3, 4), "ab   cd");
}

#[test]
fn a_row_that_wraps_into_a_blank_row_still_wraps() {
    assert_shown_again(GridSize::new(3, 2), "    \r\na");
}

#[test]
fn a_wide_character_that_did_not_fit_wraps_again() {
    let size = GridSize::new(3, 3);
    let mut core = new_core(size);
    core.feed("ab日c".as_bytes());
    let again = shown_again(&core, size);
    assert_eq!(shown(&again), shown(&core));
    assert_eq!(again.snapshot().rows[0].end, RowEnd::WrappedBeforeWide);
}

#[test]
fn a_blank_before_a_wide_character_that_is_gone_is_an_ordinary_blank() {
    let size = GridSize::new(3, 3);
    let mut core = new_core(size);
    // The wide character at the start of the second row is written over.
    core.feed("ab日\rxy".as_bytes());
    let again = shown_again(&core, size);
    let texts = |core: &AlacrittyCore| -> Vec<String> {
        shown(core).iter().map(Row::text).collect::<Vec<_>>()
    };
    assert_eq!(texts(&again), texts(&core));
}

/// Found by Codex: the blank left before a wide character keeps its own
/// color when the wide character is written again in another. A core
/// leaves that blank in the color of the wide character, so the blank
/// is written as an ordinary one, and the row wraps as an ordinary row.
#[test]
fn a_blank_before_a_wide_character_of_another_color_keeps_its_color() {
    let size = GridSize::new(4, 3);
    let mut core = new_core(size);
    core.feed("ab\x1b[41m日\r\x1b[2X\x1b[42m日".as_bytes());
    let showed = shown(&core);
    assert_eq!(showed[0].end, RowEnd::WrappedBeforeWide);
    let again = shown_again(&core, size);
    let mut expected = showed.clone();
    expected[0].end = RowEnd::Wrapped;
    assert_eq!(shown(&again), expected);
    assert_eq!(expected[0].cells[2].bg, Color::Indexed(1));
}

#[test]
fn wide_and_combining_characters_are_shown_again() {
    assert_shown_again(GridSize::new(3, 10), "日本語 e\u{301}\u{302} 🙂\r\nｱ");
}

#[test]
fn colors_are_shown_again() {
    assert_shown_again(
        GridSize::new(3, 20),
        "\x1b[31mred\x1b[92mbright\x1b[38;5;200mmany\x1b[38;2;1;2;3mrgb\
         \x1b[0;44mback\x1b[48;5;17mb\x1b[48;2;9;8;7mc",
    );
}

#[test]
fn attributes_are_shown_again() {
    assert_shown_again(
        GridSize::new(3, 40),
        "\x1b[1mb\x1b[0;2md\x1b[0;3mi\x1b[0;7mv\x1b[0;8mh\x1b[0;9ms\
         \x1b[0;4mu\x1b[0;4:2mu\x1b[0;4:3mu\x1b[0;4:4mu\x1b[0;4:5mu\x1b[0;1;3;4;9mall",
    );
}

#[test]
fn every_kind_of_underline_is_written() {
    let size = GridSize::new(2, 8);
    let mut core = new_core(size);
    core.feed(b"\x1b[4ma\x1b[4:2mb\x1b[4:3mc\x1b[4:4md\x1b[4:5me");
    let again = shown_again(&core, size);
    let underlines: Vec<Underline> = again.snapshot().rows[0].cells[..5]
        .iter()
        .map(|cell| cell.attrs.underline)
        .collect();
    assert_eq!(
        underlines,
        [
            Underline::Single,
            Underline::Double,
            Underline::Curly,
            Underline::Dotted,
            Underline::Dashed
        ]
    );
}

#[test]
fn blanks_with_a_background_are_shown_again() {
    assert_shown_again(
        GridSize::new(3, 8),
        "a\x1b[41m   \x1b[0m\r\n\x1b[42m\x1b[K\x1b[0m\r\nb",
    );
}

#[test]
fn a_background_does_not_spread_to_the_rows_made_by_scrolling() {
    // The screen scrolls for a line end and for a wrap while a
    // background is set.
    assert_shown_again(GridSize::new(2, 4), "\x1b[41ma\r\nb\r\nc\x1b[0m\r\nd");
    assert_shown_again(GridSize::new(2, 4), "abc\x1b[41mdef\x1b[0m\r\ng");
    assert_shown_again(GridSize::new(2, 4), "x\r\nabc\x1b[41mdefgh\x1b[0mi\r\ng");
}

#[test]
fn the_cursor_is_left_at_the_start_of_the_row_below() {
    let size = GridSize::new(5, 8);
    let mut core = new_core(size);
    core.feed(b"\x1b[31mab\r\ncd");
    let again = shown_again(&core, size);
    let cursor = again.cursor();
    assert_eq!((cursor.row, cursor.col), (2, 0));
    // What the shell writes next has no color left over.
    let mut again = again;
    again.feed(b"$");
    assert_eq!(again.snapshot().rows[2].cells[0].fg, Color::Default);
}

#[test]
fn a_last_row_that_wraps_is_closed() {
    let size = GridSize::new(3, 4);
    let mut core = new_core(size);
    core.feed(b"abcdef");
    let rows = shown(&core);
    let mut again = new_core(size);
    again.feed(&history_bytes(&rows[..1]));
    again.feed(b"$");
    assert_eq!(again.snapshot().rows[0].end, RowEnd::LineEnd);
    assert_eq!(again.snapshot().rows[1].cells[0].ch, '$');
}

#[test]
fn a_core_of_another_width_wraps_the_lines_where_they_fit() {
    let mut core = new_core(GridSize::new(3, 4));
    core.feed("abcdefgh\r\n日本語\r\nz".as_bytes());
    let again = shown_again(&core, GridSize::new(3, 6));
    let texts: Vec<String> = shown(&again)
        .iter()
        .map(|row| row.text().trim_end().to_owned())
        .collect();
    assert_eq!(texts, ["abcdef", "gh", "日本語", "z"]);
}

/// Found by Codex: rows with a color change in every cell take more
/// bytes than a file may have, and then none of them was saved.
#[test]
fn of_rows_that_take_too_many_bytes_the_last_that_fit_are_written() {
    let size = GridSize::new(4, 10);
    let mut core = new_core(size);
    for line in 0..40 {
        core.feed(format!("\x1b[38;2;1;2;{line}mline {line}\x1b[0m\r\n").as_bytes());
    }
    let rows = shown(&core);
    let all = history_bytes(&rows);
    assert_eq!(history_bytes_within(&rows, all.len()), all);

    let some = history_bytes_within(&rows, all.len() - 1);
    assert!(!some.is_empty() && some.len() < all.len());
    assert!(is_history(&some));
    let mut again = new_core(size);
    again.feed(&some);
    let kept = shown(&again);
    assert_eq!(kept[..], rows[rows.len() - kept.len()..]);

    assert_eq!(history_bytes_within(&rows, 3), b"");
}

#[test]
fn only_text_line_ends_and_colors_are_history() {
    assert!(is_history(b""));
    assert!(is_history(
        "ab 日本\r\n\x1b[0;1;4:3;38;2;1;2;3mx\x1b[0m\r\n".as_bytes()
    ));
    for bytes in [
        // Cut off, or ending with a color that the prompt would get.
        &b"ab"[..],
        b"\x1b[41mab\r\n",
        b"\x1b[0;8mab\x1b[m\x1b[1m\r\n",
        b"\x1b]0;title\x07\r\n",
        b"\x1b[6n",
        b"\x1b[?1049h",
        b"\x1b[2J",
        b"\x1bc",
        b"\x1b[0",
        b"\x1b",
        b"a\x07",
        b"a\x08",
        b"a\tb",
        b"\x7f",
        b"\xc2\x9b6n",
        b"\xff",
    ] {
        assert!(!is_history(bytes), "{bytes:?}");
    }
}

#[test]
fn a_control_character_in_a_cell_is_written_as_a_blank() {
    let mut row = Row {
        cells: vec![Cell::default(); 4],
        end: RowEnd::LineEnd,
    };
    row.cells[0].ch = '\x1b';
    row.cells[1].ch = 'a';
    row.cells[1].zero_width = vec!['\u{9b}', '\u{301}'];
    let bytes = history_bytes(&[row]);
    assert_eq!(bytes, " a\u{301}\r\n".as_bytes());
    assert!(is_history(&bytes));
}

/// A color or attribute change.
fn sgr() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("\x1b[0m".to_owned()),
        (0u8..=9).prop_map(|n| format!("\x1b[{n}m")),
        prop_oneof![
            Just("4:2"),
            Just("4:3"),
            Just("4:4"),
            Just("4:5"),
            Just("22"),
            Just("24")
        ]
        .prop_map(|n| format!("\x1b[{n}m")),
        (30u8..=37).prop_map(|n| format!("\x1b[{n}m")),
        (40u8..=47).prop_map(|n| format!("\x1b[{n}m")),
        (90u8..=97).prop_map(|n| format!("\x1b[{n}m")),
        any::<u8>().prop_map(|n| format!("\x1b[38;5;{n}m")),
        any::<u8>().prop_map(|n| format!("\x1b[48;5;{n}m")),
        any::<(u8, u8, u8)>().prop_map(|(r, g, b)| format!("\x1b[38;2;{r};{g};{b}m")),
        any::<(u8, u8, u8)>().prop_map(|(r, g, b)| format!("\x1b[48;2;{r};{g};{b}m")),
        Just("\x1b[39m".to_owned()),
        Just("\x1b[49m".to_owned()),
    ]
}

/// What a program that only prints writes: text with narrow, wide and
/// combining characters, colors, and line ends.
fn printed() -> impl Strategy<Value = String> {
    let piece = prop_oneof![
        4 => "[a-c ]{1,6}",
        2 => "[日本🙂ｱe\u{301}]{1,4}",
        2 => sgr(),
        2 => Just("\r\n".to_owned()),
        1 => Just("\x1b[K".to_owned()),
    ];
    vec(piece, 0..40).prop_map(|pieces| pieces.concat())
}

/// Any bytes, with escape sequences more often than chance gives them.
fn any_output() -> impl Strategy<Value = Vec<u8>> {
    let csi = (vec(0u16..=12, 0..3), 0x40u8..=0x7e).prop_map(|(params, last)| {
        let params: Vec<String> = params.iter().map(u16::to_string).collect();
        let mut bytes = format!("\x1b[{}", params.join(";")).into_bytes();
        bytes.push(last);
        bytes
    });
    let piece = prop_oneof![
        2 => vec(any::<u8>(), 0..16),
        3 => "[ -~日🙂\u{301}]{0,12}".prop_map(String::into_bytes),
        2 => printed().prop_map(String::into_bytes),
        3 => csi,
        1 => prop_oneof![Just("\x1b[?1049h"), Just("\x1b[?1049l"), Just("\x1bc"), Just("\x1bM")]
            .prop_map(|text| text.as_bytes().to_vec()),
    ];
    vec(piece, 0..12).prop_map(|pieces| pieces.concat())
}

proptest! {
    /// Whatever a program printed, a new core of the same size shows the
    /// same rows again, in the scrollback and on the screen.
    #[test]
    fn what_was_printed_is_shown_again(
        output in printed(),
        rows in 1u16..6,
        cols in 2u16..12,
    ) {
        let size = GridSize::new(rows, cols);
        let mut core = new_core(size);
        core.feed(output.as_bytes());
        let again = shown_again(&core, size);
        prop_assert_eq!(shown(&again), shown(&core));
    }

    /// A core that starts with another size and is then resized, as a
    /// pane is once it has been measured, shows the same text.
    #[test]
    fn what_was_printed_is_shown_again_after_a_resize(
        output in printed(),
        rows in 1u16..6,
        cols in 2u16..12,
        first_cols in 2u16..12,
    ) {
        let size = GridSize::new(rows, cols);
        let mut core = new_core(size);
        core.feed(output.as_bytes());
        let mut again = shown_again(&core, GridSize::new(rows, first_cols));
        again.resize(size);
        let lines = |core: &AlacrittyCore| lines_of(&shown(core));
        prop_assert_eq!(lines(&again), lines(&core));
    }

    /// Whatever a core was fed, what is written for its rows is history,
    /// and feeding it only shows rows: no answer to the program, no
    /// event, no mode, and the cursor at the start of a row.
    #[test]
    fn the_bytes_of_any_rows_only_show_rows(
        output in any_output(),
        rows in 1u16..6,
        cols in 2u16..12,
    ) {
        let size = GridSize::new(rows, cols);
        let mut core = new_core(size);
        core.feed(&output);
        let bytes = history_bytes(&shown(&core));
        prop_assert!(is_history(&bytes));

        let mut again = new_core(size);
        again.feed(&bytes);
        prop_assert_eq!(again.take_pty_replies(), b"");
        prop_assert_eq!(again.take_events(), []);
        prop_assert_eq!(again.modes(), Modes::default());
        prop_assert_eq!(again.cursor().col, 0);
        let texts = |core: &AlacrittyCore| -> Vec<String> {
            shown(core).iter().map(|row| row.text().trim_end().to_owned()).collect()
        };
        prop_assert_eq!(texts(&again), texts(&core));
    }
}

/// The text of the lines of `rows`: rows that wrap are joined to the
/// next, and blanks at the end of a line are dropped.
fn lines_of(rows: &[Row]) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = String::new();
    for row in rows {
        match row.end {
            RowEnd::WrappedBeforeWide => {
                let cells = row.cells[..row.cells.len() - 1].to_vec();
                line.push_str(
                    &Row {
                        cells,
                        end: row.end,
                    }
                    .text(),
                );
            }
            _ => line.push_str(&row.text()),
        }
        if row.end == RowEnd::LineEnd {
            lines.push(std::mem::take(&mut line).trim_end().to_owned());
        }
    }
    lines
}

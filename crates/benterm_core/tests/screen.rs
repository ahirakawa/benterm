//! Screen contents: text, cursor position and grid size.

use benterm_core::{AlacrittyCore, GridSize, TerminalCore};

fn core(rows: u16, cols: u16) -> AlacrittyCore {
    AlacrittyCore::new(GridSize::new(rows, cols), 100)
}

#[test]
fn new_core_has_a_blank_screen_of_the_requested_size() {
    let core = core(3, 5);
    let snapshot = core.snapshot();

    assert_eq!(core.size(), GridSize::new(3, 5));
    assert_eq!(snapshot.size, GridSize::new(3, 5));
    assert_eq!(snapshot.rows.len(), 3);
    for row in &snapshot.rows {
        assert_eq!(row.cells.len(), 5);
        assert_eq!(row.text(), "     ");
    }
    assert_eq!((snapshot.cursor.row, snapshot.cursor.col), (0, 0));
    assert!(snapshot.cursor.visible);
}

#[test]
fn fed_text_appears_on_the_screen_and_moves_the_cursor() {
    let mut core = core(3, 10);
    core.feed(b"hello\r\nwor");
    core.feed(b"ld");
    let snapshot = core.snapshot();

    assert_eq!(snapshot.rows[0].text(), "hello     ");
    assert_eq!(snapshot.rows[1].text(), "world     ");
    assert_eq!((snapshot.cursor.row, snapshot.cursor.col), (1, 5));
}

#[test]
fn escape_sequence_split_across_feeds_is_still_understood() {
    let mut core = core(5, 10);
    core.feed(b"\x1b[3");
    core.feed(b";4Hx");
    let snapshot = core.snapshot();

    assert_eq!(snapshot.rows[2].text(), "   x      ");
    assert_eq!((snapshot.cursor.row, snapshot.cursor.col), (2, 4));
}

#[test]
fn resize_changes_the_grid_and_keeps_the_text() {
    let mut core = core(3, 10);
    core.feed(b"abc");
    core.resize(GridSize::new(6, 20));
    let snapshot = core.snapshot();

    assert_eq!(core.size(), GridSize::new(6, 20));
    assert_eq!(snapshot.size, GridSize::new(6, 20));
    assert_eq!(snapshot.rows.len(), 6);
    assert!(snapshot.rows.iter().all(|row| row.cells.len() == 20));
    assert_eq!(snapshot.rows[0].text().trim_end(), "abc");
}

#[test]
fn sizes_below_the_minimum_are_raised_to_it() {
    let mut core = core(0, 0);
    assert_eq!(core.size(), GridSize::new(1, 2));

    core.resize(GridSize::new(4, 1));
    assert_eq!(core.size(), GridSize::new(4, 2));
    assert_eq!(core.snapshot().rows.len(), 4);
}

#[test]
fn wide_character_fits_the_narrowest_grid() {
    let mut core = core(1, 1);
    core.feed("あ".as_bytes());
    assert_eq!(core.snapshot().rows[0].text(), "あ");
}

#[test]
fn snapshots_of_cores_fed_the_same_bytes_are_equal() {
    let mut a = core(4, 12);
    let mut b = core(4, 12);
    a.feed(b"one\r\ntwo\x1b[1;31mthree");
    b.feed(b"one\r\ntwo\x1b[1;31mthree");
    assert_eq!(a.snapshot(), b.snapshot());

    b.feed(b"!");
    assert_ne!(a.snapshot(), b.snapshot());
}

#[test]
fn synchronized_update_does_not_delay_the_snapshot() {
    // Holding output back until a timer fires would make the snapshot depend
    // on wall-clock time.
    let mut core = core(3, 10);
    core.feed(b"\x1b[?2026hhello");
    assert_eq!(core.snapshot().rows[0].text(), "hello     ");
}

#[test]
fn character_split_across_feeds_does_not_swallow_the_bytes_after_it() {
    // "À", backspace, then a stray continuation byte. The backspace must
    // take effect wherever the input happens to be cut.
    let input = b"\xc3\x80\x08\x80";

    let mut whole = core(1, 4);
    whole.feed(input);

    let mut pieces = core(1, 4);
    pieces.feed(&input[..1]);
    pieces.feed(&input[1..]);

    assert_eq!(whole.snapshot(), pieces.snapshot());
}

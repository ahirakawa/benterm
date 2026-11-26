//! Rows that scrolled off the top of the screen.

use benterm_core::{AlacrittyCore, GridSize, Row, TerminalCore};

fn texts(rows: &[Row]) -> Vec<String> {
    rows.iter()
        .map(|row| row.text().trim_end().to_owned())
        .collect()
}

/// A 3x8 core that has printed the lines "1" to "5".
fn core_with_five_lines(scrollback_limit: usize) -> AlacrittyCore {
    let mut core = AlacrittyCore::new(GridSize::new(3, 8), scrollback_limit);
    core.feed(b"1\r\n2\r\n3\r\n4\r\n5");
    core
}

#[test]
fn new_core_has_no_scrollback() {
    let core = AlacrittyCore::new(GridSize::new(3, 8), 100);
    assert_eq!(core.scrollback_len(), 0);
    assert_eq!(core.snapshot().scrollback_len, 0);
    assert!(core.scrollback_rows(0..10).is_empty());
}

#[test]
fn rows_scrolled_off_the_top_are_kept_oldest_first() {
    let core = core_with_five_lines(100);
    let snapshot = core.snapshot();

    assert_eq!(texts(&snapshot.rows), ["3", "4", "5"]);
    assert_eq!(core.scrollback_len(), 2);
    assert_eq!(snapshot.scrollback_len, 2);
    assert_eq!(texts(&core.scrollback_rows(0..2)), ["1", "2"]);
    assert_eq!(texts(&core.scrollback_rows(1..2)), ["2"]);
}

#[test]
fn scrollback_rows_are_as_wide_as_the_grid() {
    let core = core_with_five_lines(100);
    assert!(
        core.scrollback_rows(0..2)
            .iter()
            .all(|row| row.cells.len() == 8)
    );
}

#[test]
fn scrollback_range_is_clamped() {
    let core = core_with_five_lines(100);

    assert_eq!(texts(&core.scrollback_rows(1..10)), ["2"]);
    assert!(core.scrollback_rows(5..9).is_empty());
    assert!(core.scrollback_rows(1..1).is_empty());
}

#[test]
fn scrollback_keeps_only_the_newest_rows_up_to_the_limit() {
    let core = core_with_five_lines(1);
    assert_eq!(core.scrollback_len(), 1);
    assert_eq!(texts(&core.scrollback_rows(0..1)), ["2"]);

    let core = core_with_five_lines(0);
    assert_eq!(core.scrollback_len(), 0);
}

#[test]
fn alternate_screen_has_no_scrollback_and_leaves_the_primary_one_intact() {
    let mut core = core_with_five_lines(100);

    core.feed(b"\x1b[?1049h");
    assert_eq!(core.scrollback_len(), 0);
    assert!(core.scrollback_rows(0..2).is_empty());

    core.feed(b"\x1b[?1049l");
    assert_eq!(texts(&core.scrollback_rows(0..2)), ["1", "2"]);
    assert_eq!(core.scrollback_evicted(), 0);
}

#[test]
fn scrollback_evicted_counts_rows_dropped_at_the_limit() {
    let mut core = AlacrittyCore::new(GridSize::new(3, 12), 5);
    core.feed(b"0\r\n1\r\n2\r\n3\r\n4\r\n5\r\n6\r\n7");
    assert_eq!(core.scrollback_len(), 5);
    assert_eq!(core.scrollback_evicted(), 0);

    core.feed(b"\r\n8");
    assert_eq!(core.scrollback_len(), 5);
    assert_eq!(core.scrollback_evicted(), 1);
    assert_eq!(
        texts(&core.scrollback_rows(0..5)),
        ["1", "2", "3", "4", "5"]
    );

    // A single burst larger than the scrollback limit counts every evicted row.
    let burst: String = (9..25).map(|i| format!("\r\n{i}")).collect();
    core.feed(burst.as_bytes());
    assert_eq!(core.scrollback_len(), 5);
    assert_eq!(core.scrollback_evicted(), 17);
    assert_eq!(
        texts(&core.scrollback_rows(0..5)),
        ["17", "18", "19", "20", "21"]
    );
}

#[test]
fn scrollback_evicted_handles_limit_one_and_zero() {
    let mut core = core_with_five_lines(1);
    assert_eq!(core.scrollback_len(), 1);
    assert_eq!(core.scrollback_evicted(), 1);
    core.feed(b"\r\n6\r\n7");
    assert_eq!(core.scrollback_len(), 1);
    assert_eq!(core.scrollback_evicted(), 3);
    assert_eq!(texts(&core.scrollback_rows(0..1)), ["4"]);

    let core = core_with_five_lines(0);
    assert_eq!(core.scrollback_len(), 0);
    assert_eq!(core.scrollback_evicted(), 0);
}

#[test]
fn clearing_scrollback_or_full_reset_counts_cleared_rows_as_evicted() {
    let mut core = AlacrittyCore::new(GridSize::new(3, 12), 10);
    core.feed(b"0\r\n1\r\n2\r\n3\r\n4\r\n5\r\n6");
    assert_eq!(core.scrollback_len(), 4);
    assert_eq!(core.scrollback_evicted(), 0);

    // CSI 3 J clears all 4 scrollback rows.
    core.feed(b"\x1b[3J");
    assert_eq!(core.scrollback_len(), 0);
    assert_eq!(core.scrollback_evicted(), 4);

    // More output, then full reset (ESC c) while on alternate screen.
    core.feed(b"\r\na\r\nb\r\nc\r\nd\r\ne");
    let before_alt = core.scrollback_len();
    assert!(before_alt > 0);
    core.feed(b"\x1b[?1049h");
    assert_eq!(core.scrollback_evicted(), 4);
    core.feed(b"\x1bc");
    assert_eq!(core.scrollback_len(), 0);
    assert_eq!(core.scrollback_evicted(), 4 + before_alt as u64);
}

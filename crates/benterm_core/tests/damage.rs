//! Reading only the rows that changed.

use benterm_core::{AlacrittyCore, CursorShape, Damage, GridSize, Row, TerminalCore};

fn core() -> AlacrittyCore {
    AlacrittyCore::new(GridSize::new(6, 20), 100)
}

/// A copy of the screen kept up to date from damage alone, the way the
/// renderer does it.
fn refresh(core: &mut AlacrittyCore, mirror: &mut Vec<Row>) {
    match core.take_damage() {
        Damage::Full => *mirror = core.screen_rows(0..core.size().rows),
        Damage::Rows(rows) => {
            for row in rows {
                mirror[usize::from(row)] = core.screen_rows(row..row + 1).remove(0);
            }
        }
    }
}

#[test]
fn a_new_core_is_fully_damaged() {
    assert_eq!(core().take_damage(), Damage::Full);
}

#[test]
fn taking_damage_resets_it() {
    let mut core = core();
    core.take_damage();
    assert_eq!(core.take_damage(), Damage::Rows(Vec::new()));
}

#[test]
fn text_damages_only_its_row() {
    let mut core = core();
    core.feed(b"\x1b[3;1H");
    core.take_damage();
    core.feed(b"hello");
    assert_eq!(core.take_damage(), Damage::Rows(vec![2]));
}

#[test]
fn moving_the_cursor_damages_the_rows_it_leaves_and_enters() {
    let mut core = core();
    core.take_damage();
    core.feed(b"\x1b[5;1H");
    assert_eq!(core.take_damage(), Damage::Rows(vec![0, 4]));
}

#[test]
fn resizing_damages_everything() {
    let mut core = core();
    core.take_damage();
    core.resize(GridSize::new(8, 30));
    assert_eq!(core.take_damage(), Damage::Full);
}

#[test]
fn scrolling_keeps_a_mirror_equal_to_the_snapshot() {
    let mut core = core();
    let mut mirror = Vec::new();
    refresh(&mut core, &mut mirror);
    for line in 0..20 {
        core.feed(format!("line {line}\r\n").as_bytes());
        refresh(&mut core, &mut mirror);
        assert_eq!(mirror, core.snapshot().rows);
    }
}

#[test]
fn screen_rows_are_the_snapshot_rows() {
    let mut core = core();
    core.feed("a\r\n日本語\r\n\x1b[1;31mred".as_bytes());
    let snapshot = core.snapshot();
    assert_eq!(core.screen_rows(0..6), snapshot.rows);
    assert_eq!(core.screen_rows(1..3), snapshot.rows[1..3]);
}

#[test]
fn screen_rows_clamp_the_range_to_the_screen() {
    let core = core();
    assert_eq!(core.screen_rows(4..100).len(), 2);
    assert!(core.screen_rows(50..60).is_empty());
}

#[test]
fn cursor_is_the_snapshot_cursor() {
    let mut core = core();
    core.feed(b"\x1b[2;5H\x1b[5 q\x1b[?25l");
    let cursor = core.cursor();
    assert_eq!(cursor, core.snapshot().cursor);
    assert_eq!((cursor.row, cursor.col), (1, 4));
    assert_eq!(cursor.shape, CursorShape::Bar);
    assert!(!cursor.visible);
}

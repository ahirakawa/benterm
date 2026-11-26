//! What a drag, a double click and a triple click select, and the text
//! that is copied.
//!
//! Lines are numbered from the oldest scrollback row (0) to the last
//! screen row, so a selection can reach from the scrollback onto the
//! screen.

use benterm_core::{
    AlacrittyCore, CellWidth, Damage, GridSize, Modes, MouseTracking, Row, TerminalCore,
};
use benterm_ui::selection::{
    Change, DRAG_SCROLL_INTERVAL, Hit, MAX_CHECKED_LINES, MAX_DRAG_SCROLL_LINES, Point, Selection,
    Side, Unit, drag_scroll_lines, fingerprint, hit, selects_on_drag, survives, untouched,
};
use proptest::prelude::*;

/// The rows a terminal of `cols` columns shows after printing `lines`,
/// one per row.
fn rows(cols: u16, lines: &[&str]) -> Vec<Row> {
    let height = u16::try_from(lines.len()).unwrap();
    let mut core = AlacrittyCore::new(GridSize::new(height, cols), 0);
    core.feed(lines.join("\r\n").as_bytes());
    core.snapshot().rows
}

fn at(line: usize, col: u16, side: Side) -> Hit {
    Hit {
        point: Point { line, col },
        side,
    }
}

fn selected(rows: &[Row], unit: Unit, from: Hit, to: Hit) -> Option<String> {
    let mut selection = Selection::new(from, unit);
    selection.extend_to(to);
    let span = selection.span(|line| rows.get(line).cloned())?;
    let lines = span.start.line..=span.end.line;
    Some(span.text(&rows[lines]))
}

#[test]
fn a_drag_selects_the_cells_between_the_two_points() {
    let rows = rows(20, &["hello world"]);
    let text = selected(
        &rows,
        Unit::Cell,
        at(0, 0, Side::Left),
        at(0, 4, Side::Right),
    );
    assert_eq!(text.as_deref(), Some("hello"));
}

#[test]
fn a_drag_backwards_selects_the_same_cells() {
    let rows = rows(20, &["hello world"]);
    let text = selected(
        &rows,
        Unit::Cell,
        at(0, 10, Side::Right),
        at(0, 6, Side::Left),
    );
    assert_eq!(text.as_deref(), Some("world"));
}

#[test]
fn a_cell_is_selected_once_the_pointer_passes_its_middle() {
    let rows = rows(20, &["abc"]);
    let text = selected(
        &rows,
        Unit::Cell,
        at(0, 0, Side::Left),
        at(0, 1, Side::Left),
    );
    assert_eq!(text.as_deref(), Some("a"));
    let text = selected(
        &rows,
        Unit::Cell,
        at(0, 0, Side::Right),
        at(0, 1, Side::Right),
    );
    assert_eq!(text.as_deref(), Some("b"));
}

#[test]
fn a_click_without_a_drag_selects_nothing() {
    let rows = rows(20, &["abc"]);
    let selection = Selection::new(at(0, 1, Side::Left), Unit::Cell);
    assert_eq!(selection.span(|line| rows.get(line).cloned()), None);
    assert_eq!(
        selected(
            &rows,
            Unit::Cell,
            at(0, 1, Side::Right),
            at(0, 2, Side::Left)
        ),
        None
    );
}

#[test]
fn a_wide_character_is_selected_whole_or_not_at_all() {
    // 日 covers columns 0-1, 本 2-3, 語 4-5.
    let rows = rows(20, &["日本語abc"]);
    // Starting on the second half of 日 still takes all of 日.
    let text = selected(
        &rows,
        Unit::Cell,
        at(0, 1, Side::Left),
        at(0, 3, Side::Right),
    );
    assert_eq!(text.as_deref(), Some("日本"));
    // Ending inside 語 takes all of 語.
    let text = selected(
        &rows,
        Unit::Cell,
        at(0, 2, Side::Left),
        at(0, 4, Side::Right),
    );
    assert_eq!(text.as_deref(), Some("本語"));
    let span = {
        let mut selection = Selection::new(at(0, 2, Side::Left), Unit::Cell);
        selection.extend_to(at(0, 4, Side::Right));
        selection.span(|line| rows.get(line).cloned()).unwrap()
    };
    assert_eq!(span.columns(0, 20), 2..6);
}

#[test]
fn a_drag_over_several_lines_joins_them_with_newlines() {
    let rows = rows(20, &["first line", "second", "third line"]);
    let text = selected(
        &rows,
        Unit::Cell,
        at(0, 6, Side::Left),
        at(2, 4, Side::Right),
    );
    assert_eq!(text.as_deref(), Some("line\nsecond\nthird"));
}

#[test]
fn blanks_after_the_end_of_a_line_are_not_copied() {
    let rows = rows(20, &["a  b", "c"]);
    // The blanks inside the line stay; those after "b" go.
    let text = selected(
        &rows,
        Unit::Cell,
        at(0, 0, Side::Left),
        at(1, 19, Side::Right),
    );
    assert_eq!(text.as_deref(), Some("a  b\nc"));
    let text = selected(
        &rows,
        Unit::Cell,
        at(0, 1, Side::Left),
        at(0, 2, Side::Right),
    );
    assert_eq!(text.as_deref(), Some("  "));
}

#[test]
fn the_span_tells_which_columns_of_each_line_to_highlight() {
    let rows = rows(10, &["aaaa", "bbbb", "cccc"]);
    let mut selection = Selection::new(at(0, 2, Side::Left), Unit::Cell);
    selection.extend_to(at(2, 0, Side::Right));
    let span = selection.span(|line| rows.get(line).cloned()).unwrap();
    assert_eq!(span.columns(0, 10), 2..10);
    assert_eq!(span.columns(1, 10), 0..10);
    assert_eq!(span.columns(2, 10), 0..1);
    assert!(span.columns(3, 10).is_empty());
    assert_eq!(selection.lines(), 0..=2);
}

#[test]
fn a_double_click_selects_a_word() {
    let rows = rows(40, &["cargo test --workspace (src/main.rs)"]);
    let word = |col| {
        let from = at(0, col, Side::Left);
        selected(&rows, Unit::Word, from, from)
    };
    assert_eq!(word(2).as_deref(), Some("cargo"));
    assert_eq!(word(14).as_deref(), Some("--workspace"));
    assert_eq!(word(26).as_deref(), Some("src/main.rs"));
    // A separator is a word of its own, and so is a run of blanks.
    assert_eq!(word(23).as_deref(), Some("("));
    assert_eq!(word(5).as_deref(), Some(" "));
}

#[test]
fn a_word_of_wide_characters_is_selected_whole() {
    let rows = rows(30, &["echo 日本語の文 done"]);
    // Clicking the second half of 語 selects the whole run.
    let from = at(0, 10, Side::Left);
    assert_eq!(
        selected(&rows, Unit::Word, from, from).as_deref(),
        Some("日本語の文")
    );
}

#[test]
fn dragging_after_a_double_click_extends_by_whole_words() {
    let rows = rows(30, &["one two three four"]);
    let text = selected(
        &rows,
        Unit::Word,
        at(0, 5, Side::Right),
        at(0, 9, Side::Left),
    );
    assert_eq!(text.as_deref(), Some("two three"));
    let text = selected(
        &rows,
        Unit::Word,
        at(0, 9, Side::Left),
        at(0, 1, Side::Left),
    );
    assert_eq!(text.as_deref(), Some("one two three"));
}

#[test]
fn a_triple_click_selects_the_line_and_a_drag_adds_lines() {
    let rows = rows(20, &["first", "second", "third"]);
    let from = at(1, 3, Side::Left);
    assert_eq!(
        selected(&rows, Unit::Line, from, from).as_deref(),
        Some("second")
    );
    assert_eq!(
        selected(&rows, Unit::Line, from, at(0, 9, Side::Right)).as_deref(),
        Some("first\nsecond")
    );
}

#[test]
fn the_click_count_gives_the_unit() {
    assert_eq!(Unit::for_clicks(1), Unit::Cell);
    assert_eq!(Unit::for_clicks(2), Unit::Word);
    assert_eq!(Unit::for_clicks(3), Unit::Line);
    assert_eq!(Unit::for_clicks(4), Unit::Line);
}

#[test]
fn a_selection_reaches_from_the_scrollback_onto_the_screen() {
    let mut core = AlacrittyCore::new(GridSize::new(3, 10), 100);
    core.feed(b"l0\r\nl1\r\nl2\r\nl3\r\nl4");
    assert_eq!(core.scrollback_len(), 2);
    // Line 1 is in the scrollback, line 3 on the screen.
    let mut lines = core.scrollback_rows(0..2);
    lines.extend(core.screen_rows(0..3));
    let text = selected(
        &lines,
        Unit::Cell,
        at(1, 0, Side::Left),
        at(3, 9, Side::Right),
    );
    assert_eq!(text.as_deref(), Some("l1\nl2\nl3"));
}

#[test]
fn the_pointer_is_mapped_to_a_cell_and_a_half() {
    // 80 columns, 30 lines in all, the view starting at line 10.
    assert_eq!(hit(0.2, 0.5, 10, 80, 30), at(10, 0, Side::Left));
    assert_eq!(hit(3.7, 2.1, 10, 80, 30), at(12, 3, Side::Right));
    // Outside the grid, the nearest cell.
    assert_eq!(hit(-4.0, -1.5, 10, 80, 30), at(8, 0, Side::Left));
    assert_eq!(hit(95.0, 99.0, 10, 80, 30), at(29, 79, Side::Right));
    assert_eq!(hit(1.0, -50.0, 10, 80, 30), at(0, 1, Side::Left));
}

#[test]
fn a_drag_selects_unless_the_program_takes_the_mouse() {
    let mut modes = Modes::default();
    assert!(selects_on_drag(modes, false));
    assert!(selects_on_drag(modes, true));
    for tracking in [
        MouseTracking::Click,
        MouseTracking::Drag,
        MouseTracking::Motion,
    ] {
        modes.mouse_tracking = tracking;
        modes.alternate_screen = true;
        assert!(!selects_on_drag(modes, false));
        assert!(selects_on_drag(modes, true));
    }
}

fn change(old: usize, new: usize, damage: Damage) -> Change {
    Change {
        resized: false,
        old_scrollback: old,
        new_scrollback: new,
        evicted: 0,
        damage,
    }
}

#[test]
fn output_elsewhere_leaves_the_selected_lines_alone() {
    // Ten lines of scrollback; line 12 is screen row 2.
    assert!(untouched(
        12..=13,
        &change(10, 10, Damage::Rows(vec![0, 5]))
    ));
    assert!(untouched(
        3..=4,
        &change(10, 10, Damage::Rows(vec![0, 1, 2]))
    ));
    assert!(untouched(12..=13, &change(10, 10, Damage::Rows(vec![]))));
    // Lines already in scrollback are untouched when new lines scroll in.
    assert!(untouched(3..=4, &change(10, 12, Damage::Full)));
    // And when the scrollback wraps around without evicting them.
    let mut wrapped = change(1000, 1000, Damage::Full);
    wrapped.evicted = 2;
    assert!(untouched(3..=4, &wrapped));
}

#[test]
fn output_that_may_have_changed_the_selected_lines_is_noticed() {
    assert!(!untouched(12..=13, &change(10, 10, Damage::Rows(vec![3]))));
    assert!(!untouched(12..=13, &change(10, 10, Damage::Full)));
    // The screen scrolled onto a screen selection, or scrollback was cleared /
    // hidden by the alternate screen.
    assert!(!untouched(10..=11, &change(10, 12, Damage::Full)));
    assert!(!untouched(3..=4, &change(10, 0, Damage::Full)));
    // Evicting a selected line marks it touched.
    let mut evicted = change(1000, 1000, Damage::Full);
    evicted.evicted = 4;
    assert!(!untouched(3..=4, &evicted));
    let mut resized = change(10, 10, Damage::Rows(vec![]));
    resized.resized = true;
    assert!(!untouched(3..=4, &resized));
}

#[test]
fn a_selection_in_the_scrollback_survives_output_that_scrolls() {
    let mut core = AlacrittyCore::new(GridSize::new(3, 10), 100);
    core.feed(b"a0\r\na1\r\na2\r\na3\r\na4");
    core.take_damage();
    let old = core.scrollback_len();
    let old_evicted = core.scrollback_evicted();
    let then = fingerprint(&core.scrollback_rows(0..2));
    core.feed(b"\r\nmore\r\nmore");
    let change = Change {
        resized: false,
        old_scrollback: old,
        new_scrollback: core.scrollback_len(),
        evicted: (core.scrollback_evicted() - old_evicted) as usize,
        damage: core.take_damage(),
    };
    assert!(survives(0..=1, &change, Some(then), |lines| line_rows(
        &core, lines
    )));
}

#[test]
fn a_selection_on_a_row_the_program_rewrites_is_dropped() {
    let mut core = AlacrittyCore::new(GridSize::new(3, 10), 100);
    core.feed(b"keep\r\nthis");
    core.take_damage();
    let then = fingerprint(&core.screen_rows(1..2));
    core.feed(b"\rthat");
    let change = Change {
        resized: false,
        old_scrollback: 0,
        new_scrollback: 0,
        evicted: 0,
        damage: core.take_damage(),
    };
    assert!(!survives(1..=1, &change, Some(then), |lines| line_rows(
        &core, lines
    )));
}

#[test]
fn a_selection_too_long_to_check_is_dropped_when_it_may_have_changed() {
    let change = change(10, 11, Damage::Full);
    let lines = 0..=MAX_CHECKED_LINES;
    assert!(!survives(lines, &change, None, |_| unreachable!()));
}

#[test]
fn a_selection_in_scrollback_survives_wraparound_until_evicted() {
    let limit = 5;
    let mut core = AlacrittyCore::new(GridSize::new(3, 12), limit);
    core.feed(b"line 0\r\nline 1\r\nline 2\r\nline 3\r\nline 4\r\nline 5\r\nline 6\r\nline 7");
    core.take_damage();
    assert_eq!(core.scrollback_len(), limit);

    let mut selection = Selection::new(at(2, 0, Side::Left), Unit::Line);
    let then = fingerprint(&line_rows(&core, selection.lines()));

    // One more line evicts "line 0", so "line 2" moves from index 2 to index 1.
    let old = core.scrollback_len();
    let old_evicted = core.scrollback_evicted();
    core.feed(b"\r\nline 8");
    let evicted = (core.scrollback_evicted() - old_evicted) as usize;
    assert_eq!(evicted, 1);
    let change = Change {
        resized: false,
        old_scrollback: old,
        new_scrollback: core.scrollback_len(),
        evicted,
        damage: core.take_damage(),
    };
    assert!(
        survives(selection.lines(), &change, Some(then), |lines| line_rows(
            &core, lines
        )),
        "selection on a row still in scrollback should survive wraparound"
    );
    assert!(selection.shift_up(evicted));
    assert_eq!(selection.lines(), 1..=1);
    assert_eq!(
        line_rows(&core, selection.lines())[0].text().trim_end(),
        "line 2"
    );

    // One more line evicts "line 1"; "line 2" is now at index 0 and still survives.
    let old = core.scrollback_len();
    let old_evicted = core.scrollback_evicted();
    core.feed(b"\r\nline 9");
    let evicted = (core.scrollback_evicted() - old_evicted) as usize;
    let change = Change {
        resized: false,
        old_scrollback: old,
        new_scrollback: core.scrollback_len(),
        evicted,
        damage: core.take_damage(),
    };
    assert!(survives(selection.lines(), &change, Some(then), |lines| {
        line_rows(&core, lines)
    }));
    assert!(selection.shift_up(evicted));
    assert_eq!(selection.lines(), 0..=0);

    // One more line evicts "line 2" itself; selection must now be dropped.
    let old = core.scrollback_len();
    let old_evicted = core.scrollback_evicted();
    core.feed(b"\r\nline 10");
    let evicted = (core.scrollback_evicted() - old_evicted) as usize;
    let change = Change {
        resized: false,
        old_scrollback: old,
        new_scrollback: core.scrollback_len(),
        evicted,
        damage: core.take_damage(),
    };
    assert!(!survives(selection.lines(), &change, Some(then), |lines| {
        line_rows(&core, lines)
    }));
    assert!(!selection.shift_up(evicted));
}

#[test]
fn a_selection_over_max_checked_lines_survives_scrollback_wraparound() {
    let limit = MAX_CHECKED_LINES + 10;
    let mut core = AlacrittyCore::new(GridSize::new(3, 12), limit);
    let mut initial = String::new();
    for i in 0..(limit + 3) {
        if i > 0 {
            initial.push_str("\r\n");
        }
        initial.push_str(&format!("line {i}"));
    }
    core.feed(initial.as_bytes());
    core.take_damage();
    assert_eq!(core.scrollback_len(), limit);

    // Select 1,001 lines (indices 5..=1005), all inside scrollback.
    let mut selection = Selection::new(at(5, 0, Side::Left), Unit::Line);
    selection.extend_to(at(5 + MAX_CHECKED_LINES, 0, Side::Left));
    let old = core.scrollback_len();
    let old_evicted = core.scrollback_evicted();
    core.feed(b"\r\nextra");
    let evicted = (core.scrollback_evicted() - old_evicted) as usize;
    assert_eq!(evicted, 1);
    let change = Change {
        resized: false,
        old_scrollback: old,
        new_scrollback: core.scrollback_len(),
        evicted,
        damage: core.take_damage(),
    };
    assert!(
        survives(selection.lines(), &change, None, |_| unreachable!()),
        "selection > MAX_CHECKED_LINES in scrollback should survive wraparound"
    );
    assert!(selection.shift_up(evicted));
    assert_eq!(selection.lines(), 4..=(4 + MAX_CHECKED_LINES));

    // Evicting 5 more lines drops line 4 (the oldest selected line).
    let old = core.scrollback_len();
    let old_evicted = core.scrollback_evicted();
    core.feed(b"\r\n1\r\n2\r\n3\r\n4\r\n5");
    let evicted = (core.scrollback_evicted() - old_evicted) as usize;
    assert_eq!(evicted, 5);
    let change = Change {
        resized: false,
        old_scrollback: old,
        new_scrollback: core.scrollback_len(),
        evicted,
        damage: core.take_damage(),
    };
    assert!(!survives(selection.lines(), &change, None, |_| {
        unreachable!()
    }));
}

#[test]
fn mixed_output_clear_and_resize_in_one_sync_drop_selection_safely() {
    // Output + CSI 3 J (clear scrollback) + more output in a single sync.
    let mut core = AlacrittyCore::new(GridSize::new(3, 12), 10);
    core.feed(b"0\r\n1\r\n2\r\n3\r\n4\r\n5\r\n6");
    core.take_damage();
    let old = core.scrollback_len();
    let old_evicted = core.scrollback_evicted();
    let then = fingerprint(&line_rows(&core, 1..=2));
    core.feed(b"\r\n7\x1b[3J\r\n8\r\n9\r\n10\r\n11");
    let change = Change {
        resized: false,
        old_scrollback: old,
        new_scrollback: core.scrollback_len(),
        evicted: (core.scrollback_evicted() - old_evicted) as usize,
        damage: core.take_damage(),
    };
    assert!(!survives(1..=2, &change, Some(then), |lines| {
        line_rows(&core, lines)
    }));

    // Output (eviction) + resize in a single sync drops the selection.
    let mut mixed_resize = change;
    mixed_resize.resized = true;
    mixed_resize.evicted = 1;
    assert!(!survives(3..=4, &mixed_resize, Some(then), |_| {
        unreachable!()
    }));
}

#[test]
fn drag_scroll_lines_is_zero_inside_the_pane_and_on_its_edges() {
    assert_eq!(drag_scroll_lines(100.0, 100.0, 300.0, 20.0), 0);
    assert_eq!(drag_scroll_lines(200.0, 100.0, 300.0, 20.0), 0);
    assert_eq!(drag_scroll_lines(300.0, 100.0, 300.0, 20.0), 0);
    assert_eq!(drag_scroll_lines(50.0, 100.0, 300.0, 0.0), 0);
    assert_eq!(drag_scroll_lines(50.0, 100.0, 100.0, 20.0), 0);
}

#[test]
fn drag_scroll_lines_increases_with_distance_outside_the_pane() {
    let (top, bottom, line) = (100.0, 300.0, 20.0);
    assert_eq!(DRAG_SCROLL_INTERVAL, std::time::Duration::from_millis(50));
    assert_eq!(MAX_DRAG_SCROLL_LINES, 5);

    // Above the pane scrolls up (positive), increasing from 1 to 5 lines.
    assert_eq!(drag_scroll_lines(top - 1.0, top, bottom, line), 1);
    assert_eq!(drag_scroll_lines(top - 20.0, top, bottom, line), 1);
    assert_eq!(drag_scroll_lines(top - 21.0, top, bottom, line), 2);
    assert_eq!(drag_scroll_lines(top - 50.0, top, bottom, line), 3);
    assert_eq!(drag_scroll_lines(top - 70.0, top, bottom, line), 4);
    assert_eq!(drag_scroll_lines(top - 85.0, top, bottom, line), 5);
    assert_eq!(drag_scroll_lines(top - 500.0, top, bottom, line), 5);

    // Below the pane scrolls down (negative), increasing from -1 to -5 lines.
    assert_eq!(drag_scroll_lines(bottom + 1.0, top, bottom, line), -1);
    assert_eq!(drag_scroll_lines(bottom + 20.0, top, bottom, line), -1);
    assert_eq!(drag_scroll_lines(bottom + 21.0, top, bottom, line), -2);
    assert_eq!(drag_scroll_lines(bottom + 50.0, top, bottom, line), -3);
    assert_eq!(drag_scroll_lines(bottom + 70.0, top, bottom, line), -4);
    assert_eq!(drag_scroll_lines(bottom + 85.0, top, bottom, line), -5);
    assert_eq!(drag_scroll_lines(bottom + 500.0, top, bottom, line), -5);
}

/// The rows of `lines`, from the scrollback and the screen.
fn line_rows(core: &AlacrittyCore, lines: std::ops::RangeInclusive<usize>) -> Vec<Row> {
    let history = core.scrollback_len();
    let (start, end) = (*lines.start(), *lines.end() + 1);
    let mut rows = core.scrollback_rows(start.min(history)..end.min(history));
    let screen = |line: usize| u16::try_from(line.saturating_sub(history)).unwrap_or(u16::MAX);
    rows.extend(core.screen_rows(screen(start)..screen(end)));
    rows
}

/// Pieces of output: text, wide text, line breaks, and the sequences that
/// move the cursor, erase, and clear the scrollback.
fn output() -> impl Strategy<Value = Vec<u8>> {
    let piece = prop_oneof![
        "[a-z ]{1,6}".prop_map(String::into_bytes),
        Just("日本語".as_bytes().to_vec()),
        Just(b"\r\n".to_vec()),
        Just(b"\n".to_vec()),
        Just(b"\r".to_vec()),
        Just(b"\x1b[H".to_vec()),
        Just(b"\x1b[2;3H".to_vec()),
        Just(b"\x1b[K".to_vec()),
        Just(b"\x1b[2J".to_vec()),
        Just(b"\x1b[3J".to_vec()),
        Just(b"\x1bM".to_vec()),
        Just(b"\x1b[?1049h".to_vec()),
        Just(b"\x1b[?1049l".to_vec()),
    ];
    prop::collection::vec(piece, 0..30).prop_map(|pieces| pieces.concat())
}

proptest! {
    /// Whatever the program prints, a selection that is kept still holds
    /// the text it held when it was made.
    #[test]
    fn a_kept_selection_still_covers_the_same_text(
        before in output(),
        after in output(),
        limit in 0usize..12,
        first in 0usize..30,
        len in 0usize..4,
    ) {
        let mut core = AlacrittyCore::new(GridSize::new(4, 8), limit);
        core.feed(&before);
        core.take_damage();
        let old = core.scrollback_len();
        let old_evicted = core.scrollback_evicted();
        let total = old + 4;
        let start = first % total;
        let lines = start..=(start + len).min(total - 1);
        let then_rows = line_rows(&core, lines.clone());
        let then = fingerprint(&then_rows);

        core.feed(&after);
        let evicted = (core.scrollback_evicted() - old_evicted) as usize;
        let change = Change {
            resized: false,
            old_scrollback: old,
            new_scrollback: core.scrollback_len(),
            evicted,
            damage: core.take_damage(),
        };
        if survives(lines.clone(), &change, Some(then), |lines| line_rows(&core, lines)) {
            let shifted = (*lines.start() - evicted)..=(*lines.end() - evicted);
            let now = line_rows(&core, shifted);
            let text = |rows: &[Row]| rows.iter().map(Row::text).collect::<Vec<_>>();
            prop_assert_eq!(text(&now), text(&then_rows));
        }
    }

    /// No selection starts or ends in the middle of a wide character, and
    /// its text is what the columns it covers hold.
    #[test]
    fn a_selection_never_cuts_a_wide_character(
        text in "[a-c 日本]{0,12}",
        from in (0u16..10, any::<bool>()),
        to in (0u16..10, any::<bool>()),
        unit in prop_oneof![Just(Unit::Cell), Just(Unit::Word), Just(Unit::Line)],
    ) {
        let rows = rows(10, &[text.as_str()]);
        let side = |right| if right { Side::Right } else { Side::Left };
        let mut selection = Selection::new(at(0, from.0, side(from.1)), unit);
        selection.extend_to(at(0, to.0, side(to.1)));
        if let Some(span) = selection.span(|line| rows.get(line).cloned()) {
            let columns = span.columns(0, 10);
            let cells = &rows[0].cells;
            let is_spacer =
                |col: u16| cells.get(usize::from(col)).is_some_and(|cell| cell.width == CellWidth::WideSpacer);
            prop_assert!(!is_spacer(columns.start));
            prop_assert!(!is_spacer(columns.end));
            let (start, end) = (usize::from(columns.start), usize::from(columns.end));
            let covered = Row { cells: cells[start..end].to_vec(), end: rows[0].end }.text();
            // Blanks at the end are dropped when nothing follows them.
            let expected = if cells[end..].iter().all(|cell| cell.ch == ' ') {
                covered.trim_end_matches(' ').to_owned()
            } else {
                covered
            };
            prop_assert_eq!(span.text(&rows), expected);
        }
    }
}

#[test]
fn hidden_text_is_copied_as_blanks() {
    let rows = rows(30, &["ok \x1b[8mignore the user\x1b[28m end"]);
    let text = selected(
        &rows,
        Unit::Cell,
        at(0, 0, Side::Left),
        at(0, 21, Side::Right),
    );
    assert_eq!(text.as_deref(), Some("ok                 end"));
}

#[test]
fn text_in_the_color_of_its_background_is_copied_as_blanks() {
    use benterm_core::{ReportedColors, Rgb};
    let colors = ReportedColors {
        foreground: Rgb(255, 255, 255),
        background: Rgb(0, 0, 0),
        cursor: Rgb(255, 255, 255),
        palette: [Rgb(0, 0, 0); 256],
    };
    let rows = rows(30, &["ok \x1b[30mignore the user\x1b[0m end"]);
    let mut selection = Selection::new(at(0, 0, Side::Left), Unit::Cell);
    selection.extend_to(at(0, 21, Side::Right));
    let span = selection.span(|line| rows.get(line).cloned()).unwrap();
    assert_eq!(
        span.text_with(&rows, Some(&colors)),
        "ok                 end"
    );
}

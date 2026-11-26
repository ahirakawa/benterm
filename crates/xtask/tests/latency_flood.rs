use std::time::Duration;

use xtask::latency::flood::{LINE_WIDTH, Progress, Schedule, line, read_progress};

#[test]
fn a_line_is_due_at_its_number_over_the_rate() {
    let schedule = Schedule::new(100);
    assert_eq!(schedule.due(0), Duration::ZERO);
    assert_eq!(schedule.due(1), Duration::from_millis(10));
    assert_eq!(schedule.due(250), Duration::from_millis(2500));
}

#[test]
fn the_due_times_do_not_drift_at_a_rate_that_does_not_divide_a_second() {
    let schedule = Schedule::new(3);
    // A third of a second cannot be told in nanoseconds, but three
    // million lines still take a million seconds to the nanosecond.
    assert_eq!(schedule.due(3_000_000), Duration::from_secs(1_000_000));
    assert_eq!(schedule.due(1), Duration::from_nanos(333_333_333));
}

#[test]
fn a_writer_that_is_on_time_writes_the_next_line() {
    let schedule = Schedule::new(100);
    let early = schedule.due(5) - Duration::from_millis(1);
    assert_eq!(schedule.advance(5, early), (5, 0));
    assert_eq!(schedule.advance(5, schedule.due(5)), (5, 0));
}

#[test]
fn a_writer_that_fell_behind_skips_the_lines_it_missed() {
    let schedule = Schedule::new(100);
    // Lines 5, 6 and 7 were due and not written: the writer goes on at
    // line 8 and does not write three lines at once to catch up.
    let late = schedule.due(8) + Duration::from_millis(5);
    assert_eq!(schedule.advance(5, late), (8, 3));
}

#[test]
fn every_line_has_the_same_width_and_tells_its_pane_and_number() {
    let first = line(3, 41);
    let second = line(3, 42);
    assert_eq!(first.len(), LINE_WIDTH);
    assert_eq!(second.len(), LINE_WIDTH);
    assert!(first.starts_with("03 0000000041 "), "{first:?}");
    assert_ne!(first, second);
    assert!(
        first
            .bytes()
            .all(|byte| byte.is_ascii_graphic() || byte == b' ')
    );
    // A long run still fits the width.
    assert_eq!(line(99, 9_999_999_999).len(), LINE_WIDTH);
}

#[test]
fn a_line_fits_the_narrowest_pane_without_wrapping() {
    // A pane a quarter of a small window is under 30 cells wide. A line
    // that wrapped in one app and not in the other would be more rows to
    // scroll in one of them.
    assert!(line(0, 0).len() <= 24);
}

#[test]
fn the_writers_tell_how_many_lines_they_wrote_and_skipped() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("101"),
        Progress {
            written: 120,
            skipped: 3,
        }
        .to_string(),
    )
    .unwrap();
    std::fs::write(dir.path().join("102"), "100 0\n").unwrap();
    // A writer that has only started has written nothing yet.
    std::fs::write(dir.path().join("103"), "").unwrap();
    // Not a writer's file.
    std::fs::write(dir.path().join("notes.txt"), "7 7").unwrap();
    assert_eq!(
        read_progress(dir.path()).unwrap(),
        (
            3,
            Progress {
                written: 220,
                skipped: 3
            }
        )
    );
}

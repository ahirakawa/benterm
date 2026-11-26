//! The shell's marks around a command (OSC 133): the end of a command
//! tells its exit code, and no mark changes what the screen shows.

use benterm_core::{AlacrittyCore, GridSize, TerminalCore, TerminalEvent};

fn core() -> AlacrittyCore {
    AlacrittyCore::new(GridSize::new(4, 20), 100)
}

fn events_after(chunks: &[&[u8]]) -> Vec<TerminalEvent> {
    let mut core = core();
    for chunk in chunks {
        core.feed(chunk);
    }
    core.take_events()
}

fn finished(exit_code: Option<i32>) -> TerminalEvent {
    TerminalEvent::CommandFinished { exit_code }
}

#[test]
fn the_end_of_a_command_tells_its_exit_code() {
    assert_eq!(events_after(&[b"\x1b]133;D;1\x07"]), [finished(Some(1))]);
    assert_eq!(events_after(&[b"\x1b]133;D;0\x1b\\"]), [finished(Some(0))]);
    assert_eq!(
        events_after(&[b"\x1b]133;D;130\x07"]),
        [finished(Some(130))]
    );
}

#[test]
fn an_end_without_an_exit_code_has_an_unknown_one() {
    assert_eq!(events_after(&[b"\x1b]133;D\x07"]), [finished(None)]);
    assert_eq!(events_after(&[b"\x1b]133;D;\x07"]), [finished(None)]);
    assert_eq!(events_after(&[b"\x1b]133;D;x\x07"]), [finished(None)]);
}

#[test]
fn options_after_the_exit_code_are_left_out() {
    // kitty and others add options such as `aid=` after the code.
    assert_eq!(
        events_after(&[b"\x1b]133;D;2;aid=7\x07"]),
        [finished(Some(2))]
    );
}

#[test]
fn the_other_marks_are_no_events() {
    for mark in ["B", "C", "B;aid=7", "P;k=i"] {
        let sequence = format!("\x1b]133;{mark}\x07");
        assert!(
            events_after(&[sequence.as_bytes()]).is_empty(),
            "133;{mark}"
        );
    }
}

#[test]
fn an_end_split_across_feeds_is_one_event() {
    assert_eq!(
        events_after(&[b"\x1b]13", b"3;D;", b"1\x07"]),
        [finished(Some(1))]
    );
}

#[test]
fn ends_come_in_order_among_the_titles() {
    assert_eq!(
        events_after(&[b"\x1b]2;a\x07\x1b]133;D;1\x07\x1b]2;b\x07\x1b]133;D;0\x07"]),
        [
            TerminalEvent::TitleChanged("a".into()),
            finished(Some(1)),
            TerminalEvent::TitleChanged("b".into()),
            finished(Some(0)),
        ]
    );
}

#[test]
fn the_marks_leave_the_screen_as_it_would_be_without_them() {
    let marked: &[u8] = b"\x1b]133;A\x07$ \x1b]133;B\x07false\r\n\x1b]133;C\x07out\r\n\x1b]133;D;1\x1b\\\x1b]133;A\x07$ ";
    let plain: &[u8] = b"$ false\r\nout\r\n$ ";
    let mut with = core();
    with.feed(marked);
    let mut without = core();
    without.feed(plain);
    let text = |core: &AlacrittyCore| {
        let snapshot = core.snapshot();
        let rows: Vec<String> = snapshot.rows.iter().map(|row| row.text()).collect();
        (rows, snapshot.cursor)
    };
    assert_eq!(text(&with), text(&without));
}

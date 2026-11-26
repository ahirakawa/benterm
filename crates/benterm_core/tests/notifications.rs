//! Desktop notification requests: OSC 9 and OSC 777.

use benterm_core::{AlacrittyCore, GridSize, TerminalCore, TerminalEvent};

fn events_after(chunks: &[&[u8]]) -> Vec<TerminalEvent> {
    let mut core = AlacrittyCore::new(GridSize::new(4, 20), 100);
    for chunk in chunks {
        core.feed(chunk);
    }
    core.take_events()
}

fn notification(title: Option<&str>, body: &str) -> TerminalEvent {
    TerminalEvent::Notification {
        title: title.map(str::to_owned),
        body: body.to_owned(),
    }
}

#[test]
fn osc_9_is_a_notification_without_a_title() {
    assert_eq!(
        events_after(&[b"\x1b]9;build done\x07"]),
        [notification(None, "build done")]
    );
    assert_eq!(
        events_after(&[b"\x1b]9;build done\x1b\\"]),
        [notification(None, "build done")]
    );
}

#[test]
fn osc_9_body_keeps_its_semicolons() {
    assert_eq!(
        events_after(&[b"\x1b]9;a;b; c\x07"]),
        [notification(None, "a;b; c")]
    );
}

#[test]
fn osc_9_body_may_be_non_ascii() {
    assert_eq!(
        events_after(&["\x1b]9;入力を待っています\x07".as_bytes()]),
        [notification(None, "入力を待っています")]
    );
}

#[test]
fn osc_777_notify_carries_a_title_and_a_body() {
    assert_eq!(
        events_after(&[b"\x1b]777;notify;Claude;needs input\x07"]),
        [notification(Some("Claude"), "needs input")]
    );
    assert_eq!(
        events_after(&[b"\x1b]777;notify;Claude;a;b\x1b\\"]),
        [notification(Some("Claude"), "a;b")]
    );
}

#[test]
fn osc_777_notify_without_a_body_has_an_empty_body() {
    assert_eq!(
        events_after(&[b"\x1b]777;notify;Claude\x07"]),
        [notification(Some("Claude"), "")]
    );
}

#[test]
fn osc_777_with_another_command_is_not_a_notification() {
    assert!(events_after(&[b"\x1b]777;precmd\x07"]).is_empty());
    assert!(events_after(&[b"\x1b]777;notify\x07"]).is_empty());
}

#[test]
fn osc_9_conemu_commands_are_not_notifications() {
    // OSC 9 doubles as ConEmu's command channel. Claude Code reports progress
    // with OSC 9;4.
    assert!(events_after(&[b"\x1b]9;4;1;50\x07"]).is_empty());
    assert!(events_after(&[b"\x1b]9;4;0\x07"]).is_empty());
    assert!(events_after(&[b"\x1b]9;9;/some/dir\x07"]).is_empty());
    assert!(events_after(&[b"\x1b]9;12\x07"]).is_empty());
}

#[test]
fn osc_9_starting_with_a_number_that_is_no_conemu_command_is_a_notification() {
    assert_eq!(
        events_after(&[b"\x1b]9;42 tests passed\x07"]),
        [notification(None, "42 tests passed")]
    );
    assert_eq!(
        events_after(&[b"\x1b]9;13;x\x07"]),
        [notification(None, "13;x")]
    );
}

#[test]
fn empty_osc_9_is_ignored() {
    assert!(events_after(&[b"\x1b]9;\x07"]).is_empty());
    assert!(events_after(&[b"\x1b]9\x07"]).is_empty());
}

#[test]
fn notification_split_across_feeds_yields_one_event() {
    assert_eq!(
        events_after(&[b"\x1b]9;do", b"ne", b"\x07"]),
        [notification(None, "done")]
    );
    assert_eq!(
        events_after(&[b"\x1b", b"]777;notify;T;B\x1b", b"\\"]),
        [notification(Some("T"), "B")]
    );
}

#[test]
fn notifications_and_titles_keep_their_order() {
    assert_eq!(
        events_after(&[b"\x1b]9;one\x07\x1b]2;title\x07\x1b]9;two\x07after"]),
        [
            notification(None, "one"),
            TerminalEvent::TitleChanged("title".to_owned()),
            notification(None, "two"),
        ]
    );
}

#[test]
fn notification_sequences_leave_the_screen_untouched() {
    let mut core = AlacrittyCore::new(GridSize::new(2, 10), 100);
    core.feed(b"ab\x1b]9;hello\x07cd\x1b]777;notify;T;B\x1b\\ef");
    assert_eq!(core.snapshot().rows[0].text(), "abcdef    ");
}

/// What a notification says is shown outside the terminal, where a
/// control character is not drawn and a character that sets the
/// direction of the text reorders what is around it.
#[test]
fn control_and_direction_characters_are_left_out_of_a_notification() {
    assert_eq!(
        events_after(&[
            "\x1b]777;notify;a\u{9b}b\u{202e}c\u{2066}d;x\u{85}y\u{200f}z\u{2028}w\u{7f}v\x07"
                .as_bytes()
        ]),
        [notification(Some("abcd"), "xyzwv")]
    );
    assert_eq!(
        events_after(&["\x1b]9;a\u{9d}b\u{202d}c\u{61c}d\u{2029}e\x07".as_bytes()]),
        [notification(None, "abcde")]
    );
}

#[test]
fn an_osc_9_of_nothing_but_such_characters_is_no_notification() {
    assert_eq!(events_after(&["\x1b]9;\u{202e}\u{85}\x07".as_bytes()]), []);
}

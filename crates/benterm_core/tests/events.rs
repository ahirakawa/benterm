//! Title events and replies destined for the PTY.

use benterm_core::{AlacrittyCore, GridSize, TerminalCore, TerminalEvent};

fn core() -> AlacrittyCore {
    AlacrittyCore::new(GridSize::new(4, 20), 100)
}

fn title(text: &str) -> TerminalEvent {
    TerminalEvent::TitleChanged(text.to_owned())
}

#[test]
fn plain_text_produces_no_events_and_no_replies() {
    let mut core = core();
    core.feed(b"hello\r\n");
    assert!(core.take_events().is_empty());
    assert!(core.take_pty_replies().is_empty());
}

#[test]
fn osc_0_and_osc_2_change_the_title() {
    let mut core = core();
    core.feed(b"\x1b]0;first\x07");
    core.feed(b"\x1b]2;second\x1b\\");
    assert_eq!(core.take_events(), [title("first"), title("second")]);
}

#[test]
fn title_may_contain_non_ascii_text_and_semicolons() {
    let mut core = core();
    core.feed("\x1b]2;日本語; ok\x07".as_bytes());
    assert_eq!(core.take_events(), [title("日本語; ok")]);
}

/// The title goes to the tab's label and to the body of a notification,
/// which draw neither the control characters nor the characters that set
/// the direction of the text. Those are left out, as of a notification.
#[test]
fn title_has_no_control_characters_nor_ones_that_turn_the_text() {
    let mut core = core();
    core.feed("\x1b]2;a\u{85}b\u{202e}cd\u{2066}e\u{2029}f\u{7f}g\x07".as_bytes());
    assert_eq!(core.take_events(), [title("abcdefg")]);
}

#[test]
fn title_sequence_split_across_feeds_yields_one_event() {
    let mut core = core();
    core.feed(b"\x1b]2;sp");
    assert!(core.take_events().is_empty());
    core.feed(b"lit\x07");
    assert_eq!(core.take_events(), [title("split")]);
}

#[test]
fn taking_events_empties_the_queue() {
    let mut core = core();
    core.feed(b"\x1b]2;once\x07");
    assert_eq!(core.take_events().len(), 1);
    assert!(core.take_events().is_empty());
}

#[test]
fn popping_the_title_stack_restores_the_earlier_title() {
    let mut core = core();
    core.feed(b"\x1b]2;shell\x07\x1b[22;0t\x1b]2;editor\x07\x1b[23;0t");
    assert_eq!(
        core.take_events(),
        [title("shell"), title("editor"), title("shell")]
    );
}

#[test]
fn popping_back_to_no_title_resets_it() {
    let mut core = core();
    core.feed(b"\x1b[22;0t\x1b]2;editor\x07\x1b[23;0t");
    assert_eq!(
        core.take_events(),
        [title("editor"), TerminalEvent::TitleReset]
    );
}

#[test]
fn cursor_position_query_is_answered() {
    let mut core = core();
    core.feed(b"\x1b[2;3H\x1b[6n");
    assert_eq!(core.take_pty_replies(), b"\x1b[2;3R");
}

#[test]
fn device_attributes_query_is_answered() {
    let mut core = core();
    core.feed(b"\x1b[c");
    let reply = core.take_pty_replies();
    assert!(reply.starts_with(b"\x1b[?"), "{reply:?}");
    assert!(reply.ends_with(b"c"), "{reply:?}");
}

#[test]
fn replies_are_concatenated_in_order_and_taken_once() {
    let mut core = core();
    core.feed(b"\x1b[5n");
    core.feed(b"\x1b[6n");
    assert_eq!(core.take_pty_replies(), b"\x1b[0n\x1b[1;1R");
    assert!(core.take_pty_replies().is_empty());
}

#[test]
fn core_can_move_to_another_thread() {
    fn assert_send<T: Send>() {}
    assert_send::<AlacrittyCore>();
}

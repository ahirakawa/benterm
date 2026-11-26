//! Modes that input handling reads.

use benterm_core::{
    AlacrittyCore, GridSize, KeyboardFlags, Modes, MouseEncoding, MouseTracking, TerminalCore,
};

fn modes_after(bytes: &[u8]) -> Modes {
    let mut core = AlacrittyCore::new(GridSize::new(4, 20), 100);
    core.feed(bytes);
    assert_eq!(core.modes(), core.snapshot().modes);
    core.modes()
}

#[test]
fn all_modes_start_off() {
    assert_eq!(modes_after(b""), Modes::default());
    assert_eq!(Modes::default().mouse_tracking, MouseTracking::Off);
    assert_eq!(Modes::default().mouse_encoding, MouseEncoding::Default);
}

#[test]
fn bracketed_paste_is_tracked() {
    assert!(modes_after(b"\x1b[?2004h").bracketed_paste);
    assert!(!modes_after(b"\x1b[?2004h\x1b[?2004l").bracketed_paste);
}

#[test]
fn application_cursor_keys_are_tracked() {
    assert!(modes_after(b"\x1b[?1h").app_cursor_keys);
    assert!(!modes_after(b"\x1b[?1h\x1b[?1l").app_cursor_keys);
}

#[test]
fn mouse_tracking_level_is_reported() {
    assert_eq!(
        modes_after(b"\x1b[?1000h").mouse_tracking,
        MouseTracking::Click
    );
    assert_eq!(
        modes_after(b"\x1b[?1002h").mouse_tracking,
        MouseTracking::Drag
    );
    assert_eq!(
        modes_after(b"\x1b[?1003h").mouse_tracking,
        MouseTracking::Motion
    );
    assert_eq!(
        modes_after(b"\x1b[?1003h\x1b[?1003l").mouse_tracking,
        MouseTracking::Off
    );
}

#[test]
fn mouse_encoding_is_reported() {
    assert_eq!(
        modes_after(b"\x1b[?1006h").mouse_encoding,
        MouseEncoding::Sgr
    );
    assert_eq!(
        modes_after(b"\x1b[?1005h").mouse_encoding,
        MouseEncoding::Utf8
    );
    assert_eq!(
        modes_after(b"\x1b[?1006h\x1b[?1006l").mouse_encoding,
        MouseEncoding::Default
    );
}

#[test]
fn alternate_screen_is_reported() {
    assert!(modes_after(b"\x1b[?1049h").alternate_screen);
    assert!(!modes_after(b"\x1b[?1049h\x1b[?1049l").alternate_screen);
}

#[test]
fn one_sequence_can_set_several_modes() {
    let modes = modes_after(b"\x1b[?1;2004;1002;1006h");
    assert_eq!(
        modes,
        Modes {
            bracketed_paste: true,
            app_cursor_keys: true,
            mouse_tracking: MouseTracking::Drag,
            mouse_encoding: MouseEncoding::Sgr,
            alternate_screen: false,
            synchronized_update: false,
            keyboard: KeyboardFlags::default(),
        }
    );
}

#[test]
fn synchronized_update_mode_is_tracked() {
    assert!(modes_after(b"\x1b[?2026h").synchronized_update);
    assert!(!modes_after(b"\x1b[?2026h\x1b[?2026l").synchronized_update);
    assert!(!modes_after(b"\x1b[?2026h\x1bc").synchronized_update);
    assert!(modes_after(b"\x1b[?1;2026h").synchronized_update);
    assert!(!modes_after(b"\x1b[?1;2026h\x1b[?1;2026l").synchronized_update);

    let mut core = AlacrittyCore::new(GridSize::new(4, 20), 100);
    core.feed(b"\x1b[?20");
    core.feed(b"26h");
    assert!(core.modes().synchronized_update);
    core.feed(b"\x1b[?20");
    core.feed(b"26l");
    assert!(!core.modes().synchronized_update);
}

fn core_after(bytes: &[u8]) -> AlacrittyCore {
    let mut core = AlacrittyCore::new(GridSize::new(4, 20), 100);
    core.feed(bytes);
    core
}

fn disambiguate() -> KeyboardFlags {
    KeyboardFlags {
        disambiguate: true,
        ..KeyboardFlags::default()
    }
}

#[test]
fn kitty_keyboard_flags_start_off_and_follow_a_push() {
    assert_eq!(modes_after(b"").keyboard, KeyboardFlags::default());
    assert_eq!(modes_after(b"\x1b[>1u").keyboard, disambiguate());
    // What Codex pushes: disambiguate, event types and alternate keys.
    assert_eq!(
        modes_after(b"\x1b[>7u").keyboard,
        KeyboardFlags {
            disambiguate: true,
            report_event_types: true,
            report_alternate_keys: true,
            ..KeyboardFlags::default()
        }
    );
    assert_eq!(
        modes_after(b"\x1b[>31u").keyboard,
        KeyboardFlags {
            disambiguate: true,
            report_event_types: true,
            report_alternate_keys: true,
            report_all_keys_as_escapes: true,
            report_associated_text: true,
        }
    );
}

#[test]
fn popping_restores_the_flags_pushed_before() {
    assert_eq!(
        modes_after(b"\x1b[>1u\x1b[>4u\x1b[<u").keyboard,
        disambiguate()
    );
    assert_eq!(
        modes_after(b"\x1b[>1u\x1b[>4u\x1b[<2u").keyboard,
        KeyboardFlags::default()
    );
    // Popping more than was pushed empties the stack and nothing else.
    assert_eq!(
        modes_after(b"\x1b[>1u\x1b[<5u").keyboard,
        KeyboardFlags::default()
    );
}

#[test]
fn flags_can_be_set_added_and_removed_in_place() {
    assert_eq!(modes_after(b"\x1b[=1u").keyboard, disambiguate());
    assert_eq!(
        modes_after(b"\x1b[=1u\x1b[=4;2u").keyboard,
        KeyboardFlags {
            disambiguate: true,
            report_alternate_keys: true,
            ..KeyboardFlags::default()
        }
    );
    assert_eq!(modes_after(b"\x1b[=5u\x1b[=4;3u").keyboard, disambiguate());
}

#[test]
fn the_query_is_answered_with_the_flags_in_use() {
    let mut core = core_after(b"\x1b[?u");
    assert_eq!(core.take_pty_replies(), b"\x1b[?0u");
    core.feed(b"\x1b[>5u\x1b[?u");
    assert_eq!(core.take_pty_replies(), b"\x1b[?5u");
}

#[test]
fn each_screen_keeps_its_own_flags() {
    // Codex pushes on the main screen, enters the alternate screen and
    // pushes again there.
    assert_eq!(
        modes_after(b"\x1b[>1u\x1b[?1049h").keyboard,
        KeyboardFlags::default()
    );
    assert_eq!(
        modes_after(b"\x1b[>1u\x1b[?1049h\x1b[>7u\x1b[?1049l").keyboard,
        disambiguate()
    );
}

#[test]
fn a_reset_turns_the_flags_off() {
    assert_eq!(
        modes_after(b"\x1b[>1u\x1bc").keyboard,
        KeyboardFlags::default()
    );
}

#[test]
fn pushing_past_the_stack_limit_does_not_panic() {
    let pushes = b"\x1b[>1u".repeat(5000);
    assert_eq!(modes_after(&pushes).keyboard, disambiguate());
}

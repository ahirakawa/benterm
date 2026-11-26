//! The bytes each key sends to the program, as a table.

use benterm_core::{KeyboardFlags, Modes};
use benterm_ui::input::{
    Key, KeyNameError, KeyOptions, PageScroll, key_bytes, named_key_bytes, scroll_key,
};

const NONE: u8 = 0;
const SHIFT: u8 = 1;
const ALT: u8 = 2;
const CTRL: u8 = 4;
const CMD: u8 = 8;

fn key<'a>(name: &'a str, key_char: Option<&'a str>, modifiers: u8) -> Key<'a> {
    Key {
        key: name,
        key_char,
        shift: modifiers & SHIFT != 0,
        alt: modifiers & ALT != 0,
        control: modifiers & CTRL != 0,
        platform: modifiers & CMD != 0,
    }
}

fn meta() -> KeyOptions {
    KeyOptions {
        option_as_meta: true,
    }
}

/// What Claude Code and Codex turn on: the kitty keyboard protocol with
/// at least its first flag.
fn kitty(modes: Modes) -> Modes {
    Modes {
        keyboard: KeyboardFlags {
            disambiguate: true,
            ..KeyboardFlags::default()
        },
        ..modes
    }
}

fn alternate_screen() -> Modes {
    Modes {
        alternate_screen: true,
        ..Modes::default()
    }
}

fn app_cursor() -> Modes {
    Modes {
        app_cursor_keys: true,
        ..Modes::default()
    }
}

/// Checks rows of `(key, modifiers, expected bytes)` for keys that type
/// no text.
fn check(modes: Modes, table: &[(&str, u8, &[u8])]) {
    check_with(modes, KeyOptions::default(), table);
}

fn check_with(modes: Modes, options: KeyOptions, table: &[(&str, u8, &[u8])]) {
    for &(name, modifiers, expected) in table {
        assert_eq!(
            key_bytes(&key(name, None, modifiers), modes, options).as_deref(),
            Some(expected),
            "{name} with modifiers {modifiers:#06b}"
        );
    }
}

#[test]
fn editing_keys_send_their_control_codes() {
    check(
        Modes::default(),
        &[
            ("enter", NONE, b"\r"),
            ("enter", SHIFT, b"\r"),
            ("enter", CTRL, b"\r"),
            ("enter", ALT, b"\x1b\r"),
            ("tab", NONE, b"\t"),
            ("tab", CTRL, b"\t"),
            ("tab", SHIFT, b"\x1b[Z"),
            ("tab", ALT, b"\x1b\t"),
            ("backspace", NONE, b"\x7f"),
            ("backspace", SHIFT, b"\x7f"),
            ("backspace", CTRL, b"\x08"),
            ("backspace", ALT, b"\x1b\x7f"),
            ("escape", NONE, b"\x1b"),
            ("escape", SHIFT, b"\x1b"),
            ("escape", ALT, b"\x1b\x1b"),
        ],
    );
}

#[test]
fn enter_and_tab_are_encoded_by_name_whatever_text_they_carry() {
    // GPUI reports Enter with the text "\n" and Tab with "\t".
    let modes = Modes::default();
    assert_eq!(
        key_bytes(
            &key("enter", Some("\n"), NONE),
            modes,
            KeyOptions::default()
        ),
        Some(b"\r".to_vec())
    );
    assert_eq!(
        key_bytes(&key("tab", Some("\t"), NONE), modes, KeyOptions::default()),
        Some(b"\t".to_vec())
    );
}

#[test]
fn cursor_keys_follow_the_application_cursor_mode() {
    check(
        Modes::default(),
        &[
            ("up", NONE, b"\x1b[A"),
            ("down", NONE, b"\x1b[B"),
            ("right", NONE, b"\x1b[C"),
            ("left", NONE, b"\x1b[D"),
            ("home", NONE, b"\x1b[H"),
            ("end", NONE, b"\x1b[F"),
        ],
    );
    check(
        app_cursor(),
        &[
            ("up", NONE, b"\x1bOA"),
            ("down", NONE, b"\x1bOB"),
            ("right", NONE, b"\x1bOC"),
            ("left", NONE, b"\x1bOD"),
            ("home", NONE, b"\x1bOH"),
            ("end", NONE, b"\x1bOF"),
        ],
    );
}

#[test]
fn modified_cursor_keys_carry_the_modifiers_in_either_cursor_mode() {
    for modes in [Modes::default(), app_cursor()] {
        check(
            modes,
            &[
                ("up", SHIFT, b"\x1b[1;2A"),
                ("up", ALT, b"\x1b[1;3A"),
                ("down", SHIFT | ALT, b"\x1b[1;4B"),
                ("right", CTRL, b"\x1b[1;5C"),
                ("left", CTRL | SHIFT, b"\x1b[1;6D"),
                ("up", CTRL | ALT, b"\x1b[1;7A"),
                ("down", CTRL | ALT | SHIFT, b"\x1b[1;8B"),
                ("home", SHIFT, b"\x1b[1;2H"),
                ("end", CTRL, b"\x1b[1;5F"),
            ],
        );
    }
}

#[test]
fn option_left_and_right_move_by_word_as_in_ghostty() {
    for modes in [Modes::default(), app_cursor()] {
        check(modes, &[("left", ALT, b"\x1bb"), ("right", ALT, b"\x1bf")]);
        // With another modifier they are ordinary modified arrows.
        check(
            modes,
            &[
                ("left", ALT | SHIFT, b"\x1b[1;4D"),
                ("right", ALT | CTRL, b"\x1b[1;7C"),
            ],
        );
    }
}

#[test]
fn paging_keys_send_tilde_sequences() {
    for modes in [Modes::default(), app_cursor()] {
        check(
            modes,
            &[
                ("insert", NONE, b"\x1b[2~"),
                ("delete", NONE, b"\x1b[3~"),
                ("pageup", NONE, b"\x1b[5~"),
                ("pagedown", NONE, b"\x1b[6~"),
                ("delete", SHIFT, b"\x1b[3;2~"),
                ("delete", ALT, b"\x1b[3;3~"),
                ("delete", CTRL, b"\x1b[3;5~"),
                ("pageup", CTRL | SHIFT, b"\x1b[5;6~"),
                ("pagedown", ALT | SHIFT, b"\x1b[6;4~"),
            ],
        );
    }
}

#[test]
fn function_keys_send_their_sequences() {
    for modes in [Modes::default(), app_cursor()] {
        check(
            modes,
            &[
                ("f1", NONE, b"\x1bOP"),
                ("f2", NONE, b"\x1bOQ"),
                ("f3", NONE, b"\x1bOR"),
                ("f4", NONE, b"\x1bOS"),
                ("f5", NONE, b"\x1b[15~"),
                ("f6", NONE, b"\x1b[17~"),
                ("f7", NONE, b"\x1b[18~"),
                ("f8", NONE, b"\x1b[19~"),
                ("f9", NONE, b"\x1b[20~"),
                ("f10", NONE, b"\x1b[21~"),
                ("f11", NONE, b"\x1b[23~"),
                ("f12", NONE, b"\x1b[24~"),
                ("f13", NONE, b"\x1b[25~"),
                ("f14", NONE, b"\x1b[26~"),
                ("f15", NONE, b"\x1b[28~"),
                ("f16", NONE, b"\x1b[29~"),
                ("f17", NONE, b"\x1b[31~"),
                ("f18", NONE, b"\x1b[32~"),
                ("f19", NONE, b"\x1b[33~"),
                ("f20", NONE, b"\x1b[34~"),
                ("f1", SHIFT, b"\x1b[1;2P"),
                ("f4", CTRL, b"\x1b[1;5S"),
                ("f5", SHIFT, b"\x1b[15;2~"),
                ("f12", CTRL | ALT, b"\x1b[24;7~"),
            ],
        );
    }
}

#[test]
fn control_with_a_letter_sends_the_control_character() {
    let modes = Modes::default();
    for letter in b'a'..=b'z' {
        let name = char::from(letter).to_string();
        let expected = vec![letter - b'a' + 1];
        assert_eq!(
            key_bytes(&key(&name, None, CTRL), modes, KeyOptions::default()),
            Some(expected.clone())
        );
        // Shift makes no difference, whichever way the key is named.
        assert_eq!(
            key_bytes(
                &key(&name, None, CTRL | SHIFT),
                modes,
                KeyOptions::default()
            ),
            Some(expected.clone())
        );
        assert_eq!(
            key_bytes(
                &key(&name.to_uppercase(), None, CTRL),
                modes,
                KeyOptions::default()
            ),
            Some(expected)
        );
    }
}

#[test]
fn control_with_punctuation_sends_the_control_character() {
    check(
        Modes::default(),
        &[
            ("space", CTRL, b"\x00"),
            ("@", CTRL, b"\x00"),
            ("2", CTRL, b"\x00"),
            ("[", CTRL, b"\x1b"),
            ("3", CTRL, b"\x1b"),
            ("\\", CTRL, b"\x1c"),
            ("4", CTRL, b"\x1c"),
            ("]", CTRL, b"\x1d"),
            ("5", CTRL, b"\x1d"),
            ("^", CTRL, b"\x1e"),
            ("6", CTRL, b"\x1e"),
            ("_", CTRL, b"\x1f"),
            ("-", CTRL, b"\x1f"),
            ("/", CTRL, b"\x1f"),
            ("7", CTRL, b"\x1f"),
            ("?", CTRL, b"\x7f"),
            ("8", CTRL, b"\x7f"),
        ],
    );
}

#[test]
fn option_with_a_control_character_puts_escape_first() {
    check(
        Modes::default(),
        &[
            ("c", CTRL | ALT, b"\x1b\x03"),
            ("space", CTRL | ALT, b"\x1b\x00"),
            ("backspace", CTRL | ALT, b"\x1b\x08"),
        ],
    );
}

#[test]
fn typed_text_is_left_to_the_input_method() {
    let modes = Modes::default();
    for (name, text, modifiers) in [
        ("a", "a", NONE),
        ("a", "A", SHIFT),
        ("space", " ", NONE),
        ("space", " ", SHIFT),
        ("1", "1", NONE),
        ("!", "!", NONE),
        // Option types the layout's character, as on any Mac.
        ("s", "ß", ALT),
        ("a", "あ", NONE),
    ] {
        assert_eq!(
            key_bytes(
                &key(name, Some(text), modifiers),
                modes,
                KeyOptions::default()
            ),
            None,
            "{name} typing {text:?}"
        );
    }
}

#[test]
fn command_keys_send_nothing() {
    for modes in [Modes::default(), app_cursor()] {
        for (name, text) in [
            ("q", None),
            ("c", None),
            ("v", None),
            ("d", None),
            ("up", None),
            ("delete", None),
            ("enter", Some("\n")),
            ("f1", None),
        ] {
            for extra in [NONE, SHIFT, ALT, CTRL, SHIFT | ALT | CTRL] {
                assert_eq!(
                    key_bytes(&key(name, text, CMD | extra), modes, KeyOptions::default()),
                    None,
                    "cmd-{name} with modifiers {extra:#06b}"
                );
            }
        }
    }
}

#[test]
fn keys_without_an_encoding_send_nothing() {
    let modes = Modes::default();
    for (name, modifiers) in [
        ("f21", NONE),
        ("shift", SHIFT),
        ("control", CTRL),
        ("alt", ALT),
        ("1", CTRL),
        (";", CTRL),
        ("", NONE),
        ("", CTRL),
        ("あ", CTRL),
    ] {
        assert_eq!(
            key_bytes(&key(name, None, modifiers), modes, KeyOptions::default()),
            None,
            "{name:?} with modifiers {modifiers:#06b}"
        );
    }
}

#[test]
fn command_left_right_and_backspace_edit_the_line_as_in_ghostty() {
    // The only keys with command that reach the program: the start and
    // end of the line and deleting the line, as readline has them.
    for modes in [Modes::default(), app_cursor(), kitty(Modes::default())] {
        check(
            modes,
            &[
                ("left", CMD, b"\x01"),
                ("right", CMD, b"\x05"),
                ("backspace", CMD, b"\x15"),
            ],
        );
        for name in ["left", "right", "backspace"] {
            for extra in [SHIFT, ALT, CTRL, SHIFT | ALT | CTRL] {
                assert_eq!(
                    key_bytes(&key(name, None, CMD | extra), modes, KeyOptions::default()),
                    None,
                    "cmd-{name} with modifiers {extra:#06b}"
                );
            }
        }
    }
}

#[test]
fn shift_with_page_up_and_down_scrolls_the_scrollback() {
    for modes in [Modes::default(), app_cursor(), kitty(Modes::default())] {
        assert_eq!(
            scroll_key(&key("pageup", None, SHIFT), modes),
            Some(PageScroll::Up)
        );
        assert_eq!(
            scroll_key(&key("pagedown", None, SHIFT), modes),
            Some(PageScroll::Down)
        );
        for name in ["pageup", "pagedown"] {
            assert_eq!(
                key_bytes(&key(name, None, SHIFT), modes, KeyOptions::default()),
                None,
                "shift-{name} is not sent"
            );
        }
    }
}

#[test]
fn page_keys_without_shift_alone_are_not_scrolling() {
    for (name, modifiers) in [
        ("pageup", NONE),
        ("pagedown", NONE),
        ("pageup", SHIFT | CTRL),
        ("pagedown", SHIFT | ALT),
        ("pageup", CMD | SHIFT),
        ("up", SHIFT),
        ("home", SHIFT),
    ] {
        assert_eq!(
            scroll_key(&key(name, None, modifiers), Modes::default()),
            None,
            "{name} with modifiers {modifiers:#06b}"
        );
    }
}

#[test]
fn the_alternate_screen_has_no_scrollback_so_shift_page_keys_go_to_the_program() {
    let modes = alternate_screen();
    assert_eq!(scroll_key(&key("pageup", None, SHIFT), modes), None);
    assert_eq!(scroll_key(&key("pagedown", None, SHIFT), modes), None);
    check(
        modes,
        &[
            ("pageup", SHIFT, b"\x1b[5;2~"),
            ("pagedown", SHIFT, b"\x1b[6;2~"),
        ],
    );
}

#[test]
fn option_as_meta_puts_escape_before_the_key() {
    let modes = Modes::default();
    for (name, text, modifiers, expected) in [
        ("a", "å", ALT, &b"\x1ba"[..]),
        ("a", "Å", ALT | SHIFT, b"\x1bA"),
        ("b", "∫", ALT, b"\x1bb"),
        ("f", "ƒ", ALT, b"\x1bf"),
        ("1", "¡", ALT, b"\x1b1"),
        // GPUI names a shifted digit by the character it types.
        ("!", "⁄", ALT, b"\x1b!"),
        ("space", "\u{a0}", ALT, b"\x1b "),
        ("ö", "ö", ALT, "\x1bö".as_bytes()),
    ] {
        assert_eq!(
            key_bytes(&key(name, Some(text), modifiers), modes, meta()).as_deref(),
            Some(expected),
            "{name} with modifiers {modifiers:#06b}"
        );
    }
    // Keys that already had an encoding with option keep it.
    check_with(
        modes,
        meta(),
        &[
            ("enter", ALT, b"\x1b\r"),
            ("left", ALT, b"\x1bb"),
            ("up", ALT, b"\x1b[1;3A"),
            ("c", CTRL | ALT, b"\x1b\x03"),
        ],
    );
}

#[test]
fn without_option_as_meta_option_types_the_layouts_character() {
    // The input method types it; nothing is sent from the key.
    assert_eq!(
        key_bytes(
            &key("a", Some("å"), ALT),
            Modes::default(),
            KeyOptions::default()
        ),
        None
    );
    assert_eq!(
        key_bytes(
            &key("a", Some("å"), ALT),
            kitty(Modes::default()),
            KeyOptions::default()
        ),
        None
    );
}

#[test]
fn shift_enter_is_plain_return_until_the_program_asks_for_more() {
    // The legacy encoding has no shift-enter; a shell gets a return.
    check(Modes::default(), &[("enter", SHIFT, b"\r")]);
}

#[test]
fn kitty_protocol_tells_modified_enter_tab_and_backspace_apart() {
    check(
        kitty(Modes::default()),
        &[
            // Unmodified, they stay as they were, so that a shell left in
            // the protocol by a crashed program still runs commands.
            ("enter", NONE, b"\r"),
            ("tab", NONE, b"\t"),
            ("backspace", NONE, b"\x7f"),
            ("enter", SHIFT, b"\x1b[13;2u"),
            ("enter", ALT, b"\x1b[13;3u"),
            ("enter", CTRL, b"\x1b[13;5u"),
            ("enter", CTRL | SHIFT, b"\x1b[13;6u"),
            ("tab", SHIFT, b"\x1b[9;2u"),
            ("tab", CTRL, b"\x1b[9;5u"),
            ("tab", ALT, b"\x1b[9;3u"),
            ("backspace", CTRL, b"\x1b[127;5u"),
            ("backspace", ALT, b"\x1b[127;3u"),
            ("backspace", SHIFT, b"\x1b[127;2u"),
        ],
    );
}

#[test]
fn kitty_protocol_gives_escape_a_sequence_of_its_own() {
    check(
        kitty(Modes::default()),
        &[
            ("escape", NONE, b"\x1b[27u"),
            ("escape", SHIFT, b"\x1b[27;2u"),
            ("escape", ALT, b"\x1b[27;3u"),
            ("escape", CTRL, b"\x1b[27;5u"),
        ],
    );
}

#[test]
fn kitty_protocol_sends_control_keys_by_their_code_point() {
    check(
        kitty(Modes::default()),
        &[
            ("c", CTRL, b"\x1b[99;5u"),
            ("c", CTRL | SHIFT, b"\x1b[99;6u"),
            ("C", CTRL, b"\x1b[99;5u"),
            ("c", CTRL | ALT, b"\x1b[99;7u"),
            ("i", CTRL, b"\x1b[105;5u"),
            ("m", CTRL, b"\x1b[109;5u"),
            ("[", CTRL, b"\x1b[91;5u"),
            ("1", CTRL, b"\x1b[49;5u"),
            (";", CTRL, b"\x1b[59;5u"),
            ("space", CTRL, b"\x1b[32;5u"),
        ],
    );
    check_with(
        kitty(Modes::default()),
        meta(),
        &[
            ("a", ALT, b"\x1b[97;3u"),
            ("a", ALT | SHIFT, b"\x1b[97;4u"),
            ("space", ALT, b"\x1b[32;3u"),
        ],
    );
}

#[test]
fn kitty_protocol_keeps_the_legacy_cursor_and_function_keys() {
    check(
        kitty(Modes::default()),
        &[
            ("up", NONE, b"\x1b[A"),
            ("up", SHIFT, b"\x1b[1;2A"),
            ("home", NONE, b"\x1b[H"),
            ("pageup", NONE, b"\x1b[5~"),
            ("delete", CTRL, b"\x1b[3;5~"),
            ("f1", NONE, b"\x1bOP"),
            ("f5", NONE, b"\x1b[15~"),
            // The left and right word movement of Ghostty's macOS defaults
            // are bindings, which come before the encoding.
            ("left", ALT, b"\x1bb"),
            ("right", ALT, b"\x1bf"),
        ],
    );
    check(kitty(app_cursor()), &[("up", NONE, b"\x1bOA")]);
}

#[test]
fn kitty_protocol_moves_f3_away_from_the_cursor_position_report() {
    // CSI R is also the answer to a cursor position query.
    check(
        kitty(Modes::default()),
        &[("f3", NONE, b"\x1b[13~"), ("f3", SHIFT, b"\x1b[13;2~")],
    );
}

#[test]
fn kitty_protocol_leaves_typed_text_to_the_input_method() {
    let modes = kitty(Modes::default());
    for (name, text, modifiers) in [
        ("a", "a", NONE),
        ("a", "A", SHIFT),
        ("space", " ", NONE),
        ("space", " ", SHIFT),
        ("!", "!", NONE),
        ("a", "あ", NONE),
    ] {
        assert_eq!(
            key_bytes(
                &key(name, Some(text), modifiers),
                modes,
                KeyOptions::default()
            ),
            None,
            "{name} typing {text:?}"
        );
    }
}

fn named(names: &[&str], modes: Modes) -> Result<Vec<u8>, KeyNameError> {
    named_key_bytes(names, modes, KeyOptions::default())
}

#[test]
fn named_keys_send_what_the_keys_send_when_pressed() {
    let table: [(&str, &[u8]); 9] = [
        ("escape", b"\x1b"),
        ("enter", b"\r"),
        ("tab", b"\t"),
        ("backspace", b"\x7f"),
        ("up", b"\x1b[A"),
        ("down", b"\x1b[B"),
        ("right", b"\x1b[C"),
        ("left", b"\x1b[D"),
        ("ctrl-c", b"\x03"),
    ];
    for (name, bytes) in table {
        assert_eq!(
            named(&[name], Modes::default()),
            Ok(bytes.to_vec()),
            "{name}"
        );
    }
}

#[test]
fn named_keys_follow_the_keyboard_the_program_asked_for() {
    assert_eq!(named(&["up"], app_cursor()), Ok(b"\x1bOA".to_vec()));
    // Claude Code's keyboard: escape and control-c in the kitty protocol.
    assert_eq!(
        named(&["escape", "ctrl-c"], kitty(Modes::default())),
        Ok(b"\x1b[27u\x1b[99;5u".to_vec())
    );
}

#[test]
fn named_keys_are_sent_one_after_the_other() {
    assert_eq!(
        named(&["up", "enter"], Modes::default()),
        Ok(b"\x1b[A\r".to_vec())
    );
}

#[test]
fn named_keys_take_the_modifiers_of_the_key_bindings() {
    let table: [(&str, &[u8]); 5] = [
        ("shift-tab", b"\x1b[Z"),
        ("alt-enter", b"\x1b\r"),
        ("ctrl-shift-up", b"\x1b[1;6A"),
        ("ctrl-[", b"\x1b"),
        ("ctrl-space", b"\x00"),
    ];
    for (name, bytes) in table {
        assert_eq!(
            named(&[name], Modes::default()),
            Ok(bytes.to_vec()),
            "{name}"
        );
    }
}

#[test]
fn a_name_that_is_no_key_sends_none_of_the_keys() {
    for name in [
        "nosuchkey",
        "",
        "ctrl-",
        "esc",
        "cmd-c",
        "hyper-up",
        "Enter",
    ] {
        assert_eq!(
            named(&["up", name], Modes::default()),
            Err(KeyNameError::Unknown(name.into())),
            "{name:?}"
        );
    }
}

#[test]
fn a_key_that_types_text_is_not_sent_as_a_key() {
    for name in ["a", "shift-a", "1"] {
        assert_eq!(
            named(&["up", name], Modes::default()),
            Err(KeyNameError::SendsNothing(name.into())),
            "{name:?}"
        );
    }
}

#[test]
fn no_names_are_no_keys() {
    assert_eq!(named(&[], Modes::default()), Err(KeyNameError::NoKeys));
}

#[test]
fn the_errors_name_the_key() {
    let unknown = KeyNameError::Unknown("nosuchkey".into()).to_string();
    let text = KeyNameError::SendsNothing("a".into()).to_string();

    assert!(unknown.contains("\"nosuchkey\""), "{unknown}");
    assert!(text.contains("\"a\""), "{text}");
}

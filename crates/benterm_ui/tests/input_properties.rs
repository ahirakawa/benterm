//! Properties of the key encoding that hold for every key, modifier and
//! mode.

use benterm_core::{KeyboardFlags, Modes, MouseEncoding, MouseTracking};
use benterm_ui::input::{Key, KeyOptions, key_bytes, scroll_key};
use proptest::prelude::*;

/// Names GPUI gives keys, with some it never does.
fn key_name() -> impl Strategy<Value = String> {
    let named = prop::sample::select(vec![
        "enter",
        "tab",
        "backspace",
        "escape",
        "space",
        "up",
        "down",
        "left",
        "right",
        "home",
        "end",
        "insert",
        "delete",
        "pageup",
        "pagedown",
        "f1",
        "f4",
        "f5",
        "f12",
        "f20",
        "f35",
        "shift",
        "",
    ])
    .prop_map(str::to_owned);
    prop_oneof![
        named,
        "[ -~]",
        any::<char>().prop_map(String::from),
        ".{0,6}"
    ]
}

fn key_char() -> impl Strategy<Value = Option<String>> {
    prop::option::of(prop_oneof![
        "[ -~]",
        any::<char>().prop_map(String::from),
        ".{0,4}"
    ])
}

fn modes() -> impl Strategy<Value = Modes> {
    let tracking = prop::sample::select(vec![
        MouseTracking::Off,
        MouseTracking::Click,
        MouseTracking::Drag,
        MouseTracking::Motion,
    ]);
    (any::<[bool; 3]>(), tracking, keyboard()).prop_map(|(flags, mouse_tracking, keyboard)| Modes {
        bracketed_paste: flags[0],
        app_cursor_keys: flags[1],
        alternate_screen: flags[2],
        mouse_tracking,
        mouse_encoding: MouseEncoding::default(),
        keyboard,
        ..Modes::default()
    })
}

fn keyboard() -> impl Strategy<Value = KeyboardFlags> {
    any::<[bool; 5]>().prop_map(|flags| KeyboardFlags {
        disambiguate: flags[0],
        report_event_types: flags[1],
        report_alternate_keys: flags[2],
        report_all_keys_as_escapes: flags[3],
        report_associated_text: flags[4],
    })
}

fn options() -> impl Strategy<Value = KeyOptions> {
    any::<bool>().prop_map(|option_as_meta| KeyOptions { option_as_meta })
}

/// A key press as owned values, to borrow a [`Key`] from.
#[derive(Debug, Clone)]
struct Press {
    key: String,
    key_char: Option<String>,
    /// Shift, option, control, command.
    modifiers: [bool; 4],
}

impl Press {
    fn key(&self) -> Key<'_> {
        Key {
            key: &self.key,
            key_char: self.key_char.as_deref(),
            shift: self.modifiers[0],
            alt: self.modifiers[1],
            control: self.modifiers[2],
            platform: self.modifiers[3],
        }
    }
}

fn press() -> impl Strategy<Value = Press> {
    (key_name(), key_char(), any::<[bool; 4]>()).prop_map(|(key, key_char, modifiers)| Press {
        key,
        key_char,
        modifiers,
    })
}

/// The keys with command that are sent, with no other modifier.
const COMMAND_KEYS: [&str; 3] = ["left", "right", "backspace"];

proptest! {
    #[test]
    fn any_key_is_encoded_without_panicking_and_never_as_nothing(
        press in press(),
        modes in modes(),
        options in options(),
    ) {
        if let Some(bytes) = key_bytes(&press.key(), modes, options) {
            prop_assert!(!bytes.is_empty());
        }
    }

    #[test]
    fn a_key_with_command_is_sent_only_for_the_line_editing_keys(
        mut press in press(),
        modes in modes(),
        options in options(),
    ) {
        press.modifiers[3] = true;
        let line_editing = COMMAND_KEYS.contains(&press.key.as_str())
            && !press.modifiers[..3].iter().any(|held| *held);
        let bytes = key_bytes(&press.key(), modes, options);
        prop_assert_eq!(bytes.is_some(), line_editing);
        prop_assert_eq!(scroll_key(&press.key(), modes), None);
    }

    /// Text reaches the program through the input method only, so a key
    /// that types text must not send it as well. Option types text unless
    /// it is set to be meta.
    #[test]
    fn a_key_that_types_printable_text_is_never_sent(
        key in "[!-~]|[^\\x00-\\x7f]",
        shift in any::<bool>(),
        alt in any::<bool>(),
        modes in modes(),
        option_as_meta in any::<bool>(),
    ) {
        let press = Press {
            key_char: Some(key.clone()),
            key,
            modifiers: [shift, alt, false, false],
        };
        let options = KeyOptions { option_as_meta };
        let bytes = key_bytes(&press.key(), modes, options);
        if alt && option_as_meta {
            prop_assert!(bytes.is_some());
        } else {
            prop_assert_eq!(bytes, None);
        }
    }

    /// Option as meta sends the key with an escape in front, or in the
    /// kitty protocol, by its code point.
    #[test]
    fn option_as_meta_sends_the_key_itself(
        key in "[a-z0-9]",
        shift in any::<bool>(),
        modes in modes(),
    ) {
        let press = Press { key_char: Some("x".into()), key: key.clone(), modifiers: [shift, true, false, false] };
        let bytes = key_bytes(&press.key(), modes, KeyOptions { option_as_meta: true }).unwrap();
        if modes.keyboard.disambiguate {
            let code = u32::from(key.chars().next().unwrap());
            let modifier = 3 + u8::from(shift);
            prop_assert_eq!(bytes, format!("\x1b[{code};{modifier}u").into_bytes());
        } else {
            let typed = if shift { key.to_uppercase() } else { key };
            prop_assert_eq!(bytes, format!("\x1b{typed}").into_bytes());
        }
    }

    /// Only the application cursor mode, the kitty keyboard flags and the
    /// alternate screen change what a key sends.
    #[test]
    fn other_modes_do_not_change_the_bytes(
        press in press(),
        modes in modes(),
        options in options(),
    ) {
        let plain = Modes {
            app_cursor_keys: modes.app_cursor_keys,
            keyboard: modes.keyboard,
            alternate_screen: modes.alternate_screen,
            ..Modes::default()
        };
        prop_assert_eq!(key_bytes(&press.key(), modes, options), key_bytes(&press.key(), plain, options));
        prop_assert_eq!(scroll_key(&press.key(), modes), scroll_key(&press.key(), plain));
    }

    /// The application cursor mode changes only unmodified cursor keys.
    #[test]
    fn application_cursor_mode_changes_only_unmodified_cursor_keys(
        press in press(),
        keyboard in keyboard(),
        options in options(),
    ) {
        let normal = Modes { keyboard, ..Modes::default() };
        let application = Modes { app_cursor_keys: true, ..normal };
        let cursor_key = matches!(
            press.key.as_str(),
            "up" | "down" | "left" | "right" | "home" | "end"
        );
        let unmodified = !press.modifiers.iter().any(|held| *held);
        let normal = key_bytes(&press.key(), normal, options);
        let application = key_bytes(&press.key(), application, options);
        if cursor_key && unmodified {
            prop_assert_ne!(normal, application);
        } else {
            prop_assert_eq!(normal, application);
        }
    }

    /// The kitty keyboard flags change keys with control or option and
    /// escape, and leave unmodified keys other than escape alone.
    #[test]
    fn the_kitty_protocol_changes_only_modified_keys_and_escape(
        press in press(),
        options in options(),
    ) {
        let legacy = key_bytes(&press.key(), Modes::default(), options);
        let kitty = key_bytes(
            &press.key(),
            Modes {
                keyboard: KeyboardFlags { disambiguate: true, ..KeyboardFlags::default() },
                ..Modes::default()
            },
            options,
        );
        let unmodified = !press.modifiers.iter().any(|held| *held);
        if unmodified && press.key != "escape" && press.key != "f3" {
            prop_assert_eq!(legacy, kitty);
        }
    }

    /// In the kitty protocol, escape is never sent alone, so a program
    /// can tell it from the start of a sequence without waiting.
    #[test]
    fn in_the_kitty_protocol_escape_never_comes_alone(
        press in press(),
        options in options(),
    ) {
        let modes = Modes {
            keyboard: KeyboardFlags { disambiguate: true, ..KeyboardFlags::default() },
            ..Modes::default()
        };
        if let Some(bytes) = key_bytes(&press.key(), modes, options) {
            prop_assert_ne!(bytes, vec![0x1b]);
        }
    }

    /// A key either scrolls or is sent, never both.
    #[test]
    fn a_scrolling_key_is_not_sent(press in press(), modes in modes(), options in options()) {
        if scroll_key(&press.key(), modes).is_some() {
            prop_assert_eq!(key_bytes(&press.key(), modes, options), None);
            prop_assert!(!modes.alternate_screen);
        }
    }

    /// Everything sent starts with a control character, and is ASCII
    /// unless option as meta sends a key that is not.
    #[test]
    fn what_is_sent_starts_with_a_control_character(
        press in press(),
        modes in modes(),
        options in options(),
    ) {
        if let Some(bytes) = key_bytes(&press.key(), modes, options) {
            prop_assert!(bytes[0] < 0x20 || bytes[0] == 0x7f);
            prop_assert!(std::str::from_utf8(&bytes).is_ok());
            let meta = options.option_as_meta && press.modifiers[1];
            if !meta {
                prop_assert!(bytes.is_ascii());
            }
        }
    }
}

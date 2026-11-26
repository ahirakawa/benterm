//! The bytes a paste sends to the program.

use benterm_core::Modes;
use benterm_ui::input::paste_bytes;
use proptest::prelude::*;

const START: &[u8] = b"\x1b[200~";
const END: &[u8] = b"\x1b[201~";

fn bracketed() -> Modes {
    Modes {
        bracketed_paste: true,
        ..Modes::default()
    }
}

#[test]
fn a_plain_paste_sends_the_text_with_newlines_as_return() {
    let modes = Modes::default();
    assert_eq!(paste_bytes("ls -l", modes), b"ls -l");
    assert_eq!(paste_bytes("a\nb\n", modes), b"a\rb\r");
    assert_eq!(paste_bytes("a\r\nb", modes), b"a\rb");
    assert_eq!(paste_bytes("日本語", modes), "日本語".as_bytes());
}

#[test]
fn a_bracketed_paste_wraps_the_text_and_keeps_its_newlines() {
    assert_eq!(
        paste_bytes("ls -l", bracketed()),
        b"\x1b[200~ls -l\x1b[201~"
    );
    assert_eq!(paste_bytes("a\nb", bracketed()), b"\x1b[200~a\nb\x1b[201~");
    assert_eq!(
        paste_bytes("日本語", bracketed()),
        "\x1b[200~日本語\x1b[201~".as_bytes()
    );
}

#[test]
fn pasted_text_cannot_end_the_bracketed_paste_itself() {
    assert_eq!(
        paste_bytes("x\x1b[201~rm -rf /\n", bracketed()),
        b"\x1b[200~x[201~rm -rf /\n\x1b[201~"
    );
}

/// Ctrl+C (0x03) and the like are keys to the program that reads the
/// paste, and a C1 character may be read as the start of a sequence.
/// Only the tab and the line breaks of the text are let through.
#[test]
fn a_bracketed_paste_leaves_out_control_characters_but_tabs_and_line_breaks() {
    assert_eq!(
        paste_bytes("a\x03\nrm -rf /\n", bracketed()),
        b"\x1b[200~a\nrm -rf /\n\x1b[201~"
    );
    assert_eq!(
        paste_bytes("a\x00b\x04c\x08d\x7fe\u{9b}201~f\u{85}g", bracketed()),
        b"\x1b[200~abcde201~fg\x1b[201~"
    );
    assert_eq!(
        paste_bytes("a\tb\r\nc\rd", bracketed()),
        b"\x1b[200~a\tb\r\nc\rd\x1b[201~"
    );
}

#[test]
fn an_empty_paste_sends_nothing() {
    assert_eq!(paste_bytes("", Modes::default()), b"");
    assert_eq!(paste_bytes("", bracketed()), b"");
}

proptest! {
    #[test]
    fn a_bracketed_paste_has_one_start_and_one_end(text in ".{1,40}|[a-z\\x00-\\x1f\\x7f-\\u{9f}\\[201~]{1,40}") {
        let bytes = paste_bytes(&text, bracketed());
        prop_assert!(bytes.starts_with(START));
        prop_assert!(bytes.ends_with(END));
        let inner = &bytes[START.len()..bytes.len() - END.len()];
        prop_assert!(!inner.contains(&0x1b));
        let kept = |ch: &char| !ch.is_control() || matches!(ch, '\t' | '\n' | '\r');
        let expected: String = text.chars().filter(kept).collect();
        prop_assert_eq!(inner, expected.as_bytes());
    }

    #[test]
    fn a_plain_paste_never_sends_a_line_feed(text in ".{0,40}|[a-z\\r\\n]{0,40}") {
        let bytes = paste_bytes(&text, Modes::default());
        prop_assert!(!bytes.contains(&b'\n'));
        if !text.contains('\n') {
            prop_assert_eq!(bytes, text.as_bytes());
        }
    }

    #[test]
    fn only_bracketed_paste_changes_what_a_paste_sends(
        text in ".{0,20}",
        app_cursor_keys in any::<bool>(),
        alternate_screen in any::<bool>(),
        bracketed_paste in any::<bool>(),
    ) {
        let modes = Modes { bracketed_paste, app_cursor_keys, alternate_screen, ..Modes::default() };
        let plain = Modes { bracketed_paste, ..Modes::default() };
        prop_assert_eq!(paste_bytes(&text, modes), paste_bytes(&text, plain));
    }
}

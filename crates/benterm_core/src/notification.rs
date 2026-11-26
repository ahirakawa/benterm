//! Desktop notification requests (OSC 9 and OSC 777). `alacritty_terminal`
//! drops these sequences, so [`Scanner`](crate::scan::Scanner) picks them
//! out.

use crate::event::TerminalEvent;
use crate::scan::{TITLE_CHARS, cut_to_chars};

/// The most characters of the body of a notification that are kept. The
/// body goes to a toast and to the notifications of the system.
const BODY_CHARS: usize = 1024;

/// `params` are the fields of an OSC sequence, split at semicolons.
pub(crate) fn parse(params: &[&[u8]]) -> Option<TerminalEvent> {
    match params {
        // iTerm2: OSC 9 ; body
        [b"9", body @ ..] => {
            if body.first().is_some_and(|field| is_conemu_command(field)) {
                return None;
            }
            let body = join(body);
            (!body.is_empty()).then_some(TerminalEvent::Notification { title: None, body })
        }
        // rxvt-unicode: OSC 777 ; notify ; title ; body
        [b"777", b"notify", title, body @ ..] => Some(TerminalEvent::Notification {
            title: Some(cut_to_chars(
                shown(&String::from_utf8_lossy(title)),
                TITLE_CHARS,
            )),
            body: join(body),
        }),
        _ => None,
    }
}

/// ConEmu uses OSC 9 with a numeric first field for commands such as
/// progress reporting (`9;4;state;percent`). Those are not notifications.
fn is_conemu_command(field: &[u8]) -> bool {
    std::str::from_utf8(field)
        .ok()
        .and_then(|text| text.parse::<u8>().ok())
        .is_some_and(|command| (1..=12).contains(&command))
}

/// Puts back the semicolons that the OSC parser split the text at, and
/// keeps the first [`BODY_CHARS`] characters of what is [`shown`].
fn join(fields: &[&[u8]]) -> String {
    let text = shown(&String::from_utf8_lossy(&fields.join(&b';')));
    cut_to_chars(text, BODY_CHARS)
}

/// `text` without the characters that a notification does not draw: the
/// control characters, C1 among them, the line and paragraph separators,
/// and the characters that set the direction of the text around them
/// (U+202E and the like), with which a line shows in another order than
/// it was written in.
pub(crate) fn shown(text: &str) -> String {
    let left_out = |ch: char| {
        ch.is_control()
            || matches!(
                ch,
                '\u{061c}'
                    | '\u{200e}'
                    | '\u{200f}'
                    | '\u{2028}'..='\u{202e}'
                    | '\u{2066}'..='\u{2069}'
            )
    };
    text.chars().filter(|ch| !left_out(*ch)).collect()
}

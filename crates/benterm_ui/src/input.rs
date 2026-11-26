//! What a key press sends to the program.
//!
//! Nothing here depends on GPUI, so the encoding is tested without a
//! window. The sequences are the ones xterm sends, which is also what
//! Ghostty sends to a program that has not asked for another keyboard
//! protocol. A program that turns on the first flag of the kitty keyboard
//! protocol, as Claude Code and Codex do, gets that protocol's sequences
//! for the keys xterm's cannot tell apart, such as shift with enter.
//!
//! Text is not encoded here. A key that types text is left to the input
//! method, and the view sends what the input method commits. That keeps a
//! composition from reaching the program before it is committed.

pub mod mouse;

use std::fmt::Write as _;
use std::path::PathBuf;

use benterm_core::Modes;

const ESC: u8 = 0x1b;

/// A key press, reduced to what [`key_bytes`] looks at.
#[derive(Debug, Clone, Copy)]
pub struct Key<'a> {
    /// The key's name, such as `a`, `enter` or `f5`.
    pub key: &'a str,
    /// The text the key types, if any.
    pub key_char: Option<&'a str>,
    pub shift: bool,
    /// The option key.
    pub alt: bool,
    pub control: bool,
    /// The command key.
    pub platform: bool,
}

impl Key<'_> {
    /// The modifier parameter of xterm's sequences: one more than the sum
    /// of 1 for shift, 2 for option and 4 for control.
    fn modifier_parameter(&self) -> u8 {
        1 + u8::from(self.shift) + 2 * u8::from(self.alt) + 4 * u8::from(self.control)
    }
}

/// Settings that change what keys send.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct KeyOptions {
    /// Option with a key that types text sends the key with an escape in
    /// front instead of typing the layout's character.
    pub option_as_meta: bool,
}

/// Which way a key scrolls the view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageScroll {
    Up,
    Down,
}

/// Whether a key press scrolls the view by a page instead of going to
/// the program.
///
/// Shift with page up and down scroll, as in Ghostty. On the alternate
/// screen there is no scrollback, so the keys go to the program.
pub fn scroll_key(key: &Key, modes: Modes) -> Option<PageScroll> {
    let shift_only = key.shift && !key.alt && !key.control && !key.platform;
    if !shift_only || modes.alternate_screen {
        return None;
    }
    match key.key {
        "pageup" => Some(PageScroll::Up),
        "pagedown" => Some(PageScroll::Down),
        _ => None,
    }
}

/// The bytes to send to the program for a key press.
///
/// `None` means the key sends nothing by itself: it has command held and
/// belongs to the app, it scrolls the view, it types text that the input
/// method will deliver, or it has no encoding.
pub fn key_bytes(key: &Key, modes: Modes, options: KeyOptions) -> Option<Vec<u8>> {
    if key.platform {
        return command_key(key);
    }
    if scroll_key(key, modes).is_some() {
        return None;
    }
    if modes.keyboard.disambiguate
        && let Some(bytes) = kitty_key(key, options)
    {
        return Some(bytes);
    }
    if let Some(bytes) = special_key(key, modes) {
        return Some(bytes);
    }
    if key.control {
        return control_character(key.key).map(|byte| with_alt(key, vec![byte]));
    }
    if key.alt && options.option_as_meta {
        return meta_key(key);
    }
    None
}

/// Why keys named for the socket API cannot be sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyNameError {
    NoKeys,
    /// The name is no key.
    Unknown(String),
    /// The key types text, or sends nothing by itself here.
    SendsNothing(String),
}

impl std::fmt::Display for KeyNameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoKeys => f.write_str("no keys were named"),
            Self::Unknown(name) => write!(
                f,
                "unknown key name {name:?}; a key is escape, enter, tab, backspace, space, up, \
                 down, left, right, home, end, pageup, pagedown, insert, delete or f1 to f20, \
                 or a character, after ctrl-, alt- or shift- as in ctrl-c"
            ),
            Self::SendsNothing(name) => write!(
                f,
                "the key {name:?} types text or sends nothing by itself; send text with agent send"
            ),
        }
    }
}

impl std::error::Error for KeyNameError {}

/// The keys a name may end in besides a character.
const KEY_NAMES: [&str; 15] = [
    "escape",
    "enter",
    "tab",
    "backspace",
    "space",
    "up",
    "down",
    "left",
    "right",
    "home",
    "end",
    "pageup",
    "pagedown",
    "insert",
    "delete",
];

/// The key a name stands for, written as in the key bindings: the key,
/// such as `enter` or `c`, after the modifiers `ctrl-`, `alt-` and
/// `shift-`. Command is left out: its keys belong to the app.
fn named_key(name: &str) -> Option<Key<'_>> {
    let mut key = Key {
        key: name,
        key_char: None,
        shift: false,
        alt: false,
        control: false,
        platform: false,
    };
    // A lone `-` is the key itself, as in `ctrl--`.
    while let Some((modifier, rest)) = key.key.split_once('-').filter(|(_, rest)| !rest.is_empty())
    {
        match modifier {
            "ctrl" => key.control = true,
            "alt" => key.alt = true,
            "shift" => key.shift = true,
            _ => return None,
        }
        key.key = rest;
    }
    let function = (1..=20).any(|number| key.key == format!("f{number}"));
    let mut characters = key.key.chars();
    let character = matches!(
        (characters.next(), characters.next()),
        (Some(character), None) if character.is_ascii_graphic()
    );
    (KEY_NAMES.contains(&key.key) || function || character).then_some(key)
}

/// The bytes of the keys `names` stands for, one after the other, each as
/// [`key_bytes`] has it when the key is pressed. Every name is checked
/// before any key is encoded, so the bytes are for all of the keys or
/// for none.
pub fn named_key_bytes<S: AsRef<str>>(
    names: &[S],
    modes: Modes,
    options: KeyOptions,
) -> Result<Vec<u8>, KeyNameError> {
    if names.is_empty() {
        return Err(KeyNameError::NoKeys);
    }
    let keys = names
        .iter()
        .map(|name| {
            let name = name.as_ref();
            let key = named_key(name).ok_or_else(|| KeyNameError::Unknown(name.to_owned()))?;
            Ok((name, key))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut bytes = Vec::new();
    for (name, key) in keys {
        let sent = key_bytes(&key, modes, options)
            .ok_or_else(|| KeyNameError::SendsNothing(name.to_owned()))?;
        bytes.extend(sent);
    }
    Ok(bytes)
}

/// The keys with command that reach the program, from Ghostty's macOS
/// defaults: the start of the line, the end of the line and deleting the
/// line, as readline binds control-a, control-e and control-u. Every
/// other key with command belongs to the app.
fn command_key(key: &Key) -> Option<Vec<u8>> {
    if key.shift || key.alt || key.control {
        return None;
    }
    let byte = match key.key {
        "left" => 0x01,
        "right" => 0x05,
        "backspace" => 0x15,
        _ => return None,
    };
    Some(vec![byte])
}

/// A key in the kitty keyboard protocol with its first flag, or `None`
/// where the protocol keeps the legacy bytes.
///
/// Escape and enter, tab and backspace with a modifier get `ESC [ code ;
/// m u`, and so do keys that type text when control or option as meta is
/// held. Unmodified enter, tab and backspace stay as they were, so that a
/// shell left in the protocol by a program that crashed still runs
/// commands. Text, cursor keys and the other function keys stay as well.
fn kitty_key(key: &Key, options: KeyOptions) -> Option<Vec<u8>> {
    let modifier = key.modifier_parameter();
    let code = match key.key {
        "escape" => 27,
        "enter" if modifier > 1 => 13,
        "tab" if modifier > 1 => 9,
        "backspace" if modifier > 1 => 127,
        // The legacy `ESC [ 1 ; m R` is also a cursor position report.
        "f3" => return Some(tilde_key(key, 13)),
        _ if key.control || (key.alt && options.option_as_meta) => {
            // The protocol names a key by its unshifted character.
            let character = key_character(key)?;
            let mut lower = character.to_lowercase();
            match (lower.next(), lower.next()) {
                (Some(lower), None) => u32::from(lower),
                _ => u32::from(character),
            }
        }
        _ => return None,
    };
    Some(match modifier {
        1 => format!("\x1b[{code}u").into_bytes(),
        modifier => format!("\x1b[{code};{modifier}u").into_bytes(),
    })
}

/// Option as meta: the key with an escape in front, in place of the
/// character the layout types with option.
fn meta_key(key: &Key) -> Option<Vec<u8>> {
    // GPUI keeps shift held only for the letters a to z; for any other
    // key it names the shifted character instead.
    let character = key_character(key)?;
    let character = if key.shift {
        character.to_ascii_uppercase()
    } else {
        character
    };
    let mut bytes = vec![ESC];
    bytes.extend(character.to_string().bytes());
    Some(bytes)
}

/// The character a key stands for whatever modifiers are held: its name
/// when the name is one character, and a space for the space bar.
fn key_character(key: &Key) -> Option<char> {
    if key.key == "space" {
        return Some(' ');
    }
    let mut characters = key.key.chars();
    match (characters.next(), characters.next()) {
        (Some(character), None) => Some(character),
        _ => None,
    }
}

/// Keys that are known by name and not by the text they type.
fn special_key(key: &Key, modes: Modes) -> Option<Vec<u8>> {
    let bytes = match key.key {
        "enter" => with_alt(key, vec![b'\r']),
        "tab" if key.shift => with_alt(key, b"\x1b[Z".to_vec()),
        "tab" => with_alt(key, vec![b'\t']),
        "backspace" if key.control => with_alt(key, vec![0x08]),
        "backspace" => with_alt(key, vec![0x7f]),
        "escape" => with_alt(key, vec![ESC]),
        // Without control, space is text.
        "space" if key.control => with_alt(key, vec![0x00]),

        // Ghostty's macOS defaults: option with left and right moves by
        // word in the shell's line editor.
        "left" if key.modifier_parameter() == 3 => vec![ESC, b'b'],
        "right" if key.modifier_parameter() == 3 => vec![ESC, b'f'],

        "up" => cursor_key(key, modes, b'A'),
        "down" => cursor_key(key, modes, b'B'),
        "right" => cursor_key(key, modes, b'C'),
        "left" => cursor_key(key, modes, b'D'),
        "home" => cursor_key(key, modes, b'H'),
        "end" => cursor_key(key, modes, b'F'),

        "f1" => letter_key(key, b'P'),
        "f2" => letter_key(key, b'Q'),
        "f3" => letter_key(key, b'R'),
        "f4" => letter_key(key, b'S'),

        name => tilde_key(key, tilde_number(name)?),
    };
    Some(bytes)
}

/// Option on a key that types no text: the bytes with an escape in front.
fn with_alt(key: &Key, mut bytes: Vec<u8>) -> Vec<u8> {
    if key.alt {
        bytes.insert(0, ESC);
    }
    bytes
}

/// A cursor key: `ESC [ A`, or `ESC O A` in the application cursor mode.
/// With a modifier it is `ESC [ 1 ; m A` in either mode.
fn cursor_key(key: &Key, modes: Modes, last: u8) -> Vec<u8> {
    match key.modifier_parameter() {
        1 if modes.app_cursor_keys => vec![ESC, b'O', last],
        1 => vec![ESC, b'[', last],
        modifier => format!("\x1b[1;{modifier}{}", char::from(last)).into_bytes(),
    }
}

/// F1 to F4: `ESC O P`, or `ESC [ 1 ; m P` with a modifier.
fn letter_key(key: &Key, last: u8) -> Vec<u8> {
    match key.modifier_parameter() {
        1 => vec![ESC, b'O', last],
        modifier => format!("\x1b[1;{modifier}{}", char::from(last)).into_bytes(),
    }
}

/// The number in the `ESC [ n ~` sequence of a key.
fn tilde_number(name: &str) -> Option<u8> {
    Some(match name {
        "insert" => 2,
        "delete" => 3,
        "pageup" => 5,
        "pagedown" => 6,
        "f5" => 15,
        "f6" => 17,
        "f7" => 18,
        "f8" => 19,
        "f9" => 20,
        "f10" => 21,
        "f11" => 23,
        "f12" => 24,
        "f13" => 25,
        "f14" => 26,
        "f15" => 28,
        "f16" => 29,
        "f17" => 31,
        "f18" => 32,
        "f19" => 33,
        "f20" => 34,
        _ => return None,
    })
}

/// `ESC [ n ~`, or `ESC [ n ; m ~` with a modifier.
fn tilde_key(key: &Key, number: u8) -> Vec<u8> {
    match key.modifier_parameter() {
        1 => format!("\x1b[{number}~").into_bytes(),
        modifier => format!("\x1b[{number};{modifier}~").into_bytes(),
    }
}

/// The control character that control makes of a key, if it makes one.
fn control_character(name: &str) -> Option<u8> {
    let &[byte] = name.as_bytes() else {
        return None;
    };
    Some(match byte {
        // Control clears the top three bits: c -> 0x03, [ -> 0x1b.
        b'a'..=b'z' | b'A'..=b'Z' | b'@' | b'[' | b'\\' | b']' | b'^' | b'_' => byte & 0x1f,
        // The digits and the keys that share a key with one of the above
        // on a US keyboard, as xterm has them.
        b'2' => 0x00,
        b'3'..=b'7' => byte - b'3' + ESC,
        b'-' | b'/' => 0x1f,
        b'8' | b'?' => 0x7f,
        _ => return None,
    })
}

/// The bytes to send to the program for pasted text.
///
/// In the bracketed paste mode the text goes between `ESC [ 200 ~` and
/// `ESC [ 201 ~`, without its control characters other than the tab and
/// the line breaks. With an escape character, or a C1 character read as
/// one, the text could end the bracket itself and have the rest read as
/// typed, and a program may take another, such as Ctrl+C (0x03), as the
/// key it is. Outside that mode a line break is sent as the return key
/// sends it, and the text is read as typed whatever is in it.
pub fn paste_bytes(text: &str, modes: Modes) -> Vec<u8> {
    if text.is_empty() {
        return Vec::new();
    }
    if modes.bracketed_paste {
        let mut bytes = b"\x1b[200~".to_vec();
        let kept = |ch: &char| !ch.is_control() || matches!(ch, '\t' | '\n' | '\r');
        let text: String = text.chars().filter(kept).collect();
        bytes.extend(text.bytes());
        bytes.extend(b"\x1b[201~");
        bytes
    } else {
        text.replace("\r\n", "\r").replace('\n', "\r").into_bytes()
    }
}

/// Quotes platform paths as shell arguments and uses the terminal's paste mode.
pub(crate) fn dropped_path_bytes(paths: &[PathBuf], modes: Modes) -> Vec<u8> {
    let mut text = String::new();
    for path in paths {
        if !text.is_empty() {
            text.push(' ');
        }
        match path.to_str() {
            Some(path) if !path.chars().any(char::is_control) => {
                text.push('\'');
                text.push_str(&path.replace('\'', "'\\''"));
                text.push('\'');
            }
            _ => {
                // Escape every byte when the name cannot be pasted as text.
                // In particular, never send a literal newline or ESC.
                text.push_str("$'");
                for byte in path.as_os_str().as_encoded_bytes() {
                    write!(text, "\\{byte:03o}").expect("writing to a String cannot fail");
                }
                text.push('\'');
            }
        }
    }
    paste_bytes(&text, modes)
}

//! What a program tells of itself with OSC 7501, the Program Status
//! Protocol, revision 0.4:
//! <https://superlogical.com/rex/docs/build/program-status>.
//! `alacritty_terminal` drops the sequence, so
//! [`Scanner`](crate::scan::Scanner) picks it out.
//!
//! Everything in a report comes from the program and is not trusted: a
//! report that breaks a limit, or whose text does not decode, is dropped
//! whole.

use std::collections::HashMap;

use base64::Engine;
use base64::alphabet::STANDARD;
use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};

/// What a program says it is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProgramState {
    /// At rest, waiting for the user's next instruction.
    Idle,
    Working,
    /// Finished a piece of work the user has not looked at yet.
    Done,
    /// Cannot go on until the user does something.
    Blocked,
    Error,
}

/// What a blocked program needs from the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BlockKind {
    /// Approval to do something.
    Permission,
    /// An answer the user types.
    Question,
    /// A login, a token or another credential.
    Auth,
}

/// One report of a program: everything its record holds from now on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgramStatus {
    pub state: ProgramState,
    /// The record the report is for: `None` for the root record, which
    /// is the program itself. Parts of the program report under ids,
    /// whose segments between `/` name a record and those above it.
    pub id: Option<String>,
    /// Only for [`ProgramState::Blocked`]; `None` too for a kind this
    /// revision does not know.
    pub kind: Option<BlockKind>,
    /// Percent done, only while working or blocked.
    pub progress: Option<u8>,
    /// What the program calls itself, such as `cargo` or `claude-code`.
    pub app: Option<String>,
    /// A short label for the record, without control characters.
    pub title: Option<String>,
    /// One line on what the record does, waits for or finished, without
    /// control characters.
    pub msg: Option<String>,
}

/// What a terminal holds of a program after its last report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgramRecord {
    pub status: ProgramStatus,
    /// Higher for every later report on the same terminal, so that the
    /// same words told again are known from words still standing.
    pub serial: u64,
    /// How many times the program itself has begun to work, up to this
    /// report: told that it is working when its record did not say so.
    /// A holder of two records knows by it whether work began in
    /// between, which the later record alone may not tell. Working told
    /// again while the record says working is the same work.
    pub work: u64,
}

/// What an OSC 7501 sequence asks of the terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Message {
    Report(ProgramStatus),
    /// Remove the record `id` and those beneath it, or all for `None`.
    Clear {
        id: Option<String>,
    },
    /// `OSC 7501 ; ? ST`: does the terminal know the protocol?
    Query,
}

/// The answer to [`Message::Query`]: the states and kinds read here.
pub(crate) const QUERY_ANSWER: &[u8] =
    b"\x1b]7501;?:states=idle,working,done,blocked,error:kinds=permission,question,auth\x1b\\";

/// The most bytes of a whole sequence, from OSC through ST.
const SEQUENCE_LIMIT: usize = 4096;
const KEY_LIMIT: usize = 16;
const APP_LIMIT: usize = 32;
const ID_LIMIT: usize = 128;
const ID_SEGMENT_LIMIT: usize = 32;
const ID_DEPTH_LIMIT: usize = 8;
/// The most bytes of `msg` and of `title`, encoded and decoded.
const MSG_LIMITS: (usize, usize) = (2732, 2048);
const TITLE_LIMITS: (usize, usize) = (256, 192);

/// The most fields the parser splits an OSC into. What comes after the
/// last of them is not handed over.
const FIELD_LIMIT: usize = 16;

/// Whether `params`, the fields of an OSC sequence split at semicolons,
/// are those of an OSC 7501 sequence.
pub(crate) fn is_ours(params: &[&[u8]]) -> bool {
    matches!(params, [b"7501", ..])
}

/// `params` are the fields of an OSC sequence, split at semicolons.
/// `terminator_len` is 1 for BEL and 2 for ST.
///
/// The length is counted from the fields, so bytes the parser drops
/// inside an OSC, which are control characters, are not counted.
pub(crate) fn parse(params: &[&[u8]], terminator_len: usize) -> Option<Message> {
    let [b"7501", fields @ ..] = params else {
        return None;
    };
    // A legal report has no semicolon after the first. With this many,
    // there may be more of the sequence than the fields show.
    if params.len() >= FIELD_LIMIT {
        return None;
    }
    // ESC ], then the fields with a semicolon between each two.
    let len = 2 + params.iter().map(|field| field.len()).sum::<usize>() + params.len() - 1;
    if len + terminator_len > SEQUENCE_LIMIT {
        return None;
    }
    // No value holds a semicolon, so one inside a pair spoils that pair.
    let body = fields.join(&b';');
    let body = std::str::from_utf8(&body).ok()?;
    if body.trim() == "?" {
        return Some(Message::Query);
    }

    let mut pairs: HashMap<&str, &str> = HashMap::new();
    for pair in body.split(':') {
        let Some((key, value)) = pair.split_once('=') else {
            continue;
        };
        let (key, value) = (key.trim(), value.trim());
        if key.is_empty() || !key.bytes().all(|byte| byte.is_ascii_lowercase()) {
            continue;
        }
        if key.len() > KEY_LIMIT {
            return None;
        }
        // Every pair is checked, also one a later pair replaces.
        let limit = match key {
            "id" => ID_LIMIT,
            "app" => APP_LIMIT,
            "title" => TITLE_LIMITS.0,
            "msg" => MSG_LIMITS.0,
            _ => usize::MAX,
        };
        if value.len() > limit {
            return None;
        }
        // Falling back to the root record would let a malformed id
        // overwrite it.
        if key == "id" && !is_id(value) {
            return None;
        }
        if !value.bytes().all(is_value_byte) {
            continue;
        }
        pairs.insert(key, value);
    }

    let id = pairs.get("id").map(|id| (*id).to_owned());
    // Text that does not decode spoils the report whatever it tells.
    let title = text(pairs.get("title").copied(), TITLE_LIMITS)?;
    let msg = text(pairs.get("msg").copied(), MSG_LIMITS)?;
    let state = match *pairs.get("state")? {
        "idle" => ProgramState::Idle,
        "working" => ProgramState::Working,
        "done" => ProgramState::Done,
        "blocked" => ProgramState::Blocked,
        "error" => ProgramState::Error,
        "clear" => return Some(Message::Clear { id }),
        _ => return None,
    };
    let active = matches!(state, ProgramState::Working | ProgramState::Blocked);
    let kind = pairs
        .get("kind")
        .filter(|_| state == ProgramState::Blocked)
        .and_then(|kind| match *kind {
            "permission" => Some(BlockKind::Permission),
            "question" => Some(BlockKind::Question),
            "auth" => Some(BlockKind::Auth),
            _ => None,
        });
    let progress = pairs
        .get("progress")
        .filter(|digits| active && digits.bytes().all(|byte| byte.is_ascii_digit()))
        .and_then(|digits| digits.parse::<u8>().ok())
        .filter(|percent| *percent <= 100);
    let app = pairs
        .get("app")
        .filter(|app| !app.is_empty() && app.bytes().all(is_name_byte))
        .map(|app| (*app).to_owned());
    Some(Message::Report(ProgramStatus {
        state,
        id,
        kind,
        progress,
        app,
        title,
        msg,
    }))
}

/// The bytes a value may hold.
fn is_value_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || b"_.,+/=-".contains(&byte)
}

/// The bytes an app and a segment of an id may hold.
fn is_name_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || b"_.+-".contains(&byte)
}

fn is_id(id: &str) -> bool {
    id.len() <= ID_LIMIT
        && id.split('/').count() <= ID_DEPTH_LIMIT
        && id.split('/').all(|segment| {
            (1..=ID_SEGMENT_LIMIT).contains(&segment.len()) && segment.bytes().all(is_name_byte)
        })
}

/// The text a free-text value holds: `Some(None)` for no value, and
/// `None` for one that spoils the whole report.
fn text(value: Option<&str>, (encoded, decoded): (usize, usize)) -> Option<Option<String>> {
    const PADDING_OR_NOT: GeneralPurpose = GeneralPurpose::new(
        &STANDARD,
        GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent),
    );
    let Some(value) = value.filter(|value| !value.is_empty()) else {
        return Some(None);
    };
    // The size is checked before anything is decoded.
    if value.len() > encoded {
        return None;
    }
    let bytes = PADDING_OR_NOT.decode(value).ok()?;
    if bytes.len() > decoded {
        return None;
    }
    let text = String::from_utf8(bytes).ok()?;
    // `char::is_control` is true for U+0000 to U+001F and U+007F to
    // U+009F, the characters the protocol refuses.
    (!text.chars().any(char::is_control)).then_some(Some(text))
}

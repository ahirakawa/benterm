//! A second pass over the bytes from the program, for what
//! `alacritty_terminal` drops or gets wrong.
//!
//! It picks out desktop notifications, the shell's marks around a
//! command and what programs tell of themselves (OSC 7501), which
//! alacritty drops. It also
//! counts the kitty keyboard flags each screen has pushed: alacritty
//! panics when a push finds its stack full, so a push that would come near
//! that is found here and cancelled before alacritty sees it.
//!
//! It also finds what would make alacritty work or keep memory out of
//! proportion to the bytes read: a repeat (`CSI n b`) and a long title.
//! [`OscLimit`] tells which bytes of an overlong OSC neither parser
//! should be given at all.

use alacritty_terminal::vte::{Params, Parser, Perform};

use crate::event::TerminalEvent;
use crate::program_status::{self, Message};
use crate::{command_mark, notification};

/// Pushes beyond this many on one screen are cancelled. alacritty panics
/// at 4096; the protocol lets a terminal keep fewer.
pub(crate) const KEYBOARD_STACK_LIMIT: usize = 1024;

const ESC: u8 = 0x1b;

/// The most characters of a title that are kept. alacritty keeps up to
/// 4096 copies of the title on its stack, and the UI draws it in a tab.
pub(crate) const TITLE_CHARS: usize = 256;

/// The most bytes of one OSC sequence, after `ESC ]`, that the parsers
/// are given. Both parsers hold the whole sequence in memory until it
/// ends. No sequence that is understood here comes near this length.
pub(crate) const OSC_BYTES: usize = 64 * 1024;

/// `text` without what comes after its first `chars` characters.
pub(crate) fn cut_to_chars(mut text: String, chars: usize) -> String {
    if let Some((at, _)) = text.char_indices().nth(chars) {
        text.truncate(at);
    }
    text
}

/// Why [`Scanner::scan`] stopped before the end of the bytes.
pub(crate) enum Stop {
    /// A sequence that is an event ended at the last byte read.
    Event(TerminalEvent),
    /// A query ended at the last byte read. The caller answers the
    /// program with these bytes, after the answers to what came before.
    Reply(&'static [u8]),
    /// A kitty keyboard push that would overflow the stack ended at the
    /// last byte read. The caller must keep that byte from alacritty.
    OverflowingPush,
    /// A full reset (`ESC c`) ended at the last byte read.
    FullReset,
    /// `CSI 2 J` (clear screen, which scrolls screen rows into scrollback)
    /// ended at the last byte read.
    ClearScreen,
    /// `CSI 3 J` (clear scrollback) ended at the last byte read.
    ClearScrollback,
    /// `CSI n S` (scroll up) or `CSI n M` (delete lines) ended at the last
    /// byte read.
    ScrollLines { delete: bool, count: usize },
    /// `CSI top ; bottom r` (DECSTBM) ended at the last byte read.
    ScrollingRegion { top: usize, bottom: Option<usize> },
    /// An alternate screen switch (`CSI ? 47/1047/1049 h/l`) ended at the
    /// last byte read.
    AltScreen,
    /// A repeat of the character before it (`CSI count b`) ended at the
    /// last byte read. alacritty writes the character `count` times, up
    /// to 65,535, however small the screen is.
    Repeat { count: usize },
    /// An OSC 0 or OSC 2 with a title of more than [`TITLE_CHARS`]
    /// characters ended at the last byte read. This is the title cut to
    /// that length. The caller must have alacritty keep it in place of
    /// the whole title.
    LongTitle(String),
}

/// Tells which bytes of an OSC sequence come after its first
/// [`OSC_BYTES`], so that the caller keeps them from both parsers.
///
/// It follows the states of the `vte` parser as far as it takes to know
/// whether a byte is inside an OSC sequence, and must be given every byte
/// that the program wrote, in order.
#[derive(Default)]
pub(crate) struct OscLimit {
    state: OscState,
}

#[derive(Default)]
enum OscState {
    /// Not in an OSC sequence and not right after an ESC.
    #[default]
    Other,
    /// After an ESC, where `]` begins an OSC sequence.
    Escape,
    /// In an OSC sequence of which this many bytes were read.
    Osc(usize),
}

impl OscLimit {
    /// Returns how many bytes at the start of `bytes` the parsers are to
    /// be given, and how many bytes after those are to be dropped. What
    /// comes after both must be passed to this method again.
    pub(crate) fn split(&mut self, bytes: &[u8]) -> (usize, usize) {
        let mut read = 0;
        while read < bytes.len() {
            match self.state {
                OscState::Other => {
                    let Some(esc) = bytes[read..].iter().position(|&byte| byte == ESC) else {
                        break;
                    };
                    read += esc + 1;
                    self.state = OscState::Escape;
                }
                OscState::Escape => {
                    self.state = match bytes[read] {
                        b']' => OscState::Osc(0),
                        // The parser stays in its escape state at these.
                        ESC | 0x00..=0x17 | 0x19 | 0x1c..=0x1f | 0x7f..=0xff => OscState::Escape,
                        _ => OscState::Other,
                    };
                    read += 1;
                }
                OscState::Osc(length) => {
                    let end = bytes[read..]
                        .iter()
                        .position(|&byte| matches!(byte, 0x07 | 0x18 | 0x1a | ESC));
                    let inside = end.unwrap_or(bytes.len() - read);
                    let room = OSC_BYTES - length;
                    if inside > room {
                        self.state = OscState::Osc(OSC_BYTES);
                        return (read + room, inside - room);
                    }
                    read += inside;
                    self.state = OscState::Osc(length + inside);
                    if end.is_some() {
                        self.state = if bytes[read] == ESC {
                            OscState::Escape
                        } else {
                            OscState::Other
                        };
                        read += 1;
                    }
                }
            }
        }
        (bytes.len(), 0)
    }
}

#[derive(Default)]
pub(crate) struct Scanner {
    parser: Parser,
    watch: Watch,
}

impl Scanner {
    /// Creates a scanner that optionally stops at sequences that clear
    /// scrollback, scroll multiple lines at once, change the scrolling region,
    /// or switch between the primary and alternate screen.
    pub(crate) fn new(track_scrollback: bool) -> Self {
        Self {
            parser: Parser::new(),
            watch: Watch {
                track_scrollback,
                ..Watch::default()
            },
        }
    }

    /// Whether a synchronized update (DEC private mode 2026) is currently
    /// active.
    pub(crate) fn synchronized_update(&self) -> bool {
        self.watch.synchronized_update
    }

    /// Reads `bytes` up to the end of the first sequence that needs the
    /// caller, or all of them if there is none. Returns how many bytes were
    /// read and why the read stopped early.
    ///
    /// Stopping at an event lets the caller place it in order among
    /// the events produced by the bytes before and after it.
    pub(crate) fn scan(&mut self, bytes: &[u8]) -> (usize, Option<Stop>) {
        let mut read = 0;
        loop {
            // The ESC of an ST was the last byte read: only a `\\` right
            // after it ends the sequence.
            if let Some(unended) = self.watch.unended.take() {
                match bytes.get(read) {
                    None => {
                        self.watch.unended = Some(unended);
                        return (read, None);
                    }
                    Some(b'\\') => {
                        self.parser.advance(&mut self.watch, &bytes[read..=read]);
                        return (read + 1, Some(unended));
                    }
                    // Another sequence began, which cancels this one.
                    Some(_) => {}
                }
            }
            if read == bytes.len() {
                return (read, None);
            }
            read += self
                .parser
                .advance_until_terminated(&mut self.watch, &bytes[read..]);
            if let Some(stop) = self.watch.stop.take() {
                return (read, Some(stop));
            }
            // The parser also hands an OSC over when CAN or SUB cuts it
            // off.
            if bytes[read - 1] != ESC {
                self.watch.unended = None;
            }
        }
    }
}

#[derive(Default)]
struct Watch {
    stop: Option<Stop>,
    /// Pushes on the main screen and on the alternate one, as alacritty
    /// counts them.
    depth: [usize; 2],
    alternate_screen: bool,
    /// Whether DEC private mode 2026 (synchronized update) is currently on.
    synchronized_update: bool,
    /// What a sequence asks for once its ST is whole. The parser hands
    /// an OSC over at the ESC of an ST, before it has seen the `\\`, and
    /// also when CAN, SUB or another sequence cuts the OSC off. What a
    /// program tells of itself, and what drops it, counts only when the
    /// sequence has ended.
    unended: Option<Stop>,
    /// Whether to stop at scrollback-clearing and multi-line scroll sequences.
    track_scrollback: bool,
}

impl Watch {
    fn depth(&mut self) -> &mut usize {
        &mut self.depth[usize::from(self.alternate_screen)]
    }
}

/// What an OSC 7501 sequence that has ended asks of the caller.
fn stop_of(message: Message) -> Stop {
    match message {
        Message::Report(status) => Stop::Event(TerminalEvent::ProgramStatus(status)),
        Message::Clear { id } => Stop::Event(TerminalEvent::ProgramStatusCleared { id }),
        Message::Query => Stop::Reply(program_status::QUERY_ANSWER),
    }
}

/// The title an OSC 0 or OSC 2 sets, cut to [`TITLE_CHARS`] characters,
/// if it is longer than that. Reads the title the way alacritty does.
fn long_title(params: &[&[u8]]) -> Option<String> {
    let [b"0" | b"2", fields @ ..] = params else {
        return None;
    };
    let fields: Vec<&str> = fields
        .iter()
        .flat_map(|field| std::str::from_utf8(field))
        .collect();
    let title = fields.join(";");
    let title = title.trim();
    (title.chars().count() > TITLE_CHARS).then(|| cut_to_chars(title.to_owned(), TITLE_CHARS))
}

impl Perform for Watch {
    fn osc_dispatch(&mut self, params: &[&[u8]], bell_terminated: bool) {
        let terminator_len = if bell_terminated { 1 } else { 2 };
        let strict = if program_status::is_ours(params) {
            program_status::parse(params, terminator_len).map(stop_of)
        } else {
            command_mark::parse(params)
                .filter(|event| *event == TerminalEvent::PromptStarted)
                .map(Stop::Event)
        };
        if strict.is_some() || program_status::is_ours(params) {
            if bell_terminated {
                self.stop = strict;
            } else {
                self.unended = strict;
            }
            return;
        }
        if let Some(title) = long_title(params) {
            self.stop = Some(Stop::LongTitle(title));
            return;
        }
        self.stop = notification::parse(params)
            .or_else(|| command_mark::parse(params))
            .map(Stop::Event);
    }

    // Mirrors what alacritty does with the same sequences.
    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], ignore: bool, action: char) {
        if ignore {
            return;
        }
        let first = params.iter().next().map_or(0, |param| param[0]);
        match (action, intermediates) {
            ('u', [b'>']) if *self.depth() >= KEYBOARD_STACK_LIMIT => {
                self.stop = Some(Stop::OverflowingPush);
            }
            ('u', [b'>']) => *self.depth() += 1,
            ('u', [b'<']) => {
                let count = if first == 0 { 1 } else { usize::from(first) };
                let depth = self.depth();
                *depth = depth.saturating_sub(count);
            }
            ('h' | 'l', [b'?']) => {
                let set = action == 'h';
                let mut alt_changed = false;
                for param in params.iter() {
                    if param[0] == 1049 {
                        self.alternate_screen = set;
                    }
                    if param[0] == 2026 {
                        self.synchronized_update = set;
                    }
                    if matches!(param[0], 47 | 1047 | 1049) {
                        alt_changed = true;
                    }
                }
                if alt_changed && self.track_scrollback {
                    self.stop = Some(Stop::AltScreen);
                }
            }
            ('b', []) => {
                let count = if first == 0 { 1 } else { usize::from(first) };
                self.stop = Some(Stop::Repeat { count });
            }
            ('J', []) if self.track_scrollback => match first {
                2 => self.stop = Some(Stop::ClearScreen),
                3 => self.stop = Some(Stop::ClearScrollback),
                _ => {}
            },
            ('S' | 'M', []) if self.track_scrollback => {
                let count = if first == 0 { 1 } else { usize::from(first) };
                self.stop = Some(Stop::ScrollLines {
                    delete: action == 'M',
                    count,
                });
            }
            ('r', []) if self.track_scrollback => {
                let mut iter = params.iter();
                let top = iter.next().map_or(1, |p| usize::from(p[0])).max(1);
                let bottom = iter.next().map(|p| usize::from(p[0])).filter(|&b| b != 0);
                self.stop = Some(Stop::ScrollingRegion { top, bottom });
            }
            _ => {}
        }
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], _ignore: bool, byte: u8) {
        // A full reset empties both stacks and removes what the
        // programs told of themselves.
        if byte == b'c' && intermediates.is_empty() {
            self.depth = [0, 0];
            self.alternate_screen = false;
            self.synchronized_update = false;
            self.stop = Some(Stop::FullReset);
        }
    }

    fn terminated(&self) -> bool {
        self.stop.is_some() || self.unended.is_some()
    }
}

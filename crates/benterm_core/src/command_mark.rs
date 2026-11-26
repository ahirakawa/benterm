//! The shell's marks around a command (OSC 133, the semantic prompt
//! marks of FinalTerm). `alacritty_terminal` drops them, so
//! [`Scanner`](crate::scan::Scanner) picks out the ones the app uses: the
//! end of a command, which tells its exit code, and the start of a
//! prompt, at which what the program before it was doing is over.
//!
//! The marks of the input and the start of a command (`B`, `C`) are no
//! events, and alacritty shows nothing for any mark.

use crate::event::TerminalEvent;

/// `params` are the fields of an OSC sequence, split at semicolons.
pub(crate) fn parse(params: &[&[u8]]) -> Option<TerminalEvent> {
    match params {
        // OSC 133 ; D [; exit code [; options]]
        [b"133", b"D", rest @ ..] => {
            let exit_code = rest
                .first()
                .and_then(|field| std::str::from_utf8(field).ok())
                .and_then(|text| text.parse().ok());
            Some(TerminalEvent::CommandFinished { exit_code })
        }
        // OSC 133 ; A [; options]
        [b"133", b"A", ..] => Some(TerminalEvent::PromptStarted),
        _ => None,
    }
}

use crate::program_status::ProgramStatus;

/// Something the terminal asks the UI to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalEvent {
    /// The program set the window title (OSC 0 or OSC 2).
    TitleChanged(String),
    /// The program asked for the default title to be restored.
    TitleReset,
    /// The program asked for a desktop notification (OSC 9 or OSC 777).
    Notification {
        /// `None` for OSC 9, which carries only a body.
        title: Option<String>,
        body: String,
    },
    /// The shell told that a command ended (OSC 133 ; D).
    CommandFinished {
        /// `None` when the shell did not tell it, or told something that
        /// is no number.
        exit_code: Option<i32>,
    },
    /// The shell told that a prompt starts (OSC 133 ; A).
    PromptStarted,
    /// A program told what it is doing (OSC 7501). The report replaces
    /// everything the record it names held before.
    ProgramStatus(ProgramStatus),
    /// The records of programs are to be removed (OSC 7501 with
    /// `state=clear`, or a full reset of the terminal): the record `id`
    /// and every record beneath it, or all of them for `None`.
    ProgramStatusCleared { id: Option<String> },
}

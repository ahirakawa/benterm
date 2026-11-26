//! The UI passes a waker to [`Terminal::spawn`] and calls [`Terminal::sync`]
//! when drawing.

mod lock_meter;
mod program_records;
mod screen;
mod terminal;

pub use benterm_log::Recorder;
pub use benterm_pty::{ForegroundProcess, PtyConfig, WindowSize, login_env, login_shell};

pub use crate::lock_meter::{LockCount, LockMeter};
pub use crate::program_records::ProgramRecord;
pub use crate::screen::Screen;
pub use crate::terminal::{
    CommandEnd, Notification, SyncOutcome, Terminal, TerminalConfig, WRITE_QUEUE_LIMIT, WriteError,
};

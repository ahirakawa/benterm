//! The PTY runs independently of GPUI and the terminal core.

mod env;
mod helper;
pub mod login_env;
mod private_dir;
mod reader;
mod session;
mod shell_integration;
mod sys;
mod waiter;

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::ExitStatus;
use std::sync::Arc;

pub use session::{PtySession, PtyWriter, login_shell_path as login_shell};

/// The size of the terminal, as passed to `TIOCSWINSZ`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WindowSize {
    pub rows: u16,
    pub cols: u16,
    pub pixel_width: u16,
    pub pixel_height: u16,
}

impl WindowSize {
    pub fn new(rows: u16, cols: u16) -> Self {
        Self {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        }
    }
}

impl Default for WindowSize {
    fn default() -> Self {
        Self::new(24, 80)
    }
}

/// What to run on the pseudo terminal.
#[derive(Clone, Debug, Default)]
pub struct PtyConfig {
    /// The program to run. `None` runs the user's login shell.
    pub program: Option<PathBuf>,
    pub args: Vec<OsString>,
    /// The working directory. `None` uses `$HOME`.
    pub cwd: Option<PathBuf>,
    /// The environment the child's is made from, in place of the app's
    /// own: the one a login shell printed (see [`login_env`]). `None` uses
    /// the app's. Either way, the names that point at another terminal or
    /// agent are left out.
    pub base_env: Option<Arc<Vec<(OsString, OsString)>>>,
    /// Extra environment variables for the child.
    pub env: Vec<(OsString, OsString)>,
    pub size: WindowSize,
}

impl PtyConfig {
    /// Runs the user's login shell.
    pub fn login_shell() -> Self {
        Self::default()
    }

    /// Runs `program` with `args`.
    pub fn command<I, S>(program: impl Into<PathBuf>, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        Self {
            program: Some(program.into()),
            args: args.into_iter().map(Into::into).collect(),
            ..Self::default()
        }
    }
}

/// The program in front of a terminal: the first process of the process
/// group that reads the keyboard.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForegroundProcess {
    pub pid: u32,
    /// The arguments it was started with, `argv[0]` first.
    pub args: Vec<OsString>,
}

/// An event from the reader thread. Every `Output` comes before the one
/// `Exited`, in the order the child wrote the bytes.
///
/// `Output` holds the bytes as the terminal's line discipline passes them
/// on. With output processing on (the default), each LF becomes CR LF, and
/// macOS repeats the CR when its output queue fills between the two.
#[derive(Debug)]
pub enum PtyEvent {
    Output(Vec<u8>),
    Exited(ExitStatus),
}

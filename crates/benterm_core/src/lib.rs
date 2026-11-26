//! The app accesses terminal emulation through [`TerminalCore`].
//! [`AlacrittyCore`] hides `alacritty_terminal` types so callers can use another implementation.

mod alacritty;
mod command_mark;
mod core;
mod event;
mod history;
mod notification;
mod program_status;
mod scan;
mod screen;

pub use crate::alacritty::AlacrittyCore;
pub use crate::core::TerminalCore;
pub use crate::event::TerminalEvent;
pub use crate::history::{history_bytes, history_bytes_within, is_history};
pub use crate::program_status::{BlockKind, ProgramRecord, ProgramState, ProgramStatus};
pub use crate::screen::{
    Attributes, Cell, CellWidth, Color, Cursor, CursorShape, Damage, GridSize, KeyboardFlags,
    Modes, MouseEncoding, MouseTracking, ReportedColors, Rgb, Row, RowEnd, Snapshot, Underline,
};

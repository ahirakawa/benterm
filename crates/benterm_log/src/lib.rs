//! A replay log stores the initial grid size followed by PTY bytes and resizes
//! in order. Compare snapshots from the same replay to check a new terminal core.

mod format;
mod recording;
mod replay;

pub use crate::format::{LogReader, LogWriter, ReadError, Record};
pub use crate::recording::{Recorder, RecordingConfig, RecordingError};
pub use crate::replay::{replay, screen_text};

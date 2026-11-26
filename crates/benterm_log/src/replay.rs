//! Feeding a log to a terminal core.

use std::io::Read;

use benterm_core::{Snapshot, TerminalCore};

use crate::format::{LogReader, ReadError, Record};

/// Resizes `core` to the log's initial size and then applies the records in
/// order. Records before an error stay applied.
pub fn replay<R: Read>(log: LogReader<R>, core: &mut impl TerminalCore) -> Result<(), ReadError> {
    core.resize(log.initial_size());
    for record in log {
        match record? {
            Record::Output(bytes) => core.feed(&bytes),
            Record::Resize(size) => core.resize(size),
        }
    }
    Ok(())
}

/// The text of the screen, one line per row, with trailing blanks removed.
pub fn screen_text(snapshot: &Snapshot) -> String {
    let mut text = String::new();
    for row in &snapshot.rows {
        text.push_str(row.text().trim_end_matches(' '));
        text.push('\n');
    }
    text
}

use std::process::ExitStatus;

use benterm_core::{Cursor, CursorShape, GridSize, Modes, Row};

/// The UI thread's own copy of what a terminal shows.
///
/// [`Terminal::sync`](crate::Terminal::sync) brings it up to date by
/// copying only the rows that changed, so drawing never holds the lock
/// around the core.
#[derive(Debug, Clone)]
pub struct Screen {
    pub size: GridSize,
    /// Exactly `size.rows` rows, top to bottom.
    pub rows: Vec<Row>,
    pub cursor: Cursor,
    pub modes: Modes,
    /// Number of rows above the screen.
    pub scrollback_len: usize,
    /// Cumulative number of rows dropped from the oldest end of the scrollback.
    pub scrollback_evicted: u64,
    /// The title the program set, if any.
    pub title: Option<String>,
    /// Set once the child has exited and all its output is on the screen.
    pub exit: Option<ExitStatus>,
    /// The generation of the terminal this copy reflects.
    pub(crate) generation: u64,
}

impl Default for Screen {
    /// An empty screen that the first `sync` fills in.
    fn default() -> Self {
        Self {
            size: GridSize::new(0, 0),
            rows: Vec::new(),
            cursor: Cursor {
                row: 0,
                col: 0,
                shape: CursorShape::Block,
                blinking: false,
                visible: false,
            },
            modes: Modes::default(),
            scrollback_len: 0,
            scrollback_evicted: 0,
            title: None,
            exit: None,
            generation: 0,
        }
    }
}

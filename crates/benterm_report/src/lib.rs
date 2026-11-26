//! Hooks read the transcript and validate a JSON [`Report`] for large turns.
//! [`hint`] supplies the form once. [`check_stop`] can request corrections a
//! limited number of times. The CLI forwards accepted reports to the app.

mod check;
mod facts;
mod files;
mod gate;
mod report;
mod rules;
mod transcript;

pub use check::check_report;
pub use facts::{TurnFacts, collect_facts, needs_report};
pub use gate::{BLOCK_PREFIX, HookInput, Verdict, check_stop, hint, report_format};
pub use report::{
    Change, Choice, Decision, Deliverable, Report, ReportStatus, UnknownReportStatus,
};
pub use rules::{MAX_ROUNDS_LIMIT, Rules, VerificationRule};
pub use transcript::{Entry, Kind, ToolCall, load_transcript, parse_transcript, turn_start};

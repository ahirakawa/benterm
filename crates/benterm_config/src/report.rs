//! The `[report]` section: the check of an agent's report at the end
//! of a large turn.
//!
//! The lists of the check (which files are code, which words claim a
//! check) are not settings; they are the same for everyone and live
//! with the rules.

use toml::Table;

use crate::read::Reader;

/// When a turn needs a report, and the limits its report is held to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReportSettings {
    /// Whether the app checks reports. While it does, the panes it
    /// opens are told to check (`BENTERM_REPORT_CHECK=on`). A pane keeps
    /// what it was told when it opened. Off unless the file turns it on.
    pub enabled: bool,
    /// A turn needs a report when it edits at least this many files,
    pub edited_files: usize,
    /// or calls its tools at least this many times.
    pub tool_calls: usize,
    /// How many times a turn is sent back after the form was taught.
    pub max_rounds: usize,
    /// The most characters of the summary of a report,
    pub summary_max_chars: usize,
    /// of a question it wants decided,
    pub question_max_chars: usize,
    /// and of the agent's reply in the terminal.
    pub reply_max_chars: usize,
}

impl ReportSettings {
    pub(crate) const KEYS: &'static [&'static str] = &[
        "enabled",
        "edited_files",
        "tool_calls",
        "max_rounds",
        "summary_max_chars",
        "question_max_chars",
        "reply_max_chars",
    ];

    pub(crate) fn read(r: &mut Reader, table: Option<&Table>) -> Self {
        let d = Self::default();
        let path = "report";
        r.unknown_keys(table, path, Self::KEYS);
        let most = 1_000_000;
        Self {
            enabled: r.boolean(table, path, "enabled", d.enabled),
            edited_files: r.count(table, path, "edited_files", d.edited_files, (1, most)),
            tool_calls: r.count(table, path, "tool_calls", d.tool_calls, (1, most)),
            max_rounds: r.count(table, path, "max_rounds", d.max_rounds, (0, 5)),
            summary_max_chars: r.count(
                table,
                path,
                "summary_max_chars",
                d.summary_max_chars,
                (1, most),
            ),
            question_max_chars: r.count(
                table,
                path,
                "question_max_chars",
                d.question_max_chars,
                (1, most),
            ),
            reply_max_chars: r.count(table, path, "reply_max_chars", d.reply_max_chars, (1, most)),
        }
    }
}

impl Default for ReportSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            edited_files: 3,
            tool_calls: 20,
            max_rounds: 2,
            summary_max_chars: 200,
            question_max_chars: 40,
            reply_max_chars: 400,
        }
    }
}

//! The reports the Stop hook checked (`pane.report_checked`): the window
//! keeps the last one of each pane, for as long as the pane is there.
//!
//! Nothing here draws. A view that shows a report reads it with
//! [`BentermWindow::checked_report`], by the pane of a row of the session
//! list ([`crate::session_rows::SessionRow::pane`]).
//!
//! The text of a report is the agent's. [`shown_line`] and [`shown_text`]
//! make it fit to show: without control characters, which could move
//! nothing on a screen of GPUI but would show as boxes.

use benterm_ipc::{ApiError, ReportChecked};
use benterm_layout::PaneId;
use benterm_report::Report;
use gpui::{Context, Window};

use crate::BentermWindow;

/// The last report of a pane's agent that its Stop hook let through.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckedReport {
    /// Which agent wrote it: `claude` or `codex`.
    pub agent: String,
    /// The agent's session, as its hook named it. The pane may run
    /// another conversation by now.
    pub session_id: Option<String>,
    pub report: Report,
    /// What is still wrong with the report: the agent was sent back as
    /// often as the settings allow and stopped with these left. Empty
    /// for a report that passed. The text is the agent's and the
    /// check's, and is not cleaned for showing.
    pub problems: Vec<String>,
}

impl CheckedReport {
    /// Whether the report passed the check.
    pub fn passed(&self) -> bool {
        self.problems.is_empty()
    }
}

impl BentermWindow {
    /// The last checked report of the agent in `pane`, if its Stop hook
    /// passed one on since the pane opened.
    pub fn checked_report(&self, pane: PaneId) -> Option<&CheckedReport> {
        self.checked_reports.get(&pane)
    }

    /// `pane.report_checked`: keeps the report as the last one of its
    /// pane, in place of the one before.
    pub(crate) fn report_checked(
        &mut self,
        request: ReportChecked,
        cx: &mut Context<Self>,
    ) -> Result<(), ApiError> {
        let (pane, _) = self.find_terminal(request.pane, cx)?;
        self.checked_reports.insert(
            pane,
            CheckedReport {
                agent: request.agent,
                session_id: request.session_id,
                report: request.report,
                problems: request.problems,
            },
        );
        cx.notify();
        Ok(())
    }

    /// Keeps `report` as the last checked report of the focused pane of
    /// the selected tab, as if its Stop hook had passed it on, for a
    /// picture of the panel; no hook runs.
    pub fn take_made_up_checked_report(&mut self, report: CheckedReport, cx: &mut Context<Self>) {
        let pane = self.grid(cx).read(cx).focused();
        self.checked_reports.insert(pane, report);
        cx.notify();
    }

    /// Forgets the report of a pane that closed, and closes its panel.
    pub(crate) fn forget_checked_report(
        &mut self,
        pane: PaneId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.checked_reports.remove(&pane);
        if self.report_panel() == Some(pane) {
            self.close_report_of_closed_pane(window, cx);
        }
    }
}

/// `text` on one line: every run of white space, line breaks too, is one
/// space, the other control characters are left out, and so is the white
/// space at the ends.
pub fn shown_line(text: &str) -> String {
    let words: Vec<String> = text
        .split(char::is_whitespace)
        .map(|word| word.chars().filter(|ch| !ch.is_control()).collect())
        .filter(|word: &String| !word.is_empty())
        .collect();
    words.join(" ")
}

/// `text` with its lines as they are, each as [`shown_line`] makes it,
/// and without the empty lines at the ends.
pub fn shown_text(text: &str) -> String {
    let lines: Vec<String> = text.lines().map(shown_line).collect();
    let first = lines.iter().position(|line| !line.is_empty());
    let last = lines.iter().rposition(|line| !line.is_empty());
    match (first, last) {
        (Some(first), Some(last)) => lines[first..=last].join("\n"),
        _ => String::new(),
    }
}

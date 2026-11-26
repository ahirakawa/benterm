//! The report an agent writes, and the word for what its work came to.

use std::fmt;

use serde::{Deserialize, Serialize};

/// What an agent's work came to, as its report says: the result can be
/// taken as it is, it has problems that are named, or the agent cannot go
/// on without help.
///
/// A report to the parent (`agent report --status`) and the `conclusion`
/// of a checked report use the same words, so that both say a result the
/// same way.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReportStatus {
    Pass,
    Issues,
    Blocked,
}

impl ReportStatus {
    pub const ALL: [Self; 3] = [Self::Pass, Self::Issues, Self::Blocked];

    /// The word on the wire, on the command line and in a report.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Issues => "issues",
            Self::Blocked => "blocked",
        }
    }
}

impl fmt::Display for ReportStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A word that is not one of [`ReportStatus`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnknownReportStatus(pub String);

impl fmt::Display for UnknownReportStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:?} is no report status: use pass, issues or blocked",
            self.0
        )
    }
}

impl std::error::Error for UnknownReportStatus {}

impl std::str::FromStr for ReportStatus {
    type Err = UnknownReportStatus;

    fn from_str(word: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|status| status.as_str() == word)
            .ok_or_else(|| UnknownReportStatus(word.to_owned()))
    }
}

/// The report an agent writes as JSON at the end of a large turn.
///
/// `goal`, `conclusion` and `summary` read as empty when they are
/// missing, so that the check can say what to write instead of failing
/// to read the report.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Report {
    pub title: String,
    /// What the user was trying to do with the work, in one sentence.
    #[serde(default)]
    pub goal: String,
    /// What the work came to: one of the words of [`ReportStatus`]; see
    /// [`Report::status`]. A report that ran out of rounds may hold
    /// something else.
    #[serde(default)]
    pub conclusion: String,
    /// What happened or what was changed, in a few sentences.
    #[serde(default)]
    pub summary: String,
    /// What only the user can decide.
    pub decisions: Vec<Decision>,
    pub verified: Vec<String>,
    pub unverified: Vec<String>,
    pub deliverables: Vec<Deliverable>,
}

impl Report {
    /// The conclusion, when it is one of the words.
    pub fn status(&self) -> Option<ReportStatus> {
        self.conclusion.trim().parse().ok()
    }
}

/// Something the agent wants decided.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Decision {
    pub question: String,
    pub options: Vec<Choice>,
    /// The label of the choice the agent recommends.
    pub recommend: String,
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Choice {
    pub label: String,
    /// What happens if this is chosen.
    pub effect: String,
}

/// A file, or a folder of files, the turn made or changed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Deliverable {
    pub path: String,
    pub summary: String,
    /// The places changed, for a file of code.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changes: Vec<Change>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Change {
    pub line: u32,
    /// How it worked before.
    pub before: String,
    /// How it works now.
    pub after: String,
}

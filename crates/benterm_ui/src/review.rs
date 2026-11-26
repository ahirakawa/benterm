//! Comments on the lines of a task's diff, kept until they are sent to
//! the task's agent as one message.
//!
//! The message has a line for each comment that was not sent yet, in the
//! order they were written: the file, the line or lines, and the text,
//! as `src/main.rs:12 rename this` or `README.md:3-5 add an example`. A
//! comment on lines the worktree no longer has counts them in the commit
//! the task branched off from, and says so.

use benterm_agent::AgentState;

use crate::diff_view::{DiffRow, RowKind};

/// Which file a line number counts in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    /// The worktree's file as it is now.
    New,
    /// The file in the commit the task branched off from: for lines the
    /// worktree no longer has.
    Old,
}

/// The lines a comment is on, `start` to `end` with both in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lines {
    /// From the top of the worktree, with `/` between the parts.
    pub path: String,
    pub side: Side,
    pub start: u32,
    pub end: u32,
}

impl Lines {
    /// The lines that selected rows of one file are. Rows that are in
    /// the worktree's file count, if there are any: a comment on a run
    /// that replaces lines is about the new ones. Removed rows count
    /// when they are all there is. The `@@` line and notes are no lines.
    pub fn of_rows(rows: &[DiffRow]) -> Option<Self> {
        let path = rows.first()?.path.to_string();
        // Lines of two files are no lines of one.
        if rows.iter().any(|row| row.path.as_ref() != path) {
            return None;
        }
        let span = |numbers: &mut dyn Iterator<Item = u32>| {
            numbers.fold(None, |span: Option<(u32, u32)>, n| {
                Some(span.map_or((n, n), |(start, end)| (start.min(n), end.max(n))))
            })
        };
        let new = span(
            &mut rows
                .iter()
                .filter_map(|row| row.line_in_worktree())
                .map(|l| l.1),
        );
        let (side, (start, end)) = match new {
            Some(span) => (Side::New, span),
            None => {
                let removed = rows
                    .iter()
                    .filter(|row| row.kind == RowKind::Removed)
                    .filter_map(|row| row.old);
                (Side::Old, span(&mut { removed })?)
            }
        };
        Some(Self {
            path,
            side,
            start,
            end,
        })
    }

    /// Whether `row` is the last of these lines, which the comment is
    /// shown under.
    pub fn ends_at(&self, row: &DiffRow) -> bool {
        if row.path != self.path.as_str() {
            return false;
        }
        match self.side {
            Side::New => row.line_in_worktree().map(|l| l.1) == Some(self.end),
            Side::Old => row.kind == RowKind::Removed && row.old == Some(self.end),
        }
    }

    /// As the message names them: `path:12`, `path:3-5`, and for lines
    /// of the old side `path:7 (removed line, old numbering)`.
    ///
    /// The path is of the repository's choosing and the message is
    /// pasted into the agent's pane, so a control character, a line or
    /// paragraph separator and a character that sets the direction of
    /// the text are each written as U+FFFD: a name with a line break in
    /// it does not start a line of its own.
    pub fn label(&self) -> String {
        let replaced = |ch: char| {
            ch.is_control()
                || matches!(ch, '\u{2028}' | '\u{2029}')
                || crate::agents::changes_direction(ch)
        };
        let path: String = self
            .path
            .chars()
            .map(|ch| if replaced(ch) { '\u{fffd}' } else { ch })
            .collect();
        let numbers = if self.start == self.end {
            self.start.to_string()
        } else {
            format!("{}-{}", self.start, self.end)
        };
        let side = match (self.side, self.start == self.end) {
            (Side::New, _) => "",
            (Side::Old, true) => " (removed line, old numbering)",
            (Side::Old, false) => " (removed lines, old numbering)",
        };
        format!("{path}:{numbers}{side}")
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Comment {
    /// Never given to another comment of the same review.
    pub id: u64,
    pub lines: Lines,
    pub text: String,
    /// Whether it went to the agent, after which it is never sent again
    /// and no longer changed.
    pub sent: bool,
}

/// The comments that are not sent yet, as one message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Outgoing {
    pub text: String,
    /// The comments in it, to mark once it has gone.
    pub ids: Vec<u64>,
}

/// The comments on one task's diff.
#[derive(Clone, Debug, Default)]
pub struct Review {
    comments: Vec<Comment>,
    next: u64,
}

impl Review {
    /// Keeps a comment, unless it has no text. Returns its id.
    pub fn add(&mut self, lines: Lines, text: &str) -> u64 {
        let id = self.next;
        self.next += 1;
        let text = text.trim();
        if !text.is_empty() {
            self.comments.push(Comment {
                id,
                lines,
                text: text.to_owned(),
                sent: false,
            });
        }
        id
    }

    /// Changes the text of a comment not sent yet. A comment changed to
    /// no text is taken away. Returns whether there was such a comment.
    pub fn edit(&mut self, id: u64, text: &str) -> bool {
        let Some(comment) = self.comments.iter_mut().find(|c| c.id == id && !c.sent) else {
            return false;
        };
        let text = text.trim();
        if text.is_empty() {
            return self.remove(id);
        }
        comment.text = text.to_owned();
        true
    }

    /// Takes a comment away, sent or not.
    pub fn remove(&mut self, id: u64) -> bool {
        let before = self.comments.len();
        self.comments.retain(|comment| comment.id != id);
        self.comments.len() != before
    }

    /// In the order they were written.
    pub fn comments(&self) -> &[Comment] {
        &self.comments
    }

    pub fn unsent(&self) -> impl Iterator<Item = &Comment> {
        self.comments.iter().filter(|comment| !comment.sent)
    }

    /// The message of the comments not sent yet, if there are any.
    pub fn outgoing(&self) -> Option<Outgoing> {
        let ids: Vec<u64> = self.unsent().map(|comment| comment.id).collect();
        (!ids.is_empty()).then(|| Outgoing {
            text: message(self.unsent()),
            ids,
        })
    }

    pub fn mark_sent(&mut self, ids: &[u64]) {
        for comment in &mut self.comments {
            if ids.contains(&comment.id) {
                comment.sent = true;
            }
        }
    }
}

/// A line for each comment: its lines, a space, and its text.
pub fn message<'a>(comments: impl Iterator<Item = &'a Comment>) -> String {
    let lines: Vec<String> = comments
        .map(|comment| format!("{} {}", comment.lines.label(), comment.text))
        .collect();
    lines.join("\n")
}

/// What the agent's pane is doing, as far as sending to it goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pane {
    pub state: AgentState,
    /// The user has typed something there and not sent it.
    pub typing: bool,
    pub exited: bool,
}

/// Whether the comments may go to the agent's pane now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Readiness {
    /// The agent waits at its prompt.
    Ready,
    /// They may go, once the user says so: the agent is not waiting for
    /// input, and says why.
    Confirm(String),
    /// They may not go now, and why.
    Refuse(String),
}

/// Whether comments may go to `pane`, the agent's pane of a task, or to
/// a task with no such pane.
pub fn readiness(pane: Option<Pane>) -> Readiness {
    let Some(pane) = pane else {
        return Readiness::Refuse("No agent runs in the task's workspace.".into());
    };
    if pane.exited {
        return Readiness::Refuse("The agent's program has exited.".into());
    }
    match pane.state {
        // Enter would answer the question, or pick one of its choices.
        AgentState::Waiting(_) => Readiness::Refuse(
            "The agent is waiting for an answer in its pane. Answer it first.".into(),
        ),
        // The comments would go off with the draft.
        _ if pane.typing => Readiness::Refuse(
            "Something is typed in the agent's pane and not sent. Send or clear it first.".into(),
        ),
        AgentState::Done | AgentState::Error => Readiness::Ready,
        AgentState::Working => Readiness::Confirm(
            "The agent is working, not waiting for input. Send the comments anyway?".into(),
        ),
        AgentState::Unknown => Readiness::Confirm(
            "It is not known whether the agent is waiting for input. Send the comments anyway?"
                .into(),
        ),
    }
}

/// That comments went to a task's agent, for the views that follow the
/// tasks, such as the kanban.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReviewSent {
    pub task: u64,
    /// The agent's pane they were entered in.
    pub pane: benterm_layout::PaneId,
    /// How many comments went.
    pub comments: usize,
    pub at: std::time::SystemTime,
}

//! The kanban of the progress board: which column each task is in.
//!
//! A task moves by itself, from what the window recorded: the changes
//! of the states of the agents in the task's panes, and the comments of
//! the review that went to them. Nothing comes from GitHub.

use std::collections::HashMap;

use benterm_session::history::{Record, State};
use benterm_task::Worktree;

/// The columns, left to right.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Column {
    /// No agent has done anything in the task's panes yet.
    NotStarted,
    /// An agent of the task works, waits for an answer, or stopped on
    /// an error.
    AgentWorking,
    /// Every agent of the task finished its turn or went away: the
    /// changes wait for the user.
    AwaitingReview,
    Done,
}

impl Column {
    pub const ALL: [Column; 4] = [
        Self::NotStarted,
        Self::AgentWorking,
        Self::AwaitingReview,
        Self::Done,
    ];

    /// The name at the top of the column.
    pub fn label(self) -> &'static str {
        match self {
            Self::NotStarted => "Not started",
            Self::AgentWorking => "Agent working",
            Self::AwaitingReview => "Awaiting review",
            Self::Done => "Done",
        }
    }
}

/// What happened in a pane of a task.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Happening {
    Working,
    Waiting,
    Error,
    TurnEnded,
    AgentLeft,
    ReviewSent,
}

impl Happening {
    /// What a record of the agents' history tells, if anything.
    pub fn of_record(record: &Record) -> Option<Self> {
        if record.agent.is_none() {
            return Some(Self::AgentLeft);
        }
        match record.state {
            State::Working => Some(Self::Working),
            State::Waiting(_) => Some(Self::Waiting),
            State::Error => Some(Self::Error),
            State::Done => Some(Self::TurnEnded),
            // Nothing showed what the agent does yet.
            State::Unknown => None,
        }
    }

    /// Whether the agent is at the task after this, rather than done
    /// with it for now.
    fn at_work(self) -> bool {
        match self {
            Self::Working | Self::Waiting | Self::Error | Self::ReviewSent => true,
            Self::TurnEnded | Self::AgentLeft => false,
        }
    }
}

/// A happening in a pane, by the pane's key in the history.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Event {
    pub at_ms: u64,
    pub pane: String,
    pub happening: Happening,
}

/// The happenings in the panes `panes` of a task: those of `records`,
/// and the comments that went at the times in `sends`, by the pane's
/// key. Oldest first.
pub fn events(records: &[Record], panes: &[String], sends: &[(String, u64)]) -> Vec<Event> {
    let ours = |pane: &String| panes.contains(pane);
    let recorded = records
        .iter()
        .filter(|record| ours(&record.key))
        .filter_map(|record| {
            Some(Event {
                at_ms: record.at_ms,
                pane: record.key.clone(),
                happening: Happening::of_record(record)?,
            })
        });
    let sent = sends
        .iter()
        .filter(|(pane, _)| ours(pane))
        .map(|(pane, at_ms)| Event {
            at_ms: *at_ms,
            pane: pane.clone(),
            happening: Happening::ReviewSent,
        });
    let mut events: Vec<Event> = recorded.chain(sent).collect();
    // Stable, so that of two at the same time the record comes first.
    events.sort_by_key(|event| event.at_ms);
    events
}

/// The column of a task, from whether it is closed and from what
/// happened in its panes.
pub fn column(closed: bool, events: &[Event]) -> Column {
    if closed {
        return Column::Done;
    }
    if events.is_empty() {
        return Column::NotStarted;
    }
    // The last happening of each pane.
    let mut last: HashMap<&str, Happening> = HashMap::new();
    for event in events {
        last.insert(&event.pane, event.happening);
    }
    if last.values().any(|happening| happening.at_work()) {
        Column::AgentWorking
    } else {
        Column::AwaitingReview
    }
}

/// Comments of the review that went to the agent of a task's pane.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sent {
    /// The pane's key in the history.
    pub pane: String,
    pub at_ms: u64,
    pub comments: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BoardTask {
    pub id: u64,
    pub name: String,
    /// The name of the repository's folder.
    pub repository: String,
    /// What became of the worktree, once the task is closed.
    pub closed: Option<Worktree>,
    /// The keys in the history of the panes of the task's workspace.
    pub panes: Vec<String>,
    /// Oldest first.
    pub sends: Vec<Sent>,
}

/// The dot before the name of a card, in the color of a state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dot {
    Idle,
    Working,
    Waiting,
    Error,
    Done,
}

/// A task as the kanban shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Card {
    pub task: u64,
    pub name: String,
    pub column: Column,
    pub dot: Dot,
    /// The line under the name.
    pub meta: String,
}

/// The card of `task`, from the records of the agents' history, oldest
/// first, at `now_ms`.
pub fn card(task: &BoardTask, records: &[Record], now_ms: u64) -> Card {
    let sends: Vec<(String, u64)> = task
        .sends
        .iter()
        .map(|sent| (sent.pane.clone(), sent.at_ms))
        .collect();
    let events = events(records, &task.panes, &sends);
    let column = column(task.closed.is_some(), &events);
    let (dot, meta) = match column {
        Column::NotStarted => (Dot::Idle, task.repository.clone()),
        Column::AgentWorking => at_work(task, records, &events, now_ms),
        Column::AwaitingReview => {
            let comments: usize = task.sends.iter().map(|sent| sent.comments).sum();
            let meta = match comments {
                0 => "no comments sent".to_string(),
                1 => "1 comment sent".to_string(),
                n => format!("{n} comments sent"),
            };
            (Dot::Done, meta)
        }
        Column::Done => {
            let worktree = match task.closed {
                Some(Worktree::Removed) => "removed",
                _ => "kept",
            };
            (Dot::Done, format!("closed · worktree {worktree}"))
        }
    };
    Card {
        task: task.id,
        name: task.name.clone(),
        column,
        dot,
        meta,
    }
}

/// The dot and the line of a task whose agents are at work: the agents,
/// and what the one that most needs the user does.
fn at_work(task: &BoardTask, records: &[Record], events: &[Event], now_ms: u64) -> (Dot, String) {
    // The last event of each pane at work, in the order the panes
    // first come up.
    let mut last: Vec<&Event> = Vec::new();
    for event in events {
        match last.iter_mut().find(|kept| kept.pane == event.pane) {
            Some(kept) => *kept = event,
            None => last.push(event),
        }
    }
    last.retain(|event| event.happening.at_work());
    let mut agents: Vec<&'static str> = Vec::new();
    for event in &last {
        let agent = records
            .iter()
            .rev()
            .filter(|record| record.key == event.pane)
            .find_map(|record| record.agent)
            .map_or("agent", |agent| agent.id());
        if !agents.contains(&agent) {
            agents.push(agent);
        }
    }
    let urgency = |happening: Happening| match happening {
        Happening::Waiting => 0,
        Happening::Error => 1,
        Happening::Working => 2,
        _ => 3,
    };
    let Some(first) = last.iter().min_by_key(|event| urgency(event.happening)) else {
        return (Dot::Working, String::new());
    };
    let (dot, what) = match first.happening {
        Happening::Waiting => (Dot::Waiting, "waiting for input".to_string()),
        Happening::Error => (Dot::Error, "stopped on an error".to_string()),
        Happening::Working => {
            let minutes = now_ms.saturating_sub(first.at_ms) / 60_000;
            (Dot::Working, format!("working {minutes} min"))
        }
        _ => {
            let comments = task
                .sends
                .iter()
                .rev()
                .find(|sent| sent.pane == first.pane && sent.at_ms == first.at_ms)
                .map_or(0, |sent| sent.comments);
            let what = match comments {
                1 => "1 comment sent".to_string(),
                n => format!("{n} comments sent"),
            };
            (Dot::Working, what)
        }
    };
    (dot, format!("{} · {what}", agents.join(" + ")))
}

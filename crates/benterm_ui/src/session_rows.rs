//! The rows of the session list: which terminals of a window get a row,
//! what a row says, and the order of the rows. Nothing here knows about
//! the screen; the window collects the entries from its terminals and
//! draws the rows.
//!
//! A view that adds to the list reads a [`SessionRow`]: its `pane` is
//! the key for whatever the view keeps about the agent, and its `place`
//! says whether there is a pane to go to.
//!
//! What is under a row to answer its agent with, and the keys an answer
//! sends, are decided here too: [`SessionRow::controls`] and
//! [`SessionRow::answer_key`].

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use benterm_agent::{AgentState, Plan, PlanItem, PlanStatus, WaitReason, builtin};
use benterm_ipc::ReportStatus;
use benterm_layout::{PaneId, TabId, WorkspaceId};

use crate::checked_reports::{CheckedReport, shown_line};

/// Where the terminal of an agent is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionPlace {
    /// In a pane of a tab.
    Pane { workspace: WorkspaceId, tab: TabId },
    /// Behind the window, in no tab.
    Background,
}

/// What the list is told about one terminal of the window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionEntry {
    /// The id of the terminal, which is the id of its pane if it has one.
    pub pane: PaneId,
    /// The pane that started the agent.
    pub parent: Option<PaneId>,
    /// The id of the definition of the agent recognised in the terminal.
    pub agent: Option<&'static str>,
    pub state: AgentState,
    /// The name the agent was started under, if it was started by name.
    pub name: Option<String>,
    /// The directory of the terminal's program.
    pub directory: Option<PathBuf>,
    pub place: SessionPlace,
    /// Whether the terminal has something the user has not looked at
    /// yet, such as a turn that ended while another pane was in view.
    pub unseen: bool,
    /// Whether the terminal has the attention mark that its pane, tab and
    /// workspace have when it is in one.
    pub marked: bool,
    /// What the agent means to do, if its hooks told.
    pub plan: Option<Plan>,
    /// The status of the agent's last report that had one.
    pub report_status: Option<ReportStatus>,
    /// The last report of the agent that its Stop hook checked.
    pub checked: Option<CheckedReport>,
    /// Whether the agent is marked as stalled: at work, with neither its
    /// screen nor its files changing for longer than the threshold.
    pub stalled: bool,
    /// What the program told of itself with OSC 7501 (`msg`), if any.
    pub msg: Option<String>,
}

/// What an agent is doing, as the list says it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionState {
    /// Started under a name, and not recognised or seen at work yet.
    Starting,
    Working,
    Waiting(WaitReason),
    /// A turn has ended that the user has not looked at yet.
    Done,
    /// At its prompt, with nothing new for the user.
    Idle,
    /// The last turn did not finish.
    Error,
}

impl SessionState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Starting => "Starting",
            Self::Working => "Working",
            Self::Waiting(WaitReason::Permission | WaitReason::Trust) => "Needs approval",
            Self::Waiting(_) => "Waiting",
            Self::Done => "Done",
            Self::Idle => "Idle",
            Self::Error => "Error",
        }
    }
}

/// A button under a row.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SessionButton {
    /// Allows what the agent asks permission for, this once.
    Allow,
    Deny,
    /// Goes to the agent's pane.
    Go,
}

impl SessionButton {
    pub fn label(self) -> &'static str {
        match self {
            Self::Allow => "Allow",
            Self::Deny => "Deny",
            Self::Go => "Go",
        }
    }
}

/// What is under a row to answer its agent with: a field first, if
/// there is one, and the buttons after it in order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SessionControls {
    /// A one-line field whose text goes to the agent with Enter.
    pub field: bool,
    pub buttons: &'static [SessionButton],
}

/// A key that answers an agent by itself, with nothing after it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnswerKey {
    /// A key that types this text.
    Text(&'static str),
    /// Escape, sent as the key would be from the keyboard.
    Escape,
}

/// One row of the list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionRow {
    pub pane: PaneId,
    pub parent: Option<PaneId>,
    pub agent: Option<&'static str>,
    pub state: SessionState,
    pub name: Option<String>,
    pub directory: Option<PathBuf>,
    pub place: SessionPlace,
    /// How many agents are above this one in its family: 0 for an agent
    /// whose parent has no row.
    pub depth: usize,
    /// Whether the row has the attention mark.
    pub marked: bool,
    /// What the agent means to do, if its hooks told.
    pub plan: Option<Plan>,
    /// Whether the agent at work is marked as stalled. Never set for a
    /// row in another state.
    pub stalled: bool,
    /// The status of the agent's last report that had one.
    pub report_status: Option<ReportStatus>,
    /// The last report of the agent that its Stop hook checked, which
    /// the row opens in a panel.
    pub checked: Option<CheckedReport>,
    /// Whether the terminal has something the user has not looked at
    /// yet.
    pub unseen: bool,
    /// What the program told of itself with OSC 7501 (`msg`), on one
    /// line without control characters. Never set for a row whose state
    /// is neither waiting nor an error.
    pub msg: Option<String>,
}

/// The line of a row that opens the agent's checked report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReportLine {
    /// The first question of the report, or its title if it asks none,
    /// on one line.
    pub text: String,
    /// Whether `text` is a question.
    pub asks: bool,
    /// How many questions the report asks after the first.
    pub more: usize,
    /// Whether the report ran out of rounds with problems left.
    pub stopped: bool,
}

/// The status of an agent's last report as its row shows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReportMark {
    pub label: &'static str,
    /// Whether the work has problems or cannot go on, which the row
    /// shows so that it is seen before the report is read.
    pub stands_out: bool,
}

/// The most items of a plan a row shows.
pub const PLAN_LINES: usize = 5;

/// The plan of an agent as its row shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlanView {
    /// How many items are done, in progress and left.
    pub summary: String,
    /// The items the row shows, in the order of the plan.
    pub items: Vec<PlanItem>,
    /// How many items of the plan come before the ones shown.
    pub before: usize,
    /// How many come after them.
    pub after: usize,
    /// Completed items count for the progress meter.
    pub done: usize,
    /// Total items count for the progress meter.
    pub total: usize,
}

/// The icon of a row, which says the kind of its agent in place of the
/// kind's name. Each is a plain shape of our own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentIcon {
    ClaudeCode,
    Codex,
    /// An agent of a kind that is not built in.
    Agent,
    /// A program that is no agent and tells its state itself.
    Program,
}

impl SessionRow {
    /// The status of the agent's work as the row shows it: the
    /// conclusion of its checked report, as the panel of the report
    /// shows it, or else the status of its last report to its parent.
    pub fn report_status(&self) -> Option<ReportStatus> {
        self.checked
            .as_ref()
            .and_then(|checked| checked.report.status())
            .or(self.report_status)
    }

    /// The status of the agent's work, if it has one; see
    /// [`SessionRow::report_status`].
    pub fn report_mark(&self) -> Option<ReportMark> {
        let (label, stands_out) = match self.report_status()? {
            ReportStatus::Pass => ("Pass", false),
            ReportStatus::Issues => ("Issues", true),
            ReportStatus::Blocked => ("Blocked", true),
        };
        Some(ReportMark { label, stands_out })
    }

    /// The line that opens the agent's checked report, if it has one.
    pub fn report_line(&self) -> Option<ReportLine> {
        let checked = self.checked.as_ref()?;
        let report = &checked.report;
        let (text, asks) = match report.decisions.first() {
            Some(decision) => (shown_line(&decision.question), true),
            None => (shown_line(&report.title), false),
        };
        Some(ReportLine {
            text,
            asks,
            more: report.decisions.len().saturating_sub(1),
            stopped: !checked.passed(),
        })
    }

    /// The kind of the agent as the user knows it. A program that was
    /// not started as an agent and is none, and tells its state itself,
    /// is a program.
    pub fn kind(&self) -> &'static str {
        let Some(id) = self.agent else {
            return if self.name.is_some() {
                "Agent"
            } else {
                "Program"
            };
        };
        builtin()
            .into_iter()
            .find(|definition| definition.id == id)
            .map_or("Agent", |definition| definition.name)
    }

    /// The first line of the row: what tells it from the others of its
    /// kind, or its kind when nothing does.
    pub fn title(&self) -> String {
        let label = self.label();
        if label.is_empty() {
            self.kind().to_owned()
        } else {
            label
        }
    }

    /// The icon that says the kind of the agent.
    pub fn icon(&self) -> AgentIcon {
        match self.agent {
            Some("claude") => AgentIcon::ClaudeCode,
            Some("codex") => AgentIcon::Codex,
            Some(_) => AgentIcon::Agent,
            None if self.name.is_some() => AgentIcon::Agent,
            None => AgentIcon::Program,
        }
    }

    /// What tells the row from others of its kind: the name the agent
    /// was started under, or else the last part of its directory.
    pub fn label(&self) -> String {
        if let Some(name) = &self.name {
            return name.clone();
        }
        let Some(directory) = &self.directory else {
            return String::new();
        };
        directory.file_name().map_or_else(
            || directory.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        )
    }

    /// Whether the row has a field for a reply.
    pub fn takes_reply(&self) -> bool {
        self.controls().field
    }

    /// Whether the row shows its field for a reply: `chosen` says
    /// whether the user chose the row to answer it.
    ///
    /// A row that waits for an answer shows its field always, so that
    /// it is answered at once: a question that may take text, and a turn
    /// that ended or failed while the user was not looking. A row the
    /// user has looked at shows its field only when chosen, so that a
    /// list of many agents at their prompts is not filled with fields.
    pub fn shows_field(&self, chosen: bool) -> bool {
        let waits = match self.state {
            SessionState::Waiting(_) | SessionState::Done => true,
            SessionState::Error => self.unseen,
            _ => false,
        };
        self.takes_reply() && (waits || chosen)
    }

    /// What is under the row to answer its agent with.
    ///
    /// A permission is answered with a button, which sends one key: a
    /// line of text with Enter after it could answer the question with
    /// its first character and the next one with the Enter. The question
    /// whether to trust a folder is answered in the pane, as it comes
    /// once for a folder, at start-up. A question of choices is answered
    /// in the pane too, as its choices differ from one question to the
    /// next. A question that may take text, and an agent
    /// at its prompt, take a line in the field; the question also has a
    /// way to the pane, as it may be one of choices after all. A
    /// permission of a program whose keys are not known is answered in
    /// the pane.
    pub fn controls(&self) -> SessionControls {
        use SessionButton::{Allow, Deny, Go};
        let keys_known = self.answer_key(Allow).is_some();
        let (field, buttons): (bool, &'static [SessionButton]) = match self.state {
            SessionState::Waiting(WaitReason::Permission) if !keys_known => (false, &[Go]),
            SessionState::Waiting(WaitReason::Permission) => (false, &[Allow, Deny, Go]),
            SessionState::Waiting(WaitReason::Trust | WaitReason::Choice) => (false, &[Go]),
            SessionState::Waiting(WaitReason::Question) => (true, &[Go]),
            SessionState::Done | SessionState::Idle | SessionState::Error => (true, &[]),
            SessionState::Starting | SessionState::Working => (false, &[]),
        };
        SessionControls { field, buttons }
    }

    /// The key that `button` sends to answer the agent, or `None` for a
    /// button that sends nothing, or a row that asks no permission.
    ///
    /// Each key is the one the agent's dialog shows for the choice, and
    /// it picks the choice without Enter: Claude Code numbers its choices
    /// ("1. Yes") and Codex names a letter ("Yes, proceed (y)"). Both
    /// mark the refusal "(esc)", which leaves the dialog at once; the
    /// numbered refusal of Claude Code opens a field for what to do
    /// instead.
    pub fn answer_key(&self, button: SessionButton) -> Option<AnswerKey> {
        if self.state != SessionState::Waiting(WaitReason::Permission) {
            return None;
        }
        match (self.agent?, button) {
            ("claude", SessionButton::Allow) => Some(AnswerKey::Text("1")),
            ("codex", SessionButton::Allow) => Some(AnswerKey::Text("y")),
            ("claude" | "codex", SessionButton::Deny) => Some(AnswerKey::Escape),
            _ => None,
        }
    }

    /// The plan as the row shows it: `None` without a plan, and for a
    /// plan without items.
    ///
    /// A plan longer than [`PLAN_LINES`] is shown from the item before
    /// the first one in progress, or before the first one left to do
    /// when nothing is in progress; near its end, and when it is all
    /// done, its last items are shown.
    pub fn plan_view(&self) -> Option<PlanView> {
        let items = &self.plan.as_ref()?.items;
        let last = items.len().checked_sub(1)?;
        let first_in = |status| items.iter().position(|item| item.status == status);
        let current = first_in(PlanStatus::InProgress)
            .or_else(|| first_in(PlanStatus::Pending))
            .unwrap_or(last);
        let shown = items.len().min(PLAN_LINES);
        let before = current.saturating_sub(1).min(items.len() - shown);
        let counts = self.plan.as_ref()?.counts();
        Some(PlanView {
            summary: format!(
                "{} done \u{b7} {} in progress \u{b7} {} left",
                counts.done, counts.in_progress, counts.remaining
            ),
            items: items[before..before + shown].to_vec(),
            before,
            after: items.len() - before - shown,
            done: counts.done,
            total: items.len(),
        })
    }

    /// Whether the agent's terminal is behind the window.
    pub fn is_background(&self) -> bool {
        self.place == SessionPlace::Background
    }
}

/// What the list says of an entry, or `None` for one without a row: a
/// terminal whose state nothing has shown, as that of a program that is
/// no agent and tells nothing of itself, unless it was started as an
/// agent under a name.
///
/// A program that is no agent has a state when it tells one itself
/// (OSC 7501). At rest it has no row: an agent then waits at its prompt
/// for the next turn, and a program asks for nothing.
fn shown_state(entry: &SessionEntry) -> Option<SessionState> {
    let recognised = entry.agent.is_some();
    match entry.state {
        AgentState::Working => Some(SessionState::Working),
        AgentState::Waiting(reason) => Some(SessionState::Waiting(reason)),
        AgentState::Error => Some(SessionState::Error),
        AgentState::Done if recognised && entry.unseen => Some(SessionState::Done),
        AgentState::Done if recognised => Some(SessionState::Idle),
        _ => entry.name.is_some().then_some(SessionState::Starting),
    }
}

/// The rows of the entries that get one. The rows keep the order of the
/// entries, except that the children of an agent come right after it,
/// one step further in, each followed by its own children.
///
/// Whatever the entries name as parents, every entry that gets a row
/// gets exactly one: of agents that name each other as parent in a ring,
/// the first is listed as if it had no parent.
pub fn session_rows(entries: Vec<SessionEntry>) -> Vec<SessionRow> {
    let shown: Vec<(SessionEntry, SessionState)> = entries
        .into_iter()
        .filter_map(|entry| shown_state(&entry).map(|state| (entry, state)))
        .collect();
    let index: HashMap<PaneId, usize> = shown
        .iter()
        .enumerate()
        .map(|(at, (entry, _))| (entry.pane, at))
        .collect();
    // The parent of each entry among the entries, if it has one there.
    let parents: Vec<Option<usize>> = shown
        .iter()
        .enumerate()
        .map(|(at, (entry, _))| {
            let parent = *index.get(&entry.parent?)?;
            (parent != at).then_some(parent)
        })
        .collect();
    let mut children: Vec<Vec<usize>> = vec![Vec::new(); shown.len()];
    for (at, parent) in parents.iter().enumerate() {
        if let Some(parent) = parent {
            children[*parent].push(at);
        }
    }

    // The entries at the top: those without a parent, and the first of
    // every ring of entries that no entry at the top leads to.
    let mut roots: Vec<usize> = Vec::new();
    let mut reached: HashSet<usize> = HashSet::new();
    let reach = |root: usize, reached: &mut HashSet<usize>| {
        let mut ahead = vec![root];
        while let Some(at) = ahead.pop() {
            if reached.insert(at) {
                ahead.extend(&children[at]);
            }
        }
    };
    for at in (0..shown.len()).filter(|at| parents[*at].is_none()) {
        roots.push(at);
        reach(at, &mut reached);
    }
    for at in 0..shown.len() {
        if reached.contains(&at) {
            continue;
        }
        // Nothing at the top leads here, so going up from here never
        // ends: it comes to a ring. The first entry of the ring goes to
        // the top, and what hangs on the ring stays under its parent.
        let mut passed: HashSet<usize> = HashSet::new();
        let mut in_ring = at;
        while passed.insert(in_ring) {
            in_ring = parents[in_ring].expect("an entry no root leads to has a parent");
        }
        let mut first = in_ring;
        let mut member = parents[in_ring].expect("an entry of a ring has a parent");
        while member != in_ring {
            first = first.min(member);
            member = parents[member].expect("an entry of a ring has a parent");
        }
        roots.push(first);
        reach(first, &mut reached);
    }
    roots.sort_unstable();

    let mut order: Vec<(usize, usize)> = Vec::with_capacity(shown.len());
    let mut placed: HashSet<usize> = HashSet::new();
    // The children of the entry on top come next, the first of them
    // first.
    let mut ahead: Vec<(usize, usize)> = roots.into_iter().rev().map(|at| (at, 0)).collect();
    while let Some((at, depth)) = ahead.pop() {
        if !placed.insert(at) {
            continue;
        }
        order.push((at, depth));
        ahead.extend(children[at].iter().rev().map(|child| (*child, depth + 1)));
    }

    let mut shown: Vec<Option<(SessionEntry, SessionState)>> =
        shown.into_iter().map(Some).collect();
    order
        .into_iter()
        .map(|(at, depth)| {
            let (entry, state) = shown[at].take().expect("an entry is placed once");
            let msg = matches!(state, SessionState::Waiting(_) | SessionState::Error)
                .then_some(entry.msg.as_deref())
                .flatten()
                .map(shown_line)
                .filter(|msg| !msg.is_empty());
            SessionRow {
                pane: entry.pane,
                parent: entry.parent,
                agent: entry.agent,
                state,
                name: entry.name,
                directory: entry.directory,
                place: entry.place,
                depth,
                marked: entry.marked,
                plan: entry.plan,
                report_status: entry.report_status,
                checked: entry.checked,
                unseen: entry.unseen,
                stalled: entry.stalled && state == SessionState::Working,
                msg,
            }
        })
        .collect()
}

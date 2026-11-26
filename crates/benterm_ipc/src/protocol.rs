//! The wire format: one JSON object per line, in both directions.
//!
//! A request is `{"id": 1, "method": "pane.split", "params": {...}}`. The
//! client picks the id, and it only has to be unique among the requests that
//! connection is still waiting on. The response is `{"id": 1, "result": ...}`
//! or `{"id": 1, "error": {"code": "...", "message": "..."}}`. Responses may
//! arrive in a different order than the requests were sent, so a request may
//! take as long as it needs without holding up the ones after it.
//!
//! A request may carry `"timeout_ms": 1500`: how long the app has to start
//! it. The app answers a request it gets to later with an `expired` error
//! and does not carry it out. A caller that gives up waiting sends a
//! shorter time than it waits, so a request it gave up on either was
//! carried out before it gave up or never will be.
//!
//! A line from the server with an `event` and no `id` is a notification:
//! `{"event": "pane.agent_state_changed", "params": {...}}`. Only a
//! connection that subscribed gets them; clients skip the ones they do not
//! know.
//!
//! Unknown fields are ignored everywhere, so fields can be added later.

use std::fmt;
use std::num::ParseIntError;
use std::str::FromStr;
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

/// Identifies a pane. It is the number behind the layout tree's pane id,
/// which is never reused after the pane is closed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PaneId(u64);

/// Identifies a workspace. Like a pane id, it is never reused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct WorkspaceId(u64);

macro_rules! id_type {
    ($name:ident) => {
        impl $name {
            pub fn new(id: u64) -> Self {
                Self(id)
            }

            /// The number behind the id.
            pub fn get(self) -> u64 {
                self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(f)
            }
        }

        impl FromStr for $name {
            type Err = ParseIntError;

            fn from_str(text: &str) -> Result<Self, Self::Err> {
                text.parse().map(Self)
            }
        }
    };
}

id_type!(PaneId);
id_type!(WorkspaceId);

/// Where the new pane goes when a pane is split.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SplitDirection {
    /// To the right of the pane, which becomes the left half.
    Right,
    /// Below the pane, which becomes the top half.
    Down,
}

/// An operation: its method name on the wire and what it answers with.
pub trait Method: Serialize + DeserializeOwned {
    const NAME: &'static str;
    type Output: Serialize + DeserializeOwned;
}

/// `pane.split`: split a pane and start a shell in the new half.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SplitPane {
    pub pane: PaneId,
    pub direction: SplitDirection,
}

/// `browser.open`: split a pane and show a web page in the new half.
/// The keyboard stays where it is.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenBrowser {
    /// The address of the page: `http` or `https` with a host.
    pub url: String,
    /// The pane to split. Without one, the focused pane of the tab on
    /// view.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane: Option<PaneId>,
    /// Without one, the new pane goes to the right.
    #[serde(default = "to_the_right")]
    pub direction: SplitDirection,
}

fn to_the_right() -> SplitDirection {
    SplitDirection::Right
}

/// `workspace.create`: open a new workspace with one pane.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateWorkspace {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The working directory of the first pane.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// The pane whose program asks, when a program in a pane asks. The
    /// app refuses the call with `unknown_pane` when it has no such pane,
    /// as when the shell was thrown away with a session that could not
    /// be restored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller: Option<PaneId>,
}

/// `task.create`: make a branch and a git worktree for a piece of work,
/// and open a workspace in the worktree. With a role, the role's agent
/// runs in the workspace's first pane; without one, a shell.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateTask {
    pub name: String,
    /// The branch to make. Without one it is named as the task is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// A directory of the checkout to branch off from, as an absolute
    /// path. The task starts at the commit that checkout is on.
    pub cwd: String,
    /// The name of a role of the app's roles directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// What the agent is to do. Its first message is the role's
    /// instructions, a blank line, and this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work: Option<String>,
    /// The pane whose program asks, as for `workspace.create`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller: Option<PaneId>,
}

/// `task.list`: every task the app has made, closed ones too, oldest
/// first.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListTasks {}

/// `task.close`: close a task and its workspace.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CloseTask {
    pub task: u64,
    /// Also remove the task's worktree and branch. Refused when files
    /// in the worktree have changes that are in no commit, or the branch
    /// has commits that are on no other branch, unless `force` is set.
    #[serde(default)]
    pub remove: bool,
    #[serde(default)]
    pub force: bool,
}

/// What became of the worktree and the branch of a closed task.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorktreeLeft {
    Kept,
    Removed,
}

/// When and how a task was closed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskClosure {
    /// Milliseconds since 1970.
    pub at_ms: u64,
    pub worktree: WorktreeLeft,
}

/// A task, as `task.list` tells it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskInfo {
    /// The task keeps its number across restarts of the app.
    pub id: u64,
    pub name: String,
    /// The top directory of the main checkout of the task's repository.
    pub repository: String,
    /// The branch the task branched off from.
    pub base_branch: String,
    /// The commit the task's branch started at, by its full name.
    pub base_commit: String,
    pub branch: String,
    /// The directory of the task's worktree.
    pub worktree: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// Milliseconds since 1970.
    pub created_at_ms: u64,
    /// Not there while the task is open.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closed: Option<TaskClosure>,
    /// The workspace that shows the task now, if one does. Workspace
    /// ids are new at every start of the app.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<WorkspaceId>,
}

/// The answer to `task.create`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskCreated {
    pub task: TaskInfo,
    /// The first pane of the task's workspace.
    pub pane: PaneId,
}

/// The answer to `task.list`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskList {
    pub tasks: Vec<TaskInfo>,
}

/// The answer to `task.close`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskClosed {
    pub task: TaskInfo,
    /// Why the branch is still there, when the worktree was removed and
    /// git refused to delete the branch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch_left: Option<String>,
}

/// `pane.send_text`: type text into a pane, exactly as given. To press
/// Enter, end the text with a carriage return.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SendText {
    pub pane: PaneId,
    pub text: String,
}

/// `pane.read`: read what a pane shows.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadOutput {
    pub pane: PaneId,
    /// With a number, the last lines of the scrollback and the screen
    /// together. Without one, the visible screen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lines: Option<u32>,
}

/// `pane.list`: every terminal of the app: the panes of every workspace
/// and tab, then the terminals behind the window, oldest first.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListPanes {}

/// One terminal in the answer to `pane.list`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneInfo {
    pub pane: PaneId,
    /// The workspace of the pane; none for a terminal behind the window.
    pub workspace: Option<WorkspaceId>,
    /// The place of the pane's tab in its workspace, from 1 at the left;
    /// none for a terminal behind the window.
    pub tab: Option<u32>,
    /// The name the terminal was started under with `agent.spawn`.
    pub name: Option<String>,
    /// The working directory of the program in front, when the app can
    /// tell it.
    pub cwd: Option<String>,
    /// The agent recognised in the terminal: `claude`, `codex`, or none.
    pub agent: Option<String>,
    pub state: AgentActivity,
    /// Set exactly when `state` is `waiting`.
    pub reason: Option<WaitingFor>,
}

/// The answer to `pane.list`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneList {
    pub panes: Vec<PaneInfo>,
}

/// `pane.report_hook`: an agent's hook reports an event for the pane the
/// agent runs in. The fields after `event` are passed on from the hook's
/// JSON input when it has them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportHook {
    pub pane: PaneId,
    /// Which agent the hook belongs to: `claude` or `codex`.
    pub agent: String,
    /// The name of the hook event, such as `Stop`.
    pub event: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notification_type: Option<String>,
    /// The agent's session. The app uses it to tell the agent in front
    /// from one that it runs as a tool.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Set when the event comes from a subagent of the session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    /// What the tool was given, for a tool the app reads something from:
    /// one the agent keeps its plan with. `benterm hook` leaves out
    /// the input of every other tool.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_input: Option<serde_json::Value>,
    /// What the tool answered, for the same tools.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_response: Option<serde_json::Value>,
}

/// `pane.agent_state`: read which agent runs in a pane and what it is
/// doing.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadAgentState {
    pub pane: PaneId,
}

/// `pane.subscribe_agent_state`: from the answer on, send this connection
/// a `pane.agent_state_changed` event whenever the agent state of any pane
/// changes, and a `pane.closed` event when a pane closes. To learn the
/// states as they are now, read them after subscribing. The subscription
/// lasts as long as the connection. A subscriber that does not read its
/// events is disconnected once too many are waiting.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubscribeAgentState {}

/// Names an agent: by the name it was started with, or by the id of the
/// pane its terminal has, which a terminal has whether it is in a pane or
/// behind. On the wire a name is a string and a pane a number, and so on
/// the command line, which is why a name is never all digits.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AgentTarget {
    Pane(PaneId),
    Name(String),
}

impl fmt::Display for AgentTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pane(pane) => pane.fmt(f),
            Self::Name(name) => name.fmt(f),
        }
    }
}

impl FromStr for AgentTarget {
    type Err = std::convert::Infallible;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Ok(match text.parse() {
            Ok(pane) => Self::Pane(pane),
            Err(_) => Self::Name(text.to_owned()),
        })
    }
}

/// Default maximum number of peer-to-peer messages allowed in one sibling
/// conversation under a parent before the parent is notified.
pub const DEFAULT_MAX_PEER_ROUNDS: u32 = 10;

/// `agent.spawn`: start a program in a terminal behind the window, one
/// that has no pane, under a name. Its terminal has a pane id like any
/// other, so the `pane.*` requests reach it too.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpawnAgent {
    /// Unique among the agents that are running. Letters, digits, `-`,
    /// `_` and `.`, at most 64, and not all digits.
    pub name: String,
    /// The program and its arguments. Empty for the shell every pane
    /// runs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub command: Vec<String>,
    /// The working directory, an absolute path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// Variables for the program, on top of the ones every pane gets. A
    /// `PATH` given here is also where the program itself is looked up.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env: Vec<(String, String)>,
    /// The pane that starts the agent. The agent is its child: it reports
    /// to it, and it ends when the pane closes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<PaneId>,
    /// Keep the agent running when its parent's pane closes.
    #[serde(default, skip_serializing_if = "is_false")]
    pub outlive_parent: bool,
    /// A line put before the first `agent.send` message to the agent, on
    /// a line of its own. `agent run` gives the line that asks to read the
    /// rules for workers here, as it cannot put it in a command that may
    /// be any program's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_message_line: Option<String>,
    /// When the program ends, its terminal goes on with the shell a new
    /// pane runs, in the same directory, instead of closing; the agent
    /// ends there, and a conversation its hooks told is typed into the
    /// shell to take up again. An agent behind the window has no pane to
    /// go on in, and its terminal closes as before. The roles start
    /// their agents so.
    #[serde(default, skip_serializing_if = "is_false")]
    pub shell_after: bool,
    /// Allow this agent to exchange direct messages with sibling agents
    /// that share the same parent and also opted in.
    #[serde(default, skip_serializing_if = "is_false")]
    pub allow_peers: bool,
    /// Maximum number of peer messages in a sibling conversation this agent
    /// participates in. Defaults to [`DEFAULT_MAX_PEER_ROUNDS`] when `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_peer_rounds: Option<u32>,
    /// Give the agent a credential for the whole API, as a pane of the
    /// window has, so that it may start and direct agents of its own.
    /// Without it, the agent may only report its own hooks and reports,
    /// report to its parent and, with `allow_peers`, message its peers.
    #[serde(default, skip_serializing_if = "is_false")]
    pub control: bool,
}

/// `agent.list`: the agents that were started and still run.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListAgents {
    /// Only the children of this pane.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<PaneId>,
    /// Only the peer-enabled siblings of this pane (same parent, excluding
    /// this pane, with `allow_peers == true`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub siblings_of: Option<PaneId>,
}

/// `agent.show`: open the terminal of an agent that is behind as a tab.
/// The program keeps running and the screen stays as it is. An agent that
/// is in a tab already stays where it is.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShowAgent {
    pub agent: AgentTarget,
    /// Show the tab and give it the keyboard. Without it the tab opens
    /// behind the one on view.
    #[serde(default, skip_serializing_if = "is_false")]
    pub select: bool,
}

/// `agent.send`: give an agent a message. The text is pasted and Enter is
/// pressed after it, unlike `pane.send_text`, which types what it is
/// given.
///
/// An agent that waits for the user to answer a dialog is given nothing,
/// and the request fails with `agent_waiting`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SendToAgent {
    pub agent: AgentTarget,
    pub text: String,
    /// The pane whose program sends, when a program in a pane of the same
    /// app does. The app keeps it as the sender of the message. A sender
    /// the app has no pane for is left out, and the message is given all
    /// the same.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caller: Option<PaneId>,
}

/// `agent.read`: `pane.read` for an agent named by its name.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadAgent {
    pub agent: AgentTarget,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lines: Option<u32>,
}

/// `agent.keys`: press keys in an agent's terminal, by their names, such
/// as `escape`, `enter`, `up` or `ctrl-c`. Each key is sent as it would be
/// pressed in the pane: in the keyboard protocol the program asked for,
/// so `up` is `ESC O A` to a program that turned on the application
/// cursor keys. The names are all checked first and the keys written in
/// one go; a name that is no key, or a key that types text, sends none of
/// them. Text is for `agent.send`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SendKeys {
    pub agent: AgentTarget,
    pub keys: Vec<String>,
}

/// `agent.kill`: close an agent's terminal, which hangs its program up.
/// An agent in a tab loses its pane, and one behind the window its
/// terminal. Either way it leaves `agent.list`, and a wait for it is
/// answered with the agent closed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KillAgent {
    pub agent: AgentTarget,
}

/// `agent.wait`: answer once an agent, or every child of a pane, is in
/// one of the states of `until` and has been for `settle_ms`. An agent
/// whose terminal has closed counts as there. An agent that was given a
/// message with `agent.send`, or started with a command, is not there
/// until it has been seen at work since: it owes a turn. The children are
/// the ones the pane has when the wait starts.
///
/// The wait ends with a `wait_timed_out` error after `wait_ms`. That time
/// is the caller's own and has nothing to do with `timeout_ms` of the
/// request, which is how long the app has to start waiting. A connection
/// that waits must not stop sending before the answer: the app takes that
/// for the caller having left and gives the wait up.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WaitForAgents {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<AgentTarget>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub children_of: Option<PaneId>,
    /// Empty for `done`, `waiting` and `error`: until the agent stops
    /// working.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub until: Vec<AgentActivity>,
    /// Without it the wait lasts as long as the connection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wait_ms: Option<u64>,
    /// Without it one second: an agent that says it is done may be sent
    /// on by a hook a moment later.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settle_ms: Option<u64>,
}

/// `agent.report`: an agent tells the pane that started it something. The
/// text arrives as input of the parent's program, between tags that name
/// the agent, once, and not while the user has a draft there: it is held
/// back, with the parent's pane marked, until the draft is gone.
/// The answer says the report was taken, not that it has arrived.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportToParent {
    /// The pane of the agent that reports.
    pub from: PaneId,
    pub text: String,
    /// What the agent's work came to. A report with one starts with its
    /// word in the parent's pane, and the agent keeps it as the status of
    /// its last report until another report with a status comes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<ReportStatus>,
}

/// What an agent's work came to: `pass`, `issues` or `blocked`. The
/// conclusion of a checked report ([`ReportChecked`]) uses the same
/// words, so the type lives with the report.
pub use benterm_report::{ReportStatus, UnknownReportStatus};

/// `pane.report_checked`: the Stop hook of the agent in a pane checked
/// the report of the turn that ends, and the agent stops. The app keeps
/// the report as the last one of the pane.
///
/// A report without problems passed the check. One with problems was
/// sent back as often as the settings allow and still has them; it is
/// passed on so that the problems are seen with the report.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportChecked {
    /// The pane of the agent that wrote the report.
    pub pane: PaneId,
    /// Which agent the hook belongs to: `claude` or `codex`.
    pub agent: String,
    /// The agent's session, as its hook named it: the conversation the
    /// report belongs to, which may no longer be the one in the pane.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    pub report: benterm_report::Report,
    /// What is still wrong with the report, one problem a line.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub problems: Vec<String>,
}

/// The answer to `agent.spawn`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSpawned {
    pub name: String,
    pub pane: PaneId,
}

/// One line of the answer to `agent.list`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentInfo {
    pub name: String,
    pub pane: PaneId,
    /// The pane that started the agent, also after that pane has closed.
    pub parent: Option<PaneId>,
    /// Whether the terminal is behind the window, in no tab.
    pub background: bool,
    pub outlive_parent: bool,
    pub state: AgentActivity,
    /// The status of the agent's last report that had one. A report
    /// without a status leaves it as it was.
    #[serde(default)]
    pub report_status: Option<ReportStatus>,
    /// Whether the agent is marked as stalled: at work, with neither its
    /// screen nor the files of its directory changed for longer than the
    /// threshold of the settings. The mark changes neither `state` nor a
    /// wait.
    #[serde(default, skip_serializing_if = "is_false")]
    pub stalled: bool,
    /// The last change seen of an agent at work, in milliseconds since
    /// 1970: of its screen, of a file, or the start of the work. `None`
    /// for an agent that is not at work.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub changed_at_ms: Option<u64>,
    /// What the agent told of itself with OSC 7501 (`msg`), on one line
    /// without control characters. Set only while `state` is `waiting`
    /// or `error` and the terminal's root record carries a `msg`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub msg: Option<String>,
    /// Whether this agent opted in to direct sibling communication.
    #[serde(default, skip_serializing_if = "is_false")]
    pub allow_peers: bool,
}

/// The answer to `agent.list`, oldest agent first.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentList {
    pub agents: Vec<AgentInfo>,
}

/// The answer to `agent.show`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentShown {
    pub pane: PaneId,
    /// As in [`AgentInfo`]; left out for an agent that has none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub report_status: Option<ReportStatus>,
    /// As in [`AgentInfo::stalled`].
    #[serde(default, skip_serializing_if = "is_false")]
    pub stalled: bool,
    /// As in [`AgentInfo::changed_at_ms`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub changed_at_ms: Option<u64>,
}

/// One agent in the answer to `agent.wait`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WaitedAgent {
    /// `None` for a pane that was not started as an agent.
    pub name: Option<String>,
    pub pane: PaneId,
    /// The terminal closed; `state` is `unknown` then.
    pub closed: bool,
    pub state: AgentActivity,
    pub reason: Option<WaitingFor>,
}

/// The answer to `agent.wait`: the agents waited for, as they are now.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentsWaited {
    pub agents: Vec<WaitedAgent>,
}

/// The answer to `agent.report`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportAccepted {
    pub parent: PaneId,
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// `agent.history`: read the changes of the agents' states that the app
/// has recorded, in all panes, oldest first. The app keeps them across
/// restarts for a time, a week unless it is told otherwise, and gives
/// none that is older.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadAgentHistory {
    /// With a time, only the records after it. No two records have the
    /// same time, so the time of the last record a caller has gets it
    /// the ones it has not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since_ms: Option<u64>,
}

/// The answer to `agent.history`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentHistory {
    pub records: Vec<AgentRecord>,
}

/// One change of a pane's agent or of the agent's state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentRecord {
    /// When the change was recorded, in milliseconds since 1970.
    pub at_ms: u64,
    /// Names the pane across restarts of the app.
    pub key: String,
    /// The id the pane had when the change was recorded. The pane gets
    /// another id when the app starts again.
    pub pane: PaneId,
    /// `claude` or `codex`; none where the pane's agent went away, as
    /// when it ended, the pane was closed or the app quit.
    pub agent: Option<String>,
    pub state: AgentActivity,
    /// Set exactly when `state` is `waiting`.
    pub reason: Option<WaitingFor>,
}

/// What an agent is doing, without the reason it waits.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentActivity {
    Working,
    /// Stopped until the user answers; see [`WaitingFor`].
    Waiting,
    /// At its prompt: a turn has finished, or none has started.
    Done,
    /// At its prompt after a turn that did not finish: it failed, or was
    /// interrupted or refused.
    Error,
    /// No agent is recognised in the pane, or nothing has shown its state.
    Unknown,
}

/// What a waiting agent wants from the user.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WaitingFor {
    /// Approval to run a tool or to trust a folder.
    Permission,
    /// An answer to a question the agent asked.
    Question,
}

/// The answer to `pane.agent_state`, and what `pane.agent_state_changed`
/// carries.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneAgentState {
    pub pane: PaneId,
    /// The agent recognised in the pane: `claude`, `codex`, or none.
    pub agent: Option<String>,
    pub state: AgentActivity,
    /// Set exactly when `state` is `waiting`.
    pub reason: Option<WaitingFor>,
}

/// A notification: its name on the wire. Its fields are the `params`.
pub trait Event: Serialize + DeserializeOwned {
    const NAME: &'static str;
}

/// `pane.agent_state_changed`: the agent of a pane, or its state, is no
/// longer what it was.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AgentStateChanged(pub PaneAgentState);

/// `pane.closed`: the pane is gone, and no event about it follows.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneClosed {
    pub pane: PaneId,
}

impl Event for AgentStateChanged {
    const NAME: &'static str = "pane.agent_state_changed";
}

impl Event for PaneClosed {
    const NAME: &'static str = "pane.closed";
}

/// The answer to `pane.split`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneCreated {
    pub pane: PaneId,
}

/// The answer to `workspace.create`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceCreated {
    pub workspace: WorkspaceId,
    /// The first pane of the workspace.
    pub pane: PaneId,
}

/// The answer to `pane.read`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneOutput {
    pub text: String,
}

impl Method for SplitPane {
    const NAME: &'static str = "pane.split";
    type Output = PaneCreated;
}

impl Method for CreateWorkspace {
    const NAME: &'static str = "workspace.create";
    type Output = WorkspaceCreated;
}

impl Method for CreateTask {
    const NAME: &'static str = "task.create";
    type Output = TaskCreated;
}

impl Method for ListTasks {
    const NAME: &'static str = "task.list";
    type Output = TaskList;
}

impl Method for CloseTask {
    const NAME: &'static str = "task.close";
    type Output = TaskClosed;
}

impl Method for SendText {
    const NAME: &'static str = "pane.send_text";
    type Output = ();
}

impl Method for ReadOutput {
    const NAME: &'static str = "pane.read";
    type Output = PaneOutput;
}

impl Method for ListPanes {
    const NAME: &'static str = "pane.list";
    type Output = PaneList;
}

impl Method for ReportHook {
    const NAME: &'static str = "pane.report_hook";
    type Output = ();
}

impl Method for ReportChecked {
    const NAME: &'static str = "pane.report_checked";
    type Output = ();
}

impl Method for ReadAgentState {
    const NAME: &'static str = "pane.agent_state";
    type Output = PaneAgentState;
}

impl Method for ReadAgentHistory {
    const NAME: &'static str = "agent.history";
    type Output = AgentHistory;
}

impl Method for SubscribeAgentState {
    const NAME: &'static str = "pane.subscribe_agent_state";
    type Output = ();
}

impl Method for SpawnAgent {
    const NAME: &'static str = "agent.spawn";
    type Output = AgentSpawned;
}

impl Method for ListAgents {
    const NAME: &'static str = "agent.list";
    type Output = AgentList;
}

impl Method for ShowAgent {
    const NAME: &'static str = "agent.show";
    type Output = AgentShown;
}

impl Method for SendToAgent {
    const NAME: &'static str = "agent.send";
    type Output = ();
}

impl Method for ReadAgent {
    const NAME: &'static str = "agent.read";
    type Output = PaneOutput;
}

impl Method for SendKeys {
    const NAME: &'static str = "agent.keys";
    type Output = ();
}

impl Method for KillAgent {
    const NAME: &'static str = "agent.kill";
    type Output = ();
}

impl Method for WaitForAgents {
    const NAME: &'static str = "agent.wait";
    type Output = AgentsWaited;
}

impl Method for OpenBrowser {
    const NAME: &'static str = "browser.open";
    type Output = PaneCreated;
}

impl Method for ReportToParent {
    const NAME: &'static str = "agent.report";
    type Output = ReportAccepted;
}

/// Every operation the server understands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    SplitPane(SplitPane),
    OpenBrowser(OpenBrowser),
    CreateWorkspace(CreateWorkspace),
    SendText(SendText),
    ReadOutput(ReadOutput),
    ReportHook(ReportHook),
    ReportChecked(ReportChecked),
    ReadAgentState(ReadAgentState),
    SubscribeAgentState(SubscribeAgentState),
    SpawnAgent(SpawnAgent),
    ListAgents(ListAgents),
    ShowAgent(ShowAgent),
    SendToAgent(SendToAgent),
    ReadAgent(ReadAgent),
    SendKeys(SendKeys),
    KillAgent(KillAgent),
    WaitForAgents(WaitForAgents),
    ReportToParent(ReportToParent),
    ReadAgentHistory(ReadAgentHistory),
    ListPanes(ListPanes),
    CreateTask(CreateTask),
    ListTasks(ListTasks),
    CloseTask(CloseTask),
}

impl Request {
    /// The wire method name of this request.
    pub fn method_name(&self) -> &'static str {
        match self {
            Self::SplitPane(_) => SplitPane::NAME,
            Self::OpenBrowser(_) => OpenBrowser::NAME,
            Self::CreateWorkspace(_) => CreateWorkspace::NAME,
            Self::SendText(_) => SendText::NAME,
            Self::ReadOutput(_) => ReadOutput::NAME,
            Self::ReportHook(_) => ReportHook::NAME,
            Self::ReportChecked(_) => ReportChecked::NAME,
            Self::ReadAgentState(_) => ReadAgentState::NAME,
            Self::SubscribeAgentState(_) => SubscribeAgentState::NAME,
            Self::SpawnAgent(_) => SpawnAgent::NAME,
            Self::ListAgents(_) => ListAgents::NAME,
            Self::ShowAgent(_) => ShowAgent::NAME,
            Self::SendToAgent(_) => SendToAgent::NAME,
            Self::ReadAgent(_) => ReadAgent::NAME,
            Self::SendKeys(_) => SendKeys::NAME,
            Self::KillAgent(_) => KillAgent::NAME,
            Self::WaitForAgents(_) => WaitForAgents::NAME,
            Self::ReportToParent(_) => ReportToParent::NAME,
            Self::ReadAgentHistory(_) => ReadAgentHistory::NAME,
            Self::ListPanes(_) => ListPanes::NAME,
            Self::CreateTask(_) => CreateTask::NAME,
            Self::ListTasks(_) => ListTasks::NAME,
            Self::CloseTask(_) => CloseTask::NAME,
        }
    }
}

/// What went wrong, for programs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// The line is not a request: not JSON, or no usable id or method.
    InvalidRequest,
    /// The request lacked a valid authentication token in restricted mode.
    Unauthorized,
    /// The credential is valid but does not permit this method or target pane.
    Forbidden,
    UnknownMethod,
    InvalidParams,
    UnknownPane,
    UnknownAgent,
    /// An agent that still runs has the name.
    NameTaken,
    /// The time a wait was given ran out before what it waited for
    /// happened. Nothing is wrong; the agents are as they were.
    WaitTimedOut,
    /// Removing a task's worktree and branch would lose work, and the
    /// request did not say to do it all the same. Nothing was removed.
    AtRisk,
    /// The agent waits for the user to answer a dialog: a permission, a
    /// question, a choice or whether to trust a folder. A message and its
    /// Enter could answer it, so nothing was typed.
    AgentWaiting,
    /// The terminal's input queue is full because its program is not
    /// reading input fast enough. Nothing was sent.
    InputQueueFull,
    /// Direct messaging between these two agents is not allowed: they must
    /// share the same parent and both opt in with `allow_peers`.
    PeerNotAllowed,
    /// The sibling conversation under this parent has reached its round
    /// limit. The parent has been notified.
    RoundLimitReached,
    Failed,
    /// The app gave up the request without answering it.
    Internal,
    /// The time the request gave the app ran out before the app got to it,
    /// so the app did not carry it out.
    Expired,
    /// A code this version does not know.
    #[serde(other)]
    Other,
}

/// An error response.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiError {
    pub code: ErrorCode,
    /// What went wrong, for people.
    pub message: String,
}

impl ApiError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ApiError {}

/// A request line that could not be understood.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RequestError {
    /// The id of the request, when the line got far enough to have one.
    pub id: Option<u64>,
    pub error: ApiError,
}

/// One line from a client.
#[derive(Clone, PartialEq, Eq)]
pub struct RequestFrame {
    pub id: u64,
    pub request: Request,
    /// How long the app has to start the request, counted from when the
    /// server reads it. `None` for as long as it takes.
    pub timeout: Option<Duration>,
    /// Optional opaque authentication credential. Omitted on the wire when
    /// `None` so older clients and servers remain compatible.
    pub token: Option<String>,
}

impl fmt::Debug for RequestFrame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RequestFrame")
            .field("id", &self.id)
            .field("request", &self.request)
            .field("timeout", &self.timeout)
            .field("token", &self.token.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

impl RequestFrame {
    pub fn new(id: u64, request: Request) -> Self {
        Self {
            id,
            request,
            timeout: None,
            token: None,
        }
    }

    pub fn with_timeout(self, timeout: Duration) -> Self {
        Self {
            timeout: Some(timeout),
            ..self
        }
    }

    pub fn with_token(self, token: impl Into<String>) -> Self {
        Self {
            token: Some(token.into()),
            ..self
        }
    }

    /// The frame as one line, ending in a newline.
    pub fn to_line(&self) -> String {
        let (id, timeout, token) = (self.id, self.timeout, self.token.as_deref());
        match &self.request {
            Request::SplitPane(params) => request_line(id, params, timeout, token),
            Request::OpenBrowser(params) => request_line(id, params, timeout, token),
            Request::CreateWorkspace(params) => request_line(id, params, timeout, token),
            Request::SendText(params) => request_line(id, params, timeout, token),
            Request::ReadOutput(params) => request_line(id, params, timeout, token),
            Request::ReportHook(params) => request_line(id, params, timeout, token),
            Request::ReportChecked(params) => request_line(id, params, timeout, token),
            Request::ReadAgentState(params) => request_line(id, params, timeout, token),
            Request::SubscribeAgentState(params) => request_line(id, params, timeout, token),
            Request::SpawnAgent(params) => request_line(id, params, timeout, token),
            Request::ListAgents(params) => request_line(id, params, timeout, token),
            Request::ShowAgent(params) => request_line(id, params, timeout, token),
            Request::SendToAgent(params) => request_line(id, params, timeout, token),
            Request::ReadAgent(params) => request_line(id, params, timeout, token),
            Request::SendKeys(params) => request_line(id, params, timeout, token),
            Request::KillAgent(params) => request_line(id, params, timeout, token),
            Request::WaitForAgents(params) => request_line(id, params, timeout, token),
            Request::ReportToParent(params) => request_line(id, params, timeout, token),
            Request::ReadAgentHistory(params) => request_line(id, params, timeout, token),
            Request::ListPanes(params) => request_line(id, params, timeout, token),
            Request::CreateTask(params) => request_line(id, params, timeout, token),
            Request::ListTasks(params) => request_line(id, params, timeout, token),
            Request::CloseTask(params) => request_line(id, params, timeout, token),
        }
    }

    pub fn parse(line: &str) -> Result<Self, RequestError> {
        let invalid = |id, message: &str| RequestError {
            id,
            error: ApiError::new(ErrorCode::InvalidRequest, message),
        };

        let Ok(Value::Object(mut object)) = serde_json::from_str::<Value>(line) else {
            return Err(invalid(None, "a request must be one JSON object"));
        };
        let Some(id) = object.get("id").and_then(Value::as_u64) else {
            return Err(invalid(None, "a request needs a non-negative integer id"));
        };
        let Some(Value::String(method)) = object.remove("method") else {
            return Err(invalid(Some(id), "a request needs a method name"));
        };
        let params = object.remove("params");
        let timeout = match object.remove("timeout_ms") {
            None => None,
            Some(timeout) => match timeout.as_u64() {
                Some(ms) => Some(Duration::from_millis(ms)),
                None => {
                    return Err(invalid(
                        Some(id),
                        "timeout_ms must be a non-negative integer",
                    ));
                }
            },
        };
        let token = match object.remove("token") {
            None => None,
            Some(Value::String(token)) => Some(token),
            Some(_) => return Err(invalid(Some(id), "token must be a string")),
        };

        let request = match method.as_str() {
            SplitPane::NAME => params_of(id, params).map(Request::SplitPane),
            OpenBrowser::NAME => params_of(id, params).map(Request::OpenBrowser),
            CreateWorkspace::NAME => params_of(id, params).map(Request::CreateWorkspace),
            SendText::NAME => params_of(id, params).map(Request::SendText),
            ReadOutput::NAME => params_of(id, params).map(Request::ReadOutput),
            ReportHook::NAME => params_of(id, params).map(Request::ReportHook),
            ReportChecked::NAME => params_of(id, params).map(Request::ReportChecked),
            ReadAgentState::NAME => params_of(id, params).map(Request::ReadAgentState),
            SubscribeAgentState::NAME => params_of(id, params).map(Request::SubscribeAgentState),
            SpawnAgent::NAME => params_of(id, params).map(Request::SpawnAgent),
            ListAgents::NAME => params_of(id, params).map(Request::ListAgents),
            ShowAgent::NAME => params_of(id, params).map(Request::ShowAgent),
            SendToAgent::NAME => params_of(id, params).map(Request::SendToAgent),
            ReadAgent::NAME => params_of(id, params).map(Request::ReadAgent),
            SendKeys::NAME => params_of(id, params).map(Request::SendKeys),
            KillAgent::NAME => params_of(id, params).map(Request::KillAgent),
            WaitForAgents::NAME => params_of(id, params).map(Request::WaitForAgents),
            ReportToParent::NAME => params_of(id, params).map(Request::ReportToParent),
            ReadAgentHistory::NAME => params_of(id, params).map(Request::ReadAgentHistory),
            ListPanes::NAME => params_of(id, params).map(Request::ListPanes),
            CreateTask::NAME => params_of(id, params).map(Request::CreateTask),
            ListTasks::NAME => params_of(id, params).map(Request::ListTasks),
            CloseTask::NAME => params_of(id, params).map(Request::CloseTask),
            _ => Err(RequestError {
                id: Some(id),
                error: ApiError::new(
                    ErrorCode::UnknownMethod,
                    format!("unknown method {method:?}"),
                ),
            }),
        }?;
        Ok(Self {
            id,
            request,
            timeout,
            token,
        })
    }
}

/// The request line for one operation, with how long the app has to start
/// it if there is a limit, and an optional authentication token.
pub(crate) fn request_line<M: Method>(
    id: u64,
    params: &M,
    timeout: Option<Duration>,
    token: Option<&str>,
) -> String {
    let mut request = json!({"id": id, "method": M::NAME, "params": to_value(params)});
    if let Some(timeout) = timeout {
        // Rounded down, so the app never gets more time than it was given.
        let ms = u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX);
        request["timeout_ms"] = ms.into();
    }
    if let Some(token) = token {
        request["token"] = token.into();
    }
    line(request)
}

fn params_of<M: Method>(id: u64, params: Option<Value>) -> Result<M, RequestError> {
    // Left-out params mean no params, so operations whose params are all
    // optional can be called without any.
    let params = params.unwrap_or_else(|| Value::Object(Map::new()));
    serde_json::from_value(params).map_err(|error| RequestError {
        id: Some(id),
        error: ApiError::new(
            ErrorCode::InvalidParams,
            format!("invalid params for {}: {error}", M::NAME),
        ),
    })
}

/// One line from the server.
#[derive(Clone, Debug, PartialEq)]
pub enum ServerMessage {
    Response {
        /// The id of the request. `None` when the request was too broken to
        /// have one.
        id: Option<u64>,
        result: Result<Value, ApiError>,
    },
    /// A notification: something happened that no request asked for.
    Event { event: String, params: Value },
}

/// A line from the server that is neither a response nor a notification.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InvalidServerMessage;

impl fmt::Display for InvalidServerMessage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("the server sent a line that is neither a response nor a notification")
    }
}

impl std::error::Error for InvalidServerMessage {}

impl ServerMessage {
    pub fn result<T: Serialize>(id: u64, result: &T) -> Self {
        Self::Response {
            id: Some(id),
            result: Ok(to_value(result)),
        }
    }

    pub fn error(id: Option<u64>, error: ApiError) -> Self {
        Self::Response {
            id,
            result: Err(error),
        }
    }

    /// The notification for `event`.
    pub fn event<E: Event>(event: &E) -> Self {
        Self::Event {
            event: E::NAME.to_owned(),
            params: to_value(event),
        }
    }

    /// The message as the notification `E`, when it is one.
    pub fn as_event<E: Event>(&self) -> Option<E> {
        match self {
            Self::Event { event, params } if event == E::NAME => {
                serde_json::from_value(params.clone()).ok()
            }
            _ => None,
        }
    }

    /// The message as one line, ending in a newline.
    pub fn to_line(&self) -> String {
        line(match self {
            Self::Response {
                id,
                result: Ok(result),
            } => json!({"id": id, "result": result}),
            Self::Response {
                id,
                result: Err(error),
            } => {
                json!({"id": id, "error": to_value(error)})
            }
            Self::Event { event, params } => json!({"event": event, "params": params}),
        })
    }

    pub fn parse(line: &str) -> Result<Self, InvalidServerMessage> {
        let Ok(Value::Object(mut object)) = serde_json::from_str::<Value>(line) else {
            return Err(InvalidServerMessage);
        };
        if let Some(Value::String(event)) = object.remove("event") {
            let params = object.remove("params").unwrap_or(Value::Null);
            return Ok(Self::Event { event, params });
        }

        let id = match object.remove("id") {
            Some(Value::Null) => None,
            Some(id) => Some(id.as_u64().ok_or(InvalidServerMessage)?),
            None => return Err(InvalidServerMessage),
        };
        let result = if let Some(error) = object.remove("error") {
            Err(serde_json::from_value(error).map_err(|_| InvalidServerMessage)?)
        } else {
            Ok(object.remove("result").ok_or(InvalidServerMessage)?)
        };
        Ok(Self::Response { id, result })
    }
}

fn line(value: Value) -> String {
    // serde_json escapes control characters inside strings, so the only
    // newline is the one that ends the line.
    let mut line = value.to_string();
    line.push('\n');
    line
}

fn to_value<T: Serialize>(value: &T) -> Value {
    // The types on the wire are plain structs with string keys, which always
    // serialize.
    serde_json::to_value(value).expect("wire types serialize to JSON")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_frame_omits_token_when_none_and_roundtrips_when_some() {
        let plain = RequestFrame::new(
            1,
            Request::ReadOutput(ReadOutput {
                pane: PaneId::new(3),
                lines: None,
            }),
        );
        let plain_json: Value = serde_json::from_str(&plain.to_line()).unwrap();
        assert!(plain_json.get("token").is_none());
        assert_eq!(RequestFrame::parse(&plain.to_line()).unwrap(), plain);

        let authed = plain.clone().with_token("secret-token-value");
        let authed_json: Value = serde_json::from_str(&authed.to_line()).unwrap();
        assert_eq!(authed_json["token"], "secret-token-value");
        assert_eq!(RequestFrame::parse(&authed.to_line()).unwrap(), authed);
    }

    #[test]
    fn request_frame_rejects_non_string_token_and_redacts_debug() {
        let err = RequestFrame::parse(r#"{"id":2,"method":"pane.list","token":123}"#).unwrap_err();
        assert_eq!(err.id, Some(2));
        assert_eq!(err.error.code, ErrorCode::InvalidRequest);

        let frame = RequestFrame::new(3, Request::ListPanes(ListPanes {})).with_token("top-secret");
        let debug = format!("{frame:?}");
        assert!(!debug.contains("top-secret"));
        assert!(debug.contains("<redacted>"));
    }

    #[test]
    fn auth_error_codes_serialize_as_snake_case() {
        assert_eq!(
            serde_json::to_value(ErrorCode::Unauthorized).unwrap(),
            "unauthorized"
        );
        assert_eq!(
            serde_json::to_value(ErrorCode::Forbidden).unwrap(),
            "forbidden"
        );
    }
}

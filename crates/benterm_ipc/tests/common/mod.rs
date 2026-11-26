//! A fake app and a raw socket client shared by the tests.
#![allow(dead_code)]

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use benterm_ipc::{
    AgentActivity, AgentHistory, AgentInfo, AgentList, AgentRecord, AgentShown, AgentSpawned,
    AgentTarget, AgentsWaited, ApiError, App, ConnectionId, CreateWorkspace, ErrorCode, EventSink,
    KillAgent, ListAgents, ListPanes, PaneAgentState, PaneCreated, PaneId, PaneInfo, PaneList,
    PaneOutput, ReadAgent, ReadAgentHistory, ReadAgentState, ReadOutput, Reply, ReportAccepted,
    ReportHook, ReportStatus, ReportToParent, Request, SendKeys, SendText, SendToAgent, Server,
    ServerLimits, ShowAgent, SpawnAgent, SplitPane, SubscribeAgentState, WaitForAgents,
    WaitedAgent, WaitingFor, WorkspaceCreated, WorkspaceId,
};
use serde_json::Value;

/// Reading this pane fails with `unknown_pane`.
pub const MISSING_PANE: u64 = 404;
/// The app drops the reply for a read of this pane without answering.
pub const DROPPED_PANE: u64 = 500;
/// The app keeps the reply for a read of this pane until the test answers.
pub const HELD_PANE: u64 = 600;

/// Stands in for the app: records every request and answers from the
/// request alone, so a test can tell whose answer it got.
#[derive(Default)]
pub struct FakeApp {
    pub requests: Mutex<Vec<Request>>,
    pub held: Mutex<Vec<(ReadOutput, Reply<PaneOutput>)>>,
    /// The connections that subscribed, in the order they did.
    pub sinks: Mutex<Vec<EventSink>>,
    /// The waits for the agent named [`HELD_AGENT`], with the connection
    /// each came from.
    pub waits: Mutex<Vec<(ConnectionId, Reply<AgentsWaited>)>>,
    /// The connections the server said have hung up, in that order.
    pub hung_up: Mutex<Vec<ConnectionId>>,
    /// When set, `agent.list` answers with no agents.
    pub no_agents: AtomicBool,
    /// When set, `agent.list` answers with these agents instead of the
    /// default pair.
    pub agent_list: Mutex<Option<Vec<AgentInfo>>>,
}

/// Starting an agent with this name fails with `name_taken`.
pub const TAKEN_NAME: &str = "taken";
/// A message for this agent is refused: it waits for a permission.
pub const ASKING_AGENT: &str = "asking";
/// A wait for this agent runs out at once.
pub const SLOW_AGENT: &str = "slow";
/// The app keeps the reply of a wait for this agent.
pub const HELD_AGENT: &str = "held";
/// Keys for, or killing, the agent with this name fails with
/// `unknown_agent`.
pub const MISSING_AGENT: &str = "nobody";
/// The pane the fake app gives every agent it starts or is asked about.
pub const AGENT_PANE: u64 = 20;

impl FakeApp {
    pub fn requests(&self) -> Vec<Request> {
        self.requests.lock().unwrap().clone()
    }

    /// Waits until the app holds `count` unanswered reads.
    pub fn wait_for_held(&self, count: usize) {
        wait_until(|| self.held.lock().unwrap().len() >= count);
    }

    /// Waits until `count` connections have subscribed.
    pub fn wait_for_sinks(&self, count: usize) -> Vec<EventSink> {
        wait_until(|| self.sinks.lock().unwrap().len() >= count);
        self.sinks.lock().unwrap().clone()
    }

    /// Waits until the app holds `count` unanswered waits.
    pub fn wait_for_waits(&self, count: usize) {
        wait_until(|| self.waits.lock().unwrap().len() >= count);
    }

    pub fn hung_up(&self) -> Vec<ConnectionId> {
        self.hung_up.lock().unwrap().clone()
    }

    fn record(&self, request: Request) {
        self.requests.lock().unwrap().push(request);
    }

    pub fn take_held(&self) -> Vec<(ReadOutput, Reply<PaneOutput>)> {
        std::mem::take(&mut *self.held.lock().unwrap())
    }
}

/// What the fake app answers to a read.
pub fn output_of(request: &ReadOutput) -> String {
    match request.lines {
        Some(lines) => format!("last {lines} lines of pane {}", request.pane),
        None => format!("screen of pane {}", request.pane),
    }
}

impl App for FakeApp {
    fn split_pane(&self, request: SplitPane, reply: Reply<PaneCreated>) {
        self.requests
            .lock()
            .unwrap()
            .push(Request::SplitPane(request.clone()));
        reply.ok(PaneCreated {
            pane: PaneId::new(request.pane.get() + 1),
        });
    }

    fn open_browser(&self, request: benterm_ipc::OpenBrowser, reply: Reply<PaneCreated>) {
        self.requests
            .lock()
            .unwrap()
            .push(Request::OpenBrowser(request.clone()));
        reply.ok(PaneCreated {
            pane: PaneId::new(request.pane.map_or(0, PaneId::get) + 1),
        });
    }

    fn create_workspace(&self, request: CreateWorkspace, reply: Reply<WorkspaceCreated>) {
        self.requests
            .lock()
            .unwrap()
            .push(Request::CreateWorkspace(request));
        reply.ok(WorkspaceCreated {
            workspace: WorkspaceId::new(2),
            pane: PaneId::new(10),
        });
    }

    fn send_text(&self, request: SendText, reply: Reply<()>) {
        self.requests
            .lock()
            .unwrap()
            .push(Request::SendText(request));
        reply.ok(());
    }

    fn read_output(&self, request: ReadOutput, reply: Reply<PaneOutput>) {
        self.requests
            .lock()
            .unwrap()
            .push(Request::ReadOutput(request.clone()));
        match request.pane.get() {
            MISSING_PANE => reply.err(ApiError::new(
                ErrorCode::UnknownPane,
                format!("no pane {}", request.pane),
            )),
            DROPPED_PANE => drop(reply),
            HELD_PANE => self.held.lock().unwrap().push((request, reply)),
            _ => reply.ok(PaneOutput {
                text: output_of(&request),
            }),
        }
    }

    fn report_hook(&self, request: ReportHook, reply: Reply<()>) {
        let pane = request.pane;
        self.requests
            .lock()
            .unwrap()
            .push(Request::ReportHook(request));
        match pane.get() {
            MISSING_PANE => reply.err(no_pane(pane)),
            _ => reply.ok(()),
        }
    }

    fn report_checked(&self, request: benterm_ipc::ReportChecked, reply: Reply<()>) {
        let pane = request.pane;
        self.record(Request::ReportChecked(request));
        match pane.get() {
            MISSING_PANE => reply.err(no_pane(pane)),
            _ => reply.ok(()),
        }
    }

    fn read_agent_state(&self, request: ReadAgentState, reply: Reply<PaneAgentState>) {
        let pane = request.pane;
        self.requests
            .lock()
            .unwrap()
            .push(Request::ReadAgentState(request));
        match pane.get() {
            MISSING_PANE => reply.err(no_pane(pane)),
            _ => reply.ok(agent_state_of(pane)),
        }
    }

    fn read_agent_history(&self, request: ReadAgentHistory, reply: Reply<AgentHistory>) {
        let since = request.since_ms.unwrap_or(0);
        self.requests
            .lock()
            .unwrap()
            .push(Request::ReadAgentHistory(request));
        reply.ok(AgentHistory {
            records: history().into_iter().filter(|r| r.at_ms > since).collect(),
        });
    }

    fn subscribe_agent_state(
        &self,
        request: SubscribeAgentState,
        reply: Reply<()>,
        events: EventSink,
    ) {
        self.requests
            .lock()
            .unwrap()
            .push(Request::SubscribeAgentState(request));
        // The answer goes first, so the events follow it on the wire.
        reply.ok(());
        self.sinks.lock().unwrap().push(events);
    }

    fn spawn_agent(&self, request: SpawnAgent, reply: Reply<AgentSpawned>) {
        self.record(Request::SpawnAgent(request.clone()));
        if request.name == TAKEN_NAME {
            reply.err(ApiError::new(ErrorCode::NameTaken, "the name is taken"));
        } else {
            reply.ok(AgentSpawned {
                name: request.name,
                pane: PaneId::new(AGENT_PANE),
            });
        }
    }

    fn list_agents(&self, request: ListAgents, reply: Reply<AgentList>) {
        self.record(Request::ListAgents(request));
        if self.no_agents.load(Ordering::Relaxed) {
            reply.ok(AgentList { agents: Vec::new() });
            return;
        }
        if let Some(agents) = self.agent_list.lock().unwrap().clone() {
            reply.ok(AgentList { agents });
            return;
        }
        reply.ok(AgentList {
            agents: vec![
                AgentInfo {
                    name: "w1".into(),
                    pane: PaneId::new(AGENT_PANE),
                    parent: Some(PaneId::new(3)),
                    background: true,
                    outlive_parent: false,
                    state: AgentActivity::Working,
                    report_status: None,
                    stalled: true,
                    changed_at_ms: Some(1_700_000_000_000),
                    msg: None,
                    allow_peers: false,
                },
                AgentInfo {
                    name: "w2".into(),
                    pane: PaneId::new(AGENT_PANE + 1),
                    parent: None,
                    background: false,
                    outlive_parent: true,
                    state: AgentActivity::Done,
                    report_status: Some(ReportStatus::Issues),
                    stalled: false,
                    changed_at_ms: None,
                    msg: None,
                    allow_peers: false,
                },
            ],
        });
    }

    fn create_task(
        &self,
        request: benterm_ipc::CreateTask,
        reply: Reply<benterm_ipc::TaskCreated>,
    ) {
        let task = task_info(4, &request.name, request.branch.as_deref());
        self.record(Request::CreateTask(request));
        reply.ok(benterm_ipc::TaskCreated {
            task,
            pane: PaneId::new(11),
        });
    }

    fn list_tasks(&self, request: benterm_ipc::ListTasks, reply: Reply<benterm_ipc::TaskList>) {
        self.record(Request::ListTasks(request));
        let mut closed = task_info(1, "done", None);
        closed.workspace = None;
        closed.closed = Some(benterm_ipc::TaskClosure {
            at_ms: 9,
            worktree: benterm_ipc::WorktreeLeft::Removed,
        });
        reply.ok(benterm_ipc::TaskList {
            tasks: vec![closed, task_info(4, "kanban", None)],
        });
    }

    fn close_task(&self, request: benterm_ipc::CloseTask, reply: Reply<benterm_ipc::TaskClosed>) {
        self.record(Request::CloseTask(request.clone()));
        if request.task == DIRTY_TASK && !request.force {
            reply.err(ApiError::new(
                ErrorCode::AtRisk,
                "removing it would lose 2 uncommitted files",
            ));
            return;
        }
        let mut task = task_info(request.task, "kanban", None);
        task.workspace = None;
        reply.ok(benterm_ipc::TaskClosed {
            task,
            branch_left: (request.task == BRANCH_LEFT_TASK)
                .then(|| "the branch kanban was not deleted: it is checked out".to_owned()),
        });
    }

    fn list_panes(&self, request: ListPanes, reply: Reply<PaneList>) {
        self.record(Request::ListPanes(request));
        let pane = |pane, workspace: Option<(u64, u32)>, name: Option<&str>| PaneInfo {
            pane: PaneId::new(pane),
            workspace: workspace.map(|(workspace, _)| WorkspaceId::new(workspace)),
            tab: workspace.map(|(_, tab)| tab),
            name: name.map(str::to_owned),
            cwd: None,
            agent: None,
            state: AgentActivity::Unknown,
            reason: None,
        };
        reply.ok(PaneList {
            panes: vec![
                PaneInfo {
                    cwd: Some("/work/my repo".into()),
                    agent: Some("claude".into()),
                    state: AgentActivity::Working,
                    ..pane(3, Some((1, 1)), None)
                },
                pane(4, Some((1, 2)), None),
                PaneInfo {
                    cwd: Some("/work".into()),
                    agent: Some("codex".into()),
                    state: AgentActivity::Waiting,
                    reason: Some(WaitingFor::Question),
                    ..pane(AGENT_PANE, None, Some("w1"))
                },
            ],
        });
    }

    fn show_agent(&self, request: ShowAgent, reply: Reply<AgentShown>) {
        let second = request.agent == AgentTarget::Name("w2".into());
        self.record(Request::ShowAgent(request));
        reply.ok(match second {
            false => AgentShown {
                pane: PaneId::new(AGENT_PANE),
                stalled: true,
                changed_at_ms: Some(1_700_000_000_000),
                report_status: None,
            },
            true => AgentShown {
                pane: PaneId::new(AGENT_PANE + 1),
                stalled: false,
                changed_at_ms: None,
                report_status: Some(ReportStatus::Blocked),
            },
        });
    }

    fn send_to_agent(&self, request: SendToAgent, reply: Reply<()>) {
        let asking = request.agent == AgentTarget::Name(ASKING_AGENT.into());
        self.record(Request::SendToAgent(request));
        if asking {
            reply.err(ApiError::new(
                ErrorCode::AgentWaiting,
                "asking is waiting for a permission",
            ));
            return;
        }
        reply.ok(());
    }

    fn read_agent(&self, request: ReadAgent, reply: Reply<PaneOutput>) {
        self.record(Request::ReadAgent(request.clone()));
        reply.ok(PaneOutput {
            text: format!("screen of agent {}", request.agent),
        });
    }

    fn send_keys(&self, request: SendKeys, reply: Reply<()>) {
        self.record(Request::SendKeys(request.clone()));
        reply.send(missing_agent(&request.agent));
    }

    fn kill_agent(&self, request: KillAgent, reply: Reply<()>) {
        self.record(Request::KillAgent(request.clone()));
        reply.send(missing_agent(&request.agent));
    }

    fn wait_for_agents(
        &self,
        request: WaitForAgents,
        reply: Reply<AgentsWaited>,
        connection: ConnectionId,
    ) -> bool {
        self.record(Request::WaitForAgents(request.clone()));
        match request.agent {
            Some(AgentTarget::Name(name)) if name == SLOW_AGENT => reply.err(ApiError::new(
                ErrorCode::WaitTimedOut,
                "slow is still working",
            )),
            Some(AgentTarget::Name(name)) if name == HELD_AGENT => {
                self.waits.lock().unwrap().push((connection, reply));
                return true;
            }
            _ => reply.ok(AgentsWaited {
                agents: vec![WaitedAgent {
                    name: Some("w1".into()),
                    pane: PaneId::new(AGENT_PANE),
                    closed: false,
                    state: AgentActivity::Done,
                    reason: None,
                }],
            }),
        }
        false
    }

    fn report_to_parent(&self, request: ReportToParent, reply: Reply<ReportAccepted>) {
        self.record(Request::ReportToParent(request));
        reply.ok(ReportAccepted {
            parent: PaneId::new(3),
        });
    }

    fn hung_up(&self, connection: ConnectionId) {
        self.hung_up.lock().unwrap().push(connection);
    }
}

/// `unknown_agent` for [`MISSING_AGENT`].
fn missing_agent(agent: &AgentTarget) -> Result<(), ApiError> {
    match agent {
        AgentTarget::Name(name) if name == MISSING_AGENT => Err(ApiError::new(
            ErrorCode::UnknownAgent,
            format!("there is no agent named {name:?}"),
        )),
        _ => Ok(()),
    }
}

/// What the fake app answers when asked for the agent state of `pane`.
pub fn agent_state_of(pane: PaneId) -> PaneAgentState {
    PaneAgentState {
        pane,
        agent: Some("claude".into()),
        state: AgentActivity::Waiting,
        reason: Some(WaitingFor::Question),
    }
}

fn no_pane(pane: PaneId) -> ApiError {
    ApiError::new(ErrorCode::UnknownPane, format!("no pane {pane}"))
}

/// A server with a fake app, on a socket in a fresh private directory.
pub struct TestServer {
    pub app: Arc<FakeApp>,
    pub server: Server,
    pub path: PathBuf,
    _dir: tempfile::TempDir,
}

/// A fresh directory that only the user can use, like the real socket
/// directory. `tempfile::tempdir` alone leaves it open to others.
pub fn private_dir() -> tempfile::TempDir {
    use std::os::unix::fs::PermissionsExt;
    tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap()
}

pub fn start_server() -> TestServer {
    start_server_with(ServerLimits::default())
}

/// A server like [`start_server`]'s that reads its clients' lines with
/// `limits`.
pub fn start_server_with(limits: ServerLimits) -> TestServer {
    let dir = private_dir();
    let path = dir.path().join("benterm.sock");
    let app = Arc::new(FakeApp::default());
    let server = Server::bind_with(&path, app.clone(), limits).unwrap();
    TestServer {
        app,
        server,
        path,
        _dir: dir,
    }
}

/// A client that writes and reads raw lines.
pub struct RawClient {
    pub stream: UnixStream,
    reader: BufReader<UnixStream>,
}

impl RawClient {
    pub fn connect(path: &Path) -> Self {
        let stream = UnixStream::connect(path).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let reader = BufReader::new(stream.try_clone().unwrap());
        Self { stream, reader }
    }

    pub fn send(&mut self, line: &str) {
        self.stream.write_all(line.as_bytes()).unwrap();
        self.stream.write_all(b"\n").unwrap();
    }

    /// The next line from the server, or `None` at end of file.
    pub fn read(&mut self) -> Option<Value> {
        let mut line = String::new();
        match self.reader.read_line(&mut line).unwrap() {
            0 => None,
            _ => Some(serde_json::from_str(&line).unwrap()),
        }
    }

    pub fn call(&mut self, line: &str) -> Value {
        self.send(line);
        self.read().expect("the server closed the connection")
    }
}

pub fn wait_until(mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(Instant::now() < deadline, "timed out");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// The history the fake app has: a turn of Claude Code in pane 3.
pub fn history() -> Vec<AgentRecord> {
    let record = |at_ms, state| AgentRecord {
        at_ms,
        key: "0123456789abcdef0123456789abcdef".into(),
        pane: PaneId::new(3),
        agent: Some("claude".into()),
        state,
        reason: None,
    };
    vec![
        record(1_000, AgentActivity::Working),
        record(2_000, AgentActivity::Done),
    ]
}

/// The task `task.close` refuses to remove without `force`.
pub const DIRTY_TASK: u64 = 5;
/// The task whose branch `task.close` says is left.
pub const BRANCH_LEFT_TASK: u64 = 6;

/// A task as the made-up app tells it.
pub fn task_info(id: u64, name: &str, branch: Option<&str>) -> benterm_ipc::TaskInfo {
    let branch = branch.unwrap_or(name);
    benterm_ipc::TaskInfo {
        id,
        name: name.to_owned(),
        repository: "/work/benterm".into(),
        base_branch: "main".into(),
        base_commit: "0123456789abcdef0123456789abcdef01234567".into(),
        branch: branch.to_owned(),
        worktree: format!("/trees/benterm/{branch}"),
        role: None,
        created_at_ms: 7,
        closed: None,
        workspace: Some(WorkspaceId::new(2)),
    }
}

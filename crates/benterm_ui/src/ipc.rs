//! The socket API in the app: how a request gets from the socket's
//! threads to the window, and what the window does with it.
//!
//! The server in `benterm_ipc` calls [`ApiSender`] on a connection's
//! reader thread, which must not wait. The sender puts the request and its
//! reply in a queue, and one task on the UI thread takes them out and
//! carries them out, one per update.
//!
//! The window also tells the connections that subscribed when the agent
//! state of a pane changes. That happens on the UI thread, inside the
//! update that changed the state, and never waits: an event is put in the
//! connection's queue in `benterm_ipc`, or the connection is dropped.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::fmt;
use std::io;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::SystemTime;

use benterm_agent::{AgentState, HookEvent, PlanUpdate, WaitReason};
use benterm_ipc::{
    AgentActivity, AgentHistory, AgentList, AgentShown, AgentSpawned, AgentStateChanged,
    AgentsWaited, ApiError, CONTROL_TOKEN_ENV, CloseTask, ConnectionId, CreateTask,
    CreateWorkspace, ErrorCode, Event, EventSink, KillAgent, ListAgents, ListPanes, ListTasks,
    OpenBrowser, PANE_ID_ENV, PANE_TOKEN_ENV, PaneAgentState, PaneClosed, PaneCreated, PaneInfo,
    PaneList, PaneOutput, PermissionTable, ReadAgent, ReadAgentHistory, ReadAgentState, ReadOutput,
    Reply, ReportAccepted, ReportChecked, ReportHook, ReportToParent, SOCKET_ENV, SendKeys,
    SendText, SendToAgent, Server, ServerLimits, ShowAgent, SpawnAgent, SplitPane,
    SubscribeAgentState, TaskClosed, TaskCreated, TaskList, WaitForAgents, WaitingFor,
    WorkspaceCreated,
};
use benterm_layout::{PaneId, SplitDirection};
use benterm_session::{RestrictedPane, Terminal as SavedTerminal};

use crate::browser::BrowserError;
use benterm_terminal::{PtyConfig, WriteError};
use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use futures::{FutureExt, StreamExt};
use gpui::{App, Context, Entity, Window, WindowHandle};

use crate::agent_feed::AgentFeed;
use crate::pane_area::pane_area;
use crate::pane_grid::{SplitError, spawn_in};
use crate::settings::ConfigFeed;
use crate::theme::TerminalTheme;
use crate::{
    AgentNews, AgentStatus, BentermWindow, FirstPane, PaneGrid, SpawnTerminal, StartedTerminal,
    TerminalView,
};

/// How many calls wait for the UI thread before new ones are refused. The
/// UI takes a call in well under a frame, so only a UI that has stopped
/// gets near this.
pub const QUEUE_CAPACITY: usize = 64;

/// One request of the socket API with the right to answer it.
pub(crate) enum Call {
    SplitPane(SplitPane, Reply<PaneCreated>),
    OpenBrowser(OpenBrowser, Reply<PaneCreated>),
    CreateWorkspace(CreateWorkspace, Reply<WorkspaceCreated>),
    SendText(SendText, Reply<()>),
    ReadOutput(ReadOutput, Reply<PaneOutput>),
    ReportHook(ReportHook, Reply<()>),
    ReportChecked(ReportChecked, Reply<()>),
    ReadAgentState(ReadAgentState, Reply<PaneAgentState>),
    SubscribeAgentState(Reply<()>, EventSink),
    SpawnAgent(SpawnAgent, Reply<AgentSpawned>),
    ListAgents(ListAgents, Reply<AgentList>),
    ShowAgent(ShowAgent, Reply<AgentShown>),
    SendToAgent(SendToAgent, Reply<()>),
    ReadAgent(ReadAgent, Reply<PaneOutput>),
    SendKeys(SendKeys, Reply<()>),
    KillAgent(KillAgent, Reply<()>),
    WaitForAgents(WaitForAgents, Reply<AgentsWaited>, ConnectionId),
    ReportToParent(ReportToParent, Reply<ReportAccepted>),
    /// Not a request: a connection that waited has hung up. Nobody is
    /// answered, so it has no time to run out and is not counted against
    /// the capacity of the queue.
    HungUp(ConnectionId),
    ReadAgentHistory(ReadAgentHistory, Reply<AgentHistory>),
    ListPanes(ListPanes, Reply<PaneList>),
    CreateTask(CreateTask, Reply<TaskCreated>),
    ListTasks(ListTasks, Reply<TaskList>),
    CloseTask(CloseTask, Reply<TaskClosed>),
}

impl Call {
    /// Whether the call takes one of the places of the queue. A notice
    /// that a connection hung up does not: it must not be refused, or the
    /// waits of the connection would never be given up. There is at most
    /// one for each connection that has a wait in the queue or in the
    /// window, so they are bounded by the waits, which are counted.
    fn counted(&self) -> bool {
        !matches!(self, Self::HungUp(_))
    }

    /// Whether carrying out the call needs the user's login shell
    /// environment first.
    fn needs_login_env(&self) -> bool {
        matches!(self, Self::SpawnAgent(..) | Self::CreateTask(..))
    }

    /// The agent name this call targets, if it addresses an agent by name.
    fn target_agent_name(&self) -> Option<&str> {
        let target = match self {
            Self::ShowAgent(req, _) => Some(&req.agent),
            Self::SendToAgent(req, _) => Some(&req.agent),
            Self::ReadAgent(req, _) => Some(&req.agent),
            Self::SendKeys(req, _) => Some(&req.agent),
            Self::KillAgent(req, _) => Some(&req.agent),
            Self::WaitForAgents(req, _, _) => req.agent.as_ref(),
            _ => None,
        };
        match target? {
            benterm_ipc::AgentTarget::Name(name) => Some(name.as_str()),
            benterm_ipc::AgentTarget::Pane(_) => None,
        }
    }

    /// Whether this call must stay behind a call already in `deferred`
    /// so that operations on the pane or agent that deferred call will
    /// make run after it.
    fn depends_on_deferred(&self, deferred: &VecDeque<Call>) -> bool {
        if let Some(name) = self.target_agent_name()
            && deferred.iter().any(|pending| match pending {
                Self::SpawnAgent(spawn, _) => spawn.name == name,
                Self::CreateTask(task, _) => task.role.as_deref() == Some(name),
                _ => false,
            })
        {
            return true;
        }
        match self {
            Self::WaitForAgents(req, _, _) => req.children_of.is_some_and(|parent| {
                deferred.iter().any(|pending| {
                    matches!(pending, Self::SpawnAgent(spawn, _) if spawn.parent == Some(parent))
                })
            }),
            Self::HungUp(conn) => deferred.iter().any(|pending| {
                matches!(pending, Self::WaitForAgents(_, _, deferred_conn) if deferred_conn == conn)
            }),
            _ => false,
        }
    }

    /// Whether the time the caller gave the app has run out.
    fn expired(&self) -> bool {
        match self {
            Self::SplitPane(_, reply) => reply.expired(),
            Self::OpenBrowser(_, reply) => reply.expired(),
            Self::CreateWorkspace(_, reply) => reply.expired(),
            Self::SendText(_, reply) => reply.expired(),
            Self::ReadOutput(_, reply) => reply.expired(),
            Self::ReportHook(_, reply) => reply.expired(),
            Self::ReportChecked(_, reply) => reply.expired(),
            Self::ReadAgentState(_, reply) => reply.expired(),
            Self::SubscribeAgentState(reply, _) => reply.expired(),
            Self::SpawnAgent(_, reply) => reply.expired(),
            Self::ListAgents(_, reply) => reply.expired(),
            Self::ShowAgent(_, reply) => reply.expired(),
            Self::SendToAgent(_, reply) => reply.expired(),
            Self::ReadAgent(_, reply) => reply.expired(),
            Self::SendKeys(_, reply) => reply.expired(),
            Self::KillAgent(_, reply) => reply.expired(),
            Self::WaitForAgents(_, reply, _) => reply.expired(),
            Self::ReportToParent(_, reply) => reply.expired(),
            Self::HungUp(_) => false,
            Self::ReadAgentHistory(_, reply) => reply.expired(),
            Self::ListPanes(_, reply) => reply.expired(),
            Self::CreateTask(_, reply) => reply.expired(),
            Self::ListTasks(_, reply) => reply.expired(),
            Self::CloseTask(_, reply) => reply.expired(),
        }
    }

    /// Answers that the call was not carried out because its time ran out.
    fn expire(self) {
        match self {
            Self::SplitPane(_, reply) => reply.expire(),
            Self::OpenBrowser(_, reply) => reply.expire(),
            Self::CreateWorkspace(_, reply) => reply.expire(),
            Self::SendText(_, reply) => reply.expire(),
            Self::ReadOutput(_, reply) => reply.expire(),
            Self::ReportHook(_, reply) => reply.expire(),
            Self::ReportChecked(_, reply) => reply.expire(),
            Self::ReadAgentState(_, reply) => reply.expire(),
            Self::SubscribeAgentState(reply, _) => reply.expire(),
            Self::SpawnAgent(_, reply) => reply.expire(),
            Self::ListAgents(_, reply) => reply.expire(),
            Self::ShowAgent(_, reply) => reply.expire(),
            Self::SendToAgent(_, reply) => reply.expire(),
            Self::ReadAgent(_, reply) => reply.expire(),
            Self::SendKeys(_, reply) => reply.expire(),
            Self::KillAgent(_, reply) => reply.expire(),
            Self::WaitForAgents(_, reply, _) => reply.expire(),
            Self::ReportToParent(_, reply) => reply.expire(),
            Self::HungUp(_) => {}
            Self::ReadAgentHistory(_, reply) => reply.expire(),
            Self::ListPanes(_, reply) => reply.expire(),
            Self::CreateTask(_, reply) => reply.expire(),
            Self::ListTasks(_, reply) => reply.expire(),
            Self::CloseTask(_, reply) => reply.expire(),
        }
    }

    /// Answers that the app could not carry the call out.
    pub(crate) fn fail(self, message: &str) {
        let error = ApiError::new(ErrorCode::Failed, message);
        match self {
            Self::SplitPane(_, reply) => reply.err(error),
            Self::OpenBrowser(_, reply) => reply.err(error),
            Self::CreateWorkspace(_, reply) => reply.err(error),
            Self::SendText(_, reply) => reply.err(error),
            Self::ReadOutput(_, reply) => reply.err(error),
            Self::ReportHook(_, reply) => reply.err(error),
            Self::ReportChecked(_, reply) => reply.err(error),
            Self::ReadAgentState(_, reply) => reply.err(error),
            Self::SubscribeAgentState(reply, _) => reply.err(error),
            Self::SpawnAgent(_, reply) => reply.err(error),
            Self::ListAgents(_, reply) => reply.err(error),
            Self::ShowAgent(_, reply) => reply.err(error),
            Self::SendToAgent(_, reply) => reply.err(error),
            Self::ReadAgent(_, reply) => reply.err(error),
            Self::SendKeys(_, reply) => reply.err(error),
            Self::KillAgent(_, reply) => reply.err(error),
            Self::WaitForAgents(_, reply, _) => reply.err(error),
            Self::ReportToParent(_, reply) => reply.err(error),
            Self::HungUp(_) => {}
            Self::ReadAgentHistory(_, reply) => reply.err(error),
            Self::ListPanes(_, reply) => reply.err(error),
            Self::CreateTask(_, reply) => reply.err(error),
            Self::ListTasks(_, reply) => reply.err(error),
            Self::CloseTask(_, reply) => reply.err(error),
        }
    }
}

/// The reader threads' end of the queue: the app the server calls.
pub struct ApiSender {
    calls: UnboundedSender<Call>,
    waiting: Arc<AtomicUsize>,
    capacity: usize,
}

/// The UI thread's end of the queue. Dropping it answers the calls still
/// in it, and every later call, with an error.
pub struct ApiCalls {
    calls: UnboundedReceiver<Call>,
    /// Calls set aside while the user's login shell environment is still
    /// pending. Still counted in
    /// `waiting` so the total held by the bridge stays within `capacity`.
    deferred: VecDeque<Call>,
    waiting: Arc<AtomicUsize>,
}

/// A queue that holds up to `capacity` calls.
pub fn queue(capacity: usize) -> (ApiSender, ApiCalls) {
    let (sender, receiver) = unbounded();
    let waiting = Arc::new(AtomicUsize::new(0));
    (
        ApiSender {
            calls: sender,
            waiting: waiting.clone(),
            capacity,
        },
        ApiCalls {
            calls: receiver,
            deferred: VecDeque::new(),
            waiting,
        },
    )
}

impl ApiSender {
    /// Never waits: a call that
    /// cannot be queued is answered here. Returns whether it was queued.
    fn submit(&self, call: Call) -> bool {
        if !call.counted() {
            // A window that has closed keeps no waits.
            return self.calls.unbounded_send(call).is_ok();
        }
        // Readers that find the queue full at the same moment may count
        // past the capacity for an instant, which only refuses a call
        // that was about to be refused anyway.
        if self.waiting.fetch_add(1, Ordering::AcqRel) >= self.capacity {
            self.waiting.fetch_sub(1, Ordering::AcqRel);
            call.fail("the app is busy: too many requests are waiting");
            return false;
        }
        if let Err(refused) = self.calls.unbounded_send(call) {
            self.waiting.fetch_sub(1, Ordering::AcqRel);
            refused.into_inner().fail("the window has closed");
            return false;
        }
        true
    }
}

impl benterm_ipc::App for ApiSender {
    fn split_pane(&self, request: SplitPane, reply: Reply<PaneCreated>) {
        self.submit(Call::SplitPane(request, reply));
    }

    fn open_browser(&self, request: OpenBrowser, reply: Reply<PaneCreated>) {
        self.submit(Call::OpenBrowser(request, reply));
    }

    fn create_workspace(&self, request: CreateWorkspace, reply: Reply<WorkspaceCreated>) {
        self.submit(Call::CreateWorkspace(request, reply));
    }

    fn send_text(&self, request: SendText, reply: Reply<()>) {
        self.submit(Call::SendText(request, reply));
    }

    fn read_output(&self, request: ReadOutput, reply: Reply<PaneOutput>) {
        self.submit(Call::ReadOutput(request, reply));
    }

    fn report_hook(&self, request: ReportHook, reply: Reply<()>) {
        self.submit(Call::ReportHook(request, reply));
    }

    fn report_checked(&self, request: ReportChecked, reply: Reply<()>) {
        self.submit(Call::ReportChecked(request, reply));
    }

    fn list_panes(&self, request: ListPanes, reply: Reply<PaneList>) {
        self.submit(Call::ListPanes(request, reply));
    }

    fn read_agent_state(&self, request: ReadAgentState, reply: Reply<PaneAgentState>) {
        self.submit(Call::ReadAgentState(request, reply));
    }

    fn read_agent_history(&self, request: ReadAgentHistory, reply: Reply<AgentHistory>) {
        self.submit(Call::ReadAgentHistory(request, reply));
    }

    fn subscribe_agent_state(&self, _: SubscribeAgentState, reply: Reply<()>, events: EventSink) {
        self.submit(Call::SubscribeAgentState(reply, events));
    }

    fn spawn_agent(&self, request: SpawnAgent, reply: Reply<AgentSpawned>) {
        self.submit(Call::SpawnAgent(request, reply));
    }

    fn list_agents(&self, request: ListAgents, reply: Reply<AgentList>) {
        self.submit(Call::ListAgents(request, reply));
    }

    fn show_agent(&self, request: ShowAgent, reply: Reply<AgentShown>) {
        self.submit(Call::ShowAgent(request, reply));
    }

    fn send_to_agent(&self, request: SendToAgent, reply: Reply<()>) {
        self.submit(Call::SendToAgent(request, reply));
    }

    fn read_agent(&self, request: ReadAgent, reply: Reply<PaneOutput>) {
        self.submit(Call::ReadAgent(request, reply));
    }

    fn send_keys(&self, request: SendKeys, reply: Reply<()>) {
        self.submit(Call::SendKeys(request, reply));
    }

    fn kill_agent(&self, request: KillAgent, reply: Reply<()>) {
        self.submit(Call::KillAgent(request, reply));
    }

    fn wait_for_agents(
        &self,
        request: WaitForAgents,
        reply: Reply<AgentsWaited>,
        connection: ConnectionId,
    ) -> bool {
        // A wait that was not queued has been answered here, and leaves
        // nothing to give up when its connection ends.
        self.submit(Call::WaitForAgents(request, reply, connection))
    }

    fn report_to_parent(&self, request: ReportToParent, reply: Reply<ReportAccepted>) {
        self.submit(Call::ReportToParent(request, reply));
    }

    fn hung_up(&self, connection: ConnectionId) {
        self.submit(Call::HungUp(connection));
    }

    fn create_task(&self, request: CreateTask, reply: Reply<TaskCreated>) {
        self.submit(Call::CreateTask(request, reply));
    }

    fn list_tasks(&self, request: ListTasks, reply: Reply<TaskList>) {
        self.submit(Call::ListTasks(request, reply));
    }

    fn close_task(&self, request: CloseTask, reply: Reply<TaskClosed>) {
        self.submit(Call::CloseTask(request, reply));
    }
}

impl ApiCalls {
    /// How many calls are in the queue now, including deferred ones.
    pub fn waiting(&self) -> usize {
        self.waiting.load(Ordering::Acquire)
    }

    /// The oldest incoming call, once there is one, without releasing its
    /// capacity slot yet so a deferred call stays counted against capacity.
    async fn next_incoming(&mut self) -> Option<Call> {
        self.calls.next().await
    }

    /// Releases the capacity slot of a call that is about to be carried
    /// out or failed.
    fn release(&self, call: &Call) {
        if call.counted() {
            self.waiting.fetch_sub(1, Ordering::AcqRel);
        }
    }

    /// Sets `call` aside until the login shell environment arrives.
    fn defer(&mut self, call: Call) {
        self.deferred.push_back(call);
    }

    fn has_deferred(&self) -> bool {
        !self.deferred.is_empty()
    }

    fn depends_on_deferred(&self, call: &Call) -> bool {
        call.depends_on_deferred(&self.deferred)
    }

    /// Takes the oldest deferred call to carry it out now.
    fn take_deferred(&mut self) -> Option<Call> {
        let call = self.deferred.pop_front()?;
        self.release(&call);
        Some(call)
    }
}

impl Drop for ApiCalls {
    /// The calls still in the queue (including deferred ones) are answered
    /// here, and every later one where it is submitted.
    fn drop(&mut self) {
        while let Some(call) = self.deferred.pop_front() {
            self.release(&call);
            call.fail(WINDOW_CLOSED);
        }
        self.calls.close();
        loop {
            match self.calls.next().now_or_never() {
                Some(Some(call)) => {
                    self.release(&call);
                    call.fail(WINDOW_CLOSED);
                }
                // A closed channel that holds nothing has ended.
                Some(None) => break,
                // A reader is in the middle of putting a call in. It
                // does not wait for anything, so the call is there in a
                // moment.
                None => std::thread::yield_now(),
            }
        }
    }
}

const WINDOW_CLOSED: &str = "the window has closed";

pub type StartTerminal = Rc<dyn Fn(PtyConfig, &TerminalTheme) -> io::Result<StartedTerminal>>;

/// What every pane of the app runs, and how its program finds the app.
pub(crate) struct Shells {
    pub(crate) pty: PtyConfig,
    start: StartTerminal,
    /// The socket the app listens on, if it does.
    socket: Option<PathBuf>,
    /// Server-side IPC permission table, shared with the IPC server when
    /// listening.
    permissions: Option<PermissionTable>,
    /// The connections that subscribed to the agent states.
    watchers: Rc<Watchers>,
    /// The receivers in the app of the agent states.
    agents: Rc<AgentFeed>,
    /// The environment of the user's shell, once it has come.
    pub(crate) login_env: crate::login_env::LoginEnv,
    /// Whether the app checks the reports of agents, as the settings
    /// say now.
    pub(crate) report_check: std::cell::Cell<bool>,
}

/// The connections to tell when the agent state of a pane changes. Only
/// the UI thread touches them.
#[derive(Default)]
struct Watchers {
    sinks: RefCell<Vec<EventSink>>,
}

impl Watchers {
    /// Queues the event for every subscriber and
    /// forgets the ones that are gone. Never waits.
    fn publish<E: Event>(&self, event: &E) {
        self.sinks.borrow_mut().retain(|sink| sink.send(event));
    }

    /// Keeps the sink, unless its connection has subscribed before: a
    /// connection is told each change once.
    fn add(&self, sink: EventSink) {
        let mut sinks = self.sinks.borrow_mut();
        sinks.retain(|sink| !sink.is_closed());
        if !sinks.iter().any(|kept| kept.same_connection(&sink)) {
            sinks.push(sink);
        }
    }
}

impl Shells {
    /// The credentials the socket API checks, when the app listens.
    pub(crate) fn permissions(&self) -> Option<&PermissionTable> {
        self.permissions.as_ref()
    }

    pub(crate) fn restore_restricted(
        self: &Rc<Self>,
        pane: PaneId,
        theme: &TerminalTheme,
        terminal: &SavedTerminal,
        restricted: &RestrictedPane,
        roles: &[benterm_config::Role],
    ) -> io::Result<StartedTerminal> {
        let program = restricted.role.as_deref().and_then(|name| {
            let role = roles.iter().find(|role| role.name == name)?;
            let conversation = terminal.conversation.as_ref()?;
            crate::roles::resume_program(self, role, conversation)
        });
        let spawn = self.spawn_program_with_permissions(
            program.unwrap_or_else(|| self.pty.clone()),
            None,
            false,
        );
        let mut started = spawn_in(
            &spawn,
            pane,
            theme,
            terminal.directory(),
            Default::default(),
        )?;
        started.restricted = Some(restricted.clone());
        if restricted.role.is_none()
            && let Some(conversation) = &terminal.conversation
        {
            started.type_first(conversation.resume_command());
        }
        Ok(started)
    }

    /// Starts the terminals of panes whose programs begin in `cwd`, or
    /// where `pty` says without one. Each program is told the socket and
    /// the id of its pane.
    ///
    /// These are panes of the window, which the user opened or a pane
    /// with control asked for, so each gets a control credential.
    pub(crate) fn spawn_in(self: &Rc<Self>, cwd: Option<PathBuf>) -> SpawnTerminal {
        self.spawn_program(self.pty.clone(), cwd)
    }

    /// Like [`spawn_in`](Self::spawn_in), for terminals that run `program`
    /// instead of what every pane runs.
    pub(crate) fn spawn_program(
        self: &Rc<Self>,
        program: PtyConfig,
        cwd: Option<PathBuf>,
    ) -> SpawnTerminal {
        self.spawn_program_with_permissions(program, cwd, true)
    }

    /// Every terminal gets a credential for its own pane. One started with
    /// `control` gets a credential for the whole API besides.
    pub(crate) fn spawn_program_with_permissions(
        self: &Rc<Self>,
        program: PtyConfig,
        cwd: Option<PathBuf>,
        control: bool,
    ) -> SpawnTerminal {
        let shells = self.clone();
        Rc::new(move |pane, theme, directory, size| {
            // A pane opened beside another starts where that one is, and
            // the first pane of a workspace where the workspace says.
            let mut pty = crate::in_directory(&program, directory.or(cwd.as_deref()));
            pty.size = size;
            if pty.base_env.is_none() {
                pty.base_env = shells.login_env.now();
            }
            // Without a socket the name is set to nothing, so a program
            // does not take the socket of an app this one was started
            // in for its own.
            let socket = shells
                .socket
                .as_ref()
                .map(|socket| socket.clone().into_os_string())
                .unwrap_or_default();
            // First, so that variables the program was given, as a role
            // gives its agent, are put over them. Reserved IPC token variables
            // are stripped from caller-supplied overrides and set afterwards
            // by Benterm so roles or `agent.spawn` env cannot spoof or replace
            // them.
            let given: Vec<_> = std::mem::take(&mut pty.env)
                .into_iter()
                .filter(|(name, _)| name != PANE_TOKEN_ENV && name != CONTROL_TOKEN_ENV)
                .collect();
            pty.env = vec![
                (SOCKET_ENV.into(), socket),
                (PANE_ID_ENV.into(), pane.get().to_string().into()),
            ];
            // One decision for the life of the pane. Without a socket
            // the app can take no report.
            if shells.report_check.get() && shells.socket.is_some() {
                pty.env
                    .push((benterm_ipc::REPORT_CHECK_ENV.into(), "on".into()));
            }
            pty.env.extend(given);
            let pane_api_id = api_id(pane);
            // The credentials are in the program's environment, where
            // another process of the user can read them. So they are
            // taken only from the processes of this pane's program,
            // which the table is told once the program has started.
            if let Some(permissions) = &shells.permissions {
                permissions.expect_pane_process(pane_api_id);
            }
            let minted_token = shells
                .permissions
                .as_ref()
                .map(|permissions| permissions.issue_pane_token(pane_api_id));
            if let Some(token) = &minted_token {
                pty.env.push((PANE_TOKEN_ENV.into(), token.into()));
            }
            if control && let Some(permissions) = &shells.permissions {
                let token = permissions.issue_control_token(Some(pane_api_id));
                pty.env.push((CONTROL_TOKEN_ENV.into(), token.into()));
            }
            let mut started = match (shells.start)(pty, theme) {
                Ok(started) => started,
                Err(error) => {
                    if let Some(permissions) = &shells.permissions {
                        permissions.revoke_pane(pane_api_id);
                    }
                    return Err(error);
                }
            };
            // A terminal whose program has ended already tells no process,
            // and nobody is left to use its credentials.
            if let (Some(permissions), Some(pid)) = (&shells.permissions, started.process_id()) {
                permissions.bind_pane_process(pane_api_id, pid);
            }
            if !control {
                started.restricted = Some(RestrictedPane::default());
            }
            let watchers = shells.watchers.clone();
            let agents = shells.agents.clone();
            let permissions = shells.permissions.clone();
            Ok(started.tell_agent_news(Box::new(move |news| {
                agents.publish(pane, news, SystemTime::now());
                let pane = api_id(pane);
                match news {
                    AgentNews::Changed(status) => {
                        watchers.publish(&AgentStateChanged(agent_state(pane, status)));
                    }
                    AgentNews::Closed => {
                        if let Some(permissions) = &permissions {
                            permissions.revoke_pane(pane);
                        }
                        watchers.publish(&PaneClosed { pane });
                    }
                }
            })))
        })
    }
}

/// Opens a window whose panes each run `pty`, started by `start`, and
/// serves the socket API for it on `socket`.
///
/// Every pane's program gets `BENTERM_SOCKET` and `BENTERM_PANE_ID`.
/// Without a `socket`, or when it cannot be listened on, the window opens
/// all the same and the app has no socket API. A socket that cannot be
/// listened on is named in the window's bar, with the reason. Sockets
/// that crashed apps left in the socket's directory are removed.
pub fn open_window(
    pty: PtyConfig,
    start: StartTerminal,
    socket: Option<PathBuf>,
    feed: ConfigFeed,
    cx: &mut App,
) -> anyhow::Result<WindowHandle<BentermWindow>> {
    open_session_window(pty, start, socket, None, feed, cx)
}

/// Like [`open_window`], with the workspaces, tabs and panes of the
/// session saved at `session`, which the window then keeps up to date.
/// See [`crate::session::open_session_window`].
pub fn open_session_window(
    pty: PtyConfig,
    start: StartTerminal,
    socket: Option<PathBuf>,
    session: Option<PathBuf>,
    feed: ConfigFeed,
    cx: &mut App,
) -> anyhow::Result<WindowHandle<BentermWindow>> {
    let (sender, calls) = queue(QUEUE_CAPACITY);
    // The server listens before the first shell starts, so the shell can
    // connect from its first command on. Its calls wait in the queue until
    // the window serves them.
    let mut notice = None;
    let permissions = PermissionTable::new(feed.settings.restricts_ipc());
    // The app's own code may use its socket; the programs of its panes
    // are other processes, and carry the credentials they were given.
    permissions.set_trusts_own_process(true);
    let server = socket.and_then(|path| {
        match Server::bind_with_permissions(
            &path,
            Arc::new(sender),
            ServerLimits::default(),
            permissions,
        ) {
            Ok(server) => {
                // Binding has checked that the directory is ours alone, so
                // nobody else can have put files there for us to remove.
                if let Some(directory) = server.path().parent() {
                    let _ = benterm_ipc::remove_dead_sockets(directory);
                }
                Some(server)
            }
            Err(error) => {
                let path = path.display();
                eprintln!("benterm: no socket API: cannot listen on {path}: {error}");
                notice = Some(format!(
                    "cannot listen on {path}: {error}. The window works, but benterm commands in \
                 its panes fail until the app is restarted."
                ));
                None
            }
        }
    });
    let shells = Rc::new(Shells {
        pty,
        start,
        socket: server.as_ref().map(|server| server.path().to_path_buf()),
        permissions: server.as_ref().map(|server| server.permissions().clone()),
        watchers: Rc::default(),
        agents: Rc::default(),
        login_env: crate::login_env::LoginEnv::default(),
        report_check: std::cell::Cell::new(feed.settings.checks_reports()),
    });
    let spawn = shells.spawn_in(None);
    let window =
        crate::session::open_with_shells(spawn, Some(&shells), feed, session.as_deref(), cx)?;
    window.update(cx, |root, window, cx| {
        root.shells = Some(shells.clone());
        root.follow_agents(&shells.agents, window, cx);
        root.record_agents(&shells.agents, session.as_deref(), window, cx);
    })?;
    if notice.is_some() {
        window.update(cx, |root, _, cx| {
            root.socket_notice = notice;
            cx.notify();
        })?;
    }
    if let Some(server) = server {
        window.update(cx, |root, window, cx| root.serve(calls, shells, window, cx))?;
        // Quitting does not return to `main`, so nothing is dropped on the
        // way out: the socket is closed and removed here. The reader
        // threads never wait for the UI, so joining them does not hang.
        let mut server = Some(server);
        cx.on_app_quit(move |_| {
            drop(server.take());
            async {}
        })
        .detach();
    }
    Ok(window)
}

impl BentermWindow {
    /// Why the app has no socket API, as the bar shows it now.
    pub fn shown_socket_notice(&self) -> Option<String> {
        self.socket_notice.clone()
    }

    /// Carries out the calls of the socket API for as long as the window
    /// is open.
    fn serve(
        &mut self,
        mut calls: ApiCalls,
        shells: Rc<Shells>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.spawn_in(window, async move |this, cx| {
            let mut closed = false;
            loop {
                // An agent or task asked for before the user's shell has
                // given its environment is set aside, along with
                // any call targeting the agent it will create, so hook
                // reports and reads behind it can pass. Once the
                // environment arrives, deferred calls run first in arrival
                // order.
                let call = if !shells.login_env.is_pending()
                    && let Some(call) = calls.take_deferred()
                {
                    call
                } else if shells.login_env.is_pending() && calls.has_deferred() {
                    if closed {
                        shells.login_env.taken().await;
                        continue;
                    }
                    let next = {
                        let env_ready = shells.login_env.taken().fuse();
                        let incoming = calls.next_incoming().fuse();
                        futures::pin_mut!(env_ready, incoming);
                        futures::select_biased! {
                            () = env_ready => None,
                            maybe = incoming => Some(maybe),
                        }
                    };
                    match next {
                        None => continue,
                        Some(Some(call)) => {
                            if !call.expired()
                                && (call.needs_login_env() || calls.depends_on_deferred(&call))
                            {
                                calls.defer(call);
                                continue;
                            }
                            calls.release(&call);
                            call
                        }
                        Some(None) => {
                            closed = true;
                            continue;
                        }
                    }
                } else if closed {
                    break;
                } else if let Some(call) = calls.next_incoming().await {
                    if shells.login_env.is_pending() && !call.expired() && call.needs_login_env() {
                        calls.defer(call);
                        continue;
                    }
                    calls.release(&call);
                    call
                } else {
                    break;
                };
                // One call per update. An update runs to its end
                // before the next one starts, so a call never sees a pane
                // that has its id but is not in the window yet.
                let mut call = Some(call);
                let served = this.update_in(cx, |this, window, cx| {
                    if let Some(call) = call.take() {
                        this.carry_out(call, &shells, window, cx);
                    }
                });
                if served.is_err() {
                    if let Some(call) = call {
                        call.fail(WINDOW_CLOSED);
                    }
                    // Dropping the queue answers the rest, including any
                    // deferred calls.
                    break;
                }
            }
        })
        .detach();
    }

    fn carry_out(
        &mut self,
        call: Call,
        shells: &Rc<Shells>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // The caller has given up on a call whose time has
        // run out, and may send it again, so carrying it out now could
        // type the same text twice. A call taken before its time runs out
        // is carried out to the end.
        if call.expired() {
            call.expire();
            return;
        }
        match call {
            Call::SplitPane(request, reply) => reply.send(self.split_pane(&request, window, cx)),
            Call::OpenBrowser(request, reply) => {
                reply.send(self.open_browser_for(&request, window, cx));
            }
            Call::CreateWorkspace(request, reply) => {
                reply.send(self.create_workspace(request, shells, window, cx));
            }
            Call::SendText(request, reply) => reply.send(self.send_text(&request, cx)),
            Call::ReadOutput(request, reply) => reply.send(self.read_output(&request, cx)),
            Call::ReportHook(request, reply) => reply.send(self.report_hook(request, cx)),
            Call::ReportChecked(request, reply) => reply.send(self.report_checked(request, cx)),
            Call::ReadAgentState(request, reply) => {
                reply.send(self.read_agent_state(&request, cx));
            }
            Call::ReadAgentHistory(request, reply) => {
                reply.ok(self.read_agent_history(&request));
            }
            Call::SubscribeAgentState(reply, events) => {
                // The answer is queued before the sink is
                // kept, and both in this one update, so the connection
                // gets every change from here on, behind the answer.
                reply.ok(());
                shells.watchers.add(events);
            }
            Call::SpawnAgent(request, reply) => {
                reply.send(self.spawn_agent(request, None, shells, window, cx));
            }
            Call::ListAgents(request, reply) => reply.ok(self.list_agents(&request, cx)),
            Call::ShowAgent(request, reply) => reply.send(self.show_agent(&request, window, cx)),
            Call::SendToAgent(request, reply) => reply.send(self.send_to_agent(&request, cx)),
            Call::ReadAgent(request, reply) => reply.send(self.read_agent(&request, cx)),
            Call::SendKeys(request, reply) => reply.send(self.send_keys(&request, cx)),
            Call::KillAgent(request, reply) => {
                reply.send(self.kill_agent(&request, window, cx));
            }
            Call::WaitForAgents(request, reply, connection) => {
                self.wait_for_agents(request, reply, connection, cx);
            }
            Call::ReportToParent(request, reply) => {
                reply.send(self.report_to_parent(request, cx));
            }
            Call::HungUp(connection) => self.hung_up(connection),
            Call::ListPanes(_, reply) => reply.ok(self.list_panes(cx)),
            Call::CreateTask(request, reply) => {
                self.create_task_for(request, reply, shells, window, cx);
            }
            Call::ListTasks(_, reply) => reply.send(self.list_tasks()),
            Call::CloseTask(request, reply) => self.close_task_for(request, reply, window, cx),
        }
    }

    /// `pane.split`. The keyboard stays where it is, so a program that
    /// arranges panes does not take it from the user.
    fn split_pane(
        &mut self,
        request: &SplitPane,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<PaneCreated, ApiError> {
        let (grid, id) = self.find_pane(request.pane, cx)?;
        let direction = match request.direction {
            benterm_ipc::SplitDirection::Right => SplitDirection::Right,
            benterm_ipc::SplitDirection::Down => SplitDirection::Down,
        };
        let shown = pane_area(window, cx).unwrap_or_default();
        let new = grid
            .update(cx, |grid, cx| {
                grid.split_pane(id, direction, shown, window, cx)
            })
            .map_err(|error| match error {
                SplitError::Layout(error) => failed(format!("cannot split pane {id}: {error}")),
                SplitError::Spawn(error) => could_not_start(&error),
            })?;
        Ok(PaneCreated { pane: api_id(new) })
    }

    /// `browser.open`. The keyboard stays where it is, as for
    /// `pane.split`.
    fn open_browser_for(
        &mut self,
        request: &OpenBrowser,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<PaneCreated, ApiError> {
        let beside = match request.pane {
            Some(pane) => Some(self.find_pane(pane, cx)?.1),
            None => None,
        };
        let direction = match request.direction {
            benterm_ipc::SplitDirection::Right => SplitDirection::Right,
            benterm_ipc::SplitDirection::Down => SplitDirection::Down,
        };
        let opened = self.open_browser(Some(&request.url), beside, direction, false, window, cx);
        let new = opened.map_err(|error| match error {
            BrowserError::NotAPage => ApiError::new(
                ErrorCode::InvalidParams,
                format!("cannot open {:?}: {error}", request.url),
            ),
            BrowserError::UnknownPane => match request.pane {
                Some(pane) => unknown_pane(pane),
                None => failed(format!("cannot open a browser: {error}")),
            },
            BrowserError::Layout(error) => failed(format!("cannot open a browser: {error}")),
        })?;
        Ok(PaneCreated { pane: api_id(new) })
    }

    /// `workspace.create`. The workspace is added at the bottom, and the
    /// selection stays where it is.
    fn create_workspace(
        &mut self,
        request: CreateWorkspace,
        shells: &Rc<Shells>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<WorkspaceCreated, ApiError> {
        // A shell the window has thrown away, with a session it could not
        // restore, may have asked before it went. Its call is not for
        // this window. Pane ids are never given twice, so the pane is
        // gone for good. The caller may be in a pane or in a terminal
        // behind the window.
        if let Some(caller) = request.caller
            && self.locate(caller.get(), cx).is_none()
            && self.agents.terminal_behind(caller.get()).is_none()
        {
            return Err(ApiError::new(
                ErrorCode::UnknownPane,
                format!("there is no pane {caller}, which the request came from"),
            ));
        }
        // The caller's working directory is not the app's, so a relative
        // path would name a different directory here.
        let cwd = request.cwd.map(PathBuf::from);
        if let Some(cwd) = &cwd
            && !(cwd.is_absolute() && cwd.is_dir())
        {
            return Err(ApiError::new(
                ErrorCode::InvalidParams,
                format!(
                    "cwd must be an absolute path to a directory: {}",
                    cwd.display()
                ),
            ));
        }
        let spawn = shells.spawn_in(cwd);
        // The workspace is not shown, so no frame will fit its terminal:
        // it starts at the size it would have on view.
        let theme = &self.settings.theme().terminal;
        let size = crate::pane_area::terminal_size(window, theme, cx);
        let first =
            FirstPane::start(&spawn, theme, None, size).map_err(|error| could_not_start(&error))?;
        let pane = first.id();
        let workspace = self.add_workspace(request.name.as_deref(), first, spawn, window, cx);
        Ok(WorkspaceCreated {
            workspace: benterm_ipc::WorkspaceId::new(workspace.get()),
            pane: api_id(pane),
        })
    }

    /// `pane.send_text`. The text is queued for the program; whether the
    /// program reads it is its own business.
    fn send_text(&self, request: &SendText, cx: &App) -> Result<(), ApiError> {
        let (id, view) = self.find_terminal(request.pane, cx)?;
        match view.read(cx).try_send_text(&request.text) {
            Ok(()) => Ok(()),
            Err(WriteError::Full) => Err(input_queue_full(format!("pane {id}"))),
            Err(WriteError::Closed) => Err(failed(format!("the program of pane {id} has exited"))),
        }
    }

    /// The grid a pane is in, whichever workspace and tab it is in, and
    /// the pane's id in the grid's tree. A terminal behind the window has
    /// no grid and is not found here.
    fn find_pane(
        &self,
        pane: benterm_ipc::PaneId,
        cx: &App,
    ) -> Result<(Entity<PaneGrid>, PaneId), ApiError> {
        self.locate(pane.get(), cx)
            .map(|place| (place.grid, place.pane))
            .ok_or_else(|| unknown_pane(pane))
    }

    /// Where a pane is in the window.
    pub(crate) fn locate(&self, pane: u64, cx: &App) -> Option<PanePlace> {
        for workspace in self.workspaces.iter() {
            let group = &workspace.content().tabs;
            let tabs = group.read(cx);
            for tab in tabs.tabs() {
                let Some(grid) = tabs.grid(tab) else {
                    continue;
                };
                let found = grid
                    .read(cx)
                    .panes()
                    .into_iter()
                    .find(|id| id.get() == pane);
                if let Some(id) = found {
                    return Some(PanePlace {
                        workspace: workspace.id(),
                        tabs: group.clone(),
                        tab,
                        grid: grid.clone(),
                        pane: id,
                    });
                }
            }
        }
        None
    }

    /// The terminal with the id of a pane: the one of the pane, in
    /// whichever workspace and tab it is, or one behind the window.
    pub(crate) fn find_terminal(
        &self,
        pane: benterm_ipc::PaneId,
        cx: &App,
    ) -> Result<(PaneId, Entity<TerminalView>), ApiError> {
        if let Some(place) = self.locate(pane.get(), cx) {
            let view = place.grid.read(cx).terminal(place.pane).cloned();
            return Ok((place.pane, view.ok_or_else(pane_without_terminal)?));
        }
        self.agents
            .terminal_behind(pane.get())
            .ok_or_else(|| unknown_pane(pane))
    }

    /// `pane.report_hook`. An event of an agent the app does not know, or
    /// one the pane's detector has no use for, is taken and changes
    /// nothing.
    fn report_hook(&self, request: ReportHook, cx: &mut App) -> Result<(), ApiError> {
        let (_, view) = self.find_terminal(request.pane, cx)?;
        let plan = PlanUpdate::from_hook(
            &request.agent,
            &request.event,
            request.tool_name.as_deref(),
            request.tool_input.as_ref(),
            request.tool_response.as_ref(),
        );
        let event = HookEvent {
            agent: request.agent,
            name: request.event,
            tool: request.tool_name,
            notification_type: request.notification_type,
            session: request.session_id,
            subagent: request.agent_id.is_some(),
            plan,
        };
        view.update(cx, |view, cx| view.report_hook(&event, cx));
        Ok(())
    }

    fn read_agent_state(
        &self,
        request: &ReadAgentState,
        cx: &App,
    ) -> Result<PaneAgentState, ApiError> {
        let (_, view) = self.find_terminal(request.pane, cx)?;
        Ok(agent_state(request.pane, view.read(cx).agent_status()))
    }

    /// `pane.list`: the panes of every workspace from the top, tab by tab
    /// from the left, then the terminals behind the window.
    fn list_panes(&self, cx: &App) -> PaneList {
        let mut panes = Vec::new();
        for workspace in self.workspaces.iter() {
            let workspace_id = benterm_ipc::WorkspaceId::new(workspace.id().get());
            let tabs = workspace.content().tabs.read(cx);
            for (place, tab) in (1..).zip(tabs.tabs()) {
                let Some(grid) = tabs.grid(tab) else {
                    continue;
                };
                let grid = grid.read(cx);
                for id in grid.panes() {
                    if let Some(view) = grid.terminal(id) {
                        let mut info = self.pane_info(id, view, cx);
                        info.workspace = Some(workspace_id);
                        info.tab = Some(place);
                        panes.push(info);
                    }
                }
            }
        }
        for (id, view) in self.agents.terminals_behind() {
            panes.push(self.pane_info(id, &view, cx));
        }
        PaneList { panes }
    }

    /// What `pane.list` says of a terminal, without where it is.
    fn pane_info(&self, id: PaneId, view: &Entity<TerminalView>, cx: &App) -> PaneInfo {
        let view = view.read(cx);
        let state = agent_state(api_id(id), view.agent_status());
        PaneInfo {
            pane: api_id(id),
            workspace: None,
            tab: None,
            name: self.agents.name_of(id.get()).map(str::to_owned),
            cwd: view
                .working_directory()
                .and_then(|cwd| cwd.into_os_string().into_string().ok()),
            agent: state.agent,
            state: state.state,
            reason: state.reason,
        }
    }

    fn read_output(&self, request: &ReadOutput, cx: &App) -> Result<PaneOutput, ApiError> {
        let (_, view) = self.find_terminal(request.pane, cx)?;
        let lines = request.lines.map(|lines| lines as usize);
        Ok(PaneOutput {
            text: view.read(cx).text(lines),
        })
    }
}

/// Where a pane is: its workspace, the tabs of that workspace, its tab,
/// the grid of the tab, and its id in the grid's tree.
pub(crate) struct PanePlace {
    pub(crate) workspace: benterm_layout::WorkspaceId,
    pub(crate) tabs: Entity<crate::TabGroup>,
    pub(crate) tab: benterm_layout::TabId,
    pub(crate) grid: Entity<PaneGrid>,
    pub(crate) pane: PaneId,
}

fn unknown_pane(pane: benterm_ipc::PaneId) -> ApiError {
    ApiError::new(ErrorCode::UnknownPane, format!("there is no pane {pane}"))
}

/// A pane's agent status as the socket API gives it.
pub(crate) fn agent_state(pane: benterm_ipc::PaneId, status: AgentStatus) -> PaneAgentState {
    let (state, reason) = match status.state {
        AgentState::Working => (AgentActivity::Working, None),
        // The socket API tells a folder to trust apart from a tool no
        // further.
        AgentState::Waiting(WaitReason::Permission | WaitReason::Trust) => {
            (AgentActivity::Waiting, Some(WaitingFor::Permission))
        }
        // The socket API tells questions apart no further.
        AgentState::Waiting(WaitReason::Question | WaitReason::Choice) => {
            (AgentActivity::Waiting, Some(WaitingFor::Question))
        }
        AgentState::Done => (AgentActivity::Done, None),
        AgentState::Error => (AgentActivity::Error, None),
        AgentState::Unknown => (AgentActivity::Unknown, None),
    };
    PaneAgentState {
        pane,
        agent: status.agent.map(str::to_owned),
        state,
        reason,
    }
}

pub(crate) fn api_id(pane: PaneId) -> benterm_ipc::PaneId {
    benterm_ipc::PaneId::new(pane.get())
}

pub(crate) fn failed(message: String) -> ApiError {
    ApiError::new(ErrorCode::Failed, message)
}

pub(crate) fn input_queue_full(target: impl fmt::Display) -> ApiError {
    ApiError::new(
        ErrorCode::InputQueueFull,
        format!(
            "the input queue of {target} is full because its program is not reading input; \
             nothing was sent"
        ),
    )
}

fn could_not_start(error: &io::Error) -> ApiError {
    failed(format!(
        "could not start a terminal for the new pane: {error}"
    ))
}

/// The pane shows a browser, which nothing is typed into or read from.
fn pane_without_terminal() -> ApiError {
    ApiError::new(ErrorCode::Failed, "the pane has no terminal")
}

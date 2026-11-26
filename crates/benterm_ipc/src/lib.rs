//! Benterm's socket protocol, server, client, and CLI. The UI implements [`App`]
//! to handle requests. The IPC crate has no GPUI dependency.

mod auth;
mod process;
mod protocol;

pub use auth::{Grant, PermissionScope, PermissionTable};
pub use protocol::{
    AgentActivity, AgentHistory, AgentInfo, AgentList, AgentRecord, AgentShown, AgentSpawned,
    AgentStateChanged, AgentTarget, AgentsWaited, ApiError, CloseTask, CreateTask, CreateWorkspace,
    DEFAULT_MAX_PEER_ROUNDS, ErrorCode, Event, InvalidServerMessage, KillAgent, ListAgents,
    ListPanes, ListTasks, Method, OpenBrowser, PaneAgentState, PaneClosed, PaneCreated, PaneId,
    PaneInfo, PaneList, PaneOutput, ReadAgent, ReadAgentHistory, ReadAgentState, ReadOutput,
    ReportAccepted, ReportChecked, ReportHook, ReportStatus, ReportToParent, Request, RequestError,
    RequestFrame, SendKeys, SendText, SendToAgent, ServerMessage, ShowAgent, SpawnAgent,
    SplitDirection, SplitPane, SubscribeAgentState, TaskClosed, TaskClosure, TaskCreated, TaskInfo,
    TaskList, UnknownReportStatus, WaitForAgents, WaitedAgent, WaitingFor, WorkspaceCreated,
    WorkspaceId, WorktreeLeft,
};
mod server;
mod socket;

pub use server::{App, ConnectionId, EVENT_BACKLOG, EventSink, Reply, Server, ServerLimits};
pub use socket::{
    AGENT_NAME_ENV, CONTROL_TOKEN_ENV, MAILBOX_DIR_ENV, PANE_ID_ENV, PANE_TOKEN_ENV,
    PARENT_PANE_ID_ENV, SOCKET_ENV, default_socket_path, remove_dead_sockets, socket_path,
    state_dir,
};
mod report_check;

pub use report_check::REPORT_CHECK_ENV;
mod client;

pub use client::{Client, ClientError};
pub mod cli;
pub mod setup;

//! Tasks in the window: a piece of work with a branch and a git worktree
//! of its own, shown in a workspace of its own.
//!
//! The tasks are kept by `benterm_task`, in the tasks file beside the
//! session file. The window knows which workspace shows which task: a
//! workspace is made for a task when the task is made, and a session
//! that is made again gives each task's workspace back to it.
//!
//! git runs on a background thread. The window takes the result in an
//! update of its own, so a task and its workspace come and go together
//! as far as the other requests can see.
//!
//! A task is closed with its workspace. Closing the workspace by hand
//! asks first whether the worktree and the branch are kept or removed,
//! and says what removing would lose. A workspace whose last pane ends by
//! itself closes its task and keeps both.
//!
//! [`BentermWindow::tasks`] is what the other views read: the diff takes a
//! task's `base_commit` and `worktree`, the sidebar its `repository`, and
//! the board the whole list.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use benterm_config::TaskAgent;
use benterm_ipc::{
    ApiError, CloseTask, CreateTask, ErrorCode, SpawnAgent, TaskClosed, TaskClosure, TaskCreated,
    TaskInfo, TaskList, WorktreeLeft,
};
use benterm_layout::{PaneId, WorkspaceId};
use benterm_task::{
    AtRisk, CloseError, Closing, Git, IssueTasks, LocalIssueChange, LocalIssueState,
    NewTask as TaskToMake, Task, TaskStore, change_local_issues,
};
use gpui::{App, AppContext, Context, PromptButton, PromptLevel, Window, actions};

use crate::agent_watch::AgentStatus;
use crate::ipc::{Shells, api_id, failed};
use crate::kanban::{BoardTask, Sent};
use crate::task_form::OpenTaskForm;
use crate::{BentermWindow, FirstPane, WorkspaceTabs};

actions!(
    task,
    [
        /// Opens the form for a new task: a branch and a git worktree of
        /// its own, and a workspace in it.
        NewTask,
    ]
);

/// The buttons of the question a closing task asks.
const KEEP: &str = "Keep Worktree";
const REMOVE: &str = "Remove Worktree and Branch";
const CANCEL: &str = "Cancel";
/// The answers to whether the local issue of a task is closed with it.
const CLOSE_ISSUE: &str = "Close Issue";
const LEAVE_ISSUE: &str = "Leave Issue Open";
/// What the counts leave out.
const IGNORED: &str = "Files that git ignores are not counted and go with the worktree.";

fn millis(at: std::time::SystemTime) -> u64 {
    at.duration_since(std::time::SystemTime::UNIX_EPOCH)
        .map_or(0, |since| since.as_millis() as u64)
}

fn window_closed() -> benterm_task::TaskError {
    benterm_task::TaskError::new("the window has closed")
}

/// A task and the workspace that shows it now, if one does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskRow {
    pub task: Task,
    pub workspace: Option<WorkspaceId>,
}

/// The summarised state of the agents running in an issue's open task
/// workspace(s).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum IssueAgentState {
    /// No open task has an agent with a known state.
    #[default]
    None,
    Working,
    Waiting,
    Done,
    /// An agent stopped with an error.
    Error,
}

impl IssueAgentState {
    fn from_status(status: AgentStatus) -> Self {
        match status.state {
            benterm_agent::AgentState::Waiting(_) => Self::Waiting,
            benterm_agent::AgentState::Error => Self::Error,
            benterm_agent::AgentState::Working => Self::Working,
            benterm_agent::AgentState::Done => Self::Done,
            benterm_agent::AgentState::Unknown => Self::None,
        }
    }

    fn priority(self) -> u8 {
        match self {
            Self::None => 0,
            Self::Done => 1,
            Self::Working => 2,
            Self::Error => 3,
            Self::Waiting => 4,
        }
    }
}

/// Summarises the agent statuses observed in an issue's task workspaces.
pub fn issue_agent_state_from_statuses(
    statuses: impl IntoIterator<Item = AgentStatus>,
) -> IssueAgentState {
    statuses
        .into_iter()
        .map(IssueAgentState::from_status)
        .max_by_key(|state| state.priority())
        .unwrap_or(IssueAgentState::None)
}

/// Looks up the summarised agent state for `issue` across open tasks whose
/// repository satisfies `same_repository`.
pub fn issue_agent_state_for_tasks(
    tasks: &[Task],
    issue: u64,
    same_repository: impl FnMut(&Path) -> bool,
    statuses_of_task: impl FnMut(&Task) -> Vec<AgentStatus>,
) -> IssueAgentState {
    let matched = benterm_task::tasks_for_issue(tasks, issue, same_repository);
    let statuses = matched.open.iter().flat_map(statuses_of_task);
    issue_agent_state_from_statuses(statuses)
}

/// Looks up the summarised agent state for every issue present in `tasks`
/// whose repository satisfies `same_repository`.
pub fn issue_agent_states_for_tasks(
    tasks: &[Task],
    same_repository: impl FnMut(&Path) -> bool,
    mut statuses_of_task: impl FnMut(&Task) -> Vec<AgentStatus>,
) -> BTreeMap<u64, IssueAgentState> {
    benterm_task::tasks_by_issue(tasks, same_repository)
        .into_iter()
        .map(|(issue, matched)| {
            let statuses = matched.open.iter().flat_map(&mut statuses_of_task);
            (issue, issue_agent_state_from_statuses(statuses))
        })
        .collect()
}

/// The tasks of a window.
pub(crate) struct Tasks {
    store: Arc<TaskStore>,
    /// The tasks as the file had them when the window last read it.
    list: Vec<Task>,
    /// Whether the file could be read then.
    readable: bool,
    /// Why the tasks could not be read, or a task not closed.
    pub(crate) notice: Option<String>,
    pub(crate) form: Option<OpenTaskForm>,
}

impl Default for Tasks {
    /// Tasks that are kept only as long as the window.
    fn default() -> Self {
        Self::new(TaskStore::open(None))
    }
}

impl Tasks {
    pub(crate) fn new(store: TaskStore) -> Self {
        let mut tasks = Self {
            store: Arc::new(store),
            list: Vec::new(),
            readable: false,
            notice: None,
            form: None,
        };
        tasks.read();
        tasks
    }

    /// Reads the tasks again.
    fn read(&mut self) {
        match self.store.tasks() {
            Ok(list) => {
                self.list = list;
                self.readable = true;
            }
            Err(error) => {
                self.readable = false;
                self.notice = Some(format!("{error}. Tasks cannot be made or closed."));
            }
        }
    }

    /// Whether a workspace of a saved session is still the task
    /// `id`'s. While the tasks cannot be read nobody can tell, and the
    /// session goes on saying so.
    pub(crate) fn keeps_link(&self, id: u64) -> bool {
        !self.readable || self.is_open(id)
    }

    /// The saved session without the workspaces of tasks that are
    /// closed, or no session if none is left, and whether a workspace
    /// was left out.
    pub(crate) fn without_closed(
        &self,
        mut session: benterm_session::Session,
    ) -> (Option<benterm_session::Session>, bool) {
        let closed = |task: Option<u64>| {
            let known = task.and_then(|id| self.list.iter().find(|task| task.id == id));
            self.readable && known.is_some_and(|task| task.closed.is_some())
        };
        let before = session.workspaces.len();
        let selected = session.selected_workspace;
        let gone_before = session
            .workspaces
            .iter()
            .take(selected)
            .filter(|workspace| closed(workspace.task))
            .count();
        session
            .workspaces
            .retain(|workspace| !closed(workspace.task));
        let left = session.workspaces.len();
        if left == 0 {
            return (None, true);
        }
        session.selected_workspace = (selected - gone_before).min(left - 1);
        (Some(session), left != before)
    }

    /// Whether `id` is a task that is not closed.
    pub(crate) fn is_open(&self, id: u64) -> bool {
        self.list
            .iter()
            .any(|task| task.id == id && task.closed.is_none())
    }
}

/// What a task is made from, as the form and the socket give it.
pub(crate) struct TaskRequest {
    /// A directory of the checkout to branch off from.
    pub(crate) directory: PathBuf,
    pub(crate) name: String,
    pub(crate) branch: Option<String>,
    pub(crate) role: Option<String>,
    pub(crate) agent: Option<TaskAgent>,
    /// What the agent is to do; may be empty.
    pub(crate) work: String,
    /// Whether the task's workspace is put on view.
    pub(crate) select: bool,
}

struct TaskAgentLaunch {
    name: String,
    command: Vec<String>,
    env: Vec<(String, String)>,
}

/// A task that was made, with its workspace.
pub(crate) struct TaskStarted {
    pub(crate) task: Task,
    pub(crate) workspace: WorkspaceId,
    pub(crate) pane: PaneId,
}

/// Called when a task has been closed or could not be, on the window's
/// thread but maybe after the window: closing its last workspace closes
/// it. Gives what the bar is to say, if the window is still there.
type Closed = Box<dyn FnOnce(Result<Closing, CloseError>) -> Option<String>>;

/// Called in an update of the window when what was asked for is done.
type Done<T, E> =
    Box<dyn FnOnce(&mut BentermWindow, Result<T, E>, &mut Window, &mut Context<BentermWindow>)>;

fn info(task: &Task, workspace: Option<WorkspaceId>) -> TaskInfo {
    let text = |path: &Path| path.to_string_lossy().into_owned();
    TaskInfo {
        id: task.id,
        name: task.name.clone(),
        repository: text(&task.repository),
        base_branch: task.base_branch.clone(),
        base_commit: task.base_commit.clone(),
        branch: task.branch.clone(),
        worktree: text(&task.worktree),
        role: task.role.clone(),
        created_at_ms: task.created_at_ms,
        closed: task.closed.as_ref().map(|closed| TaskClosure {
            at_ms: closed.at_ms,
            worktree: match closed.worktree {
                benterm_task::Worktree::Kept => WorktreeLeft::Kept,
                benterm_task::Worktree::Removed => WorktreeLeft::Removed,
            },
        }),
        workspace: workspace.map(|id| benterm_ipc::WorkspaceId::new(id.get())),
    }
}

impl BentermWindow {
    /// Every task, closed ones too, oldest first, each with the
    /// workspace that shows it now.
    pub fn tasks(&self) -> Vec<TaskRow> {
        let rows = self.tasks.list.iter().map(|task| TaskRow {
            task: task.clone(),
            workspace: self.workspace_of_task(task.id),
        });
        rows.collect()
    }

    pub(crate) fn board_repository(&self, cx: &App) -> Option<PathBuf> {
        if let Some(task) = self.task_of_workspace(self.selected_workspace()) {
            return Some(task.repository.clone());
        }
        let directory = self.focused_terminal(cx)?.read(cx).working_directory()?;
        // The nearest checkout takes precedence, including nested repositories.
        let checkout = directory
            .ancestors()
            .find(|path| path.join(".git").exists())?;
        Some(
            self.tasks
                .list
                .iter()
                .find(|task| task.worktree == checkout)
                .map_or_else(|| checkout.to_owned(), |task| task.repository.clone()),
        )
    }

    pub(crate) fn board_task_store(&self) -> Arc<TaskStore> {
        self.tasks.store.clone()
    }

    pub(crate) fn board_branches(&self, repository: &Path) -> Vec<(u64, String, String)> {
        self.tasks
            .list
            .iter()
            .filter(|task| task.repository == repository && task.closed.is_none())
            .map(|task| (task.id, task.branch.clone(), task.base_branch.clone()))
            .collect()
    }

    /// The tasks as the kanban of the board takes them: each with the
    /// keys in the agents' history of its workspace's panes, and the
    /// comments that went to them.
    pub(crate) fn board_tasks(&self, repository: Option<&Path>, cx: &App) -> Vec<BoardTask> {
        let Some(repository) = repository else {
            return Vec::new();
        };
        let sends = self.review_sends();
        self.tasks
            .list
            .iter()
            .filter(|task| task.repository == repository)
            .map(|task| {
                let panes: Vec<PaneId> = self
                    .workspace_of_task(task.id)
                    .map(|workspace| self.workspace_panes(workspace, cx))
                    .unwrap_or_default();
                let key = |pane: PaneId| self.agent_history.key(pane).map(str::to_owned);
                let sent = sends.iter().filter(|sent| sent.task == task.id);
                let sent = sent.filter_map(|sent| {
                    Some(Sent {
                        pane: key(sent.pane)?,
                        at_ms: millis(sent.at),
                        comments: sent.comments,
                    })
                });
                BoardTask {
                    id: task.id,
                    name: task.name.clone(),
                    repository: task
                        .repository
                        .file_name()
                        .map_or_else(String::new, |name| name.to_string_lossy().into_owned()),
                    closed: task.closed.as_ref().map(|closed| closed.worktree),
                    panes: panes.into_iter().filter_map(key).collect(),
                    sends: sent.collect(),
                }
            })
            .collect()
    }

    /// The panes of the workspace `id`, in the order of its tabs.
    fn workspace_panes(&self, id: WorkspaceId, cx: &App) -> Vec<PaneId> {
        let Some(workspace) = self.workspaces.get(id) else {
            return Vec::new();
        };
        let tabs = workspace.content().tabs.read(cx);
        tabs.tabs()
            .iter()
            .filter_map(|tab| tabs.grid(*tab))
            .flat_map(|grid| grid.read(cx).panes())
            .collect()
    }

    pub(crate) fn workspace_agent_statuses(&self, id: WorkspaceId, cx: &App) -> Vec<AgentStatus> {
        let Some(workspace) = self.workspaces.get(id) else {
            return Vec::new();
        };
        let tabs = workspace.content().tabs.read(cx);
        tabs.tabs()
            .iter()
            .filter_map(|tab| tabs.grid(*tab))
            .flat_map(|grid| {
                let grid = grid.read(cx);
                grid.panes()
                    .into_iter()
                    .filter_map(|pane| grid.terminal(pane).map(|view| view.read(cx).agent_status()))
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    /// The open and closed tasks matching `issue` in the same repository.
    pub fn issue_tasks(
        &self,
        issue: u64,
        same_repository: impl FnMut(&Path) -> bool,
    ) -> IssueTasks {
        benterm_task::tasks_for_issue(&self.tasks.list, issue, same_repository)
    }

    /// The summarised agent state of the open task workspaces for `issue`.
    pub fn issue_agent_state(
        &self,
        issue: u64,
        same_repository: impl FnMut(&Path) -> bool,
        cx: &App,
    ) -> IssueAgentState {
        issue_agent_state_for_tasks(&self.tasks.list, issue, same_repository, |task| {
            self.workspace_of_task(task.id)
                .map(|workspace| self.workspace_agent_statuses(workspace, cx))
                .unwrap_or_default()
        })
    }

    pub(crate) fn open_task_repositories(&self) -> std::collections::BTreeSet<PathBuf> {
        self.tasks
            .list
            .iter()
            .filter(|task| task.closed.is_none())
            .map(|task| task.repository.clone())
            .collect()
    }

    /// The summarised agent state for every issue with tasks in the same
    /// repository.
    pub fn issue_agent_states(
        &self,
        same_repository: impl FnMut(&Path) -> bool,
        cx: &App,
    ) -> BTreeMap<u64, IssueAgentState> {
        issue_agent_states_for_tasks(&self.tasks.list, same_repository, |task| {
            self.workspace_of_task(task.id)
                .map(|workspace| self.workspace_agent_statuses(workspace, cx))
                .unwrap_or_default()
        })
    }

    /// The task the workspace `id` shows, if it shows one.
    pub fn task_of_workspace(&self, id: WorkspaceId) -> Option<&Task> {
        let task = self.workspaces.get(id)?.content().task.get()?;
        self.tasks.list.iter().find(|known| known.id == task)
    }

    pub(crate) fn workspace_of_task(&self, task: u64) -> Option<WorkspaceId> {
        self.workspaces
            .iter()
            .find(|workspace| workspace.content().task.get() == Some(task))
            .map(|workspace| workspace.id())
    }

    pub(crate) fn shown_task_notice(&self) -> Option<String> {
        self.tasks.notice.clone()
    }

    pub(crate) fn task_failed(&mut self, message: String, cx: &mut Context<Self>) {
        self.tasks.notice = Some(message);
        self.dismissed = false;
        cx.notify();
    }

    /// git as the panes find it.
    pub(crate) fn git(&self, shells: Option<&Rc<Shells>>) -> Git {
        Git {
            path: shells.and_then(|shells| crate::roles::programs_path(shells)),
            ..Git::default()
        }
    }

    /// Makes a task: its branch and worktree on a background thread, and
    /// then its workspace, with the role's agent or a shell in the
    /// worktree. `done` is told the task, or why nothing was made.
    pub(crate) fn create_task(
        &mut self,
        request: TaskRequest,
        shells: &Rc<Shells>,
        window: &mut Window,
        cx: &mut Context<Self>,
        done: Done<TaskStarted, String>,
    ) {
        let prepared = (|| {
            let launch = match &request.role {
                Some(name) => {
                    let roles = self.settings.roles();
                    let role = roles.iter().find(|role| &role.name == name);
                    let role = role.ok_or(format!("there is no role {name:?}"))?;
                    Some(TaskAgentLaunch {
                        name: role.name.clone(),
                        command: role.command(&request.work),
                        env: role.env.clone(),
                    })
                }
                None => request.agent.as_ref().map(|agent| {
                    let name = agent.agent.program().to_owned();
                    let mut command = agent.agent.launch();
                    if let Some(model) = &agent.model {
                        command.extend(["--model".to_owned(), model.clone()]);
                    }
                    if !request.work.is_empty() {
                        command.extend(["--".to_owned(), request.work.clone()]);
                    }
                    TaskAgentLaunch {
                        name,
                        command,
                        env: Vec::new(),
                    }
                }),
            };
            if !request.directory.is_absolute() {
                return Err(format!(
                    "the repository must be an absolute path: {}",
                    request.directory.display()
                ));
            }
            let home = std::env::var_os("HOME").map(PathBuf::from);
            let root = self.settings.worktree_root(home.as_deref()).ok_or(
                "there is no directory for worktrees: worktrees.directory is below the home \
                 directory, and HOME is not set",
            )?;
            Ok((launch, root))
        })();
        let (launch, root) = match prepared {
            Ok(prepared) => prepared,
            Err(error) => return done(self, Err(error), window, cx),
        };
        let git = self.git(Some(shells));
        let store = self.tasks.store.clone();
        let make = TaskToMake {
            directory: request.directory,
            name: request.name,
            branch: request.branch,
            role: request.role,
            root,
        };
        let made = cx.background_spawn({
            let git = git.clone();
            async move { benterm_task::create(&git, &store, make) }
        });
        let shells = shells.clone();
        let select = request.select;
        cx.spawn_in(window, async move |this, cx| {
            let made = made.await;
            // A window that has closed leaves the task without a
            // workspace; its worktree is there to be used.
            let _ = this.update_in(cx, |this, window, cx| {
                let started = made.map_err(|error| error.to_string()).and_then(|task| {
                    this.start_task(task, launch, select, &shells, &git, window, cx)
                });
                done(this, started, window, cx);
            });
        })
        .detach();
    }

    /// Opens the workspace of a task that was just made. If its first
    /// terminal cannot start, the task is taken back.
    #[allow(clippy::too_many_arguments)]
    fn start_task(
        &mut self,
        task: Task,
        launch: Option<TaskAgentLaunch>,
        select: bool,
        shells: &Rc<Shells>,
        git: &Git,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<TaskStarted, String> {
        let worktree = task.worktree.clone();
        let theme = self.settings.theme().clone();
        // Not on view yet, so no frame will fit its terminal.
        let size = crate::pane_area::terminal_size(window, &theme.terminal, cx);
        // Later panes of the workspace are shells in the worktree.
        let shell = shells.spawn_in(Some(worktree.clone()));
        let agent = launch.as_ref().map(|launch| {
            let taken: Vec<String> = self.agents(cx).into_iter().map(|row| row.name).collect();
            crate::roles::free_name(&launch.name, &taken)
        });
        let first = (|| match (launch, &agent) {
            (Some(TaskAgentLaunch { command, env, .. }), Some(agent)) => {
                let path = git.path.clone();
                let mut command = command;
                benterm_config::fit_codex_launch(&mut command, path.as_deref());
                let request = SpawnAgent {
                    name: agent.clone(),
                    command: crate::roles::launch_command(
                        command,
                        &env,
                        path.as_deref(),
                        &crate::roles::login_shell(),
                    ),
                    cwd: None,
                    env: path
                        .and_then(|path| path.into_string().ok())
                        .map(|path| ("PATH".to_owned(), path))
                        .into_iter()
                        .chain(env)
                        .collect(),
                    parent: None,
                    outlive_parent: false,
                    // The first prompt has the line for worker.md already.
                    first_message_line: None,
                    shell_after: true,
                    ..SpawnAgent::default()
                };
                let program =
                    crate::agents::program(&request, &shells.pty, shells.login_env.path())
                        .map_err(|error| error.message)?;
                let spawn = shells.spawn_program(program, Some(worktree.clone()));
                FirstPane::start(&spawn, &theme.terminal, None, size)
                    .map_err(|error| format!("the agent could not be started: {error}"))
            }
            _ => FirstPane::start(&shell, &theme.terminal, None, size)
                .map_err(|error| format!("a terminal could not be started: {error}")),
        })();
        let first = match first {
            Ok(first) => first,
            Err(error) => {
                // Nothing has worked in the worktree yet.
                let undone = benterm_task::undo(git, &self.tasks.store, &task);
                self.tasks.read();
                return Err(match undone {
                    Ok(()) => error,
                    Err(left) => format!(
                        "{error}; the task {} is left with its worktree at {}: {left}",
                        task.id,
                        worktree.display()
                    ),
                });
            }
        };
        let pane = first.id();
        let tabs = WorkspaceTabs {
            task: std::cell::Cell::new(Some(task.id)),
            ..Self::workspace_tabs_with(first, shell, theme, window, cx)
        };
        let workspace = self.workspaces.add(Some(&task.name), tabs);
        if let Some(agent) = agent
            && let Some(place) = self.locate(pane.get(), cx)
            && let Some(view) = place.grid.read(cx).terminal(place.pane).cloned()
        {
            // Its program was started to give way to a shell.
            let shell = crate::roles::shell_argv(&shells.pty);
            view.update(cx, |view, _| {
                if let Some(role) = &task.role {
                    view.restricted = Some(benterm_session::RestrictedPane {
                        role: Some(role.clone()),
                    });
                }
                view.give_way_to(shell);
            });
            self.adopt_agent(agent, pane, &view, window, cx);
        }
        self.tasks.read();
        self.sync_board(cx);
        if select {
            self.select_workspace(workspace, window, cx);
        }
        cx.notify();
        Ok(TaskStarted {
            task,
            workspace,
            pane,
        })
    }

    /// Closes the task `id` and its workspace. With `remove`, which says
    /// what may be lost with them, its worktree and branch are removed
    /// on a background thread.
    ///
    /// What would be lost is counted first, and nothing is closed when
    /// it is more than `remove` says. Then the workspace is closed, which
    /// ends the programs that write in the worktree, and only then the
    /// worktree is removed, after counting once more. The last workspace
    /// is closed after the worktree is removed instead: the window goes
    /// with it, and nothing would be left to say how it went.
    pub(crate) fn close_task(
        &mut self,
        id: u64,
        remove: Option<AtRisk>,
        window: &mut Window,
        cx: &mut Context<Self>,
        done: Closed,
    ) {
        let git = self.git(self.shells.as_ref());
        let store = self.tasks.store.clone();
        let Some(accepted) = remove else {
            // Kept: the task closes with its workspace, or alone.
            let closed = match self.workspace_of_task(id) {
                Some(workspace) if self.tasks.is_open(id) => {
                    self.close_workspace(workspace, window, cx);
                    self.tasks.read();
                    match self.tasks.list.iter().find(|task| task.id == id) {
                        Some(task) if task.closed.is_some() => Ok(Closing {
                            task: task.clone(),
                            branch_left: None,
                        }),
                        _ => benterm_task::close(&git, &store, id, None),
                    }
                }
                _ => benterm_task::close(&git, &store, id, None),
            };
            self.tasks.read();
            self.sync_board(cx);
            let said = done(closed);
            self.tasks.notice = said.or(self.tasks.notice.take());
            return cx.notify();
        };
        let checked = cx.background_spawn({
            let (git, store) = (git.clone(), store.clone());
            async move { benterm_task::check_removal(&git, &store, id, accepted) }
        });
        cx.spawn_in(window, async move |this, cx| {
            let said = 'closed: {
                if let Err(error) = checked.await {
                    break 'closed done(Err(error));
                }
                let remove = move || benterm_task::close(&git, &store, id, Some(accepted));
                let writers_first = this.update_in(cx, |this, window, cx| {
                    let workspace = this.workspace_of_task(id);
                    let ended = workspace.filter(|_| this.workspaces.count() > 1);
                    if let Some(workspace) = ended {
                        this.close_workspace(workspace, window, cx);
                    }
                    ended.is_some() || workspace.is_none()
                });
                let Ok(writers_first) = writers_first else {
                    break 'closed done(Err(CloseError::Failed(window_closed())));
                };
                let closed = cx.background_spawn(async move { remove() }).await;
                if !writers_first && closed.is_ok() {
                    let _ = this.update_in(cx, |this, window, cx| {
                        if let Some(workspace) = this.workspace_of_task(id) {
                            this.close_workspace(workspace, window, cx);
                        }
                    });
                }
                done(closed)
            };
            let _ = this.update_in(cx, |this, _, cx| {
                this.tasks.read();
                this.sync_board(cx);
                match said {
                    Some(said) => this.task_failed(said, cx),
                    None => cx.notify(),
                }
            });
        })
        .detach();
    }

    /// The task of a workspace that has closed is closed too, with its
    /// worktree and branch kept, unless it was closed before its
    /// workspace.
    pub(crate) fn workspace_of_task_closed(&mut self, task: Option<u64>) {
        let Some(task) = task.filter(|task| self.tasks.is_open(*task)) else {
            return;
        };
        let kept = benterm_task::close(&Git::default(), &self.tasks.store, task, None);
        if let Err(error) = kept {
            self.tasks.notice = Some(format!("The task {task} was not closed: {error}."));
            self.dismissed = false;
        }
        self.tasks.read();
    }

    /// Closes the workspace `id`. If it shows a task, asks first what
    /// becomes of the task's worktree and branch, and says what removing
    /// them would lose.
    pub fn ask_to_close_workspace(
        &mut self,
        id: WorkspaceId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(task) = self.task_of_workspace(id).cloned() else {
            return self.close_workspace(id, window, cx);
        };
        if task.closed.is_some() {
            return self.close_workspace(id, window, cx);
        }
        let git = self.git(self.shells.as_ref());
        let counted = cx.background_spawn({
            let task = task.clone();
            async move { git.at_risk(&task) }
        });
        cx.spawn_in(window, async move |this, cx| {
            let risk = counted.await;
            let place = format!(
                "Its worktree is {}, on the branch {}.",
                task.worktree.display(),
                task.branch
            );
            let detail = match &risk {
                Ok(risk) if risk.is_nothing() => format!(
                    "{place} Every change in it is in a commit, and every commit is on another \
                     branch too. {IGNORED}"
                ),
                Ok(risk) => format!("{place} Removing them loses {risk}. {IGNORED}"),
                Err(error) => format!("{place} They can only be kept: {error}."),
            };
            let mut buttons = vec![PromptButton::new(KEEP)];
            if risk.is_ok() {
                buttons.push(PromptButton::new(REMOVE));
            }
            buttons.push(PromptButton::cancel(CANCEL));
            let question = format!("Close the task {}?", task.name);
            let Ok(answer) = this.update_in(cx, |_, window, cx| {
                window.prompt(PromptLevel::Warning, &question, Some(&detail), &buttons, cx)
            }) else {
                return;
            };
            let remove = match (answer.await, risk) {
                (Ok(0), _) => None,
                // What the user saw is what may be lost.
                (Ok(1), Ok(risk)) => Some(risk),
                _ => return,
            };
            // A task on the branch of an issue that the repository
            // keeps in its local file, and that is still open there:
            // whether the issue is done with the task is asked too.
            let issue = cx
                .background_spawn({
                    let repository = task.repository.clone();
                    let number = benterm_task::issue_number_in_branch(&task.branch);
                    async move {
                        let issues = benterm_task::read_local_issues(&repository).ok()?;
                        let issue = issues.issue(number?)?;
                        (issue.state == LocalIssueState::Open).then(|| issue.clone())
                    }
                })
                .await;
            let mut close_issue = None;
            if let Some(issue) = issue {
                let question = format!("Close the local issue #{} too?", issue.number);
                let detail = format!(
                    "The task is on the branch of the issue \"{}\", which is open in {}/{}.",
                    crate::issue_graph_view::one_line(&issue.title),
                    benterm_task::LOCAL_ISSUES_DIRECTORY,
                    benterm_task::LOCAL_ISSUES_FILE
                );
                let buttons = [
                    PromptButton::new(CLOSE_ISSUE),
                    PromptButton::new(LEAVE_ISSUE),
                ];
                let Ok(answer) = this.update_in(cx, |_, window, cx| {
                    window.prompt(PromptLevel::Info, &question, Some(&detail), &buttons, cx)
                }) else {
                    return;
                };
                if answer.await == Ok(0) {
                    close_issue = Some(issue.number);
                }
            }
            let _ = this.update_in(cx, |this, window, cx| {
                let done: Closed = Box::new(move |closed| {
                    let name = &task.name;
                    // The issue is closed only with a task that was.
                    let issue_left = close_issue.filter(|_| closed.is_ok()).and_then(|number| {
                        let state = LocalIssueState::Closed;
                        let change = LocalIssueChange::SetState { number, state };
                        let error = change_local_issues(&task.repository, change).err()?;
                        Some(format!(
                            "The task {name} is closed, but the local issue #{number} was not: \
                             {error}."
                        ))
                    });
                    let said = match closed {
                        Ok(Closing {
                            branch_left: Some(why),
                            ..
                        }) => Some(format!("The task {name} is closed, but {why}.")),
                        Ok(_) => None,
                        Err(CloseError::AtRisk(now)) => Some(format!(
                            "The worktree and the branch of the task {name} were not removed: \
                             since the question, more would be lost with them ({now}). Close \
                             the task again, or run benterm task close {} --remove.",
                            task.id
                        )),
                        Err(CloseError::Failed(error)) => Some(format!(
                            "The worktree and the branch of the task {name} were not removed: \
                             {error}."
                        )),
                    };
                    said.or(issue_left)
                });
                this.close_task(task.id, remove, window, cx, done);
            });
        })
        .detach();
    }

    /// `task.create`.
    pub(crate) fn create_task_for(
        &mut self,
        request: CreateTask,
        reply: benterm_ipc::Reply<TaskCreated>,
        shells: &Rc<Shells>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(caller) = request.caller
            && self.locate(caller.get(), cx).is_none()
            && self.agents.terminal_behind(caller.get()).is_none()
        {
            return reply.err(ApiError::new(
                ErrorCode::UnknownPane,
                format!("there is no pane {caller}, which the request came from"),
            ));
        }
        let request = TaskRequest {
            directory: PathBuf::from(request.cwd),
            name: request.name,
            branch: request.branch,
            role: request.role,
            agent: None,
            work: request.work.unwrap_or_default(),
            select: false,
        };
        let done: Done<TaskStarted, String> = Box::new(move |_, started, _, _| {
            reply.send(started.map_err(failed).map(|started| TaskCreated {
                task: info(&started.task, Some(started.workspace)),
                pane: api_id(started.pane),
            }));
        });
        self.create_task(request, shells, window, cx, done);
    }

    /// `task.list`.
    pub(crate) fn list_tasks(&mut self) -> Result<TaskList, ApiError> {
        // Another Benterm may have made or closed tasks since.
        let list = self
            .tasks
            .store
            .tasks()
            .map_err(|e| failed(e.to_string()))?;
        self.tasks.list = list;
        let tasks = self.tasks();
        Ok(TaskList {
            tasks: tasks
                .iter()
                .map(|row| info(&row.task, row.workspace))
                .collect(),
        })
    }

    /// `task.close`.
    pub(crate) fn close_task_for(
        &mut self,
        request: CloseTask,
        reply: benterm_ipc::Reply<TaskClosed>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if request.force && !request.remove {
            return reply.err(ApiError::new(
                ErrorCode::InvalidParams,
                "force goes with remove: without remove nothing is removed",
            ));
        }
        let remove = request.remove.then_some(if request.force {
            AtRisk::ANYTHING
        } else {
            AtRisk::default()
        });
        let id = request.task;
        let done: Closed = Box::new(move |closed| {
            reply.send(match closed {
                Ok(closing) => Ok(TaskClosed {
                    task: info(&closing.task, None),
                    branch_left: closing.branch_left.map(|why| why.to_string()),
                }),
                Err(CloseError::AtRisk(risk)) => Err(ApiError::new(
                    ErrorCode::AtRisk,
                    format!(
                        "removing the worktree and the branch of the task {id} would lose {risk}"
                    ),
                )),
                Err(CloseError::Failed(error)) => Err(failed(error.to_string())),
            });
            None
        });
        self.close_task(id, remove, window, cx, done);
    }
}

/// The directory a path typed into the form names: `~/` is the home
/// directory.
pub(crate) fn typed_directory(text: &str, home: Option<&Path>) -> PathBuf {
    let text = text.trim();
    match (text.strip_prefix("~/"), home) {
        (Some(below), Some(home)) => home.join(below),
        _ => PathBuf::from(text),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use benterm_agent::{AgentState, WaitReason};
    use benterm_task::{Closed, Recorded, Worktree};

    use super::*;

    fn status(state: AgentState) -> AgentStatus {
        AgentStatus {
            agent: (state != AgentState::Unknown).then_some("claude"),
            state,
        }
    }

    fn make_task(store: &TaskStore, repo: &str, branch: &str) -> Task {
        store
            .add(Recorded {
                name: branch.to_owned(),
                repository: PathBuf::from(repo),
                base_branch: "main".to_owned(),
                base_commit: "0123456789abcdef0123456789abcdef01234567".to_owned(),
                branch: branch.to_owned(),
                worktree: PathBuf::from("/w/benterm").join(branch),
                role: None,
            })
            .unwrap()
    }

    #[test]
    fn a_typed_path_below_the_home_directory_is_made_whole() {
        let home = Some(Path::new("/Users/me"));
        assert_eq!(
            typed_directory(" ~/src/benterm ", home),
            PathBuf::from("/Users/me/src/benterm")
        );
        assert_eq!(
            typed_directory("/src/benterm", home),
            PathBuf::from("/src/benterm")
        );
        assert_eq!(typed_directory("~/src", None), PathBuf::from("~/src"));
    }

    #[test]
    fn issue_state_is_waiting_when_task_workspace_has_waiting_agent() {
        let store = TaskStore::open(None);
        let t1 = make_task(&store, "/r/benterm", "issue-7");
        let t2 = make_task(&store, "/r/other", "issue-7");
        let tasks = store.tasks().unwrap();

        let statuses: HashMap<u64, Vec<AgentStatus>> = [
            (
                t1.id,
                vec![
                    status(AgentState::Working),
                    status(AgentState::Waiting(WaitReason::Permission)),
                ],
            ),
            (t2.id, vec![status(AgentState::Error)]),
        ]
        .into_iter()
        .collect();

        let state = issue_agent_state_for_tasks(
            &tasks,
            7,
            |repo| repo == Path::new("/r/benterm"),
            |task| statuses.get(&task.id).cloned().unwrap_or_default(),
        );
        assert_eq!(state, IssueAgentState::Waiting);
    }

    #[test]
    fn issue_agent_state_prioritises_waiting_error_working_done_none() {
        assert_eq!(
            issue_agent_state_from_statuses([status(AgentState::Unknown)]),
            IssueAgentState::None
        );
        assert_eq!(
            issue_agent_state_from_statuses([status(AgentState::Done)]),
            IssueAgentState::Done
        );
        assert_eq!(
            issue_agent_state_from_statuses([
                status(AgentState::Done),
                status(AgentState::Working),
            ]),
            IssueAgentState::Working
        );
        assert_eq!(
            issue_agent_state_from_statuses([
                status(AgentState::Working),
                status(AgentState::Error),
            ]),
            IssueAgentState::Error
        );
        assert_eq!(
            issue_agent_state_from_statuses([
                status(AgentState::Error),
                status(AgentState::Waiting(WaitReason::Question)),
            ]),
            IssueAgentState::Waiting
        );
    }

    #[test]
    fn closed_tasks_do_not_contribute_agent_state() {
        let store = TaskStore::open(None);
        let closed = make_task(&store, "/r/benterm", "issue-7");
        store
            .set_closed(closed.id, Closed::new(100, Worktree::Kept))
            .unwrap();
        let open = make_task(&store, "/r/benterm", "issue-8");
        let tasks = store.tasks().unwrap();

        let states = issue_agent_states_for_tasks(
            &tasks,
            |repo| repo == Path::new("/r/benterm"),
            |task| {
                if task.id == closed.id {
                    vec![status(AgentState::Error)]
                } else if task.id == open.id {
                    vec![status(AgentState::Working)]
                } else {
                    Vec::new()
                }
            },
        );
        assert_eq!(states.get(&7), Some(&IssueAgentState::None));
        assert_eq!(states.get(&8), Some(&IssueAgentState::Working));
    }
}

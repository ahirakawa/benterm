//! The agents a window runs for the programs in its panes: started over
//! the socket API under a name, each in a terminal behind the window.
//!
//! A terminal behind the window is a [`TerminalView`] that no grid holds.
//! Nothing draws it, and its program runs all the same: the view takes
//! the terminal's changes and watches the agent in it whether it is drawn
//! or not. It has the id of a pane, so the requests for panes reach it,
//! and it can be moved into a tab of its own, where it is a pane like any
//! other.
//!
//! An agent is the child of the pane that started it. Three things follow
//! from that, and all of them are decided here, on the UI thread, one
//! update at a time.
//!
//! - A child ends when the terminal of its parent closes, unless it was
//!   started to outlive it. Its own children end with it. Every agent
//!   ends with the window: a terminal lives in the app. A window whose
//!   last pane closes stays while a child started to outlive its parent
//!   runs behind it, and shows the oldest one in its only tab; see
//!   [`Agents::take_outliving`].
//! - A child reports to its parent. A report is kept in the parent's
//!   inbox from the update that takes it until the update that writes it
//!   to the parent's terminal, once, as a pasted message with Enter after
//!   it. It is held back for as long as the user has a draft there, and
//!   the parent's pane is marked meanwhile. It is written for an agent
//!   to read and for no other program: while the program in front of the
//!   parent's terminal is a shell, or cannot be told, the report stays
//!   in the inbox, as a shell would run what a child wrote. If
//!   the parent closes first the report is dropped with it.
//! - A pane waits for an agent, or for all its children, to stop working.
//!   The wait is answered once: by the state it waited for, by its time
//!   running out, or because its connection hung up.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use benterm_agent::{AgentState, WaitReason};
use benterm_ipc::{
    AgentActivity, AgentInfo, AgentList, AgentShown, AgentSpawned, AgentTarget, AgentsWaited,
    ApiError, ConnectionId, ErrorCode, KillAgent, ListAgents, PaneOutput, ReadAgent, Reply,
    ReportAccepted, ReportStatus, ReportToParent, SendKeys, SendToAgent, ShowAgent, SpawnAgent,
    WaitForAgents, WaitedAgent,
};
use benterm_layout::PaneId;
use benterm_terminal::{PtyConfig, WriteError};
use futures::StreamExt;
use gpui::{App, Context, Entity, Subscription, Task, WeakEntity, Window};

use crate::graph::MessageKind;
use crate::ipc::{Shells, agent_state, api_id, failed, input_queue_full};
use crate::mailbox::{MailboxWatcher, PeerEntry, PendingDelivery};
use crate::pane_grid::LoneTerminal;
use crate::{BentermWindow, FirstPane, TerminalEvent, TerminalView};

/// How long a state has to last before a wait takes it, unless the wait
/// says otherwise. An agent whose turn has ended may be sent on by a hook
/// a moment later; a caller that knows its hooks take longer says so.
const SETTLE: Duration = Duration::from_secs(1);

/// How often a report that is held back is tried again.
const RETRY: Duration = Duration::from_millis(250);

/// How many reports wait for one parent before new ones are refused.
const INBOX_CAPACITY: usize = 64;
/// The most text one report carries.
pub(crate) const REPORT_LIMIT: usize = 64 << 10;
/// How many waits the window holds before new ones are refused.
const WAIT_CAPACITY: usize = 256;
const NAME_LIMIT: usize = 64;

/// An agent the window runs, for the views that list them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentRow {
    pub name: String,
    /// The id of the agent's terminal, which is the id of its pane once
    /// it is in a tab.
    pub pane: PaneId,
    /// The pane that started the agent, also after that pane has closed.
    pub parent: Option<PaneId>,
    /// Whether the terminal is behind the window, in no tab.
    pub background: bool,
    pub outlive_parent: bool,
    /// The status of the agent's last report that had one.
    pub report_status: Option<ReportStatus>,
}

/// What a window keeps about its agents. Only the UI thread touches it.
#[derive(Default)]
pub(crate) struct Agents {
    /// The agents that run, oldest first.
    list: Vec<Agent>,
    /// The terminals whose end the window is told: those of the agents
    /// and of their parents, by the number of the pane id.
    followed: HashMap<u64, [Subscription; 2]>,
    /// Queued messages on their way to any pane (parent reports, sibling
    /// peer messages, and system notifications), by the target pane id.
    inboxes: HashMap<u64, Inbox>,
    /// Round counters for sibling conversations, keyed by the parent pane id.
    peer_rounds: HashMap<u64, PeerConversation>,
    waits: HashMap<u64, Wait>,
    next_wait: u64,
    /// The terminals that owe a turn: their program was started with a
    /// command or given a message, and has not been seen at work since.
    /// By the number of the pane id, with what watches for the turn.
    owing: HashMap<u64, Subscription>,
    /// Watches file-based `outbox/` directories of sandboxed agents.
    mailbox: Option<MailboxWatcher>,
}

#[derive(Default)]
struct PeerConversation {
    rounds: u32,
    limit_notified: bool,
}

struct Agent {
    name: String,
    pane: PaneId,
    parent: Option<PaneId>,
    outlive_parent: bool,
    allow_peers: bool,
    max_peer_rounds: u32,
    place: Place,
    /// The line to put before the first message the agent is given, until
    /// one is.
    first_message_line: Option<String>,
    /// The status of the last report that had one, kept from the update
    /// that takes the report: it is what the agent's work came to, also
    /// while the report is held back.
    report_status: Option<ReportStatus>,
    /// Private file-based mailbox directory, when
    /// created for this agent.
    mailbox_dir: Option<PathBuf>,
}

enum Place {
    /// In no grid: the agent is what keeps the terminal.
    Behind { lone: LoneTerminal },
    /// In a pane, whose grid keeps the terminal.
    Tab(WeakEntity<TerminalView>),
}

impl Agent {
    fn view(&self) -> Option<Entity<TerminalView>> {
        match &self.place {
            Place::Behind { lone } => Some(lone.view().clone()),
            Place::Tab(view) => view.upgrade(),
        }
    }
}

/// Queued messages for one target pane, oldest first.
#[derive(Default)]
struct Inbox {
    items: VecDeque<InboxItem>,
    /// Whether the oldest item is held back waiting on the user (typing or
    /// a dialog), which the target's pane is marked for.
    held: bool,
    /// Tries again while the oldest item is held back.
    retry: Option<Task<()>>,
}

enum InboxKind {
    /// A child's report to its parent (`agent.report`).
    Report { status: Option<ReportStatus> },
    /// A direct message from a sibling agent (`agent.send`).
    Peer { round: u32, max_rounds: u32 },
    /// A system notification to the parent when a sibling conversation
    /// reaches its round limit.
    PeerLimit,
}

struct InboxItem {
    /// The pane the message is for.
    to: PaneId,
    from_name: String,
    from_pane: u64,
    text: String,
    kind: InboxKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PeerPolicy {
    pane: u64,
    parent: Option<u64>,
    allow_peers: bool,
    max_peer_rounds: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SendRoute {
    Direct,
    Peer { parent: u64, max_rounds: u32 },
}

fn authorize_send(
    sender: Option<PeerPolicy>,
    target: Option<PeerPolicy>,
    target_pane: u64,
) -> Result<SendRoute, ErrorCode> {
    let Some(sender) = sender else {
        return Ok(SendRoute::Direct);
    };
    if sender.parent == Some(target_pane) {
        return Ok(SendRoute::Direct);
    }
    if target.is_some_and(|t| t.parent == Some(sender.pane)) {
        return Ok(SendRoute::Direct);
    }
    if let Some(target) = target
        && sender.pane != target.pane
        && let Some(parent) = sender.parent
        && target.parent == Some(parent)
        && sender.allow_peers
        && target.allow_peers
    {
        return Ok(SendRoute::Peer {
            parent,
            max_rounds: sender.max_peer_rounds.min(target.max_peer_rounds),
        });
    }
    Err(ErrorCode::PeerNotAllowed)
}

fn should_hold(kind: &InboxKind, user_is_typing: bool, state: AgentState) -> bool {
    if user_is_typing || matches!(state, AgentState::Waiting(_)) {
        return true;
    }
    matches!(kind, InboxKind::Peer { .. }) && matches!(state, AgentState::Working)
}

/// Whether a child's report may be written into its parent's terminal:
/// `agent_in_front` is the agent the program in front was just read to
/// be, and `state` what that agent is doing.
///
/// A report is input a child chose, and the child may be sandboxed while
/// its parent is not. An agent reads it as a message. A shell would run
/// it, so with a shell in front, or a program that could not be read,
/// nothing is written. An agent that has shown no state yet is not at its
/// prompt as far as anyone knows: it may be starting, at a dialog the
/// screen has not shown for long enough.
fn takes_report(agent_in_front: Option<&str>, state: AgentState) -> bool {
    agent_in_front.is_some()
        && matches!(
            state,
            AgentState::Working | AgentState::Done | AgentState::Error
        )
}

fn is_agent_there(
    closed: bool,
    state: AgentActivity,
    until: &[AgentActivity],
    owing: bool,
    pending_inbox: bool,
) -> bool {
    closed || (until.contains(&state) && !owing && !pending_inbox)
}

struct Wait {
    connection: ConnectionId,
    reply: Reply<AgentsWaited>,
    targets: Vec<Target>,
    until: Vec<AgentActivity>,
    settle: Duration,
    /// Runs while the targets are all where the wait wants them and have
    /// not been for long enough yet.
    settling: Option<Task<()>>,
    _deadline: Option<Task<()>>,
    _watching: Vec<Subscription>,
}

struct Target {
    name: Option<String>,
    pane: PaneId,
    view: WeakEntity<TerminalView>,
}

impl Agents {
    pub(crate) fn terminal_behind(&self, pane: u64) -> Option<(PaneId, Entity<TerminalView>)> {
        self.list.iter().find_map(|agent| match &agent.place {
            Place::Behind { lone } if agent.pane.get() == pane => {
                Some((agent.pane, lone.view().clone()))
            }
            _ => None,
        })
    }

    /// The terminals behind the window, oldest first.
    pub(crate) fn terminals_behind(&self) -> Vec<(PaneId, Entity<TerminalView>)> {
        self.list
            .iter()
            .filter_map(|agent| match &agent.place {
                Place::Behind { lone } => Some((agent.pane, lone.view().clone())),
                Place::Tab(_) => None,
            })
            .collect()
    }

    /// The name the terminal with the pane id `pane` was started under.
    pub(crate) fn name_of(&self, pane: u64) -> Option<&str> {
        self.with_pane(pane).map(|agent| agent.name.as_str())
    }

    fn has_pending_inbox(inboxes: &HashMap<u64, Inbox>, pane: u64) -> bool {
        inboxes
            .get(&pane)
            .is_some_and(|inbox| !inbox.items.is_empty())
    }

    /// Whether the terminal counts as where a wait wants it: in one of
    /// the wait's states, not owing a turn, and with no undelivered inbox
    /// messages waiting for it.
    fn is_there(
        owing: &HashMap<u64, Subscription>,
        inboxes: &HashMap<u64, Inbox>,
        wait: &Wait,
        agent: &WaitedAgent,
    ) -> bool {
        let pane = agent.pane.get();
        is_agent_there(
            agent.closed,
            agent.state,
            &wait.until,
            owing.contains_key(&pane),
            Self::has_pending_inbox(inboxes, pane),
        )
    }

    /// Moves the oldest agent behind the window that was started to
    /// outlive its parent into a pane, and returns its terminal for the
    /// pane. The agent stays the same agent, now in a tab.
    ///
    /// Only these keep a window whose last pane has closed: the other
    /// agents behind it are children of a pane that is closing, which
    /// end with it, or were started from outside the window, which end
    /// with the window as they always have.
    pub(crate) fn take_outliving(&mut self) -> Option<LoneTerminal> {
        let agent = self
            .list
            .iter_mut()
            .find(|agent| agent.outlive_parent && matches!(agent.place, Place::Behind { .. }))?;
        let view = agent.view()?.downgrade();
        match std::mem::replace(&mut agent.place, Place::Tab(view)) {
            Place::Behind { lone } => Some(lone),
            Place::Tab(_) => unreachable!("the agent was behind the window"),
        }
    }

    fn named(&self, name: &str) -> Option<&Agent> {
        self.list.iter().find(|agent| agent.name == name)
    }

    fn with_pane(&self, pane: u64) -> Option<&Agent> {
        self.list.iter().find(|agent| agent.pane.get() == pane)
    }

    fn with_pane_mut(&mut self, pane: u64) -> Option<&mut Agent> {
        self.list.iter_mut().find(|agent| agent.pane.get() == pane)
    }
}

impl BentermWindow {
    /// The agents the window runs, oldest first.
    pub fn agents(&self, _: &App) -> Vec<AgentRow> {
        self.agents
            .list
            .iter()
            .map(|agent| AgentRow {
                name: agent.name.clone(),
                pane: agent.pane,
                parent: agent.parent,
                background: matches!(agent.place, Place::Behind { .. }),
                outlive_parent: agent.outlive_parent,
                report_status: agent.report_status,
            })
            .collect()
    }

    /// Makes the program of the focused pane of the selected tab the
    /// agent `name`, whose last report had `status`. For pictures of the
    /// session list made without a socket to start agents and report
    /// over; the report itself is not written anywhere.
    pub fn take_made_up_report(
        &mut self,
        name: &str,
        status: ReportStatus,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let grid = self.grid(cx).read(cx);
        let pane = grid.focused();
        let Some(view) = grid.terminal(pane).cloned() else {
            return;
        };
        self.adopt_agent(name.to_owned(), pane, &view, window, cx);
        if let Some(agent) = self.agents.with_pane_mut(pane.get()) {
            agent.report_status = Some(status);
        }
        cx.notify();
    }

    /// How many reports and messages wait in the inbox of the pane with
    /// the number `pane`, taken and not written to its terminal.
    pub fn inbox_len(&self, pane: u64) -> usize {
        let inbox = self.agents.inboxes.get(&pane);
        inbox.map_or(0, |inbox| inbox.items.len())
    }

    /// Whether the pane with the number `pane` is marked because its
    /// inbox is held back until the user is done there.
    pub fn inbox_is_held(&self, pane: u64) -> bool {
        let inbox = self.agents.inboxes.get(&pane);
        inbox.is_some_and(|inbox| inbox.held)
    }

    /// Takes what the agent `name` writes to the mailbox `dir` from now
    /// on, as for an agent started with one. The window makes a mailbox
    /// for Codex alone, among the app's files; this is for a mailbox made
    /// elsewhere. Returns whether the agent runs and the mailboxes are
    /// watched.
    pub fn watch_mailbox(&mut self, name: &str, dir: PathBuf) -> bool {
        let agents = self.agents.list.iter_mut();
        let Some(agent) = agents.into_iter().find(|agent| agent.name == name) else {
            return false;
        };
        let Some(watcher) = &mut self.agents.mailbox else {
            return false;
        };
        watcher.watch(agent.pane.get(), dir.clone());
        agent.mailbox_dir = Some(dir);
        true
    }

    /// The terminal of the agent with the name, in a tab or behind.
    pub fn agent_terminal(&self, name: &str) -> Option<Entity<TerminalView>> {
        self.agents.named(name).and_then(Agent::view)
    }

    /// The terminal with the id of a pane, in a pane or behind the
    /// window.
    pub fn terminal_with_id(&self, pane: PaneId, cx: &App) -> Option<Entity<TerminalView>> {
        self.find_terminal(api_id(pane), cx)
            .ok()
            .map(|(_, view)| view)
    }

    /// The panes whose inbox items are held back until the user is done
    /// typing there, or has answered the dialog there.
    pub(crate) fn panes_holding_reports(&self) -> Vec<PaneId> {
        let held = self.agents.inboxes.values().filter(|inbox| inbox.held);
        held.filter_map(|inbox| inbox.items.front().map(|item| item.to))
            .collect()
    }

    /// The terminal owes a turn from now until a
    /// new turn begins, asks the user, or stops by an error: by OSC
    /// 7501 telling that work began, an accepted hook telling one of
    /// those states, or the state changing into one of them. Until then
    /// a wait does not take it for done. Callers that send input to a
    /// running terminal show the detector what the terminal already
    /// holds first, so unseen work from the turn before enters the
    /// baseline rather than counting as the turn the message starts.
    pub(crate) fn owe_turn(
        &mut self,
        pane: PaneId,
        view: &Entity<TerminalView>,
        cx: &mut Context<Self>,
    ) {
        let number = pane.get();
        let turns_before = view.read(cx).agent_turns_begun();
        let watch = cx.observe(view, move |this, view, cx| {
            if view.read(cx).agent_turns_begun() != turns_before
                && this.agents.owing.remove(&number).is_some()
            {
                this.check_waits(cx);
            }
        });
        self.agents.owing.insert(number, watch);
    }

    fn check_waits(&mut self, cx: &mut Context<Self>) {
        let ids: Vec<u64> = self.agents.waits.keys().copied().collect();
        for id in ids {
            self.check_wait(id, cx);
        }
    }

    /// How many waits for agents the window holds now.
    pub fn agent_waits(&self) -> usize {
        self.agents.waits.len()
    }

    /// Starts the file-based mailbox watcher for sandboxed agents and
    /// routes incoming outbox deliveries to the UI thread.
    pub(crate) fn init_mailbox(&mut self, cx: &mut Context<Self>) {
        let Ok((watcher, mut deliveries)) = MailboxWatcher::start() else {
            return;
        };
        self.agents.mailbox = Some(watcher);
        cx.spawn(async move |this, cx| {
            while let Some(delivery) = deliveries.next().await {
                if this
                    .update(cx, |this, cx| this.deliver_mailbox_message(delivery, cx))
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
    }

    fn deliver_mailbox_message(&mut self, delivery: PendingDelivery, cx: &mut Context<Self>) {
        if self
            .agents
            .mailbox
            .as_ref()
            .is_none_or(|watcher| !watcher.is_current(&delivery))
        {
            return;
        }
        let sender_pane = delivery.pane();
        let Some(sender_id) = self.agents.with_pane(sender_pane).map(|agent| agent.pane) else {
            let _ = delivery.reject("sender agent is no longer running");
            return;
        };
        let to = delivery.message().to.trim().to_owned();
        let text = delivery.message().text.clone();
        let status = delivery.message().status;
        let outcome = if to == "parent" {
            self.report_to_parent(
                ReportToParent {
                    from: api_id(sender_id),
                    text,
                    status,
                },
                cx,
            )
            .map(|_| ())
        } else {
            let Ok(agent) = to.parse::<AgentTarget>();
            self.send_to_agent(
                &SendToAgent {
                    agent,
                    text,
                    caller: Some(api_id(sender_id)),
                },
                cx,
            )
        };
        match outcome {
            Ok(()) => {
                let _ = delivery.accept();
            }
            Err(error) => {
                let _ = delivery.reject(&error.message);
            }
        }
    }

    fn sync_mailbox_peers(&self, parent: PaneId) {
        for agent in &self.agents.list {
            if agent.parent != Some(parent) {
                continue;
            }
            let Some(dir) = &agent.mailbox_dir else {
                continue;
            };
            let peers: Vec<PeerEntry> = if agent.allow_peers {
                self.agents
                    .list
                    .iter()
                    .filter(|other| {
                        other.pane != agent.pane
                            && other.parent == Some(parent)
                            && other.allow_peers
                    })
                    .map(|other| PeerEntry {
                        name: other.name.clone(),
                        pane: other.pane.get(),
                    })
                    .collect()
            } else {
                Vec::new()
            };
            let _ = crate::mailbox::write_peers(dir, &peers);
        }
    }

    /// `agent.spawn`.
    pub(crate) fn spawn_agent(
        &mut self,
        request: SpawnAgent,
        role: Option<String>,
        shells: &Rc<Shells>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<AgentSpawned, ApiError> {
        check_name(&request.name)?;
        if request.max_peer_rounds == Some(0) {
            return Err(invalid("max_peer_rounds must be at least 1".into()));
        }
        let max_peer_rounds = request
            .max_peer_rounds
            .unwrap_or(benterm_ipc::DEFAULT_MAX_PEER_ROUNDS);
        if self.agents.named(&request.name).is_some() {
            return Err(ApiError::new(
                ErrorCode::NameTaken,
                format!(
                    "an agent named {:?} is running; names are free again when their agent ends",
                    request.name
                ),
            ));
        }
        let parent = request
            .parent
            .map(|parent| self.find_terminal(parent, cx))
            .transpose()?;
        let cwd = request.cwd.as_deref().map(PathBuf::from);
        if let Some(cwd) = &cwd
            && !(cwd.is_absolute() && cwd.is_dir())
        {
            return Err(invalid(format!(
                "cwd must be an absolute path to a directory: {}",
                cwd.display()
            )));
        }
        let mut program = agent_program(&request, &shells.pty, shells.login_env.path())?;
        let mailbox_dir = if request.allow_peers || parent.is_some() {
            configure_mailbox(&mut program, cwd.as_deref())
        } else {
            None
        };
        program
            .env
            .retain(|(name, _)| name != benterm_ipc::MAILBOX_DIR_ENV);
        program.env.push((
            benterm_ipc::MAILBOX_DIR_ENV.into(),
            mailbox_dir
                .as_ref()
                .map(|dir| dir.as_os_str().to_owned())
                .unwrap_or_default(),
        ));
        let program = with_shell_after(program, &request, &shells.pty);
        let spawn = shells.spawn_program_with_permissions(program, cwd, request.control);
        // Nothing draws the terminal, so no frame will fit it: it starts
        // at the size a pane that has the window to itself would have.
        let theme = self.settings.theme().clone();
        let size = crate::pane_area::terminal_size(window, &theme.terminal, cx);
        let first = FirstPane::start(&spawn, &theme.terminal, None, size).map_err(|error| {
            if let Some(dir) = &mailbox_dir {
                crate::mailbox::cleanup_mailbox(dir);
            }
            failed(format!(
                "could not start a terminal for the agent {:?}: {error}",
                request.name
            ))
        })?;
        let lone = first.into_lone(theme, window, cx);
        let pane = lone.id();

        // From here the agent is found by its name and its id, all within
        // this update.
        self.follow(pane, lone.view(), window, cx);
        if !request.command.is_empty() {
            // A program started with a command has its first turn ahead
            // of it: an agent's start screen is not the end of its work.
            self.owe_turn(pane, lone.view(), cx);
        }
        lone.view().update(cx, |view, _| {
            if let Some(role) = role {
                view.restricted = Some(benterm_session::RestrictedPane { role: Some(role) });
            }
            if request.shell_after && !request.command.is_empty() {
                view.give_way_to(crate::roles::shell_argv(&shells.pty));
            }
        });
        let parent_id = parent.as_ref().map(|(parent, _)| *parent);
        if let Some((parent, view)) = &parent {
            self.follow(*parent, view, window, cx);
        }
        if let Some(dir) = &mailbox_dir
            && let Some(watcher) = &mut self.agents.mailbox
        {
            watcher.watch(pane.get(), dir.clone());
        }
        self.agents.list.push(Agent {
            name: request.name.clone(),
            pane,
            parent: parent_id,
            outlive_parent: request.outlive_parent,
            allow_peers: request.allow_peers,
            max_peer_rounds,
            place: Place::Behind { lone },
            first_message_line: request.first_message_line,
            report_status: None,
            mailbox_dir,
        });
        if let Some(parent_id) = parent_id {
            self.sync_mailbox_peers(parent_id);
        }
        self.sync_graph(cx);
        cx.notify();
        Ok(AgentSpawned {
            name: request.name,
            pane: api_id(pane),
        })
    }

    /// Has the window told when the terminal with the id `pane` ends:
    /// when its program exits, and when the terminal itself is gone,
    /// which is how a pane the user closes ends. The exit is not left to
    /// the second: a terminal lasts for as long as anything keeps its
    /// view, and its children are not to last that long.
    /// Makes the program of a pane the agent `name`, as if it had been
    /// started as one and shown in its tab. It has no parent.
    pub(crate) fn adopt_agent(
        &mut self,
        name: String,
        pane: PaneId,
        view: &Entity<TerminalView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.follow(pane, view, window, cx);
        // An agent's start screen is not the end of its work.
        self.owe_turn(pane, view, cx);
        self.agents.list.push(Agent {
            name,
            pane,
            parent: None,
            outlive_parent: false,
            allow_peers: false,
            max_peer_rounds: benterm_ipc::DEFAULT_MAX_PEER_ROUNDS,
            place: Place::Tab(view.downgrade()),
            first_message_line: None,
            report_status: None,
            mailbox_dir: None,
        });
        self.sync_graph(cx);
    }

    fn follow(
        &mut self,
        pane: PaneId,
        view: &Entity<TerminalView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.agents.followed.contains_key(&pane.get()) {
            return;
        }
        let exited = cx.subscribe_in(view, window, move |this, _, event, window, cx| {
            if matches!(event, TerminalEvent::Exited | TerminalEvent::GaveWayToShell) {
                this.terminal_closed(pane, window, cx);
            }
        });
        let gone = cx.observe_release_in(view, window, move |this, _, window, cx| {
            this.terminal_closed(pane, window, cx);
        });
        self.agents.followed.insert(pane.get(), [exited, gone]);
    }

    /// The terminal with the id `pane` has
    /// ended, behind the window or in a pane, or its agent has given way
    /// to a shell. Its agent is no more, the reports on their way to it
    /// are dropped, and its children end, except the ones started to
    /// outlive it. A pane whose agent gave way to a shell stays, as a
    /// terminal like any other; behind the window, the terminal closes,
    /// as there is no pane for the shell.
    fn terminal_closed(&mut self, pane: PaneId, window: &mut Window, cx: &mut Context<Self>) {
        // Told twice when a program exits and its terminal goes after
        // it; the second time there is nothing left to do.
        if self.agents.followed.remove(&pane.get()).is_none() {
            return;
        }
        self.keep_closed_node(pane, cx);
        self.agents.owing.remove(&pane.get());
        self.agents.peer_rounds.remove(&pane.get());
        if self
            .agents
            .inboxes
            .remove(&pane.get())
            .is_some_and(|inbox| inbox.held)
        {
            self.show_marks(cx);
        }
        let closed_info = self
            .agents
            .with_pane(pane.get())
            .map(|agent| (agent.parent, agent.mailbox_dir.clone()));
        if let Some(watcher) = &mut self.agents.mailbox {
            watcher.unwatch(pane.get());
        }
        if let Some((_, Some(dir))) = &closed_info {
            crate::mailbox::cleanup_mailbox(dir);
        }
        // Behind the window the agent is what keeps the terminal, so
        // this closes it. A pane is closed by its grid.
        self.agents.list.retain(|agent| agent.pane != pane);
        if let Some((Some(parent_id), _)) = closed_info {
            self.sync_mailbox_peers(parent_id);
        }
        let children: Vec<PaneId> = self
            .agents
            .list
            .iter()
            .filter(|agent| agent.parent == Some(pane) && !agent.outlive_parent)
            .map(|agent| agent.pane)
            .collect();
        for child in children {
            self.end_agent(child, window, cx);
        }
        self.sync_graph(cx);
        cx.notify();
    }

    /// Closes the terminal of an agent, which hangs its program up.
    pub(crate) fn end_agent(&mut self, pane: PaneId, window: &mut Window, cx: &mut Context<Self>) {
        // A pane closes through its grid, so the tab goes with it.
        if let Some(place) = self.locate(pane.get(), cx) {
            place
                .grid
                .update(cx, |grid, cx| grid.close_pane(place.pane, window, cx));
        }
        self.keep_closed_node(pane, cx);
        // Behind the window the agent is all that keeps the terminal.
        self.agents
            .list
            .retain(|agent| agent.pane != pane || matches!(agent.place, Place::Tab(_)));
    }

    /// `agent.list`.
    pub(crate) fn list_agents(&self, request: &ListAgents, cx: &App) -> AgentList {
        let parent = request.parent.map(benterm_ipc::PaneId::get);
        let siblings = request.siblings_of.map(|pane| {
            let caller = pane.get();
            let caller_parent = self
                .agents
                .with_pane(caller)
                .and_then(|agent| agent.parent)
                .map(PaneId::get);
            (caller, caller_parent)
        });
        let agents = self
            .agents
            .list
            .iter()
            .filter(|agent| {
                if let Some(parent) = parent
                    && agent.parent.map(PaneId::get) != Some(parent)
                {
                    return false;
                }
                if let Some((caller, caller_parent)) = siblings {
                    let Some(caller_parent) = caller_parent else {
                        return false;
                    };
                    if agent.pane.get() == caller
                        || agent.parent.map(PaneId::get) != Some(caller_parent)
                        || !agent.allow_peers
                    {
                        return false;
                    }
                }
                true
            })
            .map(|agent| {
                let mark = self.stall_mark(agent.pane);
                let (state, msg) = agent.view().map_or((AgentActivity::Unknown, None), |view| {
                    let view = view.read(cx);
                    (
                        agent_state(api_id(agent.pane), view.agent_status()).state,
                        view.agent_msg(),
                    )
                });
                AgentInfo {
                    name: agent.name.clone(),
                    pane: api_id(agent.pane),
                    parent: agent.parent.map(api_id),
                    background: matches!(agent.place, Place::Behind { .. }),
                    outlive_parent: agent.outlive_parent,
                    state,
                    stalled: mark.is_some_and(|mark| mark.stalled),
                    changed_at_ms: mark.map(|mark| mark.last_change_ms),
                    report_status: agent.report_status,
                    msg,
                    allow_peers: agent.allow_peers,
                }
            })
            .collect();
        AgentList { agents }
    }

    /// The terminal a request names, with its id and the name of its
    /// agent if it is one.
    fn resolve(
        &self,
        target: &AgentTarget,
        cx: &App,
    ) -> Result<(PaneId, Entity<TerminalView>, Option<String>), ApiError> {
        match target {
            AgentTarget::Name(name) => self
                .agents
                .named(name)
                .and_then(|agent| Some((agent.pane, agent.view()?, Some(agent.name.clone()))))
                .ok_or_else(|| {
                    ApiError::new(
                        ErrorCode::UnknownAgent,
                        format!("there is no agent named {name:?}"),
                    )
                }),
            AgentTarget::Pane(pane) => {
                let (pane, view) = self.find_terminal(*pane, cx)?;
                let name = self.agents.with_pane(pane.get()).map(|a| a.name.clone());
                Ok((pane, view, name))
            }
        }
    }

    /// `agent.show`.
    pub(crate) fn show_agent(
        &mut self,
        request: &ShowAgent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<AgentShown, ApiError> {
        let (pane, view, _) = self.resolve(&request.agent, cx)?;
        let mark = self.stall_mark(pane);
        let shown = AgentShown {
            pane: api_id(pane),
            stalled: mark.is_some_and(|mark| mark.stalled),
            changed_at_ms: mark.map(|mark| mark.last_change_ms),
            report_status: self
                .agents
                .with_pane(pane.get())
                .and_then(|agent| agent.report_status),
        };
        if let Some(place) = self.locate(pane.get(), cx) {
            // In a tab already: only the selection is left to do.
            if request.select {
                place
                    .tabs
                    .update(cx, |tabs, cx| tabs.select(place.tab, window, cx));
                self.select_workspace(place.workspace, window, cx);
            }
            return Ok(shown);
        }
        let Some(agent) = self.agents.list.iter_mut().find(|a| a.pane == pane) else {
            return Err(failed(format!(
                "the terminal {pane} is in no tab and is no agent"
            )));
        };
        // `Show`: the tab goes where the agent's parent is, if it is
        // still in the window, and into the workspace on view otherwise.
        let parent = agent.parent;
        let Place::Behind { lone } =
            std::mem::replace(&mut agent.place, Place::Tab(view.downgrade()))
        else {
            return Err(failed(format!("the terminal {pane} is in no tab")));
        };
        let home = parent.and_then(|parent| self.locate(parent.get(), cx));
        let (workspace, tabs) = match home {
            Some(place) => (place.workspace, place.tabs),
            None => (self.selected_workspace(), self.tabs().clone()),
        };
        tabs.update(cx, |tabs, cx| {
            tabs.open_tab_adopting(lone, request.select, window, cx);
        });
        if request.select {
            self.select_workspace(workspace, window, cx);
        }
        self.sync_graph(cx);
        cx.notify();
        Ok(shown)
    }

    fn peer_policy(&self, pane: u64) -> Option<PeerPolicy> {
        self.agents.with_pane(pane).map(|agent| PeerPolicy {
            pane: agent.pane.get(),
            parent: agent.parent.map(PaneId::get),
            allow_peers: agent.allow_peers,
            max_peer_rounds: agent.max_peer_rounds,
        })
    }

    /// `agent.send`.
    pub fn send_to_agent(
        &mut self,
        request: &SendToAgent,
        cx: &mut Context<Self>,
    ) -> Result<(), ApiError> {
        let (pane, view, name) = self.resolve(&request.agent, cx)?;
        let sender_policy = request
            .caller
            .and_then(|caller| self.peer_policy(caller.get()));
        let target_policy = self.peer_policy(pane.get());
        match authorize_send(sender_policy, target_policy, pane.get()) {
            Err(ErrorCode::PeerNotAllowed) => {
                let who = name.unwrap_or_else(|| format!("terminal {pane}"));
                return Err(ApiError::new(
                    ErrorCode::PeerNotAllowed,
                    format!(
                        "peer messaging to {who} is not allowed: both agents must share the same \
                         parent and be started with --allow-peers"
                    ),
                ));
            }
            Err(other) => return Err(ApiError::new(other, "peer messaging refused")),
            Ok(SendRoute::Peer { parent, max_rounds }) => {
                let sender_pane = request
                    .caller
                    .expect("peer route requires a caller pane")
                    .get();
                return self.send_to_peer(sender_pane, pane, parent, max_rounds, &request.text, cx);
            }
            Ok(SendRoute::Direct) => {
                // The socket let a worker's credential through for its
                // peers only: a pane without control sends no further.
                // The panes of the window have control. Whether the
                // sender is still an agent does not count: the pane of
                // an agent that gave way to a shell keeps its credential,
                // and what the agent left running there holds it. A
                // sender whose pane is gone has no credential left to
                // have come with: someone with control names it.
                let permissions = self.shells.as_ref().and_then(|shells| shells.permissions());
                if let (Some(permissions), Some(caller)) = (permissions, request.caller)
                    && permissions.is_restricted()
                    && !permissions.has_control(caller)
                    && self.find_terminal(caller, cx).is_ok()
                {
                    return Err(ApiError::new(
                        ErrorCode::Forbidden,
                        "a pane without control may send only to its peers; \
                         an agent reports to its parent with benterm agent report",
                    ));
                }
            }
        }
        // What the terminal already holds of its program belongs to the
        // turn before this message: show it to the detector before
        // checking for a dialog or recording the baseline in `owe_turn`.
        view.update(cx, |view, cx| view.observe_program(cx));
        // The Enter after the message could answer the dialog, so an
        // agent at one is given nothing. The user answers it, from the
        // session list or the peek. A draft does not stop a message: this
        // is no input of the user's, unlike the pane a report goes into.
        if let AgentState::Waiting(reason) = view.read(cx).agent_status().state {
            let who = name.unwrap_or_else(|| format!("terminal {pane}"));
            let what = match reason {
                WaitReason::Permission => "a permission",
                WaitReason::Trust => "whether to trust its folder",
                WaitReason::Question => "the answer to a question",
                WaitReason::Choice => "a choice",
            };
            return Err(ApiError::new(
                ErrorCode::AgentWaiting,
                format!(
                    "{who} is waiting for {what}; nothing was sent. \
                     Answer it from the session list or its peek, then send again"
                ),
            ));
        }
        let agent = self.agents.list.iter_mut().find(|agent| agent.pane == pane);
        let line = agent
            .as_ref()
            .and_then(|agent| agent.first_message_line.as_deref());
        let text = match line {
            Some(line) => format!("{line}\n{}", request.text),
            None => request.text.clone(),
        };
        match view.read(cx).enter_message(&text) {
            Ok(()) => {
                // The line went with this message, so the next goes alone.
                if let Some(agent) = agent {
                    agent.first_message_line = None;
                }
                self.owe_turn(pane, &view, cx);
                // The sender is kept for the graph, if the window has its
                // terminal. One it has not, as a program that outlived its
                // pane, sends all the same.
                let caller = request.caller;
                if let Some((from, _)) = caller.and_then(|from| self.find_terminal(from, cx).ok()) {
                    self.keep_message(MessageKind::Request, from, pane, &request.text, cx);
                }
                Ok(())
            }
            Err(WriteError::Full) => Err(input_queue_full(format!("terminal {pane}"))),
            Err(WriteError::Closed) => {
                Err(failed(format!("the program of terminal {pane} has exited")))
            }
        }
    }

    /// Queues a direct peer message between two opt-in siblings sharing
    /// `parent`, enforcing the conversation round limit and notifying the
    /// parent's inbox when the limit is reached.
    fn send_to_peer(
        &mut self,
        sender_pane: u64,
        target_pane: PaneId,
        parent: u64,
        max_rounds: u32,
        text: &str,
        cx: &mut Context<Self>,
    ) -> Result<(), ApiError> {
        if text.len() > REPORT_LIMIT {
            return Err(invalid(format!(
                "a peer message is at most {REPORT_LIMIT} bytes; write the rest to a file and name it"
            )));
        }
        let Some((sender_id, sender_name)) = self
            .agents
            .with_pane(sender_pane)
            .map(|agent| (agent.pane, agent.name.clone()))
        else {
            return Err(ApiError::new(
                ErrorCode::PeerNotAllowed,
                "sender is no longer a running agent",
            ));
        };
        let conv = self.agents.peer_rounds.entry(parent).or_default();
        if conv.rounds >= max_rounds {
            let rounds = conv.rounds;
            let should_notify = !conv.limit_notified;
            conv.limit_notified = true;
            if should_notify {
                self.notify_parent_peer_limit(parent, rounds, max_rounds, cx);
            }
            return Err(ApiError::new(
                ErrorCode::RoundLimitReached,
                format!(
                    "sibling conversation under pane {parent} reached round limit ({rounds}/{max_rounds})"
                ),
            ));
        }
        let inbox_len = self
            .agents
            .inboxes
            .get(&target_pane.get())
            .map_or(0, |inbox| inbox.items.len());
        if inbox_len >= INBOX_CAPACITY {
            return Err(failed(format!(
                "{INBOX_CAPACITY} messages are waiting for pane {target_pane} already"
            )));
        }
        let conv = self.agents.peer_rounds.entry(parent).or_default();
        conv.rounds += 1;
        let round = conv.rounds;
        let inbox = self.agents.inboxes.entry(target_pane.get()).or_default();
        inbox.items.push_back(InboxItem {
            to: target_pane,
            from_name: sender_name,
            from_pane: sender_pane,
            text: text.to_owned(),
            kind: InboxKind::Peer { round, max_rounds },
        });
        let line = crate::graph::first_line(text);
        self.keep_message(MessageKind::Request, sender_id, target_pane, &line, cx);
        self.check_waits(cx);
        self.deliver_inbox(target_pane, cx);
        Ok(())
    }

    /// Enqueues a system notification in the parent's inbox when a sibling
    /// conversation hits its round limit, so the conductor can intervene.
    fn notify_parent_peer_limit(
        &mut self,
        parent: u64,
        rounds: u32,
        max_rounds: u32,
        cx: &mut Context<Self>,
    ) {
        let Ok((parent_id, view)) = self.find_terminal(benterm_ipc::PaneId::new(parent), cx) else {
            return;
        };
        if view.read(cx).has_exited() {
            return;
        }
        let inbox = self.agents.inboxes.entry(parent).or_default();
        if inbox.items.len() >= INBOX_CAPACITY {
            return;
        }
        inbox.items.push_back(InboxItem {
            to: parent_id,
            from_name: "system".to_owned(),
            from_pane: 0,
            text: format!("Sibling conversation reached round limit ({rounds}/{max_rounds})."),
            kind: InboxKind::PeerLimit,
        });
        self.deliver_inbox(parent_id, cx);
    }

    /// `agent.read`.
    pub(crate) fn read_agent(&self, request: &ReadAgent, cx: &App) -> Result<PaneOutput, ApiError> {
        let (_, view, _) = self.resolve(&request.agent, cx)?;
        let lines = request.lines.map(|lines| lines as usize);
        Ok(PaneOutput {
            text: view.read(cx).text(lines),
        })
    }

    /// `agent.keys`.
    /// The keys do not make the agent owe a turn: escape stops one and
    /// starts none, so a wait after it would wait for nothing.
    pub(crate) fn send_keys(&self, request: &SendKeys, cx: &App) -> Result<(), ApiError> {
        let (pane, view, _) = self.resolve(&request.agent, cx)?;
        match view.read(cx).press_keys(&request.keys) {
            Ok(Ok(())) => Ok(()),
            Ok(Err(WriteError::Full)) => Err(input_queue_full(format!("terminal {pane}"))),
            Ok(Err(WriteError::Closed)) => {
                Err(failed(format!("the program of terminal {pane} has exited")))
            }
            Err(error) => Err(invalid(error.to_string())),
        }
    }

    /// `agent.kill`: as the parent's closing ends a child. The rest
    /// follows from the end of the terminal: the agent leaves the list,
    /// its waits are answered and its own children end.
    pub(crate) fn kill_agent(
        &mut self,
        request: &KillAgent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), ApiError> {
        let (pane, _, _) = self.resolve(&request.agent, cx)?;
        self.end_agent(pane, window, cx);
        Ok(())
    }

    /// `agent.report`.
    pub(crate) fn report_to_parent(
        &mut self,
        request: ReportToParent,
        cx: &mut Context<Self>,
    ) -> Result<ReportAccepted, ApiError> {
        let (from, _) = self.find_terminal(request.from, cx)?;
        let agent = self.agents.with_pane(from.get());
        let Some((name, parent)) = agent.and_then(|a| Some((a.name.clone(), a.parent?))) else {
            return Err(failed(format!(
                "pane {from} was not started by another pane, so it has nobody to report to"
            )));
        };
        let parent_runs = self
            .find_terminal(api_id(parent), cx)
            .is_ok_and(|(_, view)| !view.read(cx).has_exited());
        if !parent_runs {
            return Err(failed(format!(
                "pane {parent}, which started the agent {name:?}, has closed"
            )));
        }
        if request.text.len() > REPORT_LIMIT {
            return Err(invalid(format!(
                "a report is at most {REPORT_LIMIT} bytes; write the rest to a file and name it"
            )));
        }
        let inbox = self.agents.inboxes.entry(parent.get()).or_default();
        if inbox.items.len() >= INBOX_CAPACITY {
            return Err(failed(format!(
                "{INBOX_CAPACITY} reports are waiting for pane {parent} already"
            )));
        }
        // The report is in the inbox before the answer says it
        // was taken, so a report that was taken is never lost on the way.
        // Kept for the graph once, here where it is taken, however long
        // it is held back.
        let line = crate::graph::first_line(&request.text);
        inbox.items.push_back(InboxItem {
            to: parent,
            from_name: name,
            from_pane: from.get(),
            text: request.text,
            kind: InboxKind::Report {
                status: request.status,
            },
        });
        if let Some(status) = request.status
            && let Some(agent) = self.agents.with_pane_mut(from.get())
        {
            agent.report_status = Some(status);
            cx.notify();
        }
        self.keep_message(MessageKind::Report, from, parent, &line, cx);
        self.check_waits(cx);
        self.deliver_inbox(parent, cx);
        Ok(ReportAccepted {
            parent: api_id(parent),
        })
    }

    /// Writes the oldest inbox item for `target` to its terminal,
    /// unless something holds it back. Taking an item out of the inbox and
    /// writing it happen in this one update, so an item is written once.
    /// The program in front is read in this update too, for every try, so
    /// a report is written to the agent that was found there and to
    /// nothing that came to the front since an earlier try.
    /// For peer messages, `owe_turn` is recorded in the same update so no
    /// wait can slip in between popping the inbox and the peer starting work.
    fn deliver_inbox(&mut self, target: PaneId, cx: &mut Context<Self>) {
        let Ok((_, view)) = self.find_terminal(api_id(target), cx) else {
            self.agents.inboxes.remove(&target.get());
            self.check_waits(cx);
            return;
        };
        if !self.agents.inboxes.contains_key(&target.get()) {
            return;
        }
        if view.read(cx).has_exited() {
            // The pane is about to close, and the inbox with it.
            self.agents.inboxes.remove(&target.get());
            self.check_waits(cx);
            return;
        }
        // Before the state is read: a process that has just come to the
        // front starts with no state, and the state below is its own.
        let agent_in_front = view.update(cx, |view, cx| view.agent_in_front(cx));
        view.update(cx, |view, cx| view.observe_program(cx));
        let (user_is_typing, state, marked_paste) = {
            let terminal = view.read(cx);
            (
                terminal.user_is_typing(),
                terminal.agent_status().state,
                terminal.takes_marked_pastes(),
            )
        };
        let first_line = self
            .agents
            .with_pane(target.get())
            .and_then(|agent| agent.first_message_line.clone());
        let mut delivered_peer = false;
        let mut closed_on_write = false;
        if let Some(inbox) = self.agents.inboxes.get_mut(&target.get()) {
            inbox.retry = None;
            if let Some(item) = inbox.items.front()
                && !should_hold(&item.kind, user_is_typing, state)
                && (!matches!(item.kind, InboxKind::Report { .. })
                    || takes_report(agent_in_front, state))
            {
                let is_peer = matches!(item.kind, InboxKind::Peer { .. });
                let body = framed(item, marked_paste);
                let text = match (is_peer, first_line.as_deref()) {
                    (true, Some(line)) => format!("{line}\n{body}"),
                    _ => body,
                };
                match view.read(cx).enter_message(&text) {
                    Ok(()) => {
                        inbox.items.pop_front();
                        delivered_peer = is_peer;
                    }
                    Err(WriteError::Full) => {
                        // Keep the item in the inbox and try again on the
                        // next tick once the program reads.
                    }
                    Err(WriteError::Closed) => {
                        closed_on_write = true;
                    }
                }
            }
        }
        if closed_on_write {
            self.agents.inboxes.remove(&target.get());
            self.check_waits(cx);
            return;
        }
        if delivered_peer {
            if let Some(agent) = self.agents.with_pane_mut(target.get()) {
                agent.first_message_line = None;
            }
            self.owe_turn(target, &view, cx);
        }
        let needs_user = user_is_typing || matches!(state, AgentState::Waiting(_));
        let Some(inbox) = self.agents.inboxes.get_mut(&target.get()) else {
            return;
        };
        let marked = inbox.held;
        inbox.held = needs_user && !inbox.items.is_empty();
        let mark_changed = marked != inbox.held;
        if inbox.items.is_empty() {
            self.agents.inboxes.remove(&target.get());
            if mark_changed {
                self.show_marks(cx);
            }
            self.check_waits(cx);
            return;
        }
        inbox.retry = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(RETRY).await;
            let _ = this.update(cx, |this, cx| this.deliver_inbox(target, cx));
        }));
        if mark_changed {
            self.show_marks(cx);
        }
    }

    /// `agent.wait`.
    pub(crate) fn wait_for_agents(
        &mut self,
        request: WaitForAgents,
        reply: Reply<AgentsWaited>,
        connection: ConnectionId,
        cx: &mut Context<Self>,
    ) {
        let targets = match self.wait_targets(&request, cx) {
            Ok(targets) => targets,
            Err(error) => return reply.err(error),
        };
        if self.agents.waits.len() >= WAIT_CAPACITY {
            return reply.err(failed(format!(
                "{WAIT_CAPACITY} waits are open already; end some first"
            )));
        }
        let id = self.agents.next_wait;
        self.agents.next_wait += 1;
        let mut watching = Vec::new();
        for target in &targets {
            let Some(view) = target.view.upgrade() else {
                continue;
            };
            // The view tells whenever the state of its agent changes, and
            // its end is the end of the terminal.
            watching.push(cx.observe(&view, move |this, _, cx| this.check_wait(id, cx)));
            watching.push(cx.observe_release(&view, move |this, _, cx| this.check_wait(id, cx)));
        }
        let deadline = request.wait_ms.map(|wait| {
            cx.spawn(async move |this, cx| {
                let wait = Duration::from_millis(wait);
                cx.background_executor().timer(wait).await;
                let _ = this.update(cx, |this, cx| this.wait_ran_out(id, cx));
            })
        });
        let until = match request.until.is_empty() {
            true => vec![
                AgentActivity::Done,
                AgentActivity::Waiting,
                AgentActivity::Error,
            ],
            false => request.until,
        };
        // Looking at the agents and keeping the wait are one
        // update, so no change falls between the two.
        self.agents.waits.insert(
            id,
            Wait {
                connection,
                reply,
                targets,
                until,
                settle: request.settle_ms.map_or(SETTLE, Duration::from_millis),
                settling: None,
                _deadline: deadline,
                _watching: watching,
            },
        );
        self.check_wait(id, cx);
    }

    fn wait_targets(&self, request: &WaitForAgents, cx: &App) -> Result<Vec<Target>, ApiError> {
        match (&request.agent, request.children_of) {
            (Some(agent), None) => {
                let (pane, view, name) = self.resolve(agent, cx)?;
                let view = view.downgrade();
                Ok(vec![Target { name, pane, view }])
            }
            (None, Some(parent)) => {
                let (parent, _) = self.find_terminal(parent, cx)?;
                let children = self.agents.list.iter();
                Ok(children
                    .filter(|agent| agent.parent == Some(parent))
                    .filter_map(|agent| {
                        Some(Target {
                            name: Some(agent.name.clone()),
                            pane: agent.pane,
                            view: agent.view()?.downgrade(),
                        })
                    })
                    .collect())
            }
            _ => Err(invalid(
                "a wait names one agent or the pane whose children it waits for".into(),
            )),
        }
    }

    /// Looks at the agents of a wait. Answers it when they are
    /// where it wants them and that cannot change any more, counts the
    /// time from now when they have just got there, and stops counting
    /// when one of them has left.
    fn check_wait(&mut self, id: u64, cx: &mut Context<Self>) {
        let Agents {
            waits,
            owing,
            inboxes,
            ..
        } = &mut self.agents;
        let Some(wait) = waits.get_mut(&id) else {
            return;
        };
        let states = wait.states(cx);
        let there = states
            .iter()
            .all(|agent| Agents::is_there(owing, inboxes, wait, agent));
        if !there {
            wait.settling = None;
            return;
        }
        // Closed terminals do not change any more.
        let settled = wait.settle.is_zero() || states.iter().all(|agent| agent.closed);
        if settled {
            self.finish_wait(id, Ok(AgentsWaited { agents: states }));
        } else if wait.settling.is_none() {
            let settle = wait.settle;
            wait.settling = Some(cx.spawn(async move |this, cx| {
                cx.background_executor().timer(settle).await;
                let _ = this.update(cx, |this, cx| this.wait_settled(id, cx));
            }));
        }
    }

    /// The agents of a wait have been where it wants them for
    /// as long as it asked. A change in between has stopped the count
    /// (`check_wait`); looking once more keeps the answer from resting on
    /// the order in which the app runs the two.
    fn wait_settled(&mut self, id: u64, cx: &mut Context<Self>) {
        let Agents {
            waits,
            owing,
            inboxes,
            ..
        } = &mut self.agents;
        let Some(wait) = waits.get_mut(&id) else {
            return;
        };
        wait.settling = None;
        let states = wait.states(cx);
        let there = states
            .iter()
            .all(|agent| Agents::is_there(owing, inboxes, wait, agent));
        if there {
            self.finish_wait(id, Ok(AgentsWaited { agents: states }));
        }
    }

    fn wait_ran_out(&mut self, id: u64, cx: &mut Context<Self>) {
        let Some(wait) = self.agents.waits.get(&id) else {
            return;
        };
        let states: Vec<String> = wait
            .states(cx)
            .into_iter()
            .map(|agent| {
                let name = agent.name.unwrap_or_else(|| agent.pane.to_string());
                let state = match (agent.closed, agent.state) {
                    (true, _) => "closed",
                    (_, AgentActivity::Working) => "still working",
                    (_, AgentActivity::Waiting) => "waiting",
                    (_, AgentActivity::Done) => "done",
                    (_, AgentActivity::Error) => "stopped by an error",
                    (_, AgentActivity::Unknown) => "in no known state",
                };
                format!("{name} is {state}")
            })
            .collect();
        let message = format!("the time to wait ran out: {}", states.join(", "));
        self.finish_wait(id, Err(ApiError::new(ErrorCode::WaitTimedOut, message)));
    }

    /// Nobody reads the answers of the connection's waits any
    /// more. They are answered all the same, which is what lets the
    /// connection close.
    pub(crate) fn hung_up(&mut self, connection: ConnectionId) {
        let gone: Vec<u64> = self
            .agents
            .waits
            .iter()
            .filter(|(_, wait)| wait.connection == connection)
            .map(|(id, _)| *id)
            .collect();
        for id in gone {
            let error = failed("the connection stopped sending while it waited".into());
            self.finish_wait(id, Err(error));
        }
    }

    /// Answers a wait and forgets it. Whoever gets here first answers:
    /// the wait is taken out of the table, and with it its timers and
    /// what it watched.
    fn finish_wait(&mut self, id: u64, result: Result<AgentsWaited, ApiError>) {
        if let Some(wait) = self.agents.waits.remove(&id) {
            wait.reply.send(result);
        }
    }
}

impl Wait {
    /// The agents of the wait as they are now.
    fn states(&self, cx: &App) -> Vec<WaitedAgent> {
        self.targets
            .iter()
            .map(|target| {
                let pane = api_id(target.pane);
                let status = target.view.upgrade().map(|view| {
                    let view = view.read(cx);
                    (agent_state(pane, view.agent_status()), view.has_exited())
                });
                match status {
                    Some((status, false)) => WaitedAgent {
                        name: target.name.clone(),
                        pane,
                        closed: false,
                        state: status.state,
                        reason: status.reason,
                    },
                    // The program has exited or the terminal is gone.
                    _ => WaitedAgent {
                        name: target.name.clone(),
                        pane,
                        closed: true,
                        state: AgentActivity::Unknown,
                        reason: None,
                    },
                }
            })
            .collect()
    }
}

/// The text a pane is given for an inbox message: between tags that say who
/// sent it (and, for peer messages, the kind and round). What the message
/// itself says cannot end the tag, and carries no control characters and
/// none that change the direction of the text. For a
/// program that cannot tell a pasted line break from Enter, the whole is
/// one line.
fn framed(item: &InboxItem, marked_paste: bool) -> String {
    framed_parts(
        &item.from_name,
        item.from_pane,
        &item.text,
        &item.kind,
        marked_paste,
    )
}

fn framed_parts(
    from_name: &str,
    from_pane: u64,
    text: &str,
    kind: &InboxKind,
    marked_paste: bool,
) -> String {
    let body: String = text
        .chars()
        .filter(|ch| !ch.is_control() || matches!(ch, '\n' | '\t'))
        .filter(|ch| !changes_direction(*ch))
        .collect();
    let body = without_closing_tag(&body);
    let (attrs, body) = match kind {
        InboxKind::Report { status } => {
            // The status comes first, so the parent knows what the work came
            // to before it reads on.
            let body = match status {
                Some(status) => format!("{}: {}", status_word(*status), body.trim_start()),
                None => body,
            };
            (format!("from={from_name} pane={from_pane}"), body)
        }
        InboxKind::Peer { round, max_rounds } => (
            format!("from={from_name} pane={from_pane} kind=peer round={round}/{max_rounds}"),
            body,
        ),
        InboxKind::PeerLimit => (
            format!("from={from_name} pane={from_pane} kind=peer_limit"),
            body,
        ),
    };
    if marked_paste {
        format!("<benterm {attrs}>\n{}\n</benterm>", body.trim())
    } else {
        let line = body.split_whitespace().collect::<Vec<_>>().join(" ");
        format!("<benterm {attrs}> {line} </benterm>")
    }
}

/// Whether `ch` is one of the characters that set the direction of the
/// text around them (U+202E and the like). They are not shown, and with
/// them a line shows in another order than a program reads it in.
pub(crate) fn changes_direction(ch: char) -> bool {
    matches!(
        ch,
        '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'
    )
}

/// `text` with a blank put into every `</benterm`, in capitals or not, so
/// that nothing in it reads as the end of the tag around it.
fn without_closing_tag(text: &str) -> String {
    const TAG: &[u8] = b"</benterm";
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut copied = 0;
    let mut at = 0;
    while at + TAG.len() <= bytes.len() {
        if bytes[at..at + TAG.len()].eq_ignore_ascii_case(TAG) {
            // `</` is ASCII, so both cuts are between characters.
            out.push_str(&text[copied..at + 2]);
            out.push(' ');
            copied = at + 2;
            at += TAG.len();
        } else {
            at += 1;
        }
    }
    out.push_str(&text[copied..]);
    out
}

/// The word a report with `status` starts with in the parent's pane, as
/// the swarm of pstack has its workers start theirs.
fn status_word(status: ReportStatus) -> &'static str {
    match status {
        ReportStatus::Pass => "PASS",
        ReportStatus::Issues => "ISSUES",
        ReportStatus::Blocked => "BLOCKED",
    }
}

/// A name is what `AgentTarget` takes for one: not a number, and nothing
/// a shell or the report's tag would read as more than a word.
fn check_name(name: &str) -> Result<(), ApiError> {
    let word = name
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'));
    let number = name.chars().all(|ch| ch.is_ascii_digit());
    if name.is_empty() || name.len() > NAME_LIMIT || !word || number {
        return Err(invalid(format!(
            "a name is 1 to {NAME_LIMIT} letters, digits, '-', '_' or '.', and not a number: \
             {name:?}"
        )));
    }
    Ok(())
}

/// Only forward options that cannot select a different sandbox or workspace.
fn codex_prompt_index(args: &[std::ffi::OsString]) -> Option<usize> {
    let mut index = 0;
    while let Some(arg) = args.get(index) {
        match arg.to_str()? {
            "--no-daemon" | "--no-alt-screen" => index += 1,
            "-m" | "--model" => {
                let model = args.get(index + 1)?.to_str()?;
                if model.starts_with('-') {
                    return None;
                }
                index += 2;
            }
            "--sandbox" | "-s" if args.get(index + 1)?.to_str()? == "workspace-write" => {
                index += 2;
            }
            "--" if args.len() <= index + 2 => return Some(index),
            prompt if !prompt.starts_with('-') && args.len() == index + 1 => return Some(index),
            _ => return None,
        }
    }
    Some(index)
}

fn configure_mailbox(program: &mut PtyConfig, cwd: Option<&Path>) -> Option<PathBuf> {
    if program.program.as_deref().and_then(Path::file_name)? != "codex" {
        return None;
    }
    let prompt = codex_prompt_index(&program.args)?;
    let workdir = cwd?.canonicalize().ok()?;
    let state = benterm_session::session_path()?.parent()?.to_owned();
    std::fs::create_dir_all(&state).ok()?;
    let state = state.canonicalize().ok()?;
    // The working directory must grant no access to sibling mailboxes.
    if state.starts_with(&workdir) || workdir.starts_with(&state) {
        return None;
    }
    let dir = crate::mailbox::prepare_mailbox(&state).ok()?;
    let Some(root) = dir.to_str() else {
        crate::mailbox::cleanup_mailbox(&dir);
        return None;
    };
    let roots = serde_json::to_string(&[root]).expect("string array serializes");
    if let Some(arg) = program.args.get(prompt)
        && arg != "--"
    {
        program.args.insert(prompt, "--".into());
    }
    program.args.splice(
        0..0,
        [
            "-c".into(),
            "sandbox_mode=\"workspace-write\"".into(),
            "-c".into(),
            format!("sandbox_workspace_write.writable_roots={roots}").into(),
        ],
    );
    Some(dir)
}

/// What the agent's terminal runs, including its optional follow-up shell.
pub(crate) fn program(
    request: &SpawnAgent,
    shell: &PtyConfig,
    login_path: Option<std::ffi::OsString>,
) -> Result<PtyConfig, ApiError> {
    agent_program(request, shell, login_path)
        .map(|program| with_shell_after(program, request, shell))
}

fn with_shell_after(mut program: PtyConfig, request: &SpawnAgent, shell: &PtyConfig) -> PtyConfig {
    if request.shell_after && !request.command.is_empty() {
        let given = std::mem::replace(&mut program.env, shell.env.clone());
        program.env.extend(given);
        return crate::roles::then_shell(program, &crate::roles::shell_argv(shell));
    }
    program
}

fn agent_program(
    request: &SpawnAgent,
    shell: &PtyConfig,
    login_path: Option<std::ffi::OsString>,
) -> Result<PtyConfig, ApiError> {
    let env = |pty: &mut PtyConfig| {
        pty.env.push((
            benterm_ipc::AGENT_NAME_ENV.into(),
            request.name.clone().into(),
        ));
        if let Some(parent) = request.parent {
            pty.env.push((
                benterm_ipc::PARENT_PANE_ID_ENV.into(),
                parent.get().to_string().into(),
            ));
        }
        pty.env.extend(
            request
                .env
                .iter()
                .map(|(name, value)| (name.into(), value.into())),
        );
    };
    let Some((name, args)) = request.command.split_first() else {
        let mut pty = shell.clone();
        env(&mut pty);
        return Ok(pty);
    };
    // A terminal looks a program up on the `PATH` of the user's shell, or
    // the app's before that has come. The caller's is the one the command
    // was written for.
    let given = request.env.iter().rev().find(|(name, _)| name == "PATH");
    let path = match given {
        Some((_, path)) => Some(path.into()),
        None => login_path.or_else(|| std::env::var_os("PATH")),
    };
    // Only the absolute entries are looked in. A relative or an empty one
    // is the directory the agent starts in, or one inside it: a
    // repository, which could hold a program with the name of an agent.
    let found = if name.contains('/') {
        Some(PathBuf::from(name))
    } else {
        path.as_ref().and_then(|path| {
            std::env::split_paths(path)
                .filter(|dir| dir.is_absolute())
                .find_map(|dir| executable(&dir.join(name)))
        })
    };
    let Some(found) = found else {
        return Err(failed(format!("there is no program {name:?} on the PATH")));
    };
    let mut pty = PtyConfig::command(found, args.iter().cloned());
    env(&mut pty);
    Ok(pty)
}

pub(crate) fn executable(path: &Path) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let metadata = std::fs::metadata(path).ok()?;
    (metadata.is_file() && metadata.permissions().mode() & 0o111 != 0).then(|| path.to_owned())
}

fn invalid(message: String) -> ApiError {
    ApiError::new(ErrorCode::InvalidParams, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use benterm_ipc::PaneId as ApiPaneId;

    #[test]
    fn program_injects_agent_name_and_parent_pane_id_env() {
        let shell = PtyConfig::command("/bin/sh", std::iter::empty::<&str>());
        let with_parent = SpawnAgent {
            name: "worker-a".into(),
            parent: Some(ApiPaneId::new(7)),
            ..SpawnAgent::default()
        };
        let pty = program(&with_parent, &shell, None).unwrap();
        assert!(
            pty.env
                .iter()
                .any(|(k, v)| k == benterm_ipc::AGENT_NAME_ENV && v == "worker-a"),
            "expected BENTERM_AGENT_NAME=worker-a in {:?}",
            pty.env
        );
        assert!(
            pty.env
                .iter()
                .any(|(k, v)| k == benterm_ipc::PARENT_PANE_ID_ENV && v == "7"),
            "expected BENTERM_PARENT_PANE_ID=7 in {:?}",
            pty.env
        );

        let without_parent = SpawnAgent {
            name: "solo".into(),
            parent: None,
            ..SpawnAgent::default()
        };
        let pty = program(&without_parent, &shell, None).unwrap();
        assert!(
            pty.env
                .iter()
                .any(|(k, v)| k == benterm_ipc::AGENT_NAME_ENV && v == "solo")
        );
        assert!(
            !pty.env
                .iter()
                .any(|(k, _)| k == benterm_ipc::PARENT_PANE_ID_ENV)
        );
    }

    #[test]
    fn peer_authorization_enforces_same_parent_and_both_opt_in() {
        // User or external -> agent: Direct
        assert_eq!(authorize_send(None, None, 10), Ok(SendRoute::Direct));
        // Parent agent -> child agent: Direct
        let parent_agent = PeerPolicy {
            pane: 1,
            parent: None,
            allow_peers: false,
            max_peer_rounds: 10,
        };
        let child_agent = PeerPolicy {
            pane: 2,
            parent: Some(1),
            allow_peers: false,
            max_peer_rounds: 10,
        };
        assert_eq!(
            authorize_send(Some(parent_agent), Some(child_agent), 2),
            Ok(SendRoute::Direct)
        );
        // Child agent -> parent pane: Direct
        assert_eq!(
            authorize_send(Some(child_agent), Some(parent_agent), 1),
            Ok(SendRoute::Direct)
        );
        // Siblings both allow_peers with same parent: Peer
        let sib_a = PeerPolicy {
            pane: 2,
            parent: Some(1),
            allow_peers: true,
            max_peer_rounds: 8,
        };
        let sib_b = PeerPolicy {
            pane: 3,
            parent: Some(1),
            allow_peers: true,
            max_peer_rounds: 5,
        };
        assert_eq!(
            authorize_send(Some(sib_a), Some(sib_b), 3),
            Ok(SendRoute::Peer {
                parent: 1,
                max_rounds: 5,
            })
        );
        // One sibling without allow_peers: PeerNotAllowed
        let sib_no = PeerPolicy {
            allow_peers: false,
            ..sib_b
        };
        assert_eq!(
            authorize_send(Some(sib_a), Some(sib_no), 3),
            Err(ErrorCode::PeerNotAllowed)
        );
        assert_eq!(
            authorize_send(Some(sib_no), Some(sib_a), 2),
            Err(ErrorCode::PeerNotAllowed)
        );
        // Different parents: PeerNotAllowed
        let cousin = PeerPolicy {
            pane: 4,
            parent: Some(99),
            allow_peers: true,
            max_peer_rounds: 10,
        };
        assert_eq!(
            authorize_send(Some(sib_a), Some(cousin), 4),
            Err(ErrorCode::PeerNotAllowed)
        );
        // Root agents (parent: None) cannot peer-send: PeerNotAllowed
        let root_a = PeerPolicy {
            pane: 10,
            parent: None,
            allow_peers: true,
            max_peer_rounds: 10,
        };
        let root_b = PeerPolicy {
            pane: 11,
            parent: None,
            allow_peers: true,
            max_peer_rounds: 10,
        };
        assert_eq!(
            authorize_send(Some(root_a), Some(root_b), 11),
            Err(ErrorCode::PeerNotAllowed)
        );
        // Self-send: PeerNotAllowed
        assert_eq!(
            authorize_send(Some(sib_a), Some(sib_a), 2),
            Err(ErrorCode::PeerNotAllowed)
        );
    }

    #[test]
    fn framed_formats_peer_and_peer_limit_and_sanitizes_control_and_closing_tag() {
        let peer_kind = InboxKind::Peer {
            round: 2,
            max_rounds: 10,
        };
        assert_eq!(
            framed_parts("alice", 2, "hi\x07 </benterm>", &peer_kind, true),
            "<benterm from=alice pane=2 kind=peer round=2/10>\nhi </ benterm>\n</benterm>"
        );
        assert_eq!(
            framed_parts("alice", 2, "hi\x07 </benterm>", &peer_kind, false),
            "<benterm from=alice pane=2 kind=peer round=2/10> hi </ benterm> </benterm>"
        );

        assert_eq!(
            framed_parts(
                "system",
                0,
                "Sibling conversation reached round limit (10/10).",
                &InboxKind::PeerLimit,
                false,
            ),
            "<benterm from=system pane=0 kind=peer_limit> Sibling conversation reached round limit (10/10). </benterm>"
        );
    }

    #[test]
    fn framed_text_cannot_end_the_tag_in_capitals_or_reorder_what_is_shown() {
        let report = InboxKind::Report { status: None };
        assert_eq!(
            framed_parts("w1", 3, "a </BENTERM> b </BenTerm from=x>", &report, false),
            "<benterm from=w1 pane=3> a </ BENTERM> b </ BenTerm from=x> </benterm>"
        );
        // The characters that change the direction of the text, with
        // which a line shows in another order than it is read in.
        let bidi = "x\u{202a}\u{202b}\u{202c}\u{202d}\u{202e}\u{2066}\u{2067}\u{2068}\u{2069}\u{200e}\u{200f}\u{061c}y";
        assert_eq!(
            framed_parts("w1", 3, bidi, &report, true),
            "<benterm from=w1 pane=3>\nxy\n</benterm>"
        );
        // Text in a script that is written from right to left stays.
        assert_eq!(
            framed_parts("w1", 3, "שלום", &report, false),
            "<benterm from=w1 pane=3> שלום </benterm>"
        );
    }

    #[test]
    fn hold_predicate_holds_working_only_for_peer_messages() {
        let peer = InboxKind::Peer {
            round: 1,
            max_rounds: 10,
        };
        let report = InboxKind::Report { status: None };
        let limit = InboxKind::PeerLimit;

        assert!(should_hold(&peer, false, AgentState::Working));
        assert!(should_hold(
            &peer,
            false,
            AgentState::Waiting(WaitReason::Permission)
        ));
        assert!(should_hold(&peer, true, AgentState::Done));
        assert!(!should_hold(&peer, false, AgentState::Done));
        assert!(!should_hold(&peer, false, AgentState::Error));

        assert!(!should_hold(&report, false, AgentState::Working));
        assert!(should_hold(
            &report,
            false,
            AgentState::Waiting(WaitReason::Question)
        ));
        assert!(should_hold(&report, true, AgentState::Done));

        assert!(!should_hold(&limit, false, AgentState::Working));
        assert!(should_hold(&limit, true, AgentState::Done));
    }

    #[test]
    fn a_report_is_for_an_agent_in_front_that_has_shown_its_state() {
        for state in [AgentState::Working, AgentState::Done, AgentState::Error] {
            assert!(takes_report(Some("claude"), state));
            assert!(takes_report(Some("codex"), state));
            // A shell, or a program that could not be read.
            assert!(!takes_report(None, state));
        }
        assert!(!takes_report(Some("claude"), AgentState::Unknown));
        assert!(!takes_report(None, AgentState::Unknown));
    }

    #[test]
    fn wait_is_not_there_while_target_has_pending_inbox_messages() {
        let until = [
            AgentActivity::Done,
            AgentActivity::Waiting,
            AgentActivity::Error,
        ];
        assert!(!is_agent_there(
            false,
            AgentActivity::Done,
            &until,
            false,
            true
        ));
        assert!(is_agent_there(
            false,
            AgentActivity::Done,
            &until,
            false,
            false
        ));
        assert!(!is_agent_there(
            false,
            AgentActivity::Done,
            &until,
            true,
            false
        ));
        assert!(is_agent_there(
            true,
            AgentActivity::Unknown,
            &until,
            false,
            true
        ));
    }
}

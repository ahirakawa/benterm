//! The history of the agents' states of a window: which agent was in
//! which state in which pane, and from when.
//!
//! The window takes the agent news of its panes from the
//! [`AgentFeed`] and records every change with
//! [`benterm_session::history`], which keeps the records in a file
//! beside the session file when the window holds the session's lock. A
//! window that does not, as the second of two apps, reads what the file
//! held when it opened and keeps its own records in memory.
//!
//! A pane is named in the history by the key of its scrollback, which
//! the session file has for it, so that the pane made again after a
//! restart is the same pane. A pane that has no key yet gets it with
//! its first record. With no scrollback kept, the session file has no
//! keys, and a pane's key lasts until the app quits.
//!
//! When a pane closes, and when the window closes or the app quits, the
//! agents are recorded as gone, so that no state lasts longer than the
//! agent that was in it.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::Path;
use std::rc::Rc;
use std::time::SystemTime;

use benterm_agent::{AgentState, WaitReason};
use benterm_ipc::{AgentActivity, AgentHistory, AgentRecord, ReadAgentHistory, WaitingFor};
use benterm_layout::PaneId;
use benterm_session::history::{Change, History, RETENTION, Record, State, Waiting};
use benterm_session::{Agent, new_scrollback_key};
use futures::StreamExt;
use futures::channel::mpsc::unbounded;
use gpui::{App, Context, Task, Window};

use crate::progress_board::LanePlace;
use crate::{AgentFeed, AgentNews, BentermWindow, MainView};

/// What the panes told and the window has not recorded yet.
type Pending = Rc<RefCell<Vec<(PaneId, AgentNews, SystemTime)>>>;

/// Records the agent states of a window's panes.
pub(crate) struct AgentRecorder {
    history: History,
    /// The key of each pane that has a record, until the pane closes.
    keys: HashMap<PaneId, String>,
    pending: Pending,
    /// The window is closing: the agents were recorded as gone, and
    /// nothing is recorded after that.
    finished: bool,
    _task: Option<Task<()>>,
}

impl Default for AgentRecorder {
    fn default() -> Self {
        Self {
            history: History::unsaved(RETENTION),
            keys: HashMap::new(),
            pending: Pending::default(),
            finished: false,
            _task: None,
        }
    }
}

impl AgentRecorder {
    /// The key the history has for `pane`, if the pane has a record.
    pub(crate) fn key(&self, pane: PaneId) -> Option<&str> {
        self.keys.get(&pane).map(String::as_str)
    }
}

impl BentermWindow {
    /// Records the agent news of the window's panes from `feed`, in the
    /// history beside the session file at `session`. The history is
    /// written if the window saves its session, and only read if not.
    pub fn record_agents(
        &mut self,
        feed: &AgentFeed,
        session: Option<&Path>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let now = SystemTime::now();
        self.agent_history.history = match (self.session_store(), session) {
            (Some(store), _) => History::open(store, RETENTION, now),
            (None, Some(session)) => History::read_only(session, RETENTION, now),
            (None, None) => History::unsaved(RETENTION),
        };
        let pending = self.agent_history.pending.clone();
        let (wake, mut woken) = unbounded();
        feed.subscribe(Box::new(move |pane, news, at| {
            // The window may be in the middle of an update of its own,
            // so the news waits here until the window takes it.
            pending.borrow_mut().push((pane, news, at));
            let _ = wake.unbounded_send(());
        }));
        self.agent_history._task = Some(cx.spawn_in(window, async move |this, cx| {
            while woken.next().await.is_some() {
                let recorded = this.update(cx, |this, cx| {
                    for pane in this.record_agent_news() {
                        this.keep_closed_node(pane, cx);
                    }
                    this.remember_graph(cx);
                    this.sync_board(cx);
                });
                if recorded.is_err() {
                    break;
                }
            }
        }));
    }

    /// Records what the panes have told since the last time, and
    /// returns the panes that have closed.
    fn record_agent_news(&mut self) -> Vec<PaneId> {
        let news = std::mem::take(&mut *self.agent_history.pending.borrow_mut());
        let closed = news.iter();
        let closed: Vec<PaneId> = closed
            .filter(|(_, news, _)| matches!(news, AgentNews::Closed))
            .map(|(pane, _, _)| *pane)
            .collect();
        if self.agent_history.finished {
            return closed;
        }
        for (pane, news, at) in news {
            match news {
                AgentNews::Changed(status) => {
                    // An agent the history has no name for is none. A
                    // program that is no agent has a state too, when it
                    // tells one itself (OSC 7501).
                    let agent = status.agent.and_then(Agent::from_id);
                    let state = match status.state {
                        AgentState::Unknown => State::Unknown,
                        AgentState::Working => State::Working,
                        AgentState::Waiting(WaitReason::Permission | WaitReason::Trust) => {
                            State::Waiting(Waiting::Permission)
                        }
                        AgentState::Waiting(WaitReason::Question | WaitReason::Choice) => {
                            State::Waiting(Waiting::Question)
                        }
                        AgentState::Done => State::Done,
                        AgentState::Error => State::Error,
                    };
                    let nothing = agent.is_none() && state == State::Unknown;
                    if nothing && self.agent_history.key(pane).is_none() {
                        // Nothing was recorded of the pane, so there is
                        // nothing to end, and no key to make for it.
                        continue;
                    }
                    let Some(key) = self.history_key(pane) else {
                        continue;
                    };
                    let change = Change {
                        key,
                        pane: pane.get(),
                        agent,
                        state,
                    };
                    self.agent_history.history.record(change, at);
                }
                AgentNews::Closed => {
                    if let Some(key) = self.agent_history.keys.remove(&pane) {
                        self.agent_history.history.end(&key, at);
                    }
                }
            }
        }
        closed
    }

    /// The key of `pane` in the history: the one it has, or the key of
    /// its scrollback, or a new one. `None` without random bytes.
    fn history_key(&mut self, pane: PaneId) -> Option<String> {
        if let Some(key) = self.agent_history.keys.get(&pane) {
            return Some(key.clone());
        }
        let key = match self.scrollback_key(pane) {
            Some(key) => key,
            None => new_scrollback_key().ok()?,
        };
        self.agent_history.keys.insert(pane, key.clone());
        Some(key)
    }

    /// Records every agent of the window as gone, after what the panes
    /// told last. For when the window closes or the app quits; nothing
    /// is recorded afterwards.
    pub(crate) fn finish_agent_history(&mut self) {
        self.record_agent_news();
        if !self.agent_history.finished {
            self.agent_history.finished = true;
            self.agent_history.history.end_all(SystemTime::now());
        }
    }

    /// The changes of the agents' states in the panes of the window and
    /// of the app's earlier runs, oldest first: those of the retention
    /// time, a week. The times of the records go up, and no two are the
    /// same.
    pub fn agent_history(&self) -> Vec<Record> {
        self.agent_history.history.records(SystemTime::now())
    }

    /// Those of [`agent_history`](Self::agent_history) with a time
    /// after `at_ms`: what was recorded since the caller last asked,
    /// given the time of the last record it has.
    pub fn agent_history_since(&self, at_ms: u64) -> Vec<Record> {
        let now = SystemTime::now();
        self.agent_history.history.records_since(at_ms, now)
    }

    /// Hands the progress board, while it is shown, what was recorded
    /// since it last took records, and where the panes are now.
    pub(crate) fn sync_board(&mut self, cx: &mut Context<Self>) {
        if self.main_view() != MainView::ProgressBoard {
            return;
        }
        self.sync_graph(cx);
        let Some(board) = self.board.as_ref().map(|open| open.view.clone()) else {
            return;
        };
        let records = match board.read(cx).last_record_ms() {
            Some(at_ms) => self.agent_history_since(at_ms),
            None => self.agent_history(),
        };
        let places = self.lane_places(cx);
        let repository = self.board_repository(cx);
        let tasks = self.board_tasks(repository.as_deref(), cx);
        let branches = repository
            .as_deref()
            .map_or_else(Vec::new, |repo| self.board_branches(repo));
        let store = self.board_task_store();
        board.update(cx, |board, cx| {
            board.take_records(records, places, cx);
            board.take_tasks(tasks, cx);
            board.sync_pull_requests(repository, branches, store, cx);
        });
    }

    /// Where each pane that has a record is: in which workspace, or
    /// which agent behind the window it is.
    fn lane_places(&self, cx: &App) -> HashMap<String, LanePlace> {
        let behind = self.agents(cx);
        self.agent_history
            .keys
            .iter()
            .filter_map(|(pane, key)| {
                let name = match self.place_of(*pane, cx) {
                    Some((workspace, _)) => self.workspace_name(workspace)?,
                    None => behind.iter().find(|row| row.pane == *pane)?.name.clone(),
                };
                let stalled_since_ms = self
                    .stall_mark(*pane)
                    .filter(|mark| mark.stalled)
                    .map(|mark| mark.last_change_ms);
                let place = LanePlace {
                    pane: *pane,
                    name,
                    stalled_since_ms,
                };
                Some((key.clone(), place))
            })
            .collect()
    }

    /// `agent.history`.
    pub(crate) fn read_agent_history(&mut self, request: &ReadAgentHistory) -> AgentHistory {
        // What a hook reported before this call is in the answer.
        self.record_agent_news();
        let since = request.since_ms.unwrap_or(0);
        let records = self.agent_history_since(since);
        AgentHistory {
            records: records.into_iter().map(api_record).collect(),
        }
    }
}

fn api_record(record: Record) -> AgentRecord {
    let (state, reason) = match record.state {
        State::Working => (AgentActivity::Working, None),
        State::Waiting(Waiting::Permission) => {
            (AgentActivity::Waiting, Some(WaitingFor::Permission))
        }
        State::Waiting(Waiting::Question) => (AgentActivity::Waiting, Some(WaitingFor::Question)),
        State::Done => (AgentActivity::Done, None),
        State::Error => (AgentActivity::Error, None),
        State::Unknown => (AgentActivity::Unknown, None),
    };
    AgentRecord {
        at_ms: record.at_ms,
        key: record.key,
        pane: benterm_ipc::PaneId::new(record.pane),
        agent: record.agent.map(|agent| agent.id().to_owned()),
        state,
        reason,
    }
}

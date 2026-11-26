//! What a window keeps for the graph of the progress board: the messages
//! its terminals gave each other, and the agents whose terminals have
//! closed.
//!
//! The nodes are the terminals of the window as they are now. A terminal
//! is a node when an agent is recognised in it, when it was started
//! under a name, or when it started another or has a message to or from
//! another. An agent whose terminal closes stays for a while as a closed
//! node, so that what it reported can still be seen.
//!
//! All of it is kept in memory and ends with the window. Nothing is read
//! from outside the app.

use std::collections::{HashSet, VecDeque};
use std::time::SystemTime;

use benterm_agent::AgentState;
use benterm_layout::PaneId;
use gpui::{App, Context};

use crate::BentermWindow;
use crate::graph::{Message, MessageKind, Messages, Node};
use crate::session_rows::{SessionEntry, SessionPlace};
use crate::timeline::BandState;

/// How long a closed node is kept.
const KEPT_MS: u64 = 30 * 60 * 1000;
/// How many closed nodes are kept.
const MOST_CLOSED: usize = 64;

#[derive(Default)]
pub(crate) struct AgentGraph {
    messages: Messages,
    /// The nodes of closed terminals with the time they closed, oldest
    /// first.
    closed: VecDeque<(Node, u64)>,
    /// The nodes of the terminals that ran when an agent last told
    /// something, for what a terminal was when it closes.
    last: Vec<Node>,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |since| since.as_millis() as u64)
}

/// The state a node shows for the state of an agent.
fn band(agent: Option<&str>, state: AgentState) -> Option<BandState> {
    agent?;
    match state {
        AgentState::Working => Some(BandState::Working),
        AgentState::Waiting(_) => Some(BandState::Waiting),
        AgentState::Done => Some(BandState::Done),
        AgentState::Error => Some(BandState::Error),
        AgentState::Unknown => None,
    }
}

impl BentermWindow {
    /// The messages the terminals of the window gave each other with
    /// `agent send` and `agent report`, oldest first: the latest of them,
    /// since the window opened.
    pub fn agent_messages(&self) -> Vec<Message> {
        self.graph.messages.iter().cloned().collect()
    }

    /// The nodes of the graph: the terminals of every workspace and the
    /// ones behind the window that are agents or have to do with one, in
    /// the order of the session list, and then the closed ones.
    pub fn agent_graph(&self, cx: &App) -> Vec<Node> {
        let entries = self.session_entries(false, cx);
        let mut linked: HashSet<u64> = HashSet::new();
        linked.extend(entries.iter().filter_map(|e| Some(e.parent?.get())));
        let now = now_ms();
        let closed = self.graph.closed.iter();
        // A terminal that still runs is no closed node, whatever was
        // told of it.
        let runs: HashSet<u64> = entries.iter().map(|entry| entry.pane.get()).collect();
        let closed: Vec<&Node> = closed
            .filter(|(node, at_ms)| {
                now.saturating_sub(*at_ms) < KEPT_MS && !runs.contains(&node.id)
            })
            .map(|(node, _)| node)
            .collect();
        linked.extend(closed.iter().filter_map(|node| node.parent));
        for message in self.graph.messages.iter() {
            linked.extend([message.from, message.to]);
        }
        let mut nodes: Vec<Node> = entries
            .into_iter()
            .filter(|entry| {
                entry.agent.is_some() || entry.name.is_some() || linked.contains(&entry.pane.get())
            })
            .map(|entry| self.node(entry))
            .collect();
        nodes.extend(closed.into_iter().cloned());
        nodes
    }

    fn node(&self, entry: SessionEntry) -> Node {
        let name = entry.name.unwrap_or_else(|| match entry.place {
            SessionPlace::Pane { workspace, .. } => {
                self.workspace_name(workspace).unwrap_or_default()
            }
            SessionPlace::Background => format!("pane {}", entry.pane),
        });
        Node {
            id: entry.pane.get(),
            parent: entry.parent.map(PaneId::get),
            name,
            agent: entry.agent,
            state: band(entry.agent, entry.state),
            closed: false,
            background: entry.place == SessionPlace::Background,
        }
    }

    /// Keeps the nodes of the terminals that run now, whether the board
    /// is shown or not: a terminal that closes is drawn as it was last.
    pub(crate) fn remember_graph(&mut self, cx: &App) {
        let nodes = self.agent_graph(cx);
        self.graph.last = nodes.into_iter().filter(|node| !node.closed).collect();
    }

    /// Hands the board the graph as it is now, if the board is shown.
    pub(crate) fn sync_graph(&mut self, cx: &mut Context<Self>) {
        self.remember_graph(cx);
        let Some(board) = self.progress_board() else {
            return;
        };
        let nodes = self.agent_graph(cx);
        let messages = self.agent_messages();
        board.update(cx, |board, cx| board.take_graph(nodes, messages, cx));
    }

    /// Keeps what `from` gave `to`, and shows it in the board.
    pub(crate) fn keep_message(
        &mut self,
        kind: MessageKind,
        from: PaneId,
        to: PaneId,
        text: &str,
        cx: &mut Context<Self>,
    ) {
        let messages = &mut self.graph.messages;
        messages.push(kind, from.get(), to.get(), now_ms(), text);
        self.sync_graph(cx);
    }

    /// The terminal with the id `pane` is closing: if it is a node, it
    /// stays as a closed one. Called before the window forgets the agent.
    pub(crate) fn keep_closed_node(&mut self, pane: PaneId, cx: &App) {
        let id = pane.get();
        if self.graph.closed.iter().any(|(node, _)| node.id == id) {
            return;
        }
        // What the board showed last, or what the window still knows.
        let last = self.graph.last.iter().find(|node| node.id == id).cloned();
        let node = last.or_else(|| {
            let agent = self.agents(cx).into_iter().find(|row| row.pane == pane)?;
            Some(Node {
                id,
                parent: agent.parent.map(PaneId::get),
                name: agent.name,
                agent: None,
                state: None,
                closed: false,
                background: agent.background,
            })
        });
        let Some(mut node) = node else {
            return;
        };
        node.closed = true;
        let now = now_ms();
        let closed = &mut self.graph.closed;
        closed.retain(|(_, at_ms)| now.saturating_sub(*at_ms) < KEPT_MS);
        if closed.len() == MOST_CLOSED {
            closed.pop_front();
        }
        closed.push_back((node, now));
    }
}

//! The receivers in the app of what the agents of a window's panes do.
//!
//! Every pane's [`AgentListener`](crate::AgentListener) hands its news to
//! one feed, which passes it on to whoever subscribed, with the pane and
//! the time the news came.

use std::cell::RefCell;
use std::time::SystemTime;

use benterm_layout::PaneId;

use crate::AgentNews;

/// Called on the UI thread, inside an update of the pane's view, which
/// may itself run inside an update of the window. It must not wait for
/// anything, nor update the window: one that has to sends the news on.
pub type FeedListener = Box<dyn FnMut(PaneId, AgentNews, SystemTime)>;

/// Passes on the agent news of every pane of a window.
#[derive(Default)]
pub struct AgentFeed {
    listeners: RefCell<Vec<FeedListener>>,
}

impl AgentFeed {
    /// Has `listener` told every later news of every pane.
    pub fn subscribe(&self, listener: FeedListener) {
        self.listeners.borrow_mut().push(listener);
    }

    /// Tells every listener that `news` came from `pane` at `at`.
    pub fn publish(&self, pane: PaneId, news: AgentNews, at: SystemTime) {
        for listener in self.listeners.borrow_mut().iter_mut() {
            listener(pane, news, at);
        }
    }
}

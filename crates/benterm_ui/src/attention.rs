//! The attention marks: a pane whose agent has finished or waits for the
//! user, or whose program asked for a notification, is marked until the
//! user looks at it, and a macOS notification is posted for it.
//!
//! The user looks at a pane when the window is active and the pane is
//! the focused one of the selected tab of the selected workspace.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::{Duration, SystemTime};

use benterm_agent::{AgentState, WaitReason, builtin};
use benterm_layout::{PaneId, TabId, WorkspaceId};
use benterm_terminal::Notification;
use futures::StreamExt;
use futures::channel::mpsc::unbounded;
use gpui::{App, Context, SystemNotification, Task, Window};

use crate::{APP_NAME, AgentFeed, AgentNews, BentermWindow, MainView};

/// What happened in a pane that the user should see.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttentionEvent {
    pub pane: PaneId,
    pub kind: AttentionKind,
    /// When the app learned of it.
    pub at: SystemTime,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttentionKind {
    /// The agent finished its turn. `agent` is the id of its definition,
    /// `claude` or `codex`.
    Done { agent: Option<&'static str> },
    /// The agent waits for the user.
    Waiting {
        agent: Option<&'static str>,
        reason: WaitReason,
    },
    /// The program asked for a notification with OSC 9 or OSC 777.
    Osc { title: Option<String>, body: String },
}

/// A second event of a pane this soon after the one that was notified
/// marks the pane without a notification of its own. An agent that
/// reports the end of its turn by a hook and by OSC 9 is notified once.
pub const REPEAT_WINDOW: Duration = Duration::from_secs(3);

/// Which panes are marked, and which one the user looks at.
#[derive(Debug, Default)]
pub struct Attention {
    marked: HashSet<PaneId>,
    viewing: Option<PaneId>,
    /// When each pane was last notified.
    notified: HashMap<PaneId, SystemTime>,
}

impl Attention {
    /// Takes an event. Returns whether to post a notification for it.
    ///
    /// The pane the user looks at is not marked and not notified.
    pub fn take(&mut self, event: &AttentionEvent) -> bool {
        if self.viewing == Some(event.pane) {
            return false;
        }
        self.marked.insert(event.pane);
        let repeated = self.notified.get(&event.pane).is_some_and(|last| {
            event
                .at
                .duration_since(*last)
                .is_ok_and(|since| since < REPEAT_WINDOW)
        });
        if repeated {
            return false;
        }
        self.notified.insert(event.pane, event.at);
        true
    }

    /// The user now looks at `pane`, or at none. Returns whether a mark
    /// went.
    pub fn set_viewing(&mut self, pane: Option<PaneId>) -> bool {
        self.viewing = pane;
        pane.is_some_and(|pane| self.marked.remove(&pane))
    }

    /// The pane the user looks at.
    pub fn viewing(&self) -> Option<PaneId> {
        self.viewing
    }

    pub fn is_marked(&self, pane: PaneId) -> bool {
        self.marked.contains(&pane)
    }

    pub fn marked(&self) -> &HashSet<PaneId> {
        &self.marked
    }

    /// Forgets a pane that closed. Returns whether it was marked.
    pub fn forget(&mut self, pane: PaneId) -> bool {
        self.notified.remove(&pane);
        if self.viewing == Some(pane) {
            self.viewing = None;
        }
        self.marked.remove(&pane)
    }
}

/// Posts the notifications of a window. The app posts with GPUI; a test
/// puts in one that keeps them.
pub trait Notifier {
    fn show(&self, notification: SystemNotification, cx: &mut App);
}

/// Posts with GPUI, which asks macOS for permission before the first one
/// and posts nothing outside an app bundle, such as under `cargo run`.
pub struct SystemNotifier;

impl Notifier for SystemNotifier {
    fn show(&self, notification: SystemNotification, cx: &mut App) {
        cx.show_system_notification(notification);
    }
}

/// What a window keeps for the attention marks.
pub(crate) struct WindowAttention {
    rules: Attention,
    notifier: Rc<dyn Notifier>,
    /// The notifications in a corner of the window, shown whether or not
    /// macOS shows the ones `notifier` posts.
    pub(crate) toasts: crate::toast::WindowToasts,
    /// Hands the agent news to the window outside the update it came in.
    _agents: Option<Task<()>>,
}

impl Default for WindowAttention {
    fn default() -> Self {
        Self {
            rules: Attention::default(),
            notifier: Rc::new(SystemNotifier),
            toasts: Default::default(),
            _agents: None,
        }
    }
}

const TAG_PREFIX: &str = "benterm-pane-";

/// Who a notification a program asked for with OSC 9 or OSC 777 says it
/// is from. The program chooses the rest of the words and may choose
/// those of an agent's notification, so this always comes first.
pub(crate) const PROGRAM_SOURCE: &str = "Terminal program";

/// The tag of the notifications of `pane`. A pane is notified under one
/// tag, so a newer notification of it takes the place of the older one.
/// The tag names the process too: pane ids start again with each run,
/// and a notification left from an earlier run must not lead to the pane
/// that has its id now.
pub fn tag_of(pane: PaneId) -> String {
    format!("{TAG_PREFIX}{}-{}", std::process::id(), pane.get())
}

/// The number of the pane a notification of this run was posted for,
/// as [`PaneId::get`] gives it.
pub fn pane_of_tag(tag: &str) -> Option<u64> {
    let (process, pane) = tag.strip_prefix(TAG_PREFIX)?.split_once('-')?;
    if process.parse::<u32>().ok()? != std::process::id() {
        return None;
    }
    pane.parse().ok()
}

/// Has a click on a notification bring the window of its terminal to the
/// front with the terminal in view: in its pane, or in the peek for an
/// agent behind the window. Call once, at the start of the app.
pub fn answer_notifications(cx: &mut App) {
    cx.on_system_notification_response(|response, cx| {
        let Some(number) = pane_of_tag(&response.tag) else {
            return;
        };
        for window in cx.windows() {
            let Some(window) = window.downcast::<BentermWindow>() else {
                continue;
            };
            let shown = window.update(cx, |root, window, cx| {
                let Some(pane) = root.pane_numbered(number, cx) else {
                    return false;
                };
                // In front first, so that the terminal shown is looked
                // at and loses its mark.
                window.activate_window();
                root.open_notified(pane, window, cx)
            });
            if shown.unwrap_or(false) {
                cx.activate(true);
                return;
            }
        }
    });
}

impl BentermWindow {
    /// Posts this window's notifications with `notifier` from now on.
    pub fn set_notifier(&mut self, notifier: Rc<dyn Notifier>) {
        self.attention.notifier = notifier;
    }

    /// Whether a pane of the window is marked.
    pub fn needs_attention(&self, pane: PaneId) -> bool {
        self.attention.rules.is_marked(pane)
    }

    /// Whether a pane of a workspace is marked.
    pub fn workspace_needs_attention(&self, id: WorkspaceId, cx: &App) -> bool {
        self.workspaces
            .get(id)
            .is_some_and(|workspace| workspace.content().tabs.read(cx).needs_attention(cx))
    }

    /// Takes the agent news of the window's panes from `feed`.
    pub fn follow_agents(&mut self, feed: &AgentFeed, window: &mut Window, cx: &mut Context<Self>) {
        let (sender, mut news) = unbounded();
        feed.subscribe(Box::new(move |pane, news, at| {
            // The window may be in the middle of an update of its own.
            let _ = sender.unbounded_send((pane, news, at));
        }));
        self.attention._agents = Some(cx.spawn_in(window, async move |this, cx| {
            while let Some((pane, news, at)) = news.next().await {
                let taken = this.update_in(cx, |this, window, cx| {
                    this.take_agent_news(pane, news, at, window, cx);
                    // After the marks, which the session list reads.
                    this.session_news(pane, window, cx);
                });
                if taken.is_err() {
                    break;
                }
            }
        }));
    }

    fn take_agent_news(
        &mut self,
        pane: PaneId,
        news: AgentNews,
        at: SystemTime,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let status = match news {
            AgentNews::Changed(status) => status,
            AgentNews::Closed => {
                self.forget_checked_report(pane, window, cx);
                if self.attention.rules.forget(pane) {
                    self.show_marks(cx);
                }
                return;
            }
        };
        // A program that is no agent and tells its state itself (OSC
        // 7501) is shown with it and asks for no attention: at rest it
        // has finished no turn.
        if status.agent.is_none() {
            return;
        }
        let kind = match status.state {
            // A turn that failed leaves the agent at its prompt and the
            // next step to the user, like one that went well.
            AgentState::Done | AgentState::Error => AttentionKind::Done {
                agent: status.agent,
            },
            AgentState::Waiting(reason) => AttentionKind::Waiting {
                agent: status.agent,
                reason,
            },
            AgentState::Working | AgentState::Unknown => return,
        };
        self.take_attention(AttentionEvent { pane, kind, at }, window, cx);
    }

    /// Takes a notification the program of a pane asked for.
    pub(crate) fn take_notification(
        &mut self,
        pane: PaneId,
        notification: Notification,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let kind = AttentionKind::Osc {
            title: notification.title,
            body: notification.body,
        };
        let at = SystemTime::now();
        self.take_attention(AttentionEvent { pane, kind, at }, window, cx);
    }

    fn take_attention(
        &mut self,
        event: AttentionEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // The focus may have moved since the window last looked.
        self.look(window, cx);
        let notify = self.attention.rules.take(&event);
        self.show_marks(cx);
        if notify {
            self.show_toast(&event, cx);
            let notification = self.notification(&event, cx);
            let notifier = self.attention.notifier.clone();
            notifier.show(notification, cx);
        }
    }

    /// Finds the pane the user looks at now and takes its mark away.
    pub(crate) fn look(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // The peek hides every pane and shows its own terminal. Another
        // view in the pane area hides every pane too.
        let pane = self.peeked().or_else(|| {
            (self.main_view() == MainView::Terminals).then(|| self.grid(cx).read(cx).focused())
        });
        let viewing = pane.filter(|_| window.is_window_active());
        if self.attention.rules.set_viewing(viewing) {
            self.show_marks(cx);
        }
    }

    /// The terminals that have the mark, in a pane or behind the window.
    pub(crate) fn marked_panes(&self) -> HashSet<PaneId> {
        let mut marked = self.attention.rules.marked().clone();
        // A pane that holds reports back until its draft is gone needs
        // the user too, also while the user looks at it.
        marked.extend(self.panes_holding_reports());
        marked
    }

    /// Hands the marks to the tabs and panes, which draw them. The
    /// session list reads them from [`Self::marked_panes`] when it is
    /// drawn.
    pub(crate) fn show_marks(&self, cx: &mut Context<Self>) {
        let marked = self.marked_panes();
        for workspace in self.workspaces.iter() {
            workspace
                .content()
                .tabs
                .update(cx, |tabs, cx| tabs.set_attention(&marked, cx));
        }
        cx.notify();
    }

    /// The terminal of the window whose [`PaneId::get`] is `number`, in a
    /// pane or behind the window.
    fn pane_numbered(&self, number: u64, cx: &App) -> Option<PaneId> {
        let id = benterm_ipc::PaneId::new(number);
        self.find_terminal(id, cx).ok().map(|(pane, _)| pane)
    }

    /// Shows the terminal a clicked notification is for: goes to its pane,
    /// or opens the peek of an agent behind the window, which has none.
    /// Returns whether the terminal is in this window.
    pub(crate) fn open_notified(
        &mut self,
        pane: PaneId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.show_pane(pane, window, cx) || self.peek(pane, window, cx)
    }

    /// The workspace and the tab a pane is in.
    pub(crate) fn place_of(&self, pane: PaneId, cx: &App) -> Option<(WorkspaceId, TabId)> {
        self.workspaces.iter().find_map(|workspace| {
            let tabs = workspace.content().tabs.read(cx);
            tabs.tabs()
                .into_iter()
                .find(|tab| {
                    tabs.grid(*tab)
                        .is_some_and(|grid| grid.read(cx).terminal(pane).is_some())
                })
                .map(|tab| (workspace.id(), tab))
        })
    }

    /// The notification of an event: what happened, and where: the
    /// workspace and the tab, or for an agent behind the window its name
    /// and its folder.
    fn notification(&self, event: &AttentionEvent, cx: &App) -> SystemNotification {
        let place = self
            .place_of(event.pane, cx)
            .map(|(workspace, tab)| {
                let name = self.workspace_name(workspace).unwrap_or_default();
                let label = self
                    .workspace_tabs(workspace)
                    .map(|tabs| tabs.read(cx).label(tab, cx))
                    .unwrap_or_default();
                format!("{name} › {label}")
            })
            .or_else(|| {
                // An agent behind the window is in no workspace or tab.
                let parts = self.behind_parts(event.pane, cx);
                (!parts.is_empty()).then(|| parts.join(" · "))
            })
            .unwrap_or_else(|| APP_NAME.into());
        let (title, body) = match &event.kind {
            AttentionKind::Done { agent } => (format!("{} finished", agent_name(*agent)), place),
            AttentionKind::Waiting {
                agent,
                reason: WaitReason::Permission | WaitReason::Trust,
            } => (format!("{} asks for permission", agent_name(*agent)), place),
            AttentionKind::Waiting {
                agent,
                reason: WaitReason::Question | WaitReason::Choice,
            } => (format!("{} has a question", agent_name(*agent)), place),
            // What the program wrote is all in the body: the title says
            // that a program of the terminal asked, and where.
            AttentionKind::Osc { title, body } => (
                format!("{PROGRAM_SOURCE} · {place}"),
                match title {
                    Some(title) if !title.is_empty() => format!("{title}: {body}"),
                    _ => body.clone(),
                },
            ),
        };
        SystemNotification {
            tag: tag_of(event.pane).into(),
            title: title.into(),
            body: body.into(),
            actions: Vec::new(),
        }
    }

    /// Selects the workspace and the tab of a pane and gives the pane the
    /// keyboard. Returns whether the pane is in this window.
    pub fn show_pane(&mut self, pane: PaneId, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some((workspace, tab)) = self.place_of(pane, cx) else {
            return false;
        };
        let Some(tabs) = self.workspace_tabs(workspace) else {
            return false;
        };
        // Going to a pane the peek shows ends the peek: the pane is
        // looked at where it is.
        if self.peeked() == Some(pane) {
            self.end_peek(cx);
        }
        tabs.update(cx, |tabs, cx| {
            if let Some(grid) = tabs.grid(tab).cloned() {
                grid.update(cx, |grid, cx| grid.focus(pane, window, cx));
            }
            tabs.select(tab, window, cx);
        });
        let _ = self.workspaces.select(workspace);
        self.focus_selected(window, cx);
        self.look(window, cx);
        true
    }
}

/// The name of an agent as the user knows it.
pub(crate) fn agent_name(agent: Option<&str>) -> &'static str {
    agent
        .and_then(|id| builtin().into_iter().find(|definition| definition.id == id))
        .map_or("The agent", |definition| definition.name)
}

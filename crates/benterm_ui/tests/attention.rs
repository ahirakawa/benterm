//! The attention marks in a window: an agent that finishes or waits, or a
//! program that asks for a notification, marks its pane, tab and
//! workspace until the user looks at the pane, and a notification is
//! posted when the user does not look at it.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use benterm_agent::{AgentState, HookEvent, WaitReason};
use benterm_layout::{PaneId, TabId, WorkspaceId};
use benterm_terminal::PtyConfig;
use benterm_ui::attention::{self, Notifier};
use benterm_ui::settings::ConfigFeed;
use benterm_ui::{
    BentermWindow, NewTab, NewWorkspace, PaneGrid, SplitRight, StartedTerminal, TabGroup,
    TerminalView, bind_keys, ipc,
};
use gpui::{
    App, Entity, SystemNotification, SystemNotificationResponse, TestAppContext, VisualTestContext,
    WindowHandle,
};

const TIMEOUT: Duration = Duration::from_secs(10);

/// Every pane runs a shell loop that runs each line it is sent, without
/// an echo, so a test can make a pane print what it likes.
const SCRIPT: &str = r#"stty -echo; printf ready; while IFS= read -r line; do eval "$line"; done"#;

/// Keeps what it was asked to post.
#[derive(Clone, Default)]
struct FakeNotifier(Rc<RefCell<Vec<SystemNotification>>>);

impl Notifier for FakeNotifier {
    fn show(&self, notification: SystemNotification, _: &mut App) {
        self.0.borrow_mut().push(notification);
    }
}

struct Benterm {
    window: WindowHandle<BentermWindow>,
    cx: VisualTestContext,
    sent: FakeNotifier,
}

/// Opens an active window whose notifications go to a fake notifier.
fn open(cx: &mut TestAppContext) -> Benterm {
    let mut benterm = open_with_system_notifier(cx);
    let sent = FakeNotifier::default();
    let notifier = Rc::new(sent.clone());
    benterm
        .window
        .update(&mut benterm.cx, |root, _, _| root.set_notifier(notifier))
        .unwrap();
    benterm.sent = sent;
    benterm
}

/// Opens an active window that posts with GPUI, as the app does.
fn open_with_system_notifier(cx: &mut TestAppContext) -> Benterm {
    cx.executor().allow_parking();
    cx.update(bind_keys);
    let start: ipc::StartTerminal = Rc::new(StartedTerminal::spawn);
    let window = cx
        .update(|cx| {
            ipc::open_window(
                PtyConfig::command("/bin/sh", ["-c", SCRIPT]),
                start,
                None,
                ConfigFeed::defaults(),
                cx,
            )
        })
        .unwrap();
    let mut cx = VisualTestContext::from_window(window.into(), cx);
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();
    let mut benterm = Benterm {
        window,
        cx,
        sent: FakeNotifier::default(),
    };
    let first = benterm.focused();
    benterm.wait_for_ready(first);
    benterm
}

impl Benterm {
    fn root<T>(&mut self, read: impl FnOnce(&BentermWindow, &App) -> T) -> T {
        self.window.read_with(&self.cx, read).unwrap()
    }

    fn sent(&self) -> Vec<SystemNotification> {
        self.sent.0.borrow().clone()
    }

    fn pass_time(&mut self) {
        self.cx.run_until_parked();
        self.cx.update(|window, cx| window.draw(cx).clear(cx));
        std::thread::sleep(Duration::from_millis(5));
        self.cx.run_until_parked();
    }

    fn wait_until(&mut self, what: &str, mut done: impl FnMut(&mut Self) -> bool) {
        let deadline = Instant::now() + TIMEOUT;
        while !done(self) {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            self.pass_time();
        }
    }

    /// The focused pane of the selected tab of the selected workspace.
    fn focused(&mut self) -> PaneId {
        self.root(|root, cx| root.grid(cx).read(cx).focused())
    }

    fn workspace(&mut self) -> WorkspaceId {
        self.root(|root, _| root.selected_workspace())
    }

    fn tabs_of(&mut self, workspace: WorkspaceId) -> Entity<TabGroup> {
        self.root(|root, _| root.workspace_tabs(workspace).unwrap())
    }

    fn selected_tab(&mut self) -> TabId {
        self.root(|root, cx| root.tabs().read(cx).selected())
    }

    /// The grid and the terminal of a pane, in whichever workspace and
    /// tab it is.
    fn find(&mut self, pane: PaneId) -> (Entity<PaneGrid>, Entity<TerminalView>) {
        self.root(|root, cx| {
            for workspace in root.workspaces() {
                let tabs = root.workspace_tabs(workspace).unwrap();
                let tabs = tabs.read(cx);
                for tab in tabs.tabs() {
                    let grid = tabs.grid(tab).unwrap();
                    if let Some(view) = grid.read(cx).terminal(pane) {
                        return (grid.clone(), view.clone());
                    }
                }
            }
            panic!("there is no pane {pane}")
        })
    }

    fn wait_for_ready(&mut self, pane: PaneId) {
        let (_, view) = self.find(pane);
        self.wait_until("the pane's shell", |benterm| {
            view.read_with(&benterm.cx, |view, _| view.text(None).contains("ready"))
        });
    }

    fn hook(&mut self, pane: PaneId, event: &str) {
        let (_, view) = self.find(pane);
        view.update(&mut self.cx, |view, cx| {
            view.report_hook(&HookEvent::new("claude", event), cx)
        });
        self.pass_time();
    }

    /// The agent in `pane` works and then finishes its turn.
    fn finish_turn(&mut self, pane: PaneId) {
        self.hook(pane, "UserPromptSubmit");
        self.hook(pane, "Stop");
    }

    fn run_in(&mut self, pane: PaneId, line: &str) {
        let (_, view) = self.find(pane);
        assert!(view.read_with(&self.cx, |view, _| view.send_text(&format!("{line}\n"))));
    }

    fn dispatch(&mut self, action: impl gpui::Action) {
        self.cx.dispatch_action(action);
        self.pass_time();
    }

    /// Opens a tab in the selected workspace and returns its pane, which
    /// then has the focus.
    fn new_tab(&mut self) -> PaneId {
        self.dispatch(NewTab);
        let pane = self.focused();
        self.wait_for_ready(pane);
        pane
    }

    fn marked(&mut self, pane: PaneId) -> bool {
        let window = self.root(|root, _| root.needs_attention(pane));
        let (grid, _) = self.find(pane);
        let shown = grid.read_with(&self.cx, |grid, _| grid.needs_attention(pane));
        assert_eq!(window, shown, "the window and the grid disagree on {pane}");
        window
    }

    fn tab_marked(&mut self, workspace: WorkspaceId, tab: TabId) -> bool {
        let tabs = self.tabs_of(workspace);
        tabs.read_with(&self.cx, |tabs, cx| tabs.tab_needs_attention(tab, cx))
    }

    fn workspace_marked(&mut self, workspace: WorkspaceId) -> bool {
        self.root(|root, cx| root.workspace_needs_attention(workspace, cx))
    }
}

#[gpui::test]
fn an_agent_that_finishes_behind_another_tab_marks_its_pane_tab_and_workspace(
    cx: &mut TestAppContext,
) {
    let mut benterm = open(cx);
    let workspace = benterm.workspace();
    let behind = benterm.focused();
    let behind_tab = benterm.selected_tab();
    benterm.new_tab();
    let front_tab = benterm.selected_tab();

    benterm.hook(behind, "UserPromptSubmit");
    assert!(!benterm.marked(behind), "working is no reason for a mark");
    benterm.hook(behind, "Stop");

    assert!(benterm.marked(behind));
    assert!(benterm.tab_marked(workspace, behind_tab));
    assert!(!benterm.tab_marked(workspace, front_tab));
    assert!(benterm.workspace_marked(workspace));
    // The marks are drawn.
    assert!(benterm.cx.debug_bounds("tab-attention-0").is_some());
    assert!(benterm.cx.debug_bounds("tab-attention-1").is_none());
    assert!(benterm.cx.debug_bounds("workspace-attention-0").is_some());
}

#[gpui::test]
fn looking_at_the_pane_takes_the_marks_away(cx: &mut TestAppContext) {
    let mut benterm = open(cx);
    let workspace = benterm.workspace();
    let behind = benterm.focused();
    let behind_tab = benterm.selected_tab();
    benterm.new_tab();
    benterm.finish_turn(behind);
    assert!(benterm.marked(behind));

    let tabs = benterm.tabs_of(workspace);
    benterm
        .window
        .update(&mut benterm.cx, |_, window, cx| {
            tabs.update(cx, |tabs, cx| tabs.select(behind_tab, window, cx))
        })
        .unwrap();
    benterm.pass_time();

    assert_eq!(benterm.focused(), behind);
    assert!(!benterm.marked(behind));
    assert!(!benterm.tab_marked(workspace, behind_tab));
    assert!(!benterm.workspace_marked(workspace));
    assert!(benterm.cx.debug_bounds("tab-attention-0").is_none());
    assert!(benterm.cx.debug_bounds("workspace-attention-0").is_none());
}

#[gpui::test]
fn the_pane_the_user_looks_at_is_neither_marked_nor_notified(cx: &mut TestAppContext) {
    let mut benterm = open(cx);
    let pane = benterm.focused();
    benterm.finish_turn(pane);
    assert!(!benterm.marked(pane));
    assert!(benterm.sent().is_empty());
}

#[gpui::test]
fn a_pane_out_of_view_is_notified_once_with_a_tag_that_names_it(cx: &mut TestAppContext) {
    let mut benterm = open(cx);
    let pane = benterm.focused();
    benterm.cx.deactivate_window();
    benterm.finish_turn(pane);

    assert!(benterm.marked(pane));
    let sent = benterm.sent();
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert_eq!(attention::pane_of_tag(&sent[0].tag), Some(pane.get()));
}

#[gpui::test]
fn the_window_coming_to_the_front_takes_the_mark_of_the_pane_in_view(cx: &mut TestAppContext) {
    let mut benterm = open(cx);
    let pane = benterm.focused();
    benterm.cx.deactivate_window();
    benterm.finish_turn(pane);
    assert!(benterm.marked(pane));

    benterm.cx.update(|window, _| window.activate_window());
    benterm.pass_time();
    assert!(!benterm.marked(pane));
}

#[gpui::test]
fn clicking_the_notification_goes_to_its_pane(cx: &mut TestAppContext) {
    let mut benterm = open(cx);
    let workspace = benterm.workspace();
    let target_tab = benterm.selected_tab();
    let target = benterm.focused();
    // Another pane of the tab takes the focus, another tab is selected,
    // and another workspace is shown.
    benterm.dispatch(SplitRight);
    assert_ne!(benterm.focused(), target);
    benterm.new_tab();
    benterm.dispatch(NewWorkspace);
    assert_ne!(benterm.workspace(), workspace);

    benterm.cx.deactivate_window();
    benterm.finish_turn(target);
    let tag = benterm.sent()[0].tag.clone();

    benterm
        .cx
        .update(|_, cx| attention::answer_notifications(cx));
    benterm
        .cx
        .simulate_system_notification_response(SystemNotificationResponse {
            tag,
            action_id: None,
        });
    benterm.pass_time();

    assert_eq!(benterm.workspace(), workspace);
    assert_eq!(benterm.selected_tab(), target_tab);
    assert_eq!(benterm.focused(), target);
    let (_, view) = benterm.find(target);
    let handle = view.read_with(&benterm.cx, |view, _| view.focus_handle().clone());
    assert!(benterm.cx.update(|window, _| handle.is_focused(window)));
}

#[gpui::test]
fn a_tag_of_another_run_or_app_goes_nowhere(cx: &mut TestAppContext) {
    let mut benterm = open(cx);
    let pane = benterm.focused();
    assert_eq!(attention::pane_of_tag("benterm-notify-check"), None);
    assert_eq!(
        attention::pane_of_tag(&format!("benterm-pane-0-{}", pane.get())),
        None
    );
}

#[gpui::test]
fn osc_9_marks_the_pane_and_notifies_its_body(cx: &mut TestAppContext) {
    let mut benterm = open(cx);
    let workspace = benterm.workspace();
    let behind = benterm.focused();
    let behind_tab = benterm.selected_tab();
    benterm.new_tab();

    benterm.run_in(behind, r"printf '\033]9;build done\007'");
    benterm.wait_until("the mark", |benterm| benterm.marked(behind));

    assert!(benterm.tab_marked(workspace, behind_tab));
    let sent = benterm.sent();
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert!(sent[0].title.starts_with("Terminal program · "), "{sent:?}");
    assert_eq!(sent[0].body.as_ref(), "build done");
    assert_eq!(attention::pane_of_tag(&sent[0].tag), Some(behind.get()));
}

/// A program that is no agent tells its state with OSC 7501. The state
/// is shown, and that is all: a program at rest is not one that has
/// finished a turn, and marks and notifications are for agents.
#[gpui::test]
fn a_program_that_is_no_agent_is_neither_marked_nor_notified_for_its_state(
    cx: &mut TestAppContext,
) {
    let mut benterm = open(cx);
    let behind = benterm.focused();
    benterm.new_tab();
    let state_of = |benterm: &mut Benterm| {
        let (_, view) = benterm.find(behind);
        view.read_with(&benterm.cx, |view, _| view.agent_status().state)
    };

    for (body, state) in [
        ("state=idle", AgentState::Done),
        ("state=working", AgentState::Working),
        ("state=done", AgentState::Done),
        ("state=error", AgentState::Error),
        (
            "state=blocked:kind=permission",
            AgentState::Waiting(WaitReason::Permission),
        ),
    ] {
        benterm.run_in(behind, &format!(r"printf '\033]7501;{body}\033\\'"));
        benterm.wait_until(body, |benterm| state_of(benterm) == state);
        assert!(!benterm.marked(behind), "{body}");
    }
    assert!(benterm.sent().is_empty(), "{:?}", benterm.sent());
}

#[gpui::test]
fn osc_777_notifies_its_title_and_body(cx: &mut TestAppContext) {
    let mut benterm = open(cx);
    let behind = benterm.focused();
    benterm.new_tab();

    benterm.run_in(behind, r"printf '\033]777;notify;T;B\007'");
    benterm.wait_until("the mark", |benterm| benterm.marked(behind));

    let sent = benterm.sent();
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert!(sent[0].title.starts_with("Terminal program · "), "{sent:?}");
    assert_eq!(sent[0].body.as_ref(), "T: B");
}

/// A program may write what an agent's notification says. Its
/// notification still says that a program of the terminal asked for it.
#[gpui::test]
fn a_program_cannot_post_the_notification_of_an_agent(cx: &mut TestAppContext) {
    let mut benterm = open(cx);
    let first = benterm.focused();
    let second = benterm.new_tab();
    benterm.cx.deactivate_window();
    benterm.finish_turn(first);
    let agent = benterm.sent().pop().unwrap();

    let line = format!(
        r"printf '\033]777;notify;{};{}\007'",
        agent.title, agent.body
    );
    benterm.run_in(second, &line);
    benterm.wait_until("the notification", |benterm| benterm.sent().len() == 2);

    let sent = benterm.sent();
    assert_ne!(sent[1].title, agent.title, "{sent:?}");
    assert!(sent[1].title.starts_with("Terminal program · "), "{sent:?}");
}

#[gpui::test]
fn outside_an_app_bundle_the_pane_is_marked_and_nothing_is_posted(cx: &mut TestAppContext) {
    // The test platform, like macOS outside a bundle, posts nothing for an
    // app without an identity.
    let mut benterm = open_with_system_notifier(cx);
    let pane = benterm.focused();
    benterm.cx.deactivate_window();
    benterm.finish_turn(pane);
    assert!(benterm.marked(pane));
    assert!(benterm.cx.shown_system_notifications().is_empty());
}

#[gpui::test]
fn in_an_app_bundle_the_notification_is_posted_by_gpui(cx: &mut TestAppContext) {
    cx.update(|cx| cx.set_app_identity("benterm.test", "Benterm"));
    let mut benterm = open_with_system_notifier(cx);
    let pane = benterm.focused();
    benterm.cx.deactivate_window();
    benterm.finish_turn(pane);
    let shown = benterm.cx.shown_system_notifications();
    assert_eq!(shown.len(), 1, "{shown:?}");
    assert_eq!(attention::pane_of_tag(&shown[0].tag), Some(pane.get()));
}

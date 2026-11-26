//! The in-app notifications in a window: an event that marks a pane the
//! user does not look at shows a notification in the bottom right corner
//! for a while, and a click on it goes to the pane.

use std::rc::Rc;
use std::time::{Duration, Instant};

use benterm_agent::HookEvent;
use benterm_layout::{PaneId, TabId, WorkspaceId};
use benterm_terminal::PtyConfig;
use benterm_ui::settings::ConfigFeed;
use benterm_ui::toast::{SHOWN_FOR, Toast, ToastKind};
use benterm_ui::{
    BentermWindow, NewTab, NewWorkspace, PaneGrid, SplitRight, StartedTerminal, TerminalView,
    bind_keys, ipc,
};
use gpui::{App, Entity, Modifiers, TestAppContext, VisualTestContext, WindowHandle};

const TIMEOUT: Duration = Duration::from_secs(10);

/// Every pane runs a shell loop that runs each line it is sent, without
/// an echo, so a test can make a pane print what it likes.
const SCRIPT: &str = r#"stty -echo; printf ready; while IFS= read -r line; do eval "$line"; done"#;

struct Benterm {
    window: WindowHandle<BentermWindow>,
    cx: VisualTestContext,
}

/// Opens an active window that posts with GPUI, as the app does under
/// `cargo run`.
fn open(cx: &mut TestAppContext) -> Benterm {
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
    let mut benterm = Benterm { window, cx };
    let first = benterm.focused();
    benterm.wait_for_ready(first);
    benterm
}

impl Benterm {
    fn root<T>(&mut self, read: impl FnOnce(&BentermWindow, &App) -> T) -> T {
        self.window.read_with(&self.cx, read).unwrap()
    }

    fn toasts(&mut self) -> Vec<Toast> {
        self.root(|root, cx| root.shown_toasts(cx))
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

    fn focused(&mut self) -> PaneId {
        self.root(|root, cx| root.grid(cx).read(cx).focused())
    }

    fn workspace(&mut self) -> WorkspaceId {
        self.root(|root, _| root.selected_workspace())
    }

    fn selected_tab(&mut self) -> TabId {
        self.root(|root, cx| root.tabs().read(cx).selected())
    }

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

    fn new_tab(&mut self) -> PaneId {
        self.dispatch(NewTab);
        let pane = self.focused();
        self.wait_for_ready(pane);
        pane
    }

    fn drawn(&mut self, index: usize) -> bool {
        // GPUI looks the selectors up by names that live as long as the
        // program.
        let selector = ["toast-0", "toast-1", "toast-2", "toast-3"][index];
        self.cx.debug_bounds(selector).is_some()
    }
}

#[gpui::test]
fn an_agent_that_finishes_out_of_view_shows_a_notification_with_its_name(cx: &mut TestAppContext) {
    let mut benterm = open(cx);
    let pane = benterm.focused();
    benterm.cx.deactivate_window();
    benterm.finish_turn(pane);

    let toasts = benterm.toasts();
    assert_eq!(toasts.len(), 1, "{toasts:?}");
    assert_eq!(toasts[0].pane, pane);
    assert_eq!(toasts[0].kind, ToastKind::Done);
    assert!(toasts[0].source.starts_with("Claude Code"), "{toasts:?}");
    assert_eq!(toasts[0].title, "Finished");
    assert!(benterm.drawn(0));
}

#[gpui::test]
fn an_agent_that_asks_for_permission_says_so(cx: &mut TestAppContext) {
    let mut benterm = open(cx);
    let behind = benterm.focused();
    benterm.new_tab();
    benterm.hook(behind, "UserPromptSubmit");
    benterm.hook(behind, "PermissionRequest");

    let toasts = benterm.toasts();
    assert_eq!(toasts.len(), 1, "{toasts:?}");
    assert_eq!(toasts[0].kind, ToastKind::Waiting);
    assert_eq!(toasts[0].title, "Asks for permission");
}

#[gpui::test]
fn the_pane_the_user_looks_at_shows_no_notification(cx: &mut TestAppContext) {
    let mut benterm = open(cx);
    let pane = benterm.focused();
    benterm.finish_turn(pane);
    assert!(benterm.toasts().is_empty());
    assert!(!benterm.drawn(0));
}

#[gpui::test]
fn osc_777_shows_its_title_and_body(cx: &mut TestAppContext) {
    let mut benterm = open(cx);
    let behind = benterm.focused();
    benterm.new_tab();

    benterm.run_in(behind, r"printf '\033]777;notify;T;B\007'");
    benterm.wait_until("the notification", |benterm| !benterm.toasts().is_empty());

    let toasts = benterm.toasts();
    assert_eq!(toasts[0].kind, ToastKind::Osc);
    assert!(
        toasts[0].source.starts_with("Terminal program"),
        "{toasts:?}"
    );
    assert_eq!(toasts[0].title, "T");
    assert_eq!(toasts[0].body.as_deref(), Some("B"));
}

#[gpui::test]
fn the_notification_goes_when_its_time_is_up(cx: &mut TestAppContext) {
    let mut benterm = open(cx);
    let pane = benterm.focused();
    benterm.cx.deactivate_window();
    benterm.finish_turn(pane);
    assert!(benterm.drawn(0));

    benterm
        .cx
        .executor()
        .advance_clock(SHOWN_FOR - Duration::from_millis(100));
    benterm.pass_time();
    assert_eq!(benterm.toasts().len(), 1);

    benterm
        .cx
        .executor()
        .advance_clock(Duration::from_millis(100));
    benterm.pass_time();
    assert!(benterm.toasts().is_empty());
    assert!(!benterm.drawn(0));
}

#[gpui::test]
fn clicking_the_notification_goes_to_its_pane_in_another_workspace(cx: &mut TestAppContext) {
    let mut benterm = open(cx);
    let workspace = benterm.workspace();
    let target_tab = benterm.selected_tab();
    let target = benterm.focused();
    benterm.dispatch(SplitRight);
    benterm.new_tab();
    benterm.dispatch(NewWorkspace);
    assert_ne!(benterm.workspace(), workspace);

    benterm.finish_turn(target);
    let bounds = benterm
        .cx
        .debug_bounds("toast-0")
        .expect("the notification");
    benterm
        .cx
        .simulate_click(bounds.center(), Modifiers::none());
    benterm.pass_time();

    assert_eq!(benterm.workspace(), workspace);
    assert_eq!(benterm.selected_tab(), target_tab);
    assert_eq!(benterm.focused(), target);
    let (_, view) = benterm.find(target);
    let handle = view.read_with(&benterm.cx, |view, _| view.focus_handle().clone());
    assert!(benterm.cx.update(|window, _| handle.is_focused(window)));
    assert!(benterm.toasts().is_empty(), "the clicked one goes");
}

#[gpui::test]
fn outside_an_app_bundle_the_notification_is_drawn(cx: &mut TestAppContext) {
    // The test platform, like macOS outside a bundle, posts nothing for an
    // app without an identity; the window posts with GPUI as the app does.
    let mut benterm = open(cx);
    let pane = benterm.focused();
    benterm.cx.deactivate_window();
    benterm.finish_turn(pane);
    assert!(benterm.cx.shown_system_notifications().is_empty());
    assert!(benterm.drawn(0));
}

#[gpui::test]
fn many_at_once_are_stacked_up_to_the_limit(cx: &mut TestAppContext) {
    let mut benterm = open(cx);
    let mut behind = vec![benterm.focused()];
    for _ in 0..3 {
        behind.push(benterm.new_tab());
    }
    benterm.new_tab();
    for pane in &behind {
        benterm.finish_turn(*pane);
    }
    let shown: Vec<PaneId> = benterm.toasts().iter().map(|toast| toast.pane).collect();
    assert_eq!(shown, behind[1..]);
    assert!(benterm.drawn(2));
    assert!(!benterm.drawn(3));
}

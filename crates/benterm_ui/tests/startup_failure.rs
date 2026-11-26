//! Startup failure notifications: when a pane's program exits with a non-zero
//! code shortly after starting (or with 127 at any time), the window shows an
//! in-app notification with the exit code and the last non-empty output lines
//! while closing the pane as usual.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::time::{Duration, Instant};

use benterm_layout::PaneId;
use benterm_terminal::PtyConfig;
use benterm_ui::settings::ConfigFeed;
use benterm_ui::toast::{STARTUP_FAILURE_WINDOW, Toast, ToastKind};
use benterm_ui::{
    BentermWindow, PaneGrid, SplitRight, StartedTerminal, TerminalView, bind_keys, ipc,
};
use gpui::{App, Entity, TestAppContext, VisualTestContext, WindowHandle};

const TIMEOUT: Duration = Duration::from_secs(10);

/// A shell loop that prints `ready` and evaluates each line sent to it.
const SCRIPT: &str = r#"stty -echo; printf ready; while IFS= read -r line; do eval "$line"; done"#;

struct Benterm {
    window: WindowHandle<BentermWindow>,
    cx: VisualTestContext,
    next_commands: Rc<RefCell<VecDeque<PtyConfig>>>,
}

fn open(cx: &mut TestAppContext) -> Benterm {
    cx.executor().allow_parking();
    cx.update(bind_keys);
    let next_commands: Rc<RefCell<VecDeque<PtyConfig>>> = Rc::new(RefCell::new(VecDeque::new()));
    let queue = Rc::clone(&next_commands);
    let start: ipc::StartTerminal = Rc::new(move |pty, theme| {
        let pty = queue.borrow_mut().pop_front().unwrap_or(pty);
        StartedTerminal::spawn(pty, theme)
    });
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
        next_commands,
    };
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

    fn panes(&mut self) -> Vec<PaneId> {
        self.root(|root, cx| root.grid(cx).read(cx).panes())
    }

    fn focused(&mut self) -> PaneId {
        self.root(|root, cx| root.grid(cx).read(cx).focused())
    }

    fn find(&mut self, pane: PaneId) -> (Entity<PaneGrid>, Entity<TerminalView>) {
        self.root(|root, cx| {
            let grid = root.grid(cx);
            let view = grid.read(cx).terminal(pane).expect("pane exists").clone();
            (grid, view)
        })
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

    fn wait_for_ready(&mut self, pane: PaneId) {
        let (_, view) = self.find(pane);
        self.wait_until("the pane's shell", |benterm| {
            view.read_with(&benterm.cx, |view, _| view.text(None).contains("ready"))
        });
    }

    fn run_in(&mut self, pane: PaneId, line: &str) {
        let (_, view) = self.find(pane);
        assert!(view.read_with(&self.cx, |view, _| view.send_text(&format!("{line}\n"))));
    }

    fn split_with(&mut self, pty: PtyConfig) {
        self.next_commands.borrow_mut().push_back(pty);
        self.cx.dispatch_action(SplitRight);
        self.pass_time();
    }

    fn split_shell(&mut self) -> PaneId {
        self.cx.dispatch_action(SplitRight);
        self.pass_time();
        let pane = self.focused();
        self.wait_for_ready(pane);
        pane
    }
}

#[gpui::test]
fn quick_nonzero_exit_shows_exit_code_and_output_in_toast(cx: &mut TestAppContext) {
    let mut benterm = open(cx);
    let first = benterm.focused();

    benterm.split_with(PtyConfig::command("/bin/sh", ["-c", "echo boom; exit 3"]));
    benterm.wait_until("the failing pane closes", |benterm| {
        benterm.panes() == [first]
    });

    let toasts = benterm.toasts();
    assert_eq!(toasts.len(), 1, "{toasts:?}");
    assert_eq!(toasts[0].kind, ToastKind::Error);
    assert!(
        toasts[0].title.contains("3"),
        "expected exit code 3 in title: {:?}",
        toasts[0]
    );
    assert!(
        toasts[0]
            .body
            .as_deref()
            .is_some_and(|b| b.contains("boom")),
        "expected 'boom' in body: {:?}",
        toasts[0]
    );
}

#[gpui::test]
fn zero_exit_closes_without_toast(cx: &mut TestAppContext) {
    let mut benterm = open(cx);
    let first = benterm.focused();

    benterm.split_with(PtyConfig::command("/bin/sh", ["-c", "echo done; exit 0"]));
    benterm.wait_until("the pane closes", |benterm| benterm.panes() == [first]);

    assert!(benterm.toasts().is_empty(), "{:?}", benterm.toasts());
}

#[gpui::test]
fn command_not_found_127_shows_toast_even_after_threshold(cx: &mut TestAppContext) {
    let mut benterm = open(cx);
    let first = benterm.focused();
    let second = benterm.split_shell();
    assert_ne!(first, second);

    // Advance past the startup failure window, then run a nonexistent command.
    benterm
        .cx
        .executor()
        .advance_clock(STARTUP_FAILURE_WINDOW + Duration::from_secs(1));
    benterm.run_in(second, "benterm_no_such_command_281; exit $?");
    benterm.wait_until("the pane closes", |benterm| benterm.panes() == [first]);

    let toasts = benterm.toasts();
    assert_eq!(toasts.len(), 1, "{toasts:?}");
    assert_eq!(toasts[0].kind, ToastKind::Error);
    assert!(
        toasts[0].title.contains("127"),
        "expected exit code 127 in title: {:?}",
        toasts[0]
    );
}

#[gpui::test]
fn nonzero_exit_after_threshold_closes_without_toast(cx: &mut TestAppContext) {
    let mut benterm = open(cx);
    let first = benterm.focused();
    let second = benterm.split_shell();
    assert_ne!(first, second);

    // Advance past the startup failure window (3 seconds > 2 seconds).
    benterm.cx.executor().advance_clock(Duration::from_secs(3));
    benterm.run_in(second, "echo late-failure; exit 1");
    benterm.wait_until("the pane closes", |benterm| benterm.panes() == [first]);

    assert!(benterm.toasts().is_empty(), "{:?}", benterm.toasts());
}

#[gpui::test]
fn quick_failure_toast_keeps_at_most_five_non_empty_lines(cx: &mut TestAppContext) {
    let mut benterm = open(cx);
    let first = benterm.focused();

    let script =
        "printf 'line1\\nline2\\n\\nline3\\n   \\nline4\\nline5\\nline6\\nline7\\n\\n'; exit 2";
    benterm.split_with(PtyConfig::command("/bin/sh", ["-c", script]));
    benterm.wait_until("the failing pane closes", |benterm| {
        benterm.panes() == [first]
    });

    let toasts = benterm.toasts();
    assert_eq!(toasts.len(), 1, "{toasts:?}");
    assert_eq!(toasts[0].kind, ToastKind::Error);
    assert!(toasts[0].title.contains("2"), "{:?}", toasts[0]);
    assert_eq!(
        toasts[0].body.as_deref(),
        Some("line3\nline4\nline5\nline6\nline7")
    );
}

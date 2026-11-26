//! The tabs above the panes, and the progress board they open: switching
//! to it and back, and going from a row of the board to its pane. The
//! agents' states come from made-up hook events over the real socket.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use benterm_ipc::{Client, ClientError, Method, PaneId as ApiPaneId, ReportHook};
use benterm_layout::PaneId;
use benterm_terminal::PtyConfig;
use benterm_ui::settings::ConfigFeed;
use benterm_ui::timeline::TimeSpan;
use benterm_ui::{BentermWindow, MainView, StartedTerminal, bind_keys, ipc};
use gpui::{Modifiers, TestAppContext, VisualTestContext, WindowHandle};

const TIMEOUT: Duration = Duration::from_secs(10);

struct Benterm {
    window: WindowHandle<BentermWindow>,
    cx: VisualTestContext,
    socket: PathBuf,
    _dir: tempfile::TempDir,
}

/// Opens a window as the app does, with a session and a socket of its
/// own.
fn open(cx: &mut TestAppContext) -> Benterm {
    cx.executor().allow_parking();
    cx.update(bind_keys);
    let dir = tempfile::Builder::new()
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap();
    let socket = dir.path().join("benterm.sock");
    let session = Some(dir.path().join("state").join("session.json"));
    let start: ipc::StartTerminal = Rc::new(StartedTerminal::spawn);
    let pty = PtyConfig::command("/bin/sh", ["-c", "sleep 100"]);
    let feed = ConfigFeed::defaults();
    let window = cx
        .update(|cx| ipc::open_session_window(pty, start, Some(socket.clone()), session, feed, cx))
        .unwrap();
    let cx = VisualTestContext::from_window(window.into(), cx);
    cx.run_until_parked();
    Benterm {
        window,
        cx,
        socket,
        _dir: dir,
    }
}

impl Benterm {
    fn root<T>(&mut self, read: impl FnOnce(&BentermWindow, &gpui::App) -> T) -> T {
        self.window.read_with(&self.cx, read).unwrap()
    }

    fn wait_until(&mut self, what: &str, mut done: impl FnMut(&mut Self) -> bool) {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            self.cx.run_until_parked();
            if done(self) {
                return;
            }
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn call<M>(&mut self, request: M) -> Result<M::Output, ClientError>
    where
        M: Method + Send + 'static,
        M::Output: Send + 'static,
    {
        let (sender, answer) = mpsc::channel();
        let socket = self.socket.clone();
        std::thread::spawn(move || {
            let result = Client::connect(socket)
                .map_err(ClientError::Io)
                .and_then(|mut client| client.call(&request));
            let _ = sender.send(result);
        });
        let deadline = Instant::now() + TIMEOUT;
        loop {
            self.cx.run_until_parked();
            if let Ok(result) = answer.recv_timeout(Duration::from_millis(5)) {
                return result;
            }
            assert!(Instant::now() < deadline, "no answer before the timeout");
        }
    }

    fn hook(&mut self, pane: PaneId, agent: &str, event: &str) {
        let hook = ReportHook {
            pane: ApiPaneId::new(pane.get()),
            agent: agent.into(),
            event: event.into(),
            tool_name: None,
            notification_type: None,
            session_id: Some("s1".into()),
            agent_id: None,
            tool_input: None,
            tool_response: None,
        };
        self.call(hook).unwrap();
    }

    fn focused(&mut self) -> PaneId {
        self.root(|root, cx| root.grid(cx).read(cx).focused())
    }

    /// Whether the keyboard is in the focused pane of the selected tab.
    fn pane_has_keyboard(&mut self) -> bool {
        let handle = self.root(|root, cx| root.terminal(cx).read(cx).focus_handle().clone());
        self.cx.update(|window, _| handle.is_focused(window))
    }

    fn board_has_keyboard(&mut self) -> bool {
        let Some(board) = self.root(|root, _| root.progress_board()) else {
            return false;
        };
        let handle = board.read_with(&self.cx, |board, _| board.focus_handle().clone());
        self.cx.update(|window, _| handle.is_focused(window))
    }

    fn click(&mut self, selector: &'static str) {
        self.cx.run_until_parked();
        let _ = self
            .window
            .update(&mut self.cx, |_, window, _| window.refresh());
        self.cx.run_until_parked();
        let bounds = self
            .cx
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("{selector} is drawn"));
        self.cx.simulate_click(bounds.center(), Modifiers::none());
        self.cx.run_until_parked();
    }

    fn drawn(&mut self, selector: &'static str) -> bool {
        let _ = self
            .window
            .update(&mut self.cx, |_, window, _| window.refresh());
        self.cx.run_until_parked();
        self.cx.debug_bounds(selector).is_some()
    }
}

/// There is one bar above the pane area: the views are switched from
/// its right end, in the bar of the tabs, and stay where they are when
/// another view is shown.
#[gpui::test]
fn the_views_are_switched_from_the_right_end_of_the_one_bar(cx: &mut TestAppContext) {
    let mut benterm = open(cx);
    assert!(benterm.drawn("tab-bar"));
    let bar = benterm.cx.debug_bounds("tab-bar").unwrap();
    let new_tab = benterm.cx.debug_bounds("tab-new").unwrap();
    let switches = [
        "view-tab-terminals",
        "view-tab-diff",
        "view-tab-progress-board",
    ];
    let before = switches.map(|switch| benterm.cx.debug_bounds(switch).unwrap());
    for switch in before {
        assert!(
            switch.top() >= bar.top() && switch.bottom() <= bar.bottom(),
            "{switch:?} is in the bar {bar:?}"
        );
        assert!(switch.left() > new_tab.right());
    }

    benterm.click("view-tab-progress-board");

    assert!(benterm.drawn("progress-board"));
    let after = switches.map(|switch| benterm.cx.debug_bounds(switch).unwrap());
    assert_eq!(after, before);
    // The board starts under that one bar.
    let board = benterm.cx.debug_bounds("progress-board").unwrap();
    assert_eq!(board.top(), bar.bottom());
}

#[gpui::test]
fn the_tabs_above_the_panes_switch_to_the_board_and_back(cx: &mut TestAppContext) {
    let mut benterm = open(cx);
    // Two panes, the keyboard in the second.
    benterm.cx.simulate_keystrokes("cmd-d");
    let second = benterm.focused();
    assert_eq!(
        benterm.root(|root, _| root.main_view()),
        MainView::Terminals
    );
    assert!(benterm.drawn("view-tab-terminals"));
    assert!(benterm.drawn("view-tab-progress-board"));

    benterm.click("view-tab-progress-board");
    assert_eq!(
        benterm.root(|root, _| root.main_view()),
        MainView::ProgressBoard
    );
    assert!(benterm.drawn("progress-board"));
    assert!(!benterm.drawn("tab-bar"), "the panes are hidden");
    assert!(benterm.board_has_keyboard());

    benterm.click("view-tab-terminals");
    assert_eq!(
        benterm.root(|root, _| root.main_view()),
        MainView::Terminals
    );
    assert!(!benterm.drawn("progress-board"));
    assert!(benterm.drawn("tab-bar"));
    assert_eq!(benterm.focused(), second);
    assert!(benterm.pane_has_keyboard());
}

#[gpui::test]
fn the_keyboard_opens_the_board_and_escape_goes_back(cx: &mut TestAppContext) {
    let mut benterm = open(cx);
    benterm.cx.simulate_keystrokes("cmd-shift-b");
    assert_eq!(
        benterm.root(|root, _| root.main_view()),
        MainView::ProgressBoard
    );
    assert!(benterm.board_has_keyboard());
    benterm.cx.simulate_keystrokes("escape");
    assert_eq!(
        benterm.root(|root, _| root.main_view()),
        MainView::Terminals
    );
    assert!(benterm.pane_has_keyboard());
    // The same key goes back too.
    benterm.cx.simulate_keystrokes("cmd-shift-b cmd-shift-b");
    assert_eq!(
        benterm.root(|root, _| root.main_view()),
        MainView::Terminals
    );
    assert!(benterm.pane_has_keyboard());
}

#[gpui::test]
fn the_span_of_the_timeline_can_be_changed(cx: &mut TestAppContext) {
    let mut benterm = open(cx);
    benterm.click("view-tab-progress-board");
    let span = |benterm: &mut Benterm| {
        let board = benterm.root(|root, _| root.progress_board()).unwrap();
        board.read_with(&benterm.cx, |board, _| board.span())
    };
    assert_eq!(span(&mut benterm), TimeSpan::Hour);
    benterm.click("board-span-24h");
    assert_eq!(span(&mut benterm), TimeSpan::Day);
    benterm.cx.simulate_keystrokes("=");
    assert_eq!(span(&mut benterm), TimeSpan::SixHours);
    benterm.cx.simulate_keystrokes("-");
    assert_eq!(span(&mut benterm), TimeSpan::Day);
    // The board keeps the span when it is opened again.
    benterm.click("view-tab-terminals");
    benterm.click("view-tab-progress-board");
    assert_eq!(span(&mut benterm), TimeSpan::Day);
}

#[gpui::test]
fn a_row_of_an_agent_in_a_workspace_behind_goes_to_its_pane(cx: &mut TestAppContext) {
    let mut benterm = open(cx);
    // Two panes in the first workspace; the agent is in the first.
    let first_workspace = benterm.root(|root, _| root.selected_workspace());
    let agent_pane = benterm.focused();
    benterm.cx.simulate_keystrokes("cmd-d");
    benterm.hook(agent_pane, "claude", "UserPromptSubmit");
    benterm.wait_until("the record", |benterm| {
        !benterm.root(|root, _| root.agent_history()).is_empty()
    });
    // Another workspace in front.
    benterm.cx.simulate_keystrokes("cmd-n");
    assert_ne!(
        benterm.root(|root, _| root.selected_workspace()),
        first_workspace
    );

    benterm.cx.simulate_keystrokes("cmd-shift-b");
    assert!(benterm.drawn("board-lane-0"));
    assert!(!benterm.drawn("board-lane-1"));
    benterm.click("board-lane-0");

    assert_eq!(
        benterm.root(|root, _| root.main_view()),
        MainView::Terminals
    );
    assert_eq!(
        benterm.root(|root, _| root.selected_workspace()),
        first_workspace
    );
    assert_eq!(benterm.focused(), agent_pane);
    assert!(benterm.pane_has_keyboard());
}

#[gpui::test]
fn a_change_while_the_board_is_open_reaches_it(cx: &mut TestAppContext) {
    let mut benterm = open(cx);
    let pane = benterm.focused();
    benterm.cx.simulate_keystrokes("cmd-shift-b");
    assert!(!benterm.drawn("board-lane-0"));
    benterm.hook(pane, "codex", "UserPromptSubmit");
    benterm.wait_until("the lane", |benterm| benterm.drawn("board-lane-0"));
    // The keyboard stays in the board.
    assert!(benterm.board_has_keyboard());
}

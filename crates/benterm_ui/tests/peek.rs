//! The peek: the screen of an agent laid over the pane area, without
//! leaving the tab on view, and typed into from there. The window is a
//! real one and the programs are real; the agents' states come from
//! made-up hook events over the real socket.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use benterm_ipc::{
    Client, ClientError, Method, PaneId as ApiPaneId, ReadOutput, ReportHook, SendText, SpawnAgent,
};
use benterm_layout::PaneId;
use benterm_terminal::PtyConfig;
use benterm_ui::settings::ConfigFeed;
use benterm_ui::{BentermWindow, FocusReply, MainView, StartedTerminal, bind_keys, ipc};
use gpui::{Modifiers, TestAppContext, VisualTestContext, WindowHandle};

const TIMEOUT: Duration = Duration::from_secs(10);

/// What every terminal runs: says `ready`, and then every byte it is
/// given, in hex with a dot after it. Nothing is echoed and no key is
/// taken by the terminal's line discipline, so what is on the screen is
/// what the program was given. It ends when it is given `q`, or when
/// its terminal hangs up.
const BYTES: &str = r#"stty raw -echo; printf ready:; while :; do b=$(dd bs=1 count=1 2>/dev/null | od -An -tx1 | tr -d ' \n'); [ -n "$b" ] || exit; printf '%s.' "$b"; [ "$b" = 71 ] && exit; done"#;

struct Benterm {
    window: WindowHandle<BentermWindow>,
    cx: VisualTestContext,
    socket: PathBuf,
    _dir: tempfile::TempDir,
}

/// Opens a window as the app does, with a socket of its own.
fn open(cx: &mut TestAppContext) -> Benterm {
    cx.executor().allow_parking();
    cx.update(bind_keys);
    let dir = tempfile::Builder::new()
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap();
    let socket = dir.path().join("benterm.sock");
    let start: ipc::StartTerminal = Rc::new(StartedTerminal::spawn);
    let pty = PtyConfig::command("/bin/sh", ["-c", BYTES]);
    let feed = ConfigFeed::defaults();
    let window = cx
        .update(|cx| ipc::open_window(pty, start, Some(socket.clone()), feed, cx))
        .unwrap();
    let mut cx = VisualTestContext::from_window(window.into(), cx);
    // The window is in front, so what is in view counts as looked at.
    cx.update(|window, _| window.activate_window());
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
            self.cx.executor().advance_clock(Duration::from_millis(5));
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

    fn hook(&mut self, pane: PaneId, event: &str) {
        let hook = ReportHook {
            pane: ApiPaneId::new(pane.get()),
            agent: "claude".into(),
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

    /// Makes the program of `pane` an agent at work, which gives it a
    /// row in the session list.
    fn work_in(&mut self, pane: PaneId) {
        self.hook(pane, "UserPromptSubmit");
        self.wait_until("the row", |benterm| {
            benterm
                .root(|root, cx| root.session_rows(cx))
                .iter()
                .any(|row| row.pane == pane)
        });
    }

    /// Starts the agent `name` behind the window, as a child of
    /// `parent`, and returns the id of its terminal once it has a row
    /// and its program runs.
    fn spawn_behind(&mut self, name: &str, parent: PaneId) -> PaneId {
        let request = SpawnAgent {
            name: name.into(),
            command: vec!["/bin/sh".into(), "-c".into(), BYTES.into()],
            parent: Some(ApiPaneId::new(parent.get())),
            ..SpawnAgent::default()
        };
        let number = self.call(request).expect("the agent starts").pane.get();
        self.wait_until("the row", |benterm| {
            benterm
                .root(|root, cx| root.session_rows(cx))
                .iter()
                .any(|row| row.pane.get() == number)
        });
        let rows = self.root(|root, cx| root.session_rows(cx));
        let pane = rows
            .iter()
            .find(|row| row.pane.get() == number)
            .unwrap()
            .pane;
        self.wait_for_ready(pane);
        pane
    }

    fn screen(&mut self, pane: PaneId) -> String {
        let pane = ApiPaneId::new(pane.get());
        self.call(ReadOutput { pane, lines: None }).unwrap().text
    }

    fn wait_for_ready(&mut self, pane: PaneId) {
        self.wait_until("the program", |benterm| {
            benterm.screen(pane).contains("ready:")
        });
    }

    /// The bytes the program of `pane` says it was given.
    fn given(&mut self, pane: PaneId) -> String {
        let screen = self.screen(pane);
        let start = screen.find("ready:").expect("the program runs") + "ready:".len();
        screen[start..].split_whitespace().collect()
    }

    fn focused(&mut self) -> PaneId {
        self.root(|root, cx| root.grid(cx).read(cx).focused())
    }

    /// Whether the keyboard is in the focused pane of the selected tab.
    fn pane_has_keyboard(&mut self) -> bool {
        let handle = self.root(|root, cx| root.terminal(cx).read(cx).focus_handle().clone());
        self.cx.update(|window, _| handle.is_focused(window))
    }

    fn terminal_has_keyboard(&mut self, pane: PaneId) -> bool {
        let handle = self.root(|root, cx| {
            let view = root.terminal_with_id(pane, cx).expect("the terminal");
            view.read(cx).focus_handle().clone()
        });
        self.cx.update(|window, _| handle.is_focused(window))
    }

    fn board_has_keyboard(&mut self) -> bool {
        let Some(board) = self.root(|root, _| root.progress_board()) else {
            return false;
        };
        let handle = board.read_with(&self.cx, |board, _| board.focus_handle().clone());
        self.cx.update(|window, _| handle.is_focused(window))
    }

    fn peeked(&mut self) -> Option<PaneId> {
        self.root(|root, _| root.peeked())
    }

    fn tab_count(&mut self) -> usize {
        self.root(|root, cx| root.tabs().read(cx).tabs().len())
    }

    fn selected_tab(&mut self) -> benterm_layout::TabId {
        self.root(|root, cx| root.tabs().read(cx).selected())
    }

    /// The row of `pane` in the session list, counted from 0.
    fn row_of(&mut self, pane: PaneId) -> usize {
        let rows = self.root(|root, cx| root.session_rows(cx));
        rows.iter()
            .position(|row| row.pane == pane)
            .expect("the agent has a row")
    }

    fn marked_rows(&mut self) -> Vec<PaneId> {
        let rows = self.root(|root, cx| root.session_rows(cx));
        rows.into_iter()
            .filter(|row| row.marked)
            .map(|row| row.pane)
            .collect()
    }

    fn draw(&mut self) {
        self.cx.run_until_parked();
        let _ = self
            .window
            .update(&mut self.cx, |_, window, _| window.refresh());
        self.cx.run_until_parked();
    }

    fn drawn(&mut self, selector: &str) -> bool {
        self.draw();
        let selector: &'static str = selector.to_owned().leak();
        self.cx.debug_bounds(selector).is_some()
    }

    fn click(&mut self, selector: &str) {
        self.draw();
        let selector: &'static str = selector.to_owned().leak();
        let bounds = self
            .cx
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("{selector} is drawn"));
        self.cx.simulate_click(bounds.center(), Modifiers::none());
        self.cx.run_until_parked();
    }

    /// Clicks what opens the peek in the row of `pane`.
    fn click_peek_of(&mut self, pane: PaneId) {
        let row = self.row_of(pane);
        self.click(&format!("session-peek-{row}"));
    }

    /// Starts the agent `name` behind the window, as a child of the
    /// first pane, and makes it done, which gives its row a field for a
    /// reply. Returns the id of its terminal.
    fn done_behind(&mut self, name: &str) -> PaneId {
        let parent = self.focused();
        let agent = self.spawn_behind(name, parent);
        self.hook(agent, "Stop");
        self.wait_until("the field", |benterm| {
            let row = benterm.row_of(agent);
            benterm.drawn(&format!("session-reply-{row}"))
        });
        agent
    }

    fn click_reply_of(&mut self, pane: PaneId) {
        let row = self.row_of(pane);
        self.click(&format!("session-reply-{row}"));
    }

    /// Whether what is typed goes into the field of the row of `pane`:
    /// types a letter, reads the field, and takes the letter out again.
    fn reply_has_keyboard(&mut self, pane: PaneId) -> bool {
        let before = self.root(|root, cx| root.session_reply_text(pane, cx));
        self.cx.simulate_input("z");
        let after = self.root(|root, cx| root.session_reply_text(pane, cx));
        let typed = after.is_some() && after != before;
        if typed {
            self.cx.simulate_keystrokes("backspace");
        }
        typed
    }

    /// An agent in the pane of the first tab, and a second tab in front
    /// of it whose pane has the keyboard. Returns the two panes.
    fn agent_in_a_tab_behind(&mut self) -> (PaneId, PaneId) {
        let agent = self.focused();
        self.wait_for_ready(agent);
        self.work_in(agent);
        self.cx.simulate_keystrokes("cmd-t");
        let front = self.focused();
        assert_ne!(front, agent);
        self.wait_for_ready(front);
        (agent, front)
    }
}

#[gpui::test]
fn a_row_opens_the_peek_and_closing_it_leaves_the_tab_and_the_keyboard_as_before(
    cx: &mut TestAppContext,
) {
    let mut benterm = open(cx);
    let (agent, front) = benterm.agent_in_a_tab_behind();
    let tab = benterm.selected_tab();

    benterm.click_peek_of(agent);

    assert_eq!(benterm.peeked(), Some(agent));
    assert!(benterm.drawn("peek"));
    assert!(benterm.terminal_has_keyboard(agent));
    // The tab on view is still the one under the peek.
    assert_eq!(benterm.selected_tab(), tab);
    assert_eq!(benterm.focused(), front);

    benterm.click("peek-close");

    assert_eq!(benterm.peeked(), None);
    assert!(!benterm.drawn("peek"));
    assert_eq!(benterm.selected_tab(), tab);
    assert_eq!(benterm.focused(), front);
    assert!(benterm.pane_has_keyboard());
}

#[gpui::test]
fn what_is_typed_into_the_peek_reaches_its_agent_and_not_the_pane_under_it(
    cx: &mut TestAppContext,
) {
    let mut benterm = open(cx);
    let (agent, front) = benterm.agent_in_a_tab_behind();
    benterm.click_peek_of(agent);

    benterm.cx.simulate_input("y");
    benterm.cx.simulate_keystrokes("enter escape");

    // y, Enter and Escape, the last one too.
    benterm.wait_until("the keys", |benterm| benterm.given(agent) == "79.0d.1b.");
    assert_eq!(benterm.given(front), "");
    // Escape went to the agent, and the peek stays.
    assert_eq!(benterm.peeked(), Some(agent));
}

#[gpui::test]
fn the_close_key_closes_the_peek_and_no_pane_or_tab(cx: &mut TestAppContext) {
    let mut benterm = open(cx);
    let (agent, front) = benterm.agent_in_a_tab_behind();
    benterm.click_peek_of(agent);
    assert_eq!(benterm.peeked(), Some(agent));

    // The key that closes a pane is not the peek's, and closes nothing
    // while the peek has the keyboard.
    benterm.cx.simulate_keystrokes("cmd-w");
    assert_eq!(benterm.peeked(), Some(agent));
    assert_eq!(benterm.tab_count(), 2);

    benterm.cx.simulate_keystrokes("cmd-.");

    assert_eq!(benterm.peeked(), None);
    assert_eq!(benterm.tab_count(), 2);
    assert_eq!(benterm.focused(), front);
    assert!(benterm.pane_has_keyboard());
    // The program of the agent was not hung up either.
    assert!(benterm.root(|root, cx| root.terminal_with_id(agent, cx).is_some()));
    assert_eq!(benterm.given(agent), "");
}

#[gpui::test]
fn the_keyboard_alone_opens_the_peek_and_goes_from_one_agent_to_the_next(cx: &mut TestAppContext) {
    let mut benterm = open(cx);
    let (agent, front) = benterm.agent_in_a_tab_behind();
    let w1 = benterm.spawn_behind("w1", front);

    benterm.cx.simulate_keystrokes("cmd-p");
    assert_eq!(benterm.peeked(), Some(agent));
    assert!(benterm.terminal_has_keyboard(agent));

    benterm.cx.simulate_keystrokes("cmd-p");
    assert_eq!(benterm.peeked(), Some(w1));
    assert!(benterm.terminal_has_keyboard(w1));

    // Round to the first again.
    benterm.cx.simulate_keystrokes("cmd-p");
    assert_eq!(benterm.peeked(), Some(agent));

    benterm.cx.simulate_keystrokes("cmd-.");
    assert_eq!(benterm.peeked(), None);
    assert!(benterm.pane_has_keyboard());
}

#[gpui::test]
fn an_agent_behind_the_window_is_opened_as_a_tab_from_its_peek(cx: &mut TestAppContext) {
    let mut benterm = open(cx);
    let pane = benterm.focused();
    let w1 = benterm.spawn_behind("w1", pane);
    // The row of an agent with no pane to go to opens its peek.
    let row = benterm.row_of(w1);
    benterm.click(&format!("session-{row}"));
    assert_eq!(benterm.peeked(), Some(w1));
    benterm.cx.simulate_input("y");
    benterm.wait_until("the key", |benterm| benterm.given(w1) == "79.");
    assert_eq!(benterm.tab_count(), 1);

    benterm.click("peek-keep");

    assert_eq!(benterm.peeked(), None);
    assert_eq!(benterm.tab_count(), 2);
    assert_eq!(benterm.focused(), w1);
    assert!(benterm.pane_has_keyboard());
    // The same terminal, with what was on its screen.
    assert_eq!(benterm.given(w1), "79.");
    let rows = benterm.root(|root, cx| root.session_rows(cx));
    assert!(!rows[benterm.row_of(w1)].is_background());
}

#[gpui::test]
fn the_key_for_a_tab_goes_to_the_pane_of_an_agent_that_has_one(cx: &mut TestAppContext) {
    let mut benterm = open(cx);
    let (agent, _) = benterm.agent_in_a_tab_behind();
    benterm.click_peek_of(agent);

    benterm.cx.simulate_keystrokes("cmd-o");

    assert_eq!(benterm.peeked(), None);
    assert_eq!(benterm.tab_count(), 2);
    assert_eq!(benterm.focused(), agent);
    assert!(benterm.pane_has_keyboard());
}

#[gpui::test]
fn the_board_opens_the_peek_of_an_agent_behind_the_window_and_gets_the_keyboard_back(
    cx: &mut TestAppContext,
) {
    let mut benterm = open(cx);
    let pane = benterm.focused();
    let w1 = benterm.spawn_behind("w1", pane);
    benterm.work_in(w1);
    benterm.cx.simulate_keystrokes("cmd-shift-b");
    assert!(benterm.board_has_keyboard());

    benterm.click("board-lane-0");

    assert_eq!(benterm.peeked(), Some(w1));
    assert!(benterm.terminal_has_keyboard(w1));
    assert_eq!(
        benterm.root(|root, _| root.main_view()),
        MainView::ProgressBoard
    );

    benterm.cx.simulate_keystrokes("cmd-.");

    assert_eq!(benterm.peeked(), None);
    assert_eq!(
        benterm.root(|root, _| root.main_view()),
        MainView::ProgressBoard
    );
    assert!(benterm.board_has_keyboard());
}

#[gpui::test]
fn looking_at_an_agent_in_the_peek_takes_its_mark_away(cx: &mut TestAppContext) {
    let mut benterm = open(cx);
    let pane = benterm.focused();
    let w1 = benterm.spawn_behind("w1", pane);
    benterm.work_in(w1);
    benterm.hook(w1, "Stop");
    benterm.wait_until("the mark", |benterm| benterm.marked_rows() == vec![w1]);

    let row = benterm.row_of(w1);
    benterm.click(&format!("session-{row}"));

    assert_eq!(benterm.peeked(), Some(w1));
    assert_eq!(benterm.marked_rows(), Vec::<PaneId>::new());
    assert!(!benterm.drawn(&format!("session-attention-{row}")));
}

#[gpui::test]
fn the_marks_of_a_pane_go_with_the_peek_as_with_a_look_at_the_pane(cx: &mut TestAppContext) {
    let mut benterm = open(cx);
    let (agent, _) = benterm.agent_in_a_tab_behind();
    benterm.hook(agent, "Stop");
    benterm.wait_until("the mark", |benterm| benterm.marked_rows() == vec![agent]);
    let tab = benterm.root(|root, cx| root.tabs().read(cx).tabs()[0]);
    assert!(benterm.root(|root, cx| root.tabs().read(cx).tab_needs_attention(tab, cx)));

    benterm.click_peek_of(agent);

    assert_eq!(benterm.marked_rows(), Vec::<PaneId>::new());
    assert!(!benterm.root(|root, cx| root.tabs().read(cx).tab_needs_attention(tab, cx)));
}

#[gpui::test]
fn a_turn_that_ends_under_the_peek_marks_the_pane_the_peek_covers(cx: &mut TestAppContext) {
    let mut benterm = open(cx);
    let (agent, front) = benterm.agent_in_a_tab_behind();
    benterm.work_in(front);
    benterm.click_peek_of(agent);

    benterm.hook(front, "Stop");

    benterm.wait_until("the mark", |benterm| benterm.marked_rows() == vec![front]);
    // Back at the pane, it is looked at.
    benterm.cx.simulate_keystrokes("cmd-.");
    assert_eq!(benterm.marked_rows(), Vec::<PaneId>::new());
}

#[gpui::test]
fn the_peek_of_a_terminal_behind_the_window_leaves_its_size_as_it_is(cx: &mut TestAppContext) {
    let mut benterm = open(cx);
    let pane = benterm.focused();
    benterm.draw();
    let w1 = benterm.spawn_behind("w1", pane);
    let size = |benterm: &mut Benterm| {
        benterm.root(|root, cx| {
            let view = root.terminal_with_id(w1, cx).unwrap();
            view.read(cx).screen().size
        })
    };
    let before = size(&mut benterm);

    let row = benterm.row_of(w1);
    benterm.click(&format!("session-{row}"));
    benterm.draw();

    assert_eq!(benterm.peeked(), Some(w1));
    assert_eq!(size(&mut benterm), before);
}

/// The peek shows the agent's pane alone, also when the pane is one of
/// a split on view: it has the whole area there, and its own place and
/// size again when the peek closes.
#[gpui::test]
fn a_pane_of_a_split_on_view_is_peeked_at_alone(cx: &mut TestAppContext) {
    let mut benterm = open(cx);
    let agent = benterm.focused();
    benterm.wait_for_ready(agent);
    benterm.work_in(agent);
    benterm.cx.simulate_keystrokes("cmd-d");
    let other = benterm.focused();
    assert_ne!(other, agent);
    benterm.draw();
    let size = |benterm: &mut Benterm, pane: PaneId| {
        benterm.root(|root, cx| {
            let view = root.terminal_with_id(pane, cx).unwrap();
            view.read(cx).screen().size
        })
    };
    let before = size(&mut benterm, agent);

    benterm.click_peek_of(agent);
    benterm.draw();

    assert_eq!(benterm.peeked(), Some(agent));
    assert!(benterm.drawn("peek"));
    assert!(
        size(&mut benterm, agent).cols > before.cols,
        "the pane has the width of the whole area in the peek"
    );

    benterm.cx.simulate_keystrokes("cmd-.");
    benterm.draw();
    assert_eq!(benterm.peeked(), None);
    assert_eq!(size(&mut benterm, agent), before);
    assert_eq!(benterm.focused(), other, "the split is as it was");

    // A press on the agent's row goes to its pane, and ends a peek at it.
    benterm.click_peek_of(agent);
    benterm.draw();
    assert_eq!(benterm.peeked(), Some(agent));
    let row = benterm.row_of(agent);
    benterm.click(&format!("session-{row}"));
    benterm.draw();
    assert_eq!(benterm.peeked(), None);
    assert_eq!(benterm.focused(), agent);
    assert_eq!(size(&mut benterm, agent), before);
}

#[gpui::test]
fn the_peek_closes_when_its_program_exits(cx: &mut TestAppContext) {
    let mut benterm = open(cx);
    let pane = benterm.focused();
    let w1 = benterm.spawn_behind("w1", pane);
    let row = benterm.row_of(w1);
    benterm.click(&format!("session-{row}"));
    assert_eq!(benterm.peeked(), Some(w1));

    benterm.cx.simulate_input("q");

    benterm.wait_until("the peek to close", |benterm| benterm.peeked().is_none());
    assert_eq!(benterm.focused(), pane);
    assert!(benterm.pane_has_keyboard());
}

#[gpui::test]
fn the_keyboard_going_elsewhere_closes_the_peek(cx: &mut TestAppContext) {
    let mut benterm = open(cx);
    let (agent, front) = benterm.agent_in_a_tab_behind();
    benterm.click_peek_of(agent);
    assert_eq!(benterm.peeked(), Some(agent));

    // The view of the board takes the keyboard.
    benterm.cx.simulate_keystrokes("cmd-shift-b");

    assert_eq!(benterm.peeked(), None);
    assert!(benterm.board_has_keyboard());
    assert_eq!(benterm.focused(), front);
}

#[gpui::test]
fn the_command_palette_runs_the_commands_of_the_peek(cx: &mut TestAppContext) {
    let mut benterm = open(cx);
    let (agent, _) = benterm.agent_in_a_tab_behind();
    benterm.click_peek_of(agent);

    benterm.cx.simulate_keystrokes("cmd-k");
    // The peek stays under the palette.
    assert_eq!(benterm.peeked(), Some(agent));
    let palette = benterm.root(|root, _| root.command_palette()).unwrap();
    let names: Vec<String> = palette.read_with(&benterm.cx, |palette, cx| {
        palette.shown(cx).into_iter().map(|c| c.name).collect()
    });
    assert!(
        names
            .iter()
            .any(|name| name.to_lowercase() == "peek: close peek"),
        "{names:?}"
    );
    benterm.cx.simulate_input("close peek");
    benterm.cx.simulate_keystrokes("enter");

    assert_eq!(benterm.peeked(), None);
    assert_eq!(benterm.tab_count(), 2);
    assert!(benterm.pane_has_keyboard());
}

#[gpui::test]
fn a_click_on_a_field_of_the_palette_s_window_takes_the_peek_with_the_palette(
    cx: &mut TestAppContext,
) {
    let mut benterm = open(cx);
    let (agent, _) = benterm.agent_in_a_tab_behind();
    benterm.click_peek_of(agent);
    benterm.cx.simulate_keystrokes("cmd-k");
    assert_eq!(benterm.peeked(), Some(agent));

    // The keyboard goes from the palette to another view, not back.
    benterm.click("view-tab-progress-board");

    assert!(benterm.root(|root, _| root.command_palette()).is_none());
    assert!(!benterm.drawn("peek"));
    assert_eq!(benterm.peeked(), None);
    assert!(benterm.board_has_keyboard());
}

#[gpui::test]
fn the_peek_stays_while_the_window_is_behind_another_app(cx: &mut TestAppContext) {
    let mut benterm = open(cx);
    let (agent, _) = benterm.agent_in_a_tab_behind();
    benterm.click_peek_of(agent);

    benterm.cx.deactivate_window();
    benterm.draw();
    assert_eq!(benterm.peeked(), Some(agent));
    benterm.cx.update(|window, _| window.activate_window());
    benterm.draw();

    assert_eq!(benterm.peeked(), Some(agent));
    assert!(benterm.terminal_has_keyboard(agent));
    benterm.cx.simulate_input("y");
    benterm.wait_until("the key", |benterm| benterm.given(agent) == "79.");
}

#[gpui::test]
fn a_pane_under_the_progress_board_is_peeked_at_over_the_board(cx: &mut TestAppContext) {
    let mut benterm = open(cx);
    let agent = benterm.focused();
    benterm.wait_for_ready(agent);
    benterm.work_in(agent);
    benterm.cx.simulate_keystrokes("cmd-shift-b");

    benterm.click_peek_of(agent);

    assert_eq!(benterm.peeked(), Some(agent));
    assert_eq!(
        benterm.root(|root, _| root.main_view()),
        MainView::ProgressBoard
    );
    benterm.cx.simulate_keystrokes("cmd-.");
    assert!(benterm.board_has_keyboard());
}

#[gpui::test]
fn a_pane_of_a_split_behind_has_the_whole_area_in_the_peek_and_its_size_after(
    cx: &mut TestAppContext,
) {
    let mut benterm = open(cx);
    let agent = benterm.focused();
    benterm.wait_for_ready(agent);
    benterm.work_in(agent);
    benterm.cx.simulate_keystrokes("cmd-d");
    benterm.draw();
    benterm.cx.simulate_keystrokes("cmd-t");
    benterm.draw();
    let size = |benterm: &mut Benterm, pane: PaneId| {
        benterm.root(|root, cx| {
            let view = root.terminal_with_id(pane, cx).unwrap();
            view.read(cx).screen().size
        })
    };
    let before = size(&mut benterm, agent);
    let whole = benterm.focused();
    // Half the width of a pane that has the area to itself.
    assert!(before.cols < size(&mut benterm, whole).cols);

    benterm.click_peek_of(agent);
    benterm.draw();

    assert_eq!(benterm.peeked(), Some(agent));
    assert_eq!(
        size(&mut benterm, agent).cols,
        size(&mut benterm, whole).cols
    );

    benterm.cx.simulate_keystrokes("cmd-.");
    benterm.draw();
    assert_eq!(benterm.peeked(), None);
    // No frame draws the terminal now: it is given its size back, and
    // shows it once its program has been told.
    benterm.wait_until("the size it had", |benterm| size(benterm, agent) == before);
}

/// Says `W` every time its terminal is told a new size.
const TOLD_SIZES: &str = "trap 'printf W' WINCH; printf ready:; while :; do sleep 0.02; done";

#[gpui::test]
fn the_program_of_a_terminal_behind_the_window_is_told_no_size_for_the_peek(
    cx: &mut TestAppContext,
) {
    let mut benterm = open(cx);
    let pane = benterm.focused();
    benterm.draw();
    let request = SpawnAgent {
        name: "w1".into(),
        command: vec!["/bin/sh".into(), "-c".into(), TOLD_SIZES.into()],
        parent: Some(ApiPaneId::new(pane.get())),
        ..SpawnAgent::default()
    };
    let number = benterm.call(request).unwrap().pane.get();
    benterm.wait_until("the row", |benterm| {
        !benterm.root(|root, cx| root.session_rows(cx)).is_empty()
    });
    let w1 = benterm.root(|root, cx| root.session_rows(cx))[0].pane;
    assert_eq!(w1.get(), number);
    benterm.wait_for_ready(w1);

    benterm.click("session-0");
    benterm.draw();
    assert_eq!(benterm.peeked(), Some(w1));
    benterm.cx.simulate_keystrokes("cmd-.");
    benterm.draw();
    // Long enough for the shell to run its trap between two sleeps.
    std::thread::sleep(Duration::from_millis(200));

    assert_eq!(benterm.given(w1), "");

    // The program does tell: a smaller window has the terminal fitted
    // when it is next looked at.
    benterm
        .cx
        .simulate_resize(gpui::size(gpui::px(700.), gpui::px(400.)));
    benterm.draw();
    benterm.click("session-0");
    benterm.draw();
    benterm.wait_until("the new size", |benterm| benterm.given(w1).contains('W'));
}

#[gpui::test]
fn a_click_in_the_peek_does_not_reach_the_pane_under_it(cx: &mut TestAppContext) {
    let mut benterm = open(cx);
    let (agent, front) = benterm.agent_in_a_tab_behind();
    benterm.click_peek_of(agent);

    // In the terminal of the peek, where the pane of the tab is under it.
    benterm.click("peek");
    benterm.draw();

    assert_eq!(benterm.peeked(), Some(agent));
    assert!(benterm.terminal_has_keyboard(agent));
    benterm.cx.simulate_input("y");
    benterm.wait_until("the key", |benterm| benterm.given(agent) == "79.");
    assert_eq!(benterm.given(front), "");
}

#[gpui::test]
fn going_to_another_pane_of_the_tab_of_the_peeked_one_gives_that_pane_the_keyboard(
    cx: &mut TestAppContext,
) {
    let mut benterm = open(cx);
    let a = benterm.focused();
    benterm.wait_for_ready(a);
    benterm.work_in(a);
    benterm.cx.simulate_keystrokes("cmd-d");
    let b = benterm.focused();
    benterm.wait_for_ready(b);
    benterm.work_in(b);
    benterm.cx.simulate_keystrokes("cmd-t");
    benterm.click_peek_of(a);
    assert_eq!(benterm.peeked(), Some(a));

    // The row of the other agent goes to its pane.
    let row = benterm.row_of(b);
    benterm.click(&format!("session-{row}"));
    benterm.draw();

    assert_eq!(benterm.peeked(), None);
    assert_eq!(benterm.focused(), b);
    assert!(benterm.terminal_has_keyboard(b));
}

#[gpui::test]
fn the_command_palette_stays_when_the_program_under_it_exits(cx: &mut TestAppContext) {
    let mut benterm = open(cx);
    let (agent, _) = benterm.agent_in_a_tab_behind();
    benterm.click_peek_of(agent);
    benterm.cx.simulate_keystrokes("cmd-k");
    assert!(benterm.root(|root, _| root.command_palette()).is_some());

    benterm.root(|root, cx| {
        let view = root.terminal_with_id(agent, cx).unwrap();
        assert!(view.read(cx).send_text("q"));
    });

    benterm.wait_until("the peek to close", |benterm| benterm.peeked().is_none());
    benterm.draw();
    assert!(benterm.root(|root, _| root.command_palette()).is_some());
    // The palette gives the keyboard to the pane when it closes.
    benterm.cx.simulate_keystrokes("escape");
    assert!(benterm.pane_has_keyboard());
}

#[gpui::test]
fn a_pane_closed_under_the_peek_ends_its_program_and_the_peek(cx: &mut TestAppContext) {
    let mut benterm = open(cx);
    let (agent, front) = benterm.agent_in_a_tab_behind();
    benterm.click_peek_of(agent);
    benterm.draw();

    // The tab of the agent is closed, as its row's close would do.
    benterm
        .window
        .update(&mut benterm.cx, |root, window, cx| {
            let tabs = root.tabs().clone();
            tabs.update(cx, |tabs, cx| {
                let grid = tabs.grid(tabs.tabs()[0]).unwrap().clone();
                grid.update(cx, |grid, cx| grid.close_focused(window, cx));
            });
        })
        .unwrap();
    benterm.draw();

    assert_eq!(benterm.peeked(), None);
    assert!(!benterm.drawn("peek"));
    // The peek did not keep the terminal.
    assert!(benterm.root(|root, cx| root.terminal_with_id(agent, cx).is_none()));
    assert_eq!(benterm.tab_count(), 1);
    assert_eq!(benterm.focused(), front);
    assert!(benterm.pane_has_keyboard());
}

#[gpui::test]
fn a_click_on_a_field_of_the_list_keeps_the_peek_and_takes_the_keyboard(cx: &mut TestAppContext) {
    let mut benterm = open(cx);
    let agent = benterm.done_behind("a");
    benterm.click_peek_of(agent);
    assert_eq!(benterm.peeked(), Some(agent));

    benterm.click_reply_of(agent);

    assert_eq!(benterm.peeked(), Some(agent));
    assert!(benterm.drawn("peek"));
    assert!(!benterm.terminal_has_keyboard(agent));
    assert!(benterm.reply_has_keyboard(agent));
}

#[gpui::test]
fn a_reply_sent_under_the_peek_gives_the_keyboard_back_to_the_peek(cx: &mut TestAppContext) {
    let mut benterm = open(cx);
    let agent = benterm.done_behind("a");
    benterm.click_peek_of(agent);
    benterm.click_reply_of(agent);

    benterm.cx.simulate_input("next");
    benterm.cx.simulate_keystrokes("enter");

    // n, e, x, t and Enter.
    benterm.wait_until("the reply", |benterm| {
        benterm.given(agent) == "6e.65.78.74.0d."
    });
    assert_eq!(benterm.peeked(), Some(agent));
    assert!(benterm.terminal_has_keyboard(agent));
}

#[gpui::test]
fn escape_in_a_field_under_the_peek_gives_the_keyboard_back_to_the_peek(cx: &mut TestAppContext) {
    let mut benterm = open(cx);
    let agent = benterm.done_behind("a");
    benterm.click_peek_of(agent);
    benterm.click_reply_of(agent);

    benterm.cx.simulate_keystrokes("escape");

    assert_eq!(benterm.peeked(), Some(agent));
    assert!(benterm.terminal_has_keyboard(agent));
    // The Escape was the field's, not the agent's.
    assert_eq!(benterm.given(agent), "");
}

#[gpui::test]
fn a_reply_sent_without_the_peek_gives_the_keyboard_back_to_the_pane(cx: &mut TestAppContext) {
    let mut benterm = open(cx);
    let pane = benterm.focused();
    benterm.wait_for_ready(pane);
    let agent = benterm.done_behind("a");
    benterm.click_reply_of(agent);
    assert!(!benterm.pane_has_keyboard());

    benterm.cx.simulate_input("next");
    benterm.cx.simulate_keystrokes("enter");

    benterm.wait_until("the reply", |benterm| {
        benterm.given(agent) == "6e.65.78.74.0d."
    });
    assert_eq!(benterm.peeked(), None);
    assert_eq!(benterm.focused(), pane);
    assert!(benterm.pane_has_keyboard());
}

#[gpui::test]
fn the_field_of_another_agent_keeps_the_peek(cx: &mut TestAppContext) {
    let mut benterm = open(cx);
    let first = benterm.done_behind("a");
    let second = benterm.done_behind("b");
    benterm.click_peek_of(first);

    benterm.click_reply_of(second);
    assert_eq!(benterm.peeked(), Some(first));
    assert!(benterm.reply_has_keyboard(second));
    benterm.cx.simulate_input("next");
    benterm.cx.simulate_keystrokes("enter");

    benterm.wait_until("the reply", |benterm| {
        benterm.given(second) == "6e.65.78.74.0d."
    });
    assert_eq!(benterm.given(first), "");
    assert_eq!(benterm.peeked(), Some(first));
    assert!(benterm.terminal_has_keyboard(first));
}

#[gpui::test]
fn the_key_for_a_reply_keeps_the_peek_as_a_click_does(cx: &mut TestAppContext) {
    let mut benterm = open(cx);
    let agent = benterm.done_behind("a");
    benterm.click_peek_of(agent);

    benterm.cx.dispatch_action(FocusReply);
    benterm.draw();

    assert_eq!(benterm.peeked(), Some(agent));
    assert!(benterm.reply_has_keyboard(agent));
    benterm.cx.simulate_keystrokes("escape");
    assert!(benterm.terminal_has_keyboard(agent));
}

#[gpui::test]
fn allow_in_the_list_keeps_the_peek(cx: &mut TestAppContext) {
    let mut benterm = open(cx);
    let parent = benterm.focused();
    let agent = benterm.spawn_behind("a", parent);
    benterm.hook(agent, "PermissionRequest");
    benterm.wait_until("the buttons", |benterm| {
        let row = benterm.row_of(agent);
        benterm.drawn(&format!("session-allow-{row}"))
    });
    benterm.click_peek_of(agent);

    let row = benterm.row_of(agent);
    benterm.click(&format!("session-allow-{row}"));

    // Claude Code is answered with 1.
    benterm.wait_until("the answer", |benterm| benterm.given(agent) == "31.");
    assert_eq!(benterm.peeked(), Some(agent));
    assert!(benterm.drawn("peek"));
}

#[gpui::test]
fn the_peek_closes_when_its_program_exits_while_a_field_has_the_keyboard(cx: &mut TestAppContext) {
    let mut benterm = open(cx);
    let agent = benterm.done_behind("a");
    benterm.click_peek_of(agent);
    benterm.click_reply_of(agent);
    assert_eq!(benterm.peeked(), Some(agent));

    let request = SendText {
        pane: ApiPaneId::new(agent.get()),
        text: "q".into(),
    };
    benterm.call(request).unwrap();

    benterm.wait_until("the peek to close", |benterm| benterm.peeked().is_none());
    assert!(!benterm.drawn("peek"));
}

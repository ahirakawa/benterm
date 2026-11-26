//! The panel of a checked report: the row of an agent whose Stop hook
//! passed a report on asks the report's question and opens the panel,
//! which the keyboard closes. The window is a real one, the programs are
//! real, and the reports are made up and come over the real socket.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use benterm_ipc::{
    AgentTarget, Client, ClientError, KillAgent, Method, PaneId as ApiPaneId, ReadOutput,
    ReportChecked, ReportHook,
};
use benterm_layout::PaneId;
use benterm_terminal::PtyConfig;
use benterm_ui::session_rows::SessionRow;
use benterm_ui::settings::ConfigFeed;
use benterm_ui::{BentermWindow, StartedTerminal, bind_keys, ipc};
use gpui::{Modifiers, TestAppContext, VisualTestContext, WindowHandle};
use serde_json::json;

const TIMEOUT: Duration = Duration::from_secs(10);

/// What every pane runs: tells it is ready and reads lines.
const READY: &str = "stty -echo; echo ready; while read line; do :; done";

struct Benterm {
    window: WindowHandle<BentermWindow>,
    cx: VisualTestContext,
    socket: PathBuf,
    _dir: tempfile::TempDir,
}

fn open(cx: &mut TestAppContext) -> Benterm {
    cx.executor().allow_parking();
    cx.update(bind_keys);
    let dir = tempfile::Builder::new()
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap();
    let socket = dir.path().join("benterm.sock");
    let start: ipc::StartTerminal = Rc::new(StartedTerminal::spawn);
    let pty = PtyConfig::command("/bin/sh", ["-c", READY]);
    let feed = ConfigFeed::defaults();
    let window = cx
        .update(|cx| ipc::open_window(pty, start, Some(socket.clone()), feed, cx))
        .unwrap();
    let mut cx = VisualTestContext::from_window(window.into(), cx);
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

    /// Claude Code in `pane` starts a turn, so the pane has a row.
    fn start_turn(&mut self, pane: PaneId) {
        let hook = ReportHook {
            pane: ApiPaneId::new(pane.get()),
            agent: "claude".into(),
            event: "UserPromptSubmit".into(),
            tool_name: None,
            notification_type: None,
            session_id: Some("s1".into()),
            agent_id: None,
            tool_input: None,
            tool_response: None,
        };
        self.call(hook).unwrap();
    }

    fn report(&mut self, pane: PaneId, title: &str, questions: &[&str], problems: &[&str]) {
        self.call(checked(pane, title, questions, problems))
            .unwrap();
    }

    fn rows(&mut self) -> Vec<SessionRow> {
        self.root(|root, cx| root.session_rows(cx))
    }

    fn focused(&mut self) -> PaneId {
        self.root(|root, cx| root.grid(cx).read(cx).focused())
    }

    fn panel(&mut self) -> Option<PaneId> {
        self.root(|root, _| root.report_panel())
    }

    fn pane_has_keyboard(&mut self) -> bool {
        let handle = self.root(|root, cx| root.terminal(cx).read(cx).focus_handle().clone());
        self.cx.update(|window, _| handle.is_focused(window))
    }

    fn draw(&mut self) {
        self.cx.run_until_parked();
        let _ = self
            .window
            .update(&mut self.cx, |_, window, _| window.refresh());
        self.cx.run_until_parked();
    }

    fn drawn(&mut self, selector: &'static str) -> bool {
        self.draw();
        self.cx.debug_bounds(selector).is_some()
    }

    fn click(&mut self, selector: &'static str) {
        self.draw();
        let bounds = self
            .cx
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("{selector} is drawn"));
        self.cx.simulate_click(bounds.center(), Modifiers::none());
        self.cx.run_until_parked();
    }

    /// The text of the first line of the row of `pane`'s report.
    fn question(&mut self, pane: PaneId) -> Option<String> {
        let rows = self.rows();
        let row = rows.iter().find(|row| row.pane == pane)?;
        row.report_line().map(|line| line.text)
    }

    /// A pane with an agent at work, and with a row.
    fn agent(&mut self) -> PaneId {
        let pane = self.focused();
        self.start_turn(pane);
        self.wait_until("the row", |benterm| {
            benterm.rows().iter().any(|row| row.pane == pane)
        });
        pane
    }

    fn screen(&mut self, pane: PaneId) -> String {
        let pane = ApiPaneId::new(pane.get());
        self.call(ReadOutput { pane, lines: None }).unwrap().text
    }

    /// Splits the focused pane and returns the new pane, which has the
    /// keyboard.
    fn split(&mut self) -> PaneId {
        self.cx.simulate_keystrokes("cmd-d");
        let pane = self.focused();
        self.wait_until("the new pane's program", |benterm| {
            benterm.screen(pane).contains("ready")
        });
        pane
    }
}

/// A made-up report of the agent in `pane`.
fn checked(pane: PaneId, title: &str, questions: &[&str], problems: &[&str]) -> ReportChecked {
    let decisions: Vec<_> = questions
        .iter()
        .map(|question| {
            json!({
                "question": question,
                "options": [
                    {"label": "Keep", "effect": "Both stay."},
                    {"label": "Drop", "effect": "Only the new one stays."},
                ],
                "recommend": "Drop",
                "reason": "Nothing uses the old one.",
            })
        })
        .collect();
    let report = json!({
        "title": title,
        "goal": "To have a made-up thing",
        "conclusion": "issues",
        "summary": "The made-up thing is there.",
        "decisions": decisions,
        "verified": ["The made-up tests pass."],
        "unverified": ["Other platforms"],
        "deliverables": [{"path": "docs/plan.md", "summary": "The plan."}],
    });
    ReportChecked {
        pane: ApiPaneId::new(pane.get()),
        agent: "claude".into(),
        session_id: Some("s1".into()),
        report: serde_json::from_value(report).unwrap(),
        problems: problems.iter().map(|problem| problem.to_string()).collect(),
    }
}

#[gpui::test]
fn the_row_asks_the_question_and_opens_the_panel_which_escape_closes(cx: &mut TestAppContext) {
    let mut benterm = open(cx);
    let pane = benterm.agent();

    benterm.report(pane, "First work", &["Keep the old one?"], &[]);

    assert_eq!(benterm.question(pane).as_deref(), Some("Keep the old one?"));
    assert!(benterm.drawn("session-report-line-0"));
    assert!(!benterm.drawn("report-panel"));

    benterm.click("session-report-line-0");

    assert_eq!(benterm.panel(), Some(pane));
    assert!(benterm.drawn("report-panel"));
    assert!(benterm.drawn("report-panel-decision-0"));
    assert!(!benterm.drawn("report-panel-problems"));

    benterm.cx.simulate_keystrokes("escape");

    assert_eq!(benterm.panel(), None);
    assert!(!benterm.drawn("report-panel"));
    assert!(
        benterm.pane_has_keyboard(),
        "the keyboard is back in the pane"
    );
}

/// The keyboard alone opens the panel from the row under the sidebar's
/// cursor, and gets back to that row when the panel closes.
#[gpui::test]
fn the_keyboard_opens_the_panel_from_the_sidebar_and_gets_back_there(cx: &mut TestAppContext) {
    let mut benterm = open(cx);
    let pane = benterm.agent();
    benterm.report(pane, "First work", &["Keep the old one?"], &[]);

    // From the workspace to the row of the agent.
    benterm.cx.simulate_keystrokes("cmd-shift-s down");
    benterm.cx.simulate_keystrokes("r");

    assert_eq!(benterm.panel(), Some(pane));
    assert!(!benterm.pane_has_keyboard());

    benterm.cx.simulate_keystrokes("escape");

    assert_eq!(benterm.panel(), None);
    assert!(
        !benterm.pane_has_keyboard(),
        "the keyboard is back in the sidebar"
    );
    // The cursor is at the row still, so the same key opens it again.
    benterm.cx.simulate_keystrokes("r");
    assert_eq!(benterm.panel(), Some(pane));
}

#[gpui::test]
fn the_row_of_an_agent_without_a_report_asks_nothing_and_opens_nothing(cx: &mut TestAppContext) {
    let mut benterm = open(cx);
    let pane = benterm.agent();

    assert_eq!(benterm.question(pane), None);
    assert!(benterm.drawn("session-0"));
    assert!(!benterm.drawn("session-report-line-0"));
    let opened = benterm
        .window
        .update(&mut benterm.cx, |root, window, cx| {
            root.open_report(pane, window, cx)
        })
        .unwrap();
    assert!(!opened);
    assert_eq!(benterm.panel(), None);
    assert!(!benterm.drawn("report-panel"));
}

#[gpui::test]
fn a_second_report_takes_the_place_of_the_first_in_the_row_and_the_panel(cx: &mut TestAppContext) {
    let mut benterm = open(cx);
    let pane = benterm.agent();
    benterm.report(pane, "First work", &["Keep the old one?"], &[]);
    benterm.click("session-report-line-0");
    assert!(benterm.drawn("report-panel-decision-0"));
    assert!(!benterm.drawn("report-panel-decision-1"));

    benterm.report(pane, "Second work", &["Ship it?", "Tell the others?"], &[]);

    assert_eq!(benterm.question(pane).as_deref(), Some("Ship it?"));
    assert_eq!(benterm.panel(), Some(pane), "the panel stays open");
    assert!(benterm.drawn("report-panel-decision-1"));
    let title = benterm.root(|root, cx| root.report_panel_title(cx));
    assert_eq!(title.as_deref(), Some("Second work"));
}

#[gpui::test]
fn a_report_that_ran_out_of_rounds_shows_what_is_left_in_the_panel(cx: &mut TestAppContext) {
    let mut benterm = open(cx);
    let pane = benterm.agent();

    benterm.report(pane, "Stopped work", &[], &["title is too plain."]);

    assert!(benterm.rows()[0].report_line().unwrap().stopped);
    assert!(benterm.drawn("session-report-line-0"));
    benterm.click("session-report-line-0");
    assert!(benterm.drawn("report-panel-problems"));
    assert!(!benterm.drawn("report-panel-decision-0"));
}

#[gpui::test]
fn the_panel_closes_with_the_pane_of_its_report(cx: &mut TestAppContext) {
    let mut benterm = open(cx);
    let _first = benterm.agent();
    let second = benterm.split();
    benterm.start_turn(second);
    benterm.report(second, "Second work", &["Ship it?"], &[]);
    benterm
        .window
        .update(&mut benterm.cx, |root, window, cx| {
            root.open_report(second, window, cx)
        })
        .unwrap();
    assert_eq!(benterm.panel(), Some(second));

    let kill = KillAgent {
        agent: AgentTarget::Pane(ApiPaneId::new(second.get())),
    };
    benterm.call(kill).unwrap();
    benterm.cx.run_until_parked();

    assert_eq!(benterm.panel(), None);
    assert!(!benterm.drawn("report-panel"));
    assert!(benterm.pane_has_keyboard());
}

/// A click elsewhere takes the keyboard there, and the panel goes away.
#[gpui::test]
fn the_panel_goes_away_when_the_keyboard_goes_elsewhere(cx: &mut TestAppContext) {
    let mut benterm = open(cx);
    let pane = benterm.agent();
    benterm.report(pane, "First work", &["Keep the old one?"], &[]);
    benterm.click("session-report-line-0");
    assert_eq!(benterm.panel(), Some(pane));

    benterm.cx.simulate_keystrokes("cmd-shift-s");

    assert_eq!(benterm.panel(), None);
    assert!(!benterm.drawn("report-panel"));
}

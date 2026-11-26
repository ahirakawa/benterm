//! The session list in the sidebar: a row for every agent of the window,
//! going from a row to its pane, and answering an agent from its row:
//! with a button for a permission, and with a field for text. The window
//! is a real one, the programs are real, and the agents' states come
//! from made-up hook events over the real socket, or from made-up agent
//! screens that the programs print.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use benterm_agent::WaitReason;
use benterm_ipc::{
    AgentTarget, Client, ClientError, Method, PaneId as ApiPaneId, ReadOutput, ReportHook,
    ReportStatus, ReportToParent, SendText, ShowAgent, SpawnAgent,
};
use benterm_layout::PaneId;
use benterm_terminal::PtyConfig;
use benterm_ui::session_rows::{SessionRow, SessionState};
use benterm_ui::settings::ConfigFeed;
use benterm_ui::{BentermWindow, FocusReply, MainView, StartedTerminal, bind_keys, ipc};
use gpui::{Modifiers, TestAppContext, VisualTestContext, WindowHandle};
use serde_json::{Value, json};

const TIMEOUT: Duration = Duration::from_secs(10);

/// What every pane runs: prints `> ` before every line it reads, without
/// echo, so what is on its screen is what it was given.
const ANSWERS: &str = r#"stty -echo; echo ready; while read line; do echo "> $line"; done"#;

/// The script of [`open_asking`]. Only its first run asks; the others,
/// in the panes opened later, run what [`open`]'s panes run.
fn asker(asked: &std::path::Path, screen: &[&str]) -> String {
    let screen = screen.join("\\r\\n");
    let asked = asked.display();
    format!(
        r#"#!/bin/sh
mkdir '{asked}' 2>/dev/null || exec /bin/sh -c '{ANSWERS}'
stty raw -echo
show() {{ printf '\033[H\033[2J{screen}'; }}
show
first=$(dd bs=1 count=1 2>/dev/null | od -An -tx1 | tr -d ' \n')
show
stty min 0 time 10
then=$(dd bs=64 count=1 2>/dev/null | od -An -tx1 | tr -d ' \n')
printf '\r\ngot:%s%s.\r\n' "$first" "$then"
while :; do sleep 1; done
"#
    )
}

/// Claude Code asking whether a command may run.
const CLAUDE_PERMISSION: &[&str] = &[
    " Bash command",
    "   touch approved.txt",
    " Do you want to proceed?",
    " \u{276f} 1. Yes",
    "   2. Yes, and do not ask again for touch commands",
    "   3. No, and tell Claude what to do differently (esc)",
    "",
    " Esc to cancel \u{b7} Tab to amend",
];

/// Codex asking whether a command may run.
const CODEX_PERMISSION: &[&str] = &[
    "  Would you like to run the following command?",
    "  $ touch approved.txt",
    "\u{203a} 1. Yes, proceed (y)",
    "  3. No, and tell Codex what to do differently (esc)",
    "",
    "  Press enter to confirm or esc to cancel",
];

/// Codex asking at start-up whether to trust the folder.
const CODEX_TRUST: &[&str] = &[
    "  Folder access",
    "  Trust this folder?",
    "  Codex can read, edit, and run files here.",
    "",
    "  \u{203a} 1. Trust and continue",
    "    2. Back to Agent Command Center",
    "",
    "  enter continue \u{b7} esc back",
];

/// Claude Code asking at start-up whether to trust the folder.
const CLAUDE_TRUST: &[&str] = &[
    " Accessing workspace:",
    " Claude Code will be able to read, edit, and run files here.",
    "",
    " \u{276f} No, exit",
    "   Yes, I trust this folder",
    "",
    " Enter to confirm \u{b7} Esc to cancel",
];

/// Claude Code's AskUserQuestion.
const CLAUDE_CHOICE: &[&str] = &[
    " Which colour?",
    " \u{276f} 1. Red",
    "   2. Blue",
    "",
    " Enter to select \u{b7} \u{2191}/\u{2193} to navigate \u{b7} Esc to cancel",
];

/// Claude Code at its prompt after a turn.
const CLAUDE_PROMPT: &[&str] = &["\u{25cf} Done.", "", "\u{276f} ", "  ? for shortcuts"];

struct Benterm {
    window: WindowHandle<BentermWindow>,
    cx: VisualTestContext,
    socket: PathBuf,
    _dir: tempfile::TempDir,
}

/// Opens a window as the app does, with a socket of its own.
fn open(cx: &mut TestAppContext) -> Benterm {
    open_running(cx, None)
}

/// Opens a window whose first pane runs a script named `agent`, which
/// prints `screen` as the agent would and then tells what it is given:
/// the first byte, and whatever else comes in the second after it, as
/// `got:` and the bytes in hex. It prints the screen again after the
/// first byte, as an agent asking a second question. The panes opened
/// later run what [`open`]'s panes run.
fn open_asking(cx: &mut TestAppContext, agent: &str, screen: &[&str]) -> Benterm {
    open_running(cx, Some((agent, screen)))
}

/// Opens a window whose first pane runs `script` under the name `name`.
fn open_program(cx: &mut TestAppContext, name: &str, script: &str) -> Benterm {
    open_with(cx, |dir| {
        let path = dir.join(name);
        fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        PtyConfig::command(path, Vec::<String>::new())
    })
}

fn open_running(cx: &mut TestAppContext, asking: Option<(&str, &[&str])>) -> Benterm {
    open_with(cx, |dir| match asking {
        None => PtyConfig::command("/bin/sh", ["-c", ANSWERS]),
        Some((agent, screen)) => {
            let path = dir.join(agent);
            let script = asker(&dir.join("asked"), screen);
            fs::write(&path, script).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
            PtyConfig::command(path, Vec::<String>::new())
        }
    })
}

/// Opens a window whose first pane runs what `pty` makes in the
/// directory of the test.
fn open_with(cx: &mut TestAppContext, pty: impl FnOnce(&std::path::Path) -> PtyConfig) -> Benterm {
    cx.executor().allow_parking();
    cx.update(bind_keys);
    let dir = tempfile::Builder::new()
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap();
    let socket = dir.path().join("benterm.sock");
    let start: ipc::StartTerminal = Rc::new(StartedTerminal::spawn);
    let pty = pty(dir.path());
    let feed = ConfigFeed::defaults();
    let window = cx
        .update(|cx| ipc::open_window(pty, start, Some(socket.clone()), feed, cx))
        .unwrap();
    let mut cx = VisualTestContext::from_window(window.into(), cx);
    // The window is in front, so the pane in view counts as looked at.
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
            // The time passes on the app's clock as well, which the
            // screen of an agent waits on to settle.
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

    fn hook(&mut self, pane: PaneId, agent: &str, event: &str) {
        self.hook_of_tool(pane, agent, event, None);
    }

    /// Codex asks with request_user_input, which may take text.
    fn ask(&mut self, pane: PaneId) {
        self.hook_of_tool(pane, "codex", "PreToolUse", Some("request_user_input"));
    }

    fn hook_of_tool(&mut self, pane: PaneId, agent: &str, event: &str, tool: Option<&str>) {
        let hook = ReportHook {
            pane: ApiPaneId::new(pane.get()),
            agent: agent.into(),
            event: event.into(),
            tool_name: tool.map(str::to_owned),
            notification_type: None,
            session_id: Some("s1".into()),
            agent_id: None,
            tool_input: None,
            tool_response: None,
        };
        self.call(hook).unwrap();
    }

    /// A tool of the agent has run in the conversation `session`, as
    /// the hook after it reports: with what the tool was given and what
    /// it answered.
    fn tool_ran(&mut self, pane: PaneId, agent: &str, session: &str, tool: &str, input: Value) {
        self.call(tool_hook(pane, agent, session, tool, input))
            .unwrap();
    }

    /// Claude Code writes its to-do list: one item for every status.
    fn todos(&mut self, pane: PaneId, session: &str, statuses: &[&str]) {
        let todos: Vec<Value> = statuses
            .iter()
            .enumerate()
            .map(|(at, status)| json!({"content": format!("step {at}"), "status": status}))
            .collect();
        self.tool_ran(
            pane,
            "claude",
            session,
            "TodoWrite",
            json!({"todos": todos}),
        );
    }

    /// What the row of the only agent says of its plan.
    fn plan_summary(&mut self) -> Option<String> {
        let row = self.rows().remove(0);
        row.plan_view().map(|view| view.summary)
    }

    /// Whether `selector` is on the screen as it was last drawn, without
    /// asking for another drawing.
    fn on_screen(&mut self, selector: &'static str) -> bool {
        self.cx.run_until_parked();
        self.cx.debug_bounds(selector).is_some()
    }

    /// Starts an agent behind the window that runs what the panes run.
    fn spawn(&mut self, name: &str, parent: Option<PaneId>) -> u64 {
        let request = SpawnAgent {
            name: name.into(),
            command: vec!["/bin/sh".into(), "-c".into(), ANSWERS.into()],
            parent: parent.map(|parent| ApiPaneId::new(parent.get())),
            ..SpawnAgent::default()
        };
        self.call(request).expect("the agent starts").pane.get()
    }

    /// Starts the only agent behind the window, and returns the id of
    /// its terminal once it has a row.
    fn spawn_behind(&mut self, name: &str, parent: PaneId) -> PaneId {
        let behind = self.spawn(name, Some(parent));
        self.wait_until("the row", |benterm| benterm.rows().len() == 1);
        let row = self.rows().remove(0);
        assert_eq!(row.pane.get(), behind);
        row.pane
    }

    fn screen(&mut self, pane: PaneId) -> String {
        let pane = ApiPaneId::new(pane.get());
        self.call(ReadOutput { pane, lines: None }).unwrap().text
    }

    fn focused(&mut self) -> PaneId {
        self.root(|root, cx| root.grid(cx).read(cx).focused())
    }

    /// Whether the keyboard is in the focused pane of the selected tab.
    fn pane_has_keyboard(&mut self) -> bool {
        let handle = self.root(|root, cx| root.terminal(cx).read(cx).focus_handle().clone());
        self.cx.update(|window, _| handle.is_focused(window))
    }

    fn rows(&mut self) -> Vec<SessionRow> {
        self.root(|root, cx| root.session_rows(cx))
    }

    fn states(&mut self) -> Vec<(PaneId, SessionState)> {
        let rows = self.rows();
        rows.into_iter().map(|row| (row.pane, row.state)).collect()
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

    /// The panes whose rows have the attention mark.
    fn marked_rows(&mut self) -> Vec<PaneId> {
        let rows = self.rows();
        rows.into_iter()
            .filter(|row| row.marked)
            .map(|row| row.pane)
            .collect()
    }

    /// Whether a tab or a workspace of the window has the attention mark.
    fn tab_or_workspace_marked(&mut self) -> bool {
        self.root(|root, cx| {
            root.workspaces().into_iter().any(|workspace| {
                root.workspace_needs_attention(workspace, cx)
                    || root
                        .workspace_tabs(workspace)
                        .is_some_and(|tabs| tabs.read(cx).needs_attention(cx))
            })
        })
    }

    fn reply_text(&mut self, pane: PaneId) -> Option<String> {
        self.root(|root, cx| root.session_reply_text(pane, cx))
    }

    fn board_has_keyboard(&mut self) -> bool {
        let Some(board) = self.root(|root, _| root.progress_board()) else {
            return false;
        };
        let handle = board.read_with(&self.cx, |board, _| board.focus_handle().clone());
        self.cx.update(|window, _| handle.is_focused(window))
    }

    /// What the script of [`open_asking`] was given, in hex, once it
    /// has told.
    fn given(&mut self, pane: PaneId) -> String {
        self.wait_until("the script to tell what it got", |benterm| {
            benterm.screen(pane).contains("got:")
        });
        let screen = self.screen(pane);
        let start = screen.find("got:").unwrap() + "got:".len();
        let end = start + screen[start..].find('.').unwrap();
        screen[start..end].to_owned()
    }

    fn wait_for_state(&mut self, pane: PaneId, state: SessionState) {
        self.wait_until("the state of the row", |benterm| {
            benterm.states() == vec![(pane, state)]
        });
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

/// The report of the hook after a tool of the agent's main thread.
fn tool_hook(pane: PaneId, agent: &str, session: &str, tool: &str, input: Value) -> ReportHook {
    ReportHook {
        pane: ApiPaneId::new(pane.get()),
        agent: agent.into(),
        event: "PostToolUse".into(),
        tool_name: Some(tool.into()),
        notification_type: None,
        session_id: Some(session.into()),
        agent_id: None,
        tool_input: Some(input),
        tool_response: None,
    }
}

#[gpui::test]
fn the_plan_of_an_agent_is_under_its_row_and_follows_its_updates(cx: &mut TestAppContext) {
    let mut benterm = open(cx);
    let agent = benterm.focused();
    benterm.hook(agent, "claude", "UserPromptSubmit");
    benterm.wait_for_state(agent, SessionState::Working);
    benterm.draw();

    benterm.todos(agent, "s1", &["in_progress", "pending"]);

    assert_eq!(
        benterm.plan_summary().as_deref(),
        Some("0 done \u{b7} 1 in progress \u{b7} 1 left")
    );
    // The row is drawn anew for the plan alone.
    assert!(benterm.on_screen("session-plan-0"));
    assert!(benterm.on_screen("session-plan-0-in-progress-0"));
    assert!(benterm.on_screen("session-plan-0-pending-1"));

    benterm.todos(agent, "s1", &["completed", "in_progress"]);

    assert_eq!(
        benterm.plan_summary().as_deref(),
        Some("1 done \u{b7} 1 in progress \u{b7} 0 left")
    );
    assert!(benterm.on_screen("session-plan-0-done-0"));
    assert!(benterm.on_screen("session-plan-0-in-progress-1"));
    assert!(!benterm.on_screen("session-plan-0-pending-1"));
}

#[gpui::test]
fn the_tasks_of_claude_code_are_its_plan_too(cx: &mut TestAppContext) {
    let mut benterm = open(cx);
    let agent = benterm.focused();
    benterm.hook(agent, "claude", "UserPromptSubmit");
    for (id, subject) in [("1", "Read"), ("2", "Write")] {
        let made = ReportHook {
            tool_response: Some(json!({"task": {"id": id, "subject": subject}})),
            ..tool_hook(
                agent,
                "claude",
                "s1",
                "TaskCreate",
                json!({"subject": subject}),
            )
        };
        benterm.call(made).unwrap();
    }

    let done = json!({"taskId": "1", "status": "completed"});
    benterm.tool_ran(agent, "claude", "s1", "TaskUpdate", done);

    assert_eq!(
        benterm.plan_summary().as_deref(),
        Some("1 done \u{b7} 0 in progress \u{b7} 1 left")
    );
}

#[gpui::test]
fn the_plan_of_codex_is_under_its_row(cx: &mut TestAppContext) {
    let mut benterm = open(cx);
    let agent = benterm.focused();
    benterm.hook(agent, "codex", "UserPromptSubmit");
    benterm.wait_for_state(agent, SessionState::Working);
    let plan = json!({"explanation": "Starting", "plan": [
        {"step": "Look", "status": "completed"},
        {"step": "Think", "status": "in_progress"},
        {"step": "Do", "status": "pending"},
    ]});

    benterm.tool_ran(agent, "codex", "s1", "update_plan", plan);

    assert_eq!(
        benterm.plan_summary().as_deref(),
        Some("1 done \u{b7} 1 in progress \u{b7} 1 left")
    );
    assert!(benterm.on_screen("session-plan-0-in-progress-1"));
}

#[gpui::test]
fn an_agent_whose_hooks_tell_no_plan_has_none_under_its_row(cx: &mut TestAppContext) {
    let mut benterm = open(cx);
    let agent = benterm.focused();
    benterm.hook(agent, "claude", "UserPromptSubmit");
    benterm.wait_for_state(agent, SessionState::Working);

    // A tool that keeps no plan, and a plan tool before it has run.
    benterm.tool_ran(agent, "claude", "s1", "Bash", json!({"command": "ls"}));
    let before = ReportHook {
        event: "PreToolUse".into(),
        ..tool_hook(
            agent,
            "claude",
            "s1",
            "TodoWrite",
            json!({"todos": [{"content": "a", "status": "pending"}]}),
        )
    };
    benterm.call(before).unwrap();

    assert_eq!(benterm.plan_summary(), None);
    assert!(!benterm.drawn("session-plan-0"));
}

#[gpui::test]
fn the_plan_of_another_conversation_is_not_under_the_row(cx: &mut TestAppContext) {
    let mut benterm = open(cx);
    let agent = benterm.focused();
    benterm.hook(agent, "claude", "UserPromptSubmit");
    benterm.wait_for_state(agent, SessionState::Working);

    // An agent that the pane's agent runs as a tool, in the middle of
    // the turn, has a conversation of its own.
    benterm.todos(agent, "s2", &["in_progress"]);
    assert_eq!(benterm.plan_summary(), None);

    benterm.todos(agent, "s1", &["completed", "pending"]);
    assert!(benterm.plan_summary().is_some());

    // The next conversation in the pane starts without the plan.
    benterm.hook(agent, "claude", "Stop");
    let next = ReportHook {
        event: "UserPromptSubmit".into(),
        tool_name: None,
        tool_input: None,
        ..tool_hook(agent, "claude", "s2", "", json!(null))
    };
    benterm.call(next).unwrap();
    benterm.wait_until("the next turn", |benterm| {
        benterm.states().first().map(|(_, state)| *state) == Some(SessionState::Working)
    });

    assert_eq!(benterm.plan_summary(), None);
    assert!(!benterm.drawn("session-plan-0"));
}

#[gpui::test]
fn the_plan_of_a_subagent_is_not_that_of_the_agent(cx: &mut TestAppContext) {
    let mut benterm = open(cx);
    let agent = benterm.focused();
    benterm.hook(agent, "claude", "UserPromptSubmit");
    benterm.wait_for_state(agent, SessionState::Working);
    benterm.todos(agent, "s1", &["in_progress", "pending"]);

    let todos = json!({"todos": [{"content": "its own", "status": "pending"}]});
    let of_subagent = ReportHook {
        agent_id: Some("a1".into()),
        ..tool_hook(agent, "claude", "s1", "TodoWrite", todos)
    };
    benterm.call(of_subagent).unwrap();

    assert_eq!(
        benterm.plan_summary().as_deref(),
        Some("0 done \u{b7} 1 in progress \u{b7} 1 left")
    );
}

#[gpui::test]
fn the_list_has_a_row_for_every_agent_and_none_for_other_panes(cx: &mut TestAppContext) {
    let mut benterm = open(cx);
    let claude = benterm.focused();
    let codex = benterm.split();
    let _shell = benterm.split();

    benterm.hook(claude, "claude", "UserPromptSubmit");
    benterm.hook(codex, "codex", "PermissionRequest");

    benterm.wait_until("the two rows", |benterm| benterm.rows().len() == 2);
    assert_eq!(
        benterm.states(),
        vec![
            (claude, SessionState::Working),
            (codex, SessionState::Waiting(WaitReason::Permission)),
        ]
    );
    let kinds: Vec<_> = benterm.rows().iter().map(SessionRow::kind).collect();
    assert_eq!(kinds, vec!["Claude Code", "Codex"]);
    assert!(benterm.drawn("session-list"));
    assert!(benterm.drawn("session-0"));
    assert!(benterm.drawn("session-1"));
    assert!(!benterm.drawn("session-2"));
    // The agent asking for permission has buttons, and no field.
    assert!(!benterm.drawn("session-allow-0"));
    assert!(benterm.drawn("session-allow-1"));
    assert!(benterm.drawn("session-deny-1"));
    assert!(benterm.drawn("session-go-1"));
    assert!(!benterm.drawn("session-reply-0"));
    assert!(!benterm.drawn("session-reply-1"));
}

/// An agent that reported with a status has the status in its row,
/// from the report that had the last one.
#[gpui::test]
fn the_row_of_an_agent_shows_the_status_of_its_last_report(cx: &mut TestAppContext) {
    let mut benterm = open(cx);
    let parent = benterm.focused();
    let child = benterm.spawn_behind("w1", parent);
    assert_eq!(benterm.rows()[0].report_status, None);
    assert!(!benterm.drawn("session-report-0"));
    let report = |benterm: &mut Benterm, text: &str, status| {
        let from = ApiPaneId::new(child.get());
        let text = text.into();
        benterm.call(ReportToParent { from, text, status }).unwrap();
    };

    report(&mut benterm, "cannot build", Some(ReportStatus::Blocked));

    assert_eq!(benterm.rows()[0].report_status, Some(ReportStatus::Blocked));
    assert!(benterm.drawn("session-report-0"));

    report(&mut benterm, "still trying", None);

    assert_eq!(benterm.rows()[0].report_status, Some(ReportStatus::Blocked));
    assert!(benterm.drawn("session-report-0"));
}

/// A made-up program that is no agent tells with OSC 7501 that it
/// cannot go on without an answer, and no hook tells anything.
#[gpui::test]
fn a_program_that_tells_it_has_a_question_has_a_row_that_waits(cx: &mut TestAppContext) {
    let mut benterm = open_program(
        cx,
        "deploy",
        r"printf '\033]7501;state=blocked:kind=question:app=deploy\033\\'; sleep 30",
    );
    let pane = benterm.focused();

    benterm.wait_until("the row", |benterm| benterm.rows().len() == 1);
    assert_eq!(
        benterm.states(),
        vec![(pane, SessionState::Waiting(WaitReason::Question))]
    );
    assert_eq!(benterm.rows()[0].kind(), "Program");
    // A question takes a line of text, and has a way to its pane.
    assert!(benterm.drawn("session-reply-0"));
    assert!(benterm.drawn("session-go-0"));
}

/// A terminal whose OSC 7501 blocked record carries a msg shows that msg
/// in its session row, and drops it once the record changes to working.
#[gpui::test]
fn a_waiting_row_shows_its_osc_7501_msg_and_drops_it_when_working(cx: &mut TestAppContext) {
    // "Allow Bash: cargo test" in base64: QWxsb3cgQmFzaDogY2FyZ28gdGVzdA==
    // "Running" in base64: UnVubmluZw==
    let mut benterm = open_program(
        cx,
        "deploy",
        r"stty raw -echo; printf '\033]7501;state=blocked:kind=permission:msg=QWxsb3cgQmFzaDogY2FyZ28gdGVzdA==\033\\'; dd bs=1 count=1 >/dev/null 2>&1; printf '\033]7501;state=working:msg=UnVubmluZw==\033\\'; sleep 30",
    );
    let pane = benterm.focused();

    benterm.wait_until("the waiting row with msg", |benterm| {
        benterm.rows().len() == 1 && benterm.rows()[0].msg.is_some()
    });
    assert_eq!(
        benterm.rows()[0].msg.as_deref(),
        Some("Allow Bash: cargo test")
    );
    assert!(benterm.drawn("session-msg-0"));

    // Transition to working: the msg disappears from the row.
    let text = "x".to_owned();
    let pane = ApiPaneId::new(pane.get());
    benterm.call(SendText { pane, text }).unwrap();
    benterm.wait_until("the row to switch to working", |benterm| {
        benterm.rows().len() == 1 && benterm.rows()[0].state == SessionState::Working
    });
    assert_eq!(benterm.rows()[0].msg, None);
    assert!(!benterm.drawn("session-msg-0"));
}

/// A made-up Claude Code tells its state with OSC 7501 alone, as the
/// real one does once its query was answered.
#[gpui::test]
fn an_agent_that_tells_its_state_itself_has_a_row_without_any_hook(cx: &mut TestAppContext) {
    let mut benterm = open_program(
        cx,
        "claude",
        r"stty raw -echo min 0 time 20; printf '\033]7501;?\007'; dd bs=256 count=1 >/dev/null 2>&1; stty min 1 time 0; printf '\033]7501;state=blocked:app=claude-code:kind=permission\007'; dd bs=1 count=1 >/dev/null 2>&1; printf '\033]7501;state=clear\007'; sleep 30",
    );
    let pane = benterm.focused();

    benterm.wait_until("the row", |benterm| benterm.rows().len() == 1);
    assert_eq!(
        benterm.states(),
        vec![(pane, SessionState::Waiting(WaitReason::Permission))]
    );
    assert_eq!(benterm.rows()[0].kind(), "Claude Code");
    assert!(benterm.drawn("session-allow-0"));

    // It takes back what it told, as the real one does when it ends,
    // and nothing else has shown a state.
    let text = "x".to_owned();
    let pane = ApiPaneId::new(pane.get());
    benterm.call(SendText { pane, text }).unwrap();
    benterm.wait_until("the row to go", |benterm| benterm.rows().is_empty());
}

#[gpui::test]
fn a_row_is_drawn_anew_as_soon_as_the_state_changes(cx: &mut TestAppContext) {
    let mut benterm = open(cx);
    let pane = benterm.focused();
    benterm.hook(pane, "claude", "UserPromptSubmit");
    benterm.wait_until("the row", |benterm| benterm.drawn("session-0"));
    assert!(!benterm.drawn("session-allow-0"));

    benterm.hook(pane, "claude", "PermissionRequest");

    // Nothing but the news of the agent draws the window again.
    benterm.cx.run_until_parked();
    assert!(benterm.cx.debug_bounds("session-allow-0").is_some());

    benterm.hook(pane, "claude", "SessionEnd");
    benterm.cx.run_until_parked();
    assert!(benterm.cx.debug_bounds("session-0").is_none());
}

#[gpui::test]
fn a_row_of_a_pane_in_a_workspace_behind_goes_to_the_pane(cx: &mut TestAppContext) {
    let mut benterm = open(cx);
    let first_workspace = benterm.root(|root, _| root.selected_workspace());
    // Two panes in the first tab, the agent in the first of them and the
    // keyboard in the second; then another tab, and another workspace.
    let agent = benterm.focused();
    let other = benterm.split();
    assert_ne!(agent, other);
    let agent_tab = benterm.root(|root, cx| root.tabs().read(cx).selected());
    benterm.hook(agent, "claude", "UserPromptSubmit");
    benterm.cx.simulate_keystrokes("cmd-t");
    benterm.cx.simulate_keystrokes("cmd-n");
    assert_ne!(
        benterm.root(|root, _| root.selected_workspace()),
        first_workspace
    );
    benterm.wait_until("the row", |benterm| benterm.rows().len() == 1);

    benterm.click("session-0");

    assert_eq!(
        benterm.root(|root, _| root.selected_workspace()),
        first_workspace
    );
    assert_eq!(
        benterm.root(|root, cx| root.tabs().read(cx).selected()),
        agent_tab
    );
    assert_eq!(benterm.focused(), agent);
    assert!(benterm.pane_has_keyboard());
}

#[gpui::test]
fn a_reply_typed_in_the_row_reaches_the_agent_without_leaving_the_workspace(
    cx: &mut TestAppContext,
) {
    let mut benterm = open(cx);
    let agent = benterm.focused();
    benterm.wait_until("the agent's program", |benterm| {
        benterm.screen(agent).contains("ready")
    });
    benterm.ask(agent);
    benterm.cx.simulate_keystrokes("cmd-n");
    let second_workspace = benterm.root(|root, _| root.selected_workspace());
    benterm.wait_until("the field", |benterm| benterm.drawn("session-reply-0"));

    benterm.click("session-reply-0");
    // Enter in a field with nothing in it sends nothing.
    benterm.cx.simulate_keystrokes("enter");
    benterm.cx.simulate_input("y");
    assert_eq!(
        benterm.root(|root, cx| root.session_reply_text(agent, cx)),
        Some("y".to_owned())
    );
    benterm.cx.simulate_keystrokes("enter");

    benterm.wait_until("the reply on the agent's screen", |benterm| {
        benterm.screen(agent).contains("> y")
    });
    let screen = benterm.screen(agent);
    let lines: Vec<&str> = screen.lines().filter(|line| !line.is_empty()).collect();
    assert_eq!(lines, vec!["ready", "> y"]);
    assert_eq!(
        benterm.root(|root, _| root.selected_workspace()),
        second_workspace
    );
    // The field is empty for the next reply.
    assert_eq!(
        benterm.root(|root, cx| root.session_reply_text(agent, cx)),
        Some(String::new())
    );
}

#[gpui::test]
fn escape_in_the_field_gives_the_keyboard_back_to_the_pane(cx: &mut TestAppContext) {
    let mut benterm = open(cx);
    let agent = benterm.focused();
    benterm.hook(agent, "claude", "Stop");
    // The turn ended in view, so the row shows its field once opened.
    benterm.wait_for_state(agent, SessionState::Idle);

    benterm.cx.dispatch_action(FocusReply);
    assert!(!benterm.pane_has_keyboard());
    benterm.cx.simulate_keystrokes("escape");

    assert!(benterm.pane_has_keyboard());
}

#[gpui::test]
fn the_keyboard_goes_back_to_the_pane_when_the_field_goes_away(cx: &mut TestAppContext) {
    let mut benterm = open(cx);
    let agent = benterm.focused();
    benterm.ask(agent);
    benterm.wait_until("the field", |benterm| benterm.drawn("session-reply-0"));
    benterm.click("session-reply-0");
    assert!(!benterm.pane_has_keyboard());

    benterm.hook(agent, "codex", "PostToolUse");

    benterm.wait_until("the field to go", |benterm| {
        !benterm.drawn("session-reply-0")
    });
    assert!(benterm.pane_has_keyboard());
}

#[gpui::test]
fn an_agent_behind_the_window_has_a_row_that_says_so(cx: &mut TestAppContext) {
    let mut benterm = open(cx);
    let pane = benterm.focused();
    let workspace = benterm.root(|root, _| root.selected_workspace());

    let behind = benterm.spawn("w1", Some(pane));

    benterm.wait_until("the row", |benterm| benterm.rows().len() == 1);
    let row = benterm.rows().remove(0);
    assert_eq!(row.pane.get(), behind);
    assert_eq!(row.label(), "w1");
    assert!(row.is_background());
    assert!(benterm.drawn("session-background-0"));

    // There is no pane to go to: the row opens the peek, and leaves the
    // workspace, the tabs and the focused pane as they are.
    benterm.click("session-0");
    assert_eq!(benterm.root(|root, _| root.selected_workspace()), workspace);
    assert_eq!(
        benterm.root(|root, cx| root.tabs().read(cx).tabs().len()),
        1
    );
    assert_eq!(benterm.focused(), pane);
    assert!(benterm.rows()[0].is_background());
}

#[gpui::test]
fn the_children_of_an_agent_are_under_it(cx: &mut TestAppContext) {
    let mut benterm = open(cx);
    let parent = benterm.focused();
    let other = benterm.split();
    benterm.hook(parent, "claude", "UserPromptSubmit");
    benterm.hook(other, "codex", "UserPromptSubmit");

    let w1 = benterm.spawn("w1", Some(parent));
    let w2 = benterm.spawn("w2", Some(parent));

    benterm.wait_until("the four rows", |benterm| benterm.rows().len() == 4);
    let shown: Vec<(u64, usize)> = benterm
        .rows()
        .iter()
        .map(|row| (row.pane.get(), row.depth))
        .collect();
    assert_eq!(
        shown,
        vec![(parent.get(), 0), (w1, 1), (w2, 1), (other.get(), 0)]
    );
}

#[gpui::test]
fn a_turn_that_ended_out_of_view_is_done_until_its_pane_is_looked_at(cx: &mut TestAppContext) {
    let mut benterm = open(cx);
    let agent = benterm.focused();
    benterm.hook(agent, "claude", "UserPromptSubmit");
    benterm.cx.simulate_keystrokes("cmd-n");

    benterm.hook(agent, "claude", "Stop");

    benterm.wait_until("the turn to end", |benterm| {
        benterm.states() == vec![(agent, SessionState::Done)]
    });
    benterm.click("session-0");
    assert_eq!(benterm.states(), vec![(agent, SessionState::Idle)]);
}

#[gpui::test]
fn what_was_typed_for_one_question_is_not_kept_for_the_next(cx: &mut TestAppContext) {
    let mut benterm = open(cx);
    let agent = benterm.focused();
    benterm.ask(agent);
    benterm.wait_until("the field", |benterm| benterm.drawn("session-reply-0"));
    benterm.click("session-reply-0");
    benterm.cx.simulate_input("1");
    assert_eq!(benterm.reply_text(agent), Some("1".to_owned()));

    // The turn ends before the reply was sent; the row has a field for
    // the next request.
    benterm.hook(agent, "codex", "Stop");

    benterm.wait_for_state(agent, SessionState::Idle);
    assert_eq!(benterm.reply_text(agent), Some(String::new()));
    assert!(benterm.pane_has_keyboard());
}

#[gpui::test]
fn leaving_the_field_keeps_the_progress_board_open(cx: &mut TestAppContext) {
    let mut benterm = open(cx);
    let agent = benterm.focused();
    benterm.ask(agent);
    benterm.cx.simulate_keystrokes("cmd-shift-b");
    assert_eq!(
        benterm.root(|root, _| root.main_view()),
        MainView::ProgressBoard
    );
    benterm.wait_until("the field", |benterm| benterm.drawn("session-reply-0"));

    benterm.click("session-reply-0");
    assert!(!benterm.board_has_keyboard());
    benterm.cx.simulate_keystrokes("escape");

    assert_eq!(
        benterm.root(|root, _| root.main_view()),
        MainView::ProgressBoard
    );
    assert!(benterm.board_has_keyboard());

    // So too when the field goes away under the keyboard.
    benterm.click("session-reply-0");
    benterm.hook(agent, "codex", "PostToolUse");
    benterm.wait_until("the field to go", |benterm| {
        !benterm.drawn("session-reply-0")
    });
    assert_eq!(
        benterm.root(|root, _| root.main_view()),
        MainView::ProgressBoard
    );
    assert!(benterm.board_has_keyboard());
}

#[gpui::test]
fn the_keyboard_alone_gets_to_the_fields_of_the_waiting_agents(cx: &mut TestAppContext) {
    let mut benterm = open(cx);
    let first = benterm.focused();
    benterm.wait_until("the first program", |benterm| {
        benterm.screen(first).contains("ready")
    });
    let second = benterm.split();
    benterm.ask(first);
    benterm.ask(second);
    benterm.wait_until("the fields", |benterm| benterm.drawn("session-reply-1"));

    // The command goes to the first field, and from a field to the next.
    benterm.cx.dispatch_action(FocusReply);
    benterm.cx.simulate_input("a");
    benterm.cx.dispatch_action(FocusReply);
    benterm.cx.simulate_input("b");
    assert_eq!(benterm.reply_text(first), Some("a".to_owned()));
    assert_eq!(benterm.reply_text(second), Some("b".to_owned()));
    // After the last field comes the first again.
    benterm.cx.dispatch_action(FocusReply);
    benterm.cx.simulate_keystrokes("enter");

    benterm.wait_until("the reply on the first agent's screen", |benterm| {
        benterm.screen(first).contains("> a")
    });
    assert!(!benterm.screen(second).contains("> b"));
}

#[gpui::test]
fn an_agent_behind_the_window_that_is_done_marks_its_row_and_nothing_else(cx: &mut TestAppContext) {
    let mut benterm = open(cx);
    let pane = benterm.focused();
    let w1 = benterm.spawn_behind("w1", pane);
    benterm.hook(w1, "claude", "UserPromptSubmit");

    benterm.hook(w1, "claude", "Stop");

    benterm.wait_until("the mark", |benterm| benterm.marked_rows() == vec![w1]);
    assert!(benterm.drawn("session-attention-0"));
    assert!(!benterm.tab_or_workspace_marked());
}

#[gpui::test]
fn the_mark_of_an_agent_behind_the_window_goes_when_its_tab_is_looked_at(cx: &mut TestAppContext) {
    let mut benterm = open(cx);
    let pane = benterm.focused();
    let w1 = benterm.spawn_behind("w1", pane);
    benterm.hook(w1, "claude", "UserPromptSubmit");
    benterm.hook(w1, "claude", "Stop");
    benterm.wait_until("the mark", |benterm| benterm.marked_rows() == vec![w1]);

    benterm
        .call(ShowAgent {
            agent: AgentTarget::Pane(ApiPaneId::new(w1.get())),
            select: true,
        })
        .unwrap();

    assert_eq!(benterm.focused(), w1);
    assert_eq!(benterm.marked_rows(), Vec::<PaneId>::new());
    assert!(!benterm.drawn("session-attention-0"));
    assert!(!benterm.tab_or_workspace_marked());
}

#[gpui::test]
fn an_agent_in_a_pane_has_its_row_and_its_tab_marked_and_unmarked_together(
    cx: &mut TestAppContext,
) {
    let mut benterm = open(cx);
    let agent = benterm.focused();
    benterm.hook(agent, "claude", "UserPromptSubmit");
    benterm.cx.simulate_keystrokes("cmd-t");

    benterm.hook(agent, "claude", "Stop");

    benterm.wait_until("the marks", |benterm| benterm.marked_rows() == vec![agent]);
    let tab = benterm.root(|root, cx| root.tabs().read(cx).tabs()[0]);
    assert!(benterm.root(|root, cx| root.tabs().read(cx).tab_needs_attention(tab, cx)));
    assert!(benterm.drawn("session-attention-0"));

    benterm.click("session-0");

    assert_eq!(benterm.focused(), agent);
    assert_eq!(benterm.marked_rows(), Vec::<PaneId>::new());
    assert!(!benterm.tab_or_workspace_marked());
    assert!(!benterm.drawn("session-attention-0"));
}

#[gpui::test]
fn allow_sends_claude_code_one_key_and_nothing_reaches_the_next_question(cx: &mut TestAppContext) {
    let mut benterm = open_asking(cx, "claude", CLAUDE_PERMISSION);
    let agent = benterm.focused();
    benterm.wait_for_state(agent, SessionState::Waiting(WaitReason::Permission));

    benterm.click("session-allow-0");

    // "1" and nothing after it: no Enter, which the second question,
    // shown at once, would have taken.
    assert_eq!(benterm.given(agent), "31");
}

#[gpui::test]
fn deny_sends_claude_code_escape_alone(cx: &mut TestAppContext) {
    let mut benterm = open_asking(cx, "claude", CLAUDE_PERMISSION);
    let agent = benterm.focused();
    benterm.wait_for_state(agent, SessionState::Waiting(WaitReason::Permission));

    benterm.click("session-deny-0");

    assert_eq!(benterm.given(agent), "1b");
}

#[gpui::test]
fn allow_sends_codex_its_own_key(cx: &mut TestAppContext) {
    let mut benterm = open_asking(cx, "codex", CODEX_PERMISSION);
    let agent = benterm.focused();
    benterm.wait_for_state(agent, SessionState::Waiting(WaitReason::Permission));

    benterm.click("session-allow-0");

    assert_eq!(benterm.given(agent), "79");
}

#[gpui::test]
fn deny_sends_codex_escape_alone(cx: &mut TestAppContext) {
    let mut benterm = open_asking(cx, "codex", CODEX_PERMISSION);
    let agent = benterm.focused();
    benterm.wait_for_state(agent, SessionState::Waiting(WaitReason::Permission));

    benterm.click("session-deny-0");

    assert_eq!(benterm.given(agent), "1b");
}

/// Opens an agent asking to trust its folder, and checks that its row
/// only has the way to its pane, which goes there and sends nothing.
fn a_row_asking_to_trust_only_goes_to_its_pane(
    cx: &mut TestAppContext,
    agent_kind: &str,
    screen: &[&str],
) {
    let mut benterm = open_asking(cx, agent_kind, screen);
    let agent = benterm.focused();
    let workspace = benterm.root(|root, _| root.selected_workspace());
    benterm.wait_for_state(agent, SessionState::Waiting(WaitReason::Trust));
    benterm.cx.simulate_keystrokes("cmd-n");
    assert_ne!(benterm.root(|root, _| root.selected_workspace()), workspace);

    assert!(!benterm.drawn("session-allow-0"));
    assert!(!benterm.drawn("session-deny-0"));
    assert!(!benterm.drawn("session-reply-0"));
    benterm.click("session-go-0");

    assert_eq!(benterm.root(|root, _| root.selected_workspace()), workspace);
    assert_eq!(benterm.focused(), agent);
    assert!(benterm.pane_has_keyboard());
    // Going there sends the agent nothing.
    assert!(!benterm.screen(agent).contains("got:"));
}

#[gpui::test]
fn codex_asking_to_trust_the_folder_only_has_a_way_to_its_pane(cx: &mut TestAppContext) {
    a_row_asking_to_trust_only_goes_to_its_pane(cx, "codex", CODEX_TRUST);
}

#[gpui::test]
fn claude_code_asking_to_trust_the_folder_only_has_a_way_to_its_pane(cx: &mut TestAppContext) {
    a_row_asking_to_trust_only_goes_to_its_pane(cx, "claude", CLAUDE_TRUST);
}

#[gpui::test]
fn a_second_press_before_the_state_changes_sends_nothing(cx: &mut TestAppContext) {
    let mut benterm = open_asking(cx, "claude", CLAUDE_PERMISSION);
    let agent = benterm.focused();
    benterm.wait_for_state(agent, SessionState::Waiting(WaitReason::Permission));

    benterm.click("session-allow-0");
    benterm.click("session-allow-0");
    benterm.click("session-deny-0");

    assert_eq!(benterm.given(agent), "31");
}

#[gpui::test]
fn a_question_of_choices_has_no_field_and_its_button_goes_to_the_pane(cx: &mut TestAppContext) {
    let mut benterm = open_asking(cx, "claude", CLAUDE_CHOICE);
    let agent = benterm.focused();
    let workspace = benterm.root(|root, _| root.selected_workspace());
    benterm.wait_for_state(agent, SessionState::Waiting(WaitReason::Choice));
    benterm.cx.simulate_keystrokes("cmd-n");
    assert_ne!(benterm.root(|root, _| root.selected_workspace()), workspace);

    assert!(!benterm.drawn("session-reply-0"));
    assert!(!benterm.drawn("session-allow-0"));
    benterm.click("session-go-0");

    assert_eq!(benterm.root(|root, _| root.selected_workspace()), workspace);
    assert_eq!(benterm.focused(), agent);
    assert!(benterm.pane_has_keyboard());
    // Going there sends the agent nothing.
    assert!(!benterm.screen(agent).contains("got:"));
}

#[gpui::test]
fn a_row_at_its_prompt_takes_the_next_request_with_enter(cx: &mut TestAppContext) {
    let mut benterm = open_asking(cx, "claude", CLAUDE_PROMPT);
    let agent = benterm.focused();
    benterm.wait_for_state(agent, SessionState::Idle);
    assert!(!benterm.drawn("session-go-0"));

    benterm.cx.dispatch_action(FocusReply);
    benterm
        .cx
        .simulate_input("\u{6b21}\u{306e}\u{4f5c}\u{696d}");
    benterm.cx.simulate_keystrokes("enter");

    let expected: String = "\u{6b21}\u{306e}\u{4f5c}\u{696d}\r"
        .bytes()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    assert_eq!(benterm.given(agent), expected);
}

/// Claude Code at its prompt after a turn, having asked for pastes to
/// be marked, as Claude Code and Codex do.
const CLAUDE_PROMPT_MARKING_PASTES: &[&str] = &[
    "\\033[?2004h\u{25cf} Done.",
    "",
    "\u{276f} ",
    "  ? for shortcuts",
];

/// The bytes of `text` in hex, as [`Benterm::given`] tells them.
fn hex(text: &str) -> String {
    text.bytes().map(|byte| format!("{byte:02x}")).collect()
}

#[gpui::test]
fn a_reply_to_an_agent_that_marks_pastes_is_pasted_and_then_entered(cx: &mut TestAppContext) {
    let mut benterm = open_asking(cx, "claude", CLAUDE_PROMPT_MARKING_PASTES);
    let agent = benterm.focused();
    benterm.wait_for_state(agent, SessionState::Idle);

    benterm.cx.dispatch_action(FocusReply);
    benterm.cx.simulate_input("next");
    benterm.cx.simulate_keystrokes("enter");

    // Enter after the paste is a key of its own, which sends the text
    // off; Codex takes a carriage return in a paste for a line break.
    assert_eq!(benterm.given(agent), hex("\x1b[200~next\x1b[201~\r"));
}

#[gpui::test]
fn a_question_that_may_take_text_has_a_field_and_a_way_to_the_pane(cx: &mut TestAppContext) {
    let mut benterm = open(cx);
    let agent = benterm.focused();
    benterm.ask(agent);

    benterm.wait_for_state(agent, SessionState::Waiting(WaitReason::Question));
    assert!(benterm.drawn("session-reply-0"));
    assert!(benterm.drawn("session-go-0"));
    assert!(!benterm.drawn("session-allow-0"));
}

#[gpui::test]
fn the_keyboard_alone_presses_deny(cx: &mut TestAppContext) {
    let mut benterm = open_asking(cx, "claude", CLAUDE_PERMISSION);
    let agent = benterm.focused();
    benterm.wait_for_state(agent, SessionState::Waiting(WaitReason::Permission));
    benterm.draw();

    // The command goes to the first button, Allow; Tab to the next.
    benterm.cx.dispatch_action(FocusReply);
    benterm.cx.simulate_keystrokes("tab");
    benterm.cx.simulate_keystrokes("enter");

    assert_eq!(benterm.given(agent), "1b");
}

#[gpui::test]
fn space_presses_a_button_too(cx: &mut TestAppContext) {
    let mut benterm = open_asking(cx, "codex", CODEX_PERMISSION);
    let agent = benterm.focused();
    benterm.wait_for_state(agent, SessionState::Waiting(WaitReason::Permission));
    benterm.draw();

    benterm.cx.dispatch_action(FocusReply);
    benterm.cx.simulate_keystrokes("space");

    assert_eq!(benterm.given(agent), "79");
}

/// Opens a window whose first pane is Claude Code at its prompt after a
/// turn the user has seen, and waits for its row to say so. Returns the
/// id of its terminal.
fn open_idle(cx: &mut TestAppContext) -> (Benterm, PaneId) {
    let mut benterm = open_asking(cx, "claude", CLAUDE_PROMPT);
    let agent = benterm.focused();
    benterm.wait_for_state(agent, SessionState::Idle);
    (benterm, agent)
}

impl Benterm {
    /// Whether what is typed goes into the field of the row of `pane`:
    /// types a letter, reads the field, and takes the letter out again.
    fn reply_has_keyboard(&mut self, pane: PaneId) -> bool {
        let before = self.reply_text(pane);
        self.cx.simulate_input("z");
        let after = self.reply_text(pane);
        let typed = after.is_some() && after != before;
        if typed {
            self.cx.simulate_keystrokes("backspace");
        }
        typed
    }
}

#[gpui::test]
fn a_row_at_its_prompt_that_was_seen_has_no_field(cx: &mut TestAppContext) {
    let (mut benterm, _) = open_idle(cx);
    assert!(benterm.drawn("session-0"));
    assert!(!benterm.drawn("session-reply-0"));
}

#[gpui::test]
fn the_key_for_a_reply_opens_the_field_of_a_row_without_one(cx: &mut TestAppContext) {
    let (mut benterm, agent) = open_idle(cx);

    benterm.cx.dispatch_action(FocusReply);

    assert!(benterm.drawn("session-reply-0"));
    assert!(benterm.reply_has_keyboard(agent));
    benterm.cx.simulate_input("next");
    benterm.cx.simulate_keystrokes("enter");
    assert_eq!(benterm.given(agent), "6e6578740d");
    // The field closes, and the keyboard is back in the pane.
    assert!(!benterm.drawn("session-reply-0"));
    assert!(benterm.pane_has_keyboard());
}

#[gpui::test]
fn escape_closes_an_opened_field_and_sends_nothing(cx: &mut TestAppContext) {
    let (mut benterm, agent) = open_idle(cx);
    benterm.cx.dispatch_action(FocusReply);
    benterm.cx.simulate_input("next");

    benterm.cx.simulate_keystrokes("escape");

    assert!(!benterm.drawn("session-reply-0"));
    assert!(benterm.pane_has_keyboard());
    // The field keeps what was typed, as it does until its agent is
    // heard of again. What reaches the agent first is what is sent
    // after, so the Escape sent nothing.
    benterm.cx.dispatch_action(FocusReply);
    assert_eq!(benterm.reply_text(agent), Some("next".to_owned()));
    benterm
        .cx
        .simulate_keystrokes("backspace backspace backspace backspace x enter");
    assert_eq!(benterm.given(agent), "780d");
}

#[gpui::test]
fn an_opened_field_closes_when_the_keyboard_goes_elsewhere(cx: &mut TestAppContext) {
    let (mut benterm, agent) = open_idle(cx);
    benterm.cx.dispatch_action(FocusReply);
    assert!(benterm.reply_has_keyboard(agent));

    // The keyboard goes to the sidebar, with its cursor on the
    // workspace.
    benterm.cx.simulate_keystrokes("cmd-shift-s");

    assert!(!benterm.drawn("session-reply-0"));
}

#[gpui::test]
fn the_cursor_of_the_sidebar_shows_the_field_of_its_row_and_tab_goes_into_it(
    cx: &mut TestAppContext,
) {
    let (mut benterm, agent) = open_idle(cx);
    benterm.cx.simulate_keystrokes("cmd-shift-s");
    assert!(!benterm.drawn("session-reply-0"));

    // From the row of the workspace to the row of the agent.
    benterm.cx.simulate_keystrokes("down");
    assert!(benterm.drawn("session-reply-0"));
    benterm.cx.simulate_keystrokes("tab");

    assert!(benterm.reply_has_keyboard(agent));
    benterm.cx.simulate_input("next");
    benterm.cx.simulate_keystrokes("enter");
    assert_eq!(benterm.given(agent), "6e6578740d");
    assert!(!benterm.drawn("session-reply-0"));
    assert!(benterm.pane_has_keyboard());
}

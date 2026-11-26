//! The agent state of a pane, end to end: made-up agent screens are
//! printed in a real pane, made-up hook events come in over the real
//! socket, and the state is read and watched over the socket.
//!
//! The pane's program prints the screens. The screen names an agent only
//! while one is in front, so for the tests that need that, the program
//! is a script named after the agent.

#[path = "../../benterm_agent/tests/scenes/mod.rs"]
#[allow(dead_code)]
mod scenes;

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use benterm_agent::{AgentState, WaitReason};
use benterm_ipc::{
    AgentActivity, AgentStateChanged, Client, ClientError, ErrorCode, Method, PaneAgentState,
    PaneClosed, PaneId as ApiPaneId, ReadAgentState, ReadOutput, ReportHook, SendText,
    ServerMessage, SplitDirection, SplitPane, SubscribeAgentState, WaitingFor,
};
use benterm_layout::PaneId;
use benterm_terminal::PtyConfig;
use benterm_ui::settings::ConfigFeed;
use benterm_ui::{AgentStatus, BentermWindow, StartedTerminal, bind_keys, ipc};
use gpui::{TestAppContext, VisualTestContext, WindowHandle};

const TIMEOUT: Duration = Duration::from_secs(10);

/// A window whose pane prints the made-up screen it is told the name of.
struct App {
    window: WindowHandle<BentermWindow>,
    cx: VisualTestContext,
    socket: PathBuf,
    pane: PaneId,
    _dir: tempfile::TempDir,
}

/// The last frame of every scene as a file, and the start screen of each
/// agent, which shows its banner.
fn write_frames(dir: &std::path::Path) {
    let write = |name: &str, frame: &[String]| {
        let mut bytes = b"\x1b[H\x1b[2J".to_vec();
        bytes.extend_from_slice(frame.join("\r\n").as_bytes());
        std::fs::write(dir.join(name), bytes).unwrap();
    };
    for scene in scenes::all() {
        let name = scene.name.replace('/', "-");
        write(&name, scene.frames.last().unwrap());
        if name.ends_with("-idle") {
            write(&name.replace("-idle", "-start"), &scene.frames[0]);
        }
    }
    std::fs::write(dir.join("shell"), "\x1b[H\x1b[2J$ ").unwrap();
}

/// A pane whose program is no agent.
fn open(cx: &mut TestAppContext) -> App {
    open_as(cx, None)
}

/// A pane whose program is the script `agent`, or no agent for `None`.
fn open_as(cx: &mut TestAppContext, agent: Option<&str>) -> App {
    cx.executor().allow_parking();
    cx.update(bind_keys);
    let dir = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap();
    write_frames(dir.path());
    let socket = dir.path().join("benterm.sock");
    let script = format!(
        r#"stty -echo; cd '{}'; printf ready; while IFS= read -r name; do [ "$name" = quit ] && exit 0; cat "$name"; done"#,
        dir.path().display()
    );
    let pty = match agent {
        Some(agent) => {
            let path = dir.path().join(agent);
            std::fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            PtyConfig::command(path, Vec::<String>::new())
        }
        None => PtyConfig::command("/bin/sh", ["-c", script.as_str()]),
    };
    let start: ipc::StartTerminal = Rc::new(StartedTerminal::spawn);
    let window = cx
        .update(|cx| ipc::open_window(pty, start, Some(socket.clone()), ConfigFeed::defaults(), cx))
        .unwrap();
    let mut vcx = VisualTestContext::from_window(window.into(), cx);
    vcx.run_until_parked();
    let pane = window
        .update(&mut vcx, |root, _, cx| root.grid(cx).read(cx).focused())
        .unwrap();
    let mut app = App {
        window,
        cx: vcx,
        socket,
        pane,
        _dir: dir,
    };
    app.wait_until("the pane's program to start", |app| {
        app.read().contains("ready")
    });
    app
}

impl App {
    /// Lets the app run and half a tenth of a second pass, on the test's
    /// clock and on the real one, which the detector reads.
    fn pass_time(&mut self) {
        self.cx.run_until_parked();
        // The app draws a frame after every change. The test window does
        // not by itself, and the last frame keeps the view of a closed
        // pane alive.
        self.cx.update(|window, cx| window.draw(cx).clear(cx));
        std::thread::sleep(Duration::from_millis(25));
        self.cx.executor().advance_clock(Duration::from_millis(25));
        self.cx.run_until_parked();
    }

    fn wait_until(&mut self, what: &str, mut done: impl FnMut(&mut Self) -> bool) {
        let deadline = Instant::now() + TIMEOUT;
        while !done(self) {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            self.pass_time();
        }
    }

    /// Makes one call from a thread of its own, as a program in a pane
    /// would, and lets the app run until the answer is there.
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

    fn api_pane(&self) -> ApiPaneId {
        ApiPaneId::new(self.pane.get())
    }

    fn read(&mut self) -> String {
        let pane = self.api_pane();
        self.call(ReadOutput { pane, lines: None }).unwrap().text
    }

    /// Prints the made-up screen `name` in the pane.
    fn show(&mut self, name: &str) {
        let pane = self.api_pane();
        self.call(SendText {
            pane,
            text: format!("{name}\n"),
        })
        .unwrap();
    }

    fn hook(&mut self, agent: &str, session: &str, event: &str) {
        let hook = self.hook_of(agent, session, event);
        self.call(hook).unwrap();
    }

    fn hook_of(&self, agent: &str, session: &str, event: &str) -> ReportHook {
        ReportHook {
            pane: self.api_pane(),
            agent: agent.into(),
            event: event.into(),
            tool_name: None,
            notification_type: None,
            session_id: Some(session.into()),
            agent_id: None,
            tool_input: None,
            tool_response: None,
        }
    }

    /// The agent and its state as the socket gives them.
    fn state(&mut self) -> (Option<String>, AgentActivity, Option<WaitingFor>) {
        let pane = self.api_pane();
        let state = self.call(ReadAgentState { pane }).unwrap();
        assert_eq!(state.pane, pane);
        (state.agent, state.state, state.reason)
    }

    fn wait_for_state(&mut self, agent: Option<&str>, state: AgentActivity) {
        let want = (agent.map(str::to_owned), state);
        let mut last = None;
        let deadline = Instant::now() + TIMEOUT;
        loop {
            let (agent, state, _) = self.state();
            if (agent.clone(), state) == want {
                return;
            }
            last.replace((agent, state));
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {want:?}; the state is {last:?}"
            );
            self.pass_time();
        }
    }

    /// The state as the app itself reads it, without the socket.
    fn status(&mut self) -> AgentStatus {
        self.window
            .update(&mut self.cx, |root, _, cx| {
                root.terminal(cx).read(cx).agent_status()
            })
            .unwrap()
    }

    /// Subscribes on a connection of its own and returns the events it
    /// gets, once the app has answered the subscription.
    fn watch(&mut self) -> Receiver<ServerMessage> {
        let (ready, subscribed) = mpsc::channel();
        let (sender, events) = mpsc::channel();
        let socket = self.socket.clone();
        std::thread::spawn(move || {
            let mut client = Client::connect(socket).unwrap();
            client.call(&SubscribeAgentState {}).unwrap();
            ready.send(()).unwrap();
            while let Ok(event) = client.next_event() {
                if sender.send(event).is_err() {
                    break;
                }
            }
        });
        self.wait_until("the subscription", |_| subscribed.try_recv().is_ok());
        events
    }

    fn next_event(&mut self, events: &Receiver<ServerMessage>) -> ServerMessage {
        let mut event = None;
        self.wait_until("an event", |_| {
            event = events.try_recv().ok();
            event.is_some()
        });
        event.unwrap()
    }
}

fn changed(event: &ServerMessage) -> (Option<String>, AgentActivity, Option<WaitingFor>) {
    let AgentStateChanged(PaneAgentState {
        agent,
        state,
        reason,
        ..
    }) = event.as_event().expect("a state change");
    (agent, state, reason)
}

fn code<T: std::fmt::Debug>(result: Result<T, ClientError>) -> ErrorCode {
    match result {
        Err(ClientError::Api(error)) => error.code,
        other => panic!("expected an error from the app, got {other:?}"),
    }
}

#[gpui::test]
fn a_pane_without_an_agent_is_unknown(cx: &mut TestAppContext) {
    let mut app = open(cx);
    assert_eq!(app.state(), (None, AgentActivity::Unknown, None));
    assert_eq!(app.status().agent, None);
    assert_eq!(app.status().state, AgentState::Unknown);
}

#[gpui::test]
fn the_screen_alone_tells_the_agent_and_its_state(cx: &mut TestAppContext) {
    let mut app = open_as(cx, Some("claude"));

    app.show("claude-start");
    app.wait_for_state(Some("claude"), AgentActivity::Done);
    app.show("claude-working");
    app.wait_for_state(Some("claude"), AgentActivity::Working);
    app.show("claude-permission");
    app.wait_for_state(Some("claude"), AgentActivity::Waiting);
    assert_eq!(app.state().2, Some(WaitingFor::Permission));
    app.show("claude-question");
    app.wait_until("the question", |app| {
        app.state().2 == Some(WaitingFor::Question)
    });
    app.show("claude-done");
    app.wait_for_state(Some("claude"), AgentActivity::Done);
    assert_eq!(app.status().state, AgentState::Done);
}

#[gpui::test]
fn the_screen_tells_codex_too(cx: &mut TestAppContext) {
    let mut app = open_as(cx, Some("codex"));

    app.show("codex-start");
    app.wait_for_state(Some("codex"), AgentActivity::Done);
    app.show("codex-working");
    app.wait_for_state(Some("codex"), AgentActivity::Working);
    app.show("codex-permission");
    app.wait_for_state(Some("codex"), AgentActivity::Waiting);
}

#[gpui::test]
fn hooks_alone_tell_the_agent_and_its_state_at_once(cx: &mut TestAppContext) {
    let mut app = open(cx);

    app.hook("codex", "s1", "SessionStart");
    assert_eq!(
        app.state(),
        (Some("codex".into()), AgentActivity::Unknown, None)
    );
    app.hook("codex", "s1", "UserPromptSubmit");
    assert_eq!(app.state().1, AgentActivity::Working);
    app.hook("codex", "s1", "PermissionRequest");
    assert_eq!(
        app.state(),
        (
            Some("codex".into()),
            AgentActivity::Waiting,
            Some(WaitingFor::Permission)
        )
    );
    assert_eq!(
        app.status().state,
        AgentState::Waiting(WaitReason::Permission)
    );
    app.hook("codex", "s1", "Stop");
    assert_eq!(app.state().1, AgentActivity::Done);
    app.hook("codex", "s1", "SessionEnd");
    assert_eq!(app.state(), (None, AgentActivity::Unknown, None));
}

#[gpui::test]
fn a_turn_that_a_hook_says_failed_is_an_error_over_the_socket(cx: &mut TestAppContext) {
    let mut app = open(cx);

    app.hook("codex", "s1", "UserPromptSubmit");
    app.hook("codex", "s1", "Interrupt");
    assert_eq!(
        app.state(),
        (Some("codex".into()), AgentActivity::Error, None)
    );
    assert_eq!(app.status().state, AgentState::Error);

    app.hook("claude", "s2", "UserPromptSubmit");
    app.hook("claude", "s2", "StopFailure");
    assert_eq!(
        app.state(),
        (Some("claude".into()), AgentActivity::Error, None)
    );
}

#[gpui::test]
fn a_hook_passes_its_tool_and_ignores_subagents_and_other_sessions(cx: &mut TestAppContext) {
    let mut app = open(cx);

    app.hook("claude", "s1", "UserPromptSubmit");
    let question = ReportHook {
        tool_name: Some("AskUserQuestion".into()),
        ..app.hook_of("claude", "s1", "PreToolUse")
    };
    app.call(question).unwrap();
    assert_eq!(app.state().2, Some(WaitingFor::Question));

    // An agent the one in front runs as a tool, and a subagent.
    app.hook("claude", "nested", "Stop");
    let subagent = ReportHook {
        agent_id: Some("a1".into()),
        tool_input: None,
        tool_response: None,
        ..app.hook_of("claude", "s1", "Stop")
    };
    app.call(subagent).unwrap();
    assert_eq!(app.state().2, Some(WaitingFor::Question));

    // An agent the app does not know is answered and changes nothing.
    app.hook("somebody", "s1", "Stop");
    assert_eq!(app.state().1, AgentActivity::Waiting);
}

#[gpui::test]
fn a_hook_wins_at_once_and_a_screen_that_stays_contrary_wins_later(cx: &mut TestAppContext) {
    let mut app = open_as(cx, Some("claude"));
    // The banner has to be seen before the next screen replaces it.
    app.show("claude-start");
    app.wait_for_state(Some("claude"), AgentActivity::Done);
    app.show("claude-working");
    app.wait_for_state(Some("claude"), AgentActivity::Working);

    // The hook is ahead of the screen: it counts at once.
    app.hook("claude", "s1", "Stop");
    assert_eq!(app.state().1, AgentActivity::Done);
    // The screen never follows, so the hook was wrong or is out of date.
    // Nothing more is printed: the app looks again by itself.
    app.wait_for_state(Some("claude"), AgentActivity::Working);
}

#[gpui::test]
fn an_interrupted_turn_ends_by_the_screen_without_any_hook(cx: &mut TestAppContext) {
    let mut app = open_as(cx, Some("claude"));
    app.show("claude-start");
    app.hook("claude", "s1", "UserPromptSubmit");
    app.show("claude-working");
    app.wait_until("the working screen", |app| app.read().contains("interrupt"));
    assert_eq!(app.state().1, AgentActivity::Working);

    // Esc: the prompt comes back and no hook says so.
    app.show("claude-done");
    app.wait_for_state(Some("claude"), AgentActivity::Done);
}

#[gpui::test]
fn the_end_of_the_session_leaves_the_pane_unknown_whatever_stays_on_the_screen(
    cx: &mut TestAppContext,
) {
    let mut app = open_as(cx, Some("claude"));
    app.show("claude-start");
    app.wait_for_state(Some("claude"), AgentActivity::Done);
    app.hook("claude", "s1", "UserPromptSubmit");
    app.hook("claude", "s1", "Stop");

    app.hook("claude", "s1", "SessionEnd");
    assert_eq!(app.state(), (None, AgentActivity::Unknown, None));
    // The banner and the prompt are still there.
    for _ in 0..30 {
        app.pass_time();
    }
    assert_eq!(app.state(), (None, AgentActivity::Unknown, None));

    // A new start after the screen was cleared is seen again.
    app.show("shell");
    app.wait_until("the shell", |app| app.read().contains('$'));
    app.show("claude-start");
    app.wait_for_state(Some("claude"), AgentActivity::Done);
}

#[gpui::test]
fn a_subscriber_is_told_every_change_in_order_and_when_the_pane_closes(cx: &mut TestAppContext) {
    let mut app = open(cx);
    let events = app.watch();

    app.hook("claude", "s1", "UserPromptSubmit");
    app.hook("claude", "s1", "PermissionRequest");
    // The same state again is no change.
    app.hook("claude", "s1", "PermissionRequest");
    app.hook("claude", "s1", "Stop");

    // Naming the agent and its first state are one change.
    let first = app.next_event(&events);
    assert_eq!(
        changed(&first),
        (Some("claude".into()), AgentActivity::Working, None)
    );
    assert_eq!(
        first.as_event::<AgentStateChanged>().unwrap().0.pane,
        app.api_pane()
    );
    let second = app.next_event(&events);
    assert_eq!(
        changed(&second),
        (
            Some("claude".into()),
            AgentActivity::Waiting,
            Some(WaitingFor::Permission)
        )
    );
    let third = app.next_event(&events);
    assert_eq!(changed(&third).1, AgentActivity::Done);

    // A change the screen makes is told as well.
    app.show("claude-start");
    app.show("claude-working");
    let fourth = app.next_event(&events);
    assert_eq!(changed(&fourth).1, AgentActivity::Working);

    // The window closes with its last pane, so a second one is closed.
    let pane = app.api_pane();
    let second = app
        .call(SplitPane {
            pane,
            direction: SplitDirection::Right,
        })
        .unwrap()
        .pane;
    app.call(SendText {
        pane: second,
        text: "quit\n".into(),
    })
    .unwrap();
    let last = app.next_event(&events);
    assert_eq!(last.as_event(), Some(PaneClosed { pane: second }));
}

#[gpui::test]
fn the_agent_calls_name_a_pane_that_is_there(cx: &mut TestAppContext) {
    let mut app = open(cx);
    let missing = ApiPaneId::new(999_999);

    let hook = ReportHook {
        pane: missing,
        ..app.hook_of("claude", "s1", "Stop")
    };
    assert_eq!(code(app.call(hook)), ErrorCode::UnknownPane);
    assert_eq!(
        code(app.call(ReadAgentState { pane: missing })),
        ErrorCode::UnknownPane
    );
}

#[gpui::test]
fn subscribing_twice_on_a_connection_tells_each_change_once(cx: &mut TestAppContext) {
    let mut app = open(cx);
    let (ready, subscribed) = mpsc::channel();
    let (sender, events) = mpsc::channel();
    let socket = app.socket.clone();
    std::thread::spawn(move || {
        let mut client = Client::connect(socket).unwrap();
        client.call(&SubscribeAgentState {}).unwrap();
        client.call(&SubscribeAgentState {}).unwrap();
        ready.send(()).unwrap();
        while let Ok(event) = client.next_event() {
            if sender.send(event).is_err() {
                break;
            }
        }
    });
    app.wait_until("the subscriptions", |_| subscribed.try_recv().is_ok());

    app.hook("claude", "s1", "UserPromptSubmit");
    app.hook("claude", "s1", "Stop");

    let first = app.next_event(&events);
    assert_eq!(changed(&first).1, AgentActivity::Working);
    let second = app.next_event(&events);
    assert_eq!(changed(&second).1, AgentActivity::Done);
}

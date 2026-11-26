//! The rows of agents behind the window whose turns ended unseen: each
//! stays done, with its field for a reply and its mark, until the user
//! looks at its agent, whatever else happens in the window meanwhile.
//!
//! The agents are scripts named `claude`, started behind the window as
//! `agent run` starts them, that show Claude Code's prompt and read what
//! they are sent. Their turns are told with made-up hook events over the
//! real socket.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use benterm_ipc::{
    AgentTarget, Client, ClientError, Method, PaneId as ApiPaneId, ReadOutput, ReportHook,
    SendToAgent, SpawnAgent,
};
use benterm_layout::PaneId;
use benterm_terminal::PtyConfig;
use benterm_ui::session_rows::SessionState;
use benterm_ui::settings::ConfigFeed;
use benterm_ui::{BentermWindow, StartedTerminal, bind_keys, ipc};
use gpui::{TestAppContext, VisualTestContext, WindowHandle};

const TIMEOUT: Duration = Duration::from_secs(20);

const AGENTS: usize = 10;

/// What the window's own pane runs.
const ANSWERS: &str = r#"stty -echo; echo ready; while read line; do echo "> $line"; done"#;

/// A made-up Claude Code at its prompt, which takes every line it reads
/// for a request and shows its prompt again.
const CLAUDE: &str = r#"#!/bin/sh
stty -echo
prompt() { printf '\033[H\033[2J\342\227\217 %s\r\n\r\n\342\235\257 \r\n  ? for shortcuts' "$1"; }
prompt Ready.
while read line; do prompt "Did $line."; done
"#;

struct Bento {
    window: WindowHandle<BentermWindow>,
    cx: VisualTestContext,
    socket: PathBuf,
    claude: PathBuf,
    _dir: tempfile::TempDir,
}

fn open(cx: &mut TestAppContext) -> Bento {
    cx.executor().allow_parking();
    cx.update(bind_keys);
    let dir = tempfile::Builder::new()
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap();
    let socket = dir.path().join("bento.sock");
    let claude = dir.path().join("claude");
    fs::write(&claude, CLAUDE).unwrap();
    fs::set_permissions(&claude, fs::Permissions::from_mode(0o755)).unwrap();
    let start: ipc::StartTerminal = Rc::new(StartedTerminal::spawn);
    let pty = PtyConfig::command("/bin/sh", ["-c", ANSWERS]);
    let feed = ConfigFeed::defaults();
    let window = cx
        .update(|cx| ipc::open_window(pty, start, Some(socket.clone()), feed, cx))
        .unwrap();
    let mut cx = VisualTestContext::from_window(window.into(), cx);
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();
    Bento {
        window,
        cx,
        socket,
        claude,
        _dir: dir,
    }
}

impl Bento {
    fn root<T>(&mut self, read: impl FnOnce(&BentermWindow, &gpui::App) -> T) -> T {
        self.window.read_with(&self.cx, read).unwrap()
    }

    /// Waits for `done`, with the app's clock going on as the real one
    /// does.
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
        self.call(ReportHook {
            pane: ApiPaneId::new(pane.get()),
            agent: "claude".into(),
            event: event.into(),
            tool_name: None,
            notification_type: None,
            session_id: Some(format!("s{}", pane.get())),
            agent_id: None,
            tool_input: None,
            tool_response: None,
        })
        .unwrap();
    }

    fn screen(&mut self, pane: PaneId) -> String {
        let pane = ApiPaneId::new(pane.get());
        self.call(ReadOutput { pane, lines: None }).unwrap().text
    }

    fn focused(&mut self) -> PaneId {
        self.root(|root, cx| root.grid(cx).read(cx).focused())
    }

    /// Starts the made-up Claude Code behind the window as a child of
    /// `parent`, as `agent run` does.
    fn run_agent(&mut self, name: &str, parent: PaneId) -> PaneId {
        let request = SpawnAgent {
            name: name.into(),
            command: vec![self.claude.display().to_string()],
            parent: Some(ApiPaneId::new(parent.get())),
            ..SpawnAgent::default()
        };
        let number = self.call(request).expect("the agent starts").pane.get();
        let mut pane = None;
        self.wait_until("the agent's row", |bento| {
            let rows = bento.root(|root, cx| root.session_rows(cx));
            pane = rows
                .into_iter()
                .find(|row| row.pane.get() == number)
                .map(|row| row.pane);
            pane.is_some()
        });
        pane.unwrap()
    }

    /// Sends the agent a request, and tells its turn as its hooks do.
    fn give_turn(&mut self, name: &str, pane: PaneId) {
        self.call(SendToAgent {
            agent: AgentTarget::Name(name.into()),
            text: format!("job {name}"),
            caller: None,
        })
        .unwrap();
        self.hook(pane, "UserPromptSubmit");
        self.wait_until("the request on the agent's screen", |bento| {
            bento.screen(pane).contains(&format!("Did job {name}."))
        });
        self.hook(pane, "Stop");
    }

    fn state(&mut self, pane: PaneId) -> Option<SessionState> {
        let rows = self.root(|root, cx| root.session_rows(cx));
        rows.into_iter()
            .find(|row| row.pane == pane)
            .map(|row| row.state)
    }

    fn marked(&mut self, pane: PaneId) -> bool {
        self.root(|root, _| root.needs_attention(pane))
    }

    /// Whether the row of `pane` has its field drawn.
    fn has_field(&mut self, pane: PaneId) -> bool {
        self.draw();
        let rows = self.root(|root, cx| root.session_rows(cx));
        let Some(position) = rows.iter().position(|row| row.pane == pane) else {
            return false;
        };
        let selector: &'static str = format!("session-reply-{position}").leak();
        self.cx.debug_bounds(selector).is_some()
    }

    fn draw(&mut self) {
        self.cx.run_until_parked();
        let _ = self
            .window
            .update(&mut self.cx, |_, window, _| window.refresh());
        self.cx.run_until_parked();
    }

    fn peek(&mut self, pane: PaneId) {
        let peeked = self
            .window
            .update(&mut self.cx, |root, window, cx| root.peek(pane, window, cx))
            .unwrap();
        assert!(peeked);
        self.cx.run_until_parked();
    }

    /// Lets `time` pass on both clocks, a little at a time, as it passes
    /// in the app.
    fn pass(&mut self, time: Duration) {
        let step = Duration::from_millis(50);
        let mut passed = Duration::ZERO;
        while passed < time {
            self.cx.run_until_parked();
            std::thread::sleep(Duration::from_millis(2));
            self.cx.executor().advance_clock(step);
            passed += step;
        }
        self.cx.run_until_parked();
    }

    /// Asserts that each of `panes` has a row that is done, its field
    /// and its mark.
    fn assert_unseen(&mut self, panes: &[PaneId], when: &str) {
        for &pane in panes {
            assert_eq!(
                self.state(pane),
                Some(SessionState::Done),
                "the row of {pane} is done {when}"
            );
            assert!(self.marked(pane), "{pane} is marked {when}");
            assert!(
                self.has_field(pane),
                "the row of {pane} has its field {when}"
            );
        }
    }

    /// Ten agents behind the window that each ended a turn, unseen.
    fn ten_agents_done(&mut self) -> Vec<PaneId> {
        let parent = self.focused();
        self.wait_until("the pane's program", |bento| {
            bento.screen(parent).contains("ready")
        });
        let agents: Vec<(String, PaneId)> = (1..=AGENTS)
            .map(|at| {
                let name = format!("a{at}");
                let pane = self.run_agent(&name, parent);
                (name, pane)
            })
            .collect();
        for (name, pane) in &agents {
            self.wait_until("the agent's prompt", |bento| {
                bento.screen(*pane).contains("Ready.")
            });
            self.give_turn(name, *pane);
        }
        let panes: Vec<PaneId> = agents.into_iter().map(|(_, pane)| pane).collect();
        self.wait_until("every row to be done", |bento| {
            panes
                .iter()
                .all(|pane| bento.state(*pane) == Some(SessionState::Done))
        });
        panes
    }
}

#[gpui::test]
fn ten_agents_that_ended_unseen_are_done_with_fields(cx: &mut TestAppContext) {
    let mut bento = open(cx);

    let agents = bento.ten_agents_done();

    bento.assert_unseen(&agents, "after their turns");
}

#[gpui::test]
fn only_the_agents_peeked_at_become_idle(cx: &mut TestAppContext) {
    let mut bento = open(cx);
    let agents = bento.ten_agents_done();
    let (seen, unseen) = agents.split_at(AGENTS / 2);

    for &pane in seen {
        bento.peek(pane);
    }
    let closed = bento.window.update(&mut bento.cx, |root, window, cx| {
        root.close_peek(window, cx)
    });
    assert!(closed.is_ok());
    bento.cx.run_until_parked();

    for &pane in seen {
        assert_eq!(bento.state(pane), Some(SessionState::Idle));
        assert!(!bento.marked(pane));
    }
    bento.assert_unseen(unseen, "after the others were peeked at");

    // Past the looks for stalls, which come every five seconds at most,
    // and past the time a screen takes to settle.
    bento.pass(Duration::from_secs(12));
    bento.assert_unseen(unseen, "after a while");

    bento.cx.deactivate_window();
    bento.pass(Duration::from_secs(1));
    bento.cx.update(|window, _| window.activate_window());
    bento.pass(Duration::from_secs(1));
    bento.assert_unseen(unseen, "after the window went behind and came back");
}

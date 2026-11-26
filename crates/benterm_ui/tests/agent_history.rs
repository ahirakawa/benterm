//! The history of the agents' states in a window: made-up hook events
//! come in over the real socket, and the records are read from the
//! window, over the socket, and from the file beside the session file.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime};

use benterm_ipc::{
    AgentActivity, AgentRecord, Client, ClientError, Method, PaneId as ApiPaneId, ReadAgentHistory,
    ReportHook, SendText, WaitingFor,
};
use benterm_layout::PaneId;
use benterm_session::history::{RETENTION, Record, State, Waiting, history_path};
use benterm_session::{Agent, Node, Session};
use benterm_terminal::PtyConfig;
use benterm_ui::settings::ConfigFeed;
use benterm_ui::{BentermWindow, StartedTerminal, bind_keys, ipc};
use gpui::{TestAppContext, VisualTestContext, WindowHandle};

const TIMEOUT: Duration = Duration::from_secs(10);

/// Where the session file and the history of a test are.
struct Place {
    dir: tempfile::TempDir,
}

impl Place {
    fn new() -> Self {
        let dir = tempfile::Builder::new()
            .permissions(fs::Permissions::from_mode(0o700))
            .tempdir()
            .unwrap();
        Self { dir }
    }

    fn session(&self) -> PathBuf {
        self.dir.path().join("state").join("session.json")
    }

    fn history(&self) -> PathBuf {
        history_path(&self.session())
    }

    /// The lines of the history file after the one with the version.
    fn lines(&self) -> Vec<serde_json::Value> {
        let text = fs::read_to_string(self.history()).unwrap_or_default();
        let lines = text.lines().skip(1);
        lines
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    /// The scrollback keys of the panes of the saved session.
    fn saved_keys(&self) -> Vec<String> {
        fn keys(node: &Node, found: &mut Vec<String>) {
            match node {
                Node::Terminal(terminal) => found.extend(terminal.scrollback.clone()),
                Node::Browser(_) => {}
                Node::Split(split) => {
                    keys(&split.first, found);
                    keys(&split.second, found);
                }
            }
        }
        let session = Session::from_json(&fs::read(self.session()).unwrap()).unwrap();
        let mut found = Vec::new();
        let tabs = session.workspaces.iter().flat_map(|w| &w.tabs);
        tabs.for_each(|tab| keys(&tab.root, &mut found));
        found
    }
}

/// A window that serves a socket of its own.
struct Benterm {
    window: WindowHandle<BentermWindow>,
    cx: VisualTestContext,
    socket: PathBuf,
}

/// Opens a window as the app does, with the session of `place`, on the
/// socket `name` in the place.
fn open(place: &Place, name: &str, cx: &mut TestAppContext) -> Benterm {
    open_running(place, name, "sleep 100", cx)
}

/// Opens a window whose pane runs `script`.
fn open_running(place: &Place, name: &str, script: &str, cx: &mut TestAppContext) -> Benterm {
    cx.executor().allow_parking();
    cx.update(bind_keys);
    let socket = place.dir.path().join(name);
    let start: ipc::StartTerminal = Rc::new(StartedTerminal::spawn);
    let pty = PtyConfig::command("/bin/sh", ["-c", script]);
    let session = Some(place.session());
    let feed = ConfigFeed::defaults();
    let window = cx
        .update(|cx| ipc::open_session_window(pty, start, Some(socket.clone()), session, feed, cx))
        .unwrap();
    let cx = VisualTestContext::from_window(window.into(), cx);
    cx.run_until_parked();
    Benterm { window, cx, socket }
}

impl Benterm {
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

    fn focused(&mut self) -> PaneId {
        self.window
            .update(&mut self.cx, |root, _, cx| root.grid(cx).read(cx).focused())
            .unwrap()
    }

    /// Types an empty line into the pane.
    fn send_line(&mut self) {
        let pane = ApiPaneId::new(self.focused().get());
        let text = "\r".to_owned();
        self.call(SendText { pane, text }).unwrap();
    }

    fn hook(&mut self, pane: PaneId, agent: &str, event: &str) {
        self.hook_of("s1", pane, agent, event);
    }

    fn hook_of(&mut self, session: &str, pane: PaneId, agent: &str, event: &str) {
        let hook = ReportHook {
            pane: ApiPaneId::new(pane.get()),
            agent: agent.into(),
            event: event.into(),
            tool_name: None,
            notification_type: None,
            session_id: Some(session.into()),
            agent_id: None,
            tool_input: None,
            tool_response: None,
        };
        self.call(hook).unwrap();
    }

    /// The history as the app itself reads it, without the socket.
    fn records(&mut self) -> Vec<Record> {
        self.window
            .update(&mut self.cx, |root, _, _| root.agent_history())
            .unwrap()
    }

    /// The history as the socket gives it.
    fn history(&mut self, since_ms: Option<u64>) -> Vec<AgentRecord> {
        self.call(ReadAgentHistory { since_ms }).unwrap().records
    }

    fn wait_for_records(&mut self, count: usize) -> Vec<Record> {
        self.wait_until("the records", |benterm| benterm.records().len() >= count);
        self.records()
    }

    /// Closes the window, as the user does, and lets it go.
    fn close(mut self) {
        // A window that does not save its session has nothing to do
        // when it closes, and says so.
        let _ = self.cx.simulate_close();
        // The test window stays until it is removed, and with it the
        // lock of the session.
        self.window
            .update(&mut self.cx, |_, window, _| window.remove_window())
            .unwrap();
        self.cx.run_until_parked();
    }
}

#[gpui::test]
fn a_turn_of_an_agent_is_recorded_with_its_times(cx: &mut TestAppContext) {
    let place = Place::new();
    let mut benterm = open(&place, "benterm.sock", cx);
    let pane = benterm.focused();
    let started = SystemTime::now();
    assert!(benterm.records().is_empty());

    benterm.hook(pane, "claude", "UserPromptSubmit");
    let before = benterm.wait_for_records(1);
    benterm.hook(pane, "claude", "Stop");
    let records = benterm.wait_for_records(2);

    assert_eq!(records.len(), before.len() + 1);
    let (working, done) = (&records[0], &records[1]);
    assert_eq!(working.state, State::Working);
    assert_eq!(done.state, State::Done);
    assert!(done.at_ms > working.at_ms);
    assert!(working.at() + Duration::from_millis(1) > started);
    assert!(done.at() <= SystemTime::now());
    for record in &records {
        assert_eq!(record.agent, Some(Agent::Claude));
        assert_eq!(record.pane, pane.get());
        assert_eq!(record.key, working.key);
    }
    assert_eq!(working.key.len(), 32);
    // The same two are in the file.
    let lines = place.lines();
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[1]["state"], "done");
    assert_eq!(lines[1]["key"], working.key.as_str());
}

/// A program that is no agent tells its state with OSC 7501: the
/// progress board draws what is recorded here.
#[gpui::test]
fn what_a_program_that_is_no_agent_tells_of_itself_is_recorded(cx: &mut TestAppContext) {
    let place = Place::new();
    let script = r"printf '\033]7501;state=working:app=deploy\033\\'; read line; printf '\033]7501;state=clear\033\\'; sleep 100";
    let mut benterm = open_running(&place, "sock", script, cx);

    let records = benterm.wait_for_records(1);
    assert_eq!(records[0].agent, None);
    assert_eq!(records[0].state, State::Working);
    let told = benterm.history(None);
    assert_eq!(told[0].agent, None);
    assert_eq!(told[0].state, AgentActivity::Working);

    // The program takes back what it told.
    benterm.send_line();
    let records = benterm.wait_for_records(2);
    assert_eq!(records[1].agent, None);
    assert_eq!(records[1].state, State::Unknown);
}

#[gpui::test]
fn what_an_agent_waits_for_and_a_failed_turn_are_recorded(cx: &mut TestAppContext) {
    let place = Place::new();
    let mut benterm = open(&place, "benterm.sock", cx);
    let pane = benterm.focused();

    benterm.hook(pane, "codex", "UserPromptSubmit");
    benterm.hook(pane, "codex", "PermissionRequest");
    benterm.hook(pane, "codex", "Interrupt");
    benterm.hook(pane, "codex", "SessionEnd");

    let records = benterm.wait_for_records(4);
    let states: Vec<_> = records.iter().map(|r| (r.agent, r.state)).collect();
    assert_eq!(
        states,
        [
            (Some(Agent::Codex), State::Working),
            (Some(Agent::Codex), State::Waiting(Waiting::Permission)),
            (Some(Agent::Codex), State::Error),
            // The agent has left the pane.
            (None, State::Unknown),
        ]
    );
}

#[gpui::test]
fn the_error_of_a_run_ends_when_the_next_run_starts(cx: &mut TestAppContext) {
    let place = Place::new();
    let mut benterm = open(&place, "benterm.sock", cx);
    let pane = benterm.focused();

    benterm.hook_of("first", pane, "claude", "UserPromptSubmit");
    benterm.hook_of("first", pane, "claude", "StopFailure");
    benterm.hook_of("second", pane, "claude", "SessionStart");

    let records = benterm.wait_for_records(3);
    let states: Vec<_> = records.iter().map(|r| (r.agent, r.state)).collect();
    assert_eq!(
        states,
        [
            (Some(Agent::Claude), State::Working),
            (Some(Agent::Claude), State::Error),
            // The new run has told nothing of its state yet.
            (Some(Agent::Claude), State::Unknown),
        ]
    );
}

#[gpui::test]
fn the_socket_gives_the_records_in_the_order_of_their_times(cx: &mut TestAppContext) {
    let place = Place::new();
    let mut benterm = open(&place, "benterm.sock", cx);
    let pane = benterm.focused();
    assert!(benterm.history(None).is_empty());

    benterm.hook(pane, "claude", "UserPromptSubmit");
    benterm.hook(pane, "claude", "PermissionRequest");
    benterm.hook(pane, "claude", "Stop");

    // Asked right after the hooks, with no time for the app in between.
    let records = benterm.history(None);
    let states: Vec<_> = records.iter().map(|r| (r.state, r.reason)).collect();
    assert_eq!(
        states,
        [
            (AgentActivity::Working, None),
            (AgentActivity::Waiting, Some(WaitingFor::Permission)),
            (AgentActivity::Done, None),
        ]
    );
    assert!(records[0].at_ms < records[1].at_ms && records[1].at_ms < records[2].at_ms);
    for record in &records {
        assert_eq!(record.agent.as_deref(), Some("claude"));
        assert_eq!(record.pane, ApiPaneId::new(pane.get()));
        assert_eq!(record.key, records[0].key);
    }
    let later = benterm.history(Some(records[0].at_ms));
    assert_eq!(later, records[1..]);
}

#[gpui::test]
fn closing_a_pane_ends_what_its_agent_did(cx: &mut TestAppContext) {
    let place = Place::new();
    let mut benterm = open(&place, "benterm.sock", cx);
    benterm.cx.simulate_keystrokes("cmd-d");
    let pane = benterm.focused();
    benterm.hook(pane, "claude", "UserPromptSubmit");
    benterm.wait_for_records(1);

    benterm.cx.simulate_keystrokes("cmd-w");

    let records = benterm.wait_for_records(2);
    assert_eq!(records[1].state, State::Unknown);
    assert_eq!(records[1].agent, None);
    assert_eq!(records[1].key, records[0].key);
    assert_eq!(records[1].pane, pane.get());
}

#[gpui::test]
fn the_records_and_the_key_of_the_pane_are_there_after_a_restart(cx: &mut TestAppContext) {
    let place = Place::new();
    let mut benterm = open(&place, "benterm.sock", cx);
    let pane = benterm.focused();
    benterm.hook(pane, "claude", "UserPromptSubmit");
    benterm.hook(pane, "claude", "Stop");
    let before = benterm.wait_for_records(2);
    benterm.close();

    // Closing the window ended the agent with it.
    let lines = place.lines();
    assert_eq!(lines.len(), 3);
    assert_eq!(lines[2]["state"], "unknown");
    assert_eq!(place.saved_keys(), [before[0].key.clone()]);

    let mut benterm = open(&place, "again.sock", cx);
    let records = benterm.records();
    assert_eq!(records.len(), 3);
    assert_eq!(records[..2], before[..]);

    // The pane that was made again is the same pane to the history.
    let pane = benterm.focused();
    benterm.hook(pane, "claude", "UserPromptSubmit");
    let records = benterm.wait_for_records(4);
    assert_eq!(records[3].key, before[0].key);
    assert_eq!(records[3].state, State::Working);
    assert_eq!(benterm.history(None).len(), 4);
}

#[gpui::test]
fn quitting_ends_what_the_agents_did(cx: &mut TestAppContext) {
    let place = Place::new();
    let mut benterm = open(&place, "benterm.sock", cx);
    let pane = benterm.focused();
    benterm.hook(pane, "claude", "UserPromptSubmit");
    benterm.wait_for_records(1);

    benterm.cx.cx.quit();

    let lines = place.lines();
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[1]["state"], "unknown");
    assert_eq!(lines[1]["agent"], serde_json::Value::Null);
}

#[gpui::test]
fn quitting_right_after_a_hook_loses_nothing(cx: &mut TestAppContext) {
    let place = Place::new();
    let mut benterm = open(&place, "benterm.sock", cx);
    let pane = benterm.focused();

    benterm.hook(pane, "codex", "UserPromptSubmit");
    benterm.hook(pane, "codex", "Interrupt");
    // The window has not had the time to take what the pane told.
    benterm.cx.cx.quit();

    let states: Vec<_> = place.lines().iter().map(|l| l["state"].clone()).collect();
    assert_eq!(states, ["working", "error", "unknown"]);
}

#[gpui::test]
fn closing_the_last_pane_ends_what_its_agent_did(cx: &mut TestAppContext) {
    let place = Place::new();
    let mut benterm = open(&place, "benterm.sock", cx);
    let pane = benterm.focused();
    benterm.hook(pane, "claude", "UserPromptSubmit");

    benterm.cx.simulate_keystrokes("cmd-w");
    benterm.cx.run_until_parked();

    assert!(
        benterm
            .window
            .update(&mut benterm.cx, |_, _, _| ())
            .is_err(),
        "the window closed"
    );
    let states: Vec<_> = place.lines().iter().map(|l| l["state"].clone()).collect();
    assert_eq!(states, ["working", "unknown"]);
}

fn write_history(path: &Path, lines: &[String]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut text = String::from("{\"agent_history\":1}\n");
    for line in lines {
        text.push_str(line);
        text.push('\n');
    }
    fs::write(path, text).unwrap();
}

fn ms(time: SystemTime) -> u64 {
    let since = time.duration_since(SystemTime::UNIX_EPOCH).unwrap();
    u64::try_from(since.as_millis()).unwrap()
}

fn done_line(at: SystemTime) -> String {
    let at_ms = ms(at);
    format!(
        r#"{{"at_ms":{at_ms},"key":"0123456789abcdef0123456789abcdef","pane":1,"agent":"claude","state":"done","reason":null}}"#
    )
}

fn unknown_line(at: SystemTime) -> String {
    let at_ms = ms(at);
    format!(
        r#"{{"at_ms":{at_ms},"key":"0123456789abcdef0123456789abcdef","pane":1,"agent":null,"state":"unknown","reason":null}}"#
    )
}

#[gpui::test]
fn a_record_older_than_the_retention_time_is_not_read(cx: &mut TestAppContext) {
    let place = Place::new();
    let now = SystemTime::now();
    let day = Duration::from_secs(24 * 60 * 60);
    let old = now - RETENTION - day;
    let lines = [
        done_line(old),
        unknown_line(old + Duration::from_secs(1)),
        done_line(now - day),
        unknown_line(now - day + Duration::from_secs(1)),
    ];
    write_history(&place.history(), &lines);

    let mut benterm = open(&place, "benterm.sock", cx);

    let records = benterm.records();
    assert_eq!(records.len(), 2);
    assert_eq!(records[0].at_ms, ms(now - day));
    assert_eq!(benterm.history(None).len(), 2);
    assert_eq!(place.lines().len(), 2, "the file has lost the old ones");
}

#[gpui::test]
fn a_second_benterm_does_not_write_the_history(cx: &mut TestAppContext) {
    let place = Place::new();
    let mut first = open(&place, "first.sock", cx);
    let pane = first.focused();
    first.hook(pane, "claude", "UserPromptSubmit");
    first.wait_for_records(1);
    let written = fs::read(place.history()).unwrap();

    // The first window holds the lock of the session.
    let mut second = open(&place, "second.sock", &mut first.cx.cx);
    let pane = second.focused();
    second.hook(pane, "codex", "UserPromptSubmit");
    second.hook(pane, "codex", "Stop");

    // It reads what the first wrote, and keeps its own in memory.
    let records = second.wait_for_records(3);
    assert_eq!(records[0].agent, Some(Agent::Claude));
    assert_eq!(records[2].state, State::Done);
    second.close();
    assert_eq!(fs::read(place.history()).unwrap(), written);
    assert_eq!(first.records().len(), 1);
}

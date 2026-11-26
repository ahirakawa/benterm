//! Keys pressed in an agent's terminal by their names, and agents killed,
//! over the socket. The window is a real one, the programs are real, and
//! the requests come in over a real socket.

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use benterm_ipc::{
    AgentTarget, Client, ClientError, ErrorCode, KillAgent, ListAgents, Method,
    PaneId as ApiPaneId, ReadAgent, SendKeys, ShowAgent, SpawnAgent, WaitForAgents,
};
use benterm_terminal::PtyConfig;
use benterm_ui::settings::ConfigFeed;
use benterm_ui::{BentermWindow, StartedTerminal, bind_keys, ipc};
use gpui::{TestAppContext, VisualTestContext, WindowHandle};

const TIMEOUT: Duration = Duration::from_secs(10);

/// Takes what one read of its terminal gets, with the terminal raw, and
/// prints it in hex: what the app wrote in one write arrives in one read.
/// `ready` says it reads.
const ONE_READ: &str = "stty raw -echo; echo ready; dd bs=64 count=1 2>/dev/null | od -An -tx1; \
                        echo .; sleep 100";

struct App {
    window: WindowHandle<BentermWindow>,
    cx: VisualTestContext,
    socket: PathBuf,
    /// The first pane of the window.
    pane: u64,
    dir: tempfile::TempDir,
}

fn open(cx: &mut TestAppContext) -> App {
    cx.executor().allow_parking();
    cx.update(bind_keys);
    let dir = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap();
    let socket = dir.path().join("benterm.sock");
    let start: ipc::StartTerminal = Rc::new(StartedTerminal::spawn);
    let window = cx
        .update(|cx| {
            ipc::open_window(
                PtyConfig::command("/bin/sh", ["-c", "echo ready; sleep 100"]),
                start,
                Some(socket.clone()),
                ConfigFeed::defaults(),
                cx,
            )
        })
        .unwrap();
    let mut vcx = VisualTestContext::from_window(window.into(), cx);
    vcx.run_until_parked();
    let pane = window
        .update(&mut vcx, |root, _, cx| root.grid(cx).read(cx).focused())
        .unwrap()
        .get();
    App {
        window,
        cx: vcx,
        socket,
        pane,
        dir,
    }
}

fn named(name: &str) -> AgentTarget {
    AgentTarget::Name(name.into())
}

fn keys(agent: &str, names: &[&str]) -> SendKeys {
    SendKeys {
        agent: named(agent),
        keys: names.iter().map(|name| name.to_string()).collect(),
    }
}

fn error<T: std::fmt::Debug>(result: Result<T, ClientError>) -> (ErrorCode, String) {
    match result {
        Err(ClientError::Api(error)) => (error.code, error.message),
        other => panic!("expected an error from the app, got {other:?}"),
    }
}

impl App {
    /// Lets the app run and a moment pass, on the test's clock and on the
    /// real one, which the programs follow.
    fn pass_time(&mut self) {
        self.cx.run_until_parked();
        // The test window does not draw by itself, and the last frame
        // keeps the view of a closed pane alive.
        self.cx.update(|window, cx| window.draw(cx).clear(cx));
        std::thread::sleep(Duration::from_millis(20));
        self.cx.executor().advance_clock(Duration::from_millis(20));
        self.cx.run_until_parked();
    }

    fn wait_until(&mut self, what: &str, mut done: impl FnMut(&mut Self) -> bool) {
        let deadline = Instant::now() + TIMEOUT;
        while !done(self) {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            self.pass_time();
        }
    }

    /// Makes one call from a thread of its own and returns where its
    /// answer arrives.
    fn start<M>(&mut self, request: M) -> Receiver<Result<M::Output, ClientError>>
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
        answer
    }

    fn answer<T>(&mut self, answer: &Receiver<T>) -> T {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            if let Ok(result) = answer.try_recv() {
                return result;
            }
            assert!(Instant::now() < deadline, "no answer before the timeout");
            self.pass_time();
        }
    }

    fn call<M>(&mut self, request: M) -> Result<M::Output, ClientError>
    where
        M: Method + Send + 'static,
        M::Output: Send + 'static,
    {
        let answer = self.start(request);
        self.answer(&answer)
    }

    fn read(&mut self, agent: &str) -> String {
        let agent = named(agent);
        self.call(ReadAgent { agent, lines: None }).unwrap().text
    }

    /// Starts an agent that runs `script`, and returns the pane id of its
    /// terminal.
    fn spawn(&mut self, name: &str, script: &str) -> u64 {
        let request = SpawnAgent {
            name: name.into(),
            command: vec!["/bin/sh".into(), "-c".into(), script.into()],
            ..SpawnAgent::default()
        };
        self.call(request).expect("the agent starts").pane.get()
    }

    /// Starts an agent that reads once, and waits until it reads.
    fn spawn_reader(&mut self, name: &str, before: &str) -> u64 {
        let pane = self.spawn(name, &format!("{before}{ONE_READ}"));
        self.wait_until("the agent to read", |app| app.read(name).contains("ready"));
        pane
    }

    /// The bytes the agent `name` got in its one read, in hex.
    fn one_read(&mut self, name: &str) -> String {
        self.wait_until("the read", |app| app.read(name).contains('.'));
        let screen = self.read(name);
        let hex = screen.split("ready").nth(1).unwrap_or_default();
        hex.split('.')
            .next()
            .unwrap_or_default()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn names(&mut self) -> Vec<String> {
        let list = self.call(ListAgents::default()).unwrap();
        list.agents.into_iter().map(|agent| agent.name).collect()
    }

    fn tab_count(&mut self) -> usize {
        self.window
            .update(&mut self.cx, |root, _, cx| {
                root.tabs().read(cx).tabs().len()
            })
            .unwrap()
    }

    /// The marker file a program made with [`App::hung_up_removes`].
    fn marker(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    /// A script that makes a marker file and removes it when its program
    /// is hung up.
    fn hung_up_removes(&self, name: &str) -> String {
        format!(
            "trap 'rm -f {0}; exit' HUP; touch {0}; while :; do sleep 0.05; done",
            self.marker(name).display()
        )
    }
}

#[gpui::test]
fn named_keys_arrive_in_one_write(cx: &mut TestAppContext) {
    let mut app = open(cx);
    app.spawn_reader("w1", "");

    app.call(keys("w1", &["up", "enter"])).unwrap();

    assert_eq!(app.one_read("w1"), "1b 5b 41 0d");
}

#[gpui::test]
fn a_cursor_key_follows_the_cursor_keys_the_program_asked_for(cx: &mut TestAppContext) {
    let mut app = open(cx);
    // DECCKM: the application cursor keys.
    app.spawn_reader("w1", r"printf '\033[?1h'; ");

    app.call(keys("w1", &["up"])).unwrap();

    assert_eq!(app.one_read("w1"), "1b 4f 41");
}

#[gpui::test]
fn an_agent_is_named_for_keys_by_the_number_of_its_pane_too(cx: &mut TestAppContext) {
    let mut app = open(cx);
    let pane = app.spawn_reader("w1", "");

    app.call(SendKeys {
        agent: AgentTarget::Pane(ApiPaneId::new(pane)),
        keys: vec!["ctrl-c".into()],
    })
    .unwrap();

    assert_eq!(app.one_read("w1"), "03");
}

#[gpui::test]
fn an_unknown_key_name_sends_none_of_the_keys(cx: &mut TestAppContext) {
    let mut app = open(cx);
    app.spawn_reader("w1", "");

    let (code, message) = error(app.call(keys("w1", &["up", "nosuchkey"])));

    assert_eq!(code, ErrorCode::InvalidParams);
    assert!(message.contains("\"nosuchkey\""), "{message}");
    // Had the first key been sent, the one read would begin with it.
    app.call(keys("w1", &["tab"])).unwrap();
    assert_eq!(app.one_read("w1"), "09");
}

#[gpui::test]
fn keys_for_an_agent_that_is_not_there_are_an_error(cx: &mut TestAppContext) {
    let mut app = open(cx);

    let (code, _) = error(app.call(keys("nobody", &["escape"])));

    assert_eq!(code, ErrorCode::UnknownAgent);
}

#[gpui::test]
fn killing_an_agent_behind_the_window_hangs_its_program_up(cx: &mut TestAppContext) {
    let mut app = open(cx);
    let script = app.hung_up_removes("alive");
    app.spawn("w1", &script);
    app.spawn("w2", "sleep 100");
    app.wait_until("the program to run", |app| app.marker("alive").exists());

    app.call(KillAgent { agent: named("w1") }).unwrap();

    app.wait_until("the program to end", |app| !app.marker("alive").exists());
    assert_eq!(app.names(), ["w2"]);
}

#[gpui::test]
fn killing_an_agent_in_a_tab_closes_its_pane(cx: &mut TestAppContext) {
    let mut app = open(cx);
    let script = app.hung_up_removes("alive");
    let pane = app.spawn("w1", &script);
    app.wait_until("the program to run", |app| app.marker("alive").exists());
    let tabs = app.tab_count();
    app.call(ShowAgent {
        agent: named("w1"),
        select: false,
    })
    .unwrap();
    assert_eq!(app.tab_count(), tabs + 1);

    app.call(KillAgent {
        agent: AgentTarget::Pane(ApiPaneId::new(pane)),
    })
    .unwrap();

    app.wait_until("the tab to close", |app| app.tab_count() == tabs);
    app.wait_until("the program to end", |app| !app.marker("alive").exists());
    assert!(app.names().is_empty());
    let first = app.pane;
    let panes = app
        .window
        .update(&mut app.cx, |root, _, cx| root.grid(cx).read(cx).panes())
        .unwrap();
    assert_eq!(panes.iter().map(|id| id.get()).collect::<Vec<_>>(), [first]);
}

#[gpui::test]
fn a_wait_for_an_agent_that_is_killed_ends_with_the_agent_closed(cx: &mut TestAppContext) {
    let mut app = open(cx);
    let pane = app.spawn("w1", "sleep 100");
    let wait = app.start(WaitForAgents {
        agent: Some(named("w1")),
        wait_ms: Some(30_000),
        ..WaitForAgents::default()
    });
    app.wait_until("the wait to start", |app| {
        app.window
            .update(&mut app.cx, |root, _, _| root.agent_waits())
            .unwrap()
            == 1
    });

    app.call(KillAgent { agent: named("w1") }).unwrap();

    let waited = app.answer(&wait).expect("the wait ends, not with an error");
    let agent = &waited.agents[0];
    assert_eq!(
        (agent.name.as_deref(), agent.pane.get(), agent.closed),
        (Some("w1"), pane, true)
    );
}

#[gpui::test]
fn killing_an_agent_that_is_not_there_is_an_error(cx: &mut TestAppContext) {
    let mut app = open(cx);

    let (code, _) = error(app.call(KillAgent {
        agent: named("nobody"),
    }));

    assert_eq!(code, ErrorCode::UnknownAgent);
}

#[gpui::test]
fn the_children_of_a_killed_agent_end_with_it_unless_told_to_outlive_it(cx: &mut TestAppContext) {
    let mut app = open(cx);
    let parent = app.spawn("w1", "sleep 100");
    for (name, outlive_parent) in [("child", false), ("stays", true)] {
        app.call(SpawnAgent {
            name: name.into(),
            command: vec!["/bin/sh".into(), "-c".into(), "sleep 100".into()],
            parent: Some(ApiPaneId::new(parent)),
            outlive_parent,
            ..SpawnAgent::default()
        })
        .unwrap();
    }

    app.call(KillAgent { agent: named("w1") }).unwrap();

    app.wait_until("the child to end", |app| app.names() == ["stays"]);
}

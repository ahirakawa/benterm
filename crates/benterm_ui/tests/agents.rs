//! Agents started over the socket: they run in terminals behind the
//! window, are children of the pane that started them, and are sent to,
//! read, waited for and reported from by name. The window is a real one,
//! the programs are real, and the requests come in over a real socket.

#[path = "../../benterm_agent/tests/scenes/mod.rs"]
#[allow(dead_code)]
mod scenes;

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use benterm_ipc::{
    AgentActivity, AgentInfo, AgentTarget, AgentsWaited, Client, ClientError, CreateWorkspace,
    ErrorCode, ListAgents, Method, PaneId as ApiPaneId, ReadAgent, ReadAgentHistory, ReadOutput,
    ReportHook, ReportStatus, ReportToParent, SendText, SendToAgent, ShowAgent, SpawnAgent,
    SplitDirection, SplitPane, WaitForAgents,
};
use benterm_terminal::PtyConfig;
use benterm_ui::settings::ConfigFeed;
use benterm_ui::{BentermWindow, StartedTerminal, bind_keys, ipc};
use gpui::{TestAppContext, VisualTestContext, WindowHandle, px, size};

const TIMEOUT: Duration = Duration::from_secs(10);

/// What the panes of the window run: prints `> ` before every line it
/// reads, so what it was given differs from what was typed, does not
/// echo, and exits when it reads `quit`.
const ANSWERS: &str = r#"stty -echo; echo ready; while read line; do [ "$line" = quit ] && exit 0; echo "> $line"; done"#;

struct App {
    window: WindowHandle<BentermWindow>,
    cx: VisualTestContext,
    socket: PathBuf,
    /// The first pane of the window.
    pane: u64,
    /// How many made-up agents were written.
    scripts: usize,
    dir: tempfile::TempDir,
}

/// The last frame of every made-up agent scene as a file, and the start
/// screen of each agent, which shows its banner.
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
}

fn open(cx: &mut TestAppContext) -> App {
    open_running(cx, "ready", |_| {
        PtyConfig::command("/bin/sh", ["-c", ANSWERS])
    })
}

/// What a shell shows when it waits for a command, in the window
/// [`open_at_a_shell`] opens.
const PROMPT: &str = "shell$";

/// A window whose first pane runs a shell that takes commands, as the
/// pane a user starts agents from does.
fn open_at_a_shell(cx: &mut TestAppContext) -> App {
    open_running(cx, PROMPT, |_| {
        let script = format!("export PS1='{PROMPT} '; exec /bin/sh");
        PtyConfig::command("/bin/sh", ["-c", script.as_str()])
    })
}

/// A window whose first pane runs a made-up Claude Code at its prompt,
/// which writes every line it reads to the file that is returned.
fn open_at_claude(cx: &mut TestAppContext) -> (App, PathBuf) {
    let mut received = PathBuf::new();
    let app = open_running(cx, "", |dir| {
        received = dir.join("received0");
        std::fs::write(&received, "").unwrap();
        let script = format!(
            "cat claude-idle; stty -echo; while read line; do echo \"$line\" >> '{}'; done",
            received.display()
        );
        let path = write_made_up(dir, 0, "claude", &script);
        PtyConfig::command("/bin/sh", [path])
    });
    (app, received)
}

/// Writes a made-up `agent` as the script number `number`: it shows the
/// agent's start screen and then runs `script` in the directory of the
/// frames. The app takes a screen for an agent's only while the agent is
/// the program in front, so this is a script named after it.
fn write_made_up(dir: &std::path::Path, number: usize, agent: &str, script: &str) -> PathBuf {
    let own = dir.join(format!("script{number}"));
    std::fs::create_dir(&own).unwrap();
    let path = own.join(agent);
    let frames = dir.display();
    let script = format!("#!/bin/sh\ncd '{frames}'; cat {agent}-start; sleep 0.3; {script}\n");
    std::fs::write(&path, script).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// Opens a window whose first pane runs what `program` makes for the
/// directory of the test, and waits until the pane shows `shown`.
fn open_running(
    cx: &mut TestAppContext,
    shown: &str,
    program: impl FnOnce(&std::path::Path) -> PtyConfig,
) -> App {
    cx.executor().allow_parking();
    cx.update(bind_keys);
    let dir = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap();
    write_frames(dir.path());
    let socket = dir.path().join("benterm.sock");
    let start: ipc::StartTerminal = Rc::new(StartedTerminal::spawn);
    let window = cx
        .update(|cx| {
            ipc::open_window(
                program(dir.path()),
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
    let mut app = App {
        window,
        cx: vcx,
        socket,
        pane,
        scripts: 0,
        dir,
    };
    app.wait_until("the first pane's program", |app| {
        app.read_pane(app.pane).contains(shown)
    });
    app
}

fn sh(script: &str) -> Vec<String> {
    vec!["/bin/sh".into(), "-c".into(), script.into()]
}

fn named(name: &str) -> AgentTarget {
    AgentTarget::Name(name.into())
}

fn code<T: std::fmt::Debug>(result: Result<T, ClientError>) -> ErrorCode {
    match result {
        Err(ClientError::Api(error)) => error.code,
        other => panic!("expected an error from the app, got {other:?}"),
    }
}

impl App {
    /// Lets the app run and a moment pass, on the test's clock, which the
    /// app's timers follow, and on the real one, which the programs and
    /// the agent detector follow.
    fn pass_time(&mut self) {
        self.cx.run_until_parked();
        // The app draws a frame after every change. The test window does
        // not by itself, and the last frame keeps the view of a closed
        // pane alive. Once the window has closed there is nothing to draw.
        if self.window_is_open() {
            self.cx.update(|window, cx| window.draw(cx).clear(cx));
        }
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

    /// Lets the app run until the answer is there.
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

    fn read_pane(&mut self, pane: u64) -> String {
        let pane = ApiPaneId::new(pane);
        self.call(ReadOutput { pane, lines: None }).unwrap().text
    }

    fn read(&mut self, agent: &str) -> String {
        let agent = named(agent);
        self.call(ReadAgent { agent, lines: None }).unwrap().text
    }

    fn type_into(&mut self, pane: u64, text: &str) {
        let pane = ApiPaneId::new(pane);
        let text = text.into();
        self.call(SendText { pane, text }).unwrap();
    }

    /// Starts an agent that runs `script` as a child of `parent`, and
    /// returns the pane id of its terminal.
    fn spawn(&mut self, name: &str, parent: Option<u64>, script: &str) -> u64 {
        let request = SpawnAgent {
            name: name.into(),
            command: sh(script),
            parent: parent.map(ApiPaneId::new),
            ..SpawnAgent::default()
        };
        let spawned = self.call(request).expect("the agent starts");
        assert_eq!(spawned.name, name);
        spawned.pane.get()
    }

    fn list(&mut self) -> Vec<AgentInfo> {
        self.call(ListAgents::default()).unwrap().agents
    }

    fn names(&mut self) -> Vec<String> {
        self.list().into_iter().map(|agent| agent.name).collect()
    }

    fn tab_count(&mut self) -> usize {
        self.window
            .update(&mut self.cx, |root, _, cx| {
                root.tabs().read(cx).tabs().len()
            })
            .unwrap()
    }

    /// Whether the first pane is outlined as one that needs the user,
    /// because a report for it is held back until the user is done
    /// there. An agent in the pane marks it for its own reasons too.
    fn parent_is_marked(&mut self) -> bool {
        let pane = self.pane;
        self.window
            .update(&mut self.cx, |root, _, cx| {
                let grid = root.grid(cx);
                let grid = grid.read(cx);
                let id = grid.panes().into_iter().find(|id| id.get() == pane);
                id.is_some_and(|id| grid.needs_attention(id)) && root.inbox_is_held(pane)
            })
            .unwrap()
    }

    fn waits(&mut self) -> usize {
        self.window
            .update(&mut self.cx, |root, _, _| root.agent_waits())
            .unwrap()
    }

    fn wait_request(agent: &str, wait_ms: Option<u64>) -> WaitForAgents {
        WaitForAgents {
            agent: Some(named(agent)),
            wait_ms,
            ..WaitForAgents::default()
        }
    }

    /// A made-up Claude Code that shows its banner, and then the screens
    /// of `script` one after the other. The app takes a screen for an
    /// agent's only while the agent is the program in front, so this is a
    /// script named after it; what is returned runs it.
    fn claude(&mut self, script: &str) -> String {
        self.made_up("claude", script)
    }

    /// A made-up `agent` that shows its start screen and then runs
    /// `script` in the directory of the frames.
    fn made_up(&mut self, agent: &str, script: &str) -> String {
        self.scripts += 1;
        let path = write_made_up(self.dir.path(), self.scripts, agent, script);
        format!("exec '{}'", path.display())
    }
}

#[gpui::test]
fn an_agent_starts_behind_the_window_as_a_child_of_the_pane_that_started_it(
    cx: &mut TestAppContext,
) {
    let mut app = open(cx);
    let tabs = app.tab_count();

    let pane = app.spawn("w1", Some(app.pane), "echo started; sleep 100");

    assert_eq!(
        app.list(),
        [AgentInfo {
            name: "w1".into(),
            pane: ApiPaneId::new(pane),
            parent: Some(ApiPaneId::new(app.pane)),
            background: true,
            outlive_parent: false,
            state: AgentActivity::Unknown,
            report_status: None,
            stalled: false,
            changed_at_ms: None,
            msg: None,
            allow_peers: false,
        }]
    );
    assert_eq!(app.tab_count(), tabs);
    assert_ne!(pane, app.pane);
    // The window itself gives the same list, for the views that show it.
    let rows = app
        .window
        .update(&mut app.cx, |root, _, cx| root.agents(cx))
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(
        (
            rows[0].name.as_str(),
            rows[0].pane.get(),
            rows[0].background
        ),
        ("w1", pane, true)
    );
    assert_eq!(rows[0].parent.map(|parent| parent.get()), Some(app.pane));
}

#[gpui::test]
fn an_agent_is_told_the_socket_and_the_id_of_its_terminal(cx: &mut TestAppContext) {
    let mut app = open(cx);

    let pane = app.spawn(
        "w1",
        None,
        r#"[ -S "$BENTERM_SOCKET" ] && printf 'pane=%s.' "$BENTERM_PANE_ID"; sleep 100"#,
    );

    app.wait_until("the agent to print", |app| app.read("w1").contains('.'));
    assert_eq!(app.read("w1"), format!("pane={pane}."));
    // The requests for panes reach a terminal behind the window by its id.
    assert_eq!(app.read_pane(pane), format!("pane={pane}."));
}

#[gpui::test]
fn an_agent_behind_the_window_may_open_a_workspace(cx: &mut TestAppContext) {
    let mut app = open(cx);
    let pane = app.spawn("w1", None, "sleep 100");

    // As benterm new-workspace in the agent's terminal asks.
    let created = app.call(CreateWorkspace {
        caller: Some(ApiPaneId::new(pane)),
        ..CreateWorkspace::default()
    });

    assert!(created.is_ok(), "{created:?}");
    let workspaces = app
        .window
        .update(&mut app.cx, |root, _, _| root.workspaces())
        .unwrap();
    assert_eq!(workspaces.len(), 2);
}

#[gpui::test]
fn an_agent_starts_in_the_directory_and_with_the_path_it_is_given(cx: &mut TestAppContext) {
    let mut app = open(cx);
    let bin = app.dir.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    let tool = bin.join("made-up-agent");
    std::fs::write(&tool, "#!/bin/sh\nprintf 'in=%s.' \"$PWD\"\nsleep 100\n").unwrap();
    std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
    let cwd = app.dir.path().canonicalize().unwrap();

    let request = SpawnAgent {
        name: "w1".into(),
        command: vec!["made-up-agent".into()],
        cwd: Some(cwd.to_str().unwrap().into()),
        env: vec![(
            "PATH".into(),
            format!("{}:/usr/bin:/bin", bin.to_str().unwrap()),
        )],
        ..SpawnAgent::default()
    };
    app.call(request)
        .expect("the program is found on the given PATH");

    app.wait_until("the agent to print", |app| app.read("w1").contains('.'));
    assert_eq!(app.read("w1"), format!("in={}.", cwd.display()));
}

#[gpui::test]
fn the_terminal_behind_has_the_size_of_the_pane_area(cx: &mut TestAppContext) {
    let mut app = open(cx);
    app.cx.simulate_resize(size(px(1100.), px(700.)));
    app.pass_time();
    // The first pane has the pane area to itself.
    let shown = app
        .window
        .update(&mut app.cx, |root, _, cx| {
            root.terminal(cx).read(cx).screen().size
        })
        .unwrap();
    assert_ne!((shown.rows, shown.cols), (24, 80), "needs another size");

    app.spawn("w1", Some(app.pane), "stty size; sleep 100");

    app.wait_until("the size", |app| !app.read("w1").is_empty());
    assert_eq!(app.read("w1"), format!("{} {}", shown.rows, shown.cols));
}

#[gpui::test]
fn showing_an_agent_moves_its_terminal_into_a_new_tab(cx: &mut TestAppContext) {
    let mut app = open(cx);
    let pane = app.spawn("w1", Some(app.pane), "echo kept; cat");
    app.wait_until("the agent to print", |app| app.read("w1").contains("kept"));
    let tabs = app.tab_count();
    let selected = app
        .window
        .update(&mut app.cx, |root, _, cx| root.tabs().read(cx).selected())
        .unwrap();

    let agent = named("w1");
    let shown = app
        .call(ShowAgent {
            agent,
            select: false,
        })
        .unwrap();

    assert_eq!(shown.pane.get(), pane);
    assert_eq!(app.tab_count(), tabs + 1);
    assert!(!app.list()[0].background);
    // The same terminal, with what it showed, is in a pane of the new tab,
    // and the tab on view stays on view.
    let (in_a_tab, still_selected) = app
        .window
        .update(&mut app.cx, |root, _, cx| {
            let tabs = root.tabs().read(cx);
            let in_a_tab = tabs.tabs().into_iter().any(|tab| {
                let grid = tabs.grid(tab).unwrap().read(cx);
                grid.panes().iter().any(|id| id.get() == pane)
            });
            (in_a_tab, tabs.selected() == selected)
        })
        .unwrap();
    assert!(in_a_tab && still_selected);
    assert_eq!(app.read("w1"), "kept");
    // The program was not started again: it still reads.
    app.call(SendToAgent {
        agent: named("w1"),
        text: "more".into(),
        caller: None,
    })
    .unwrap();
    app.wait_until("the echo", |app| app.read("w1").contains("more"));

    // Showing it again changes nothing, and selecting shows its tab.
    let agent = named("w1");
    app.call(ShowAgent {
        agent,
        select: true,
    })
    .unwrap();
    assert_eq!(app.tab_count(), tabs + 1);
    let focused = app
        .window
        .update(&mut app.cx, |root, _, cx| root.grid(cx).read(cx).focused())
        .unwrap();
    assert_eq!(focused.get(), pane);
}

#[gpui::test]
fn text_sent_to_an_agent_by_name_is_entered_and_its_screen_is_read(cx: &mut TestAppContext) {
    let mut app = open(cx);
    app.spawn("w1", Some(app.pane), "exec /bin/sh");

    app.call(SendToAgent {
        agent: named("w1"),
        text: "echo sum=$((40+2))".into(),
        caller: None,
    })
    .unwrap();

    app.wait_until("the sum", |app| app.read("w1").contains("sum=42"));
}

impl App {
    /// A made-up Claude Code that shows the screen of the scene `scene`
    /// and keeps it, and writes every line it reads to the file it
    /// returns, as a program at a dialog takes keys without showing them.
    fn claude_at(&mut self, scene: &str) -> (String, PathBuf) {
        self.made_up_at("claude", scene)
    }

    /// Like [`App::claude_at`], for a made-up Codex.
    fn codex_at(&mut self, scene: &str) -> (String, PathBuf) {
        self.made_up_at("codex", scene)
    }

    fn made_up_at(&mut self, agent: &str, scene: &str) -> (String, PathBuf) {
        let received = self
            .dir
            .path()
            .join(format!("received{}", self.scripts + 1));
        std::fs::write(&received, "").unwrap();
        let script = self.made_up(
            agent,
            &format!(
                "cat {agent}-{scene}; stty -echo; while read line; do echo \"$line\" >> '{}'; done",
                received.display()
            ),
        );
        (script, received)
    }

    fn state_of(&mut self, name: &str) -> Option<AgentActivity> {
        let agents = self.list().into_iter();
        agents
            .filter(|agent| agent.name == name)
            .map(|agent| agent.state)
            .next()
    }

    fn send(&mut self, name: &str, text: &str) -> Result<(), ClientError> {
        self.call(SendToAgent {
            agent: named(name),
            text: text.into(),
            caller: None,
        })
    }
}

#[gpui::test]
fn a_message_for_an_agent_that_asks_for_a_permission_is_refused_and_not_typed(
    cx: &mut TestAppContext,
) {
    let mut app = open(cx);
    let (script, received) = app.claude_at("permission");
    let pane = app.spawn("w1", Some(app.pane), &script);
    app.wait_until("the permission dialog", |app| {
        app.state_of("w1") == Some(AgentActivity::Waiting)
    });

    let refused = app.send("w1", "next");

    let Err(ClientError::Api(error)) = refused else {
        panic!("expected a refusal, got {refused:?}");
    };
    assert_eq!(error.code, ErrorCode::AgentWaiting);
    assert!(error.message.contains("w1"), "{}", error.message);
    assert!(error.message.contains("permission"), "{}", error.message);
    // What is typed after the refusal is read first: nothing came before it.
    app.type_into(pane, "typed\r");
    app.wait_until("the typed line", |_| {
        std::fs::read_to_string(&received)
            .unwrap()
            .contains("typed")
    });
    assert_eq!(std::fs::read_to_string(&received).unwrap(), "typed\n");
}

/// Enter at Codex's choice at start-up would pick an option, which was
/// Cancel in the one that ended an agent.
#[gpui::test]
fn a_message_for_codex_at_a_numbered_choice_is_refused_and_not_typed(cx: &mut TestAppContext) {
    let mut app = open(cx);
    let (script, received) = app.codex_at("choice");
    let pane = app.spawn("w1", Some(app.pane), &script);
    app.wait_until("the choice", |app| {
        app.state_of("w1") == Some(AgentActivity::Waiting)
    });

    let refused = app.send("w1", "next");

    let Err(ClientError::Api(error)) = refused else {
        panic!("expected a refusal, got {refused:?}");
    };
    assert_eq!(error.code, ErrorCode::AgentWaiting);
    assert!(error.message.contains("a choice"), "{}", error.message);
    // What is typed after the refusal is read first: nothing came before it.
    app.type_into(pane, "typed\r");
    app.wait_until("the typed line", |_| {
        std::fs::read_to_string(&received)
            .unwrap()
            .contains("typed")
    });
    assert_eq!(std::fs::read_to_string(&received).unwrap(), "typed\n");
}

#[gpui::test]
fn a_message_for_an_agent_that_is_done_is_entered(cx: &mut TestAppContext) {
    let mut app = open(cx);
    let (script, received) = app.claude_at("done");
    app.spawn("w1", Some(app.pane), &script);
    app.wait_until("the agent at its prompt", |app| {
        app.state_of("w1") == Some(AgentActivity::Done)
    });

    app.send("w1", "next").expect("the message is given");

    app.wait_until("the message", |_| {
        std::fs::read_to_string(&received).unwrap().contains("next")
    });
    assert_eq!(std::fs::read_to_string(&received).unwrap(), "next\n");
}

/// `agent run` cannot put the line that asks to read the rules for
/// workers in a command that may be any program's, so the app puts it
/// before the first message, and before that one only.
#[gpui::test]
fn the_line_for_the_first_message_goes_before_the_first_message_only(cx: &mut TestAppContext) {
    let mut app = open(cx);
    let received = app.dir.path().join("received-lines");
    std::fs::write(&received, "").unwrap();
    let script = format!(
        "stty -echo; while read line; do echo \"$line\" >> '{}'; done",
        received.display()
    );
    let line = "Read /c/worker.md before you start.";
    app.call(SpawnAgent {
        name: "w1".into(),
        command: sh(&script),
        parent: Some(ApiPaneId::new(app.pane)),
        first_message_line: Some(line.into()),
        ..SpawnAgent::default()
    })
    .expect("the agent starts");

    app.send("w1", "first").expect("the first message is given");
    app.send("w1", "second")
        .expect("the second message is given");

    app.wait_until("both messages", |_| {
        std::fs::read_to_string(&received)
            .unwrap()
            .contains("second")
    });
    assert_eq!(
        std::fs::read_to_string(&received).unwrap(),
        format!("{line}\nfirst\nsecond\n")
    );
}

#[gpui::test]
fn a_wait_returns_when_the_agent_is_done(cx: &mut TestAppContext) {
    let mut app = open(cx);
    let script = app.claude("cat claude-working; sleep 2; cat claude-done; sleep 100");
    let pane = app.spawn("w1", Some(app.pane), &script);
    let started = Instant::now();

    let waited = app
        .call(App::wait_request("w1", Some(30_000)))
        .expect("the wait ends before its time runs out");

    assert_eq!(waited.agents.len(), 1);
    let agent = &waited.agents[0];
    assert_eq!(
        (
            agent.name.as_deref(),
            agent.pane.get(),
            agent.closed,
            agent.state
        ),
        (Some("w1"), pane, false, AgentActivity::Done)
    );
    assert!(
        started.elapsed() >= Duration::from_secs(2),
        "not before it is done"
    );
    assert_eq!(app.waits(), 0);
}

#[gpui::test]
fn a_wait_for_an_agent_that_does_not_change_runs_out(cx: &mut TestAppContext) {
    let mut app = open(cx);
    app.spawn("w1", Some(app.pane), "sleep 100");
    let started = Instant::now();

    let result = app.call(App::wait_request("w1", Some(1000)));

    let took = started.elapsed();
    assert_eq!(code(result), ErrorCode::WaitTimedOut);
    assert!(took >= Duration::from_millis(900), "ran out after {took:?}");
    assert!(took < Duration::from_secs(5), "ran out after {took:?}");
    assert_eq!(app.waits(), 0);
    assert_eq!(app.names(), ["w1"], "the agent is as it was");
}

#[gpui::test]
fn a_state_counts_for_a_wait_once_it_has_lasted(cx: &mut TestAppContext) {
    let mut app = open(cx);
    // Done for a moment, then at work again, as when a hook sends an
    // agent on after it has stopped.
    let script = app.claude(
        "cat claude-working; sleep 1; cat claude-done; sleep 1.5; cat claude-working; sleep 2; \
         cat claude-done; sleep 100",
    );
    app.spawn("w1", Some(app.pane), &script);
    for state in [AgentActivity::Working, AgentActivity::Done] {
        app.wait_until("the first turn", |app| app.list()[0].state == state);
    }
    let started = Instant::now();

    let request = WaitForAgents {
        settle_ms: Some(2000),
        ..App::wait_request("w1", Some(30_000))
    };
    let waited = app.call(request).unwrap();

    assert_eq!(waited.agents[0].state, AgentActivity::Done);
    assert!(
        started.elapsed() >= Duration::from_millis(2500),
        "the first done did not last"
    );
}

#[gpui::test]
fn a_wait_ends_when_the_terminal_of_the_agent_closes(cx: &mut TestAppContext) {
    let mut app = open(cx);
    let pane = app.spawn("w1", Some(app.pane), "sleep 1");

    let waited = app.call(App::wait_request("w1", Some(30_000))).unwrap();

    let agent = &waited.agents[0];
    assert_eq!(
        (agent.name.as_deref(), agent.pane.get(), agent.closed),
        (Some("w1"), pane, true)
    );
    // An agent whose program has ended is gone, and so is its name.
    assert!(app.names().is_empty());
    assert_eq!(
        code(app.call(App::wait_request("w1", None))),
        ErrorCode::UnknownAgent
    );
}

#[gpui::test]
fn a_wait_for_the_children_ends_when_none_of_them_works(cx: &mut TestAppContext) {
    let mut app = open(cx);
    let slow = app.claude("cat claude-working; sleep 2; cat claude-done; sleep 100");
    let quick = app.claude("cat claude-working; sleep 1; cat claude-done; sleep 100");
    app.spawn("slow", Some(app.pane), &slow);
    app.spawn("quick", Some(app.pane), &quick);
    app.spawn("gone", Some(app.pane), "sleep 0.5");
    // Not a child of the pane: it is not waited for.
    app.spawn("other", None, "sleep 100");
    let started = Instant::now();

    let waited = app
        .call(WaitForAgents {
            children_of: Some(ApiPaneId::new(app.pane)),
            wait_ms: Some(30_000),
            ..WaitForAgents::default()
        })
        .unwrap();

    let states: Vec<_> = waited
        .agents
        .iter()
        .map(|agent| (agent.name.as_deref().unwrap(), agent.closed, agent.state))
        .collect();
    assert_eq!(
        states,
        [
            ("slow", false, AgentActivity::Done),
            ("quick", false, AgentActivity::Done),
            ("gone", true, AgentActivity::Unknown),
        ]
    );
    assert!(started.elapsed() >= Duration::from_secs(2));
}

#[gpui::test]
fn a_wait_is_given_up_when_its_connection_is_closed(cx: &mut TestAppContext) {
    let mut app = open(cx);
    app.spawn("w1", Some(app.pane), "sleep 100");
    let (sender, connected) = mpsc::channel();
    let socket = app.socket.clone();
    let caller = std::thread::spawn(move || {
        use std::io::Write;
        let mut stream = std::os::unix::net::UnixStream::connect(socket).unwrap();
        let line = r#"{"id":1,"method":"agent.wait","params":{"agent":"w1"}}"#;
        stream.write_all(format!("{line}\n").as_bytes()).unwrap();
        sender.send(()).unwrap();
        stream
    });
    connected.recv().unwrap();
    app.wait_until("the wait to start", |app| app.waits() == 1);

    drop(caller.join().unwrap());

    app.wait_until("the wait to be given up", |app| app.waits() == 0);
}

impl App {
    /// How many reports the window keeps for the first pane, taken and
    /// not written to its terminal.
    fn reports_kept(&mut self) -> usize {
        let pane = self.pane;
        self.window
            .update(&mut self.cx, |root, _, _| root.inbox_len(pane))
            .unwrap()
    }

    /// Lets enough time pass for a report that is not held back to be
    /// written and read, and for a command made of it to run.
    fn let_a_report_arrive(&mut self) {
        for _ in 0..60 {
            self.pass_time();
        }
    }
}

/// What the made-up agent of [`open_at_claude`] has read.
fn read_lines(received: &std::path::Path) -> String {
    std::fs::read_to_string(received).unwrap()
}

/// A report whose text a shell would run: what comes after the `;` is a
/// command, and the `#` makes the rest of the line a comment.
fn report_a_shell_would_run(marker: &std::path::Path) -> String {
    format!("done; touch '{}' #", marker.display())
}

/// The parent's pane runs a shell when the child was started from one,
/// and when the parent's agent ended before the child. The child may be
/// sandboxed, and the shell is not.
#[gpui::test]
fn a_report_is_not_typed_into_a_shell_in_the_parents_pane(cx: &mut TestAppContext) {
    let mut app = open_at_a_shell(cx);
    let child = app.spawn("w1", Some(app.pane), "sleep 100");
    let marker = app.dir.path().join("ran-from-the-report");
    let before = app.read_pane(app.pane);

    let accepted = app.call(ReportToParent {
        from: ApiPaneId::new(child),
        text: report_a_shell_would_run(&marker),
        status: None,
    });

    assert_eq!(accepted.unwrap().parent.get(), app.pane);
    app.let_a_report_arrive();
    assert!(!marker.exists(), "the shell ran the report as a command");
    assert_eq!(app.read_pane(app.pane), before, "no letter and no Enter");
    assert_eq!(app.reports_kept(), 1, "the report stays in the inbox");
}

#[gpui::test]
fn a_report_from_the_mailbox_is_not_typed_into_a_shell_either(cx: &mut TestAppContext) {
    let mut app = open_at_a_shell(cx);
    app.spawn("w1", Some(app.pane), "sleep 100");
    let mailbox = benterm_ui::mailbox::prepare_mailbox(app.dir.path()).unwrap();
    let watched = app.window.update(&mut app.cx, |root, _, _| {
        root.watch_mailbox("w1", mailbox.clone())
    });
    assert!(watched.unwrap(), "the window watches the mailbox");
    let marker = app.dir.path().join("ran-from-the-mailbox");
    let before = app.read_pane(app.pane);

    let message = benterm_ui::mailbox::OutboxMessage {
        to: "parent".into(),
        text: report_a_shell_would_run(&marker),
        status: None,
    };
    let staged = mailbox.join("outbox").join("0001.json.tmp");
    std::fs::write(&staged, serde_json::to_vec(&message).unwrap()).unwrap();
    std::fs::rename(&staged, mailbox.join("outbox").join("0001.json")).unwrap();

    app.wait_until("the report to be taken", |_| {
        mailbox.join("sent").join("0001.json").exists()
    });
    app.let_a_report_arrive();
    assert!(!marker.exists(), "the shell ran the report as a command");
    assert_eq!(app.read_pane(app.pane), before, "no letter and no Enter");
    assert_eq!(app.reports_kept(), 1, "the report stays in the inbox");
}

/// A report that was kept while a shell was in front is the agent's that
/// comes to the front after it, and nothing of it is the shell's.
#[gpui::test]
fn a_report_kept_at_a_shell_goes_in_once_an_agent_is_in_front(cx: &mut TestAppContext) {
    let mut app = open_at_a_shell(cx);
    let child = app.spawn("w1", Some(app.pane), "sleep 100");
    let marker = app.dir.path().join("ran-from-the-report");
    app.call(ReportToParent {
        from: ApiPaneId::new(child),
        text: report_a_shell_would_run(&marker),
        status: None,
    })
    .unwrap();
    app.let_a_report_arrive();
    assert_eq!(app.reports_kept(), 1);

    let (script, received) = app.claude_at("done");
    app.type_into(app.pane, &format!("{script}\r"));

    app.wait_until("the report", |_| {
        read_lines(&received).contains("</benterm>")
    });
    let line = format!(
        "<benterm from=w1 pane={child}> {} </benterm>\n",
        report_a_shell_would_run(&marker)
    );
    assert_eq!(read_lines(&received), line);
    assert!(!marker.exists(), "the shell ran the report as a command");
    app.wait_until("the inbox to empty", |app| app.reports_kept() == 0);
}

/// The agent in the parent's pane gave way to a shell while the report
/// waited: the report was the agent's to read, and is not the shell's.
#[gpui::test]
fn a_report_held_for_an_agent_is_not_typed_into_the_shell_after_it(cx: &mut TestAppContext) {
    let mut app = open_at_a_shell(cx);
    let child = app.spawn("w1", Some(app.pane), "sleep 100");
    // The agent asks for a permission, which holds a report back, and
    // ends when it reads a line.
    let dialog = app.claude("cat claude-permission; read line");
    let dialog = dialog.strip_prefix("exec ").unwrap().to_owned();
    app.type_into(app.pane, &format!("{dialog}\r"));
    app.wait_until("the permission dialog", |app| {
        app.read_pane(app.pane).contains("touch note.txt")
    });
    app.let_a_report_arrive();
    let marker = app.dir.path().join("ran-from-the-report");
    app.call(ReportToParent {
        from: ApiPaneId::new(child),
        text: report_a_shell_would_run(&marker),
        status: None,
    })
    .unwrap();
    app.let_a_report_arrive();
    assert_eq!(app.reports_kept(), 1, "held while the agent asks");

    app.type_into(app.pane, "\r");

    app.wait_until("the shell after the agent", |app| {
        app.read_pane(app.pane).trim_end().ends_with(PROMPT)
    });
    app.let_a_report_arrive();
    assert!(!marker.exists(), "the shell ran the report as a command");
    assert_eq!(app.reports_kept(), 1, "the report stays in the inbox");
}

#[gpui::test]
fn a_report_arrives_once_as_input_of_the_parents_program(cx: &mut TestAppContext) {
    let (mut app, received) = open_at_claude(cx);
    let child = app.spawn("w1", Some(app.pane), "sleep 100");

    let accepted = app
        .call(ReportToParent {
            from: ApiPaneId::new(child),
            text: "done: PR #1".into(),
            status: None,
        })
        .unwrap();

    assert_eq!(accepted.parent.get(), app.pane);
    let line = format!("<benterm from=w1 pane={child}> done: PR #1 </benterm>\n");
    app.wait_until("the report", |_| read_lines(&received).contains(&line));
    for _ in 0..20 {
        app.pass_time();
    }
    assert_eq!(read_lines(&received), line);
    assert_eq!(app.reports_kept(), 0);
}

/// The status of the agent's last report, as `agent.list` tells it.
fn report_status(app: &mut App, name: &str) -> Option<ReportStatus> {
    let list = app.call(ListAgents::default()).unwrap();
    let agent = list.agents.into_iter().find(|agent| agent.name == name);
    agent.expect("the agent is in the list").report_status
}

#[gpui::test]
fn a_report_with_a_status_starts_with_its_word_and_the_agent_keeps_the_status(
    cx: &mut TestAppContext,
) {
    let (mut app, received) = open_at_claude(cx);
    let child = app.spawn("w1", Some(app.pane), "sleep 100");
    let from = ApiPaneId::new(child);
    let report = |text: &str, status| ReportToParent {
        from,
        text: text.into(),
        status,
    };
    assert_eq!(report_status(&mut app, "w1"), None);

    app.call(report("cannot build", Some(ReportStatus::Blocked)))
        .unwrap();

    let open = format!("<benterm from=w1 pane={child}>");
    let blocked = format!("{open} BLOCKED: cannot build </benterm>");
    app.wait_until("the report", |_| read_lines(&received).contains(&blocked));
    assert_eq!(report_status(&mut app, "w1"), Some(ReportStatus::Blocked));

    // A report without a status leaves the last one as it is.
    app.call(report("still trying", None)).unwrap();

    let plain = format!("{open} still trying </benterm>");
    app.wait_until("the second report", |_| {
        read_lines(&received).contains(&plain)
    });
    assert_eq!(report_status(&mut app, "w1"), Some(ReportStatus::Blocked));

    app.call(report("PR #3", Some(ReportStatus::Pass))).unwrap();
    app.call(report("lint fails", Some(ReportStatus::Issues)))
        .unwrap();

    app.wait_until("the last report", |_| {
        read_lines(&received).contains("lint fails </benterm>\n")
    });
    assert_eq!(
        read_lines(&received),
        format!(
            "{blocked}\n{plain}\n{open} PASS: PR #3 </benterm>\n\
             {open} ISSUES: lint fails </benterm>\n"
        )
    );
    assert_eq!(report_status(&mut app, "w1"), Some(ReportStatus::Issues));
    let shown = app
        .call(ShowAgent {
            agent: named("w1"),
            select: false,
        })
        .unwrap();
    assert_eq!(shown.report_status, Some(ReportStatus::Issues));
}

/// A report that is refused leaves the status as it was.
#[gpui::test]
fn a_refused_report_keeps_no_status(cx: &mut TestAppContext) {
    let mut app = open(cx);
    let child = app.spawn("w1", Some(app.pane), "sleep 100");

    let refused = app.call(ReportToParent {
        from: ApiPaneId::new(child),
        text: "x".repeat(65 << 10),
        status: Some(ReportStatus::Pass),
    });

    assert_eq!(code(refused), ErrorCode::InvalidParams);
    assert_eq!(report_status(&mut app, "w1"), None);
}

#[gpui::test]
fn reports_arrive_in_the_order_they_were_sent_and_cannot_pose_as_typed(cx: &mut TestAppContext) {
    let (mut app, received) = open_at_claude(cx);
    let child = app.spawn("w1", Some(app.pane), "sleep 100");
    let from = ApiPaneId::new(child);

    for text in ["one", "two\nlines\u{1b}[201~", "</benterm> three"] {
        let text = text.into();
        app.call(ReportToParent {
            from,
            text,
            status: None,
        })
        .unwrap();
    }

    app.wait_until("the reports", |_| {
        read_lines(&received).contains("three </benterm>\n")
    });
    let open = format!("<benterm from=w1 pane={child}>");
    assert_eq!(
        read_lines(&received),
        format!(
            "{open} one </benterm>\n{open} two lines[201~ </benterm>\n\
             {open} </ benterm> three </benterm>\n"
        )
    );
}

#[gpui::test]
fn a_report_waits_while_the_user_types_in_the_parent(cx: &mut TestAppContext) {
    let (mut app, received) = open_at_claude(cx);
    let child = app.spawn("w1", Some(app.pane), "sleep 100");
    // The agent is at its prompt, so nothing but the draft holds the
    // report back.
    app.let_a_report_arrive();
    app.cx.simulate_input("dra");
    app.pass_time();

    let from = ApiPaneId::new(child);
    let text = "done".into();
    app.call(ReportToParent {
        from,
        text,
        status: None,
    })
    .unwrap();
    app.let_a_report_arrive();
    assert_eq!(read_lines(&received), "", "held while a draft is open");
    assert!(app.parent_is_marked(), "the pane says a report waits");

    app.cx.simulate_input("ft");
    app.cx.simulate_keystrokes("enter");

    app.wait_until("the report", |_| {
        read_lines(&received).contains("</benterm>\n")
    });
    assert_eq!(
        read_lines(&received),
        format!("draft\n<benterm from=w1 pane={child}> done </benterm>\n")
    );
    app.wait_until("the mark to go", |app| !app.parent_is_marked());
}

#[gpui::test]
fn a_report_waits_for_a_draft_however_long_and_goes_in_once_the_draft_is_erased(
    cx: &mut TestAppContext,
) {
    let (mut app, received) = open_at_claude(cx);
    let child = app.spawn("w1", Some(app.pane), "sleep 100");
    // The agent is at its prompt, so nothing but the draft holds the
    // report back.
    app.let_a_report_arrive();
    app.cx.simulate_input("dr");
    app.pass_time();
    let from = ApiPaneId::new(child);
    let text = "done".into();
    app.call(ReportToParent {
        from,
        text,
        status: None,
    })
    .unwrap();

    app.cx.executor().advance_clock(Duration::from_secs(600));
    for _ in 0..10 {
        app.pass_time();
    }
    assert_eq!(app.reports_kept(), 1, "no time ends the hold");
    assert!(app.parent_is_marked());

    // The user takes the draft back, letter by letter.
    app.cx.simulate_keystrokes("backspace");
    for _ in 0..20 {
        app.pass_time();
    }
    assert_eq!(app.reports_kept(), 1, "a letter is left");
    app.cx.simulate_keystrokes("backspace");

    app.wait_until("the report", |_| {
        read_lines(&received).contains("</benterm>")
    });
    app.wait_until("the mark to go", |app| !app.parent_is_marked());
}

#[gpui::test]
fn a_report_needs_a_parent_that_is_still_there(cx: &mut TestAppContext) {
    let mut app = open(cx);
    let orphan = app.spawn("orphan", None, "sleep 100");

    let report = |pane: u64| ReportToParent {
        from: ApiPaneId::new(pane),
        text: "done".into(),
        status: None,
    };
    let from_orphan = app.call(report(orphan));
    let from_a_pane = app.call(report(app.pane));
    let from_nowhere = app.call(report(4040));

    assert_eq!(code(from_orphan), ErrorCode::Failed);
    assert_eq!(code(from_a_pane), ErrorCode::Failed);
    assert_eq!(code(from_nowhere), ErrorCode::UnknownPane);
}

#[gpui::test]
fn children_end_with_the_pane_that_started_them_unless_told_to_outlive_it(cx: &mut TestAppContext) {
    let mut app = open(cx);
    let parent = app
        .call(SplitPane {
            pane: ApiPaneId::new(app.pane),
            direction: SplitDirection::Right,
        })
        .unwrap()
        .pane
        .get();
    app.wait_until("the parent's program", |app| {
        app.read_pane(parent).contains("ready")
    });
    let marker = app.dir.path().join("alive");
    let child = app.spawn(
        "child",
        Some(parent),
        &format!(
            "trap 'rm -f {0}; exit' HUP; touch {0}; while :; do sleep 0.05; done",
            marker.display()
        ),
    );
    app.spawn("grandchild", Some(child), "sleep 100");
    // In a tab by now, and still a child.
    app.spawn("shown", Some(parent), "sleep 100");
    let agent = named("shown");
    app.call(ShowAgent {
        agent,
        select: false,
    })
    .unwrap();
    let tabs = app.tab_count();
    app.call(SpawnAgent {
        name: "stays".into(),
        command: sh("sleep 100"),
        parent: Some(ApiPaneId::new(parent)),
        outlive_parent: true,
        ..SpawnAgent::default()
    })
    .unwrap();
    app.spawn("other", Some(app.pane), "sleep 100");
    app.wait_until("the child to run", |_| marker.exists());

    app.type_into(parent, "quit\r");

    app.wait_until("the children to end", |app| {
        app.names() == ["stays", "other"]
    });
    let stays = &app.list()[0];
    assert_eq!(stays.parent.map(ApiPaneId::get), Some(parent));
    assert!(stays.outlive_parent);
    // The program of the child was hung up, not left running, and the
    // tab of the child that was shown closed with it.
    app.wait_until("the child's program to end", |_| !marker.exists());
    app.wait_until("the tab to close", |app| app.tab_count() == tabs - 1);
}

#[gpui::test]
fn a_name_is_one_agents_at_a_time_and_has_to_be_a_name(cx: &mut TestAppContext) {
    let mut app = open(cx);
    app.spawn("w1", Some(app.pane), "sleep 2");
    let spawn = |name: &str| SpawnAgent {
        name: name.into(),
        command: sh("sleep 100"),
        ..SpawnAgent::default()
    };

    assert_eq!(code(app.call(spawn("w1"))), ErrorCode::NameTaken);
    for name in ["", "12", "two words", "a/b", &"x".repeat(65)] {
        assert_eq!(
            code(app.call(spawn(name))),
            ErrorCode::InvalidParams,
            "{name:?}"
        );
    }
    assert_eq!(app.names(), ["w1"]);

    // The name is free again once its agent has ended.
    app.wait_until("the agent to end", |app| app.names().is_empty());
    app.call(spawn("w1")).expect("the name is free");
}

#[gpui::test]
fn what_cannot_be_started_or_found_is_an_error(cx: &mut TestAppContext) {
    let mut app = open(cx);

    let no_parent = app.call(SpawnAgent {
        name: "w1".into(),
        parent: Some(ApiPaneId::new(4040)),
        ..SpawnAgent::default()
    });
    let no_program = app.call(SpawnAgent {
        name: "w1".into(),
        command: vec!["no-such-program-anywhere".into()],
        ..SpawnAgent::default()
    });
    let no_agent = app.call(ReadAgent {
        agent: named("nobody"),
        lines: None,
    });
    let no_pane = app.call(SendToAgent {
        agent: AgentTarget::Pane(ApiPaneId::new(4040)),
        text: "x".into(),
        caller: None,
    });
    let nobody_to_wait_for = app.call(WaitForAgents::default());

    assert_eq!(code(no_parent), ErrorCode::UnknownPane);
    assert_eq!(code(no_program), ErrorCode::Failed);
    assert_eq!(code(no_agent), ErrorCode::UnknownAgent);
    assert_eq!(code(no_pane), ErrorCode::UnknownPane);
    assert_eq!(code(nobody_to_wait_for), ErrorCode::InvalidParams);
    assert!(app.names().is_empty());
}

#[gpui::test]
fn an_agent_run_as_a_tool_of_a_child_does_not_change_the_childs_state(cx: &mut TestAppContext) {
    let mut app = open(cx);
    let script = app.claude("cat claude-working; sleep 100");
    let child = app.spawn("w1", Some(app.pane), &script);
    let hook = |session: &str, event: &str| ReportHook {
        pane: ApiPaneId::new(child),
        agent: "claude".into(),
        event: event.into(),
        tool_name: None,
        notification_type: None,
        session_id: Some(session.into()),
        agent_id: None,
        tool_input: None,
        tool_response: None,
    };
    app.call(hook("outer", "UserPromptSubmit")).unwrap();
    app.call(hook("outer", "PreToolUse")).unwrap();
    app.wait_until("the child to work", |app| {
        app.list()[0].state == AgentActivity::Working
    });
    let answer: Receiver<Result<AgentsWaited, ClientError>> =
        app.start(App::wait_request("w1", Some(1500)));

    // `claude -p` run from the child's Bash tool has the child's pane id
    // in its environment and reports a whole session of its own.
    for event in ["SessionStart", "UserPromptSubmit", "Stop", "SessionEnd"] {
        app.call(hook("inner", event)).unwrap();
    }

    assert_eq!(app.list()[0].state, AgentActivity::Working);
    assert_eq!(code(app.answer(&answer)), ErrorCode::WaitTimedOut);
}

#[gpui::test]
fn children_end_when_the_parents_program_exits_though_its_terminal_is_kept(
    cx: &mut TestAppContext,
) {
    let mut app = open(cx);
    let parent = app.spawn("parent", None, "sleep 1");
    let child = app.spawn("child", Some(parent), "sleep 100");
    app.spawn("grandchild", Some(child), "sleep 100");
    // Somebody keeps the terminal of the parent, as a view that shows it
    // would: the end of the program is the end of the parent all the same.
    let kept = app
        .window
        .update(&mut app.cx, |root, _, _| root.agent_terminal("parent"))
        .unwrap()
        .expect("the parent runs");

    app.wait_until("the family to end", |app| app.names().is_empty());
    drop(kept);
}

#[gpui::test]
fn a_parent_whose_program_has_exited_takes_no_report(cx: &mut TestAppContext) {
    let mut app = open(cx);
    let parent = app.spawn("parent", None, "sleep 0.5");
    let child = app.call(SpawnAgent {
        name: "child".into(),
        command: sh("sleep 100"),
        parent: Some(ApiPaneId::new(parent)),
        outlive_parent: true,
        ..SpawnAgent::default()
    });
    let child = child.unwrap().pane;
    app.wait_until("the parent to end", |app| app.names() == ["child"]);

    let report = app.call(ReportToParent {
        from: child,
        text: "done".into(),
        status: None,
    });

    assert_eq!(code(report), ErrorCode::Failed);
}

#[gpui::test]
fn a_paste_into_the_parent_is_a_draft_even_when_it_ends_in_a_line_break(cx: &mut TestAppContext) {
    let (mut app, received) = open_at_claude(cx);
    let child = app.spawn("w1", Some(app.pane), "sleep 100");
    // The program reads lines, so the pasted line is sent off here. The
    // app cannot know that of a program, and takes a paste for a draft.
    app.cx
        .write_to_clipboard(gpui::ClipboardItem::new_string("pasted\n".into()));
    app.cx.dispatch_action(benterm_ui::Paste);
    app.wait_until("the paste", |_| read_lines(&received) == "pasted\n");

    let from = ApiPaneId::new(child);
    let text = "done".into();
    app.call(ReportToParent {
        from,
        text,
        status: None,
    })
    .unwrap();
    app.let_a_report_arrive();

    assert_eq!(read_lines(&received), "pasted\n");
    app.cx.simulate_keystrokes("enter");
    app.wait_until("the report", |_| {
        read_lines(&received).contains("</benterm>")
    });
}

#[gpui::test]
fn a_program_is_not_looked_up_in_relative_or_empty_path_entries(cx: &mut TestAppContext) {
    let mut app = open(cx);
    // The agent's directory is a repository, which may hold a program
    // with the name of an agent: `.`, an empty entry and `bin` on the
    // PATH would all lead into it.
    let cwd = app.dir.path().canonicalize().unwrap();
    let bin = cwd.join("bin");
    std::fs::create_dir(&bin).unwrap();
    for tool in [cwd.join("made-up-agent"), bin.join("made-up-agent")] {
        std::fs::write(&tool, "#!/bin/sh\nprintf 'found.'\nsleep 100\n").unwrap();
        std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let request = |path: &str| SpawnAgent {
        name: "w1".into(),
        command: vec!["made-up-agent".into()],
        cwd: Some(cwd.to_str().unwrap().into()),
        env: vec![("PATH".into(), path.into())],
        ..SpawnAgent::default()
    };

    for path in [
        ".:/usr/bin:/bin",
        ":/usr/bin:/bin",
        "/usr/bin::/bin",
        "bin:/usr/bin:/bin",
    ] {
        let refused = app.call(request(path));
        assert_eq!(code(refused), ErrorCode::Failed, "PATH={path}");
    }

    // An absolute entry is used as before.
    let path = format!(".:{}:/usr/bin:/bin", bin.display());
    app.call(request(&path)).expect("the absolute entry");
    app.wait_until("the agent to print", |app| {
        app.read("w1").contains("found.")
    });
}

#[gpui::test]
fn a_wait_right_after_a_message_waits_for_the_turn_the_message_starts(cx: &mut TestAppContext) {
    let mut app = open(cx);
    // At work once, then done, as after a turn, until it is given a line.
    let script = app.claude(
        "cat claude-working; sleep 0.8; cat claude-done; read line; cat claude-working; sleep 2; \
         cat claude-done; sleep 100",
    );
    app.spawn("w1", Some(app.pane), &script);
    app.call(App::wait_request("w1", Some(30_000))).unwrap();
    // Done for longer than a state has to last for a wait.
    for _ in 0..75 {
        app.pass_time();
    }

    app.call(SendToAgent {
        agent: named("w1"),
        text: "go on".into(),
        caller: None,
    })
    .unwrap();
    let started = Instant::now();
    // Not even a wait that takes a state at once takes the old one.
    let request = WaitForAgents {
        settle_ms: Some(0),
        ..App::wait_request("w1", Some(30_000))
    };
    let waited = app.call(request).unwrap();

    assert_eq!(waited.agents[0].state, AgentActivity::Done);
    assert!(
        started.elapsed() >= Duration::from_secs(2),
        "the wait took the end of the turn before for the end of this one"
    );
}

#[gpui::test]
fn an_agent_that_was_given_nothing_and_is_done_is_not_waited_for(cx: &mut TestAppContext) {
    let mut app = open(cx);
    let script = app.claude("cat claude-working; sleep 0.8; cat claude-done; sleep 100");
    app.spawn("w1", Some(app.pane), &script);
    app.call(App::wait_request("w1", Some(30_000))).unwrap();

    // Its turn has ended and nothing was sent since: there is nothing to
    // wait for, however often it is asked.
    let started = Instant::now();
    let waited = app.call(App::wait_request("w1", Some(30_000))).unwrap();

    assert_eq!(waited.agents[0].state, AgentActivity::Done);
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[gpui::test]
fn the_states_of_an_agent_behind_the_window_are_recorded(cx: &mut TestAppContext) {
    let mut app = open(cx);
    let script = app.claude("cat claude-working; sleep 0.8; cat claude-done; sleep 100");
    let pane = app.spawn("w1", Some(app.pane), &script);
    app.call(App::wait_request("w1", Some(30_000))).unwrap();

    let history = app.call(ReadAgentHistory { since_ms: None }).unwrap();

    let states: Vec<AgentActivity> = history
        .records
        .iter()
        .filter(|record| record.pane.get() == pane)
        .map(|record| record.state)
        .collect();
    assert!(
        states.ends_with(&[AgentActivity::Working, AgentActivity::Done]),
        "{states:?}"
    );
}

/// Gives the agent a line, and waits for the turn it starts.
fn send_and_wait(app: &mut App, name: &str) -> Result<AgentsWaited, ClientError> {
    app.call(SendToAgent {
        agent: named(name),
        text: "answer in one line".into(),
        caller: None,
    })
    .unwrap();
    app.call(App::wait_request(name, Some(5_000)))
}

/// A made-up Claude Code with no hooks tells its turn with OSC 7501
/// alone, and the turn is over in a tenth of a second.
#[gpui::test]
fn a_wait_returns_after_a_short_turn_told_with_osc_7501(cx: &mut TestAppContext) {
    let mut app = open(cx);
    let script = app.claude(
        r"read line; printf '\033]7501;state=working\033\\'; sleep 0.1; \
          printf '\033]7501;state=done\033\\'; sleep 100",
    );
    app.spawn("w1", Some(app.pane), &script);

    let waited = send_and_wait(&mut app, "w1").expect("the wait returns at the end of the turn");

    assert_eq!(waited.agents[0].state, AgentActivity::Done);
}

/// The turn begins and ends in one write, so the terminal holds only its
/// end when the app looks: for a made-up Claude Code, and for a program
/// that is no agent.
#[gpui::test]
fn a_wait_returns_after_a_turn_told_in_one_write(cx: &mut TestAppContext) {
    let mut app = open(cx);
    let turn =
        r"read line; printf '\033]7501;state=working\033\\\033]7501;state=done\033\\'; sleep 100";
    let claude = app.claude(turn);
    for (name, script) in [("w1", claude.as_str()), ("w2", turn)] {
        app.spawn(name, Some(app.pane), script);

        let waited =
            send_and_wait(&mut app, name).expect("the wait returns at the end of the turn");

        assert_eq!(waited.agents[0].state, AgentActivity::Done, "{name}");
    }
}

/// Claude Code tells `working` again after the hook of the end of its
/// turn, while a command it left running behind is not over. That is
/// no work begun, so the turn a message starts after it is still owed.
#[gpui::test]
fn working_told_again_after_the_end_a_hook_told_is_not_the_turn_a_message_starts(
    cx: &mut TestAppContext,
) {
    let mut app = open(cx);
    let working = r"printf '\033]7501;state=working\033\\'";
    let done = r"printf '\033]7501;state=done\033\\'";
    let script = app.claude(&format!(
        "{working}; echo begun; read line; {working}; echo told-again; read line; {done}; \
         echo at-rest; read line; {working}; sleep 0.2; {done}; sleep 100"
    ));
    let pane = app.spawn("w1", Some(app.pane), &script);
    app.wait_until("the agent to work", |app| app.read("w1").contains("begun"));
    app.call(ReportHook {
        pane: ApiPaneId::new(pane),
        agent: "claude".into(),
        event: "Stop".into(),
        tool_name: None,
        notification_type: None,
        session_id: Some("a".into()),
        agent_id: None,
        tool_input: None,
        tool_response: None,
    })
    .unwrap();

    app.call(SendToAgent {
        agent: named("w1"),
        text: "go on".into(),
        caller: None,
    })
    .unwrap();
    let request = WaitForAgents {
        settle_ms: Some(0),
        ..App::wait_request("w1", Some(30_000))
    };
    let waited = app.start(request);
    app.wait_until("the agent to tell its work again", |app| {
        app.read("w1").contains("told-again")
    });
    for _ in 0..10 {
        app.pass_time();
    }
    assert_eq!(app.list()[0].state, AgentActivity::Done);
    assert_eq!(app.waits(), 1, "work told again was taken for a turn");
    app.type_into(pane, "\n");
    app.wait_until("the agent to rest", |app| {
        app.read("w1").contains("at-rest")
    });
    for _ in 0..10 {
        app.pass_time();
    }
    assert_eq!(
        app.waits(),
        1,
        "the end of work told again was taken for a turn"
    );

    // The program rests and begins to work: that is a turn.
    app.type_into(pane, "\n");
    let waited = app.answer(&waited).unwrap();
    assert_eq!(waited.agents[0].state, AgentActivity::Done);
}

impl App {
    /// Waits until the terminal of `name` has parsed `needle` on its
    /// worker thread, without running the UI thread so `TerminalView`
    /// has not woken to show the record to its detector yet.
    fn wait_for_unseen_output(&mut self, name: &str, needle: &str) {
        let view = self
            .window
            .read_with(&self.cx, |root, _| root.agent_terminal(name))
            .unwrap()
            .expect("the agent runs");
        let deadline = Instant::now() + TIMEOUT;
        while !view.read_with(&self.cx, |view, _| view.text(None).contains(needle)) {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for unseen {needle:?}"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Runs `agent.send` on the UI thread directly, without waking
    /// pending terminal updates first.
    fn send_now(&mut self, name: &str, text: &str) {
        let request = SendToAgent {
            agent: named(name),
            text: text.into(),
            caller: None,
        };
        self.window
            .update(&mut self.cx, |root, _, cx| root.send_to_agent(&request, cx))
            .unwrap()
            .unwrap();
    }
}

/// A `working` record that arrived in the terminal before `agent send`
/// and has not been shown to the detector yet belongs to the turn
/// before. Showing that same record after the send does not clear the
/// owed turn; only a new `working` after a rest does.
#[gpui::test]
fn unseen_working_shown_after_send_does_not_end_the_wait_until_new_work_begins(
    cx: &mut TestAppContext,
) {
    let mut app = open(cx);
    let working = r"printf '\033]7501;state=working\033\\'";
    let done = r"printf '\033]7501;state=done\033\\'";
    let start_prev = app.dir.path().join("start-prev");
    let end_prev = app.dir.path().join("end-prev");
    let start_next = app.dir.path().join("start-next");
    let script = app.claude(&format!(
        "echo ready; \
         until [ -f '{start_prev}' ]; do sleep 0.02; done; \
         {working}; echo begun; \
         until [ -f '{end_prev}' ]; do sleep 0.02; done; \
         {done}; echo at-rest; \
         until [ -f '{start_next}' ]; do sleep 0.02; done; \
         {working}; echo next-begun; sleep 100",
        start_prev = start_prev.display(),
        end_prev = end_prev.display(),
        start_next = start_next.display(),
    ));
    app.spawn("w1", Some(app.pane), &script);
    app.wait_until("the agent to be ready", |app| {
        app.read("w1").contains("ready")
    });

    // Deliver the previous turn's `working` to the terminal, and send
    // before the view wakes to show it to the detector.
    std::fs::write(&start_prev, "").unwrap();
    app.wait_for_unseen_output("w1", "begun");
    let state_before_send = app
        .window
        .read_with(&app.cx, |root, cx| {
            root.agent_terminal("w1")
                .unwrap()
                .read(cx)
                .agent_status()
                .state
        })
        .unwrap();
    assert_ne!(
        state_before_send,
        benterm_agent::AgentState::Working,
        "detector must not have seen the previous turn's working yet"
    );
    app.send_now("w1", "go on");

    // Wait for either Working or Done so the wait answers the moment
    // the owed turn clears.
    let waited = app.start(WaitForAgents {
        until: vec![AgentActivity::Working, AgentActivity::Done],
        settle_ms: Some(0),
        ..App::wait_request("w1", Some(30_000))
    });
    // Now let the view wake and show that same `working` record.
    for _ in 0..10 {
        app.pass_time();
    }
    assert_eq!(app.list()[0].state, AgentActivity::Working);
    assert_eq!(
        app.waits(),
        1,
        "unseen working from before send was counted as the new turn"
    );

    // The previous turn ends with `done`: still waiting for the new turn.
    std::fs::write(&end_prev, "").unwrap();
    app.wait_until("the previous turn to end", |app| {
        app.read("w1").contains("at-rest")
    });
    for _ in 0..10 {
        app.pass_time();
    }
    assert_eq!(app.list()[0].state, AgentActivity::Done);
    assert_eq!(
        app.waits(),
        1,
        "the end of the previous turn unlocked the wait"
    );

    // After `done`, a new `working` starts the turn and unlocks the wait.
    std::fs::write(&start_next, "").unwrap();
    let waited = app.answer(&waited).unwrap();
    assert_eq!(waited.agents[0].state, AgentActivity::Working);
}

/// After an unseen `working` and `agent send`, an accepted
/// `UserPromptSubmit` hook starts the turn and unlocks the wait, both
/// after the record is shown and without waking the view first.
#[gpui::test]
fn a_hook_after_send_ends_the_wait_even_after_unseen_working(cx: &mut TestAppContext) {
    let mut app = open(cx);
    let working = r"printf '\033]7501;state=working\033\\'";
    let start_a = app.dir.path().join("start-a");
    let start_b = app.dir.path().join("start-b");
    let script_a = app.claude(&format!(
        "echo ready; \
         until [ -f '{start}' ]; do sleep 0.02; done; \
         {working}; echo begun; sleep 100",
        start = start_a.display(),
    ));
    let script_b = app.claude(&format!(
        "echo ready; \
         until [ -f '{start}' ]; do sleep 0.02; done; \
         {working}; echo begun; sleep 100",
        start = start_b.display(),
    ));
    let pane_a = app.spawn("wa", Some(app.pane), &script_a);
    let pane_b = app.spawn("wb", Some(app.pane), &script_b);
    app.wait_until("both agents to be ready", |app| {
        app.read("wa").contains("ready") && app.read("wb").contains("ready")
    });

    // Case 1: record is shown after send (still waiting), then
    // UserPromptSubmit arrives and unlocks the wait.
    std::fs::write(&start_a, "").unwrap();
    app.wait_for_unseen_output("wa", "begun");
    app.send_now("wa", "go on");
    let waited_a = app.start(WaitForAgents {
        until: vec![AgentActivity::Working, AgentActivity::Done],
        settle_ms: Some(0),
        ..App::wait_request("wa", Some(30_000))
    });
    for _ in 0..10 {
        app.pass_time();
    }
    assert_eq!(app.waits(), 1, "still waiting before the hook arrives");
    app.call(ReportHook {
        pane: ApiPaneId::new(pane_a),
        agent: "claude".into(),
        event: "UserPromptSubmit".into(),
        tool_name: None,
        notification_type: None,
        session_id: Some("a".into()),
        agent_id: None,
        tool_input: None,
        tool_response: None,
    })
    .unwrap();
    let waited_a = app.answer(&waited_a).unwrap();
    assert_eq!(waited_a.agents[0].state, AgentActivity::Working);

    // Case 2: without waking the view to show the record first,
    // UserPromptSubmit arrives right after send and unlocks the wait.
    std::fs::write(&start_b, "").unwrap();
    app.wait_for_unseen_output("wb", "begun");
    app.send_now("wb", "go on");
    let hook = benterm_agent::HookEvent::new("claude", "UserPromptSubmit").with_session("b");
    app.window
        .update(&mut app.cx, |root, _, cx| {
            let view = root.agent_terminal("wb").unwrap();
            view.update(cx, |view, cx| view.report_hook(&hook, cx));
        })
        .unwrap();
    let waited_b = app
        .call(WaitForAgents {
            until: vec![AgentActivity::Working, AgentActivity::Done],
            settle_ms: Some(0),
            ..App::wait_request("wb", Some(5_000))
        })
        .expect("UserPromptSubmit unlocked the owed turn");
    assert_eq!(waited_b.agents[0].pane.get(), pane_b);
    assert_eq!(waited_b.agents[0].state, AgentActivity::Working);
}

/// If the previous turn's `working` was unseen at `agent send`, and
/// that turn ends with `done` while neither a new turn's record nor a
/// hook arrives, `agent wait` does not return.
#[gpui::test]
fn unseen_working_whose_turn_ends_with_done_does_not_answer_a_wait(cx: &mut TestAppContext) {
    let mut app = open(cx);
    let working = r"printf '\033]7501;state=working\033\\'";
    let done = r"printf '\033]7501;state=done\033\\'";
    let start_prev = app.dir.path().join("start-prev");
    let end_prev = app.dir.path().join("end-prev");
    let start_next = app.dir.path().join("start-next");
    let script = app.claude(&format!(
        "echo ready; \
         until [ -f '{start_prev}' ]; do sleep 0.02; done; \
         {working}; echo begun; \
         until [ -f '{end_prev}' ]; do sleep 0.02; done; \
         {done}; echo at-rest; \
         until [ -f '{start_next}' ]; do sleep 0.02; done; \
         {working}; sleep 0.2; {done}; sleep 100",
        start_prev = start_prev.display(),
        end_prev = end_prev.display(),
        start_next = start_next.display(),
    ));
    app.spawn("w1", Some(app.pane), &script);
    app.wait_until("the agent to be ready", |app| {
        app.read("w1").contains("ready")
    });

    std::fs::write(&start_prev, "").unwrap();
    app.wait_for_unseen_output("w1", "begun");
    app.send_now("w1", "go on");

    let waited = app.start(WaitForAgents {
        settle_ms: Some(0),
        ..App::wait_request("w1", Some(30_000))
    });
    std::fs::write(&end_prev, "").unwrap();
    app.wait_until("the previous turn to end", |app| {
        app.read("w1").contains("at-rest")
    });
    for _ in 0..10 {
        app.pass_time();
    }
    assert_eq!(app.list()[0].state, AgentActivity::Done);
    assert_eq!(
        app.waits(),
        1,
        "the end of the turn before answered the wait"
    );

    // A real new turn starts and finishes: now the wait returns.
    std::fs::write(&start_next, "").unwrap();
    let waited = app.answer(&waited).unwrap();
    assert_eq!(waited.agents[0].state, AgentActivity::Done);
}

/// Sending to an agent that is already working still waits for a turn
/// that begins after the send, not the end of the work already running.
#[gpui::test]
fn a_wait_after_sending_to_a_working_agent_waits_for_the_next_turn(cx: &mut TestAppContext) {
    let mut app = open(cx);
    let working = r"printf '\033]7501;state=working\033\\'";
    let done = r"printf '\033]7501;state=done\033\\'";
    let end_prev = app.dir.path().join("end-prev");
    let start_next = app.dir.path().join("start-next");
    let script = app.claude(&format!(
        "{working}; echo begun; \
         until [ -f '{end_prev}' ]; do sleep 0.02; done; \
         {done}; echo at-rest; \
         until [ -f '{start_next}' ]; do sleep 0.02; done; \
         {working}; sleep 0.2; {done}; sleep 100",
        end_prev = end_prev.display(),
        start_next = start_next.display(),
    ));
    app.spawn("w1", Some(app.pane), &script);
    app.wait_until("the agent to be working", |app| {
        app.state_of("w1") == Some(AgentActivity::Working)
    });

    app.send("w1", "go on").unwrap();
    let waited = app.start(WaitForAgents {
        settle_ms: Some(0),
        ..App::wait_request("w1", Some(30_000))
    });
    std::fs::write(&end_prev, "").unwrap();
    app.wait_until("the previous turn to end", |app| {
        app.read("w1").contains("at-rest")
    });
    for _ in 0..10 {
        app.pass_time();
    }
    assert_eq!(app.list()[0].state, AgentActivity::Done);
    assert_eq!(
        app.waits(),
        1,
        "the end of the work already running answered the wait"
    );

    std::fs::write(&start_next, "").unwrap();
    let waited = app.answer(&waited).unwrap();
    assert_eq!(waited.agents[0].state, AgentActivity::Done);
}

/// What the agents that stay with the window run: answers every line it
/// reads with `> ` before it, without echo, and exits on `quit`.
const ANSWERING_AGENT: &str =
    r#"stty -echo; while read line; do [ "$line" = quit ] && exit 0; echo "> $line"; done"#;

impl App {
    /// Starts an agent that runs `script` as a child of `parent` that
    /// outlives it, and returns the pane id of its terminal.
    fn spawn_outliving(&mut self, name: &str, parent: u64, script: &str) -> u64 {
        let request = SpawnAgent {
            name: name.into(),
            command: sh(script),
            parent: Some(ApiPaneId::new(parent)),
            outlive_parent: true,
            ..SpawnAgent::default()
        };
        self.call(request).expect("the agent starts").pane.get()
    }

    fn window_is_open(&mut self) -> bool {
        self.window.update(&mut self.cx, |_, _, _| ()).is_ok()
    }

    /// The panes of every tab of every workspace, by the numbers of
    /// their ids.
    fn panes(&mut self) -> Vec<u64> {
        self.window
            .update(&mut self.cx, |root, _, cx| {
                let mut panes = Vec::new();
                for workspace in root.workspaces() {
                    let tabs = root.workspace_tabs(workspace).unwrap();
                    let tabs = tabs.read(cx);
                    for tab in tabs.tabs() {
                        let grid = tabs.grid(tab).unwrap().read(cx);
                        panes.extend(grid.panes().into_iter().map(|pane| pane.get()));
                    }
                }
                panes
            })
            .unwrap()
    }

    /// Lets the app run until the window has closed.
    fn wait_for_the_window_to_close(&mut self) {
        let deadline = Instant::now() + TIMEOUT;
        while self.window_is_open() {
            assert!(Instant::now() < deadline, "the window is still open");
            self.cx.run_until_parked();
            std::thread::sleep(Duration::from_millis(5));
            self.cx.executor().advance_clock(Duration::from_millis(5));
        }
    }
}

#[gpui::test]
fn a_child_that_outlives_the_last_pane_keeps_the_window_and_takes_a_tab(cx: &mut TestAppContext) {
    let mut app = open(cx);
    let child = app.spawn_outliving("w1", app.pane, ANSWERING_AGENT);

    app.type_into(app.pane, "quit\r");

    app.wait_until("the child to take the pane", |app| app.panes() == [child]);
    assert!(app.window_is_open());
    let info = &app.list()[0];
    assert_eq!(info.name, "w1");
    assert!(!info.background, "the child is in a tab now");
    // It is still the agent w1, and runs.
    app.call(SendToAgent {
        agent: named("w1"),
        text: "hello\r".into(),
        caller: None,
    })
    .unwrap();
    app.wait_until("the child to answer", |app| {
        app.read("w1").contains("> hello")
    });
}

#[gpui::test]
fn a_last_pane_without_a_child_that_outlives_it_closes_the_window(cx: &mut TestAppContext) {
    let mut app = open(cx);
    let marker = app.dir.path().join("alive");
    app.spawn(
        "w2",
        Some(app.pane),
        &format!(
            "trap 'rm -f {0}; exit' HUP; touch {0}; while :; do sleep 0.05; done",
            marker.display()
        ),
    );
    app.wait_until("the child to run", |_| marker.exists());

    app.type_into(app.pane, "quit\r");

    app.wait_for_the_window_to_close();
    let deadline = Instant::now() + TIMEOUT;
    while marker.exists() {
        assert!(Instant::now() < deadline, "the child's program still runs");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[gpui::test]
fn the_kept_window_closes_when_the_children_that_outlived_the_pane_have_ended(
    cx: &mut TestAppContext,
) {
    let mut app = open(cx);
    let first = app.spawn_outliving("w1", app.pane, ANSWERING_AGENT);
    let second = app.spawn_outliving("w3", app.pane, ANSWERING_AGENT);
    app.type_into(app.pane, "quit\r");
    app.wait_until("the oldest child to take the pane", |app| {
        app.panes() == [first]
    });

    // The next child takes the pane of the one that ended.
    app.type_into(first, "quit\r");
    app.wait_until("the other child to take the pane", |app| {
        app.window_is_open() && app.panes() == [second]
    });
    assert_eq!(app.names(), ["w3"]);

    // With none left, the window goes as it did before.
    app.type_into(second, "quit\r");
    app.wait_for_the_window_to_close();
}

/// Typing `quit` into the last pane can close the window before the
/// answer to the typing reaches the test, so the test lets the app run
/// on after the window has gone.
#[gpui::test]
fn the_test_can_let_the_app_run_after_the_window_has_closed(cx: &mut TestAppContext) {
    let mut app = open(cx);
    let pane = app.pane;
    app.type_into(pane, "quit\r");
    app.wait_for_the_window_to_close();

    app.pass_time();
}

#[gpui::test]
fn closing_the_last_pane_by_hand_keeps_the_window_for_a_child_too(cx: &mut TestAppContext) {
    let mut app = open(cx);
    let child = app.spawn_outliving("w1", app.pane, ANSWERING_AGENT);
    app.spawn("w2", Some(app.pane), "sleep 100");

    app.cx.simulate_keystrokes("cmd-w");

    app.wait_until("the child to take the pane", |app| app.panes() == [child]);
    app.wait_until("the other child to end", |app| app.names() == ["w1"]);
}

/// `agent.list` JSON carries `msg` only for waiting (or error) agents
/// whose root OSC 7501 record has one, never for working agents.
#[gpui::test]
fn agent_list_json_includes_msg_only_for_waiting_agent(cx: &mut TestAppContext) {
    let mut app = open(cx);
    // "Allow Bash: cargo test" -> QWxsb3cgQmFzaDogY2FyZ28gdGVzdA==
    app.spawn(
        "waiting-agent",
        Some(app.pane),
        r"printf '\033]7501;state=blocked:kind=permission:msg=QWxsb3cgQmFzaDogY2FyZ28gdGVzdA==\033\\'; sleep 100",
    );
    // "Running" -> UnVubmluZw==
    app.spawn(
        "working-agent",
        Some(app.pane),
        r"printf '\033]7501;state=working:msg=UnVubmluZw==\033\\'; sleep 100",
    );

    app.wait_until("both agents report their states", |app| {
        let list = app.list();
        list.len() == 2
            && list
                .iter()
                .any(|a| a.name == "waiting-agent" && a.state == AgentActivity::Waiting)
            && list
                .iter()
                .any(|a| a.name == "working-agent" && a.state == AgentActivity::Working)
    });

    let list = app.list();
    let json = serde_json::to_value(&list).unwrap();
    let items = json.as_array().unwrap();
    let waiting = items
        .iter()
        .find(|item| item["name"] == "waiting-agent")
        .unwrap();
    let working = items
        .iter()
        .find(|item| item["name"] == "working-agent")
        .unwrap();
    assert_eq!(waiting["msg"], serde_json::json!("Allow Bash: cargo test"));
    assert!(working.get("msg").is_none());
}

#[gpui::test]
fn send_to_agent_refuses_with_input_queue_full_when_the_program_does_not_read(
    cx: &mut TestAppContext,
) {
    let mut app = open(cx);
    app.spawn("w1", Some(app.pane), "stty -echo; printf ready; sleep 100");
    app.wait_until("ready", |app| app.read("w1").contains("ready"));

    // `enter_message` appends `\r` to the text, so `WRITE_QUEUE_LIMIT - 1`
    // bytes fills the queue exactly.
    let full = "x".repeat(benterm_terminal::WRITE_QUEUE_LIMIT - 1);
    app.call(SendToAgent {
        agent: named("w1"),
        text: full,
        caller: None,
    })
    .expect("a message up to the limit fits");

    let err = app
        .call(SendToAgent {
            agent: named("w1"),
            text: "more".into(),
            caller: None,
        })
        .expect_err("a message past the limit must be refused");
    match err {
        ClientError::Api(error) => {
            assert_eq!(error.code, ErrorCode::InputQueueFull);
            assert!(
                error.message.contains("full") || error.message.contains("reading"),
                "unexpected message: {}",
                error.message
            );
        }
        other => panic!("expected Api error, got {other:?}"),
    }
}

#[gpui::test]
fn spawned_agent_receives_agent_name_and_parent_pane_id_env(cx: &mut TestAppContext) {
    let mut app = open(cx);
    let child = app.spawn(
        "alice",
        Some(app.pane),
        r#"printf 'name=%s parent=%s.' "$BENTERM_AGENT_NAME" "$BENTERM_PARENT_PANE_ID"; sleep 100"#,
    );
    app.wait_until("env printed", |app| app.read("alice").contains('.'));
    assert_eq!(
        app.read_pane(child),
        format!("name=alice parent={}.", app.pane)
    );
}

#[gpui::test]
fn siblings_can_exchange_messages_via_inbox_only_when_both_allow_peers(cx: &mut TestAppContext) {
    let mut app = open(cx);
    let spawn_peer = |app: &mut App,
                      name: &str,
                      allow_peers: bool,
                      max_peer_rounds: Option<u32>,
                      script: &str| {
        app.call(SpawnAgent {
            name: name.into(),
            command: sh(script),
            parent: Some(ApiPaneId::new(app.pane)),
            allow_peers,
            max_peer_rounds,
            ..SpawnAgent::default()
        })
        .unwrap()
        .pane
        .get()
    };

    let alice = spawn_peer(&mut app, "alice", true, Some(2), "sleep 100");
    let (bob_script, bob_received) = app.claude_at("done");
    let _bob = spawn_peer(&mut app, "bob", true, Some(5), &bob_script);
    let _charlie = spawn_peer(&mut app, "charlie", false, None, "sleep 100");

    app.wait_until("bob at prompt", |app| {
        app.state_of("bob") == Some(AgentActivity::Done)
    });

    // Sibling without allow_peers is refused with PeerNotAllowed.
    let refused = app.call(SendToAgent {
        agent: named("charlie"),
        text: "hi".into(),
        caller: Some(ApiPaneId::new(alice)),
    });
    assert_eq!(code(refused), ErrorCode::PeerNotAllowed);

    // Siblings with allow_peers deliver framed peer messages with round counter.
    app.call(SendToAgent {
        agent: named("bob"),
        text: "hello bob".into(),
        caller: Some(ApiPaneId::new(alice)),
    })
    .expect("round 1 allowed");
    app.wait_until("bob received round 1", |_| {
        std::fs::read_to_string(&bob_received)
            .unwrap()
            .contains("hello bob")
    });
    let got = std::fs::read_to_string(&bob_received).unwrap();
    assert!(
        got.contains(&format!(
            "<benterm from=alice pane={alice} kind=peer round=1/2>"
        )),
        "unexpected framing: {got:?}"
    );

    // Round 2 succeeds; round 3 hits limit (min(2, 5) = 2) and notifies parent.
    app.call(SendToAgent {
        agent: named("bob"),
        text: "round two".into(),
        caller: Some(ApiPaneId::new(alice)),
    })
    .expect("round 2 allowed");
    let limited = app.call(SendToAgent {
        agent: named("bob"),
        text: "round three".into(),
        caller: Some(ApiPaneId::new(alice)),
    });
    assert_eq!(code(limited), ErrorCode::RoundLimitReached);

    app.wait_until("parent notified of peer limit", |app| {
        app.read_pane(app.pane).contains("kind=peer_limit")
    });
    assert!(
        app.read_pane(app.pane)
            .contains("Sibling conversation reached round limit (2/2).")
    );

    // `list --siblings` returns only peer-enabled siblings, excluding self.
    let siblings = app
        .call(ListAgents {
            parent: None,
            siblings_of: Some(ApiPaneId::new(alice)),
        })
        .unwrap()
        .agents;
    let names: Vec<&str> = siblings.iter().map(|a| a.name.as_str()).collect();
    assert_eq!(names, ["bob"]);
    assert!(siblings[0].allow_peers);
}

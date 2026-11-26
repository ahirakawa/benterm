//! The environment of the user's shell, taken at startup for an app
//! opened from the Finder: later terminals, agents and roles get it, the
//! agents and roles started before it has come wait for it, and a shell
//! that does not answer leaves the app with its own and a warning.
//!
//! The window is a real one, the requests come over a real socket, and
//! the user's shell is a made-up script, called as `shell -l -i -c
//! <command>`.

use std::ffi::OsString;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use benterm_ipc::{
    AgentActivity, AgentTarget, Client, ClientError, ListAgents, ListPanes, Method,
    PaneId as ApiPaneId, ReadAgent, ReadAgentState, ReadOutput, ReportHook, SendToAgent,
    SpawnAgent, SplitDirection, SplitPane,
};
use benterm_terminal::PtyConfig;
use benterm_ui::login_env::{self, Taken};
use benterm_ui::settings::ConfigFeed;
use benterm_ui::{BentermWindow, StartedTerminal, bind_keys, ipc};
use futures::channel::oneshot;
use gpui::{TestAppContext, VisualTestContext, WindowHandle};

const TIMEOUT: Duration = Duration::from_secs(15);

const REVIEWER: &str = r#"
name = "reviewer"
agent = "claude"
prompt = "差分を読んで指摘する"
"#;

/// What the panes of the window run: says its `PATH`, and stays.
const SAYS_PATH: &str = r#"echo "path=[$PATH]"; exec sleep 100"#;

struct App {
    window: WindowHandle<BentermWindow>,
    cx: VisualTestContext,
    socket: PathBuf,
    dir: tempfile::TempDir,
}

fn executable(path: &Path, text: &str) {
    std::fs::write(path, text).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// The app's own environment, as launchd gives it: no `SHLVL`.
fn from_finder() -> Vec<(OsString, OsString)> {
    std::env::vars_os()
        .filter(|(name, _)| name != "SHLVL")
        .collect()
}

fn open(cx: &mut TestAppContext) -> App {
    cx.executor().allow_parking();
    cx.update(bind_keys);
    let dir = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap();
    let root = dir.path().canonicalize().unwrap();
    let config = root.join("config");
    std::fs::create_dir_all(config.join("roles")).unwrap();
    std::fs::write(config.join("roles/reviewer.toml"), REVIEWER).unwrap();
    // Found only on the PATH the made-up shell gives.
    let bin = root.join("bin");
    std::fs::create_dir(&bin).unwrap();
    executable(
        &bin.join("made-up-agent"),
        "#!/bin/sh\necho \"path=[$PATH]\"\nexec sleep 100\n",
    );
    executable(
        &bin.join("claude"),
        "#!/bin/sh\necho \"claude path=[$PATH]\"\nexec sleep 100\n",
    );
    executable(
        &bin.join("echo-agent"),
        "#!/bin/sh\nstty -echo 2>/dev/null || true\nwhile IFS= read -r line; do echo \"got=[$line]\"; done\n",
    );
    let socket = root.join("benterm.sock");
    let start: ipc::StartTerminal = Rc::new(StartedTerminal::spawn);
    let feed = ConfigFeed::watch(&config.join("config.toml"));
    let window = cx
        .update(|cx| {
            ipc::open_window(
                PtyConfig::command("/bin/sh", ["-c", SAYS_PATH]),
                start,
                Some(socket.clone()),
                feed,
                cx,
            )
        })
        .unwrap();
    let vcx = VisualTestContext::from_window(window.into(), cx);
    vcx.run_until_parked();
    App {
        window,
        cx: vcx,
        socket,
        dir,
    }
}

impl App {
    fn root(&self) -> PathBuf {
        self.dir.path().canonicalize().unwrap()
    }

    fn bin(&self) -> PathBuf {
        self.root().join("bin")
    }

    /// A made-up user shell running `body` before it runs the command.
    fn shell(&self, body: &str) -> PathBuf {
        let path = self.root().join("zsh");
        executable(&path, &format!("#!/bin/sh\n{body}\n"));
        path
    }

    fn take(&mut self, taken: oneshot::Receiver<Taken>) {
        self.window
            .update(&mut self.cx, |root, window, cx| {
                root.take_login_env(taken, window, cx)
            })
            .unwrap();
    }

    fn pass_time(&mut self) {
        self.cx.run_until_parked();
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

    fn path_of(&mut self, agent: &str) -> String {
        let read = |app: &mut Self| {
            let agent = benterm_ipc::AgentTarget::Name(agent.into());
            app.call(ReadAgent { agent, lines: None }).unwrap().text
        };
        self.wait_until("the agent to print its PATH", |app| read(app).contains(']'));
        printed_path(&read(self))
    }

    fn path_in_pane(&mut self, pane: u64) -> String {
        let read = |app: &mut Self| {
            let pane = ApiPaneId::new(pane);
            app.call(ReadOutput { pane, lines: None }).unwrap().text
        };
        self.wait_until("the pane to print its PATH", |app| read(app).contains(']'));
        printed_path(&read(self))
    }

    fn first_pane(&mut self) -> u64 {
        self.window
            .update(&mut self.cx, |root, _, cx| {
                root.grid(cx).read(cx).focused().get()
            })
            .unwrap()
    }

    fn agent_names(&mut self) -> Vec<String> {
        let list = self.call(ListAgents::default()).unwrap();
        list.agents.into_iter().map(|agent| agent.name).collect()
    }

    fn notice(&mut self) -> Option<String> {
        self.window
            .update(&mut self.cx, |root, _, _| root.shown_login_env_notice())
            .unwrap()
    }
}

fn printed_path(text: &str) -> String {
    let start = text.find("path=[").expect("the program printed its PATH") + "path=[".len();
    let end = start + text[start..].find(']').unwrap();
    text[start..end].to_owned()
}

fn spawn_request(name: &str, command: &[&str]) -> SpawnAgent {
    SpawnAgent {
        name: name.into(),
        command: command.iter().map(|part| part.to_string()).collect(),
        ..SpawnAgent::default()
    }
}

#[gpui::test]
fn an_agent_started_after_the_shell_answered_gets_its_path(cx: &mut TestAppContext) {
    let mut app = open(cx);
    let bin = app.bin();
    let shell = app.shell(&format!(
        "echo hello; PATH=\"/tmp/benterm-test-bin:{}:$PATH\"; export PATH; eval \"$4\"",
        bin.display()
    ));
    let taken = login_env::start(from_finder(), shell).expect("opened from the Finder");
    app.take(taken);

    // Found on the shell's PATH, not on the app's.
    app.call(spawn_request("w1", &["made-up-agent"]))
        .expect("the program is found on the shell's PATH");

    let path = app.path_of("w1");
    assert!(
        path.starts_with(&format!("/tmp/benterm-test-bin:{}:", bin.display())),
        "{path}"
    );
    assert_eq!(app.notice(), None);
}

#[gpui::test]
fn an_agent_asked_for_before_the_shell_answered_waits_for_its_path(cx: &mut TestAppContext) {
    let mut app = open(cx);
    let (answer, taken) = oneshot::channel();
    app.take(taken);

    let spawned = app.start(spawn_request("w1", &["made-up-agent"]));
    for _ in 0..20 {
        app.pass_time();
    }
    assert!(
        spawned.try_recv().is_err(),
        "the agent started before the PATH came"
    );

    let path = format!("{}:/usr/bin:/bin", app.bin().display());
    answer
        .send(Ok(vec![("PATH".into(), path.clone().into())]))
        .unwrap();
    app.answer(&spawned)
        .expect("the agent starts once the PATH has come");
    assert_eq!(app.path_of("w1"), path);
}

#[gpui::test]
fn a_role_chosen_before_the_shell_answered_waits_for_its_path(cx: &mut TestAppContext) {
    let mut app = open(cx);
    let (answer, taken) = oneshot::channel();
    app.take(taken);

    app.window
        .update(&mut app.cx, |root, window, cx| {
            root.launch_role("reviewer", window, cx)
        })
        .unwrap();
    for _ in 0..20 {
        app.pass_time();
    }
    assert_eq!(app.agent_names(), Vec::<String>::new());

    let path = format!("{}:/usr/bin:/bin", app.bin().display());
    answer
        .send(Ok(vec![("PATH".into(), path.clone().into())]))
        .unwrap();
    app.wait_until("the role's agent", |app| app.agent_names() == ["reviewer"]);
    // Started on the PATH that came, not by a login shell that looks it up.
    assert_eq!(app.path_of("reviewer"), path);
}

#[gpui::test]
fn a_pane_opened_after_the_shell_answered_gets_its_path(cx: &mut TestAppContext) {
    let mut app = open(cx);
    let first = app.first_pane();
    let own = app.path_in_pane(first);
    let path = format!("{}:/usr/bin:/bin", app.bin().display());
    let (answer, taken) = oneshot::channel();
    app.take(taken);
    answer
        .send(Ok(vec![("PATH".into(), path.clone().into())]))
        .unwrap();
    app.pass_time();

    let split = app
        .call(SplitPane {
            pane: ApiPaneId::new(first),
            direction: SplitDirection::Right,
        })
        .unwrap();

    assert_eq!(app.path_in_pane(split.pane.get()), path);
    assert_ne!(own, path);
}

#[gpui::test]
fn a_shell_that_never_answers_leaves_the_apps_environment_and_one_warning(cx: &mut TestAppContext) {
    let mut app = open(cx);
    let shell = app.shell("echo hello; exec sleep 100");
    let started = Instant::now();
    let taken = login_env::start(from_finder(), shell).expect("opened from the Finder");
    app.take(taken);

    app.call(spawn_request("w1", &["/bin/sh", "-c", SAYS_PATH]))
        .expect("the agent starts with the app's environment");

    assert!(started.elapsed() >= Duration::from_secs(5));
    assert_eq!(app.path_of("w1"), std::env::var("PATH").unwrap());
    let notice = app.notice().expect("a warning about the shell");
    assert!(notice.contains("5 seconds"), "{notice}");
    let messages = app
        .window
        .update(&mut app.cx, |root, _, _| root.shown_messages())
        .unwrap();
    assert!(!messages.iter().any(|m| m.contains("5 seconds")));
}

#[gpui::test]
fn the_shell_is_not_run_for_an_app_started_from_a_terminal(_cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let ran = dir.path().join("ran");
    let shell = dir.path().join("zsh");
    executable(
        &shell,
        &format!("#!/bin/sh\ntouch '{}'\neval \"$4\"\n", ran.display()),
    );
    let mut vars = from_finder();
    vars.push(("SHLVL".into(), "1".into()));

    assert!(login_env::start(vars, shell).is_none());
    std::thread::sleep(Duration::from_millis(300));
    assert!(!ran.exists());
}

#[gpui::test]
fn hooks_and_reads_pass_a_spawn_waiting_for_login_env(cx: &mut TestAppContext) {
    let mut app = open(cx);
    let first = ApiPaneId::new(app.first_pane());
    let (answer, taken) = oneshot::channel();
    app.take(taken);

    let spawned = app.start(spawn_request("w1", &["made-up-agent"]));
    for _ in 0..10 {
        app.pass_time();
    }
    assert!(
        spawned.try_recv().is_err(),
        "the agent started before the PATH came"
    );

    // A hook report and a pane list sent behind the waiting spawn finish
    // before the login shell answers.
    let hook = app.start(ReportHook {
        pane: first,
        agent: "claude".into(),
        event: "Stop".into(),
        tool_name: None,
        notification_type: None,
        session_id: None,
        agent_id: None,
        tool_input: None,
        tool_response: None,
    });
    let list = app.start(ListPanes {});

    let deadline = Instant::now() + Duration::from_secs(3);
    let mut hook_done = None;
    let mut list_done = None;
    while hook_done.is_none() || list_done.is_none() {
        if hook_done.is_none()
            && let Ok(res) = hook.try_recv()
        {
            hook_done = Some(res);
        }
        if list_done.is_none()
            && let Ok(res) = list.try_recv()
        {
            list_done = Some(res);
        }
        assert!(
            Instant::now() < deadline,
            "hook report and pane.list were blocked behind agent.spawn waiting for login_env"
        );
        app.pass_time();
    }
    hook_done.unwrap().expect("hook report succeeded");
    let panes = list_done.unwrap().expect("pane.list succeeded");
    assert!(panes.panes.iter().any(|p| p.pane == first));
    let state = app
        .call(ReadAgentState { pane: first })
        .expect("pane.agent_state succeeded");
    assert_eq!(state.state, AgentActivity::Done);
    assert!(
        spawned.try_recv().is_err(),
        "agent.spawn must still wait for login_env"
    );

    let path = format!("{}:/usr/bin:/bin", app.bin().display());
    answer
        .send(Ok(vec![("PATH".into(), path.clone().into())]))
        .unwrap();
    app.answer(&spawned)
        .expect("agent.spawn finishes once login_env arrives");
    assert_eq!(app.path_of("w1"), path);
}

#[gpui::test]
fn two_spawns_waiting_for_login_env_start_in_order(cx: &mut TestAppContext) {
    let mut app = open(cx);
    let (answer, taken) = oneshot::channel();
    app.take(taken);

    let first = app.start(spawn_request("w1", &["made-up-agent"]));
    for _ in 0..10 {
        app.pass_time();
    }
    let second = app.start(spawn_request("w2", &["made-up-agent"]));
    for _ in 0..10 {
        app.pass_time();
    }
    assert!(first.try_recv().is_err());
    assert!(second.try_recv().is_err());

    let path = format!("{}:/usr/bin:/bin", app.bin().display());
    answer
        .send(Ok(vec![("PATH".into(), path.clone().into())]))
        .unwrap();

    let first_spawned = app.answer(&first).expect("first agent spawned");
    let second_spawned = app.answer(&second).expect("second agent spawned");
    assert!(
        first_spawned.pane.get() < second_spawned.pane.get(),
        "expected w1 ({}) to start before w2 ({})",
        first_spawned.pane,
        second_spawned.pane
    );
    assert_eq!(app.agent_names(), vec!["w1", "w2"]);
}

#[gpui::test]
fn send_to_a_deferred_agent_runs_after_its_spawn(cx: &mut TestAppContext) {
    let mut app = open(cx);
    let (answer, taken) = oneshot::channel();
    app.take(taken);

    let spawned = app.start(spawn_request("w1", &["echo-agent"]));
    for _ in 0..10 {
        app.pass_time();
    }
    let sent = app.start(SendToAgent {
        agent: AgentTarget::Name("w1".into()),
        text: "hello-after-spawn".into(),
        caller: None,
    });
    for _ in 0..10 {
        app.pass_time();
    }

    // Unrelated reads still pass while spawn and its send wait.
    let list = app.start(ListPanes {});
    let deadline = Instant::now() + Duration::from_secs(3);
    let panes = loop {
        if let Ok(res) = list.try_recv() {
            break res.expect("pane.list succeeded");
        }
        assert!(
            Instant::now() < deadline,
            "pane.list was blocked behind deferred spawn and send"
        );
        app.pass_time();
    };
    assert!(!panes.panes.is_empty());
    assert!(spawned.try_recv().is_err());
    assert!(sent.try_recv().is_err());

    let path = format!("{}:/usr/bin:/bin", app.bin().display());
    answer.send(Ok(vec![("PATH".into(), path.into())])).unwrap();

    app.answer(&spawned).expect("agent.spawn succeeded");
    app.answer(&sent).expect("agent.send succeeded after spawn");
    app.wait_until("the agent to echo the message sent after spawn", |app| {
        let out = app
            .call(ReadAgent {
                agent: AgentTarget::Name("w1".into()),
                lines: None,
            })
            .unwrap()
            .text;
        out.contains("got=[hello-after-spawn]")
    });
}

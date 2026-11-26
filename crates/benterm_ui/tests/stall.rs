//! An agent that stays at work with neither its screen nor its files
//! changing is marked as stalled: in its row of the session list, on the
//! progress board, in `agent list` and in `agent show`. Its state stays what it was.
//!
//! The window is a real one, the made-up agent a real program, and the
//! requests come in over a real socket. The time the mark goes by is the
//! test's clock, which the test moves on.

#[path = "../../benterm_agent/tests/scenes/mod.rs"]
#[allow(dead_code)]
mod scenes;

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use benterm_config::parse;
use benterm_ipc::{
    AgentActivity, AgentInfo, AgentShown, AgentTarget, Client, ClientError, ErrorCode, ListAgents,
    Method, ShowAgent, SpawnAgent, WaitForAgents,
};
use benterm_terminal::PtyConfig;
use benterm_ui::settings::ConfigFeed;
use benterm_ui::{BentermWindow, StartedTerminal, bind_keys, ipc};
use gpui::{TestAppContext, VisualTestContext, WindowHandle};

const TIMEOUT: Duration = Duration::from_secs(10);

struct App {
    window: WindowHandle<BentermWindow>,
    cx: VisualTestContext,
    socket: PathBuf,
    dir: tempfile::TempDir,
}

/// The screen of a made-up Claude Code at work, and its start screen.
fn write_frames(dir: &Path) {
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

/// A window whose settings mark an agent after `after_seconds`.
fn open(cx: &mut TestAppContext, after_seconds: u64) -> App {
    cx.executor().allow_parking();
    cx.update(bind_keys);
    let dir = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap();
    write_frames(dir.path());
    let socket = dir.path().join("benterm.sock");
    let start: ipc::StartTerminal = Rc::new(StartedTerminal::spawn);
    let settings = parse(&format!("[stall]\nafter_seconds = {after_seconds}\n")).unwrap();
    assert_eq!(settings.warnings, vec![]);
    let window = cx
        .update(|cx| {
            ipc::open_window(
                PtyConfig::command("/bin/sh", ["-c", "echo ready; sleep 1000"]),
                start,
                Some(socket.clone()),
                ConfigFeed::new(settings),
                cx,
            )
        })
        .unwrap();
    let cx = VisualTestContext::from_window(window.into(), cx);
    cx.run_until_parked();
    App {
        window,
        cx,
        socket,
        dir,
    }
}

impl App {
    /// Lets the app run, a moment pass on the real clock, which the
    /// programs and the agent detector follow, and `step` on the test's
    /// clock, which the marks follow.
    fn pass(&mut self, step: Duration) {
        self.cx.run_until_parked();
        self.cx.update(|window, cx| window.draw(cx).clear(cx));
        std::thread::sleep(Duration::from_millis(20));
        self.cx.executor().advance_clock(step);
        self.cx.run_until_parked();
    }

    fn wait_until(&mut self, what: &str, mut done: impl FnMut(&mut Self) -> bool) {
        let deadline = Instant::now() + TIMEOUT;
        while !done(self) {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            self.pass(Duration::from_millis(20));
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
            if let Ok(result) = answer.try_recv() {
                return result;
            }
            assert!(Instant::now() < deadline, "no answer before the timeout");
            self.pass(Duration::from_millis(20));
        }
    }

    fn list(&mut self) -> Vec<AgentInfo> {
        self.call(ListAgents::default()).unwrap().agents
    }

    /// Starts a made-up Claude Code that shows its banner, then the
    /// screen of work, and then nothing more, in a directory of its own
    /// whose files do not change.
    fn spawn_stuck_claude(&mut self, name: &str) -> u64 {
        let work = self.dir.path().join("work");
        std::fs::create_dir(&work).unwrap();
        std::fs::write(work.join("notes.txt"), "x").unwrap();
        let bin = self.dir.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        let path = bin.join("claude");
        let frames = self.dir.path().display();
        let script = format!(
            "#!/bin/sh\ncat '{frames}/claude-start'; sleep 0.3; cat '{frames}/claude-working'; sleep 1000\n"
        );
        std::fs::write(&path, script).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        let request = SpawnAgent {
            name: name.into(),
            command: vec![
                "/bin/sh".into(),
                "-c".into(),
                format!("exec '{}'", path.display()),
            ],
            cwd: Some(work.display().to_string()),
            ..SpawnAgent::default()
        };
        self.call(request).expect("the agent starts").pane.get()
    }

    fn drawn(&mut self, selector: &'static str) -> bool {
        let _ = self
            .window
            .update(&mut self.cx, |_, window, _| window.refresh());
        self.cx.run_until_parked();
        self.cx.debug_bounds(selector).is_some()
    }

    /// Whether the row of the terminal `pane` in the session list has the
    /// stall mark.
    fn row_is_stalled(&mut self, pane: u64) -> bool {
        self.window
            .update(&mut self.cx, |root, _, cx| {
                root.session_rows(cx)
                    .into_iter()
                    .find(|row| row.pane.get() == pane)
                    .is_some_and(|row| row.stalled)
            })
            .unwrap()
    }
}

#[gpui::test]
fn an_agent_left_at_work_unchanged_is_marked_in_its_row_and_in_agent_list(cx: &mut TestAppContext) {
    let mut app = open(cx, 1);
    let pane = app.spawn_stuck_claude("w1");
    app.wait_until("the agent to work", |app| {
        app.list()[0].state == AgentActivity::Working
    });
    assert!(!app.list()[0].stalled);
    assert!(!app.row_is_stalled(pane));

    // A second and a half on the test's clock: past the threshold, and
    // time for the files to be looked at twice.
    for _ in 0..6 {
        app.pass(Duration::from_millis(250));
    }
    app.wait_until("the mark", |app| app.list()[0].stalled);

    let listed = app.list().remove(0);
    assert_eq!(listed.state, AgentActivity::Working);
    let changed = listed.changed_at_ms.expect("the last change is told");
    assert!(app.row_is_stalled(pane));
    let shown: AgentShown = app
        .call(ShowAgent {
            agent: AgentTarget::Name("w1".into()),
            select: false,
        })
        .unwrap();
    assert!(shown.stalled);
    assert_eq!(shown.changed_at_ms, Some(changed));

    // A wait for the agent to stop working still waits: the mark is no
    // state.
    let waited = app.call(WaitForAgents {
        agent: Some(AgentTarget::Name("w1".into())),
        wait_ms: Some(300),
        ..WaitForAgents::default()
    });
    match waited {
        Err(ClientError::Api(error)) => assert_eq!(error.code, ErrorCode::WaitTimedOut),
        other => panic!("expected the wait to run out, got {other:?}"),
    }

    // The board marks the end of the agent's band of work.
    app.cx.simulate_keystrokes("cmd-shift-b");
    assert!(app.drawn("board-lane-0"));
    assert!(app.drawn("board-stalled-0"));
}

#[gpui::test]
fn an_agent_at_work_is_not_marked_before_the_threshold(cx: &mut TestAppContext) {
    let mut app = open(cx, 600);
    let pane = app.spawn_stuck_claude("w1");
    app.wait_until("the agent to work", |app| {
        app.list()[0].state == AgentActivity::Working
    });
    for _ in 0..20 {
        app.pass(Duration::from_secs(5));
    }
    let listed = app.list().remove(0);
    assert!(!listed.stalled);
    assert!(listed.changed_at_ms.is_some());
    assert!(!app.row_is_stalled(pane));
}

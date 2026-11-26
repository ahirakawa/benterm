//! An agent started from a role gives way to a shell when it ends: its
//! pane stays, with what the agent showed, and the shell a new pane runs
//! prompts in it, with the command that takes the agent's conversation
//! up again typed and not run.
//!
//! The window is a real one, whose new panes run an interactive shell,
//! and the agent is a made-up `claude` that ends on `quit`. Its hooks
//! are reported to its pane by the test.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use benterm_agent::HookEvent;
use benterm_ipc::{
    AgentTarget, Client, ClientError, KillAgent, ListAgents, ListPanes, Method, SpawnAgent,
};
use benterm_terminal::PtyConfig;
use benterm_ui::settings::ConfigFeed;
use benterm_ui::{BentermWindow, LaunchRole, StartedTerminal, TerminalView, bind_keys, ipc, menu};
use gpui::{Action, Entity, TestAppContext, VisualTestContext, WindowHandle};

const TIMEOUT: Duration = Duration::from_secs(10);

/// What the shells show as their prompt.
const PROMPT: &str = "benterm$ ";

const WORKER: &str = r#"
name = "worker"
agent = "claude"
prompt = "作業する"
"#;

struct App {
    window: WindowHandle<BentermWindow>,
    cx: VisualTestContext,
    socket: PathBuf,
    /// Made when the made-up claude is run to take a conversation up.
    resumed: PathBuf,
    fake: PathBuf,
    _dir: tempfile::TempDir,
}

fn executable(path: &Path, text: &str) {
    std::fs::write(path, text).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn open(cx: &mut TestAppContext) -> App {
    cx.executor().allow_parking();
    cx.update(|cx| {
        bind_keys(cx);
        menu::install(cx);
    });
    let dir = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap();
    let root = dir.path().canonicalize().unwrap();
    let config = root.join("config");
    let roles = config.join("roles");
    std::fs::create_dir_all(&roles).unwrap();
    std::fs::write(roles.join("worker.toml"), WORKER).unwrap();
    let fake = root.join("fake");
    std::fs::create_dir(&fake).unwrap();
    let resumed = root.join("resumed");
    executable(
        &fake.join("claude"),
        &format!(
            "#!/bin/sh\n[ \"$1\" = --resume ] && touch '{}'\n\
             echo said-before-the-end\n\
             while read line; do [ \"$line\" = quit ] && exit 0; done\n",
            resumed.display()
        ),
    );
    let mut pty = PtyConfig::command("/bin/sh", ["-i"]);
    pty.cwd = Some(root.clone());
    pty.env = vec![
        (
            "PATH".into(),
            format!("{}:/usr/bin:/bin", fake.display()).into(),
        ),
        ("PS1".into(), PROMPT.into()),
        ("ENV".into(), "/dev/null".into()),
        ("HOME".into(), root.clone().into()),
    ];
    let socket = root.join("benterm.sock");
    let start: ipc::StartTerminal = Rc::new(StartedTerminal::spawn);
    let feed = ConfigFeed::watch(&config.join("config.toml"));
    let window = cx
        .update(|cx| ipc::open_window(pty, start, Some(socket.clone()), feed, cx))
        .unwrap();
    let vcx = VisualTestContext::from_window(window.into(), cx);
    vcx.run_until_parked();
    App {
        window,
        cx: vcx,
        socket,
        resumed,
        fake,
        _dir: dir,
    }
}

impl App {
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

    /// Makes one call on a thread of its own, letting the app run until
    /// the answer is there.
    fn call<M>(&mut self, request: M) -> Result<M::Output, ClientError>
    where
        M: Method + Send + 'static,
        M::Output: Send + 'static,
    {
        let (sender, answer) = std::sync::mpsc::channel();
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
            self.pass_time();
        }
    }

    /// Starts the role worker from the window, and waits until its agent
    /// is in front of its pane, the focused one.
    fn launch_worker(&mut self) -> Entity<TerminalView> {
        let first = self.focused();
        self.wait_until("the shell of the first pane prompts", |app| {
            app.text(&first).contains(PROMPT.trim_end())
        });
        self.window
            .update(&mut self.cx, |_, window, cx| {
                window.dispatch_action(LaunchRole("worker".into()).boxed_clone(), cx)
            })
            .unwrap();
        self.pass_time();
        let view = self.focused();
        self.wait_until("the agent is in front", |app| {
            app.cx.read(|cx| view.read(cx).agent_status().agent) == Some("claude")
        });
        self.wait_until("the agent speaks", |app| {
            app.text(&view).contains("said-before-the-end")
        });
        view
    }

    fn focused(&mut self) -> Entity<TerminalView> {
        self.window
            .read_with(&self.cx, |root, cx| root.terminal(cx))
            .unwrap()
    }

    fn text(&self, view: &Entity<TerminalView>) -> String {
        self.cx.read(|cx| view.read(cx).text(None))
    }

    fn type_into(&mut self, view: &Entity<TerminalView>, text: &str) {
        let sent = self.cx.read(|cx| view.read(cx).send_text(text));
        assert!(sent);
    }

    fn hook(&mut self, view: &Entity<TerminalView>, event: HookEvent) {
        self.cx
            .update(|_, cx| view.update(cx, |view, cx| view.report_hook(&event, cx)));
    }

    /// The last line of `view` with anything on it.
    fn last_line(&self, view: &Entity<TerminalView>) -> String {
        let text = self.text(view);
        text.lines()
            .rfind(|line| !line.trim().is_empty())
            .unwrap_or_default()
            .trim_end()
            .to_owned()
    }

    /// Waits until the shell after the agent prompts in `view`, below
    /// what the agent said.
    fn wait_for_the_shell(&mut self, view: &Entity<TerminalView>) {
        self.wait_until("the shell prompts after the agent", |app| {
            let text = app.text(view);
            text.find("said-before-the-end")
                .is_some_and(|at| text[at..].contains(PROMPT.trim_end()))
        });
    }

    fn tab_count(&mut self) -> usize {
        self.window
            .update(&mut self.cx, |root, _, cx| {
                root.tabs().read(cx).tabs().len()
            })
            .unwrap()
    }

    fn pane_count(&mut self) -> usize {
        self.window
            .update(&mut self.cx, |root, _, cx| {
                root.grid(cx).read(cx).panes().len()
            })
            .unwrap()
    }

    fn agent_names(&mut self) -> Vec<String> {
        self.window
            .update(&mut self.cx, |root, _, cx| {
                root.agents(cx).into_iter().map(|row| row.name).collect()
            })
            .unwrap()
    }

    fn session_rows(&mut self) -> usize {
        self.window
            .update(&mut self.cx, |root, _, cx| root.session_rows(cx).len())
            .unwrap()
    }
}

#[gpui::test]
fn a_role_s_agent_that_ends_leaves_its_pane_to_a_shell_with_its_resume_command_typed(
    cx: &mut TestAppContext,
) {
    let mut app = open(cx);
    let view = app.launch_worker();
    app.hook(
        &view,
        HookEvent::new("claude", "SessionStart").with_session("abc"),
    );
    app.hook(
        &view,
        HookEvent::new("claude", "UserPromptSubmit").with_session("abc"),
    );
    assert_eq!(app.tab_count(), 2);

    app.type_into(&view, "quit\r");
    app.wait_for_the_shell(&view);
    app.wait_until("the resume command is typed", |app| {
        app.last_line(&view) == format!("{PROMPT}claude --resume abc")
    });

    assert_eq!(app.tab_count(), 2);
    assert_eq!(app.pane_count(), 1);
    // What the agent said stays on the screen.
    assert!(app.text(&view).contains("said-before-the-end"));
    // The pane is a terminal like any other now: no agent, and no row.
    assert_eq!(app.agent_names(), Vec::<String>::new());
    app.wait_until("the agent's row goes", |app| app.session_rows() == 0);

    // The command was typed without Enter: what the shell runs after the
    // line is erased runs first.
    app.type_into(&view, "\x15echo typed-after\r");
    app.wait_until("the shell runs a command", |app| {
        app.text(&view).contains("\ntyped-after")
    });
    assert!(!app.resumed.exists(), "the resume command was run");
}

#[gpui::test]
fn an_agent_that_told_no_conversation_leaves_nothing_typed(cx: &mut TestAppContext) {
    let mut app = open(cx);
    let view = app.launch_worker();

    app.type_into(&view, "quit\r");
    app.wait_for_the_shell(&view);
    app.type_into(&view, "echo after\r");
    app.wait_until("the shell runs a command", |app| {
        app.text(&view).contains("\nafter")
    });

    let text = app.text(&view);
    assert!(
        text.lines()
            .any(|line| line.trim_end() == format!("{PROMPT}echo after")),
        "{text}"
    );
    assert_eq!(app.pane_count(), 1);
    // Told once the window reads the shell in front, which it does by
    // itself every so often.
    app.wait_until("the agent leaves the list", |app| {
        app.agent_names().is_empty()
    });
}

#[gpui::test]
fn the_shell_after_the_agent_closes_its_pane_when_it_exits(cx: &mut TestAppContext) {
    let mut app = open(cx);
    let view = app.launch_worker();
    app.type_into(&view, "quit\r");
    app.wait_for_the_shell(&view);

    app.type_into(&view, "exit\r");
    app.wait_until("the pane closes", |app| app.tab_count() == 1);
}

#[gpui::test]
fn killing_a_role_s_agent_closes_its_pane(cx: &mut TestAppContext) {
    let mut app = open(cx);
    app.launch_worker();
    assert_eq!(app.tab_count(), 2);

    app.call(KillAgent {
        agent: AgentTarget::Name("worker".into()),
    })
    .unwrap();

    app.wait_until("the pane closes", |app| app.tab_count() == 1);
    assert_eq!(app.agent_names(), Vec::<String>::new());
}

/// An agent behind the window has no pane to give way to a shell in:
/// its terminal closes when it ends, as before.
#[gpui::test]
fn an_agent_behind_the_window_that_ends_closes_its_terminal(cx: &mut TestAppContext) {
    let mut app = open(cx);
    let claude = app.fake.join("claude").to_str().unwrap().to_owned();
    let spawned = app
        .call(SpawnAgent {
            name: "behind".into(),
            command: vec![claude],
            shell_after: true,
            ..SpawnAgent::default()
        })
        .unwrap();
    let panes = |app: &mut App| {
        app.call(ListPanes::default())
            .unwrap()
            .panes
            .iter()
            .any(|pane| pane.pane == spawned.pane)
    };
    let agents = |app: &mut App| app.call(ListAgents::default()).unwrap().agents.len();
    assert_eq!(agents(&mut app), 1);

    let pane = benterm_ipc::SendText {
        pane: spawned.pane,
        text: "quit\r".into(),
    };
    app.wait_until("the agent reads", |app| {
        app.call(benterm_ipc::ReadOutput {
            pane: spawned.pane,
            lines: None,
        })
        .is_ok_and(|output| output.text.contains("said-before-the-end"))
    });
    app.call(pane).unwrap();

    app.wait_until("the agent is gone", |app| agents(app) == 0);
    assert!(!panes(&mut app));
}

//! The program in front of a pane's shell decides whether the pane has an
//! agent: an agent that ends leaves the pane unknown without any hook,
//! and what a shell shows is never taken for an agent.
//!
//! The agents here are scripts named `claude` early on the shell's PATH.
//! They print a made-up Claude Code screen, and send no hook; where a
//! test needs one, it reports it to the pane itself.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

use benterm_agent::{AgentState, HookEvent};
use benterm_session::{
    Agent, Conversation, Node, Session, Tab, Terminal, Workspace, new_scrollback_key,
};
use benterm_terminal::PtyConfig;
use benterm_ui::session::open_session_window;
use benterm_ui::settings::ConfigFeed;
use benterm_ui::{AgentStatus, BentermWindow, SpawnTerminal, StartedTerminal, bind_keys};
use gpui::{TestAppContext, VisualTestContext, WindowHandle};

const TIMEOUT: Duration = Duration::from_secs(10);

/// The shells' prompt starts like Claude Code's prompt box, which the
/// screen takes for an agent waiting at its prompt.
const PROMPT: &str = "❯ ";

/// A made-up Claude Code start screen: its banner, its prompt box and
/// the footer under it.
const CLAUDE_SCREEN: &[&str] = &[
    "* Welcome to Claude Code",
    "",
    "> made-up output",
    "",
    "❯ ",
    "  ? for shortcuts",
];

/// A temporary directory with its symbolic links resolved, with `bin`
/// for the agents and the place of the session file.
struct Place {
    _dir: tempfile::TempDir,
    root: PathBuf,
    file: PathBuf,
}

impl Place {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        fs::create_dir(root.join("bin")).unwrap();
        let file = root.join("state").join("session.json");
        Self {
            _dir: dir,
            root,
            file,
        }
    }

    /// Puts an executable `claude` on the shells' PATH that shows the
    /// made-up screen and then runs `then`.
    fn claude(&self, then: &str) {
        let screen = CLAUDE_SCREEN.join("\\r\\n");
        let path = self.root.join("bin").join("claude");
        let script = format!("#!/bin/sh\nprintf '\\033[H\\033[2J{screen}'\n{then}\n");
        fs::write(&path, script).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn path(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }
}

/// Starts an interactive shell, which runs each command line as a job
/// of its own in front, with the place's `bin` first on its PATH.
fn shells(place: &Place) -> SpawnTerminal {
    let root = place.root.clone();
    let path = format!(
        "{}:{}",
        root.join("bin").display(),
        std::env::var("PATH").unwrap_or_default()
    );
    Rc::new(move |_, theme, _, size| {
        let mut pty = PtyConfig::command("/bin/sh", ["-i"]);
        pty.cwd = Some(root.clone());
        pty.size = size;
        pty.env = vec![
            ("PATH".into(), path.clone().into()),
            ("PS1".into(), PROMPT.into()),
            ("ENV".into(), "/dev/null".into()),
            ("HOME".into(), root.clone().into()),
        ];
        StartedTerminal::spawn(pty, theme)
    })
}

struct App {
    window: WindowHandle<BentermWindow>,
    cx: VisualTestContext,
}

fn open(place: &Place, cx: &mut TestAppContext) -> App {
    cx.executor().allow_parking();
    cx.update(bind_keys);
    let spawn = shells(place);
    let window = cx
        .update(|cx| open_session_window(spawn, ConfigFeed::defaults(), Some(&place.file), cx))
        .unwrap();
    let vcx = VisualTestContext::from_window(window.into(), cx);
    let mut app = App { window, cx: vcx };
    app.wait_until("the shell prompts", |app| {
        app.text().contains(PROMPT.trim_end())
    });
    app
}

impl App {
    /// Lets the app run and a fortieth of a second pass, on the test's
    /// clock and on the real one, on which the shells run.
    fn pass_time(&mut self) {
        self.cx.run_until_parked();
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

    fn text(&mut self) -> String {
        self.window
            .read_with(&self.cx, |root, cx| root.terminal(cx).read(cx).text(None))
            .unwrap()
    }

    fn type_line(&mut self, line: &str) {
        let typed = self
            .window
            .read_with(&self.cx, |root, cx| {
                root.terminal(cx).read(cx).send_text(&format!("{line}\r"))
            })
            .unwrap();
        assert!(typed);
    }

    fn status(&mut self) -> AgentStatus {
        self.window
            .read_with(&self.cx, |root, cx| {
                root.terminal(cx).read(cx).agent_status()
            })
            .unwrap()
    }

    /// Reports a hook event of Claude Code's session `session`.
    fn hook(&mut self, session: &str, name: &str) {
        let event = HookEvent::new("claude", name).with_session(session);
        self.window
            .update(&mut self.cx, |root, _, cx| {
                let view = root.terminal(cx);
                view.update(cx, |view, cx| view.report_hook(&event, cx));
            })
            .unwrap();
    }

    fn conversation(&mut self) -> Option<Conversation> {
        self.window
            .read_with(&self.cx, |root, cx| {
                root.terminal(cx).read(cx).conversation()
            })
            .unwrap()
    }

    /// Lets a second pass, and checks that the pane has no agent all
    /// the while.
    fn stays_unknown(&mut self, what: &str) {
        let until = Instant::now() + Duration::from_secs(1);
        while Instant::now() < until {
            assert_eq!(self.status(), unknown(), "{what}");
            self.pass_time();
        }
    }
}

fn unknown() -> AgentStatus {
    AgentStatus {
        agent: None,
        state: AgentState::Unknown,
    }
}

fn shows_a_state(status: AgentStatus) -> bool {
    status.agent == Some("claude")
        && matches!(status.state, AgentState::Done | AgentState::Waiting(_))
}

/// Runs `line` in the shell, which starts the agent; the agent touches
/// the file `ended` as the last thing before it exits two seconds after
/// it started. Returns once the file is there.
fn run_agent_until_it_ends(place: &Place, app: &mut App, line: &str) -> Instant {
    let ended = place.path("ended");
    place.claude(&format!("sleep 2; touch '{}'", ended.display()));
    app.type_line(line);
    app.wait_until("the agent's screen to show its state", |app| {
        shows_a_state(app.status())
    });
    app.wait_until("the agent to end", |_| ended.exists());
    Instant::now()
}

#[gpui::test]
fn an_agent_that_ends_leaves_the_pane_unknown_without_a_hook(cx: &mut TestAppContext) {
    let place = Place::new();
    let mut app = open(&place, cx);

    let ended = run_agent_until_it_ends(&place, &mut app, "claude");
    app.wait_until("the pane to be unknown", |app| app.status() == unknown());
    assert!(
        ended.elapsed() < Duration::from_secs(1),
        "took {:?}",
        ended.elapsed()
    );
    // The shell's prompt looks like the agent's prompt box, and the
    // agent's banner is still on the screen.
    app.stays_unknown("the shell is in front");
}

#[gpui::test]
fn an_agent_that_ends_with_nothing_printed_after_it_is_noticed_too(cx: &mut TestAppContext) {
    let place = Place::new();
    let mut app = open(&place, cx);

    // After the agent another program comes to the front, and the
    // terminal stays as the agent left it.
    let ended = run_agent_until_it_ends(&place, &mut app, "claude; sleep 100");
    app.wait_until("the pane to be unknown", |app| app.status() == unknown());
    assert!(
        ended.elapsed() < Duration::from_secs(1),
        "took {:?}",
        ended.elapsed()
    );
}

#[gpui::test]
fn an_agent_started_again_at_once_has_nothing_of_the_run_before(cx: &mut TestAppContext) {
    let place = Place::new();
    let mut app = open(&place, cx);
    // Each run tells that it started, and stays until it is let go.
    let root = place.root.display().to_string();
    place.claude(&format!(
        "echo run >> '{root}/runs'; n=$(grep -c run '{root}/runs'); touch \"{root}/started-$n\"\n\
         while [ ! -e \"{root}/go-$n\" ]; do sleep 0.05; done"
    ));
    // The shell is in front between the two for an instant only. The
    // pane is right whether that instant was seen or not.
    app.type_line("claude; claude");
    app.wait_until("the first run", |app| {
        place.path("started-1").exists() && shows_a_state(app.status())
    });
    app.hook("first", "UserPromptSubmit");
    app.hook("first", "StopFailure");
    assert_eq!(app.status().state, AgentState::Error);
    assert_eq!(
        app.conversation(),
        Conversation::new(Agent::Claude, "first")
    );

    fs::write(place.path("go-1"), "").unwrap();
    // The prompt the second run shows would leave the error of the
    // first one as it is.
    let at_its_prompt = AgentStatus {
        agent: Some("claude"),
        state: AgentState::Done,
    };
    app.wait_until("the second run without the error", |app| {
        place.path("started-2").exists() && app.status() == at_its_prompt
    });
    assert_eq!(app.conversation(), None);
    fs::write(place.path("go-2"), "").unwrap();
}

#[gpui::test]
fn a_shell_that_shows_what_an_agent_shows_has_no_agent(cx: &mut TestAppContext) {
    let place = Place::new();
    let mut app = open(&place, cx);

    app.type_line("echo '* Welcome to Claude Code'; echo '  ? for shortcuts'");
    app.wait_until("the banner", |app| app.text().contains("for shortcuts\n"));
    app.stays_unknown("the shell shows a banner and a prompt like the agent's");
}

/// Saves a session whose one pane showed Claude Code's start screen.
fn save_a_pane_that_showed_the_agent(place: &Place) {
    let key = new_scrollback_key().unwrap();
    let session = Session {
        selected_workspace: 0,
        workspaces_made: 1,
        folded_repositories: Vec::new(),
        workspaces: vec![Workspace {
            task: None,
            name: "Workspace 1".into(),
            color: Some(0),
            selected_tab: 0,
            tabs: vec![Tab {
                name: None,
                focused_pane: 0,
                root: Node::Terminal(Terminal {
                    cwd: None,
                    scrollback: Some(key.clone()),
                    conversation: None,
                    ..Terminal::default()
                }),
            }],
        }],
    };
    fs::create_dir_all(place.file.parent().unwrap()).unwrap();
    let opened = benterm_session::open(&place.file);
    let store = opened.store.unwrap();
    store.write(store.ticket(), &session).unwrap();
    let mut history = CLAUDE_SCREEN.join("\r\n");
    history.push_str("\r\n");
    store
        .write_scrollback(store.ticket(), &key, history.as_bytes())
        .unwrap();
}

#[gpui::test]
fn a_restored_pane_is_unknown_until_an_agent_comes_to_the_front(cx: &mut TestAppContext) {
    let place = Place::new();
    save_a_pane_that_showed_the_agent(&place);
    let mut app = open(&place, cx);
    app.wait_until("the restored screen", |app| {
        app.text().contains("Welcome to Claude Code")
    });

    app.stays_unknown("the restored screen shows the agent's banner");

    place.claude("sleep 100");
    app.type_line("claude");
    app.wait_until("the agent", |app| app.status().agent == Some("claude"));
}

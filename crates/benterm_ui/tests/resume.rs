//! The window keeps the conversation of the agent in each pane, as its
//! hooks report it, and takes it up again in the pane after a restart by
//! typing the agent's resume command into the pane's shell.
//!
//! The agents here are scripts named `claude` early on the shell's PATH.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use benterm_agent::{AgentState, HookEvent};
use benterm_session::{Agent, Conversation, Node, Session, Tab, Terminal, Workspace};
use benterm_terminal::PtyConfig;
use benterm_ui::session::{SAVE_INTERVAL, open_session_window};
use benterm_ui::settings::ConfigFeed;
use benterm_ui::{BentermWindow, SpawnTerminal, StartedTerminal, bind_keys};
use gpui::{TestAppContext, VisualTestContext, WindowHandle};

/// What the test shells show as their prompt.
const PROMPT: &str = "benterm-test$ ";

/// A temporary directory with its symbolic links resolved, with the
/// directory `work` for the panes, `bin` for the agents, and the place of
/// the session file.
struct Place {
    _dir: tempfile::TempDir,
    root: PathBuf,
    file: PathBuf,
}

impl Place {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        fs::create_dir(root.join("work")).unwrap();
        fs::create_dir(root.join("bin")).unwrap();
        let file = root.join("state").join("session.json");
        Self {
            _dir: dir,
            root,
            file,
        }
    }

    fn work(&self) -> String {
        self.root.join("work").to_str().unwrap().to_owned()
    }

    /// Puts an executable `claude` on the shells' PATH that runs `script`.
    fn claude(&self, script: &str) {
        let path = self.root.join("bin").join("claude");
        fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn save(&self, session: &Session) {
        fs::create_dir_all(self.file.parent().unwrap()).unwrap();
        fs::write(&self.file, session.to_json().unwrap()).unwrap();
    }

    fn saved(&self) -> Option<Session> {
        let text = fs::read(&self.file).ok()?;
        Some(Session::from_json(&text).expect("the session file is whole"))
    }
}

/// Starts an interactive shell with the place's `bin` first on its PATH,
/// in the directory it is given, or in the place without one.
fn shells(place: &Place) -> SpawnTerminal {
    let root = place.root.clone();
    let path = format!(
        "{}:{}",
        root.join("bin").display(),
        std::env::var("PATH").unwrap_or_default()
    );
    Rc::new(move |_, theme, directory, size| {
        let mut pty = PtyConfig::command("/bin/sh", ["-i"]);
        pty.cwd = Some(directory.unwrap_or(&root).to_owned());
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

fn open(
    place: &Place,
    cx: &mut TestAppContext,
) -> (WindowHandle<BentermWindow>, VisualTestContext) {
    cx.executor().allow_parking();
    cx.update(bind_keys);
    let spawn = shells(place);
    let window = cx
        .update(|cx| open_session_window(spawn, ConfigFeed::defaults(), Some(&place.file), cx))
        .unwrap();
    let vcx = VisualTestContext::from_window(window.into(), cx);
    vcx.run_until_parked();
    (window, vcx)
}

/// Lets the app run until `done` holds. The shells run in real time on
/// other threads, hence the polling.
fn wait_until(
    cx: &mut VisualTestContext,
    what: &str,
    mut done: impl FnMut(&mut VisualTestContext) -> bool,
) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        cx.run_until_parked();
        if done(cx) {
            return;
        }
        assert!(Instant::now() < deadline, "timed out waiting until {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// The leaf of the only pane of a session.
fn leaf(session: &Session) -> Terminal {
    match &session.workspaces[0].tabs[0].root {
        Node::Terminal(terminal) => terminal.clone(),
        _ => panic!("the session has one terminal"),
    }
}

fn one_pane(terminal: Terminal) -> Session {
    Session {
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
                root: Node::Terminal(terminal),
            }],
        }],
    }
}

fn hook(window: WindowHandle<BentermWindow>, cx: &mut VisualTestContext, event: HookEvent) {
    window
        .update(cx, |root, _, cx| {
            let view = root.terminal(cx);
            view.update(cx, |view, cx| view.report_hook(&event, cx));
        })
        .unwrap();
}

fn text(window: WindowHandle<BentermWindow>, cx: &mut VisualTestContext) -> String {
    window
        .read_with(cx, |root, cx| root.terminal(cx).read(cx).text(None))
        .unwrap()
}

fn claude(id: &str) -> Option<Conversation> {
    Conversation::new(Agent::Claude, id)
}

/// Lets the time between two saves pass, and waits until the file holds
/// a conversation that `holds` accepts.
fn wait_for_saved(
    place: &Place,
    cx: &mut VisualTestContext,
    what: &str,
    holds: impl Fn(Option<Conversation>) -> bool,
) {
    cx.executor().advance_clock(SAVE_INTERVAL);
    wait_until(cx, what, |cx| {
        cx.executor().advance_clock(SAVE_INTERVAL);
        place
            .saved()
            .is_some_and(|saved| holds(leaf(&saved).conversation))
    });
}

#[gpui::test]
fn the_conversation_a_hook_reports_is_saved_in_the_pane_s_leaf(cx: &mut TestAppContext) {
    let place = Place::new();
    let (window, mut cx) = open(&place, cx);

    hook(
        window,
        &mut cx,
        HookEvent::new("claude", "SessionStart").with_session("abc"),
    );
    hook(
        window,
        &mut cx,
        HookEvent::new("claude", "UserPromptSubmit").with_session("abc"),
    );

    let session = window
        .update(&mut cx, |root, _, cx| root.session(cx))
        .unwrap();
    assert_eq!(leaf(&session).conversation, claude("abc"));
    wait_for_saved(&place, &mut cx, "the conversation is saved", |saved| {
        saved == claude("abc")
    });
}

#[gpui::test]
fn a_pane_whose_agent_ended_its_session_keeps_no_conversation(cx: &mut TestAppContext) {
    let place = Place::new();
    let (window, mut cx) = open(&place, cx);
    for name in ["SessionStart", "UserPromptSubmit", "Stop", "SessionEnd"] {
        hook(
            window,
            &mut cx,
            HookEvent::new("claude", name).with_session("abc"),
        );
    }

    let session = window
        .update(&mut cx, |root, _, cx| root.session(cx))
        .unwrap();
    assert_eq!(leaf(&session).conversation, None);
    wait_for_saved(&place, &mut cx, "the pane is saved", |saved| {
        saved.is_none()
    });
}

#[gpui::test]
fn a_nested_agent_does_not_take_the_place_of_the_one_in_front(cx: &mut TestAppContext) {
    let place = Place::new();
    let (window, mut cx) = open(&place, cx);
    hook(
        window,
        &mut cx,
        HookEvent::new("claude", "UserPromptSubmit").with_session("outer"),
    );
    // `claude -p` run from a tool of the outer one.
    for name in ["SessionStart", "UserPromptSubmit", "Stop", "SessionEnd"] {
        hook(
            window,
            &mut cx,
            HookEvent::new("claude", name).with_session("nested"),
        );
    }

    let session = window
        .update(&mut cx, |root, _, cx| root.session(cx))
        .unwrap();
    assert_eq!(leaf(&session).conversation, claude("outer"));
}

#[gpui::test]
fn the_next_run_in_the_pane_takes_the_place_of_one_that_failed(cx: &mut TestAppContext) {
    let place = Place::new();
    let (window, mut cx) = open(&place, cx);
    for name in ["SessionStart", "UserPromptSubmit", "StopFailure"] {
        hook(
            window,
            &mut cx,
            HookEvent::new("claude", name).with_session("first"),
        );
    }
    let status = |cx: &mut VisualTestContext| {
        window
            .update(cx, |root, _, cx| root.terminal(cx).read(cx).agent_status())
            .unwrap()
    };
    assert_eq!(status(&mut cx).state, AgentState::Error);

    // The agent was started again; the shell stays in front here, as a
    // wrapper script does.
    hook(
        window,
        &mut cx,
        HookEvent::new("claude", "SessionStart").with_session("second"),
    );

    assert_eq!(status(&mut cx).agent, Some("claude"));
    assert_eq!(status(&mut cx).state, AgentState::Unknown);
    let session = window
        .update(&mut cx, |root, _, cx| root.session(cx))
        .unwrap();
    assert_eq!(leaf(&session).conversation, claude("second"));
    wait_for_saved(&place, &mut cx, "the new conversation is saved", |saved| {
        saved == claude("second")
    });
}

/// Saves a session whose pane had Claude Code's conversation `abc` in
/// the directory `work`.
fn save_conversation(place: &Place) {
    place.save(&one_pane(Terminal {
        cwd: Some(place.work()),
        scrollback: None,
        conversation: claude("abc"),
        ..Terminal::default()
    }));
}

#[gpui::test]
fn the_conversation_is_taken_up_again_in_the_pane_s_directory(cx: &mut TestAppContext) {
    let place = Place::new();
    let out = place.root.join("claude-was-run");
    // printf may write its lines one at a time; the file takes its name
    // only once the whole of it is written.
    place.claude(&format!(
        r#"printf '%s\n' "$PWD" "$@" > '{out}.part'; mv '{out}.part' '{out}'; sleep 100"#,
        out = out.display()
    ));
    save_conversation(&place);

    let (_window, mut cx) = open(&place, cx);

    wait_until(&mut cx, "claude has been run", |_| out.exists());
    let run = fs::read_to_string(&out).unwrap();
    assert_eq!(run, format!("{}\n--resume\nabc\n", place.work()));
}

#[gpui::test]
fn a_conversation_that_is_not_found_leaves_the_shell_and_the_reason(cx: &mut TestAppContext) {
    let place = Place::new();
    place.claude(r#"echo "No conversation found with session ID: $2" >&2; exit 1"#);
    save_conversation(&place);

    let (window, mut cx) = open(&place, cx);

    let reason = "No conversation found with session ID: abc";
    wait_until(&mut cx, "the shell prompts again after the reason", |cx| {
        let text = text(window, cx);
        text.find(reason)
            .is_some_and(|at| text[at..].contains(PROMPT.trim_end()))
    });
    // The shell is still there to take a command.
    let typed = window
        .read_with(&cx, |root, cx| {
            root.terminal(cx).read(cx).send_text("echo still-here\r")
        })
        .unwrap();
    assert!(typed);
    wait_until(&mut cx, "the shell runs a command", |cx| {
        text(window, cx).contains("\nstill-here")
    });
}

#[gpui::test]
fn a_pane_without_a_conversation_runs_no_agent(cx: &mut TestAppContext) {
    let place = Place::new();
    let out = place.root.join("claude-was-run");
    place.claude(&format!("touch '{}'", out.display()));
    place.save(&one_pane(Terminal {
        cwd: Some(place.work()),
        ..Terminal::default()
    }));

    let (window, mut cx) = open(&place, cx);

    wait_until(&mut cx, "the shell prompts", |cx| {
        text(window, cx).contains(PROMPT.trim_end())
    });
    // Whatever was typed would have run by the time a command typed
    // after it has.
    window
        .read_with(&cx, |root, cx| {
            root.terminal(cx).read(cx).send_text("echo typed-after\r")
        })
        .unwrap();
    wait_until(&mut cx, "the shell runs a command", |cx| {
        text(window, cx).contains("\ntyped-after")
    });
    assert!(!Path::new(&out).exists());
}

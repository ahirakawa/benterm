//! The end of a command, as the pane's shell tells it with OSC 133 ; D:
//! the pane knows the exit code, and an agent that ends with an exit
//! code other than 0 goes into the history as an error. The start of
//! the next prompt (OSC 133 ; A) drops the working that a program told
//! with OSC 7501.
//!
//! The panes run zsh with Benterm's shell integration, in a home of their
//! own. The agent is a script named `claude` early on the shell's PATH,
//! which shows a made-up Claude Code screen and sends no hook.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

use benterm_session::Agent;
use benterm_session::history::{Record, State};
use benterm_terminal::{CommandEnd, PtyConfig};
use benterm_ui::session_rows::SessionState;
use benterm_ui::settings::ConfigFeed;
use benterm_ui::{BentermWindow, StartedTerminal, bind_keys, ipc};
use gpui::{TestAppContext, VisualTestContext, WindowHandle};

const TIMEOUT: Duration = Duration::from_secs(10);

const PROMPT: &str = "zsh> ";

/// A temporary home with `bin` for the agent, a `.zshrc` and the place
/// of the session file.
struct Place {
    _dir: tempfile::TempDir,
    root: PathBuf,
}

impl Place {
    fn new() -> Self {
        let dir = tempfile::Builder::new()
            .permissions(fs::Permissions::from_mode(0o700))
            .tempdir()
            .unwrap();
        let root = dir.path().canonicalize().unwrap();
        fs::create_dir(root.join("bin")).unwrap();
        fs::write(root.join(".zshrc"), format!("PS1='{PROMPT}'\n")).unwrap();
        Self { _dir: dir, root }
    }

    /// Puts an executable `claude` on the shell's PATH that shows a
    /// made-up Claude Code screen, waits for the file `go` and then
    /// exits with `code`.
    fn claude(&self, code: i32) {
        let go = self.go();
        let script = format!(
            "#!/bin/sh\nprintf '\\033[H\\033[2J* Welcome to Claude Code\\r\\n\\r\\n❯ \\r\\n  ? for shortcuts'\n\
             while [ ! -e '{}' ]; do sleep 0.02; done\nexit {code}\n",
            go.display()
        );
        let path = self.root.join("bin").join("claude");
        let _ = fs::remove_file(&go);
        fs::write(&path, script).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn go(&self) -> PathBuf {
        self.root.join("go")
    }
}

struct Benterm {
    window: WindowHandle<BentermWindow>,
    cx: VisualTestContext,
}

fn open(place: &Place, cx: &mut TestAppContext) -> Benterm {
    cx.executor().allow_parking();
    cx.update(bind_keys);
    let mut pty = PtyConfig::command("/bin/zsh", ["-i"]);
    pty.cwd = Some(place.root.clone());
    let path = format!(
        "{}:{}",
        place.root.join("bin").display(),
        std::env::var("PATH").unwrap_or_default()
    );
    pty.env = vec![
        ("HOME".into(), place.root.clone().into()),
        ("PATH".into(), path.into()),
    ];
    let start: ipc::StartTerminal = Rc::new(StartedTerminal::spawn);
    let session = Some(place.root.join("state").join("session.json"));
    let feed = ConfigFeed::defaults();
    let window = cx
        .update(|cx| ipc::open_session_window(pty, start, None, session, feed, cx))
        .unwrap();
    let cx = VisualTestContext::from_window(window.into(), cx);
    let mut benterm = Benterm { window, cx };
    benterm.wait_until("the prompt", |benterm| {
        benterm.text().contains(PROMPT.trim_end())
    });
    benterm
}

impl Benterm {
    fn wait_until(&mut self, what: &str, mut done: impl FnMut(&mut Self) -> bool) {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            self.cx.run_until_parked();
            if done(self) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {what}; screen: {:?}",
                self.text()
            );
            std::thread::sleep(Duration::from_millis(10));
            self.cx.executor().advance_clock(Duration::from_millis(10));
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

    fn last_command_end(&mut self) -> Option<CommandEnd> {
        self.window
            .read_with(&self.cx, |root, cx| {
                root.terminal(cx).read(cx).last_command_end()
            })
            .unwrap()
    }

    fn agent(&mut self) -> Option<&'static str> {
        self.window
            .read_with(&self.cx, |root, cx| {
                root.terminal(cx).read(cx).agent_status().agent
            })
            .unwrap()
    }

    fn states(&mut self) -> Vec<SessionState> {
        self.window
            .read_with(&self.cx, |root, cx| {
                root.session_rows(cx)
                    .into_iter()
                    .map(|row| row.state)
                    .collect()
            })
            .unwrap()
    }

    fn records(&mut self) -> Vec<Record> {
        self.window
            .update(&mut self.cx, |root, _, _| root.agent_history())
            .unwrap()
    }

    fn errors(&mut self) -> Vec<Record> {
        let records = self.records();
        records
            .into_iter()
            .filter(|record| record.state == State::Error)
            .collect()
    }

    /// Runs a command that prints a mark, and waits for the mark: every
    /// end the shell told before it has then been taken by the pane.
    fn settle(&mut self) {
        self.type_line("echo mark-$((40 + 2))");
        self.wait_until("the mark", |benterm| benterm.text().contains("mark-42"));
        self.type_line("clear");
        self.wait_until("the cleared screen", |benterm| {
            !benterm.text().contains("mark-42")
        });
    }

    /// Runs the made-up agent until the pane names it, then lets it exit
    /// with `code`, and waits for the shell's next prompt.
    fn run_agent(&mut self, place: &Place, code: i32) {
        place.claude(code);
        self.type_line("claude");
        self.wait_until("the agent", |benterm| benterm.agent() == Some("claude"));
        fs::write(place.go(), "").unwrap();
        self.wait_until("the agent to end", |benterm| benterm.agent().is_none());
    }
}

fn end(exit_code: Option<i32>) -> Option<CommandEnd> {
    Some(CommandEnd { exit_code })
}

#[gpui::test]
fn the_pane_knows_the_exit_code_of_the_last_command(cx: &mut TestAppContext) {
    let place = Place::new();
    let mut benterm = open(&place, cx);
    benterm.type_line(r"printf '\033]133;D;1\007'; printf '\033]133;D\007'; false");
    benterm.wait_until("the end of false", |benterm| {
        benterm.last_command_end() == end(Some(1))
    });
    // The two printed before are taken too, in order: the second one
    // had no exit code.
    benterm.type_line(r"printf '\033]133;D\007'; echo mark-$((40 + 2))");
    benterm.wait_until("the mark", |benterm| benterm.text().contains("mark-42"));
    benterm.wait_until("the end of the line", |benterm| {
        benterm.last_command_end() == end(Some(0))
    });
}

#[gpui::test]
fn an_agent_that_exits_with_1_is_recorded_as_an_error(cx: &mut TestAppContext) {
    let place = Place::new();
    let mut benterm = open(&place, cx);
    benterm.run_agent(&place, 1);
    benterm.wait_until("the error", |benterm| !benterm.errors().is_empty());
    let errors = benterm.errors();
    assert_eq!(errors.len(), 1, "{:?}", benterm.records());
    assert_eq!(errors[0].agent, Some(Agent::Claude));
    let pane = benterm
        .window
        .update(&mut benterm.cx, |root, _, cx| {
            root.grid(cx).read(cx).focused()
        })
        .unwrap();
    assert_eq!(errors[0].pane, pane.get());
    assert_eq!(benterm.last_command_end(), end(Some(1)));
    // The agent is gone after its error.
    let records = benterm.records();
    assert_eq!(records.last().unwrap().state, State::Unknown);
    assert_eq!(records.last().unwrap().agent, None);

    // The next command that fails was no agent.
    benterm.type_line("false");
    benterm.settle();
    assert_eq!(benterm.errors().len(), 1, "{:?}", benterm.records());
}

#[gpui::test]
fn an_agent_that_exits_with_0_is_no_error(cx: &mut TestAppContext) {
    let place = Place::new();
    let mut benterm = open(&place, cx);
    benterm.run_agent(&place, 0);
    benterm.settle();
    assert_eq!(benterm.last_command_end(), end(Some(0)));
    assert!(benterm.errors().is_empty(), "{:?}", benterm.records());
    assert!(!benterm.records().is_empty(), "the agent was recorded");
}

#[gpui::test]
fn a_program_that_is_no_agent_failing_is_no_error(cx: &mut TestAppContext) {
    let place = Place::new();
    let mut benterm = open(&place, cx);
    benterm.type_line("false");
    benterm.wait_until("the end of false", |benterm| {
        benterm.last_command_end() == end(Some(1))
    });
    benterm.settle();
    assert!(benterm.errors().is_empty(), "{:?}", benterm.records());
}

#[gpui::test]
fn a_program_that_ends_while_working_leaves_no_working_row(cx: &mut TestAppContext) {
    let place = Place::new();
    let mut benterm = open(&place, cx);
    let go = place.go();
    benterm.type_line(&format!(
        r"printf '\033]7501;state=working\033\\'; while [ ! -e '{}' ]; do sleep 0.02; done",
        go.display()
    ));
    benterm.wait_until("the working row", |benterm| {
        benterm.states() == [SessionState::Working]
    });
    fs::write(&go, "").unwrap();
    benterm.wait_until("the row to go", |benterm| benterm.states().is_empty());
}

/// A program in the background tells it is working after the line that
/// started it has ended, and then an empty line is entered: the shell
/// tells no end, only the start of the prompt.
#[gpui::test]
fn the_start_of_a_prompt_alone_drops_the_working_row(cx: &mut TestAppContext) {
    let place = Place::new();
    let mut benterm = open(&place, cx);
    let go = place.go();
    benterm.type_line(&format!(
        r"(while [ ! -e '{}' ]; do sleep 0.02; done; printf '\033]7501;state=working\033\\') &",
        go.display()
    ));
    benterm.wait_until("the end of the line", |benterm| {
        benterm.last_command_end() == end(Some(0))
    });
    fs::write(&go, "").unwrap();
    benterm.wait_until("the working row", |benterm| {
        benterm.states() == [SessionState::Working]
    });
    benterm.type_line("");
    benterm.wait_until("the row to go", |benterm| benterm.states().is_empty());
}

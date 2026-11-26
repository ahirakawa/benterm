//! The skill for agents, `skills/benterm/SKILL.md`, works as written:
//! a shell in a pane of the app runs every line of its `sh` examples, in
//! order, and asks a stand-in for Codex in the pane beside it.
//!
//! The window is a real one without a display, the terminals run real
//! shells on PTYs, and the commands are the real `benterm` binary,
//! typed at the prompt as an agent's tool would run them.

use std::ffi::OsString;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use benterm_agent::AgentState;
use benterm_terminal::PtyConfig;
use benterm_ui::settings::ConfigFeed;
use benterm_ui::{BentermWindow, StartedTerminal, TerminalView, bind_keys, ipc};
use gpui::{Entity, TestAppContext, VisualTestContext, WindowHandle};

const TIMEOUT: Duration = Duration::from_secs(60);

/// Stands in for Codex. Its program is named `codex`, which is how the
/// app tells which agent runs, and it shows what Codex shows: the prompt
/// with its footer when it is done, and the status line while it works.
/// Each request, the one it is started with or a line it reads, takes it
/// two seconds and gets the same answer.
const FAKE_CODEX: &str = r#"#!/bin/sh
stty -echo
done_screen() { printf '\033[H\033[2J%s\n\n\342\200\272 \n\n  ? for shortcuts\n' "$1"; }
work() {
  printf '\033[H\033[2Jrequest: %s\n\n\342\200\242 Working (0s \342\200\242 esc to interrupt)\n' "$1"
  sleep 2
  done_screen "answer: no problems found"
}
if [ -n "$1" ]; then work "$1"; else done_screen "ready"; fi
while IFS= read -r line; do work "$line"; done
"#;

fn skill() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../skills/benterm/SKILL.md");
    std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()))
}

/// Every command line of the skill's `sh` examples, in order. Comment
/// lines and blank lines are left out. The `shell` examples are for an
/// agent that another agent started, such as `agent report`, and a pane
/// that nothing started cannot run them; the command line tests of
/// `benterm_ipc` check what they send.
fn example_lines(skill: &str) -> Vec<String> {
    let mut lines = Vec::new();
    let mut in_sh = false;
    for line in skill.lines() {
        let trimmed = line.trim();
        if in_sh {
            if trimmed == "```" {
                in_sh = false;
            } else if !trimmed.is_empty() && !trimmed.starts_with('#') {
                lines.push(trimmed.to_owned());
            }
        } else if trimmed == "```sh" {
            in_sh = true;
        }
    }
    lines
}

/// An interactive shell with a plain prompt, no startup files, and the
/// stand-in Codex and the built `benterm` first on its `PATH`.
fn shell(fakes: &Path) -> PtyConfig {
    let bin = Path::new(env!("CARGO_BIN_EXE_benterm"))
        .parent()
        .expect("the binary is in a directory");
    let mut path = OsString::from(fakes);
    path.push(":");
    path.push(bin);
    path.push(":/usr/bin:/bin");
    let mut pty = PtyConfig::command("/bin/sh", ["-i"]);
    pty.env = vec![
        ("PATH".into(), path),
        ("PS1".into(), "$ ".into()),
        ("ENV".into(), "".into()),
    ];
    pty
}

struct App {
    window: WindowHandle<BentermWindow>,
    cx: VisualTestContext,
    /// The pane the commands are typed in.
    pane: u64,
    /// How many lines were typed.
    ran: usize,
}

/// One line typed at the prompt: how long it took, its exit status and
/// what it printed.
struct Ran {
    line: String,
    took: Duration,
    status: String,
    output: String,
}

impl App {
    fn text_of(&mut self, pane: u64) -> String {
        self.view_of(pane, |view| view.text(Some(1000)))
    }

    fn agent_of(&mut self, pane: u64) -> (Option<&'static str>, AgentState) {
        self.view_of(pane, |view| {
            let status = view.agent_status();
            (status.agent, status.state)
        })
    }

    /// What every terminal shows, in the tab and behind the window, and
    /// the agent the app sees in it: for a failure to tell what was on
    /// the screens when it gave up.
    fn screens(&mut self) -> String {
        self.window
            .update(&mut self.cx, |root, _, cx| {
                let grid = root.grid(cx);
                let grid = grid.read(cx);
                let mut terminals: Vec<(String, Entity<TerminalView>)> = grid
                    .panes()
                    .into_iter()
                    .filter_map(|id| Some((format!("pane {id}"), grid.terminal(id)?.clone())))
                    .collect();
                for agent in root.agents(cx) {
                    if let Some(view) = root.agent_terminal(&agent.name) {
                        terminals.push((format!("agent {} ({})", agent.name, agent.pane), view));
                    }
                }
                let mut out = String::new();
                for (label, view) in terminals {
                    let view = view.read(cx);
                    let status = view.agent_status();
                    let text = view.text(Some(1000));
                    let lines: Vec<&str> = text.lines().collect();
                    let tail = &lines[lines.len().saturating_sub(15)..];
                    out.push_str(&format!(
                        "--- {label}, agent {:?} {:?}, screen ends:\n{}\n",
                        status.agent,
                        status.state,
                        tail.join("\n")
                    ));
                }
                out
            })
            .expect("the window is open")
    }

    fn view_of<T>(&mut self, pane: u64, look: impl FnOnce(&TerminalView) -> T) -> T {
        self.window
            .update(&mut self.cx, |root, _, cx| {
                // Both panes are in the tab on view.
                let grid = root.grid(cx);
                let grid = grid.read(cx);
                let id = grid
                    .panes()
                    .into_iter()
                    .find(|id| id.get() == pane)
                    .unwrap_or_else(|| panic!("the tab has no pane {pane}"));
                let view = grid.terminal(id).expect("a pane has a terminal");
                look(view.read(cx))
            })
            .expect("the window is open")
    }

    /// Lets the app run until `done` holds for the text of a pane. The
    /// shells and the commands run in real time, and the app's timers on
    /// the test's clock, which is moved along with it.
    fn wait_for(&mut self, pane: u64, what: &str, done: impl Fn(&str) -> bool) -> String {
        self.wait_until(what, |app| {
            let text = app.text_of(pane);
            done(&text).then_some(text)
        })
    }

    fn wait_until<T>(&mut self, what: &str, mut done: impl FnMut(&mut Self) -> Option<T>) -> T {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            self.cx.run_until_parked();
            if let Some(found) = done(self) {
                return found;
            }
            if Instant::now() >= deadline {
                let screens = self.screens();
                panic!("timed out waiting for {what}\n{screens}");
            }
            std::thread::sleep(Duration::from_millis(5));
            self.cx.executor().advance_clock(Duration::from_millis(5));
        }
    }

    /// Types a line at the prompt, in the shell itself so that the
    /// variables it sets stay, and waits for it to end.
    fn run(&mut self, line: &str) -> Ran {
        self.ran += 1;
        let (start, end) = (format!("s{}.", self.ran), format!("e{}=", self.ran));
        self.cx
            .simulate_input(&format!(r#"echo {start}; {line}; echo "{end}$?.""#));
        let began = Instant::now();
        self.cx.simulate_keystrokes("enter");
        let ended = |text: &str| {
            let (_, after) = text.rsplit_once(&format!("\n{start}\n"))?;
            let (output, status) = after.rsplit_once(&format!("\n{end}")).or_else(|| {
                // A line that prints nothing.
                after.strip_prefix(&end).map(|status| ("", status))
            })?;
            let (status, _) = status.split_once('.')?;
            Some((status.to_owned(), output.to_owned()))
        };
        let pane = self.pane;
        let text = self.wait_for(pane, line, |text| ended(text).is_some());
        let took = began.elapsed();
        let (status, output) = ended(&text).unwrap();
        let output = without_setpgid_notes(&output);
        Ran {
            line: line.to_owned(),
            took,
            status,
            output,
        }
    }
}

/// The output of a line without the notes the shell prints when macOS
/// refuses to put a command in a process group of its own, such as
/// `sh: child setpgid (4982 to 4982): Operation not permitted`. They are
/// the shell's, not the command's: the shell runs the command anyway. With
/// many tests starting processes at once this happens about once in a
/// hundred runs.
fn without_setpgid_notes(output: &str) -> String {
    let is_note = |line: &str| {
        line.starts_with("sh: child setpgid (") && line.ends_with("): Operation not permitted")
    };
    if !output.lines().any(is_note) {
        return output.to_owned();
    }
    eprintln!("left out the shell's notes in {output:?}");
    let kept: Vec<&str> = output.lines().filter(|line| !is_note(line)).collect();
    kept.join("\n")
}

fn open(cx: &mut TestAppContext) -> App {
    // The terminals' and the socket's threads wake the app from outside
    // the test scheduler.
    cx.executor().allow_parking();
    cx.update(bind_keys);
    let dir: PathBuf = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap()
        .keep();
    let fakes = dir.join("bin");
    std::fs::create_dir(&fakes).unwrap();
    let codex = fakes.join("codex");
    std::fs::write(&codex, FAKE_CODEX).unwrap();
    std::fs::set_permissions(&codex, std::fs::Permissions::from_mode(0o755)).unwrap();
    let socket = dir.join("benterm.sock");
    let start: ipc::StartTerminal = Rc::new(StartedTerminal::spawn);
    let window = cx
        .update(|cx| {
            ipc::open_window(
                shell(&fakes),
                start,
                Some(socket),
                ConfigFeed::defaults(),
                cx,
            )
        })
        .unwrap();
    let mut cx = VisualTestContext::from_window(window.into(), cx);
    cx.run_until_parked();
    let pane = window
        .update(&mut cx, |root, _, cx| {
            root.grid(cx).read(cx).focused().get()
        })
        .unwrap();
    let mut app = App {
        window,
        cx,
        pane,
        ran: 0,
    };
    app.wait_for(pane, "the prompt", |text| text.ends_with('$'));
    app
}

/// Splits the pane the commands are typed in and returns the new pane.
fn split(app: &mut App) -> u64 {
    let split = app.run("benterm split --down");
    assert_eq!(split.status, "0", "{}", split.output);
    let pane: u64 = split
        .output
        .trim()
        .parse()
        .unwrap_or_else(|_| panic!("split printed {:?}", split.output));
    app.wait_for(pane, "the prompt of the new pane", |text| {
        text.ends_with('$')
    });
    pane
}

#[gpui::test]
fn the_list_has_a_line_for_each_pane_and_one_of_them_is_the_callers(cx: &mut TestAppContext) {
    let mut app = open(cx);
    let other = split(&mut app);

    let list = app.run("benterm list");

    assert_eq!(list.status, "0");
    let ids: Vec<&str> = list
        .output
        .lines()
        .map(|line| line.split(' ').next().unwrap())
        .collect();
    assert_eq!(ids, [app.pane.to_string(), other.to_string()]);
    let own = app.run(r#"echo "$BENTERM_PANE_ID""#);
    assert_eq!(own.output, app.pane.to_string());
}

#[gpui::test]
fn every_example_of_the_skill_runs_and_asks_codex_in_another_pane(cx: &mut TestAppContext) {
    let skill = skill();
    let lines = example_lines(&skill);
    assert!(!lines.is_empty(), "the skill has no sh examples");
    let mut app = open(cx);
    // The pane beside runs Codex, as a user would have it.
    let peer = split(&mut app);
    let started = app.run(&format!("benterm send --pane {peer} --enter codex"));
    assert_eq!(started.status, "0");
    app.wait_for(peer, "codex to start", |text| text.contains("ready"));
    // As a user's Codex would be by the time an agent asks it, it is
    // known to the app and at its prompt.
    app.wait_until("the app to see codex at its prompt", |app| {
        (app.agent_of(peer) == (Some("codex"), AgentState::Done)).then_some(())
    });

    let ran: Vec<Ran> = lines.iter().map(|line| app.run(line)).collect();

    for ran in &ran {
        assert_eq!(
            ran.status, "0",
            "{:?} failed and printed {:?}",
            ran.line, ran.output
        );
    }
    let find = |start: &str| {
        ran.iter()
            .find(|ran| ran.line.starts_with(start))
            .unwrap_or_else(|| panic!("the skill has no example that starts with {start:?}"))
    };
    // The examples find the pane of Codex.
    let echo = app.run(r#"echo "$PEER""#);
    assert_eq!(echo.output, peer.to_string());
    // The wait returns once Codex has answered, which takes it two
    // seconds, and well within the time the example gives it.
    let wait = find(r#"benterm agent wait "$PEER""#);
    assert_eq!(wait.output, format!("{peer} done"));
    assert!(
        wait.took >= Duration::from_secs(2) && wait.took < Duration::from_secs(20),
        "the wait took {:?}",
        wait.took
    );
    let read = find(r#"benterm agent read "$PEER""#);
    assert!(
        read.output.contains("answer: no problems found"),
        "{:?}",
        read.output
    );
    // And an agent started for the job answers the same way.
    let wait = find("benterm agent wait reviewer");
    assert_eq!(wait.output, "reviewer done");
    let read = find("benterm agent read reviewer");
    assert!(
        read.output.contains("answer: no problems found"),
        "{:?}",
        read.output
    );
}

//! What the agent commands are for: a shell in a pane of the app runs
//! `benterm agent ...` to start a worker behind the window, tell it
//! what to do, read it and wait for it, and the worker reports back.
//!
//! The window is a real one without a display, the terminals run real
//! shells on PTYs, and the commands are the real `benterm` binary,
//! found on the `PATH` and typed at the prompt as a user would.

use std::ffi::OsString;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::rc::Rc;
use std::time::{Duration, Instant};

use benterm_terminal::PtyConfig;
use benterm_ui::settings::ConfigFeed;
use benterm_ui::{BentermWindow, StartedTerminal, bind_keys, ipc};
use gpui::{TestAppContext, VisualTestContext, WindowHandle};

const TIMEOUT: Duration = Duration::from_secs(30);

/// An interactive shell with a plain prompt, no startup files, and the
/// built `benterm` first on its `PATH`.
fn shell() -> PtyConfig {
    let bin = Path::new(env!("CARGO_BIN_EXE_benterm"))
        .parent()
        .expect("the binary is in a directory");
    let mut path = OsString::from(bin);
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
    /// How many commands were typed.
    ran: usize,
}

impl App {
    fn screen(&mut self) -> String {
        self.window
            .update(&mut self.cx, |root, _, cx| {
                root.terminal(cx).read(cx).text(None)
            })
            .expect("the window is open")
    }

    /// Lets the app run until `done` holds for the screen of the pane.
    /// The shells and the commands run in real time, and the app's timers
    /// on the test's clock, which is moved along with it.
    fn wait_for(&mut self, what: &str, done: impl Fn(&str) -> bool) -> String {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            self.cx.run_until_parked();
            let text = self.screen();
            if done(&text) {
                return text;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {what}; the pane shows {text:?}"
            );
            std::thread::sleep(Duration::from_millis(5));
            self.cx.executor().advance_clock(Duration::from_millis(5));
        }
    }

    /// Types a command at the prompt and waits for it to end. Returns
    /// its exit status and what it printed on standard output.
    fn run(&mut self, command: &str) -> (String, String) {
        // A mark of its own for every command: the output of the ones
        // before is still on the screen.
        self.ran += 1;
        let mark = format!("m{}=", self.ran);
        self.cx.simulate_input(&format!(
            r#"R=$({command} 2>/dev/null); echo "{mark}$?:$R.""#
        ));
        self.cx.simulate_keystrokes("enter");
        let start = format!("\n{mark}");
        let ended = |text: &str| {
            text.rsplit_once(&start)
                .and_then(|(_, rest)| rest.split_once('.'))
                .and_then(|(result, _)| result.split_once(':'))
                .map(|(status, output)| (status.to_owned(), output.to_owned()))
        };
        let text = self.wait_for(&mark, |text| ended(text).is_some());
        ended(&text).unwrap()
    }
}

fn open(cx: &mut TestAppContext) -> App {
    // The terminals' and the socket's threads wake the app from outside
    // the test scheduler.
    cx.executor().allow_parking();
    cx.update(bind_keys);
    let dir = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap();
    let socket = dir.keep().join("benterm.sock");
    let start: ipc::StartTerminal = Rc::new(StartedTerminal::spawn);
    let window = cx
        .update(|cx| ipc::open_window(shell(), start, Some(socket), ConfigFeed::defaults(), cx))
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
    app.wait_for("the prompt", |text| text.ends_with('$'));
    app
}

#[gpui::test]
fn a_shell_starts_a_worker_talks_to_it_and_gets_its_report(cx: &mut TestAppContext) {
    let mut app = open(cx);
    let parent = app.pane;

    // The worker is a shell of its own, behind the window: no tab opens.
    let (status, started) = app.run("benterm agent run --name w1 -- sh -i");
    let worker: u64 = started
        .strip_prefix("w1 ")
        .and_then(|pane| pane.parse().ok())
        .unwrap_or_else(|| panic!("agent run ended with {status} and printed {started:?}"));
    let tabs = |app: &mut App| {
        app.window
            .update(&mut app.cx, |root, _, cx| root.tabs().read(cx).tabs().len())
            .unwrap()
    };
    assert_eq!(tabs(&mut app), 1);
    let (_, listed) = app.run("benterm agent list --children");
    assert_eq!(listed, format!("w1 {worker} {parent} background unknown"));

    // It is told what to do by name, and its screen is read by name.
    let (status, _) = app.run(r#"benterm agent send w1 'echo "sum=$((40+2))"'"#);
    assert_eq!(status, "0");
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let (_, sums) = app.run("benterm agent read w1 | grep -c '^sum=42'");
        if sums == "1" {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the worker never printed the sum"
        );
    }

    // A shell is no agent the app knows the state of, so a wait for it
    // runs out: with a status of its own, after the time it was given.
    let started = Instant::now();
    let (status, _) = app.run("benterm agent wait w1 --wait 1");
    assert_eq!(status, "3");
    assert!(started.elapsed() >= Duration::from_millis(900));

    // The worker reports. The shell that started it is no agent, so the
    // report is not typed at its prompt, where the shell would take it
    // for a command: it waits in the inbox of the pane.
    let (status, _) = app.run(r#"benterm agent send w1 'benterm agent report "done: PR #1"'"#);
    assert_eq!(status, "0");
    let deadline = Instant::now() + TIMEOUT;
    loop {
        app.cx.run_until_parked();
        let waiting = app
            .window
            .update(&mut app.cx, |root, _, _| root.inbox_len(parent))
            .expect("the window is open");
        if waiting == 1 {
            break;
        }
        assert!(Instant::now() < deadline, "the report is not in the inbox");
        std::thread::sleep(Duration::from_millis(5));
        app.cx.executor().advance_clock(Duration::from_millis(5));
    }
    let report = format!("<benterm from=w1 pane={worker}>");
    let screen = app.screen();
    assert!(!screen.contains(&report), "{screen:?}");

    // And it is opened as a tab when somebody wants to look at it.
    let (_, shown) = app.run("benterm agent show w1");
    assert_eq!(shown, worker.to_string());
    assert_eq!(tabs(&mut app), 2);
    let (_, listed) = app.run("benterm agent list");
    assert_eq!(listed, format!("w1 {worker} {parent} tab unknown"));
}

#[gpui::test]
fn list_and_agent_list_with_json_print_one_json_object_per_line(cx: &mut TestAppContext) {
    let mut app = open(cx);
    let out = tempfile::tempdir().unwrap();
    let capture = |app: &mut App, file: &str, command: &str| {
        let path = out.path().join(file);
        let (status, _) = app.run(&format!("{command} > '{}'", path.display()));
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        (status, text)
    };

    // With no agents, `agent list --json` prints nothing and exits 0.
    let (status, empty) = capture(&mut app, "agents-empty.jsonl", "benterm agent list --json");
    assert_eq!((status.as_str(), empty.as_str()), ("0", ""));

    let (status, _) = app.run("benterm agent run --name w1 -- sh -i");
    assert_eq!(status, "0");
    let (status, _) = app.run("benterm agent run --name w2 -- sh -i");
    assert_eq!(status, "0");

    // `agent list --json` prints two JSON lines with `name` and `state`.
    let (status, agents_json) = capture(&mut app, "agents.jsonl", "benterm agent list --json");
    assert_eq!(status, "0");
    let agents: Vec<serde_json::Value> = agents_json
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(agents.len(), 2, "{agents_json:?}");
    assert_eq!(agents[0]["name"], "w1");
    assert_eq!(agents[1]["name"], "w2");
    for agent in &agents {
        assert!(agent["state"].is_string(), "{agent}");
    }

    // `list --json` prints as many lines as `list`, each valid JSON.
    let (status, plain_list) = capture(&mut app, "panes.txt", "benterm list");
    assert_eq!(status, "0");
    let (status, json_list) = capture(&mut app, "panes.jsonl", "benterm list --json");
    assert_eq!(status, "0");
    let panes: Vec<serde_json::Value> = json_list
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(panes.len(), plain_list.lines().count(), "{json_list:?}");
    assert_eq!(panes.len(), 3, "{json_list:?}");
    for pane in &panes {
        assert!(pane["pane"].is_u64(), "{pane}");
        assert!(pane["state"].is_string(), "{pane}");
    }
}

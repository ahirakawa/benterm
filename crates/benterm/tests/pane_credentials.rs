//! A pane's credentials work only from the processes of the pane, and what a pane does
//! every day still works: a hook, the commands, `benterm agent run`, a worker's report, and a command
//! that an agent runs in a sandbox and that reaches the socket itself.
//!
//! The window is a real one without a display, the terminals run real
//! shells on PTYs, and the commands are the real `benterm` binary.

use std::ffi::OsString;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;
use std::rc::Rc;
use std::time::{Duration, Instant};

use benterm_terminal::PtyConfig;
use benterm_ui::settings::ConfigFeed;
use benterm_ui::{BentermWindow, StartedTerminal, bind_keys, ipc};
use gpui::{TestAppContext, VisualTestContext, WindowHandle};

const TIMEOUT: Duration = Duration::from_secs(30);

fn bin_dir() -> &'static Path {
    Path::new(env!("CARGO_BIN_EXE_benterm"))
        .parent()
        .expect("the binary is in a directory")
}

/// An interactive shell with a plain prompt, no startup files, and the
/// built `benterm` first on its `PATH`.
fn shell() -> PtyConfig {
    let mut path = OsString::from(bin_dir());
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
    /// How many commands were typed.
    ran: usize,
    /// Where the panes leave files for each other and for the test.
    dir: tempfile::TempDir,
}

impl App {
    fn screen(&mut self) -> String {
        self.window
            .update(&mut self.cx, |root, _, cx| {
                root.terminal(cx).read(cx).text(None)
            })
            .expect("the window is open")
    }

    fn pass_time(&mut self) {
        self.cx.run_until_parked();
        std::thread::sleep(Duration::from_millis(5));
        self.cx.executor().advance_clock(Duration::from_millis(5));
    }

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
            self.pass_time();
        }
    }

    /// Types a command at the prompt of the window's pane and waits for
    /// it to end. Returns its exit status and what it printed, standard
    /// error included.
    fn run(&mut self, command: &str) -> (String, String) {
        self.ran += 1;
        let mark = format!("m{}=", self.ran);
        self.cx
            .simulate_input(&format!(r#"R=$({command} 2>&1); echo "{mark}$?:$R.""#));
        self.cx.simulate_keystrokes("enter");
        let start = format!("\n{mark}");
        let ended = |text: &str| {
            text.rsplit_once(&start)
                .and_then(|(_, rest)| rest.rsplit_once('.'))
                .and_then(|(result, _)| result.split_once(':'))
                .map(|(status, output)| (status.to_owned(), output.to_owned()))
        };
        let text = self.wait_for(&mark, |text| ended(text).is_some());
        ended(&text).unwrap()
    }

    /// Waits for a file a pane writes, and returns its text.
    fn file(&mut self, name: &str) -> String {
        let path = self.dir.path().join(name);
        let deadline = Instant::now() + TIMEOUT;
        loop {
            if let Ok(text) = std::fs::read_to_string(&path) {
                return text;
            }
            assert!(Instant::now() < deadline, "nobody wrote {name}");
            self.pass_time();
        }
    }
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
        .update(|cx| ipc::open_window(shell(), start, Some(socket), ConfigFeed::defaults(), cx))
        .unwrap();
    let cx = VisualTestContext::from_window(window.into(), cx);
    let mut app = App {
        window,
        cx,
        ran: 0,
        dir,
    };
    app.wait_for("the prompt", |text| text.ends_with('$'));
    app
}

/// The input of the issue: the credential of the window's pane, read by
/// a worker in another pane and by a process outside the app's panes.
#[gpui::test]
fn the_credential_of_a_pane_is_refused_from_other_processes(cx: &mut TestAppContext) {
    let mut app = open(cx);
    let dir = app.dir.path().to_owned();
    let (status, _) = app.run(&format!(
        r#"printf %s "$BENTERM_CONTROL_TOKEN" > '{0}/token'; printf %s "$BENTERM_SOCKET" > '{0}/socket'"#,
        dir.display()
    ));
    assert_eq!(status, "0");
    let token = app.file("token");
    assert_eq!(token.len(), 64, "{token:?}");

    // The pane itself may list the panes with it.
    let (status, listed) = app.run("benterm list");
    assert_eq!(status, "0", "{listed}");

    // A worker without control may not, with its own credential or with
    // the one it read.
    let (status, started) = app.run("benterm agent run --name w1 -- sh -i");
    assert_eq!(status, "0", "{started}");
    // In single quotes, so that the worker's shell expands it.
    let steal = format!(
        "BENTERM_CONTROL_TOKEN=$(cat {0}/token) benterm list > {0}/stolen.part 2>&1; echo status=$? >> {0}/stolen.part; mv {0}/stolen.part {0}/stolen",
        dir.display()
    );
    let (status, sent) = app.run(&format!("benterm agent send w1 '{steal}'"));
    assert_eq!(status, "0", "{sent}");
    let stolen = app.file("stolen");
    assert!(stolen.contains("status=1"), "{stolen}");
    assert!(stolen.contains("belongs to a pane"), "{stolen}");
    assert!(!stolen.contains(&token), "{stolen}");

    // Nor may a process that is in no pane.
    let socket = app.file("socket");
    let (sender, answer) = std::sync::mpsc::channel();
    let outside_token = token.clone();
    std::thread::spawn(move || {
        let output = Command::new(env!("CARGO_BIN_EXE_benterm"))
            .arg("list")
            .env("BENTERM_SOCKET", socket)
            .env("BENTERM_CONTROL_TOKEN", outside_token)
            .env_remove("BENTERM_PANE_TOKEN")
            .env_remove("BENTERM_PANE_ID")
            .output()
            .unwrap();
        let _ = sender.send(output);
    });
    let deadline = Instant::now() + TIMEOUT;
    let output = loop {
        if let Ok(output) = answer.try_recv() {
            break output;
        }
        assert!(Instant::now() < deadline, "the command did not end");
        app.pass_time();
    };
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(stderr.contains("belongs to a pane"), "{stderr}");
    assert!(!stderr.contains(&token), "{stderr}");
}

/// A hook of an agent is a process the agent starts, some shells below
/// the pane's program.
#[gpui::test]
fn a_hook_in_a_pane_reaches_the_app(cx: &mut TestAppContext) {
    let mut app = open(cx);
    let hook = r#"{"session_id":"0b1c","hook_event_name":"UserPromptSubmit"}"#;
    let (status, printed) = app.run(&format!(
        r#"printf %s '{hook}' | sh -c 'sh -c "benterm hook UserPromptSubmit --agent claude"'"#
    ));
    assert_eq!((status.as_str(), printed.as_str()), ("0", ""));

    // The hook ends quietly whatever the app answers, so the app is
    // asked what it heard.
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let (status, listed) = app.run("benterm list --json");
        assert_eq!(status, "0", "{listed}");
        if listed.contains(r#""agent":"claude""#) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the hook was not heard: {listed}"
        );
    }
}

/// A worker reports with its own pane's credential, and a worker with
/// control starts a worker of its own: both from their own panes.
#[gpui::test]
fn workers_use_their_own_credentials_from_their_own_panes(cx: &mut TestAppContext) {
    let mut app = open(cx);
    let dir = app.dir.path().to_owned();
    let (status, started) = app.run("benterm agent run --name w1 --control -- sh -i");
    assert_eq!(status, "0", "{started}");

    // In single quotes, so that the worker's shell expands it.
    let recorded = |command: &str, file: &str| {
        format!(
            "benterm agent send w1 '{command} > {0}/{file}.part 2>&1; echo status=$? >> {0}/{file}.part; mv {0}/{file}.part {0}/{file}'",
            dir.display()
        )
    };
    let (status, sent) = app.run(&recorded("benterm agent run --name w2 -- sh -i", "run"));
    assert_eq!(status, "0", "{sent}");
    let run = app.file("run");
    assert!(run.starts_with("w2 ") && run.contains("status=0"), "{run}");

    // The app answers a report only once it has taken it.
    let (status, sent) = app.run(&recorded("benterm agent report done", "report"));
    assert_eq!(status, "0", "{sent}");
    let report = app.file("report");
    assert!(report.contains("status=0"), "{report}");
    assert!(!report.contains("belongs to a pane"), "{report}");
}

/// A command an agent runs in a sandbox that lets it reach the socket
/// talks to the app itself, without the mailbox, and is still a
/// descendant of the pane's program. Where a sandbox cannot be entered,
/// as inside another, there is nothing to check.
#[cfg(target_os = "macos")]
#[gpui::test]
fn a_sandboxed_command_of_a_pane_reaches_the_app(cx: &mut TestAppContext) {
    let mut app = open(cx);
    let sandbox = "/usr/bin/sandbox-exec -p '(version 1)(allow default)(deny file-write* (subpath \"/private/var/empty\"))'";
    let (status, printed) = app.run(&format!("{sandbox} /usr/bin/true"));
    if status != "0" {
        eprintln!("skipped: sandbox-exec cannot start a sandbox here: {printed}");
        return;
    }

    let bin = bin_dir().join("benterm");
    let (status, listed) = app.run(&format!("{sandbox} '{}' list", bin.display()));
    assert_eq!(status, "0", "{listed}");
    assert!(!listed.is_empty());
}

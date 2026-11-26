//! What a role is for on the command line: a shell in a pane of the app
//! runs `benterm role run` to start a worker of a role behind the
//! window, with the role's instructions and the task as its first message.
//!
//! The window is a real one without a display, the terminals run real
//! shells on PTYs, the command is the real `benterm` binary, and the
//! agent is a made-up `claude` that writes down its arguments.

use std::ffi::OsString;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use benterm_terminal::PtyConfig;
use benterm_ui::settings::ConfigFeed;
use benterm_ui::{BentermWindow, StartedTerminal, bind_keys, ipc};
use gpui::{TestAppContext, VisualTestContext, WindowHandle};

const TIMEOUT: Duration = Duration::from_secs(30);

const REVIEWER: &str = r#"
name = "reviewer"
agent = "claude"
model = "opus"
permission_mode = "auto"
prompt = "差分を読んで指摘する"
"#;

const PLANNER: &str = r#"
name = "planner"
agent = "claude"
prompt = "計画を立てる"

[env]
FOO = "bar"
"#;

struct App {
    window: WindowHandle<BentermWindow>,
    cx: VisualTestContext,
    pane: u64,
    ran: usize,
    /// Where the made-up claude writes its arguments.
    args: PathBuf,
    _dir: tempfile::TempDir,
}

impl App {
    fn screen(&mut self) -> String {
        self.window
            .update(&mut self.cx, |root, _, cx| {
                root.terminal(cx).read(cx).text(None)
            })
            .expect("the window is open")
    }

    fn wait_for(&mut self, what: &str, done: impl Fn(&mut Self) -> bool) {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            self.cx.run_until_parked();
            if done(self) {
                return;
            }
            let text = self.screen();
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
        self.ran += 1;
        let mark = format!("m{}=", self.ran);
        self.cx.simulate_input(&format!(
            r#"R=$({command} 2>/dev/null); echo "{mark}$?:$R.""#
        ));
        self.cx.simulate_keystrokes("enter");
        let start = format!("\n{mark}");
        let ended = move |text: &str| {
            text.rsplit_once(&start)
                .and_then(|(_, rest)| rest.split_once('.'))
                .and_then(|(result, _)| result.split_once(':'))
                .map(|(status, output)| (status.to_owned(), output.to_owned()))
        };
        let ended_too = ended.clone();
        self.wait_for(&mark, move |app| ended_too(&app.screen()).is_some());
        ended(&self.screen()).unwrap()
    }
}

fn executable(path: &Path, text: &str) {
    std::fs::write(path, text).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn open(cx: &mut TestAppContext) -> App {
    cx.executor().allow_parking();
    cx.update(bind_keys);
    let dir = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap();
    let root = dir.path().canonicalize().unwrap();
    let roles = root.join("config/benterm/roles");
    std::fs::create_dir_all(&roles).unwrap();
    std::fs::write(roles.join("reviewer.toml"), REVIEWER).unwrap();
    std::fs::write(roles.join("planner.toml"), PLANNER).unwrap();
    let fake = root.join("fake");
    std::fs::create_dir(&fake).unwrap();
    let args = root.join("args");
    executable(
        &fake.join("claude"),
        &format!(
            "#!/bin/sh\nprintf '%s\\0' \"$@\" > {}.part && env > {0}.env && mv {0}.part {0}\n\
             exec sleep 100\n",
            args.display()
        ),
    );

    let bin = Path::new(env!("CARGO_BIN_EXE_benterm"))
        .parent()
        .expect("the binary is in a directory");
    let mut path = OsString::from(&fake);
    path.push(":");
    path.push(bin);
    path.push(":/usr/bin:/bin");
    let mut shell = PtyConfig::command("/bin/sh", ["-i"]);
    shell.env = vec![
        ("PATH".into(), path),
        ("PS1".into(), "$ ".into()),
        ("ENV".into(), "".into()),
        ("XDG_CONFIG_HOME".into(), root.join("config").into()),
    ];

    let socket = root.join("benterm.sock");
    let start: ipc::StartTerminal = Rc::new(StartedTerminal::spawn);
    let window = cx
        .update(|cx| ipc::open_window(shell, start, Some(socket), ConfigFeed::defaults(), cx))
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
        args,
        _dir: dir,
    };
    app.wait_for("the prompt", |app| app.screen().ends_with('$'));
    app
}

#[gpui::test]
fn a_shell_starts_a_worker_of_a_role_on_a_task(cx: &mut TestAppContext) {
    let mut app = open(cx);
    let parent = app.pane;

    let (status, started) = app.run("benterm role run reviewer -- 'PR #1 を見る'");
    let worker: u64 = started
        .strip_prefix("reviewer ")
        .and_then(|pane| pane.parse().ok())
        .unwrap_or_else(|| panic!("role run ended with {status} and printed {started:?}"));

    // A child of the pane, behind the window.
    let (_, listed) = app.run("benterm agent list --children");
    let line = format!("reviewer {worker} {parent} background ");
    assert!(listed.starts_with(&line), "{listed:?}");
    let tabs = app
        .window
        .update(&mut app.cx, |root, _, cx| root.tabs().read(cx).tabs().len())
        .unwrap();
    assert_eq!(tabs, 1);

    // The made-up claude was given the role's options and first message.
    let args_file = app.args.clone();
    app.wait_for("the agent's arguments", move |_| args_file.exists());
    let args = std::fs::read(&app.args).unwrap();
    let args: Vec<String> = args
        .split(|byte| *byte == 0)
        .filter(|arg| !arg.is_empty())
        .map(|arg| String::from_utf8(arg.to_vec()).unwrap())
        .collect();
    assert_eq!(
        args,
        [
            "--model",
            "opus",
            "--permission-mode",
            "auto",
            "--",
            "差分を読んで指摘する\n\nPR #1 を見る",
        ]
    );
}

#[gpui::test]
fn a_worker_of_a_role_gets_the_roles_variables(cx: &mut TestAppContext) {
    let mut app = open(cx);

    let (status, started) = app.run("benterm role run planner");
    assert!(started.starts_with("planner "), "{status} {started:?}");

    let args_file = app.args.clone();
    app.wait_for("the agent's arguments", move |_| args_file.exists());
    let env = std::fs::read_to_string(app.args.with_extension("env")).unwrap();
    assert!(env.lines().any(|line| line == "FOO=bar"), "{env}");
}

//! The kanban of the progress board in a window: a task made through the
//! socket moves through the columns as its agent reports, and when it is
//! closed. The repository and the window are real.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use benterm_ipc::{Client, ClientError, CloseTask, CreateTask, Method, PaneId, ReportHook};
use benterm_terminal::PtyConfig;
use benterm_ui::kanban::{Card, Column};
use benterm_ui::settings::ConfigFeed;
use benterm_ui::{BentermWindow, MainView, StartedTerminal, bind_keys, ipc};
use gpui::{TestAppContext, VisualTestContext, WindowHandle};

const TIMEOUT: Duration = Duration::from_secs(10);

fn git(dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .output()
        .unwrap();
    assert!(output.status.success(), "git {args:?}");
}

struct Benterm {
    window: WindowHandle<BentermWindow>,
    cx: VisualTestContext,
    socket: PathBuf,
    repo: PathBuf,
    _dir: tempfile::TempDir,
}

/// A window with a session, a socket, and a place for worktrees of its
/// own, and a repository to make tasks in.
fn open(cx: &mut TestAppContext) -> Benterm {
    cx.executor().allow_parking();
    cx.update(bind_keys);
    let dir = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap();
    let root = dir.path().canonicalize().unwrap();
    let repo = root.join("api");
    std::fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("README.md"), "one\n").unwrap();
    git(&repo, &["add", "README.md"]);
    git(&repo, &["commit", "-q", "-m", "one"]);
    let config = root.join("config/config.toml");
    std::fs::create_dir_all(config.parent().unwrap()).unwrap();
    let trees = root.join("trees");
    std::fs::write(
        &config,
        format!("[worktrees]\ndirectory = {:?}\n", trees.to_str().unwrap()),
    )
    .unwrap();
    let socket = root.join("benterm.sock");
    let session = Some(root.join("state/session.json"));
    let start: ipc::StartTerminal = Rc::new(StartedTerminal::spawn);
    let mut pty = PtyConfig::command("/bin/sh", ["-c", "sleep 100"]);
    pty.cwd = Some(repo.clone());
    let feed = ConfigFeed::watch(&config);
    let window = cx
        .update(|cx| ipc::open_session_window(pty, start, Some(socket.clone()), session, feed, cx))
        .unwrap();
    let cx = VisualTestContext::from_window(window.into(), cx);
    cx.run_until_parked();
    Benterm {
        window,
        cx,
        socket,
        repo,
        _dir: dir,
    }
}

impl Benterm {
    fn root<T>(&mut self, read: impl FnOnce(&BentermWindow, &gpui::App) -> T) -> T {
        self.window.read_with(&self.cx, read).unwrap()
    }

    fn wait_until(&mut self, what: &str, mut done: impl FnMut(&mut Self) -> bool) {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            self.cx.run_until_parked();
            if done(self) {
                return;
            }
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(5));
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
            self.cx.run_until_parked();
            if let Ok(result) = answer.recv_timeout(Duration::from_millis(5)) {
                return result;
            }
            assert!(Instant::now() < deadline, "no answer before the timeout");
        }
    }

    fn hook(&mut self, pane: PaneId, event: &str) {
        let hook = ReportHook {
            pane,
            agent: "claude".into(),
            event: event.into(),
            tool_name: None,
            notification_type: None,
            session_id: Some("s1".into()),
            agent_id: None,
            tool_input: None,
            tool_response: None,
        };
        self.call(hook).unwrap();
    }

    fn cards(&mut self) -> Vec<Card> {
        let Some(board) = self.root(|root, _| root.progress_board()) else {
            return Vec::new();
        };
        board.read_with(&self.cx, |board, _| board.cards().to_vec())
    }

    fn column_of(&mut self, task: u64) -> Option<Column> {
        let cards = self.cards();
        cards
            .iter()
            .find(|card| card.task == task)
            .map(|card| card.column)
    }

    fn wait_for_column(&mut self, task: u64, column: Column) {
        self.wait_until(&format!("{column:?}"), |benterm| {
            benterm.column_of(task) == Some(column)
        });
    }

    fn drawn(&mut self, selector: &'static str) -> bool {
        let _ = self
            .window
            .update(&mut self.cx, |_, window, _| window.refresh());
        self.cx.run_until_parked();
        self.cx.debug_bounds(selector).is_some()
    }
}

#[gpui::test]
fn a_task_moves_through_the_columns_as_its_agent_reports(cx: &mut TestAppContext) {
    let mut benterm = open(cx);
    let created = benterm
        .call(CreateTask {
            name: "add-export-csv".into(),
            cwd: benterm.repo.to_str().unwrap().into(),
            ..CreateTask::default()
        })
        .unwrap();
    let task = created.task.id;
    let pane = created.pane;

    benterm.cx.simulate_keystrokes("cmd-shift-b");
    assert_eq!(
        benterm.root(|root, _| root.main_view()),
        MainView::ProgressBoard
    );
    benterm.wait_for_column(task, Column::NotStarted);
    let card = benterm.cards().into_iter().find(|card| card.task == task);
    assert_eq!(card.map(|card| card.meta), Some("api".into()));
    for column in ["kanban-column-0", "kanban-column-1"] {
        assert!(benterm.drawn(column), "{column}");
    }
    assert!(benterm.drawn("kanban-column-2"));
    assert!(!benterm.drawn("kanban-column-3"));
    assert!(benterm.drawn("kanban-card-0-0"), "the card in not started");

    benterm.hook(pane, "UserPromptSubmit");
    benterm.wait_for_column(task, Column::AgentWorking);
    assert!(benterm.drawn("kanban-card-1-0"));
    assert!(!benterm.drawn("kanban-card-0-0"));

    benterm.hook(pane, "Stop");
    benterm.wait_for_column(task, Column::AwaitingReview);
    assert!(benterm.drawn("kanban-card-2-0"));

    benterm
        .call(CloseTask {
            task,
            remove: false,
            force: false,
        })
        .unwrap();
    benterm.wait_for_column(task, Column::Done);
    assert!(!benterm.drawn("kanban-card-3-0"));
    assert!(benterm.drawn("toggle-completed-tasks"));
    let toggle = benterm.cx.debug_bounds("toggle-completed-tasks").unwrap();
    benterm
        .cx
        .simulate_click(toggle.center(), gpui::Modifiers::none());
    assert!(benterm.drawn("kanban-card-3-0"));
}

#[gpui::test]
fn the_kanban_stays_under_the_graph_of_the_agents(cx: &mut TestAppContext) {
    let mut benterm = open(cx);
    benterm.cx.simulate_keystrokes("cmd-shift-b r");
    assert!(benterm.drawn("kanban-column-0"));
}

#[gpui::test]
fn switching_workspaces_shows_only_tasks_of_that_repository(cx: &mut TestAppContext) {
    let mut benterm = open(cx);
    let first = benterm.repo.clone();
    let second = first.with_file_name("other");
    std::fs::create_dir(&second).unwrap();
    git(&second, &["init", "-q", "-b", "main"]);
    git(&second, &["commit", "-q", "--allow-empty", "-m", "initial"]);

    let mut make = |repo: &Path, name: &str| {
        benterm
            .call(CreateTask {
                name: name.into(),
                cwd: repo.to_str().unwrap().into(),
                ..CreateTask::default()
            })
            .unwrap()
            .task
            .id
    };
    let a = make(&first, "a");
    let b = make(&first, "b");
    let c = make(&second, "c");

    benterm.cx.simulate_keystrokes("cmd-shift-b");
    let ids = |benterm: &mut Benterm| {
        benterm
            .cards()
            .into_iter()
            .map(|card| card.task)
            .collect::<Vec<_>>()
    };
    assert_eq!(ids(&mut benterm), [a, b]);
    let workspace = benterm.root(|root, _| {
        root.tasks()
            .into_iter()
            .find(|row| row.task.id == c)
            .unwrap()
            .workspace
            .unwrap()
    });
    benterm
        .window
        .update(&mut benterm.cx, |root, window, cx| {
            root.select_workspace(workspace, window, cx);
        })
        .unwrap();
    assert_eq!(ids(&mut benterm), [c]);
}

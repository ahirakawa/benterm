//! Tasks in the window: making one makes a branch, a worktree and a
//! workspace in it with the agent of the chosen role, closing one asks
//! what becomes of the worktree, and a restart brings the tasks back.
//!
//! The window is a real one, the repository is a real one in a temporary
//! directory, and the agent is a made-up `claude` that writes down its
//! arguments and where it runs.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use benterm_ipc::{
    Client, ClientError, CloseTask, CreateTask, ErrorCode, ListTasks, Method, TaskInfo,
    WorktreeLeft,
};
use benterm_layout::WorkspaceId;
use benterm_terminal::PtyConfig;
use benterm_ui::settings::ConfigFeed;
use benterm_ui::{BentermWindow, CloseWorkspace, NewTask, StartedTerminal, bind_keys, ipc, menu};
use gpui::{Action, OwnedMenuItem, TestAppContext, VisualTestContext, WindowHandle};

const TIMEOUT: Duration = Duration::from_secs(10);

const REVIEWER: &str = r#"
name = "reviewer"
description = "PR を読む"
agent = "claude"
model = "opus"
permission_mode = "auto"
prompt = "差分を読んで指摘する"
"#;

/// A role with variables, one of them a name every pane gets, and more
/// arguments for its agent.
const PLANNER: &str = r#"
name = "planner"
agent = "claude"
prompt = "計画を立てる"
args = ["--add-dir", "../docs"]

[env]
FOO = "bar"
BENTERM_SOCKET = "mine"
"#;

/// The buttons of the question a closing task asks.
const KEEP: &str = "Keep Worktree";
const REMOVE: &str = "Remove Worktree and Branch";

struct Place {
    dir: tempfile::TempDir,
    root: PathBuf,
    /// The checkout, with one commit on `main`.
    repo: PathBuf,
    /// Where the worktrees go.
    trees: PathBuf,
    /// Where the made-up claude writes its arguments.
    args: PathBuf,
}

struct App {
    window: WindowHandle<BentermWindow>,
    cx: VisualTestContext,
    socket: PathBuf,
}

fn git(dir: &Path, args: &[&str]) -> String {
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
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn executable(path: &Path, text: &str) {
    std::fs::write(path, text).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

impl Place {
    fn new() -> Self {
        let dir = tempfile::Builder::new()
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir()
            .unwrap();
        let root = dir.path().canonicalize().unwrap();
        let repo = root.join("benterm");
        std::fs::create_dir(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        std::fs::write(repo.join("README.md"), "one\n").unwrap();
        git(&repo, &["add", "README.md"]);
        git(&repo, &["commit", "-q", "-m", "one"]);
        let trees = root.join("trees");
        let config = root.join("config");
        std::fs::create_dir_all(config.join("roles")).unwrap();
        std::fs::write(config.join("roles/reviewer.toml"), REVIEWER).unwrap();
        std::fs::write(
            config.join("config.toml"),
            format!("[worktrees]\ndirectory = {:?}\n", trees.to_str().unwrap()),
        )
        .unwrap();
        let args = root.join("args");
        std::fs::create_dir(&args).unwrap();
        std::fs::create_dir(root.join("fake")).unwrap();
        executable(
            &root.join("fake/claude"),
            &format!(
                "#!/bin/sh\n[ \"$1\" = --help ] && {{ echo '  --no-daemon'; exit 0; }}\n\
                 f={}/$BENTERM_PANE_ID\npwd > $f.cwd\nenv > $f.env\n\
                 printf '%s\\0' \"$@\" > $f.part && mv $f.part $f\n\
                 echo started\nexec sleep 100\n",
                args.display()
            ),
        );
        Self {
            dir,
            root,
            repo,
            trees,
            args,
        }
    }

    fn head(&self) -> String {
        git(&self.repo, &["rev-parse", "HEAD"])
    }

    fn branches(&self) -> Vec<String> {
        git(&self.repo, &["branch", "--format=%(refname:short)"])
            .lines()
            .map(str::to_owned)
            .collect()
    }

    fn worktrees(&self) -> usize {
        git(&self.repo, &["worktree", "list"]).lines().count()
    }

    fn open(&self, cx: &mut TestAppContext) -> App {
        cx.executor().allow_parking();
        cx.update(|cx| {
            bind_keys(cx);
            menu::install(cx);
        });
        let mut pty = PtyConfig::command("/bin/sh", ["-c", "echo ready; exec sleep 100"]);
        pty.cwd = Some(self.repo.clone());
        pty.env = vec![(
            "PATH".into(),
            format!("{}/fake:/usr/bin:/bin", self.root.display()).into(),
        )];
        let socket = self.root.join("benterm.sock");
        let start: ipc::StartTerminal = Rc::new(StartedTerminal::spawn);
        let feed = ConfigFeed::watch(&self.root.join("config/config.toml"));
        let session = self.root.join("state/session.json");
        let window = cx
            .update(|cx| {
                ipc::open_session_window(pty, start, Some(socket.clone()), Some(session), feed, cx)
            })
            .unwrap();
        let vcx = VisualTestContext::from_window(window.into(), cx);
        vcx.run_until_parked();
        let mut app = App {
            window,
            cx: vcx,
            socket,
        };
        app.pass_time();
        app
    }

    /// The arguments the made-up claude was started with in `pane`, and
    /// the directory it was started in.
    fn claude_in(&self, app: &mut App, pane: u64) -> (Vec<String>, PathBuf) {
        let file = self.args.join(pane.to_string());
        app.wait_until("the agent's arguments", |_| file.exists());
        let args = std::fs::read(&file)
            .unwrap()
            .split(|byte| *byte == 0)
            .filter(|arg| !arg.is_empty())
            .map(|arg| String::from_utf8(arg.to_vec()).unwrap())
            .collect();
        let cwd = std::fs::read_to_string(self.args.join(format!("{pane}.cwd"))).unwrap();
        (args, PathBuf::from(cwd.trim()))
    }

    fn request(&self, name: &str) -> CreateTask {
        CreateTask {
            name: name.to_owned(),
            cwd: self.repo.to_str().unwrap().to_owned(),
            ..CreateTask::default()
        }
    }
}

impl App {
    fn pass_time(&mut self) {
        self.cx.run_until_parked();
        std::thread::sleep(Duration::from_millis(10));
        self.cx.executor().advance_clock(Duration::from_millis(10));
        self.cx.run_until_parked();
    }

    fn wait_until(&mut self, what: &str, mut done: impl FnMut(&mut Self) -> bool) {
        let deadline = Instant::now() + TIMEOUT;
        while !done(self) {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            self.pass_time();
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
            self.pass_time();
            if let Ok(result) = answer.recv_timeout(Duration::from_millis(5)) {
                return result;
            }
            assert!(Instant::now() < deadline, "no answer before the timeout");
        }
    }

    fn workspaces(&mut self) -> Vec<WorkspaceId> {
        self.window
            .update(&mut self.cx, |root, _, _| root.workspaces())
            .unwrap()
    }

    fn tasks(&mut self) -> Vec<TaskInfo> {
        self.call(ListTasks {}).unwrap().tasks
    }

    /// The directory of the program in the first pane of `workspace`.
    fn directory_of(&mut self, workspace: u64) -> Option<PathBuf> {
        self.window
            .update(&mut self.cx, |root, _, cx| {
                let id = root
                    .workspaces()
                    .into_iter()
                    .find(|id| id.get() == workspace)?;
                let grid = root.workspace_tabs(id)?.read(cx).selected_grid().clone();
                let grid = grid.read(cx);
                grid.terminal(grid.panes()[0])?.read(cx).working_directory()
            })
            .unwrap()
    }

    fn select(&mut self, workspace: u64) {
        self.window
            .update(&mut self.cx, |root, window, cx| {
                let id = root
                    .workspaces()
                    .into_iter()
                    .find(|id| id.get() == workspace)
                    .expect("the workspace is there");
                root.select_workspace(id, window, cx);
            })
            .unwrap();
        self.pass_time();
    }

    fn dispatch(&mut self, action: impl Action) {
        self.window
            .update(&mut self.cx, |_, window, cx| {
                window.dispatch_action(action.boxed_clone(), cx)
            })
            .unwrap();
        self.pass_time();
    }

    /// Asks to close the workspace of the task `created` and gives the
    /// question that is asked, as its text and what is below it.
    fn ask_to_close(&mut self, workspace: u64) -> (String, String) {
        self.select(workspace);
        self.dispatch(CloseWorkspace);
        self.wait_until("the question", |app| app.cx.has_pending_prompt());
        self.cx.pending_prompt().unwrap()
    }

    fn answer(&mut self, button: &str) {
        self.cx.simulate_prompt_answer(button);
        self.pass_time();
    }

    fn form_error(&mut self) -> Option<String> {
        self.window
            .update(&mut self.cx, |root, _, cx| {
                root.task_form().and_then(|form| form.read(cx).error())
            })
            .unwrap()
    }

    /// Brings the window to the front, as the user's window is: focus
    /// changes are reported only in the active window.
    fn activate(&mut self) {
        self.cx.update(|window, _| window.activate_window());
        self.pass_time();
    }

    /// What the form holds in its fields with text: the name, the
    /// branch is left out, the work and the repository.
    fn form_texts(&mut self) -> (String, String, String) {
        self.window
            .update(&mut self.cx, |root, _, cx| {
                let form = root.task_form().expect("the form is open");
                let form = form.read(cx);
                (form.name(cx), form.work(cx), form.repository(cx))
            })
            .unwrap()
    }

    /// Clicks the row of a field of the form, by its selector, at its
    /// right end, where the field is and no role is.
    fn click_field(&mut self, selector: &'static str) {
        let row = self.cx.debug_bounds(selector).expect("the row is drawn");
        let on_field = gpui::point(row.right() - gpui::px(8.), row.center().y);
        self.cx.simulate_click(on_field, gpui::Modifiers::none());
        self.pass_time();
    }

    /// Checks that nothing typed while the form was open reached the
    /// terminal of the focused pane: once the form is closed, `marker`
    /// is typed there and shows; what came before it would show first.
    fn expect_terminal_got_nothing_but(&mut self, marker: &str, not: &str) {
        assert!(!self.form_is_open());
        let keys: Vec<String> = marker.chars().map(String::from).collect();
        self.cx.simulate_keystrokes(&keys.join(" "));
        let text = |app: &mut Self| {
            app.window
                .update(&mut app.cx, |root, _, cx| {
                    root.focused_terminal(cx).unwrap().read(cx).text(None)
                })
                .unwrap()
        };
        self.wait_until("the marker on the terminal", |app| {
            text(app).contains(marker)
        });
        let shown = text(self);
        assert!(
            !shown.contains(not),
            "the terminal got what was typed into the form: {shown:?}"
        );
    }

    fn form_is_open(&mut self) -> bool {
        self.window
            .update(&mut self.cx, |root, _, _| root.task_form().is_some())
            .unwrap()
    }
}

fn workspace_of(task: &TaskInfo) -> u64 {
    task.workspace.expect("the task has a workspace").get()
}

#[gpui::test]
fn a_task_gets_a_branch_a_worktree_and_a_workspace_in_it(cx: &mut TestAppContext) {
    let place = Place::new();
    let mut app = place.open(cx);
    let head = place.head();

    let created = app.call(place.request("kanban")).unwrap();

    let task = &created.task;
    assert_eq!(task.name, "kanban");
    assert_eq!(task.branch, "kanban");
    assert_eq!(task.repository, place.repo.to_str().unwrap());
    assert_eq!(task.base_branch, "main");
    assert_eq!(task.base_commit, head);
    let worktree = place.trees.join("benterm/kanban");
    assert_eq!(task.worktree, worktree.to_str().unwrap());
    assert_eq!(place.worktrees(), 2);
    assert_eq!(place.branches(), ["kanban", "main"]);

    // One more workspace, named after the task, with a shell in the
    // worktree.
    let workspaces = app.workspaces();
    assert_eq!(workspaces.len(), 2);
    let workspace = workspace_of(task);
    assert_eq!(workspaces[1].get(), workspace);
    let name = app
        .window
        .update(&mut app.cx, |root, _, _| root.workspace_name(workspaces[1]))
        .unwrap();
    assert_eq!(name.as_deref(), Some("kanban"));
    app.wait_until("the shell in the worktree", |app| {
        app.directory_of(workspace).as_deref() == Some(worktree.as_path())
    });
    assert_eq!(app.tasks(), std::slice::from_ref(&created.task));

    // The window tells its other views the same.
    let rows = app
        .window
        .update(&mut app.cx, |root, _, _| root.tasks())
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].task.base_commit, head);
    assert_eq!(rows[0].task.worktree, worktree);
    assert_eq!(rows[0].workspace, Some(workspaces[1]));
}

#[gpui::test]
fn a_task_with_a_role_starts_the_roles_agent_in_the_worktree(cx: &mut TestAppContext) {
    let place = Place::new();
    let mut app = place.open(cx);

    let created = app
        .call(CreateTask {
            role: Some("reviewer".into()),
            ..place.request("kanban")
        })
        .unwrap();

    let (args, cwd) = place.claude_in(&mut app, created.pane.get());
    assert_eq!(
        args,
        [
            "--model",
            "opus",
            "--permission-mode",
            "auto",
            "--",
            "差分を読んで指摘する"
        ]
    );
    assert_eq!(cwd, place.trees.join("benterm/kanban"));
    assert_eq!(created.task.role.as_deref(), Some("reviewer"));
    // The agent is one the window knows by name, in the task's workspace.
    let agents = app
        .window
        .update(&mut app.cx, |root, _, cx| root.agents(cx))
        .unwrap();
    assert_eq!(agents.len(), 1);
    assert_eq!(agents[0].name, "reviewer");
    assert_eq!(agents[0].pane.get(), created.pane.get());
    assert!(!agents[0].background);
    assert_eq!(app.workspaces().len(), 2);
}

#[gpui::test]
fn a_task_with_a_role_gives_its_agent_the_roles_variables_and_arguments(cx: &mut TestAppContext) {
    let place = Place::new();
    std::fs::write(place.root.join("config/roles/planner.toml"), PLANNER).unwrap();
    let mut app = place.open(cx);

    let created = app
        .call(CreateTask {
            role: Some("planner".into()),
            work: Some("作業".into()),
            ..place.request("plan")
        })
        .unwrap();

    let pane = created.pane.get();
    let (args, _) = place.claude_in(&mut app, pane);
    assert_eq!(args, ["--add-dir", "../docs", "--", "計画を立てる\n\n作業"]);
    let env = std::fs::read_to_string(place.args.join(format!("{pane}.env"))).unwrap();
    let env: Vec<&str> = env.lines().collect();
    assert!(env.contains(&"FOO=bar"), "{env:?}");
    // The role's value is put over the pane's.
    assert!(env.contains(&"BENTERM_SOCKET=mine"), "{env:?}");
}

#[gpui::test]
fn the_work_follows_the_roles_instructions_in_the_first_message(cx: &mut TestAppContext) {
    let place = Place::new();
    let mut app = place.open(cx);

    let created = app
        .call(CreateTask {
            role: Some("reviewer".into()),
            work: Some("かんばんの列を足す".into()),
            ..place.request("kanban")
        })
        .unwrap();

    let (args, _) = place.claude_in(&mut app, created.pane.get());
    assert_eq!(
        args.last().map(String::as_str),
        Some("差分を読んで指摘する\n\nかんばんの列を足す")
    );
}

#[gpui::test]
fn a_branch_that_exists_makes_nothing_and_says_why(cx: &mut TestAppContext) {
    let place = Place::new();
    let mut app = place.open(cx);
    git(&place.repo, &["branch", "kanban"]);

    let error = app.call(place.request("kanban")).unwrap_err();

    let ClientError::Api(error) = error else {
        panic!("{error:?}");
    };
    assert_eq!(error.code, ErrorCode::Failed);
    assert!(error.message.contains("kanban already exists"), "{error:?}");
    assert_eq!(app.workspaces().len(), 1);
    assert_eq!(place.worktrees(), 1);
    assert_eq!(app.tasks(), []);
}

#[gpui::test]
fn a_role_that_is_not_there_makes_nothing(cx: &mut TestAppContext) {
    let place = Place::new();
    let mut app = place.open(cx);

    let error = app
        .call(CreateTask {
            role: Some("nobody".into()),
            ..place.request("kanban")
        })
        .unwrap_err();

    assert!(error.to_string().contains("nobody"), "{error}");
    assert_eq!(place.branches(), ["main"]);
    assert_eq!(app.workspaces().len(), 1);
}

#[gpui::test]
fn closing_and_removing_takes_the_worktree_the_branch_and_the_workspace(cx: &mut TestAppContext) {
    let place = Place::new();
    let mut app = place.open(cx);
    let created = app.call(place.request("kanban")).unwrap();

    let (question, detail) = app.ask_to_close(workspace_of(&created.task));
    assert!(question.contains("kanban"), "{question}");
    assert!(!detail.contains("uncommitted"), "{detail}");
    app.answer(REMOVE);

    app.wait_until("the workspace to close", |app| app.workspaces().len() == 1);
    assert_eq!(place.worktrees(), 1);
    assert_eq!(place.branches(), ["main"]);
    assert!(!place.trees.join("benterm/kanban").exists());
    let tasks = app.tasks();
    assert_eq!(tasks[0].workspace, None);
    assert_eq!(
        tasks[0].closed.as_ref().map(|closed| closed.worktree),
        Some(WorktreeLeft::Removed)
    );
}

#[gpui::test]
fn the_question_says_how_many_files_have_uncommitted_changes(cx: &mut TestAppContext) {
    let place = Place::new();
    let mut app = place.open(cx);
    let created = app.call(place.request("kanban")).unwrap();
    let worktree = PathBuf::from(&created.task.worktree);
    std::fs::write(worktree.join("new.txt"), "new\n").unwrap();
    std::fs::write(worktree.join("README.md"), "changed\n").unwrap();

    let (_, detail) = app.ask_to_close(workspace_of(&created.task));
    assert!(detail.contains("2 uncommitted files"), "{detail}");

    // Having seen it, the user removes it all the same.
    app.answer(REMOVE);
    app.wait_until("the workspace to close", |app| app.workspaces().len() == 1);
    assert!(!worktree.exists());
    assert_eq!(place.branches(), ["main"]);
}

#[gpui::test]
fn closing_and_keeping_closes_only_the_workspace(cx: &mut TestAppContext) {
    let place = Place::new();
    let mut app = place.open(cx);
    let created = app.call(place.request("kanban")).unwrap();

    app.ask_to_close(workspace_of(&created.task));
    app.answer(KEEP);

    app.wait_until("the workspace to close", |app| app.workspaces().len() == 1);
    assert!(place.trees.join("benterm/kanban").is_dir());
    assert_eq!(place.branches(), ["kanban", "main"]);
    let tasks = app.tasks();
    assert_eq!(
        tasks[0].closed.as_ref().map(|closed| closed.worktree),
        Some(WorktreeLeft::Kept)
    );
}

#[gpui::test]
fn cancelling_the_question_closes_nothing(cx: &mut TestAppContext) {
    let place = Place::new();
    let mut app = place.open(cx);
    let created = app.call(place.request("kanban")).unwrap();

    app.ask_to_close(workspace_of(&created.task));
    app.answer("Cancel");

    assert_eq!(app.workspaces().len(), 2);
    assert_eq!(app.tasks()[0].closed, None);
    assert_eq!(place.worktrees(), 2);
}

#[gpui::test]
fn a_workspace_that_is_no_task_closes_without_a_question(cx: &mut TestAppContext) {
    let place = Place::new();
    let mut app = place.open(cx);
    app.call(place.request("kanban")).unwrap();
    let first = app.workspaces()[0].get();

    app.select(first);
    app.dispatch(CloseWorkspace);

    assert!(!app.cx.has_pending_prompt());
    assert_eq!(app.workspaces().len(), 1);
}

#[gpui::test]
fn the_socket_closes_a_task_and_removes_only_what_loses_nothing(cx: &mut TestAppContext) {
    let place = Place::new();
    let mut app = place.open(cx);
    let created = app.call(place.request("kanban")).unwrap();
    let worktree = PathBuf::from(&created.task.worktree);
    std::fs::write(worktree.join("new.txt"), "new\n").unwrap();
    let close = |remove, force| CloseTask {
        task: created.task.id,
        remove,
        force,
    };

    let refused = app.call(close(true, false)).unwrap_err();
    let ClientError::Api(refused) = refused else {
        panic!("{refused:?}");
    };
    assert_eq!(refused.code, ErrorCode::AtRisk);
    assert!(
        refused.message.contains("1 uncommitted file"),
        "{refused:?}"
    );
    assert!(worktree.join("new.txt").exists());
    assert_eq!(app.workspaces().len(), 2);

    let closed = app.call(close(true, true)).unwrap();
    assert_eq!(closed.task.workspace, None);
    assert_eq!(closed.branch_left, None);
    assert!(!worktree.exists());
    assert_eq!(place.branches(), ["main"]);
    assert_eq!(app.workspaces().len(), 1);
}

#[gpui::test]
fn a_task_whose_last_pane_ends_is_closed_and_its_worktree_kept(cx: &mut TestAppContext) {
    let place = Place::new();
    let mut app = place.open(cx);
    let created = app.call(place.request("kanban")).unwrap();
    let workspace = workspace_of(&created.task);

    app.window
        .update(&mut app.cx, |root, window, cx| {
            let id = root.workspaces()[1];
            assert_eq!(id.get(), workspace);
            let tabs = root.workspace_tabs(id).unwrap();
            tabs.update(cx, |tabs, cx| tabs.close(window, cx));
        })
        .unwrap();
    app.pass_time();

    assert!(!app.cx.has_pending_prompt());
    assert_eq!(app.workspaces().len(), 1);
    assert!(place.trees.join("benterm/kanban").is_dir());
    let tasks = app.tasks();
    assert_eq!(
        tasks[0].closed.as_ref().map(|closed| closed.worktree),
        Some(WorktreeLeft::Kept)
    );
}

#[gpui::test]
fn a_restart_brings_the_task_and_its_workspace_back(cx: &mut TestAppContext) {
    let place = Place::new();
    let mut app = place.open(cx);
    let created = app.call(place.request("kanban")).unwrap();
    let worktree = PathBuf::from(&created.task.worktree);
    let workspace = workspace_of(&created.task);
    app.wait_until("the shell in the worktree", |app| {
        app.directory_of(workspace).as_deref() == Some(worktree.as_path())
    });
    app.cx.cx.quit();
    drop(app);

    let mut app = place.open(cx);

    let tasks = app.tasks();
    assert_eq!(tasks.len(), 1);
    assert_eq!(
        TaskInfo {
            workspace: None,
            ..tasks[0].clone()
        },
        TaskInfo {
            workspace: None,
            ..created.task.clone()
        }
    );
    let workspaces = app.workspaces();
    assert_eq!(workspaces.len(), 2);
    let restored = workspace_of(&tasks[0]);
    assert_eq!(workspaces[1].get(), restored);
    app.wait_until("the shell in the worktree", |app| {
        app.directory_of(restored).as_deref() == Some(worktree.as_path())
    });
    // It is still the task's workspace: closing it asks.
    let (question, _) = app.ask_to_close(restored);
    assert!(question.contains("kanban"), "{question}");
    app.answer("Cancel");
}

/// The items of the menu bar that run `action`, as the menu's name and
/// the item's name.
fn menu_items<A: Action>(cx: &mut TestAppContext) -> Vec<(String, String)> {
    fn walk<A: Action>(menu: &str, items: &[OwnedMenuItem], found: &mut Vec<(String, String)>) {
        for item in items {
            match item {
                OwnedMenuItem::Action { name, action, .. } if action.as_any().is::<A>() => {
                    found.push((menu.to_owned(), name.to_string()));
                }
                OwnedMenuItem::Submenu(submenu) => walk::<A>(menu, &submenu.items, found),
                _ => {}
            }
        }
    }
    let menus = cx.update(|cx| cx.get_menus()).expect("the menus are set");
    let mut found = Vec::new();
    for menu in &menus {
        walk::<A>(&menu.name, &menu.items, &mut found);
    }
    found
}

#[gpui::test]
fn the_menu_bar_and_the_palette_open_the_form_for_a_new_task(cx: &mut TestAppContext) {
    let place = Place::new();
    let mut app = place.open(cx);
    assert_eq!(
        menu_items::<NewTask>(cx),
        [("Workspace".to_owned(), "New Task…".to_owned())]
    );

    app.cx.simulate_keystrokes("cmd-k");
    app.cx.simulate_input("new task");
    app.pass_time();
    let first = app
        .window
        .update(&mut app.cx, |root, _, cx| {
            let palette = root.command_palette().expect("the palette is open");
            palette.read(cx).shown(cx).first().map(|c| c.name.clone())
        })
        .unwrap();
    assert_eq!(first.as_deref(), Some("Task: New Task"));
    app.cx.simulate_keystrokes("enter");
    app.pass_time();

    assert!(app.form_is_open());
    // The repository is the one the focused pane is in.
    let repository = app
        .window
        .update(&mut app.cx, |root, _, cx| {
            root.task_form().unwrap().read(cx).repository(cx)
        })
        .unwrap();
    assert_eq!(repository, place.repo.to_str().unwrap());

    // Escape closes it, and nothing is made.
    app.cx.simulate_keystrokes("escape");
    app.pass_time();
    assert!(!app.form_is_open());
    assert_eq!(place.branches(), ["main"]);
}

#[gpui::test]
fn the_form_makes_a_task_with_the_chosen_role_and_the_work(cx: &mut TestAppContext) {
    let place = Place::new();
    let mut app = place.open(cx);
    app.dispatch(NewTask);

    // Name, then the work, the role and the branch.
    app.cx.simulate_input("かんばん");
    app.cx.simulate_keystrokes("tab");
    app.cx.simulate_input("かんばんの列を足す");
    app.cx.simulate_keystrokes("tab");
    let role = |app: &mut App| {
        app.window
            .update(&mut app.cx, |root, _, cx| {
                root.task_form().unwrap().read(cx).role()
            })
            .unwrap()
    };
    assert_eq!(role(&mut app), None);
    app.cx.simulate_keystrokes("right");
    assert_eq!(role(&mut app).as_deref(), Some("reviewer"));
    app.cx.simulate_keystrokes("tab");
    app.cx.simulate_input("issue-93-kanban");
    app.cx.simulate_keystrokes("enter");

    app.wait_until("the task's workspace", |app| app.workspaces().len() == 2);
    assert!(!app.form_is_open());
    let tasks = app.tasks();
    assert_eq!(tasks[0].name, "かんばん");
    assert_eq!(tasks[0].branch, "issue-93-kanban");
    assert_eq!(tasks[0].role.as_deref(), Some("reviewer"));
    // The new workspace is the one on view, and its agent has the work.
    let (selected, pane) = app
        .window
        .update(&mut app.cx, |root, _, cx| {
            (
                root.selected_workspace().get(),
                root.grid(cx).read(cx).focused().get(),
            )
        })
        .unwrap();
    assert_eq!(Some(selected), tasks[0].workspace.map(|w| w.get()));
    let (args, cwd) = place.claude_in(&mut app, pane);
    assert_eq!(
        args.last().map(String::as_str),
        Some("差分を読んで指摘する\n\nかんばんの列を足す")
    );
    assert_eq!(cwd, place.trees.join("benterm/issue-93-kanban"));
}

/// The repository of the place has no remote. An open issue of its local
/// issues file is given to an agent through the form every task is made
/// with, and its node on the graph then says that it has a task.
#[gpui::test]
fn a_local_issue_becomes_a_task_on_its_branch_and_its_node_says_so(cx: &mut TestAppContext) {
    use benterm_ui::issue_graph::IssueNodeStatus;

    let place = Place::new();
    assert_eq!(git(&place.repo, &["remote"]), "");
    std::fs::create_dir(place.repo.join(".benterm")).unwrap();
    std::fs::write(
        place.repo.join(".benterm/issues.json"),
        r#"{"version": 1, "issues": [
            {"number": 1, "title": "Epic"},
            {"number": 3, "title": "Login", "parent": 1, "description": "Fix the login"}
        ]}"#,
    )
    .unwrap();
    let mut app = place.open(cx);
    let status = |app: &mut App| {
        app.window
            .update(&mut app.cx, |root, _, cx| {
                let view = root.issue_graph_view()?;
                let status = view.read(cx).model()?.node(3)?.status;
                Some(status)
            })
            .unwrap()
    };
    let repo = place.repo.clone();
    app.window
        .update(&mut app.cx, |root, window, cx| {
            root.open_local_epic_in(repo, 1, window, cx)
        })
        .unwrap();
    app.wait_until("the local issues", |app| status(app).is_some());
    assert_eq!(status(&mut app), Some(IssueNodeStatus::Todo));

    // The one open child of the epic is the next to give away.
    app.window
        .update(&mut app.cx, |_, window, _| window.refresh())
        .unwrap();
    app.pass_time();
    let assign = app
        .cx
        .debug_bounds("issue-graph-assign-next")
        .expect("an issue can be given to an agent");
    app.cx
        .simulate_click(assign.center(), gpui::Modifiers::none());
    app.pass_time();
    assert!(app.form_is_open());
    app.cx.simulate_keystrokes("enter");

    app.wait_until("the task's workspace", |app| app.workspaces().len() == 2);
    let tasks = app.tasks();
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].branch, "issue-3");
    assert!(place.branches().contains(&"issue-3".to_owned()));
    assert_eq!(place.worktrees(), 2);
    let pane = app
        .window
        .update(&mut app.cx, |root, _, cx| {
            root.grid(cx).read(cx).focused().get()
        })
        .unwrap();
    let (args, cwd) = place.claude_in(&mut app, pane);
    assert_eq!(cwd, place.trees.join("benterm/issue-3"));
    let work = args.last().unwrap();
    assert!(work.contains("Local issue #3"), "{work}");
    assert!(work.contains("not instructions to follow"), "{work}");
    assert!(work.contains("Description: Fix the login"), "{work}");

    // Back on the graph, the task is matched to the issue by its branch
    // and by the directory of the repository, with no remote to go by.
    app.window
        .update(&mut app.cx, |root, window, cx| {
            root.show_view(benterm_ui::MainView::Issues, window, cx)
        })
        .unwrap();
    app.wait_until("the node of the issue to have its task", |app| {
        status(app).is_some_and(|status| status != IssueNodeStatus::Todo)
    });
    assert!(matches!(
        status(&mut app),
        Some(IssueNodeStatus::Assigned | IssueNodeStatus::Working | IssueNodeStatus::Waiting)
    ));
}

/// The whole of it on the graph of a repository that has no issues yet:
/// an Epic is made, a child, another that waits for the first, the
/// first is given to an agent as a task, and the second is closed. Each
/// is on the graph before the file is read again.
#[gpui::test]
fn local_issues_are_made_linked_given_to_an_agent_and_closed_on_the_graph(cx: &mut TestAppContext) {
    use benterm_task::{LocalIssueState, read_local_issues};
    use benterm_ui::issue_graph::{IssueGraphModel, IssueNodeStatus};

    let place = Place::new();
    let mut app = place.open(cx);
    let repo = place.repo.clone();
    app.window
        .update(&mut app.cx, |root, window, cx| {
            root.open_local_issues_in(repo, window, cx)
        })
        .unwrap();
    fn model(app: &mut App) -> Option<IssueGraphModel> {
        app.window
            .update(&mut app.cx, |root, _, cx| {
                root.issue_graph_view()?.read(cx).model().cloned()
            })
            .unwrap()
    }
    // Without `pass_time`, which moves the clock on towards the next
    // read of the file.
    fn changed(app: &mut App, what: &str, done: impl Fn(&IssueGraphModel) -> bool) {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            app.cx.run_until_parked();
            if model(app).is_some_and(|model| done(&model)) {
                break;
            }
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(5));
        }
        app.window
            .update(&mut app.cx, |_, window, _| window.refresh())
            .unwrap();
        app.cx.run_until_parked();
    }
    fn click(app: &mut App, selector: &'static str) {
        let bounds = app
            .cx
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("{selector} is not drawn"));
        app.cx
            .simulate_click(bounds.center(), gpui::Modifiers::none());
        app.cx.run_until_parked();
    }
    app.wait_until("the missing file to be said", |app| {
        app.window
            .update(&mut app.cx, |root, _, cx| {
                root.issue_graph_view()
                    .is_some_and(|view| view.read(cx).error().is_some())
            })
            .unwrap()
    });
    app.window
        .update(&mut app.cx, |_, window, _| window.refresh())
        .unwrap();
    app.cx.run_until_parked();

    // An Epic.
    click(&mut app, "issue-graph-first-epic");
    app.cx.simulate_input("Accounts");
    app.cx.simulate_keystrokes("enter");
    changed(&mut app, "the Epic", |model| model.nodes.len() == 1);

    // A child of it.
    app.cx.simulate_keystrokes("a");
    app.cx.simulate_input("Login");
    app.cx.simulate_keystrokes("enter");
    changed(&mut app, "the child", |model| {
        model.parent_edges == [(1, 2)]
    });

    // Another child, and then the first is to be done before it.
    click(&mut app, "issue-node-1");
    app.cx.simulate_keystrokes("a");
    app.cx.simulate_input("Logout");
    app.cx.simulate_keystrokes("enter");
    changed(&mut app, "the second child", |model| {
        model.parent_edges == [(1, 2), (1, 3)]
    });
    click(&mut app, "issue-graph-edit");
    app.cx.simulate_keystrokes("tab tab tab");
    app.cx.simulate_input("2");
    app.cx.simulate_keystrokes("enter");
    changed(&mut app, "the dependency", |model| {
        model.dep_edges == [(2, 3)]
    });

    // The first child becomes a task, through the form of every task.
    click(&mut app, "issue-node-2");
    click(&mut app, "issue-graph-assign");
    assert!(app.form_is_open());
    app.cx.simulate_keystrokes("enter");
    app.wait_until("the task's workspace", |app| app.workspaces().len() == 2);
    let tasks = app.tasks();
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].branch, "issue-2");
    assert_eq!(tasks[0].name, "Login");
    app.window
        .update(&mut app.cx, |root, window, cx| {
            root.show_view(benterm_ui::MainView::Issues, window, cx)
        })
        .unwrap();
    app.wait_until("the node of the issue to have its task", |app| {
        model(app).is_some_and(|model| model.node(2).unwrap().status != IssueNodeStatus::Todo)
    });
    app.window
        .update(&mut app.cx, |_, window, _| window.refresh())
        .unwrap();
    app.cx.run_until_parked();

    // The second child is closed.
    click(&mut app, "issue-node-3");
    click(&mut app, "issue-graph-toggle-state");
    changed(&mut app, "the closed issue", |model| {
        model.node(3).unwrap().status == IssueNodeStatus::Done
    });
    let issues = read_local_issues(&place.repo).unwrap();
    assert_eq!(issues.issue(3).unwrap().state, LocalIssueState::Closed);
    assert_eq!(issues.issue(3).unwrap().blocked_by, [2]);
    assert_eq!(issues.issue(2).unwrap().state, LocalIssueState::Open);
    let model = model(&mut app).unwrap();
    assert_eq!(model.nodes.len(), 3);
    assert_ne!(model.node(2).unwrap().status, IssueNodeStatus::Todo);
}

/// A task on the branch of an open issue of the local file is closed
/// with a second question: whether the issue is closed with it.
#[gpui::test]
fn closing_the_task_of_a_local_issue_asks_whether_the_issue_is_closed_too(cx: &mut TestAppContext) {
    use benterm_task::{LocalIssueState, read_local_issues};

    let place = Place::new();
    std::fs::create_dir(place.repo.join(".benterm")).unwrap();
    std::fs::write(
        place.repo.join(".benterm/issues.json"),
        r#"{"version": 1, "issues": [
            {"number": 3, "title": "Login"},
            {"number": 4, "title": "Logout"},
            {"number": 5, "title": "Done already", "state": "closed"}
        ]}"#,
    )
    .unwrap();
    let state = |number: u64| {
        let issues = read_local_issues(&place.repo).unwrap();
        issues.issue(number).unwrap().state
    };
    let mut app = place.open(cx);
    let on_branch = |app: &mut App, branch: &str| {
        let request = CreateTask {
            branch: Some(branch.to_owned()),
            ..place.request(branch)
        };
        workspace_of(&app.call(request).unwrap().task)
    };
    let question = |app: &mut App| {
        app.wait_until("the second question", |app| app.cx.has_pending_prompt());
        app.cx.pending_prompt().unwrap()
    };

    // Closed with its task.
    let workspace = on_branch(&mut app, "issue-3");
    app.ask_to_close(workspace);
    app.answer(KEEP);
    let (asked, detail) = question(&mut app);
    assert!(asked.contains("local issue #3"), "{asked}");
    assert!(detail.contains("Login"), "{detail}");
    assert_eq!(state(3), LocalIssueState::Open);
    app.answer("Close Issue");
    app.wait_until("the workspace to close", |app| app.workspaces().len() == 1);
    assert!(app.tasks()[0].closed.is_some());
    assert_eq!(state(3), LocalIssueState::Closed);
    assert_eq!(state(4), LocalIssueState::Open);

    // Left open.
    let workspace = on_branch(&mut app, "issue-4");
    app.ask_to_close(workspace);
    app.answer(KEEP);
    question(&mut app);
    app.answer("Leave Issue Open");
    app.wait_until("the workspace to close", |app| app.workspaces().len() == 1);
    assert_eq!(state(4), LocalIssueState::Open);

    // Cancelled at the first question, nothing more is asked or closed.
    let workspace = on_branch(&mut app, "issue-4-again");
    app.ask_to_close(workspace);
    app.answer("Cancel");
    assert!(!app.cx.has_pending_prompt());
    assert_eq!(app.workspaces().len(), 2);
    assert_eq!(state(4), LocalIssueState::Open);

    // An issue that is closed already, and a branch of no issue: the
    // one question of every task.
    for branch in ["issue-5", "kanban"] {
        let workspace = on_branch(&mut app, branch);
        let before = app.workspaces().len();
        app.ask_to_close(workspace);
        app.answer(KEEP);
        app.wait_until("the workspace to close", |app| {
            app.workspaces().len() == before - 1
        });
        assert!(!app.cx.has_pending_prompt());
    }
}

#[gpui::test]
fn the_form_launches_claude_with_the_configured_model_without_a_role(cx: &mut TestAppContext) {
    assert_direct_agent_launch(cx, "claude", "sonnet");
}

#[gpui::test]
fn the_form_launches_codex_with_the_configured_model_without_a_role(cx: &mut TestAppContext) {
    assert_direct_agent_launch(cx, "codex", "test-codex-model");
}

fn assert_direct_agent_launch(cx: &mut TestAppContext, agent: &str, model: &str) {
    let place = Place::new();
    if agent == "codex" {
        std::fs::rename(
            place.root.join("fake/claude"),
            place.root.join("fake/codex"),
        )
        .unwrap();
    }
    let config = place.root.join("config/config.toml");
    let mut text = std::fs::read_to_string(&config).unwrap();
    text.push_str(&format!(
        "\n[tasks]\ndefault_agent = {{ agent = {agent:?}, model = {model:?} }}\n"
    ));
    std::fs::remove_file(place.root.join("config/roles/reviewer.toml")).unwrap();
    std::fs::write(config, text).unwrap();

    let mut app = place.open(cx);
    app.dispatch(NewTask);
    app.cx.simulate_input("model-test");
    app.cx.simulate_keystrokes("tab");
    app.cx.simulate_input("Write a diary entry");
    app.cx.simulate_keystrokes("cmd-enter");

    app.wait_until("the task's workspace", |app| app.workspaces().len() == 2);
    let pane = app
        .window
        .update(&mut app.cx, |root, _, cx| {
            root.grid(cx).read(cx).focused().get()
        })
        .unwrap();
    let (args, _) = place.claude_in(&mut app, pane);
    // Codex runs without its shared background server.
    let mut expected = match agent {
        "codex" => vec!["--no-daemon"],
        _ => vec![],
    };
    expected.extend(["--model", model, "--", "Write a diary entry"]);
    assert_eq!(args, expected);
    assert_eq!(app.tasks()[0].role, None);
}

#[gpui::test]
fn the_form_stays_and_says_why_when_the_task_cannot_be_made(cx: &mut TestAppContext) {
    let place = Place::new();
    let mut app = place.open(cx);
    git(&place.repo, &["branch", "kanban"]);
    app.dispatch(NewTask);

    app.cx.simulate_input("kanban");
    app.cx.simulate_keystrokes("enter");
    app.wait_until("the reason", |app| app.form_error().is_some());

    let error = app.form_error().unwrap();
    assert!(error.contains("kanban already exists"), "{error}");
    assert!(app.form_is_open());
    assert_eq!(app.workspaces().len(), 1);
    assert_eq!(place.worktrees(), 1);
    let _ = &place.dir;
}

fn tasks_file(place: &Place) -> PathBuf {
    place.root.join("state/tasks.json")
}

#[gpui::test]
fn a_task_closed_while_the_app_was_away_does_not_get_its_workspace_back(cx: &mut TestAppContext) {
    let place = Place::new();
    let mut app = place.open(cx);
    let created = app.call(place.request("kanban")).unwrap();
    app.cx.cx.quit();
    drop(app);
    // As when the app ended between closing the task and saving the
    // session without its workspace.
    let store = benterm_task::TaskStore::open(Some(&tasks_file(&place)));
    benterm_task::close(&benterm_task::Git::default(), &store, created.task.id, None).unwrap();

    let mut app = place.open(cx);

    assert_eq!(app.workspaces().len(), 1);
    assert_eq!(app.tasks()[0].workspace, None);
}

#[gpui::test]
fn a_tasks_file_that_cannot_be_read_does_not_cost_a_workspace_its_task(cx: &mut TestAppContext) {
    let place = Place::new();
    let mut app = place.open(cx);
    app.call(place.request("kanban")).unwrap();
    app.cx.cx.quit();
    drop(app);
    let good = std::fs::read(tasks_file(&place)).unwrap();
    std::fs::write(tasks_file(&place), r#"{"version":2,"tasks":[]}"#).unwrap();

    // The workspace is there, and the session still says whose it is.
    let mut app = place.open(cx);
    assert_eq!(app.workspaces().len(), 2);
    app.dispatch(benterm_ui::NewWorkspace);
    app.cx
        .executor()
        .advance_clock(benterm_ui::session::SAVE_INTERVAL);
    app.pass_time();
    app.cx.cx.quit();
    drop(app);
    let session: serde_json::Value =
        serde_json::from_slice(&std::fs::read(place.root.join("state/session.json")).unwrap())
            .unwrap();
    assert_eq!(session["workspaces"][1]["task"], 1);

    // With the file as it was, the task has its workspace again.
    std::fs::write(tasks_file(&place), good).unwrap();
    let mut app = place.open(cx);
    let tasks = app.tasks();
    assert_eq!(
        tasks[0].workspace.map(|w| w.get()),
        Some(app.workspaces()[1].get())
    );
}

#[gpui::test]
fn force_without_remove_is_refused_over_the_socket(cx: &mut TestAppContext) {
    let place = Place::new();
    let mut app = place.open(cx);
    let created = app.call(place.request("kanban")).unwrap();

    let error = app
        .call(CloseTask {
            task: created.task.id,
            remove: false,
            force: true,
        })
        .unwrap_err();

    let ClientError::Api(error) = error else {
        panic!("{error:?}");
    };
    assert_eq!(error.code, ErrorCode::InvalidParams);
    assert_eq!(app.workspaces().len(), 2);
    assert_eq!(app.tasks()[0].closed, None);
}

#[gpui::test]
fn the_question_says_that_ignored_files_go_with_the_worktree(cx: &mut TestAppContext) {
    let place = Place::new();
    let mut app = place.open(cx);
    let created = app.call(place.request("kanban")).unwrap();

    let (_, detail) = app.ask_to_close(workspace_of(&created.task));

    assert!(detail.contains("ignores"), "{detail}");
    app.answer("Cancel");
}

/// Opens the form in a window whose pane has the keyboard, by `open`,
/// types `abc` and checks that it went into the name and not to the
/// terminal.
fn typing_goes_into_the_name(
    cx: &mut TestAppContext,
    open: impl FnOnce(&mut App, &mut TestAppContext),
) {
    let place = Place::new();
    let mut app = place.open(cx);
    app.activate();
    open(&mut app, cx);
    assert!(app.form_is_open());

    app.cx.simulate_keystrokes("a b c");
    app.pass_time();
    assert_eq!(app.form_texts().0, "abc");

    // Escape gives the keyboard back to the pane.
    app.cx.simulate_keystrokes("escape");
    app.pass_time();
    app.expect_terminal_got_nothing_but("xyz", "abc");
}

#[gpui::test]
fn cmd_shift_n_gives_the_name_the_keyboard(cx: &mut TestAppContext) {
    typing_goes_into_the_name(cx, |app, _| {
        app.cx.simulate_keystrokes("cmd-shift-n");
        app.pass_time();
    });
}

#[gpui::test]
fn the_menu_bar_gives_the_name_the_keyboard(cx: &mut TestAppContext) {
    typing_goes_into_the_name(cx, |app, cx| {
        // The menu bar of macOS hands the action to the app, which
        // dispatches it in the active window.
        cx.update(|cx| cx.dispatch_action(&NewTask));
        app.pass_time();
    });
}

#[gpui::test]
fn the_palette_gives_the_name_the_keyboard(cx: &mut TestAppContext) {
    typing_goes_into_the_name(cx, |app, _| {
        app.cx.simulate_keystrokes("cmd-k");
        app.cx.simulate_input("new task");
        app.pass_time();
        app.cx.simulate_keystrokes("enter");
        app.pass_time();
    });
}

#[gpui::test]
fn a_click_on_a_field_gives_it_the_keyboard(cx: &mut TestAppContext) {
    let place = Place::new();
    let mut app = place.open(cx);
    app.activate();
    app.cx.simulate_keystrokes("cmd-shift-n");
    app.pass_time();
    let repository = app.form_texts().2;

    app.click_field("task-field-Work");
    app.cx.simulate_keystrokes("w");
    app.click_field("task-field-Task");
    app.cx.simulate_keystrokes("t");
    app.click_field("task-field-Repository");
    app.cx.simulate_keystrokes("r");
    app.pass_time();
    let (name, work, typed_repository) = app.form_texts();
    assert_eq!((name.as_str(), work.as_str()), ("t", "w"));
    assert_ne!(typed_repository, repository);
    assert!(typed_repository.ends_with('r'), "{typed_repository}");

    // The roles take Right once their row is clicked.
    let role = |app: &mut App| {
        app.window
            .update(&mut app.cx, |root, _, cx| {
                root.task_form().unwrap().read(cx).role()
            })
            .unwrap()
    };
    app.click_field("task-field-Role");
    app.cx.simulate_keystrokes("right");
    app.pass_time();
    assert_eq!(role(&mut app).as_deref(), Some("reviewer"));

    // The work, then Tab goes on from the field clicked.
    app.click_field("task-field-Work");
    app.cx.simulate_keystrokes("b tab");
    app.pass_time();
    assert_eq!(role(&mut app).as_deref(), Some("reviewer"));
    app.cx.simulate_keystrokes("right");
    app.pass_time();
    assert_eq!(role(&mut app), None);

    app.cx.simulate_keystrokes("escape");
    app.pass_time();
    app.expect_terminal_got_nothing_but("xyz", "t");
}

#[gpui::test]
fn only_the_field_with_the_keyboard_shows_the_caret(cx: &mut TestAppContext) {
    let place = Place::new();
    let mut app = place.open(cx);
    app.activate();
    app.cx.simulate_keystrokes("cmd-shift-n");
    app.pass_time();
    let caret_in = |app: &mut App, row: &'static str| {
        let caret = app.cx.debug_bounds("name-editor-caret").expect("a caret");
        let row = app.cx.debug_bounds(row).expect("the row is drawn");
        // By height only: a long repository puts the caret past the
        // right end of the row, where the field clips it.
        let y = caret.center().y;
        row.top() <= y && y <= row.bottom()
    };
    assert!(caret_in(&mut app, "task-field-Task"));

    app.click_field("task-field-Work");
    assert!(caret_in(&mut app, "task-field-Work"));
    app.click_field("task-field-Branch");
    assert!(caret_in(&mut app, "task-field-Branch"));
    app.cx.simulate_keystrokes("tab");
    app.pass_time();
    assert!(caret_in(&mut app, "task-field-Repository"));

    // On the roles no field has the keyboard, and none shows a caret.
    app.click_field("task-field-Role");
    assert!(app.cx.debug_bounds("name-editor-caret").is_none());
}

/// What the agent is to do may take several lines: Enter there starts a
/// new line, and Cmd-Enter makes the task.
#[gpui::test]
fn enter_in_the_work_starts_a_line_and_cmd_enter_makes_the_task(cx: &mut TestAppContext) {
    let place = Place::new();
    let mut app = place.open(cx);
    app.dispatch(NewTask);
    app.cx.simulate_input("lines");
    app.cx.simulate_keystrokes("tab");
    app.cx.simulate_input("one");
    app.cx.simulate_keystrokes("enter");
    app.cx.simulate_input("two");
    app.pass_time();

    assert!(app.form_is_open(), "Enter in the work makes no task");
    let work = app
        .window
        .update(&mut app.cx, |root, _, cx| {
            root.task_form().unwrap().read(cx).work(cx)
        })
        .unwrap();
    assert_eq!(work, "one\ntwo");

    app.cx.simulate_keystrokes("cmd-enter");
    app.wait_until("the task's workspace", |app| app.workspaces().len() == 2);
    assert!(!app.form_is_open());
}

/// The form has buttons for what Cmd-Enter and Escape do.
#[gpui::test]
fn the_buttons_of_the_form_make_the_task_and_close_it(cx: &mut TestAppContext) {
    let place = Place::new();
    let mut app = place.open(cx);
    app.dispatch(NewTask);
    app.pass_time();
    let cancel = app
        .cx
        .debug_bounds("task-cancel")
        .expect("a button to cancel");
    app.cx
        .simulate_click(cancel.center(), gpui::Modifiers::none());
    app.pass_time();
    assert!(!app.form_is_open());

    app.dispatch(NewTask);
    app.cx.simulate_input("by-button");
    app.pass_time();
    let create = app
        .cx
        .debug_bounds("task-create")
        .expect("a button to create");
    app.cx
        .simulate_click(create.center(), gpui::Modifiers::none());
    app.wait_until("the task's workspace", |app| app.workspaces().len() == 2);
    assert!(!app.form_is_open());
}

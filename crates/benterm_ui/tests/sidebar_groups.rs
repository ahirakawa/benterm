//! The sidebar with tasks: the workspaces of a repository's tasks under
//! the workspace of the repository or under its name, folded and opened,
//! chosen with the mouse and with the keyboard, and the same after a
//! restart. The keyboard also goes down into the session list.
//!
//! The window is a real one, and the repository is a real one in a
//! temporary directory.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use benterm_ipc::{
    Client, ClientError, CreateTask, Method, PaneId as ApiPaneId, ReportHook, TaskInfo,
};
use benterm_layout::{PaneId, WorkspaceId};
use benterm_terminal::PtyConfig;
use benterm_ui::settings::ConfigFeed;
use benterm_ui::sidebar_rows::SidebarRow;
use benterm_ui::{BentermWindow, FocusSidebar, StartedTerminal, bind_keys, ipc};
use gpui::{Action, Modifiers, TestAppContext, VisualTestContext, WindowHandle};

const TIMEOUT: Duration = Duration::from_secs(10);

struct Place {
    _dir: tempfile::TempDir,
    root: PathBuf,
    /// The checkout, named benterm, with one commit on `main`.
    repo: PathBuf,
}

struct App {
    window: WindowHandle<BentermWindow>,
    cx: VisualTestContext,
    socket: PathBuf,
}

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
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
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
        let config = root.join("config");
        std::fs::create_dir_all(&config).unwrap();
        let trees = root.join("trees");
        std::fs::write(
            config.join("config.toml"),
            format!("[worktrees]\ndirectory = {:?}\n", trees.to_str().unwrap()),
        )
        .unwrap();
        Self {
            _dir: dir,
            root,
            repo,
        }
    }

    /// Opens the window, whose first workspace is in the repository and
    /// whose panes run a shell that reads lines.
    fn open(&self, cx: &mut TestAppContext) -> App {
        cx.executor().allow_parking();
        cx.update(bind_keys);
        let mut pty = PtyConfig::command("/bin/sh", ["-c", "echo ready; exec sleep 100"]);
        pty.cwd = Some(self.repo.clone());
        let socket = self.root.join("benterm.sock");
        let start: ipc::StartTerminal = Rc::new(StartedTerminal::spawn);
        let feed = ConfigFeed::watch(&self.root.join("config/config.toml"));
        let session = self.root.join("state/session.json");
        let window = cx
            .update(|cx| {
                ipc::open_session_window(pty, start, Some(socket.clone()), Some(session), feed, cx)
            })
            .unwrap();
        let mut vcx = VisualTestContext::from_window(window.into(), cx);
        vcx.update(|window, _| window.activate_window());
        vcx.run_until_parked();
        App {
            window,
            cx: vcx,
            socket,
        }
    }

    fn task(&self, app: &mut App, name: &str) -> TaskInfo {
        let request = CreateTask {
            name: name.to_owned(),
            cwd: self.repo.to_str().unwrap().to_owned(),
            ..CreateTask::default()
        };
        app.call(request).expect("the task is made").task
    }
}

impl App {
    fn pass_time(&mut self) {
        self.cx.run_until_parked();
        std::thread::sleep(Duration::from_millis(5));
        self.cx.executor().advance_clock(Duration::from_millis(5));
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

    fn root<T>(&mut self, read: impl FnOnce(&BentermWindow, &gpui::App) -> T) -> T {
        self.window.read_with(&self.cx, read).unwrap()
    }

    fn rows(&mut self) -> Vec<SidebarRow> {
        self.root(|root, cx| root.sidebar_rows(cx))
    }

    fn workspaces(&mut self) -> Vec<WorkspaceId> {
        self.root(|root, _| root.workspaces())
    }

    fn selected(&mut self) -> WorkspaceId {
        self.root(|root, _| root.selected_workspace())
    }

    fn draw(&mut self) {
        self.cx.run_until_parked();
        let _ = self
            .window
            .update(&mut self.cx, |_, window, _| window.refresh());
        self.cx.run_until_parked();
    }

    fn bounds(&mut self, selector: &'static str) -> gpui::Bounds<gpui::Pixels> {
        self.draw();
        self.cx
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("{selector} is drawn"))
    }

    fn drawn(&mut self, selector: &'static str) -> bool {
        self.draw();
        self.cx.debug_bounds(selector).is_some()
    }

    fn click(&mut self, selector: &'static str) {
        let bounds = self.bounds(selector);
        self.cx.simulate_click(bounds.center(), Modifiers::none());
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

    fn select(&mut self, id: WorkspaceId) {
        self.window
            .update(&mut self.cx, |root, window, cx| {
                root.select_workspace(id, window, cx)
            })
            .unwrap();
        self.pass_time();
    }

    fn hook(&mut self, pane: PaneId, event: &str) {
        let hook = ReportHook {
            pane: ApiPaneId::new(pane.get()),
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

    fn focused(&mut self) -> PaneId {
        self.root(|root, cx| root.grid(cx).read(cx).focused())
    }

    fn pane_has_keyboard(&mut self) -> bool {
        let handle = self.root(|root, cx| root.terminal(cx).read(cx).focus_handle().clone());
        self.cx.update(|window, _| handle.is_focused(window))
    }
}

fn id_of(task: &TaskInfo, workspaces: &[WorkspaceId]) -> WorkspaceId {
    let id = task.workspace.expect("the task has a workspace").get();
    *workspaces
        .iter()
        .find(|workspace| workspace.get() == id)
        .unwrap()
}

/// Two tasks from benterm, and the window waiting until the first
/// workspace is known to be in benterm.
fn two_tasks(place: &Place, cx: &mut TestAppContext) -> (App, WorkspaceId, [WorkspaceId; 2]) {
    let mut app = place.open(cx);
    let first = app.workspaces()[0];
    let one = place.task(&mut app, "fix-a");
    let two = place.task(&mut app, "fix-b");
    let workspaces = app.workspaces();
    let tasks = [id_of(&one, &workspaces), id_of(&two, &workspaces)];
    app.wait_until("the tasks under benterm", |app| {
        matches!(
            app.rows().first(),
            Some(SidebarRow::Workspace { group: Some(_), .. })
        )
    });
    (app, first, tasks)
}

#[gpui::test]
fn two_tasks_of_benterm_are_moved_in_under_its_workspace(cx: &mut TestAppContext) {
    let place = Place::new();
    let (mut app, first, [one, two]) = two_tasks(&place, cx);

    let rows = app.rows();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].workspace(), Some(first));
    assert_eq!(rows[1].workspace(), Some(one));
    assert_eq!(rows[2].workspace(), Some(two));
    let SidebarRow::Workspace {
        group: Some(group), ..
    } = &rows[0]
    else {
        panic!("benterm has the group: {rows:?}");
    };
    assert_eq!(group.repository, place.repo);
    assert_eq!(group.tasks, 2);
    // Drawn moved in, with their branches.
    let outer = app.bounds("workspace-icon-0").left();
    assert!(app.bounds("workspace-icon-1").left() > outer);
    assert!(app.bounds("workspace-icon-2").left() > outer);
    assert!(app.drawn("workspace-branch-1"));
    assert!(app.drawn("workspace-branch-2"));
    assert!(!app.drawn("workspace-branch-0"));
}

#[gpui::test]
fn the_tasks_stay_under_the_name_of_benterm_when_its_workspace_closes(cx: &mut TestAppContext) {
    let place = Place::new();
    let (mut app, first, [one, two]) = two_tasks(&place, cx);

    app.select(first);
    app.dispatch(benterm_ui::CloseWorkspace);

    let rows = app.rows();
    assert_eq!(rows.len(), 3, "{rows:?}");
    let SidebarRow::Heading(group) = &rows[0] else {
        panic!("a heading comes first: {rows:?}");
    };
    assert_eq!(group.name(), "benterm");
    assert_eq!(rows[1].workspace(), Some(one));
    assert_eq!(rows[2].workspace(), Some(two));
    assert!(app.drawn("repository-heading-0"));
}

#[gpui::test]
fn a_click_on_a_row_in_the_group_selects_its_workspace(cx: &mut TestAppContext) {
    let place = Place::new();
    let (mut app, first, [one, _]) = two_tasks(&place, cx);
    app.select(first);

    app.click("workspace-1");

    assert_eq!(app.selected(), one);
}

/// What folds a group is at the left of its row, where the mark of a
/// tree is looked for, large enough to press, with the number of tasks
/// apart from it at the right.
#[gpui::test]
fn the_mark_that_folds_a_group_is_left_of_the_icon(cx: &mut TestAppContext) {
    let place = Place::new();
    let (mut app, first, _) = two_tasks(&place, cx);
    app.select(first);

    let fold = app.bounds("workspace-fold-0");
    let icon = app.bounds("workspace-icon-0");
    assert!(fold.right() <= icon.left(), "{fold:?} is left of {icon:?}");
    assert!(fold.size.width >= gpui::px(16.) && fold.size.height >= gpui::px(16.));
    let count = app.bounds("workspace-count-0");
    assert!(count.left() > icon.right());
    // A press on the number does not fold: it is a press on the row.
    app.click("workspace-count-0");
    assert_eq!(app.rows().len(), 3);

    app.click("workspace-fold-0");
    assert_eq!(app.rows().len(), 1);
    // Folded, the mark is where it was.
    assert_eq!(app.bounds("workspace-fold-0"), fold);
}

#[gpui::test]
fn a_folded_group_hides_its_tasks_and_the_keys_go_past_them(cx: &mut TestAppContext) {
    let place = Place::new();
    let (mut app, first, [one, _]) = two_tasks(&place, cx);
    app.select(first);

    app.click("workspace-fold-0");

    assert_eq!(app.rows().len(), 1);
    assert!(!app.drawn("workspace-1"));
    // The only row shown is benterm's, so the next workspace is benterm's.
    app.dispatch(benterm_ui::SelectNextWorkspace);
    assert_eq!(app.selected(), first);

    app.click("workspace-fold-0");
    assert_eq!(app.rows().len(), 3);
    app.dispatch(benterm_ui::SelectNextWorkspace);
    assert_eq!(app.selected(), one);
}

#[gpui::test]
fn the_keys_follow_the_rows_as_they_are_shown(cx: &mut TestAppContext) {
    let place = Place::new();
    let mut app = place.open(cx);
    let first = app.workspaces()[0];
    // A workspace without a task, after benterm's, and then a task.
    app.cx.simulate_keystrokes("cmd-n");
    let plain = app.selected();
    let task = place.task(&mut app, "fix-a");
    let task = id_of(&task, &app.workspaces());
    app.wait_until("the task under benterm", |app| app.rows().len() == 3);
    app.select(first);

    app.dispatch(benterm_ui::SelectNextWorkspace);
    assert_eq!(app.selected(), task);
    app.dispatch(benterm_ui::SelectNextWorkspace);
    assert_eq!(app.selected(), plain);
    app.dispatch(benterm_ui::SelectPreviousWorkspace);
    assert_eq!(app.selected(), task);
}

#[gpui::test]
fn a_restart_brings_back_the_groups_and_what_was_folded(cx: &mut TestAppContext) {
    let place = Place::new();
    let (mut app, _, _) = two_tasks(&place, cx);
    app.click("workspace-fold-0");
    assert_eq!(app.rows().len(), 1);
    app.cx.cx.quit();
    drop(app);

    let mut app = place.open(cx);
    app.wait_until("the group of benterm", |app| {
        matches!(
            app.rows().first(),
            Some(SidebarRow::Workspace { group: Some(_), .. })
        )
    });
    let rows = app.rows();
    let SidebarRow::Workspace {
        group: Some(group), ..
    } = &rows[0]
    else {
        unreachable!()
    };
    assert!(group.folded);
    assert_eq!(group.tasks, 2);

    app.click("workspace-fold-0");
    let workspaces = app.workspaces();
    let shown: Vec<_> = app
        .rows()
        .iter()
        .filter_map(SidebarRow::workspace)
        .collect();
    assert_eq!(shown, workspaces);
}

#[gpui::test]
fn enter_on_the_second_session_row_goes_to_its_agents_pane(cx: &mut TestAppContext) {
    let place = Place::new();
    let mut app = place.open(cx);
    let first = app.focused();
    app.cx.simulate_keystrokes("cmd-d");
    let second = app.focused();
    assert_ne!(first, second);
    app.hook(first, "UserPromptSubmit");
    app.hook(second, "UserPromptSubmit");
    app.wait_until("two session rows", |app| {
        app.root(|root, cx| root.session_rows(cx)).len() == 2
    });
    // The keyboard in the first pane, so that going to the second shows.
    app.window
        .update(&mut app.cx, |root, window, cx| {
            root.show_pane(first, window, cx);
        })
        .unwrap();
    app.pass_time();
    assert_eq!(app.focused(), first);

    app.dispatch(FocusSidebar);
    assert!(!app.pane_has_keyboard());
    // From the workspace row to the first session row, and the second.
    app.cx.simulate_keystrokes("down down enter");
    app.pass_time();

    assert_eq!(app.focused(), second);
    assert!(app.pane_has_keyboard());
}

#[gpui::test]
fn enter_on_a_workspace_row_goes_to_that_workspace(cx: &mut TestAppContext) {
    let place = Place::new();
    let (mut app, first, [_, two]) = two_tasks(&place, cx);
    app.select(first);

    app.dispatch(FocusSidebar);
    app.cx.simulate_keystrokes("down down enter");
    app.pass_time();

    assert_eq!(app.selected(), two);
    assert!(app.pane_has_keyboard());
}

#[gpui::test]
fn escape_gives_the_keyboard_back_to_the_pane(cx: &mut TestAppContext) {
    let place = Place::new();
    let mut app = place.open(cx);

    app.dispatch(FocusSidebar);
    assert!(!app.pane_has_keyboard());
    app.cx.simulate_keystrokes("escape");
    app.pass_time();

    assert!(app.pane_has_keyboard());
}

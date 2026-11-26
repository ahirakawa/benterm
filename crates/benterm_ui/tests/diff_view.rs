//! The diff of a task, opened from the tabs above the panes: what it
//! shows, how it follows the worktree, and its colors.
//!
//! The repository is a real one in a temporary directory, and the task
//! is made by the same code the window uses.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;
use std::time::{Duration, Instant};

use benterm_config::parse;
use benterm_ipc::{Client, ClientError, CreateTask, Method};
use benterm_task::{
    DIFF_OUTPUT_LIMIT, FileStatus, Git, NEW_FILES_COUNT_LIMIT, NewTask, Task, TaskStore, create,
};
use benterm_terminal::PtyConfig;
use benterm_ui::diff_view::{
    DiffRow, DiffSource, DiffView, RowKind, row_background, row_hover_background,
};
use benterm_ui::settings::ConfigFeed;
use benterm_ui::theme::Theme;
use benterm_ui::{BentermWindow, MainView, StartedTerminal, bind_keys, ipc};
use gpui::{
    AppContext, Entity, Modifiers, TestAppContext, VisualTestContext, WindowHandle, px, size,
};

const TIMEOUT: Duration = Duration::from_secs(10);

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

/// A repository with one commit on `main`, and a task made off it.
struct Place {
    _dir: tempfile::TempDir,
    root: PathBuf,
    repo: PathBuf,
    task: Task,
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
        let store = TaskStore::open(Some(&root.join("state/tasks.json")));
        let new = NewTask {
            directory: repo.clone(),
            name: "kanban".to_owned(),
            branch: None,
            role: None,
            root: root.join("trees"),
        };
        let task = create(&Git::default(), &store, new).unwrap();
        Self {
            _dir: dir,
            root,
            repo,
            task,
        }
    }

    fn source(&self) -> DiffSource {
        DiffSource::of(&self.task)
    }

    fn write(&self, file: &str, text: &str) {
        let path = self.task.worktree.join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn commit(&self, file: &str, text: &str) {
        self.write(file, text);
        git(&self.task.worktree, &["add", file]);
        git(&self.task.worktree, &["commit", "-q", "-m", file]);
    }
}

/// The rows as `+text`, `-text`, ` text` and `@@ ...`.
fn marked(rows: &[DiffRow]) -> Vec<String> {
    rows.iter()
        .map(|row| match row.kind {
            RowKind::Added => format!("+{}", row.text),
            RowKind::Removed => format!("-{}", row.text),
            RowKind::Context => format!(" {}", row.text),
            RowKind::Hunk | RowKind::Note | RowKind::File => row.text.to_string(),
        })
        .collect()
}

fn pass_time(cx: &mut TestAppContext) {
    cx.run_until_parked();
    std::thread::sleep(Duration::from_millis(10));
    cx.executor().advance_clock(Duration::from_millis(10));
    cx.run_until_parked();
}

fn wait_for(
    cx: &mut TestAppContext,
    what: &str,
    mut done: impl FnMut(&mut TestAppContext) -> bool,
) {
    let deadline = Instant::now() + TIMEOUT;
    while !done(cx) {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        pass_time(cx);
    }
}

fn view(place: &Place, cx: &mut TestAppContext) -> Entity<DiffView> {
    cx.executor().allow_parking();
    let source = place.source();
    let view = cx.new(|cx| DiffView::new(Theme::default(), Git::default(), cx));
    view.update(cx, |view, cx| {
        view.set_shown(true, cx);
        view.set_source(Some(source), cx);
    });
    view
}

/// The rows of the file last gone to, without the line that names it.
fn rows(view: &Entity<DiffView>, cx: &mut TestAppContext) -> Vec<String> {
    view.read_with(cx, |view, _| marked(&rows_of_selected(view)))
}

fn rows_of_selected(view: &DiffView) -> Vec<DiffRow> {
    let path = view.selected_file().map(|file| file.path.clone());
    view.rows()
        .iter()
        .filter(|row| Some(row.path.as_ref()) == path.as_deref() && row.kind != RowKind::File)
        .cloned()
        .collect()
}

fn paths(view: &Entity<DiffView>, cx: &mut TestAppContext) -> Vec<(String, FileStatus)> {
    view.read_with(cx, |view, _| {
        view.files()
            .iter()
            .map(|file| (file.path.clone(), file.status))
            .collect()
    })
}

#[gpui::test]
fn committed_uncommitted_and_new_files_are_listed_and_each_can_be_shown(cx: &mut TestAppContext) {
    let place = Place::new();
    place.commit("README.md", "one\ntwo\n");
    place.write("README.md", "one\ntwo\nthree\n");
    place.write("new.txt", "a\nb\n");
    let view = view(&place, cx);

    wait_for(cx, "the diff", |cx| !paths(&view, cx).is_empty());

    assert_eq!(
        paths(&view, cx),
        [
            ("README.md".to_owned(), FileStatus::Modified),
            ("new.txt".to_owned(), FileStatus::Added),
        ]
    );
    assert_eq!(rows(&view, cx), ["@@ -1 +1,3 @@", " one", "+two", "+three"]);
    view.update(cx, |view, cx| view.select_file(1, cx));
    assert_eq!(rows(&view, cx), ["@@ -0,0 +1,2 @@", "+a", "+b"]);
}

#[gpui::test]
fn each_row_knows_its_file_and_its_line_in_the_worktree(cx: &mut TestAppContext) {
    let place = Place::new();
    place.write("README.md", "zero\none\n");
    let view = view(&place, cx);
    wait_for(cx, "the diff", |cx| !paths(&view, cx).is_empty());

    let lines: Vec<_> = view.read_with(cx, |view, _| {
        view.rows()
            .iter()
            .map(|row| {
                row.line_in_worktree()
                    .map(|(path, n)| (path.to_string(), n))
            })
            .collect()
    });

    let readme = |n| Some(("README.md".to_owned(), n));
    // The line that names the file and the `@@` line are no lines.
    assert_eq!(lines, [None, None, readme(1), readme(2)]);
}

#[gpui::test]
fn the_diff_shows_again_when_a_file_of_the_worktree_changes(cx: &mut TestAppContext) {
    let place = Place::new();
    place.write("README.md", "one\ntwo\n");
    let view = view(&place, cx);
    wait_for(cx, "the diff", |cx| {
        rows(&view, cx).contains(&"+two".into())
    });

    place.write("README.md", "one\ntwo\nthree\n");
    wait_for(cx, "the new line", |cx| {
        rows(&view, cx).contains(&"+three".into())
    });

    place.write("later.txt", "later\n");
    wait_for(cx, "the new file", |cx| paths(&view, cx).len() == 2);
}

#[gpui::test]
fn a_hidden_diff_stops_following_and_catches_up_when_shown(cx: &mut TestAppContext) {
    let place = Place::new();
    place.write("README.md", "one\ntwo\n");
    let view = view(&place, cx);
    wait_for(cx, "the diff", |cx| {
        rows(&view, cx).contains(&"+two".into())
    });

    view.update(cx, |view, cx| view.set_shown(false, cx));
    place.write("README.md", "one\nthree\n");
    view.update(cx, |view, cx| view.set_shown(true, cx));

    wait_for(cx, "the change", |cx| {
        rows(&view, cx).contains(&"+three".into())
    });
}

#[gpui::test]
fn the_shown_file_stays_when_the_diff_is_read_again(cx: &mut TestAppContext) {
    let place = Place::new();
    place.write("a.txt", "a\n");
    place.write("b.txt", "b\n");
    let view = view(&place, cx);
    wait_for(cx, "the diff", |cx| paths(&view, cx).len() == 2);
    view.update(cx, |view, cx| view.select_file(1, cx));

    // A file that comes before it in the list.
    place.write("0.txt", "0\n");
    wait_for(cx, "the new file", |cx| paths(&view, cx).len() == 3);

    let selected = view.read_with(cx, |view, _| view.selected_file().map(|f| f.path.clone()));
    assert_eq!(selected.as_deref(), Some("b.txt"));
}

#[gpui::test]
fn a_worktree_without_changes_says_so(cx: &mut TestAppContext) {
    let place = Place::new();
    let view = view(&place, cx);
    wait_for(cx, "the diff", |cx| {
        view.read_with(cx, |view, _| {
            view.notice().is_some_and(|n| n.contains("No changes"))
        })
    });
}

#[gpui::test]
fn no_task_means_no_diff(cx: &mut TestAppContext) {
    let view = cx.new(|cx| DiffView::new(Theme::default(), Git::default(), cx));
    view.update(cx, |view, cx| view.set_source(None, cx));
    let notice = view.read_with(cx, |view, _| view.notice());
    assert!(notice.unwrap().contains("no task"));
    assert!(paths(&view, cx).is_empty());
}

/// A row that takes a comment is lighter under the mouse, and still of
/// its own color; the `@@` line and the notes take none and do not change.
#[test]
fn a_row_that_takes_a_comment_is_lighter_under_the_mouse() {
    let theme = Theme::default().diff;
    let brightness =
        |color: benterm_core::Rgb| u32::from(color.0) + u32::from(color.1) + u32::from(color.2);
    for (kind, resting) in [
        (RowKind::Added, theme.added_background),
        (RowKind::Removed, theme.removed_background),
        (RowKind::Context, theme.background),
    ] {
        let hovered = row_hover_background(kind, &theme).expect("a color under the mouse");
        assert!(brightness(hovered) > brightness(resting), "{kind:?}");
    }
    let added = row_hover_background(RowKind::Added, &theme).unwrap();
    let removed = row_hover_background(RowKind::Removed, &theme).unwrap();
    assert!(added.1 > added.0, "an added row stays green");
    assert!(removed.0 > removed.1, "a removed row stays red");
    assert_eq!(row_hover_background(RowKind::Hunk, &theme), None);
    assert_eq!(row_hover_background(RowKind::Note, &theme), None);
}

#[test]
fn added_and_removed_rows_take_the_two_colors_of_the_theme() {
    let theme = Theme::default().diff;
    assert_eq!(
        row_background(RowKind::Added, &theme),
        Some(theme.added_background)
    );
    assert_eq!(
        row_background(RowKind::Removed, &theme),
        Some(theme.removed_background)
    );
    assert_ne!(theme.added_background, theme.removed_background);
    assert_eq!(
        row_background(RowKind::Hunk, &theme),
        Some(theme.hunk_background)
    );
    assert_eq!(row_background(RowKind::Context, &theme), None);
}

#[test]
fn the_colors_of_the_diff_come_from_the_config_file() {
    let config = parse(
        "[colors.diff]\n\
         added_background = \"#010203\"\n\
         removed_background = \"#040506\"\n\
         removed_marker = \"#070809\"\n",
    )
    .unwrap();
    assert!(config.warnings.is_empty(), "{:?}", config.warnings);
    let theme = Theme::from_config(&config.config).diff;
    assert_eq!(theme.added_background, benterm_core::Rgb(1, 2, 3));
    assert_eq!(theme.removed_background, benterm_core::Rgb(4, 5, 6));
    assert_eq!(theme.removed_marker, benterm_core::Rgb(7, 8, 9));
}

/// A window as the app opens it.
struct Benterm {
    window: WindowHandle<BentermWindow>,
    cx: VisualTestContext,
    _dir: tempfile::TempDir,
}

fn open(cx: &mut TestAppContext) -> Benterm {
    open_with(cx, None, ConfigFeed::defaults())
}

fn open_with(cx: &mut TestAppContext, socket: Option<PathBuf>, feed: ConfigFeed) -> Benterm {
    cx.executor().allow_parking();
    cx.update(bind_keys);
    let dir = tempfile::tempdir().unwrap();
    let session = Some(dir.path().join("state").join("session.json"));
    let start: ipc::StartTerminal = Rc::new(StartedTerminal::spawn);
    let pty = PtyConfig::command("/bin/sh", ["-c", "sleep 100"]);
    let window = cx
        .update(|cx| ipc::open_session_window(pty, start, socket, session, feed, cx))
        .unwrap();
    let cx = VisualTestContext::from_window(window.into(), cx);
    cx.simulate_resize(size(px(1200.), px(800.)));
    cx.run_until_parked();
    Benterm {
        window,
        cx,
        _dir: dir,
    }
}

impl Benterm {
    fn root<T>(&mut self, read: impl FnOnce(&BentermWindow, &gpui::App) -> T) -> T {
        self.window.read_with(&self.cx, read).unwrap()
    }

    fn click(&mut self, selector: &'static str) {
        self.drawn(selector);
        let bounds = self
            .cx
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("{selector} is not drawn"));
        self.cx
            .simulate_click(bounds.center(), Modifiers::default());
        self.cx.run_until_parked();
    }

    fn drawn(&mut self, selector: &'static str) -> bool {
        let _ = self
            .window
            .update(&mut self.cx, |_, window, _| window.refresh());
        self.cx.run_until_parked();
        self.cx.debug_bounds(selector).is_some()
    }

    fn pass_time(&mut self) {
        self.cx.run_until_parked();
        std::thread::sleep(Duration::from_millis(10));
        self.cx.executor().advance_clock(Duration::from_millis(10));
        self.drawn("view-bar");
    }

    fn diff(&mut self) -> Entity<DiffView> {
        self.root(|root, _| root.diff_view())
            .expect("the diff is shown")
    }
}

#[gpui::test]
fn the_diff_tab_of_a_workspace_without_a_task_says_there_is_no_diff(cx: &mut TestAppContext) {
    let mut benterm = open(cx);
    assert!(benterm.drawn("view-tab-diff"));

    benterm.click("view-tab-diff");

    assert_eq!(benterm.root(|root, _| root.main_view()), MainView::Diff);
    assert!(benterm.drawn("diff-view"));
    assert!(!benterm.drawn("tab-bar"), "the panes are hidden");
    let diff = benterm.diff();
    let notice = diff.read_with(&benterm.cx, |diff, _| diff.notice());
    assert!(notice.unwrap().contains("no task"));

    benterm.cx.simulate_keystrokes("escape");
    assert_eq!(
        benterm.root(|root, _| root.main_view()),
        MainView::Terminals
    );
    assert!(benterm.drawn("tab-bar"));
}

#[gpui::test]
fn the_keyboard_opens_the_diff_and_goes_back(cx: &mut TestAppContext) {
    let mut benterm = open(cx);
    benterm.cx.simulate_keystrokes("cmd-shift-g");
    assert_eq!(benterm.root(|root, _| root.main_view()), MainView::Diff);
    benterm.cx.simulate_keystrokes("cmd-shift-g");
    assert_eq!(
        benterm.root(|root, _| root.main_view()),
        MainView::Terminals
    );
}

#[gpui::test]
fn a_long_diff_draws_only_the_rows_on_screen(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    let place = Place::new();
    let text: String = (0..10_000).map(|n| format!("line {n}\n")).collect();
    place.write("long.txt", &text);
    let source = place.source();
    let window = cx.add_window(|_, cx| {
        let mut view = DiffView::new(Theme::default(), Git::default(), cx);
        view.set_shown(true, cx);
        view.set_source(Some(source), cx);
        view
    });
    let mut vcx = VisualTestContext::from_window(window.into(), cx);
    vcx.simulate_resize(size(px(1200.), px(800.)));
    let deadline = Instant::now() + TIMEOUT;
    while window.read_with(&vcx, |view, _| view.rows().len()).unwrap() < 10_001 {
        assert!(Instant::now() < deadline, "timed out waiting for the diff");
        std::thread::sleep(Duration::from_millis(10));
        vcx.run_until_parked();
    }
    let _ = window.update(&mut vcx, |_, window, _| window.refresh());
    vcx.run_until_parked();

    assert!(vcx.debug_bounds("diff-row-0").is_some());
    assert!(vcx.debug_bounds("diff-row-5000").is_none());
    assert!(vcx.debug_bounds("diff-row-10000").is_none());
}

/// Asks the window's socket, while the window goes on.
fn call<M>(benterm: &mut Benterm, socket: &Path, request: M) -> M::Output
where
    M: Method + Send + 'static,
    M::Output: Send + 'static,
{
    let (sender, answer) = std::sync::mpsc::channel();
    let socket = socket.to_owned();
    std::thread::spawn(move || {
        let result = Client::connect(socket)
            .map_err(ClientError::Io)
            .and_then(|mut client| client.call(&request));
        let _ = sender.send(result);
    });
    let deadline = Instant::now() + TIMEOUT;
    loop {
        benterm.pass_time();
        if let Ok(result) = answer.recv_timeout(Duration::from_millis(5)) {
            return result.unwrap();
        }
        assert!(Instant::now() < deadline, "no answer before the timeout");
    }
}

#[gpui::test]
fn the_diff_tab_shows_the_task_of_the_selected_workspace(cx: &mut TestAppContext) {
    let place = Place::new();
    let config = place.root.join("config/config.toml");
    std::fs::create_dir_all(config.parent().unwrap()).unwrap();
    let trees = place.root.join("window-trees");
    std::fs::write(
        &config,
        format!("[worktrees]\ndirectory = {:?}\n", trees.to_str().unwrap()),
    )
    .unwrap();
    let socket = place.root.join("benterm.sock");
    let mut benterm = open_with(cx, Some(socket.clone()), ConfigFeed::watch(&config));
    let first = benterm.root(|root, _| root.selected_workspace());
    let request = CreateTask {
        name: "diff".to_owned(),
        cwd: place.repo.to_str().unwrap().to_owned(),
        ..CreateTask::default()
    };
    let created = call(&mut benterm, &socket, request).task;
    std::fs::write(Path::new(&created.worktree).join("README.md"), "one\ntwo\n").unwrap();
    let workspace = created.workspace.expect("the task has a workspace").get();
    benterm
        .window
        .update(&mut benterm.cx, |root, window, cx| {
            let id = root
                .workspaces()
                .into_iter()
                .find(|id| id.get() == workspace)
                .unwrap();
            root.select_workspace(id, window, cx)
        })
        .unwrap();

    benterm.click("view-tab-diff");
    let diff = benterm.diff();
    let deadline = Instant::now() + TIMEOUT;
    while diff.read_with(&benterm.cx, |diff, _| diff.files().is_empty()) {
        assert!(Instant::now() < deadline, "timed out waiting for the diff");
        benterm.pass_time();
    }
    let rows = diff.read_with(&benterm.cx, |diff, _| marked(&rows_of_selected(diff)));
    assert_eq!(rows, ["@@ -1 +1,2 @@", " one", "+two"]);
    assert!(benterm.drawn("diff-file-count"));
    assert!(benterm.drawn("diff-row-2"));

    // Another workspace, with no task, while the diff is shown.
    benterm
        .window
        .update(&mut benterm.cx, |root, window, cx| {
            root.select_workspace(first, window, cx)
        })
        .unwrap();
    benterm.click("view-tab-diff");
    let diff = benterm.diff();
    let notice = diff.read_with(&benterm.cx, |diff, _| diff.notice());
    assert!(notice.unwrap().contains("no task"));
}

#[gpui::test]
fn the_board_in_place_of_the_diff_stops_its_watching(cx: &mut TestAppContext) {
    let mut benterm = open(cx);
    benterm.click("view-tab-diff");
    let diff = benterm.diff();
    assert!(diff.read_with(&benterm.cx, |diff, _| diff.is_shown()));

    benterm.click("view-tab-progress-board");

    assert!(!diff.read_with(&benterm.cx, |diff, _| diff.is_shown()));
}

#[gpui::test]
fn a_diff_over_the_output_limit_shows_too_large_notice(cx: &mut TestAppContext) {
    let place = Place::new();
    let line = "x".repeat(100) + "\n";
    let body = line.repeat((DIFF_OUTPUT_LIMIT / line.len()) + 1000);
    place.commit("big.txt", &body);
    let view = view(&place, cx);

    wait_for(cx, "the too-large notice", |cx| {
        view.read_with(cx, |view, _| {
            view.notice()
                .is_some_and(|notice| notice.to_lowercase().contains("too large"))
        })
    });

    assert!(paths(&view, cx).is_empty());

    // Shrinking the diff back under the limit recovers cleanly.
    place.write("big.txt", "small\n");
    wait_for(cx, "the recovered diff", |cx| !paths(&view, cx).is_empty());
    assert_eq!(view.read_with(cx, |view, _| view.notice()), None);
}

#[gpui::test]
fn untracked_files_over_the_limit_show_a_note_in_the_diff_view(cx: &mut TestAppContext) {
    let place = Place::new();
    for i in 0..(NEW_FILES_COUNT_LIMIT + 5) {
        place.write(&format!("u{i:04}.txt"), "x\n");
    }
    let view = view(&place, cx);

    wait_for(cx, "the truncated untracked files", |cx| {
        paths(&view, cx).len() == NEW_FILES_COUNT_LIMIT
    });

    let last = view.read_with(cx, |view, _| view.rows().last().cloned());
    let last = last.expect("a note row at the end");
    assert_eq!(last.kind, RowKind::Note);
    assert!(
        last.text.to_lowercase().contains("limit"),
        "expected limit note, got: {}",
        last.text
    );
}

#[gpui::test]
fn choosing_the_seventh_file_from_the_file_picker_jumps_to_it(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    cx.update(bind_keys);
    let place = Place::new();
    for i in 1..=10 {
        place.write(&format!("file-{i:02}.txt"), &format!("line {i}\n"));
    }
    let source = place.source();
    let window = cx.add_window(|_, cx| {
        let mut view = DiffView::new(Theme::default(), Git::default(), cx);
        view.set_shown(true, cx);
        view.set_source(Some(source), cx);
        view
    });
    let mut vcx = VisualTestContext::from_window(window.into(), cx);
    vcx.simulate_resize(size(px(1200.), px(800.)));

    let deadline = Instant::now() + TIMEOUT;
    while window
        .read_with(&vcx, |view, _| view.files().len())
        .unwrap()
        < 10
    {
        assert!(Instant::now() < deadline, "timed out waiting for 10 files");
        std::thread::sleep(Duration::from_millis(10));
        vcx.run_until_parked();
    }
    let _ = window.update(&mut vcx, |_, window, _| window.refresh());
    vcx.run_until_parked();

    assert_eq!(
        window
            .read_with(&vcx, |view, _| view.selected_file().map(|f| f.path.clone()))
            .unwrap()
            .as_deref(),
        Some("file-01.txt")
    );

    let toggle = vcx
        .debug_bounds("diff-file-picker-toggle")
        .expect("diff file picker toggle is drawn");
    vcx.simulate_click(toggle.center(), Modifiers::none());
    vcx.run_until_parked();
    let _ = window.update(&mut vcx, |_, window, _| window.refresh());
    vcx.run_until_parked();

    assert!(
        vcx.debug_bounds("diff-file-picker").is_some(),
        "diff file picker popover should open"
    );
    let seventh = vcx
        .debug_bounds("diff-file-option-6")
        .expect("7th file option is drawn in the picker");
    vcx.simulate_click(seventh.center(), Modifiers::none());
    vcx.run_until_parked();
    let _ = window.update(&mut vcx, |_, window, _| window.refresh());
    vcx.run_until_parked();

    assert_eq!(
        window
            .read_with(&vcx, |view, _| view.selected_file().map(|f| f.path.clone()))
            .unwrap()
            .as_deref(),
        Some("file-07.txt")
    );
    assert!(
        vcx.debug_bounds("diff-file-picker").is_none(),
        "picker should close after choosing a file"
    );
}

#[gpui::test]
fn typing_in_the_file_picker_filters_and_enter_jumps_to_the_match(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    cx.update(bind_keys);
    let place = Place::new();
    for i in 1..=10 {
        place.write(&format!("file-{i:02}.txt"), &format!("line {i}\n"));
    }
    let source = place.source();
    let window = cx.add_window(|_, cx| {
        let mut view = DiffView::new(Theme::default(), Git::default(), cx);
        view.set_shown(true, cx);
        view.set_source(Some(source), cx);
        view
    });
    let mut vcx = VisualTestContext::from_window(window.into(), cx);
    vcx.simulate_resize(size(px(1200.), px(800.)));

    let deadline = Instant::now() + TIMEOUT;
    while window
        .read_with(&vcx, |view, _| view.files().len())
        .unwrap()
        < 10
    {
        assert!(Instant::now() < deadline, "timed out waiting for 10 files");
        std::thread::sleep(Duration::from_millis(10));
        vcx.run_until_parked();
    }
    let _ = window.update(&mut vcx, |_, window, _| window.refresh());
    vcx.run_until_parked();

    let toggle = vcx
        .debug_bounds("diff-file-picker-toggle")
        .expect("diff file picker toggle is drawn");
    vcx.simulate_click(toggle.center(), Modifiers::none());
    vcx.run_until_parked();
    vcx.simulate_input("08");
    vcx.run_until_parked();
    vcx.simulate_keystrokes("enter");
    vcx.run_until_parked();
    let _ = window.update(&mut vcx, |_, window, _| window.refresh());
    vcx.run_until_parked();

    assert_eq!(
        window
            .read_with(&vcx, |view, _| view.selected_file().map(|f| f.path.clone()))
            .unwrap()
            .as_deref(),
        Some("file-08.txt")
    );
    assert!(vcx.debug_bounds("diff-file-picker").is_none());
}

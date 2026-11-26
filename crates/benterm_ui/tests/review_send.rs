//! Comments on the lines of a task's diff, written with the mouse and the
//! keyboard and sent to the task's agent: where they are kept, what the
//! agent's pane is given, and what happens when the agent is not waiting
//! for input. The repository, the programs and the window are real; the
//! agent is a made-up Claude Code that shows the screens of the agent
//! tests and writes down every line it reads.

#[path = "../../benterm_agent/tests/scenes/mod.rs"]
#[allow(dead_code)]
mod scenes;

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;
use std::time::{Duration, Instant};

use benterm_agent::{AgentState, WaitReason};
use benterm_ipc::{
    Client, ClientError, CreateTask, Method, PaneId as ApiPaneId, ReadOutput, SplitDirection,
    SplitPane,
};
use benterm_task::{Git, NewTask, Task, TaskStore, create};
use benterm_terminal::PtyConfig;
use benterm_ui::diff_view::{DiffSource, DiffView, RowKind};
use benterm_ui::review::{Readiness, Side};
use benterm_ui::settings::ConfigFeed;
use benterm_ui::theme::Theme;
use benterm_ui::{BentermWindow, StartedTerminal, bind_keys, ipc};
use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext, WindowHandle, px, size};

const TIMEOUT: Duration = Duration::from_secs(10);

/// What the other panes run: prints `> ` before every line it reads and
/// does not echo, so a line sent there shows on its screen.
const ANSWERS: &str = r#"stty -echo; echo ready; while read line; do echo "> $line"; done"#;

const CODER: &str = r#"
name = "coder"
description = "書く"
agent = "claude"
prompt = "直す"
"#;

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

fn numbered(lines: u32) -> String {
    (1..=lines).map(|n| format!("line {n}\n")).collect()
}

/// A repository whose `src/main.rs` and `README.md` have 20 lines each.
fn repository(root: &Path) -> PathBuf {
    let repo = root.join("benterm");
    std::fs::create_dir_all(repo.join("src")).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("src/main.rs"), numbered(20)).unwrap();
    std::fs::write(repo.join("README.md"), numbered(20)).unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "one"]);
    repo
}

fn temporary() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap();
    let root = dir.path().canonicalize().unwrap();
    (dir, root)
}

/// Every line of `src/main.rs` and lines 3 to 5 of `README.md` changed,
/// and line 7 of `README.md` removed.
fn change(worktree: &Path) {
    let main: String = (1..=20).map(|n| format!("changed {n}\n")).collect();
    std::fs::write(worktree.join("src/main.rs"), main).unwrap();
    let readme: String = (1..=20)
        .filter(|n| *n != 7)
        .map(|n| match n {
            3..=5 => format!("new {n}\n"),
            _ => format!("line {n}\n"),
        })
        .collect();
    std::fs::write(worktree.join("README.md"), readme).unwrap();
}

/// The index of the row that is line `line` of the worktree's file, or
/// with `Side::Old` the removed row that was that line.
fn row_of(view: &DiffView, side: Side, line: u32) -> usize {
    // The diff shows every file, so the line is looked for in the one
    // that was last gone to.
    let path = view.selected_file().expect("a file").path.clone();
    view.rows()
        .iter()
        .position(|row| {
            row.path.as_ref() == path
                && match side {
                    Side::New => row.line_in_worktree().map(|l| l.1) == Some(line),
                    Side::Old => row.kind == RowKind::Removed && row.old == Some(line),
                }
        })
        .unwrap_or_else(|| panic!("no row for line {line}"))
}

fn file_index(view: &DiffView, path: &str) -> usize {
    view.files()
        .iter()
        .position(|file| file.path == path)
        .unwrap_or_else(|| panic!("{path} is not changed"))
}

/// A diff view of its own in a window, on a task made off a repository.
struct Alone {
    _dir: tempfile::TempDir,
    task: Task,
    window: WindowHandle<DiffView>,
    cx: VisualTestContext,
}

impl Alone {
    fn new(cx: &mut TestAppContext) -> Self {
        cx.executor().allow_parking();
        cx.update(bind_keys);
        let (dir, root) = temporary();
        let repo = repository(&root);
        let store = TaskStore::open(Some(&root.join("state/tasks.json")));
        let new = NewTask {
            directory: repo,
            name: "review".to_owned(),
            branch: None,
            role: None,
            root: root.join("trees"),
        };
        let task = create(&Git::default(), &store, new).unwrap();
        change(&task.worktree);
        let source = DiffSource::of(&task);
        let window = cx.add_window(|window, cx| {
            let mut view = DiffView::new(Theme::default(), Git::default(), cx);
            view.set_shown(true, cx);
            view.set_source(Some(source), cx);
            window.focus(view.focus_handle(), cx);
            view
        });
        let cx = VisualTestContext::from_window(window.into(), cx);
        cx.simulate_resize(size(px(1200.), px(800.)));
        let mut alone = Self {
            _dir: dir,
            task,
            window,
            cx,
        };
        alone.wait_until("the diff", |view| view.files().len() == 2);
        alone
    }

    fn read<T>(&mut self, read: impl FnOnce(&DiffView) -> T) -> T {
        self.window
            .read_with(&self.cx, |view, _| read(view))
            .unwrap()
    }

    fn pass_time(&mut self) {
        self.cx.run_until_parked();
        std::thread::sleep(Duration::from_millis(10));
        self.cx.executor().advance_clock(Duration::from_millis(10));
        let _ = self
            .window
            .update(&mut self.cx, |_, window, _| window.refresh());
        self.cx.run_until_parked();
    }

    fn wait_until(&mut self, what: &str, mut done: impl FnMut(&DiffView) -> bool) {
        let deadline = Instant::now() + TIMEOUT;
        while !self.read(&mut done) {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            self.pass_time();
        }
    }

    fn show(&mut self, path: &str) {
        let _ = self.window.update(&mut self.cx, |view, _, cx| {
            let index = file_index(view, path);
            view.select_file(index, cx);
        });
        self.pass_time();
    }

    fn click(&mut self, selector: &str, modifiers: Modifiers) {
        self.pass_time();
        let bounds = self
            .cx
            .debug_bounds(Box::leak(selector.to_owned().into_boxed_str()))
            .unwrap_or_else(|| panic!("{selector} is not drawn"));
        self.cx.simulate_click(bounds.center(), modifiers);
        self.pass_time();
    }

    fn drawn(&mut self, selector: &str) -> bool {
        self.pass_time();
        self.cx
            .debug_bounds(Box::leak(selector.to_owned().into_boxed_str()))
            .is_some()
    }

    fn click_line(&mut self, side: Side, line: u32, modifiers: Modifiers) {
        let index = self.read(|view| row_of(view, side, line));
        self.click_row(index, modifiers);
    }

    /// Clicks a row of the diff, brought into view first: the column
    /// holds every file and draws only what is on screen.
    fn click_row(&mut self, index: usize, modifiers: Modifiers) {
        let _ = self
            .window
            .update(&mut self.cx, |view, _, cx| view.reveal_row(index, cx));
        self.click(&format!("diff-row-{index}"), modifiers);
    }

    fn type_comment(&mut self, text: &str) {
        self.cx.simulate_input(text);
        self.cx.simulate_keystrokes("enter");
        self.pass_time();
    }

    fn comments(&mut self) -> Vec<(String, String, bool)> {
        self.read(|view| {
            view.review()
                .map(|review| {
                    review
                        .comments()
                        .iter()
                        .map(|c| (c.lines.label(), c.text.clone(), c.sent))
                        .collect()
                })
                .unwrap_or_default()
        })
    }
}

#[gpui::test]
fn clicking_a_line_writes_a_comment_on_it(cx: &mut TestAppContext) {
    let mut alone = Alone::new(cx);
    alone.show("src/main.rs");

    alone.click_line(Side::New, 12, Modifiers::default());
    assert!(alone.drawn("name-editor"), "a field for the comment opens");
    alone.type_comment("名前を直す");

    assert_eq!(
        alone.comments(),
        [("src/main.rs:12".to_owned(), "名前を直す".to_owned(), false)]
    );
    assert!(!alone.drawn("name-editor"));
    let id = alone.read(|view| view.review().unwrap().comments()[0].id);
    assert!(
        alone.drawn(&format!("diff-comment-{id}")),
        "the comment stays on the diff"
    );
}

/// The field and what says its lines are in the box of the comment, at
/// its left, so that a long comment has the width of the diff.
#[gpui::test]
fn the_field_for_a_comment_starts_at_the_left_of_its_box(cx: &mut TestAppContext) {
    let mut alone = Alone::new(cx);
    alone.show("src/main.rs");

    alone.click_line(Side::New, 12, Modifiers::default());

    assert!(alone.drawn("name-editor"));
    let row = alone.cx.debug_bounds("diff-comment-editor").unwrap();
    let field = alone.cx.debug_bounds("name-editor").unwrap();
    // The numbers and the mark of the rows, the label `src/main.rs:12`
    // and the spaces about it are well within 300 points.
    assert!(
        field.left() - row.left() < px(300.),
        "the field starts {:?} from the left of the row",
        field.left() - row.left()
    );
    assert!(
        field.size.width > row.size.width * 0.6,
        "the field has {:?} of the row's {:?}",
        field.size.width,
        row.size.width
    );
}

/// A long label, as of a removed line in a narrow window, gives way to
/// the field, which is what is being typed into.
#[gpui::test]
fn a_long_label_leaves_the_field_for_a_comment_its_room(cx: &mut TestAppContext) {
    let mut alone = Alone::new(cx);
    alone.cx.simulate_resize(size(px(560.), px(800.)));
    alone.show("src/main.rs");
    let removed = alone.read(|view| {
        view.rows()
            .iter()
            .position(|row| row.kind == RowKind::Removed)
            .expect("a removed line")
    });

    alone.click_row(removed, Modifiers::default());

    assert!(alone.drawn("name-editor"));
    let row = alone.cx.debug_bounds("diff-comment-editor").unwrap();
    let field = alone.cx.debug_bounds("name-editor").unwrap();
    assert!(
        field.size.width >= px(100.),
        "the field has {:?} of the row's {:?}",
        field.size.width,
        row.size.width
    );
}

/// The list of the installed fonts takes the system a long time to make,
/// so a key typed into a comment must not ask for it.
#[gpui::test]
fn typing_a_comment_does_not_look_the_fonts_up_again(cx: &mut TestAppContext) {
    let mut alone = Alone::new(cx);
    alone.show("src/main.rs");
    alone.click_line(Side::New, 12, Modifiers::default());
    assert!(alone.drawn("name-editor"));
    let before = benterm_ui::theme::font_lookups();

    for key in ["a", "b", "c"] {
        alone.cx.simulate_input(key);
        alone.pass_time();
    }

    assert_eq!(benterm_ui::theme::font_lookups(), before);
}

/// The fonts are looked up again when the theme names other families,
/// and only then.
#[gpui::test]
fn the_fonts_are_looked_up_when_the_theme_names_other_families(cx: &mut TestAppContext) {
    let mut alone = Alone::new(cx);
    let before = benterm_ui::theme::font_lookups();

    let mut larger = Theme::default();
    larger.terminal.font_size += 2.;
    let _ = alone
        .window
        .update(&mut alone.cx, |view, _, cx| view.set_theme(&larger, cx));
    alone.pass_time();
    assert_eq!(
        benterm_ui::theme::font_lookups(),
        before,
        "a new size is no reason to ask"
    );

    let mut other = larger.clone();
    other.terminal.font_families = vec!["Menlo".to_owned()];
    let _ = alone
        .window
        .update(&mut alone.cx, |view, _, cx| view.set_theme(&other, cx));
    alone.pass_time();
    assert_eq!(benterm_ui::theme::font_lookups(), before + 1);
}

/// Cmd-Enter is the diff's key for sending, also while a comment is
/// being typed: the field does not take it for its own.
#[gpui::test]
fn cmd_enter_in_the_field_of_a_comment_is_still_the_diff_s(cx: &mut TestAppContext) {
    let mut alone = Alone::new(cx);
    alone.show("src/main.rs");
    alone.click_line(Side::New, 12, Modifiers::default());
    assert!(alone.drawn("name-editor"));
    alone.cx.simulate_input("draft");

    alone.cx.simulate_keystrokes("cmd-enter");
    alone.pass_time();

    assert!(
        alone.comments().is_empty(),
        "the draft is not kept as a comment by the key that sends"
    );
    assert!(alone.drawn("name-editor"), "the field is still open");
}

/// The diff is one column of every changed file, as `git diff` prints
/// them: a line that names each file, then its changes. There is no list
/// of files beside it.
#[gpui::test]
fn every_changed_file_is_in_the_one_column(cx: &mut TestAppContext) {
    let mut alone = Alone::new(cx);
    let (headers, paths) = alone.read(|view| {
        let headers: Vec<String> = view
            .rows()
            .iter()
            .filter(|row| row.kind == RowKind::File)
            .map(|row| row.text.to_string())
            .collect();
        let mut paths: Vec<String> = view.rows().iter().map(|row| row.path.to_string()).collect();
        paths.dedup();
        (headers, paths)
    });
    assert_eq!(
        headers,
        [
            "diff --git a/README.md b/README.md",
            "diff --git a/src/main.rs b/src/main.rs"
        ]
    );
    assert_eq!(paths, ["README.md", "src/main.rs"]);
    assert!(!alone.drawn("diff-file-0"), "there is no list of files");
    // The line that names a file takes no comment.
    let header = alone.read(|view| {
        view.rows()
            .iter()
            .position(|row| row.kind == RowKind::File)
            .unwrap()
    });
    alone.click_row(header, Modifiers::default());
    assert!(!alone.drawn("name-editor"));
}

/// A comment is on lines of one file: a selection does not reach from
/// one file into the next.
#[gpui::test]
fn a_selection_stays_in_one_file(cx: &mut TestAppContext) {
    let mut alone = Alone::new(cx);
    alone.show("README.md");
    let first = alone.read(|view| row_of(view, Side::New, 2));
    alone.show("src/main.rs");
    let second = alone.read(|view| row_of(view, Side::New, 12));

    alone.click_row(first, Modifiers::default());
    alone.click_row(second, Modifiers::shift());

    let lines = alone.read(|view| view.selected_lines()).expect("lines");
    assert_eq!(lines.path, "src/main.rs");
    assert_eq!((lines.start, lines.end), (12, 12));
}

/// After going to a file, the keys that select lines start in it, and
/// a comment being written is not moved to another file.
#[gpui::test]
fn the_lines_are_gone_through_from_the_file_gone_to(cx: &mut TestAppContext) {
    let mut alone = Alone::new(cx);
    alone.show("src/main.rs");
    alone.cx.simulate_keystrokes("alt-down");
    alone.pass_time();
    let lines = alone.read(|view| view.selected_lines()).expect("lines");
    assert_eq!(lines.path, "src/main.rs");

    // A draft about that file stays there when a row of another file
    // is pressed.
    alone.cx.simulate_keystrokes("enter");
    alone.cx.simulate_input("about main");
    alone.show("README.md");
    alone.click_line(Side::New, 2, Modifiers::default());
    alone.cx.simulate_keystrokes("enter");
    alone.pass_time();
    let comments = alone.comments();
    assert_eq!(comments.len(), 1);
    assert!(comments[0].0.starts_with("src/main.rs:"), "{comments:?}");
    assert_eq!(comments[0].1, "about main");
}

#[gpui::test]
fn a_shift_click_makes_a_range_for_one_comment(cx: &mut TestAppContext) {
    let mut alone = Alone::new(cx);
    alone.show("README.md");

    alone.click_line(Side::New, 3, Modifiers::default());
    alone.click_line(Side::New, 5, Modifiers::shift());
    alone.type_comment("例を足す");

    assert_eq!(
        alone.comments(),
        [("README.md:3-5".to_owned(), "例を足す".to_owned(), false)]
    );
}

#[gpui::test]
fn a_comment_on_a_removed_line_counts_in_the_old_file(cx: &mut TestAppContext) {
    let mut alone = Alone::new(cx);
    alone.show("README.md");

    alone.click_line(Side::Old, 7, Modifiers::default());
    alone.type_comment("消さないで");

    assert_eq!(
        alone.comments(),
        [(
            "README.md:7 (removed line, old numbering)".to_owned(),
            "消さないで".to_owned(),
            false
        )]
    );
}

#[gpui::test]
fn a_comment_can_be_changed_and_taken_away_before_it_is_sent(cx: &mut TestAppContext) {
    let mut alone = Alone::new(cx);
    alone.show("src/main.rs");
    alone.click_line(Side::New, 2, Modifiers::default());
    alone.type_comment("frist");
    let id = alone.read(|view| view.review().unwrap().comments()[0].id);

    alone.click(&format!("diff-comment-edit-{id}"), Modifiers::default());
    assert!(alone.drawn("name-editor"));
    // The field starts with the text selected, so typing replaces it.
    alone.type_comment("first");
    assert_eq!(
        alone.comments(),
        [("src/main.rs:2".to_owned(), "first".to_owned(), false)]
    );

    alone.click(&format!("diff-comment-delete-{id}"), Modifiers::default());
    assert_eq!(alone.comments(), []);
    assert!(!alone.drawn(&format!("diff-comment-{id}")));
}

#[gpui::test]
fn escape_gives_up_a_comment_being_written(cx: &mut TestAppContext) {
    let mut alone = Alone::new(cx);
    alone.show("src/main.rs");
    alone.click_line(Side::New, 2, Modifiers::default());
    alone.cx.simulate_input("never mind");

    alone.cx.simulate_keystrokes("escape");
    alone.pass_time();

    assert!(!alone.drawn("name-editor"));
    assert_eq!(alone.comments(), []);
}

#[gpui::test]
fn the_keyboard_selects_lines_and_writes_a_comment(cx: &mut TestAppContext) {
    let mut alone = Alone::new(cx);
    alone.show("README.md");
    // Down from the first line, past the removed lines 3 to 5, to the
    // new line 3, and on to line 5.
    let label = |alone: &mut Alone| alone.read(|view| view.selected_lines().map(|l| l.label()));
    for _ in 0..20 {
        alone.cx.simulate_keystrokes("alt-down");
        if label(&mut alone).as_deref() == Some("README.md:3") {
            break;
        }
    }
    assert_eq!(label(&mut alone).as_deref(), Some("README.md:3"));
    alone
        .cx
        .simulate_keystrokes("alt-shift-down alt-shift-down");
    alone.pass_time();
    assert_eq!(
        alone.read(|view| view.selected_lines().map(|lines| lines.label())),
        Some("README.md:3-5".to_owned())
    );

    alone.cx.simulate_keystrokes("enter");
    alone.type_comment("例を足す");

    assert_eq!(
        alone.comments(),
        [("README.md:3-5".to_owned(), "例を足す".to_owned(), false)]
    );
}

#[gpui::test]
fn comments_stay_with_their_task_when_another_is_shown(cx: &mut TestAppContext) {
    let mut alone = Alone::new(cx);
    alone.show("src/main.rs");
    alone.click_line(Side::New, 2, Modifiers::default());
    alone.type_comment("keep me");

    let _ = alone.window.update(&mut alone.cx, |view, _, cx| {
        view.set_source(None, cx);
    });
    assert!(alone.read(|view| view.review().is_none()));
    let source = DiffSource::of(&alone.task);
    let _ = alone.window.update(&mut alone.cx, |view, _, cx| {
        view.set_source(Some(source), cx);
    });

    assert_eq!(alone.comments().len(), 1);
}

// The window, with a task whose agent is a made-up Claude Code.

/// The last frame of every made-up agent scene as a file, and the start
/// screen of each agent, which shows its banner.
fn write_frames(dir: &Path) {
    let write = |name: &str, frame: &[String]| {
        let mut bytes = b"\x1b[H\x1b[2J".to_vec();
        bytes.extend_from_slice(frame.join("\r\n").as_bytes());
        std::fs::write(dir.join(name), bytes).unwrap();
    };
    for scene in scenes::all() {
        let name = scene.name.replace('/', "-");
        write(&name, scene.frames.last().unwrap());
        if name.ends_with("-idle") {
            write(&name.replace("-idle", "-start"), &scene.frames[0]);
        }
    }
}

struct Benterm {
    _dir: tempfile::TempDir,
    root: PathBuf,
    repo: PathBuf,
    socket: PathBuf,
    window: WindowHandle<BentermWindow>,
    cx: VisualTestContext,
}

impl Benterm {
    /// A window whose tasks of the role `coder` run a made-up Claude
    /// Code that shows the scene `scene` and writes each line it reads
    /// to `received`.
    fn open(cx: &mut TestAppContext, scene: &str) -> Self {
        cx.executor().allow_parking();
        cx.update(bind_keys);
        let (dir, root) = temporary();
        let repo = repository(&root);
        let frames = root.join("frames");
        std::fs::create_dir(&frames).unwrap();
        write_frames(&frames);
        let config = root.join("config");
        std::fs::create_dir_all(config.join("roles")).unwrap();
        std::fs::write(config.join("roles/coder.toml"), CODER).unwrap();
        let trees = root.join("trees");
        std::fs::write(
            config.join("config.toml"),
            format!("[worktrees]\ndirectory = {:?}\n", trees.to_str().unwrap()),
        )
        .unwrap();
        std::fs::create_dir(root.join("fake")).unwrap();
        let claude = root.join("fake/claude");
        std::fs::write(
            &claude,
            format!(
                "#!/bin/sh\ncd '{frames}'; cat claude-start; sleep 0.3; cat claude-{scene}\n\
                 stty -echo\nwhile read line; do echo \"$line\" >> '{received}'; done\n",
                frames = frames.display(),
                received = root.join("received").display(),
            ),
        )
        .unwrap();
        std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut pty = PtyConfig::command("/bin/sh", ["-c", ANSWERS]);
        pty.cwd = Some(repo.clone());
        pty.env = vec![(
            "PATH".into(),
            format!("{}/fake:/usr/bin:/bin", root.display()).into(),
        )];
        let socket = root.join("benterm.sock");
        let start: ipc::StartTerminal = Rc::new(StartedTerminal::spawn);
        let feed = ConfigFeed::watch(&config.join("config.toml"));
        let session = root.join("state/session.json");
        let window = cx
            .update(|cx| {
                ipc::open_session_window(pty, start, Some(socket.clone()), Some(session), feed, cx)
            })
            .unwrap();
        let cx = VisualTestContext::from_window(window.into(), cx);
        cx.simulate_resize(size(px(1200.), px(800.)));
        cx.run_until_parked();
        Self {
            _dir: dir,
            root,
            repo,
            socket,
            window,
            cx,
        }
    }

    fn pass_time(&mut self) {
        self.cx.run_until_parked();
        self.cx.update(|window, cx| window.draw(cx).clear(cx));
        std::thread::sleep(Duration::from_millis(20));
        self.cx.executor().advance_clock(Duration::from_millis(20));
        let _ = self
            .window
            .update(&mut self.cx, |_, window, _| window.refresh());
        self.cx.run_until_parked();
    }

    fn wait_until(&mut self, what: &str, mut done: impl FnMut(&mut Self) -> bool) {
        let deadline = Instant::now() + TIMEOUT;
        while !done(self) {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            self.pass_time();
        }
    }

    fn call<M>(&mut self, request: M) -> M::Output
    where
        M: Method + Send + 'static,
        M::Output: Send + 'static,
    {
        let (sender, answer) = std::sync::mpsc::channel();
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
                return result.unwrap();
            }
            assert!(Instant::now() < deadline, "no answer before the timeout");
        }
    }

    fn root<T>(&mut self, read: impl FnOnce(&BentermWindow, &gpui::App) -> T) -> T {
        self.window.read_with(&self.cx, read).unwrap()
    }

    fn screen(&mut self, pane: u64) -> String {
        let pane = ApiPaneId::new(pane);
        self.call(ReadOutput { pane, lines: None }).text
    }

    fn received(&self) -> String {
        std::fs::read_to_string(self.root.join("received")).unwrap_or_default()
    }

    fn state_of(&mut self, pane: u64) -> AgentState {
        self.window
            .update(&mut self.cx, |root, _, cx| {
                let row = root
                    .agents(cx)
                    .into_iter()
                    .find(|row| row.pane.get() == pane)?;
                root.terminal_with_id(row.pane, cx)
                    .map(|view| view.read(cx).agent_status().state)
            })
            .unwrap()
            .unwrap_or(AgentState::Unknown)
    }

    fn diff(&mut self) -> Entity<DiffView> {
        self.root(|root, _| root.diff_view())
            .expect("the diff is shown")
    }

    fn click(&mut self, selector: &str, modifiers: Modifiers) {
        self.pass_time();
        let bounds = self
            .cx
            .debug_bounds(Box::leak(selector.to_owned().into_boxed_str()))
            .unwrap_or_else(|| panic!("{selector} is not drawn"));
        self.cx.simulate_click(bounds.center(), modifiers);
        self.pass_time();
    }

    fn click_line(&mut self, side: Side, line: u32, modifiers: Modifiers) {
        let diff = self.diff();
        let index = diff.read_with(&self.cx, |view, _| row_of(view, side, line));
        // Brought into view first: the column draws what is on screen.
        diff.update(&mut self.cx, |view, cx| view.reveal_row(index, cx));
        self.click(&format!("diff-row-{index}"), modifiers);
    }

    fn comment(&mut self, text: &str) {
        self.cx.simulate_input(text);
        self.cx.simulate_keystrokes("enter");
        self.pass_time();
    }

    fn show(&mut self, path: &str) {
        let diff = self.diff();
        diff.update(&mut self.cx, |view, cx| {
            let index = file_index(view, path);
            view.select_file(index, cx);
        });
        self.pass_time();
    }

    /// Makes a task of the role `coder`, splits its agent's pane, and
    /// opens the diff once the agent shows its scene and the diff is
    /// read. Returns the task, the agent's pane, and the other pane of
    /// the task's workspace.
    fn task(&mut self, settled: AgentState) -> (u64, u64, u64) {
        let created = self.call(CreateTask {
            name: "review".to_owned(),
            cwd: self.repo.to_str().unwrap().to_owned(),
            role: Some("coder".into()),
            ..CreateTask::default()
        });
        let agent = created.pane.get();
        let split = self.call(SplitPane {
            pane: ApiPaneId::new(agent),
            direction: SplitDirection::Right,
        });
        let other = split.pane.get();
        self.wait_until("the agent's scene", |benterm| {
            benterm.state_of(agent) == settled
        });
        self.wait_until("the other pane", |benterm| {
            benterm.screen(other).contains("ready")
        });
        change(Path::new(&created.task.worktree));
        let workspace = created
            .task
            .workspace
            .expect("the task has a workspace")
            .get();
        self.window
            .update(&mut self.cx, |root, window, cx| {
                let id = root
                    .workspaces()
                    .into_iter()
                    .find(|id| id.get() == workspace)
                    .unwrap();
                root.select_workspace(id, window, cx)
            })
            .unwrap();
        self.pass_time();
        self.cx.simulate_keystrokes("cmd-shift-g");
        self.pass_time();
        self.wait_until("the diff", |benterm| {
            let diff = benterm.diff();
            diff.read_with(&benterm.cx, |view, _| view.files().len() == 2)
        });
        (created.task.id, agent, other)
    }
}

#[gpui::test]
fn the_comments_go_to_the_agent_of_the_task_and_nowhere_else(cx: &mut TestAppContext) {
    let mut benterm = Benterm::open(cx, "done");
    let first = benterm.root(|root, cx| root.grid(cx).read(cx).focused().get());
    let (task, agent, other) = benterm.task(AgentState::Done);
    benterm.show("src/main.rs");
    benterm.click_line(Side::New, 12, Modifiers::default());
    benterm.comment("名前を直す");
    benterm.show("README.md");
    benterm.click_line(Side::New, 3, Modifiers::default());
    benterm.click_line(Side::New, 5, Modifiers::shift());
    benterm.comment("例を足す");

    benterm.click("diff-send", Modifiers::default());

    benterm.wait_until("the comments", |benterm| {
        benterm.received().lines().count() >= 2
    });
    assert_eq!(
        benterm.received(),
        "src/main.rs:12 名前を直す\nREADME.md:3-5 例を足す\n"
    );
    for pane in [first, other] {
        let screen = benterm.screen(pane);
        assert!(!screen.contains("名前を直す"), "pane {pane}: {screen}");
    }
    let diff = benterm.diff();
    let sent = diff.read_with(&benterm.cx, |view, _| {
        view.review().unwrap().comments().iter().all(|c| c.sent)
    });
    assert!(sent, "the comments are marked sent");
    let records = benterm.root(|root, _| root.review_sends().to_vec());
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].task, task);
    assert_eq!(records[0].pane.get(), agent);
    assert_eq!(records[0].comments, 2);
}

#[gpui::test]
fn sending_again_sends_only_the_new_comments(cx: &mut TestAppContext) {
    let mut benterm = Benterm::open(cx, "done");
    benterm.task(AgentState::Done);
    benterm.show("src/main.rs");
    benterm.click_line(Side::New, 1, Modifiers::default());
    benterm.comment("one");
    // The key does what the button does.
    benterm.cx.simulate_keystrokes("cmd-enter");
    benterm.wait_until("the first comment", |benterm| {
        !benterm.received().is_empty()
    });

    benterm.click_line(Side::New, 2, Modifiers::default());
    benterm.comment("two");
    benterm.cx.simulate_keystrokes("cmd-enter");

    benterm.wait_until("the second comment", |benterm| {
        benterm.received().lines().count() >= 2
    });
    benterm.pass_time();
    assert_eq!(benterm.received(), "src/main.rs:1 one\nsrc/main.rs:2 two\n");
    assert_eq!(benterm.root(|root, _| root.review_sends().len()), 2);
}

#[gpui::test]
fn an_agent_at_work_is_asked_about_before_it_is_sent_to(cx: &mut TestAppContext) {
    let mut benterm = Benterm::open(cx, "working");
    benterm.task(AgentState::Working);
    benterm.show("src/main.rs");
    benterm.click_line(Side::New, 1, Modifiers::default());
    benterm.comment("one");

    benterm.click("diff-send", Modifiers::default());

    let diff = benterm.diff();
    let asking = diff.read_with(&benterm.cx, |view, _| view.asking().cloned());
    assert!(matches!(asking, Some(Readiness::Confirm(_))), "{asking:?}");
    for _ in 0..10 {
        benterm.pass_time();
    }
    assert_eq!(benterm.received(), "", "nothing is sent before the answer");

    benterm.click("diff-ask-send", Modifiers::default());

    benterm.wait_until("the comment", |benterm| !benterm.received().is_empty());
    assert_eq!(benterm.received(), "src/main.rs:1 one\n");
    let asking = diff.read_with(&benterm.cx, |view, _| view.asking().cloned());
    assert_eq!(asking, None);
}

#[gpui::test]
fn an_agent_asking_for_permission_is_not_sent_to(cx: &mut TestAppContext) {
    not_sent_at(cx, "permission", WaitReason::Permission);
}

#[gpui::test]
fn an_agent_at_a_question_of_choices_is_not_sent_to(cx: &mut TestAppContext) {
    not_sent_at(cx, "question", WaitReason::Choice);
}

/// With the agent at the screen of `scene`, which waits for `reason`,
/// Enter would answer the question: the comments stay unsent.
fn not_sent_at(cx: &mut TestAppContext, scene: &str, reason: WaitReason) {
    let mut benterm = Benterm::open(cx, scene);
    benterm.task(AgentState::Waiting(reason));
    benterm.show("src/main.rs");
    benterm.click_line(Side::New, 1, Modifiers::default());
    benterm.comment("one");

    benterm.click("diff-send", Modifiers::default());

    let diff = benterm.diff();
    let asking = diff.read_with(&benterm.cx, |view, _| view.asking().cloned());
    assert!(matches!(asking, Some(Readiness::Refuse(_))), "{asking:?}");
    assert!(
        !benterm.cx.debug_bounds("diff-ask-send").is_some(),
        "there is no sending anyway"
    );
    for _ in 0..10 {
        benterm.pass_time();
    }
    assert_eq!(benterm.received(), "");
    let unsent = diff.read_with(&benterm.cx, |view, _| {
        view.review().unwrap().comments().iter().all(|c| !c.sent)
    });
    assert!(unsent);
    assert!(benterm.root(|root, _| root.review_sends().is_empty()));
}

//! The file browser beside the panes: it shows the folder of the focused
//! pane, follows it, opens folders and files with the keys and the mouse,
//! and writes nothing.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime};

use benterm_terminal::PtyConfig;
use benterm_ui::file_preview::{Content, Preview};
use benterm_ui::file_tree::FolderEntry;
use benterm_ui::settings::ConfigFeed;
use benterm_ui::theme::Theme;
use benterm_ui::{
    BentermWindow, FileBrowser, FileReader, FsReader, TerminalView, bind_keys, open_terminal_window,
};
use gpui::{AppContext, Entity, Modifiers, TestAppContext, VisualTestContext, WindowHandle};

/// Opens a window whose panes run `script`, the first one in `root`.
fn open(
    root: &Path,
    script: &str,
    cx: &mut TestAppContext,
) -> (WindowHandle<BentermWindow>, VisualTestContext) {
    cx.executor().allow_parking();
    cx.update(bind_keys);
    let pty = PtyConfig {
        cwd: Some(root.to_owned()),
        ..PtyConfig::command("/bin/sh", ["-c", script])
    };
    let window = cx
        .update(|cx| open_terminal_window(pty, ConfigFeed::defaults(), cx))
        .unwrap();
    let vcx = VisualTestContext::from_window(window.into(), cx);
    vcx.run_until_parked();
    (window, vcx)
}

/// A folder, with its symbolic links resolved as the kernel reports
/// directories, holding `notes.md`, `big.log`, `image.bin`, `src/main.rs`,
/// `a/a/` and a `.git` folder with files in it.
fn sample() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    fs::write(root.join("notes.md"), "# Notes\n\n- one\n- two\n").unwrap();
    fs::write(root.join("big.log"), "line\n".repeat(40_000)).unwrap();
    fs::write(root.join("image.bin"), [0u8, 1, 2, 3, 255]).unwrap();
    fs::create_dir(root.join("src")).unwrap();
    fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();
    fs::create_dir_all(root.join("a/a")).unwrap();
    fs::create_dir_all(root.join(".git/refs/heads")).unwrap();
    fs::write(root.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
    fs::write(root.join(".git/index"), [0u8; 64]).unwrap();
    fs::write(root.join(".git/refs/heads/main"), "0123abcd\n").unwrap();
    (dir, root)
}

fn browser(window: WindowHandle<BentermWindow>, cx: &mut VisualTestContext) -> Entity<FileBrowser> {
    window
        .read_with(cx, |root, _| root.file_browser())
        .unwrap()
        .expect("the file browser is open")
}

fn focused(
    window: WindowHandle<BentermWindow>,
    cx: &mut VisualTestContext,
) -> Entity<TerminalView> {
    window.read_with(cx, |root, cx| root.terminal(cx)).unwrap()
}

/// Lets the app run and draw until `done` holds for the window. The
/// programs run in real time on other threads, hence the polling.
fn wait_until(
    window: WindowHandle<BentermWindow>,
    cx: &mut VisualTestContext,
    what: &str,
    done: impl Fn(&BentermWindow, &gpui::App) -> bool,
) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        cx.run_until_parked();
        let _ = window.update(cx, |_, window, _| window.refresh());
        cx.run_until_parked();
        if window.read_with(cx, |root, cx| done(root, cx)).unwrap() {
            return;
        }
        assert!(Instant::now() < deadline, "timed out waiting until {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn shown_root(root: &BentermWindow, cx: &gpui::App) -> Option<PathBuf> {
    root.file_browser()
        .and_then(|browser| browser.read(cx).root().map(Path::to_owned))
}

fn shows(view: &TerminalView, text: &str) -> bool {
    view.screen()
        .rows
        .iter()
        .any(|row| row.text().contains(text))
}

fn wait_for_text(window: WindowHandle<BentermWindow>, cx: &mut VisualTestContext, text: &str) {
    wait_until(window, cx, &format!("{text} is shown"), |root, cx| {
        shows(root.terminal(cx).read(cx), text)
    });
}

fn names(browser: &Entity<FileBrowser>, cx: &VisualTestContext) -> Vec<String> {
    browser.read_with(cx, |browser, _| {
        browser.rows().into_iter().map(|row| row.name).collect()
    })
}

#[gpui::test]
fn the_key_opens_the_tree_of_the_focused_pane_and_closes_it(cx: &mut TestAppContext) {
    let (_dir, root) = sample();
    let (window, mut cx) = open(&root, "echo ready; sleep 100", cx);
    wait_for_text(window, &mut cx, "ready");
    assert!(
        window
            .read_with(&cx, |w, _| w.file_browser())
            .unwrap()
            .is_none()
    );

    cx.simulate_keystrokes("cmd-shift-e");

    wait_until(
        window,
        &mut cx,
        "the tree shows the pane's folder",
        |w, cx| shown_root(w, cx).as_deref() == Some(&root),
    );
    let browser = browser(window, &mut cx);
    assert_eq!(
        names(&browser, &cx),
        [".git", "a", "src", "big.log", "image.bin", "notes.md"]
    );
    // The keyboard is in the tree.
    let terminal = focused(window, &mut cx);
    assert!(!cx.update(|window, cx| terminal.read(cx).focus_handle().is_focused(window)));

    cx.simulate_keystrokes("cmd-shift-e");

    assert!(
        window
            .read_with(&cx, |w, _| w.file_browser())
            .unwrap()
            .is_none()
    );
    assert!(cx.update(|window, cx| terminal.read(cx).focus_handle().is_focused(window)));
}

/// The list of the installed fonts takes the system a long time to make,
/// so drawing the tree again must not ask for it.
#[gpui::test]
fn drawing_the_tree_again_does_not_look_the_fonts_up_again(cx: &mut TestAppContext) {
    let (_dir, root) = sample();
    let (window, mut cx) = open(&root, "echo ready; sleep 100", cx);
    wait_for_text(window, &mut cx, "ready");
    cx.simulate_keystrokes("cmd-shift-e");
    wait_until(window, &mut cx, "the tree is shown", |w, cx| {
        shown_root(w, cx).as_deref() == Some(&root)
    });
    let before = benterm_ui::theme::font_lookups();

    for _ in 0..3 {
        let _ = window.update(&mut cx, |_, window, _| window.refresh());
        cx.run_until_parked();
    }

    assert_eq!(benterm_ui::theme::font_lookups(), before);
}

#[gpui::test]
fn a_cd_in_the_pane_changes_the_root(cx: &mut TestAppContext) {
    let (_dir, root) = sample();
    let script = "stty -echo; echo ready; read line; cd src; echo moved; sleep 100";
    let (window, mut cx) = open(&root, script, cx);
    wait_for_text(window, &mut cx, "ready");
    cx.simulate_keystrokes("cmd-shift-e");
    wait_until(window, &mut cx, "the tree shows the root", |w, cx| {
        shown_root(w, cx).as_deref() == Some(&root)
    });

    // Escape gives the keyboard back to the pane and leaves the tree open.
    cx.simulate_keystrokes("escape enter");

    let src = root.join("src");
    wait_until(window, &mut cx, "the tree follows the cd", |w, cx| {
        shown_root(w, cx).as_deref() == Some(&src)
    });
    assert_eq!(names(&browser(window, &mut cx), &cx), ["main.rs"]);
}

#[gpui::test]
fn focusing_another_pane_changes_the_root(cx: &mut TestAppContext) {
    let (_dir, root) = sample();
    // The first pane moves into a, and the pane split from it, which
    // starts there, into a/a.
    let (window, mut cx) = open(&root, "cd a; echo moved; sleep 100", cx);
    wait_for_text(window, &mut cx, "moved");
    cx.simulate_keystrokes("cmd-d");
    wait_for_text(window, &mut cx, "moved");
    cx.simulate_keystrokes("cmd-shift-e");
    let (a, aa) = (root.join("a"), root.join("a/a"));
    wait_until(window, &mut cx, "the tree shows a/a", |w, cx| {
        shown_root(w, cx).as_deref() == Some(&aa)
    });

    cx.simulate_keystrokes("escape cmd-alt-left");

    wait_until(window, &mut cx, "the tree shows a", |w, cx| {
        shown_root(w, cx).as_deref() == Some(&a)
    });
}

fn opened(browser: &Entity<FileBrowser>, cx: &VisualTestContext) -> Option<PathBuf> {
    browser.read_with(cx, |browser, _| browser.opened().map(Path::to_owned))
}

#[gpui::test]
fn the_arrows_and_enter_open_folders_and_files(cx: &mut TestAppContext) {
    let (_dir, root) = sample();
    let (window, mut cx) = open(&root, "echo ready; sleep 100", cx);
    wait_for_text(window, &mut cx, "ready");
    cx.simulate_keystrokes("cmd-shift-e");
    wait_until(window, &mut cx, "the tree shows the root", |w, cx| {
        shown_root(w, cx).as_deref() == Some(&root)
    });
    let browser = browser(window, &mut cx);

    // .git, a, src: open src, wait for its entries, then move into it.
    cx.simulate_keystrokes("down down down right");
    wait_until(window, &mut cx, "src opens", |w, cx| {
        w.file_browser()
            .is_some_and(|b| b.read(cx).rows().iter().any(|r| r.name == "main.rs"))
    });
    cx.simulate_keystrokes("right");
    assert_eq!(
        browser.read_with(&cx, |b, _| b.selected().map(Path::to_owned)),
        Some(root.join("src/main.rs"))
    );
    cx.simulate_keystrokes("enter");
    let main = root.join("src/main.rs");
    wait_until(window, &mut cx, "main.rs opens", |w, cx| {
        w.file_browser()
            .and_then(|b| b.read(cx).opened().map(Path::to_owned))
            == Some(main.clone())
    });
    assert_eq!(opened(&browser, &cx), Some(main));
    let content = browser.read_with(&cx, |b, _| b.preview().unwrap().content.clone());
    assert_eq!(content, Content::Text("fn main() {}\n".into()));

    // Left goes up to src, and again closes it.
    cx.simulate_keystrokes("left left");
    assert_eq!(names(&browser, &cx).len(), 6);
    // Enter on a folder opens it too.
    cx.simulate_keystrokes("enter");
    wait_until(window, &mut cx, "src reopens", |w, cx| {
        w.file_browser()
            .is_some_and(|b| b.read(cx).rows().len() == 7)
    });
    assert_eq!(names(&browser, &cx).len(), 7);
}

#[gpui::test]
fn a_click_opens_a_folder_and_a_file(cx: &mut TestAppContext) {
    let (_dir, root) = sample();
    let (window, mut cx) = open(&root, "echo ready; sleep 100", cx);
    wait_for_text(window, &mut cx, "ready");
    cx.simulate_keystrokes("cmd-shift-e");
    wait_until(window, &mut cx, "the tree shows the root", |w, cx| {
        shown_root(w, cx).as_deref() == Some(&root)
    });
    let browser = browser(window, &mut cx);
    cx.run_until_parked();

    let row = |index: usize, cx: &mut VisualTestContext| {
        cx.debug_bounds(Box::leak(format!("file-row-{index}").into_boxed_str()))
            .expect("the row is drawn")
            .center()
    };
    let src = row(2, &mut cx);
    cx.simulate_click(src, Modifiers::none());
    wait_until(window, &mut cx, "src opens on click", |w, cx| {
        w.file_browser()
            .is_some_and(|b| b.read(cx).rows().iter().any(|r| r.name == "main.rs"))
    });
    assert_eq!(names(&browser, &cx)[3], "main.rs");

    let main_path = root.join("src/main.rs");
    let main = row(3, &mut cx);
    cx.simulate_click(main, Modifiers::none());
    wait_until(window, &mut cx, "main.rs opens on click", |w, cx| {
        w.file_browser()
            .and_then(|b| b.read(cx).opened().map(Path::to_owned))
            == Some(main_path.clone())
    });
    assert_eq!(opened(&browser, &cx), Some(main_path));
}

#[gpui::test]
fn a_large_file_says_on_screen_that_only_its_start_is_shown(cx: &mut TestAppContext) {
    let (_dir, root) = sample();
    let (window, mut cx) = open(&root, "echo ready; sleep 100", cx);
    wait_for_text(window, &mut cx, "ready");
    cx.simulate_keystrokes("cmd-shift-e");
    wait_until(window, &mut cx, "the tree shows the root", |w, cx| {
        shown_root(w, cx).as_deref() == Some(&root)
    });
    let browser = browser(window, &mut cx);
    let big = root.join("big.log");
    browser.update(&mut cx, |b, cx| b.open_path(&big, cx));
    wait_until(window, &mut cx, "big.log opens", |w, cx| {
        w.file_browser()
            .and_then(|b| b.read(cx).opened().map(Path::to_owned))
            == Some(big.clone())
    });

    assert!(browser.read_with(&cx, |b, _| b.preview().unwrap().truncated));
    assert!(cx.debug_bounds("file-preview-truncated").is_some());
    assert!(cx.debug_bounds("file-preview-not-text").is_none());
}

#[gpui::test]
fn a_file_that_is_not_text_says_so_on_screen(cx: &mut TestAppContext) {
    let (_dir, root) = sample();
    let (window, mut cx) = open(&root, "echo ready; sleep 100", cx);
    wait_for_text(window, &mut cx, "ready");
    cx.simulate_keystrokes("cmd-shift-e");
    wait_until(window, &mut cx, "the tree shows the root", |w, cx| {
        shown_root(w, cx).as_deref() == Some(&root)
    });
    let browser = browser(window, &mut cx);
    let image = root.join("image.bin");
    browser.update(&mut cx, |b, cx| b.open_path(&image, cx));
    wait_until(window, &mut cx, "image.bin opens", |w, cx| {
        w.file_browser()
            .and_then(|b| b.read(cx).opened().map(Path::to_owned))
            == Some(image.clone())
    });

    assert!(cx.debug_bounds("file-preview-not-text").is_some());
    assert!(cx.debug_bounds("file-preview-truncated").is_none());
}

/// Every file and folder under `root` with its modification time, and
/// the contents of the files.
fn snapshot(root: &Path) -> BTreeMap<PathBuf, (SystemTime, Option<Vec<u8>>)> {
    let mut found = BTreeMap::new();
    let mut folders = vec![root.to_owned()];
    while let Some(folder) = folders.pop() {
        let meta = fs::symlink_metadata(&folder).unwrap();
        found.insert(folder.clone(), (meta.modified().unwrap(), None));
        for entry in fs::read_dir(&folder).unwrap() {
            let path = entry.unwrap().path();
            let meta = fs::symlink_metadata(&path).unwrap();
            if meta.is_dir() {
                folders.push(path);
            } else {
                let bytes = fs::read(&path).unwrap();
                found.insert(path, (meta.modified().unwrap(), Some(bytes)));
            }
        }
    }
    found
}

#[gpui::test]
fn browsing_changes_no_file_and_nothing_in_git(cx: &mut TestAppContext) {
    let (_dir, root) = sample();
    let before = snapshot(&root);
    let (window, mut cx) = open(&root, "echo ready; sleep 100", cx);
    wait_for_text(window, &mut cx, "ready");
    // Some time passes, so that a write would change a time.
    std::thread::sleep(Duration::from_millis(20));

    cx.simulate_keystrokes("cmd-shift-e");
    wait_until(window, &mut cx, "the tree shows the root", |w, cx| {
        shown_root(w, cx).as_deref() == Some(&root)
    });
    // Every folder opened with the keys, and the files shown on the way.
    for _ in 0..40 {
        cx.simulate_keystrokes("right down");
    }
    cx.simulate_keystrokes("enter up enter left left left escape cmd-shift-e cmd-shift-e");
    wait_until(window, &mut cx, "the tree reopens", |w, cx| {
        shown_root(w, cx).as_deref() == Some(&root)
    });
    let browser = browser(window, &mut cx);
    let mut paths: Vec<PathBuf> = before.keys().cloned().collect();
    paths.retain(|path| path.is_file());
    for path in &paths {
        browser.update(&mut cx, |b, cx| b.open_path(path, cx));
        wait_until(window, &mut cx, "the file opens", |w, cx| {
            w.file_browser()
                .and_then(|b| b.read(cx).opened().map(Path::to_owned))
                == Some(path.clone())
        });
    }
    let opened_any = browser.read_with(&cx, |b, _| b.opened().is_some());
    assert!(opened_any);
    cx.simulate_keystrokes("cmd-shift-e");
    cx.run_until_parked();

    assert_eq!(snapshot(&root), before);
}

/// Choosing a named pipe does not stop the app, and the panel says it is
/// not a regular file.
#[gpui::test]
fn a_named_pipe_says_on_screen_that_it_is_not_a_regular_file(cx: &mut TestAppContext) {
    let (_dir, root) = sample();
    let pipe = root.join("pipe");
    let status = std::process::Command::new("mkfifo")
        .arg(&pipe)
        .status()
        .unwrap();
    assert!(status.success());
    let (window, mut cx) = open(&root, "echo ready; sleep 100", cx);
    wait_for_text(window, &mut cx, "ready");
    cx.simulate_keystrokes("cmd-shift-e");
    wait_until(window, &mut cx, "the tree shows the root", |w, cx| {
        shown_root(w, cx).as_deref() == Some(&root)
    });
    let browser = browser(window, &mut cx);

    // If the panel opened the pipe, the open would wait for a writer.
    // One comes after a while, so that the test fails instead of hanging.
    let writer = pipe.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(3));
        let _ = fs::OpenOptions::new().write(true).open(writer);
    });
    let started = Instant::now();
    browser.update(&mut cx, |b, cx| b.open_path(&pipe, cx));
    wait_until(window, &mut cx, "the pipe preview opens", |w, cx| {
        w.file_browser()
            .and_then(|b| b.read(cx).opened().map(Path::to_owned))
            == Some(pipe.clone())
    });

    assert!(
        started.elapsed() < Duration::from_secs(2),
        "choosing the pipe waited"
    );
    assert_eq!(opened(&browser, &cx), Some(pipe));
    assert!(cx.debug_bounds("file-preview-not-regular").is_some());
}

/// A gate that pauses a background read on a given path until released.
struct Gate {
    state: Mutex<GateState>,
    changed: Condvar,
}

#[derive(PartialEq, Eq)]
enum GateState {
    Pending,
    Entered,
    Released,
    TimedOut,
}

impl Gate {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(GateState::Pending),
            changed: Condvar::new(),
        })
    }

    fn wait(&self, timeout: Duration) {
        let mut state = self.state.lock().unwrap();
        *state = GateState::Entered;
        self.changed.notify_all();
        let deadline = Instant::now() + timeout;
        while *state == GateState::Entered {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                *state = GateState::TimedOut;
                break;
            }
            let (next, _) = self.changed.wait_timeout(state, remaining).unwrap();
            state = next;
        }
    }

    fn wait_entered(&self, timeout: Duration) -> bool {
        let mut state = self.state.lock().unwrap();
        let deadline = Instant::now() + timeout;
        while *state == GateState::Pending {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return false;
            }
            let (next, _) = self.changed.wait_timeout(state, remaining).unwrap();
            state = next;
        }
        *state == GateState::Entered
    }

    fn release(&self) {
        let mut state = self.state.lock().unwrap();
        assert!(
            *state == GateState::Entered,
            "the read must still be waiting at the gate"
        );
        *state = GateState::Released;
        self.changed.notify_all();
    }
}

/// A fake reader that delegates to the real file system while recording
/// calls and pausing specific paths on demand.
struct ControlledReader {
    folder_gates: Mutex<HashMap<PathBuf, Arc<Gate>>>,
    preview_gates: Mutex<HashMap<PathBuf, Arc<Gate>>>,
    folder_reads: Mutex<Vec<PathBuf>>,
    gate_timeout: Duration,
}

impl ControlledReader {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            folder_gates: Mutex::new(HashMap::new()),
            preview_gates: Mutex::new(HashMap::new()),
            folder_reads: Mutex::new(Vec::new()),
            gate_timeout: Duration::from_secs(5),
        })
    }

    fn block_folder(&self, path: &Path) -> Arc<Gate> {
        let gate = Gate::new();
        self.folder_gates
            .lock()
            .unwrap()
            .insert(path.to_owned(), gate.clone());
        gate
    }

    fn block_preview(&self, path: &Path) -> Arc<Gate> {
        let gate = Gate::new();
        self.preview_gates
            .lock()
            .unwrap()
            .insert(path.to_owned(), gate.clone());
        gate
    }

    fn take_folder_reads(&self) -> Vec<PathBuf> {
        std::mem::take(&mut *self.folder_reads.lock().unwrap())
    }
}

impl FileReader for ControlledReader {
    fn read_folder(&self, folder: &Path) -> Result<Vec<FolderEntry>, String> {
        self.folder_reads.lock().unwrap().push(folder.to_owned());
        let gate = self.folder_gates.lock().unwrap().remove(folder);
        if let Some(gate) = gate {
            gate.wait(self.gate_timeout);
        }
        FsReader.read_folder(folder)
    }

    fn read_preview(&self, path: &Path, limit: usize) -> Preview {
        let gate = self.preview_gates.lock().unwrap().remove(path);
        if let Some(gate) = gate {
            gate.wait(self.gate_timeout);
        }
        FsReader.read_preview(path, limit)
    }
}

fn wait_browser(
    browser: &Entity<FileBrowser>,
    cx: &mut TestAppContext,
    what: &str,
    done: impl Fn(&FileBrowser) -> bool,
) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        cx.run_until_parked();
        if browser.read_with(cx, |b, _| done(b)) {
            return;
        }
        assert!(Instant::now() < deadline, "timed out waiting until {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[gpui::test]
fn opening_a_folder_returns_immediately_and_shows_entries_when_the_reader_finishes(
    cx: &mut TestAppContext,
) {
    cx.executor().allow_parking();
    let (_dir, root) = sample();
    let reader = ControlledReader::new();
    let root_gate = reader.block_folder(&root);
    let browser = cx.new(|cx| FileBrowser::with_reader(Theme::default(), reader.clone(), cx));

    browser.update(cx, |b, cx| b.follow(Some(root.clone()), cx));
    assert!(root_gate.wait_entered(Duration::from_secs(2)));
    assert!(browser.read_with(cx, |b, _| b.rows().is_empty()));

    root_gate.release();
    wait_browser(&browser, cx, "root rows appear", |b| {
        b.root() == Some(root.as_path()) && !b.rows().is_empty()
    });
    assert_eq!(
        browser.read_with(cx, |b, _| {
            b.rows().into_iter().map(|r| r.name).collect::<Vec<_>>()
        }),
        [".git", "a", "src", "big.log", "image.bin", "notes.md"]
    );

    // Opening a subfolder also returns immediately and keeps previous rows until done.
    let src = root.join("src");
    let src_gate = reader.block_folder(&src);
    browser.update(cx, |b, cx| b.open_path(&src, cx));
    assert!(src_gate.wait_entered(Duration::from_secs(2)));
    assert_eq!(browser.read_with(cx, |b, _| b.rows().len()), 6);

    src_gate.release();
    wait_browser(&browser, cx, "src rows appear", |b| {
        b.rows().iter().any(|r| r.path == root.join("src/main.rs"))
    });
}

#[gpui::test]
fn changing_the_root_while_reading_discards_the_old_roots_result(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    let (_dir_a, root_a) = sample();
    let dir_b = tempfile::tempdir().unwrap();
    let root_b = dir_b.path().canonicalize().unwrap();
    fs::write(root_b.join("only_in_b.txt"), "b").unwrap();

    let reader = ControlledReader::new();
    let gate_a = reader.block_folder(&root_a);
    let browser = cx.new(|cx| FileBrowser::with_reader(Theme::default(), reader.clone(), cx));

    browser.update(cx, |b, cx| b.follow(Some(root_a.clone()), cx));
    assert!(gate_a.wait_entered(Duration::from_secs(2)));
    assert_eq!(
        browser.read_with(cx, |b, _| b.root().map(Path::to_owned)),
        None
    );

    // Switch to root_b before root_a finishes.
    browser.update(cx, |b, cx| b.follow(Some(root_b.clone()), cx));
    wait_browser(&browser, cx, "root_b rows appear", |b| {
        b.root() == Some(root_b.as_path())
    });
    assert_eq!(
        browser.read_with(cx, |b, _| {
            b.rows().into_iter().map(|r| r.name).collect::<Vec<_>>()
        }),
        ["only_in_b.txt"]
    );

    // Let root_a finish now; its stale result must be discarded.
    gate_a.release();
    std::thread::sleep(Duration::from_millis(100));
    cx.run_until_parked();
    assert_eq!(
        browser.read_with(cx, |b, _| b.root().map(Path::to_owned)),
        Some(root_b)
    );
    assert_eq!(
        browser.read_with(cx, |b, _| {
            b.rows().into_iter().map(|r| r.name).collect::<Vec<_>>()
        }),
        ["only_in_b.txt"]
    );
}

#[gpui::test]
fn opening_a_file_keeps_previous_preview_and_discards_stale_results(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    let (_dir, root) = sample();
    let notes = root.join("notes.md");
    let big = root.join("big.log");
    let main = root.join("src/main.rs");

    let reader = ControlledReader::new();
    let browser = cx.new(|cx| FileBrowser::with_reader(Theme::default(), reader.clone(), cx));
    browser.update(cx, |b, cx| b.follow(Some(root.clone()), cx));
    wait_browser(&browser, cx, "root rows appear", |b| {
        b.root() == Some(root.as_path())
    });

    browser.update(cx, |b, cx| b.open_path(&main, cx));
    wait_browser(&browser, cx, "main.rs preview appears", |b| {
        b.opened() == Some(main.as_path())
    });

    // Start reading notes.md while paused: main.rs must remain shown.
    let notes_gate = reader.block_preview(&notes);
    let started = Instant::now();
    browser.update(cx, |b, cx| b.open_path(&notes, cx));
    assert!(
        started.elapsed() < Duration::from_millis(150),
        "open_path for file blocked the UI thread"
    );
    assert!(notes_gate.wait_entered(Duration::from_secs(2)));
    assert_eq!(
        browser.read_with(cx, |b, _| b.opened().map(Path::to_owned)),
        Some(main.clone())
    );

    // Open big.log before notes.md completes, then release notes.md.
    browser.update(cx, |b, cx| b.open_path(&big, cx));
    wait_browser(&browser, cx, "big.log preview appears", |b| {
        b.opened() == Some(big.as_path())
    });
    notes_gate.release();
    std::thread::sleep(Duration::from_millis(100));
    cx.run_until_parked();
    assert_eq!(
        browser.read_with(cx, |b, _| b.opened().map(Path::to_owned)),
        Some(big)
    );

    // Opening notes.md again parses Markdown off the UI thread and displays it.
    browser.update(cx, |b, cx| b.open_path(&notes, cx));
    wait_browser(&browser, cx, "notes.md markdown preview appears", |b| {
        b.opened() == Some(notes.as_path())
            && matches!(b.preview().map(|p| &p.content), Some(Content::Markdown(_)))
    });
}

#[gpui::test]
fn creating_and_removing_files_in_open_folders_updates_without_reopening(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    let (_dir, root) = sample();
    let src = root.join("src");
    let browser = cx.new(|cx| FileBrowser::with_reader(Theme::default(), Arc::new(FsReader), cx));
    browser.update(cx, |b, cx| b.follow(Some(root.clone()), cx));
    wait_browser(&browser, cx, "root rows appear", |b| {
        b.root() == Some(root.as_path())
    });
    browser.update(cx, |b, cx| b.open_path(&src, cx));
    wait_browser(&browser, cx, "src rows appear", |b| {
        b.rows().iter().any(|r| r.path == src.join("main.rs"))
    });

    // Create a file in the root and in the open subfolder.
    let added_root = root.join("added.txt");
    let added_src = src.join("extra.rs");
    fs::write(&added_root, "hello").unwrap();
    fs::write(&added_src, "pub fn extra() {}").unwrap();

    wait_browser(&browser, cx, "new files appear in open folders", |b| {
        let rows = b.rows();
        rows.iter().any(|r| r.path == added_root) && rows.iter().any(|r| r.path == added_src)
    });

    // Remove the file in the open subfolder; it disappears without reopening.
    fs::remove_file(&added_src).unwrap();
    wait_browser(&browser, cx, "removed file disappears", |b| {
        !b.rows().iter().any(|r| r.path == added_src)
    });
}

#[gpui::test]
fn creating_a_file_in_a_closed_folder_does_not_trigger_a_reread(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    let (_dir, root) = sample();
    let src = root.join("src");
    let reader = ControlledReader::new();
    let browser = cx.new(|cx| FileBrowser::with_reader(Theme::default(), reader.clone(), cx));
    browser.update(cx, |b, cx| b.follow(Some(root.clone()), cx));
    wait_browser(&browser, cx, "root rows appear", |b| {
        b.root() == Some(root.as_path())
    });

    // Open src, wait for it to load, then close it.
    browser.update(cx, |b, cx| b.open_path(&src, cx));
    wait_browser(&browser, cx, "src rows appear", |b| {
        b.rows().iter().any(|r| r.path == src.join("main.rs"))
    });
    browser.update(cx, |b, cx| b.open_path(&src, cx));
    wait_browser(&browser, cx, "src closes", |b| {
        !b.rows().iter().any(|r| r.path == src.join("main.rs"))
    });

    // Clear recorded reads, then create a file inside the closed folder.
    std::thread::sleep(Duration::from_millis(150));
    cx.run_until_parked();
    reader.take_folder_reads();

    fs::write(src.join("ignored.rs"), "// closed").unwrap();
    for _ in 0..35 {
        cx.run_until_parked();
        std::thread::sleep(Duration::from_millis(10));
    }
    let reads = reader.take_folder_reads();
    assert!(
        !reads.contains(&src),
        "closed folder was unexpectedly reread: {reads:?}"
    );

    // Meanwhile, creating a file in the open root still triggers a reread.
    let marker = root.join("still_watched.txt");
    fs::write(&marker, "ok").unwrap();
    wait_browser(&browser, cx, "open root picks up new file", |b| {
        b.rows().iter().any(|r| r.path == marker)
    });
}

//! A new tab and a split pane start in the directory of the focused pane.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use benterm_terminal::PtyConfig;
use benterm_ui::settings::ConfigFeed;
use benterm_ui::{BentermWindow, TerminalView, bind_keys, open_terminal_window};
use gpui::{Entity, TestAppContext, VisualTestContext, WindowHandle};

/// What every pane runs. It shows the name of the directory it starts
/// in, written with shell arithmetic so that only its output matches,
/// then moves into `inner` if there is one, and stays. Under that line
/// it prints more lines than any pane has, in the same write, so that
/// the line has always scrolled off the screen by the time the screen
/// shows it, as it sometimes has in a small split pane.
const SCRIPT: &str = r#"printf '%s\n%s\n' "start=$(basename "$(pwd -P)")-$((40 + 2))" "$(seq 200)"; cd inner; sleep 100"#;

/// Opens a window whose first pane starts in `root`, with the app's key
/// bindings. The terminals' worker threads wake the app from outside the
/// test scheduler, which the scheduler has to be told to accept.
fn open(root: &Path, cx: &mut TestAppContext) -> (WindowHandle<BentermWindow>, VisualTestContext) {
    cx.executor().allow_parking();
    cx.update(bind_keys);
    let pty = PtyConfig {
        cwd: Some(root.to_owned()),
        ..PtyConfig::command("/bin/sh", ["-c", SCRIPT])
    };
    let window = cx
        .update(|cx| open_terminal_window(pty, ConfigFeed::defaults(), cx))
        .unwrap();
    let vcx = VisualTestContext::from_window(window.into(), cx);
    vcx.run_until_parked();
    (window, vcx)
}

/// A directory with the directory `inner` in it, with its symbolic links
/// resolved, as the kernel reports directories.
fn root_with_inner() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    fs::create_dir(root.join("inner")).unwrap();
    (dir, root)
}

/// The terminal of the focused pane of the selected tab.
fn focused(
    window: WindowHandle<BentermWindow>,
    cx: &mut VisualTestContext,
) -> Entity<TerminalView> {
    window
        .update(cx, |root, _, cx| root.terminal(cx))
        .expect("the window is open")
}

/// Lets the app run until `done` holds for the focused terminal. The
/// programs run in real time on other threads, hence the polling.
fn wait_until(
    window: WindowHandle<BentermWindow>,
    cx: &mut VisualTestContext,
    what: &str,
    done: impl Fn(&TerminalView) -> bool,
) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        cx.run_until_parked();
        let view = focused(window, cx);
        if view.read_with(cx, |view, _| done(view)) {
            return;
        }
        assert!(Instant::now() < deadline, "timed out waiting until {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Whether the terminal has printed a line with `text` in it. A small
/// pane can have scrolled the line off its screen, or wrapped it over
/// two rows, so the whole scrollback is read as lines.
fn shows(view: &TerminalView, text: &str) -> bool {
    view.text(Some(usize::MAX))
        .lines()
        .any(|line| line.contains(text))
}

/// Opens the window and waits until its first pane has moved to `inner`.
fn open_and_move(
    root: &Path,
    cx: &mut TestAppContext,
) -> (WindowHandle<BentermWindow>, VisualTestContext) {
    let (window, mut cx) = open(root, cx);
    let inner = root.join("inner");
    wait_until(window, &mut cx, "the first pane is in inner", |view| {
        view.working_directory().as_deref() == Some(&inner)
    });
    (window, cx)
}

#[gpui::test]
fn each_pane_tells_its_working_directory(cx: &mut TestAppContext) {
    let (_dir, root) = root_with_inner();
    let (window, mut cx) = open(&root, cx);

    wait_until(window, &mut cx, "the pane is in inner", |view| {
        view.working_directory() == Some(root.join("inner"))
    });
}

fn a_split_starts_in_the_directory_of_the_focused_pane(keys: &str, cx: &mut TestAppContext) {
    let (_dir, root) = root_with_inner();
    let (window, mut cx) = open_and_move(&root, cx);
    let first = focused(window, &mut cx);

    cx.simulate_keystrokes(keys);

    assert_ne!(focused(window, &mut cx).entity_id(), first.entity_id());
    wait_until(window, &mut cx, "the new pane started in inner", |view| {
        shows(view, "start=inner-42")
    });
}

#[gpui::test]
fn a_pane_split_right_starts_in_the_directory_of_the_focused_pane(cx: &mut TestAppContext) {
    a_split_starts_in_the_directory_of_the_focused_pane("cmd-d", cx);
}

#[gpui::test]
fn a_pane_split_down_starts_in_the_directory_of_the_focused_pane(cx: &mut TestAppContext) {
    a_split_starts_in_the_directory_of_the_focused_pane("cmd-shift-d", cx);
}

#[gpui::test]
fn a_new_tab_starts_in_the_directory_of_the_focused_pane(cx: &mut TestAppContext) {
    let (_dir, root) = root_with_inner();
    let (window, mut cx) = open_and_move(&root, cx);
    let first = focused(window, &mut cx);

    cx.simulate_keystrokes("cmd-t");

    assert_ne!(focused(window, &mut cx).entity_id(), first.entity_id());
    wait_until(window, &mut cx, "the new tab started in inner", |view| {
        shows(view, "start=inner-42")
    });
}

#[gpui::test]
fn a_new_pane_starts_where_the_first_did_when_the_directory_is_gone(cx: &mut TestAppContext) {
    let (_dir, root) = root_with_inner();
    let (window, mut cx) = open_and_move(&root, cx);
    let first = focused(window, &mut cx);
    let name = root.file_name().unwrap().to_str().unwrap().to_owned();
    fs::remove_dir(root.join("inner")).unwrap();

    cx.simulate_keystrokes("cmd-d");

    assert_ne!(focused(window, &mut cx).entity_id(), first.entity_id());
    wait_until(
        window,
        &mut cx,
        "the new pane started in the root",
        |view| shows(view, &format!("start={name}-42")),
    );
}

#[gpui::test]
fn a_new_pane_starts_where_the_first_did_when_the_directory_cannot_be_entered(
    cx: &mut TestAppContext,
) {
    use std::os::unix::fs::PermissionsExt;

    let (_dir, root) = root_with_inner();
    let (window, mut cx) = open_and_move(&root, cx);
    let first = focused(window, &mut cx);
    let name = root.file_name().unwrap().to_str().unwrap().to_owned();
    // Still a directory, but no program can be started in it.
    let inner = root.join("inner");
    fs::set_permissions(&inner, fs::Permissions::from_mode(0o000)).unwrap();

    cx.simulate_keystrokes("cmd-t");

    fs::set_permissions(&inner, fs::Permissions::from_mode(0o755)).unwrap();
    assert_ne!(focused(window, &mut cx).entity_id(), first.entity_id());
    wait_until(window, &mut cx, "the new tab started in the root", |view| {
        shows(view, &format!("start={name}-42"))
    });
}

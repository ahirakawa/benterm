//! The window saves what each pane showed and shows it again, above the
//! output of the new shell, when it is made from the saved session.
//! Every test has a session file of its own in a temporary directory.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

use benterm_core::{AlacrittyCore, Color, GridSize, TerminalCore};
use benterm_session::{Node, Session, Tab, Terminal, Workspace};
use benterm_terminal::PtyConfig;
use benterm_ui::session::{SAVE_INTERVAL, open_session_window};
use benterm_ui::settings::ConfigFeed;
use benterm_ui::{BentermWindow, SpawnTerminal, StartedTerminal, bind_keys};
use gpui::{TestAppContext, VisualTestContext, WindowHandle};

/// A place for the session file and the scrollback files beside it.
struct Place {
    _dir: tempfile::TempDir,
    file: PathBuf,
    scrollback: PathBuf,
}

impl Place {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().canonicalize().unwrap().join("state");
        Self {
            _dir: dir,
            file: state.join("session.json"),
            scrollback: state.join("scrollback"),
        }
    }

    fn saved(&self) -> Option<Session> {
        let text = fs::read(&self.file).ok()?;
        Some(Session::from_json(&text).expect("the session file is whole"))
    }

    /// The keys of the panes in the session file.
    fn keys(&self) -> Vec<String> {
        let saved = self.saved();
        let keys = saved.iter().flat_map(Session::scrollback_keys);
        keys.map(str::to_owned).collect()
    }

    /// The names of the scrollback files.
    fn files(&self) -> Vec<String> {
        let Ok(entries) = fs::read_dir(&self.scrollback) else {
            return Vec::new();
        };
        let mut names: Vec<String> = entries
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    }

    /// The rows of the scrollback file of `key`, as a core shows them.
    fn rows(&self, key: &str) -> Vec<String> {
        let bytes = fs::read(self.scrollback.join(key)).unwrap_or_default();
        let mut core = AlacrittyCore::new(GridSize::new(50, 80), 100);
        core.feed(&bytes);
        let rows = core.snapshot().rows;
        let texts = rows.iter().map(|row| row.text().trim_end().to_owned());
        texts.filter(|text| !text.is_empty()).collect()
    }
}

/// Starts `sh -c script` in every pane.
fn sh(script: &'static str) -> SpawnTerminal {
    Rc::new(move |_, theme, _, size| {
        let pty = PtyConfig {
            size,
            ..PtyConfig::command("/bin/sh", ["-c", script])
        };
        StartedTerminal::spawn(pty, theme)
    })
}

fn open(
    place: &Place,
    spawn: SpawnTerminal,
    settings: &str,
    cx: &mut TestAppContext,
) -> (WindowHandle<BentermWindow>, VisualTestContext) {
    cx.executor().allow_parking();
    cx.update(bind_keys);
    let feed = ConfigFeed::new(benterm_config::parse(settings).unwrap());
    let window = cx
        .update(|cx| open_session_window(spawn, feed, Some(&place.file), cx))
        .unwrap();
    let vcx = VisualTestContext::from_window(window.into(), cx);
    vcx.run_until_parked();
    (window, vcx)
}

fn wait_until(
    cx: &mut VisualTestContext,
    what: &str,
    mut done: impl FnMut(&mut VisualTestContext) -> bool,
) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        cx.run_until_parked();
        if done(cx) {
            return;
        }
        assert!(Instant::now() < deadline, "timed out waiting until {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// The lines the focused pane shows, scrollback and screen.
fn lines(window: WindowHandle<BentermWindow>, cx: &mut VisualTestContext) -> Vec<String> {
    let text = window
        .update(cx, |root, _, cx| {
            root.terminal(cx).read(cx).text(Some(1000))
        })
        .unwrap();
    text.lines().map(str::to_owned).collect()
}

fn wait_for_line(window: WindowHandle<BentermWindow>, cx: &mut VisualTestContext, line: &str) {
    wait_until(cx, &format!("the pane shows {line:?}"), |cx| {
        lines(window, cx).iter().any(|shown| shown == line)
    });
}

/// Lets the time between two saves pass, again and again, until `done`
/// holds.
fn save_until(cx: &mut VisualTestContext, what: &str, mut done: impl FnMut() -> bool) {
    wait_until(cx, what, |cx| {
        cx.executor().advance_clock(SAVE_INTERVAL);
        cx.run_until_parked();
        done()
    });
}

/// Closes the window, which saves, and lets go of the session file.
fn close(window: WindowHandle<BentermWindow>, mut cx: VisualTestContext) -> TestAppContext {
    assert!(cx.simulate_close());
    window
        .update(&mut cx, |_, window, _| window.remove_window())
        .unwrap();
    cx.run_until_parked();
    cx.cx
}

const OLD: &str = "printf '\\033[31mred\\033[0m and plain\\r\\nsecond line\\r\\n'; sleep 100";
const NEW: &str = "printf prompt; sleep 100";

#[gpui::test]
fn what_a_pane_showed_is_shown_again_above_the_output_of_the_new_shell(cx: &mut TestAppContext) {
    let place = Place::new();
    let (window, mut cx) = open(&place, sh(OLD), "", cx);
    wait_for_line(window, &mut cx, "second line");
    let rows = |window: WindowHandle<BentermWindow>, cx: &mut VisualTestContext| {
        let view = window.update(cx, |root, _, cx| root.terminal(cx)).unwrap();
        cx.read(|cx| view.read(cx).history(1000)).unwrap()
    };
    let showed = rows(window, &mut cx);
    let mut cx = close(window, cx);

    let (window, mut cx) = open(&place, sh(NEW), "", &mut cx);
    wait_for_line(window, &mut cx, "prompt");
    assert_eq!(
        lines(window, &mut cx),
        ["red and plain", "second line", "prompt"]
    );
    // The same screen state, by way of the file: every cell of every
    // row, and how each row ends.
    let shows = rows(window, &mut cx);
    assert_eq!(shows[..showed.len()], showed[..]);
    // With its colors.
    let red = window
        .update(&mut cx, |root, _, cx| {
            root.terminal(cx).read(cx).screen().rows[0].cells[0].fg
        })
        .unwrap();
    assert_eq!(red, Color::Indexed(1));
}

#[gpui::test]
fn what_a_pane_shows_is_saved_while_the_window_is_open(cx: &mut TestAppContext) {
    let place = Place::new();
    let (window, mut cx) = open(&place, sh(OLD), "", cx);
    wait_for_line(window, &mut cx, "second line");
    save_until(&mut cx, "the pane's scrollback is saved", || {
        let keys = place.keys();
        keys.len() == 1 && place.rows(&keys[0]) == ["red and plain", "second line"]
    });
    assert_eq!(place.files(), place.keys());
}

#[gpui::test]
fn only_the_user_can_read_what_was_saved(cx: &mut TestAppContext) {
    let place = Place::new();
    let (window, mut cx) = open(&place, sh(OLD), "", cx);
    wait_for_line(window, &mut cx, "second line");
    close(window, cx);

    let mode = |path: &PathBuf| fs::metadata(path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&place.scrollback), 0o700);
    let files = place.files();
    assert_eq!(files.len(), 1);
    assert_eq!(mode(&place.scrollback.join(&files[0])), 0o600);
}

#[gpui::test]
fn a_pane_keeps_its_key_wherever_it_is_among_the_panes(cx: &mut TestAppContext) {
    let place = Place::new();
    let (window, mut cx) = open(&place, sh(OLD), "", cx);
    save_until(&mut cx, "the pane has a key", || place.keys().len() == 1);
    let first = place.keys()[0].clone();

    // A pane is split off; the focus is on the new one.
    cx.simulate_keystrokes("cmd-d");
    save_until(&mut cx, "both panes have keys", || place.keys().len() == 2);
    assert_eq!(place.keys()[0], first);
    let second = place.keys()[1].clone();
    assert_ne!(second, first);

    // After a restart each pane has the key it had.
    let mut cx = close(window, cx);
    let (window, mut cx) = open(&place, sh(NEW), "", &mut cx);
    wait_for_line(window, &mut cx, "prompt");
    save_until(&mut cx, "the panes are saved again", || {
        place.keys().len() == 2 && place.files().len() == 2
    });
    assert_eq!(place.keys(), [first.clone(), second.clone()]);

    // The first pane is closed: the second has the place the first had,
    // and still its own key. The file of the first is removed.
    cx.simulate_keystrokes("cmd-alt-left");
    cx.simulate_keystrokes("cmd-w");
    save_until(&mut cx, "the closed pane's file is removed", || {
        place.files() == [second.clone()]
    });
    assert_eq!(place.keys(), [second]);
}

/// Each pane says which one it is, so that a pane shown what another
/// showed is noticed.
#[gpui::test]
fn every_pane_is_shown_what_it_showed_itself(cx: &mut TestAppContext) {
    let place = Place::new();
    let named: SpawnTerminal = Rc::new(|pane, theme, _, size| {
        let script = format!("printf 'was pane {pane}\\r\\n'; sleep 100");
        let pty = PtyConfig {
            size,
            ..PtyConfig::command("/bin/sh", ["-c", &script])
        };
        StartedTerminal::spawn(pty, theme)
    });
    let (window, mut cx) = open(&place, named, "", cx);
    cx.simulate_keystrokes("cmd-d");
    cx.simulate_keystrokes("cmd-shift-d");
    // The focus is not on the last pane when the window closes.
    cx.simulate_keystrokes("cmd-alt-left");
    let texts = |window: WindowHandle<BentermWindow>, cx: &mut VisualTestContext| {
        let grid = window.update(cx, |root, _, cx| root.grid(cx)).unwrap();
        cx.read(|cx| {
            let grid = grid.read(cx);
            let panes = grid.panes().into_iter();
            panes
                .map(|pane| grid.terminal(pane).unwrap().read(cx).text(Some(100)))
                .collect::<Vec<String>>()
        })
    };
    wait_until(&mut cx, "every pane has said which it is", |cx| {
        texts(window, cx)
            .iter()
            .all(|text| text.contains("was pane"))
    });
    let before = texts(window, &mut cx);
    assert_eq!(before.len(), 3);
    assert!(before[0] != before[1] && before[1] != before[2] && before[0] != before[2]);
    let mut cx = close(window, cx);

    let (window, mut cx) = open(&place, sh(NEW), "", &mut cx);
    wait_until(&mut cx, "every new shell has written its prompt", |cx| {
        texts(window, cx)
            .iter()
            .all(|text| text.ends_with("prompt"))
    });
    let expected: Vec<String> = before.iter().map(|was| format!("{was}\nprompt")).collect();
    assert_eq!(texts(window, &mut cx), expected);
}

#[gpui::test]
fn no_more_rows_are_saved_than_the_settings_say(cx: &mut TestAppContext) {
    let place = Place::new();
    let settings = "[session]\nscrollback_lines = 3\n";
    let script = "seq 1 40; printf 'end\\r\\n'; sleep 100";
    let (window, mut cx) = open(&place, sh(script), settings, cx);
    wait_for_line(window, &mut cx, "end");
    let mut cx = close(window, cx);
    assert_eq!(place.rows(&place.keys()[0]), ["39", "40", "end"]);

    let (window, mut cx) = open(&place, sh(NEW), settings, &mut cx);
    wait_for_line(window, &mut cx, "prompt");
    assert_eq!(lines(window, &mut cx), ["39", "40", "end", "prompt"]);
}

#[gpui::test]
fn with_no_rows_to_keep_nothing_is_saved_and_what_was_saved_is_removed(cx: &mut TestAppContext) {
    let place = Place::new();
    let (window, mut cx) = open(&place, sh(OLD), "", cx);
    wait_for_line(window, &mut cx, "second line");
    let mut cx = close(window, cx);
    assert_eq!(place.files().len(), 1);

    let none = "[session]\nscrollback_lines = 0\n";
    let (window, mut cx) = open(&place, sh(NEW), none, &mut cx);
    wait_for_line(window, &mut cx, "prompt");
    assert_eq!(lines(window, &mut cx), ["prompt"]);
    assert_eq!(place.files(), [""; 0]);
    close(window, cx);
    assert_eq!(place.keys(), [""; 0]);
    assert_eq!(place.files(), [""; 0]);
}

#[gpui::test]
fn a_session_of_the_first_version_is_made_again_and_its_panes_get_keys(cx: &mut TestAppContext) {
    let place = Place::new();
    let first_version =
        r#"{"version":1,"workspaces":[{"name":"a","tabs":[{"root":{"kind":"terminal"}}]}]}"#;
    fs::create_dir_all(place.file.parent().unwrap()).unwrap();
    fs::write(&place.file, first_version).unwrap();

    let (window, mut cx) = open(&place, sh(NEW), "", cx);
    wait_for_line(window, &mut cx, "prompt");
    save_until(&mut cx, "the pane is saved with a key", || {
        place.keys().len() == 1 && place.files() == place.keys()
    });
    assert_eq!(place.saved().unwrap().workspaces[0].name, "a");
}

#[gpui::test]
fn a_file_that_is_not_scrollback_is_not_shown(cx: &mut TestAppContext) {
    let place = Place::new();
    let key = "0123456789abcdef0123456789abcdef";
    let session = Session {
        selected_workspace: 0,
        workspaces_made: 1,
        folded_repositories: Vec::new(),
        workspaces: vec![Workspace {
            task: None,
            name: "Workspace 1".into(),
            color: Some(0),
            selected_tab: 0,
            tabs: vec![Tab {
                name: None,
                focused_pane: 0,
                root: Node::Terminal(Terminal {
                    cwd: None,
                    scrollback: Some(key.into()),
                    conversation: None,
                    ..Terminal::default()
                }),
            }],
        }],
    };
    fs::create_dir_all(&place.scrollback).unwrap();
    fs::write(&place.file, session.to_json().unwrap()).unwrap();
    // It would set the title and make the core answer the shell.
    fs::write(place.scrollback.join(key), "old\x1b]0;title\x07\x1b[6n\r\n").unwrap();

    let (window, mut cx) = open(&place, sh("printf prompt; cat"), "", cx);
    wait_for_line(window, &mut cx, "prompt");
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(lines(window, &mut cx), ["prompt"]);
}

#[gpui::test]
fn closing_the_last_pane_removes_what_the_panes_showed(cx: &mut TestAppContext) {
    let place = Place::new();
    let (window, mut cx) = open(&place, sh(OLD), "", cx);
    wait_for_line(window, &mut cx, "second line");
    save_until(&mut cx, "the pane's scrollback is saved", || {
        place.files().len() == 1
    });

    cx.simulate_keystrokes("cmd-w");
    cx.run_until_parked();
    assert!(
        window.update(&mut cx, |_, _, _| ()).is_err(),
        "the window closed"
    );
    assert_eq!(place.saved(), None);
    assert_eq!(place.files(), [""; 0]);
}

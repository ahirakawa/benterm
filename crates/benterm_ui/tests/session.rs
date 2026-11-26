//! The window saves its workspaces, tabs, panes and directories to the
//! session file and makes them again from it. Every test has a session
//! file of its own in a temporary directory.

use std::cell::Cell;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use benterm_layout::PaneId;
use benterm_session::{Axis, Node, Session, Split, Tab, Terminal, Workspace};
use benterm_terminal::PtyConfig;
use benterm_ui::session::{SAVE_INTERVAL, open_session_window};
use benterm_ui::settings::ConfigFeed;
use benterm_ui::{BentermWindow, SpawnTerminal, StartedTerminal, bind_keys};
use gpui::{TestAppContext, VisualTestContext, WindowHandle};

/// Starts a shell that stays, in the directory it is given, or in
/// `home` without one.
fn shells(home: &Path) -> SpawnTerminal {
    let home = home.to_owned();
    Rc::new(move |_, theme, directory, size| {
        let pty = PtyConfig {
            cwd: Some(directory.unwrap_or(&home).to_owned()),
            size,
            ..PtyConfig::command("/bin/sh", ["-c", "sleep 100"])
        };
        StartedTerminal::spawn(pty, theme)
    })
}

/// A temporary directory with its symbolic links resolved, as the kernel
/// reports directories, with the directories `a` and `b` in it, and the
/// place of the session file.
struct Place {
    _dir: tempfile::TempDir,
    root: PathBuf,
    file: PathBuf,
}

impl Place {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        fs::create_dir(root.join("a")).unwrap();
        fs::create_dir(root.join("b")).unwrap();
        let file = root.join("state").join("session.json");
        Self {
            _dir: dir,
            root,
            file,
        }
    }

    /// The directory `name` of the place, or the place itself without a
    /// name, as the session file has directories.
    fn path(&self, name: &str) -> String {
        let path = match name {
            "" => self.root.clone(),
            name => self.root.join(name),
        };
        path.to_str().unwrap().to_owned()
    }

    /// Puts `session` in the session file, as an earlier run would have.
    fn save(&self, session: &Session) {
        fs::create_dir_all(self.file.parent().unwrap()).unwrap();
        fs::write(&self.file, session.to_json().unwrap()).unwrap();
    }

    /// The saved session, without the keys of the panes' scrollback,
    /// which differ from run to run.
    fn saved(&self) -> Option<Session> {
        let text = fs::read(&self.file).ok()?;
        let session = Session::from_json(&text).expect("the session file is whole");
        Some(without_keys(session))
    }
}

fn without_keys(mut session: Session) -> Session {
    fn strip(node: &mut Node) {
        match node {
            Node::Terminal(terminal) => terminal.scrollback = None,
            Node::Browser(_) => {}
            Node::Split(split) => {
                strip(&mut split.first);
                strip(&mut split.second);
            }
        }
    }
    let tabs = session.workspaces.iter_mut().flat_map(|w| &mut w.tabs);
    tabs.for_each(|tab| strip(&mut tab.root));
    session
}

/// Opens a window with the session of `place` and the app's keys. The
/// terminals' worker threads wake the app from outside the test
/// scheduler, which the scheduler has to be told to accept.
fn open(
    place: &Place,
    cx: &mut TestAppContext,
) -> (WindowHandle<BentermWindow>, VisualTestContext) {
    open_with(place, shells(&place.root), cx)
}

fn open_with(
    place: &Place,
    spawn: SpawnTerminal,
    cx: &mut TestAppContext,
) -> (WindowHandle<BentermWindow>, VisualTestContext) {
    cx.executor().allow_parking();
    cx.update(bind_keys);
    let window = cx
        .update(|cx| open_session_window(spawn, ConfigFeed::defaults(), Some(&place.file), cx))
        .unwrap();
    let vcx = VisualTestContext::from_window(window.into(), cx);
    vcx.run_until_parked();
    (window, vcx)
}

/// The session the window would save now, without the keys of the
/// panes' scrollback.
fn session(window: WindowHandle<BentermWindow>, cx: &mut VisualTestContext) -> Session {
    let session = window
        .update(cx, |root, _, cx| root.session(cx))
        .expect("the window is open");
    without_keys(session)
}

fn notice(window: WindowHandle<BentermWindow>, cx: &mut VisualTestContext) -> Option<String> {
    window
        .read_with(cx, |root, _| root.shown_session_notice())
        .unwrap()
}

/// Lets the app run until `done` holds. The shells and the writer run
/// in real time on other threads, hence the polling.
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

/// Lets the time between two saves pass, and waits for the file to hold
/// what the window has now.
fn wait_for_save(place: &Place, window: WindowHandle<BentermWindow>, cx: &mut VisualTestContext) {
    cx.executor().advance_clock(SAVE_INTERVAL);
    wait_until(cx, "the session is saved", |cx| {
        place.saved() == Some(session(window, cx))
    });
}

fn terminal(cwd: String) -> Node {
    Node::Terminal(Terminal {
        cwd: Some(cwd),
        scrollback: None,
        conversation: None,
        ..Terminal::default()
    })
}

fn split(axis: Axis, ratio: f64, first: Node, second: Node) -> Node {
    Node::Split(Split {
        axis,
        ratio,
        first: Box::new(first),
        second: Box::new(second),
    })
}

fn one_pane(cwd: String) -> Session {
    Session {
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
                root: terminal(cwd),
            }],
        }],
    }
}

/// Two workspaces: one with a tab, the other, selected, with two tabs of
/// which the second has three panes.
fn example(place: &Place) -> Session {
    Session {
        selected_workspace: 1,
        workspaces_made: 5,
        folded_repositories: Vec::new(),
        workspaces: vec![
            Workspace {
                task: None,
                name: "api".into(),
                color: Some(3),
                selected_tab: 0,
                tabs: vec![Tab {
                    name: None,
                    focused_pane: 0,
                    root: terminal(place.path("a")),
                }],
            },
            Workspace {
                task: None,
                name: "Workspace 4".into(),
                color: Some(1),
                selected_tab: 1,
                tabs: vec![
                    Tab {
                        name: None,
                        focused_pane: 0,
                        root: terminal(place.path("")),
                    },
                    Tab {
                        name: None,
                        focused_pane: 1,
                        root: split(
                            Axis::Horizontal,
                            0.25,
                            terminal(place.path("a")),
                            split(
                                Axis::Vertical,
                                0.625,
                                terminal(place.path("b")),
                                terminal(place.path("")),
                            ),
                        ),
                    },
                ],
            },
        ],
    }
}

#[gpui::test]
fn without_a_session_file_the_window_starts_with_one_pane(cx: &mut TestAppContext) {
    let place = Place::new();
    let (window, mut cx) = open(&place, cx);
    wait_until(&mut cx, "the shell is in its directory", |cx| {
        session(window, cx) == one_pane(place.path(""))
    });
    assert_eq!(notice(window, &mut cx), None);
}

#[gpui::test]
fn the_saved_session_is_made_again(cx: &mut TestAppContext) {
    let place = Place::new();
    place.save(&example(&place));

    let (window, mut cx) = open(&place, cx);

    // The directories are asked of the kernel, so every shell has
    // started where its pane was.
    wait_until(&mut cx, "the window has the saved session", |cx| {
        session(window, cx) == example(&place)
    });
    assert_eq!(notice(window, &mut cx), None);
    window
        .read_with(&cx, |root, cx| {
            let names: Vec<_> = root
                .workspaces()
                .into_iter()
                .map(|id| root.workspace_name(id).unwrap())
                .collect();
            assert_eq!(names, ["api", "Workspace 4"]);
            assert_eq!(root.tabs().read(cx).tabs().len(), 2);
            let grid = root.grid(cx);
            let grid = grid.read(cx);
            assert_eq!(grid.panes().len(), 3);
            assert_eq!(grid.focused(), grid.panes()[1]);
        })
        .unwrap();
    // The keyboard is in the focused pane of the selected tab.
    window
        .update(&mut cx, |root, window, cx| {
            let focus = root.terminal(cx).read(cx).focus_handle().clone();
            assert!(focus.is_focused(window));
        })
        .unwrap();
}

#[gpui::test]
fn a_restored_session_is_not_written_again_until_it_changes(cx: &mut TestAppContext) {
    let place = Place::new();
    place.save(&example(&place));
    let (window, mut cx) = open(&place, cx);
    wait_until(&mut cx, "the window has the saved session", |cx| {
        session(window, cx) == example(&place)
    });
    // The panes of this session had no keys for their scrollback yet,
    // so it is written once more, with keys.
    let has_keys = |place: &Place| {
        let text = fs::read(&place.file).unwrap();
        let saved = Session::from_json(&text).unwrap();
        !saved.scrollback_keys().is_empty()
    };
    wait_until(&mut cx, "the panes have keys", |cx| {
        cx.executor().advance_clock(SAVE_INTERVAL);
        has_keys(&place)
    });
    let written = fs::metadata(&place.file).unwrap().modified().unwrap();

    for _ in 0..3 {
        cx.executor().advance_clock(SAVE_INTERVAL);
        cx.run_until_parked();
    }
    std::thread::sleep(Duration::from_millis(50));
    cx.run_until_parked();

    assert_eq!(
        fs::metadata(&place.file).unwrap().modified().unwrap(),
        written
    );
}

#[gpui::test]
fn changes_are_saved_after_the_interval(cx: &mut TestAppContext) {
    let place = Place::new();
    let (window, mut cx) = open(&place, cx);
    wait_for_save(&place, window, &mut cx);
    assert_eq!(place.saved().unwrap().workspaces[0].tabs.len(), 1);

    // A split, a tab, a workspace and a name.
    cx.simulate_keystrokes("cmd-d");
    cx.simulate_keystrokes("cmd-t");
    cx.simulate_keystrokes("cmd-n");
    window
        .update(&mut cx, |root, _, cx| {
            let id = root.selected_workspace();
            root.rename_workspace(id, "web", cx).unwrap();
        })
        .unwrap();
    wait_for_save(&place, window, &mut cx);

    let saved = place.saved().unwrap();
    assert_eq!(saved.selected_workspace, 1);
    assert_eq!(saved.workspaces_made, 2);
    assert_eq!(saved.workspaces[1].name, "web");
    let first = &saved.workspaces[0];
    assert_eq!((first.tabs.len(), first.selected_tab), (2, 1));
    let Node::Split(two) = &first.tabs[0].root else {
        panic!("the first tab was split");
    };
    assert_eq!((two.axis, two.ratio), (Axis::Horizontal, 0.5));
    assert_eq!(first.tabs[0].focused_pane, 1);
}

#[gpui::test]
fn a_shell_that_moves_to_another_directory_is_saved_there(cx: &mut TestAppContext) {
    let place = Place::new();
    let moved = place.root.clone();
    // Nothing tells the app that a shell has moved, and this one writes
    // nothing after it has.
    let spawn: SpawnTerminal = Rc::new(move |_, theme, _, size| {
        let pty = PtyConfig {
            cwd: Some(moved.clone()),
            size,
            ..PtyConfig::command("/bin/sh", ["-c", "sleep 0.3; cd b; sleep 100"])
        };
        StartedTerminal::spawn(pty, theme)
    });
    let (window, mut cx) = open_with(&place, spawn, cx);

    wait_until(&mut cx, "the shell has moved", |cx| {
        session(window, cx) == one_pane(place.path("b"))
    });
    wait_for_save(&place, window, &mut cx);
    assert_eq!(place.saved(), Some(one_pane(place.path("b"))));
}

#[gpui::test]
fn a_window_made_from_what_another_saved_has_the_same_session(cx: &mut TestAppContext) {
    let place = Place::new();
    let (window, mut cx) = open(&place, cx);
    cx.simulate_keystrokes("cmd-d");
    cx.simulate_keystrokes("cmd-shift-d");
    cx.simulate_keystrokes("cmd-n");
    cx.simulate_keystrokes("cmd-t");
    wait_until(&mut cx, "the shells are in their directories", |cx| {
        let session = session(window, cx);
        let tabs = session.workspaces.iter().flat_map(|w| &w.tabs);
        tabs.flat_map(|tab| leaves(&tab.root))
            .all(|terminal| terminal.cwd.is_some())
    });
    let before = session(window, &mut cx);
    // Closing the window saves at once, without waiting for the interval.
    assert!(cx.simulate_close());
    assert_eq!(place.saved(), Some(before.clone()));
    window
        .update(&mut cx, |_, window, _| window.remove_window())
        .unwrap();
    cx.run_until_parked();

    let (window, mut cx) = open(&place, &mut cx.cx);
    wait_until(&mut cx, "the new window has the session of the old", |cx| {
        session(window, cx) == before
    });
    assert_eq!(notice(window, &mut cx), None);
}

fn leaves(node: &Node) -> Vec<&Terminal> {
    match node {
        Node::Terminal(terminal) => vec![terminal],
        Node::Browser(_) => Vec::new(),
        Node::Split(split) => {
            let mut all = leaves(&split.first);
            all.extend(leaves(&split.second));
            all
        }
    }
}

#[gpui::test]
fn quitting_saves_the_session_as_it_is(cx: &mut TestAppContext) {
    let place = Place::new();
    let (window, mut cx) = open(&place, cx);
    cx.simulate_keystrokes("cmd-d");
    let before = session(window, &mut cx);
    assert_eq!(place.saved(), None);

    cx.cx.quit();

    let saved = place.saved().expect("quitting saved the session");
    assert_eq!(
        saved.workspaces[0].tabs[0].root,
        before.workspaces[0].tabs[0].root
    );
}

#[gpui::test]
fn closing_the_last_pane_leaves_no_session_to_make_again(cx: &mut TestAppContext) {
    let place = Place::new();
    let (window, mut cx) = open(&place, cx);
    cx.simulate_keystrokes("cmd-d");
    wait_for_save(&place, window, &mut cx);

    cx.simulate_keystrokes("cmd-w");
    cx.simulate_keystrokes("cmd-w");
    cx.run_until_parked();

    assert!(
        window.update(&mut cx, |_, _, _| ()).is_err(),
        "the window closed"
    );
    assert_eq!(place.saved(), None);
    // Nothing brings the session back afterwards.
    cx.executor().advance_clock(SAVE_INTERVAL);
    cx.run_until_parked();
    assert_eq!(place.saved(), None);
}

#[gpui::test]
fn a_directory_that_is_gone_starts_the_shell_where_a_new_one_starts(cx: &mut TestAppContext) {
    let place = Place::new();
    place.save(&one_pane(place.path("gone")));

    let (window, mut cx) = open(&place, cx);

    wait_until(&mut cx, "the shell has started elsewhere", |cx| {
        session(window, cx) == one_pane(place.path(""))
    });
    assert_eq!(notice(window, &mut cx), None);
}

#[gpui::test]
fn a_damaged_file_starts_one_pane_and_the_bar_says_why(cx: &mut TestAppContext) {
    let place = Place::new();
    fs::create_dir_all(place.file.parent().unwrap()).unwrap();
    fs::write(&place.file, "{\"version\":1,\"workspaces\":[{\"na").unwrap();

    let (window, mut cx) = open(&place, cx);

    wait_until(&mut cx, "the shell is in its directory", |cx| {
        session(window, cx) == one_pane(place.path(""))
    });
    let shown = notice(window, &mut cx).expect("the bar says why");
    assert!(
        shown.starts_with("The saved session was not restored, because its file is damaged"),
        "{shown}"
    );
    // The new session takes the place of the damaged file.
    wait_for_save(&place, window, &mut cx);
    assert_eq!(place.saved(), Some(one_pane(place.path(""))));
    let copy = place.file.with_file_name("session.json.corrupt");
    assert_eq!(
        fs::read_to_string(copy).unwrap(),
        "{\"version\":1,\"workspaces\":[{\"na"
    );

    window
        .update(&mut cx, |root, _, cx| root.dismiss_messages(cx))
        .unwrap();
    assert_eq!(notice(window, &mut cx), None);
}

#[gpui::test]
fn a_file_of_a_newer_benterm_starts_one_pane_and_is_never_replaced(cx: &mut TestAppContext) {
    let place = Place::new();
    fs::create_dir_all(place.file.parent().unwrap()).unwrap();
    let newer = r#"{"version":6,"layout":"something else"}"#;
    fs::write(&place.file, newer).unwrap();

    let (window, mut cx) = open(&place, cx);
    wait_until(&mut cx, "the shell is in its directory", |cx| {
        session(window, cx) == one_pane(place.path(""))
    });
    let shown = notice(window, &mut cx).expect("the bar says why");
    assert!(shown.contains("it has version 6"), "{shown}");

    cx.simulate_keystrokes("cmd-d");
    cx.executor().advance_clock(SAVE_INTERVAL);
    cx.run_until_parked();
    // A window that does not save has nothing to do when it closes.
    cx.simulate_close();
    cx.cx.quit();
    assert_eq!(fs::read_to_string(&place.file).unwrap(), newer);
}

#[gpui::test]
fn a_second_window_on_the_same_file_neither_restores_nor_saves(cx: &mut TestAppContext) {
    let place = Place::new();
    place.save(&example(&place));
    let (first, mut first_cx) = open(&place, cx);
    wait_until(&mut first_cx, "the first window has the session", |cx| {
        session(first, cx) == example(&place)
    });

    let (second, mut cx) = open(&place, &mut first_cx.cx);

    wait_until(&mut cx, "the shell is in its directory", |cx| {
        session(second, cx) == one_pane(place.path(""))
    });
    let shown = notice(second, &mut cx).expect("the bar says why");
    assert!(shown.starts_with("Another Benterm is running"), "{shown}");
    cx.simulate_keystrokes("cmd-d");
    cx.executor().advance_clock(SAVE_INTERVAL);
    cx.run_until_parked();
    // A window that does not save has nothing to do when it closes.
    cx.simulate_close();
    assert_eq!(place.saved(), Some(example(&place)));
}

#[gpui::test]
fn a_session_whose_terminals_cannot_all_start_is_kept_for_another_start(cx: &mut TestAppContext) {
    let place = Place::new();
    place.save(&example(&place));
    let before = fs::read(&place.file).unwrap();
    // The third terminal does not start, in its directory or elsewhere.
    let started = Rc::new(Cell::new(0));
    let inner = shells(&place.root);
    let spawn: SpawnTerminal = Rc::new(move |pane, theme, directory, size| {
        if directory.is_some_and(|directory| directory.ends_with("b")) {
            return Err(io::Error::other("no more terminals"));
        }
        if directory.is_none() && started.get() == 0 {
            // Asked again without the directory.
            started.set(1);
            return Err(io::Error::other("no more terminals"));
        }
        inner(pane, theme, directory, size)
    });

    let (window, mut cx) = open_with(&place, spawn, cx);

    wait_until(&mut cx, "the shell is in its directory", |cx| {
        session(window, cx) == one_pane(place.path(""))
    });
    let shown = notice(window, &mut cx).expect("the bar says why");
    assert!(
        shown.starts_with(
            "The saved session was not restored, because a terminal could not be started"
        ),
        "{shown}"
    );
    cx.simulate_keystrokes("cmd-d");
    cx.executor().advance_clock(SAVE_INTERVAL);
    cx.run_until_parked();
    // A window that does not save has nothing to do when it closes.
    cx.simulate_close();
    assert_eq!(fs::read(&place.file).unwrap(), before);
}

#[gpui::test]
fn every_terminal_of_a_restored_session_is_started_with_the_id_of_its_pane(
    cx: &mut TestAppContext,
) {
    let place = Place::new();
    place.save(&example(&place));
    // The ids the terminals were started with, in the order of starting.
    let started = Rc::new(std::cell::RefCell::new(Vec::<PaneId>::new()));
    let inner = shells(&place.root);
    let spawn: SpawnTerminal = Rc::new({
        let started = started.clone();
        move |pane, theme, directory, size| {
            started.borrow_mut().push(pane);
            inner(pane, theme, directory, size)
        }
    });

    let (window, cx) = open_with(&place, spawn, cx);

    let panes = window
        .read_with(&cx, |root, cx| {
            let mut panes = Vec::new();
            for workspace in root.workspaces() {
                let tabs = root.workspace_tabs(workspace).unwrap();
                let tabs = tabs.read(cx);
                for tab in tabs.tabs() {
                    panes.extend(tabs.grid(tab).unwrap().read(cx).panes());
                }
            }
            panes
        })
        .unwrap();
    assert_eq!(panes.len(), 5);
    assert_eq!(*started.borrow(), panes);
}

#[gpui::test]
fn the_window_that_serves_the_socket_is_made_from_the_saved_session_too(cx: &mut TestAppContext) {
    use std::os::unix::fs::PermissionsExt;
    use std::sync::mpsc;

    use benterm_ipc::{Client, PaneId as ApiPaneId, ReadOutput};
    use benterm_ui::ipc::{self, StartTerminal};

    let place = Place::new();
    place.save(&example(&place));
    cx.executor().allow_parking();
    let dir = tempfile::Builder::new()
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap();
    let socket = dir.path().join("benterm.sock");
    // Every shell says which pane it was told it is in, and whether the
    // socket it was told is the app's.
    let script = format!(
        r#"[ "$BENTERM_SOCKET" = '{}' ] && printf 'pane=%s.' "$BENTERM_PANE_ID"; sleep 100"#,
        socket.display()
    );
    let start: StartTerminal = Rc::new(StartedTerminal::spawn);
    let pty = PtyConfig::command("/bin/sh", ["-c", script.as_str()]);
    let session = Some(place.file.clone());
    let feed = ConfigFeed::defaults();

    // This is how the app opens its window.
    let window = cx
        .update(|cx| ipc::open_session_window(pty, start, Some(socket.clone()), session, feed, cx))
        .unwrap();
    let mut cx = VisualTestContext::from_window(window.into(), cx);

    wait_until(&mut cx, "the window has the saved session", |cx| {
        self::session(window, cx) == example(&place)
    });
    let panes = window
        .read_with(&cx, |root, cx| {
            let mut panes = Vec::new();
            for workspace in root.workspaces() {
                let tabs = root.workspace_tabs(workspace).unwrap();
                let tabs = tabs.read(cx);
                for tab in tabs.tabs() {
                    panes.extend(tabs.grid(tab).unwrap().read(cx).panes());
                }
            }
            panes
        })
        .unwrap();
    assert_eq!(panes.len(), 5);
    // Each pane, also one in a tab or a workspace that is not shown, is
    // read over the socket by the id its shell was told.
    for pane in panes {
        let expected = format!("pane={pane}.");
        wait_until(&mut cx, "the pane's shell has said its id", |cx| {
            let (sender, answer) = mpsc::channel();
            let socket = socket.clone();
            std::thread::spawn(move || {
                let request = ReadOutput {
                    pane: ApiPaneId::new(pane.get()),
                    lines: None,
                };
                let read = Client::connect(socket).map(|mut client| client.call(&request));
                let _ = sender.send(read);
            });
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                cx.run_until_parked();
                if let Ok(read) = answer.recv_timeout(Duration::from_millis(5)) {
                    let output = read.unwrap().expect("the pane can be read");
                    return output.text.contains(&expected);
                }
                assert!(Instant::now() < deadline, "no answer from the socket");
            }
        });
    }
}

#[gpui::test]
fn a_workspace_asked_for_by_a_shell_of_a_session_given_up_is_not_made(cx: &mut TestAppContext) {
    use std::os::unix::fs::PermissionsExt;
    use std::sync::mpsc;

    use benterm_ipc::cli::{self, CliEnv};
    use benterm_ipc::{PANE_ID_ENV, SOCKET_ENV};
    use benterm_ui::ipc::{self, StartTerminal};

    let place = Place::new();
    place.save(&example(&place));
    cx.executor().allow_parking();
    let dir = tempfile::Builder::new()
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap();
    let socket = dir.path().join("benterm.sock");
    // The shell of the first pane runs `benterm new-workspace` as soon
    // as it starts. Its call waits in the queue while the session is
    // restored, until a terminal in `b` does not start, in its directory
    // or elsewhere, and the session is given up with the shell.
    let (sender, answer) = mpsc::channel();
    let started = Rc::new(Cell::new(0));
    let start: StartTerminal = Rc::new(move |pty: PtyConfig, theme| {
        let count = started.get();
        started.set(count + 1);
        if pty.cwd.as_ref().is_some_and(|cwd| cwd.ends_with("b")) {
            return Err(io::Error::other("no more terminals"));
        }
        if pty.cwd.is_none() && started.get() < 100 {
            // Asked again without the directory. The one pane the window
            // opens instead is started after this.
            started.set(100);
            return Err(io::Error::other("no more terminals"));
        }
        if count == 0 {
            let env = |name: &str| {
                pty.env
                    .iter()
                    .rev()
                    .find(|(key, _)| key == name)
                    .map(|(_, value)| value.clone())
            };
            let env = CliEnv {
                socket: env(SOCKET_ENV),
                pane: env(PANE_ID_ENV).map(|pane| pane.to_string_lossy().into_owned()),
                cwd: None,
                path: None,
                ..CliEnv::default()
            };
            let sender = sender.clone();
            std::thread::spawn(move || {
                let mut stdout = Vec::new();
                let mut stderr = Vec::new();
                let args = ["new-workspace".to_owned()];
                let code = cli::run(&args, &env, io::empty(), &mut stdout, &mut stderr);
                let _ = sender.send((code, String::from_utf8(stderr).unwrap()));
            });
        }
        StartedTerminal::spawn(pty, theme)
    });
    let pty = PtyConfig::command("/bin/sh", ["-c", "sleep 100"]);
    let session = Some(place.file.clone());
    let feed = ConfigFeed::defaults();

    let window = cx
        .update(|cx| ipc::open_session_window(pty, start, Some(socket), session, feed, cx))
        .unwrap();
    let mut cx = VisualTestContext::from_window(window.into(), cx);

    let deadline = Instant::now() + Duration::from_secs(10);
    let (code, stderr) = loop {
        cx.run_until_parked();
        if let Ok(answer) = answer.recv_timeout(Duration::from_millis(5)) {
            break answer;
        }
        assert!(Instant::now() < deadline, "no answer from the socket");
    };
    assert_eq!(code, 1, "{stderr}");
    assert!(stderr.contains("no pane"), "{stderr}");
    let shown = notice(window, &mut cx).expect("the bar says why");
    assert!(
        shown.starts_with("The saved session was not restored"),
        "{shown}"
    );
    let workspaces = window.read_with(&cx, |root, _| root.workspaces()).unwrap();
    assert_eq!(workspaces.len(), 1);
}

#[gpui::test]
fn a_window_kept_for_a_child_saves_it_and_leaves_no_session_when_it_closes(
    cx: &mut TestAppContext,
) {
    use std::os::unix::fs::PermissionsExt;
    use std::sync::mpsc;

    use benterm_ipc::{Client, PaneId as ApiPaneId, SendToAgent, SpawnAgent};
    use benterm_ui::ipc::{self, StartTerminal};

    let place = Place::new();
    cx.executor().allow_parking();
    cx.update(bind_keys);
    let dir = tempfile::Builder::new()
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap();
    let socket = dir.path().join("benterm.sock");
    let start: StartTerminal = Rc::new(StartedTerminal::spawn);
    let pty = PtyConfig::command("/bin/sh", ["-c", "sleep 100"]);
    let window = cx
        .update(|cx| {
            let session = Some(place.file.clone());
            let feed = ConfigFeed::defaults();
            ipc::open_session_window(pty, start, Some(socket.clone()), session, feed, cx)
        })
        .unwrap();
    let mut cx = VisualTestContext::from_window(window.into(), cx);
    cx.run_until_parked();
    let call = |cx: &mut VisualTestContext, request: SpawnAgent| {
        let (sender, answer) = mpsc::channel();
        let socket = socket.clone();
        std::thread::spawn(move || {
            let result = Client::connect(socket).map(|mut client| client.call(&request));
            let _ = sender.send(result);
        });
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            cx.run_until_parked();
            if let Ok(result) = answer.recv_timeout(Duration::from_millis(5)) {
                return result.unwrap();
            }
            assert!(Instant::now() < deadline, "no answer from the socket");
        }
    };
    let parent = window
        .update(&mut cx, |root, _, cx| root.grid(cx).read(cx).focused())
        .unwrap();
    let child = call(
        &mut cx,
        SpawnAgent {
            name: "w1".into(),
            command: vec!["/bin/sh".into(), "-c".into(), "read line".into()],
            parent: Some(ApiPaneId::new(parent.get())),
            cwd: Some(place.path("a")),
            outlive_parent: true,
            ..SpawnAgent::default()
        },
    )
    .expect("the agent starts")
    .pane;

    cx.simulate_keystrokes("cmd-w");
    wait_until(&mut cx, "the child takes the pane", |cx| {
        window
            .update(cx, |root, _, cx| root.grid(cx).read(cx).panes())
            .is_ok_and(|panes| panes.len() == 1 && panes[0].get() == child.get())
    });

    // The surviving pane keeps its launch restriction in the session.
    wait_for_save(&place, window, &mut cx);
    let mut expected = one_pane(place.path("a"));
    let Node::Terminal(terminal) = &mut expected.workspaces[0].tabs[0].root else {
        unreachable!("the session has one terminal");
    };
    terminal.restricted = Some(benterm_session::RestrictedPane::default());
    assert_eq!(place.saved(), Some(expected));

    // The child ends, and with it the window and the session.
    let (sender, answer) = mpsc::channel();
    let socket = socket.clone();
    std::thread::spawn(move || {
        let request = SendToAgent {
            agent: benterm_ipc::AgentTarget::Pane(child),
            text: "x\r".into(),
            caller: None,
        };
        let result = Client::connect(socket).map(|mut client| client.call(&request));
        let _ = sender.send(result);
    });
    wait_until(&mut cx, "the window closes", |cx| {
        let _ = answer.try_recv();
        window.update(cx, |_, _, _| ()).is_err()
    });
    assert_eq!(place.saved(), None);
}

const PAGE: &str = "http://127.0.0.1:8080/docs?page=2";

/// One tab: a shell at the left, and at the right a browser above
/// another shell. The browser is the focused pane.
fn with_a_browser(place: &Place) -> Session {
    let mut session = one_pane(place.path("a"));
    session.workspaces[0].tabs[0] = Tab {
        name: None,
        focused_pane: 1,
        root: split(
            Axis::Horizontal,
            0.4,
            terminal(place.path("a")),
            split(
                Axis::Vertical,
                0.5,
                Node::Browser(benterm_session::Browser::at(PAGE)),
                terminal(place.path("b")),
            ),
        ),
    };
    session
}

#[gpui::test]
fn a_browser_pane_is_made_again_at_its_address_in_its_place(cx: &mut TestAppContext) {
    let place = Place::new();
    place.save(&with_a_browser(&place));

    let (window, mut cx) = open(&place, cx);

    wait_until(&mut cx, "the window has the saved session", |cx| {
        session(window, cx) == with_a_browser(&place)
    });
    assert_eq!(notice(window, &mut cx), None);
    window
        .update(&mut cx, |root, window, cx| {
            let grid = root.grid(cx);
            let grid = grid.read(cx);
            let panes = grid.panes();
            assert_eq!(panes.len(), 3);
            assert!(grid.terminal(panes[0]).is_some());
            assert!(grid.terminal(panes[1]).is_none());
            assert!(grid.terminal(panes[2]).is_some());
            let browser = grid.browser(panes[1]).expect("the pane is a browser");
            assert_eq!(browser.read(cx).url().as_deref(), Some(PAGE));
            // The keyboard is in the focused pane, which is the browser.
            assert_eq!(grid.focused(), panes[1]);
            assert!(browser.read(cx).focus_handle().is_focused(window));
        })
        .unwrap();
}

#[gpui::test]
fn a_browser_opened_in_the_window_is_saved_with_its_address(cx: &mut TestAppContext) {
    let place = Place::new();
    let (window, mut cx) = open(&place, cx);
    wait_until(&mut cx, "the shell is in its directory", |cx| {
        session(window, cx) == one_pane(place.path(""))
    });

    window
        .update(&mut cx, |root, window, cx| {
            let direction = benterm_layout::SplitDirection::Right;
            root.open_browser(Some(PAGE), None, direction, false, window, cx)
                .unwrap();
        })
        .unwrap();
    wait_for_save(&place, window, &mut cx);

    let saved = place.saved().unwrap();
    let Node::Split(split) = &saved.workspaces[0].tabs[0].root else {
        panic!("the tab has two panes");
    };
    assert_eq!(*split.first, terminal(place.path("")));
    assert_eq!(
        *split.second,
        Node::Browser(benterm_session::Browser::at(PAGE))
    );
}

#[gpui::test]
fn a_tab_comes_back_with_its_name(cx: &mut TestAppContext) {
    let place = Place::new();
    let mut saved = example(&place);
    saved.workspaces[1].tabs[1].name = Some("review".into());
    place.save(&saved);

    let (window, mut cx) = open(&place, cx);

    // What the window would save has the name too.
    wait_until(&mut cx, "the window has the saved session", |cx| {
        session(window, cx) == saved
    });
    let labels = window
        .read_with(&cx, |root, cx| {
            let tabs = root.tabs().read(cx);
            let ids = tabs.tabs();
            ids.iter().map(|id| tabs.label(*id, cx)).collect::<Vec<_>>()
        })
        .unwrap();
    assert_eq!(labels[1], "review");
    assert_ne!(labels[0], "review");
}

#[gpui::test]
fn a_zoomed_pane_is_not_saved_as_zoomed_and_restores_the_original_split(cx: &mut TestAppContext) {
    let place = Place::new();
    let (window, mut vcx) = open(&place, cx);
    wait_until(&mut vcx, "the shell is in its directory", |vcx| {
        session(window, vcx) == one_pane(place.path(""))
    });

    vcx.simulate_keystrokes("cmd-d");
    vcx.simulate_keystrokes("ctrl-cmd-right");
    vcx.run_until_parked();

    let expected_session = session(window, &mut vcx);
    let Node::Split(expected_split) = &expected_session.workspaces[0].tabs[0].root else {
        panic!("expected a split tab");
    };
    assert!(expected_split.ratio > 0.5);

    // Zoom the focused pane before saving the session.
    vcx.simulate_keystrokes("cmd-shift-enter");
    vcx.run_until_parked();
    let is_zoomed = window
        .read_with(&vcx, |root, cx| root.grid(cx).read(cx).zoomed().is_some())
        .unwrap();
    assert!(is_zoomed);

    wait_for_save(&place, window, &mut vcx);
    let saved = place.saved().unwrap();
    assert_eq!(saved, expected_session);

    // Reopening from the saved session restores the unzoomed split layout.
    let _ = window.update(&mut vcx, |_, window, _| window.remove_window());
    vcx.run_until_parked();
    let (restored, mut vcx) = open(&place, cx);
    let (panes, zoomed, bounds) = restored
        .read_with(&vcx, |root, cx| {
            let grid = root.grid(cx);
            let grid = grid.read(cx);
            (grid.panes().len(), grid.zoomed(), grid.pane_bounds().len())
        })
        .unwrap();
    assert_eq!(panes, 2);
    assert_eq!(zoomed, None);
    assert_eq!(bounds, 2);
    assert_eq!(session(restored, &mut vcx), expected_session);
}

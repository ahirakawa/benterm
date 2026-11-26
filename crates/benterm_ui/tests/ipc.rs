//! The socket API against a real window: requests come in over a real
//! socket, and the panes run real programs.

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use benterm_ipc::{
    Client, ClientError, CreateWorkspace, ErrorCode, Method, PaneId as ApiPaneId, ReadOutput,
    SendText, SplitDirection, SplitPane,
};
use benterm_layout::{PaneId, WorkspaceId};
use benterm_terminal::PtyConfig;
use benterm_ui::settings::ConfigFeed;
use benterm_ui::{BentermWindow, PaneGrid, StartedTerminal, bind_keys, ipc};
use gpui::{Entity, TestAppContext, VisualTestContext, WindowHandle};

const TIMEOUT: Duration = Duration::from_secs(10);

#[path = "common/pages.rs"]
mod pages;

fn sh(script: &str) -> PtyConfig {
    PtyConfig::command("/bin/sh", ["-c", script])
}

/// A window with the socket API on a socket in a private directory.
struct Api {
    window: WindowHandle<BentermWindow>,
    cx: VisualTestContext,
    socket: PathBuf,
    _dir: tempfile::TempDir,
}

/// Opens a window whose panes all run `script` and that serves the socket
/// API. The terminals' worker threads and the socket's reader threads
/// wake the app from outside the test scheduler, which the scheduler has
/// to be told to accept.
fn open(script: &str, cx: &mut TestAppContext) -> Api {
    cx.executor().allow_parking();
    cx.update(bind_keys);
    let dir = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap();
    let socket = dir.path().join("benterm.sock");
    let start: ipc::StartTerminal = Rc::new(StartedTerminal::spawn);
    let window = cx
        .update(|cx| {
            ipc::open_window(
                sh(script),
                start,
                Some(socket.clone()),
                ConfigFeed::defaults(),
                cx,
            )
        })
        .unwrap();
    let vcx = VisualTestContext::from_window(window.into(), cx);
    vcx.run_until_parked();
    Api {
        window,
        cx: vcx,
        socket,
        _dir: dir,
    }
}

impl Api {
    fn grid(&mut self) -> Entity<PaneGrid> {
        self.window
            .update(&mut self.cx, |root, _, cx| root.grid(cx))
            .expect("the window is open")
    }

    fn focused(&mut self) -> PaneId {
        let grid = self.grid();
        grid.read_with(&self.cx, |grid, _| grid.focused())
    }

    /// Makes one call from a thread of its own, as a program in a pane
    /// would, and lets the app run until the answer is there.
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

    /// What `pane.read` gives for a pane.
    fn read(&mut self, pane: PaneId) -> String {
        let request = ReadOutput {
            pane: api_id(pane),
            lines: None,
        };
        self.call(request).expect("the pane can be read").text
    }

    fn send(&mut self, pane: PaneId, text: &str) {
        let request = SendText {
            pane: api_id(pane),
            text: text.into(),
        };
        self.call(request).expect("the pane takes text");
    }

    fn split(&mut self, pane: PaneId, direction: SplitDirection) -> Result<PaneId, ClientError> {
        let request = SplitPane {
            pane: api_id(pane),
            direction,
        };
        let created = self.call(request)?;
        Ok(self.pane_with_id(created.pane.get()))
    }

    /// The pane of the window that has the number `id`, in any workspace
    /// and tab.
    fn pane_with_id(&mut self, id: u64) -> PaneId {
        self.all_panes()
            .into_iter()
            .find(|pane| pane.get() == id)
            .unwrap_or_else(|| panic!("the window has no pane {id}"))
    }

    fn all_panes(&mut self) -> Vec<PaneId> {
        self.window
            .update(&mut self.cx, |root, _, cx| {
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
            .expect("the window is open")
    }

    fn workspaces(&mut self) -> Vec<WorkspaceId> {
        self.window
            .update(&mut self.cx, |root, _, _| root.workspaces())
            .expect("the window is open")
    }

    fn selected_workspace(&mut self) -> WorkspaceId {
        self.window
            .update(&mut self.cx, |root, _, _| root.selected_workspace())
            .expect("the window is open")
    }

    /// Reads a pane over the socket until `done` holds for its text.
    fn wait_for_text(&mut self, pane: PaneId, what: &str, done: impl Fn(&str) -> bool) -> String {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            let text = self.read(pane);
            if done(&text) {
                return text;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {what}; the pane shows {text:?}"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

fn api_id(pane: PaneId) -> ApiPaneId {
    ApiPaneId::new(pane.get())
}

fn code<T: std::fmt::Debug>(result: Result<T, ClientError>) -> ErrorCode {
    match result {
        Err(ClientError::Api(error)) => error.code,
        other => panic!("expected an error from the app, got {other:?}"),
    }
}

/// Puts `> ` before every line it reads, so its output differs from what
/// was typed, and does not echo.
const ANSWERS: &str = r#"stty -echo; echo ready; while read line; do echo "> $line"; done"#;

/// Prints the pane id and whether the socket variable names a socket.
const SHOWS_ITS_PANE: &str =
    r#"[ -S "$BENTERM_SOCKET" ] && printf 'pane=%s.' "$BENTERM_PANE_ID"; sleep 100"#;

#[gpui::test]
fn the_first_pane_is_told_the_socket_and_its_own_id(cx: &mut TestAppContext) {
    let mut api = open(SHOWS_ITS_PANE, cx);
    let first = api.focused();
    let text = api.wait_for_text(first, "the pane id", |text| text.contains("pane="));
    assert_eq!(text, format!("pane={first}."));
}

#[gpui::test]
fn the_socket_variable_is_the_path_of_the_socket(cx: &mut TestAppContext) {
    let mut api = open(r#"printf 'socket=%s.' "$BENTERM_SOCKET"; sleep 100"#, cx);
    let first = api.focused();
    let text = api.wait_for_text(first, "the socket path", |text| text.contains('.'));
    assert_eq!(text, format!("socket={}.", api.socket.display()));
}

#[gpui::test]
fn panes_of_splits_tabs_and_workspaces_are_told_their_own_ids(cx: &mut TestAppContext) {
    let mut api = open(SHOWS_ITS_PANE, cx);
    let mut seen = vec![api.focused()];
    for keys in ["cmd-d", "cmd-shift-d", "cmd-t", "cmd-n"] {
        api.cx.simulate_keystrokes(keys);
        let pane = api.focused();
        assert!(!seen.contains(&pane), "{keys} made a pane");
        let text = api.wait_for_text(pane, "the pane id", |text| text.contains("pane="));
        assert_eq!(text, format!("pane={pane}."), "after {keys}");
        seen.push(pane);
    }
}

#[gpui::test]
fn text_sent_to_a_pane_reaches_its_program(cx: &mut TestAppContext) {
    let mut api = open(ANSWERS, cx);
    let pane = api.focused();
    api.wait_for_text(pane, "the program", |text| text.contains("ready"));
    api.send(pane, "hello\r");
    let text = api.wait_for_text(pane, "the answer", |text| text.contains("> "));
    assert_eq!(text, "ready\n> hello");
}

#[gpui::test]
fn text_is_sent_as_it_is_without_pressing_enter(cx: &mut TestAppContext) {
    let mut api = open(ANSWERS, cx);
    let pane = api.focused();
    api.wait_for_text(pane, "the program", |text| text.contains("ready"));
    api.send(pane, "one ");
    api.send(pane, "two");
    api.send(pane, "");
    api.send(pane, "\r");
    let text = api.wait_for_text(pane, "the answer", |text| text.contains("> "));
    assert_eq!(text, "ready\n> one two");
}

#[gpui::test]
fn a_pane_in_a_tab_that_is_not_shown_is_reached(cx: &mut TestAppContext) {
    let mut api = open(ANSWERS, cx);
    let hidden = api.focused();
    api.wait_for_text(hidden, "the program", |text| text.contains("ready"));
    api.cx.simulate_keystrokes("cmd-t");
    api.cx.simulate_keystrokes("cmd-n");
    assert_ne!(api.focused(), hidden);
    api.send(hidden, "over here\r");
    api.wait_for_text(hidden, "the answer", |text| text.contains("> over here"));
}

#[gpui::test]
fn a_pane_that_does_not_exist_is_unknown(cx: &mut TestAppContext) {
    let mut api = open("sleep 100", cx);
    let missing = ApiPaneId::new(u64::MAX);
    let read = api.call(ReadOutput {
        pane: missing,
        lines: None,
    });
    let send = api.call(SendText {
        pane: missing,
        text: "x".into(),
    });
    let split = api.call(SplitPane {
        pane: missing,
        direction: SplitDirection::Right,
    });
    assert_eq!(code(read), ErrorCode::UnknownPane);
    assert_eq!(code(send), ErrorCode::UnknownPane);
    assert_eq!(code(split), ErrorCode::UnknownPane);
}

#[gpui::test]
fn a_pane_that_closed_is_unknown(cx: &mut TestAppContext) {
    let mut api = open("stty -echo; read line", cx);
    let first = api.focused();
    api.cx.simulate_keystrokes("cmd-d");
    assert_ne!(api.focused(), first);
    // The program of the first pane exits, and its pane closes.
    api.send(first, "\r");
    let deadline = Instant::now() + TIMEOUT;
    while api.all_panes().contains(&first) {
        assert!(Instant::now() < deadline, "the pane is still open");
        api.cx.run_until_parked();
        std::thread::sleep(Duration::from_millis(5));
    }
    let read = api.call(ReadOutput {
        pane: api_id(first),
        lines: None,
    });
    assert_eq!(code(read), ErrorCode::UnknownPane);
}

#[gpui::test]
fn the_last_lines_of_a_pane_can_be_read(cx: &mut TestAppContext) {
    let mut api = open("seq 1 200; sleep 100", cx);
    let pane = api.focused();
    api.wait_for_text(pane, "the output", |text| text.ends_with("200"));
    let request = ReadOutput {
        pane: api_id(pane),
        lines: Some(3),
    };
    assert_eq!(api.call(request).unwrap().text, "198\n199\n200");
    let request = ReadOutput {
        pane: api_id(pane),
        lines: Some(1000),
    };
    let all = api.call(request).unwrap().text;
    assert!(all.starts_with("1\n2\n3\n"), "{all:?}");
}

#[gpui::test]
fn splitting_a_pane_starts_a_program_that_knows_its_pane(cx: &mut TestAppContext) {
    let mut api = open(SHOWS_ITS_PANE, cx);
    let first = api.focused();
    let right = api.split(first, SplitDirection::Right).unwrap();
    let below = api.split(first, SplitDirection::Down).unwrap();
    let grid = api.grid();
    let panes = grid.read_with(&api.cx, |grid, _| grid.panes());
    // The pane that was split keeps the top left, the one below it comes
    // next in the tree, and the first split has the right half.
    assert_eq!(panes, [first, below, right]);
    for pane in [right, below] {
        let text = api.wait_for_text(pane, "the pane id", |text| text.contains("pane="));
        assert_eq!(text, format!("pane={pane}."));
    }
}

#[gpui::test]
fn a_split_from_the_socket_leaves_the_keyboard_where_it_is(cx: &mut TestAppContext) {
    let mut api = open(ANSWERS, cx);
    let first = api.focused();
    api.wait_for_text(first, "the program", |text| text.contains("ready"));
    let second = api.split(first, SplitDirection::Right).unwrap();
    assert_eq!(api.focused(), first);
    api.cx.simulate_input("typed");
    api.cx.simulate_keystrokes("enter");
    api.wait_for_text(first, "the typed line", |text| text.contains("> typed"));
    assert!(!api.read(second).contains("typed"));
}

#[gpui::test]
fn a_tab_too_small_for_another_pane_refuses_the_split(cx: &mut TestAppContext) {
    let mut api = open("sleep 100", cx);
    let first = api.focused();
    let mut refused = None;
    let mut made = 0;
    for _ in 0..40 {
        let before = api.all_panes().len();
        match api.split(first, SplitDirection::Right) {
            Ok(_) => {
                assert_eq!(api.all_panes().len(), before + 1);
                made += 1;
            }
            Err(error) => {
                assert_eq!(
                    api.all_panes().len(),
                    before,
                    "the refused split left no pane"
                );
                refused = Some(error);
                break;
            }
        }
    }
    assert!(
        made >= 1,
        "a window of the usual size has room for two panes"
    );
    let refused = refused.expect("a split is refused in the end");
    assert_eq!(code::<()>(Err(refused)), ErrorCode::Failed);
}

#[gpui::test]
fn a_pane_that_is_not_shown_is_split_for_the_size_the_window_has_now(cx: &mut TestAppContext) {
    let mut api = open("sleep 100", cx);
    let created = api.call(CreateWorkspace::default()).unwrap();
    let hidden = api.pane_with_id(created.pane.get());
    // The window has room for the split, and the workspace, which was
    // never shown, learns the size from the one on view.
    let second = api.split(hidden, SplitDirection::Right).unwrap();

    // The window shrinks until no pane can be split to the right. The
    // workspace is still not shown, and must not go by the old size.
    api.cx
        .simulate_resize(gpui::size(gpui::px(300.), gpui::px(400.)));
    api.cx.run_until_parked();
    let shown = api.focused();
    let on_view = api.split(shown, SplitDirection::Right);
    assert_eq!(code(on_view), ErrorCode::Failed, "the window is too narrow");
    let not_shown = api.split(second, SplitDirection::Right);
    assert_eq!(code(not_shown), ErrorCode::Failed);
}

#[gpui::test]
fn a_call_made_as_the_window_closes_is_answered(cx: &mut TestAppContext) {
    let mut api = open("sleep 100", cx);
    let pane = api.focused();
    // The call is made while the app does not run, so it waits in the
    // queue, or arrives after the window has gone. Either way it gets an
    // answer, and the answer is not a pane's text.
    let (sender, answer) = mpsc::channel();
    let socket = api.socket.clone();
    std::thread::spawn(move || {
        let request = ReadOutput {
            pane: api_id(pane),
            lines: None,
        };
        let result = Client::connect(socket).unwrap().call(&request);
        let _ = sender.send(result);
    });
    std::thread::sleep(Duration::from_millis(50));
    api.window
        .update(&mut api.cx, |_, window, _| window.remove_window())
        .unwrap();
    let deadline = Instant::now() + TIMEOUT;
    let result = loop {
        api.cx.run_until_parked();
        if let Ok(result) = answer.recv_timeout(Duration::from_millis(5)) {
            break result;
        }
        assert!(Instant::now() < deadline, "no answer before the timeout");
    };
    assert_eq!(code(result), ErrorCode::Failed);
}

#[gpui::test]
fn a_workspace_is_made_with_a_name_and_a_working_directory(cx: &mut TestAppContext) {
    let mut api = open("pwd; sleep 100", cx);
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().canonicalize().unwrap();
    let selected = api.selected_workspace();
    let focused = api.focused();

    let created = api
        .call(CreateWorkspace {
            name: Some("agents".into()),
            cwd: Some(cwd.to_str().unwrap().into()),
            caller: None,
        })
        .unwrap();

    let workspaces = api.workspaces();
    assert_eq!(workspaces.len(), 2);
    assert_eq!(workspaces[1].get(), created.workspace.get());
    let name = api
        .window
        .update(&mut api.cx, |root, _, _| root.workspace_name(workspaces[1]))
        .unwrap();
    assert_eq!(name.as_deref(), Some("agents"));
    // The selection and the keyboard stay where they are.
    assert_eq!(api.selected_workspace(), selected);
    assert_eq!(api.focused(), focused);
    // The first pane, and one split off it, start in the directory.
    let pane = api.pane_with_id(created.pane.get());
    let shown = cwd.to_str().unwrap().to_owned();
    api.wait_for_text(pane, "the directory", |text| {
        text.replace('\n', "") == shown
    });
    let split = api.split(pane, SplitDirection::Down).unwrap();
    api.wait_for_text(split, "the directory", |text| {
        text.replace('\n', "") == shown
    });
}

#[gpui::test]
fn a_workspace_without_a_name_is_numbered(cx: &mut TestAppContext) {
    let mut api = open("sleep 100", cx);
    let created = api.call(CreateWorkspace::default()).unwrap();
    let workspaces = api.workspaces();
    assert_eq!(workspaces[1].get(), created.workspace.get());
    let name = api
        .window
        .update(&mut api.cx, |root, _, _| root.workspace_name(workspaces[1]))
        .unwrap();
    assert_eq!(name.as_deref(), Some("Workspace 2"));
}

#[gpui::test]
fn a_workspace_asked_for_from_a_pane_of_the_window_is_made(cx: &mut TestAppContext) {
    let mut api = open("sleep 100", cx);
    let pane = api.focused();

    let created = api
        .call(CreateWorkspace {
            caller: Some(api_id(pane)),
            ..CreateWorkspace::default()
        })
        .unwrap();

    let workspaces = api.workspaces();
    assert_eq!(workspaces.len(), 2);
    assert_eq!(workspaces[1].get(), created.workspace.get());
}

#[gpui::test]
fn a_workspace_asked_for_from_a_pane_the_window_does_not_have_is_refused(cx: &mut TestAppContext) {
    let mut api = open("sleep 100", cx);
    // Pane ids are never given twice, so no pane will have this one.
    let gone = ApiPaneId::new(u64::MAX);

    let result = api.call(CreateWorkspace {
        caller: Some(gone),
        ..CreateWorkspace::default()
    });

    assert_eq!(code(result), ErrorCode::UnknownPane);
    assert_eq!(api.workspaces().len(), 1);
}

#[gpui::test]
fn a_working_directory_must_be_an_absolute_path_to_a_directory(cx: &mut TestAppContext) {
    let mut api = open("sleep 100", cx);
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("missing");
    for cwd in ["relative/dir", ".", "", missing.to_str().unwrap()] {
        let result = api.call(CreateWorkspace {
            name: None,
            cwd: Some(cwd.into()),
            caller: None,
        });
        assert_eq!(code(result), ErrorCode::InvalidParams, "cwd {cwd:?}");
    }
    assert_eq!(api.workspaces().len(), 1);
}

#[gpui::test]
fn calls_are_refused_once_the_window_has_closed(cx: &mut TestAppContext) {
    let mut api = open("sleep 100", cx);
    let pane = api.focused();
    api.window
        .update(&mut api.cx, |_, window, _| window.remove_window())
        .unwrap();
    api.cx.run_until_parked();
    for _ in 0..2 {
        let read = api.call(ReadOutput {
            pane: api_id(pane),
            lines: None,
        });
        assert_eq!(code(read), ErrorCode::Failed);
    }
}

#[gpui::test]
fn a_call_the_window_gets_to_after_its_time_has_run_out_is_not_carried_out(
    cx: &mut TestAppContext,
) {
    use std::io::{BufRead, BufReader, Write};

    let mut api = open(ANSWERS, cx);
    let pane = api.focused();
    api.wait_for_text(pane, "the program", |text| text.contains("ready"));

    // The call arrives while the app does not run, as when its window has
    // stopped, and its time runs out before the app gets to it.
    let (sender, answer) = mpsc::channel();
    let socket = api.socket.clone();
    let id = pane.get();
    std::thread::spawn(move || {
        let mut stream = std::os::unix::net::UnixStream::connect(socket).unwrap();
        let request = format!(
            r#"{{"id":1,"method":"pane.send_text","params":{{"pane":{id},"text":"late\r"}},"timeout_ms":50}}"#
        );
        stream.write_all(request.as_bytes()).unwrap();
        stream.write_all(b"\n").unwrap();
        let mut line = String::new();
        BufReader::new(stream).read_line(&mut line).unwrap();
        let _ = sender.send(line);
    });
    std::thread::sleep(Duration::from_millis(200));

    let deadline = Instant::now() + TIMEOUT;
    let line = loop {
        api.cx.run_until_parked();
        if let Ok(line) = answer.recv_timeout(Duration::from_millis(5)) {
            break line;
        }
        assert!(Instant::now() < deadline, "no answer before the timeout");
    };
    assert!(line.contains(r#""code":"expired""#), "{line}");

    // The text was never typed: what is typed next is all the program gets.
    api.send(pane, "now\r");
    let text = api.wait_for_text(pane, "the answer", |text| text.contains("> "));
    assert_eq!(text, "ready\n> now");
}

#[gpui::test]
fn opening_the_window_removes_sockets_crashed_apps_left_behind(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    let dir = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap();
    // No process has this id: it is above what macOS and Linux give out.
    let dead = dir.path().join(format!("{}.sock", i32::MAX));
    drop(std::os::unix::net::UnixListener::bind(&dead).unwrap());
    // A pane another test starts holds the listener open until its program
    // runs. Once refused, the socket stays dead.
    let deadline = Instant::now() + TIMEOUT;
    while std::os::unix::net::UnixStream::connect(&dead).is_ok() {
        assert!(Instant::now() < deadline, "the socket kept answering");
        std::thread::sleep(Duration::from_millis(5));
    }
    let socket = dir.path().join(format!("{}.sock", std::process::id()));
    let start: ipc::StartTerminal = Rc::new(StartedTerminal::spawn);

    cx.update(|cx| {
        ipc::open_window(
            sh("sleep 100"),
            start,
            Some(socket.clone()),
            ConfigFeed::defaults(),
            cx,
        )
    })
    .unwrap();

    assert!(!dead.exists(), "the dead socket is still there");
    std::os::unix::net::UnixStream::connect(&socket).unwrap();
}

/// Opens a window whose socket is to be in a directory nobody may write
/// to, so the app cannot listen on it. Returns the window and the path.
fn open_without_socket(
    script: &str,
    cx: &mut TestAppContext,
) -> (
    WindowHandle<BentermWindow>,
    VisualTestContext,
    PathBuf,
    tempfile::TempDir,
) {
    cx.executor().allow_parking();
    cx.update(bind_keys);
    let dir = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap();
    let closed = dir.path().join("closed");
    std::fs::create_dir(&closed).unwrap();
    std::fs::set_permissions(&closed, std::fs::Permissions::from_mode(0o500)).unwrap();
    let socket = closed.join("benterm.sock");
    let start: ipc::StartTerminal = Rc::new(StartedTerminal::spawn);
    let window = cx
        .update(|cx| {
            ipc::open_window(
                sh(script),
                start,
                Some(socket.clone()),
                ConfigFeed::defaults(),
                cx,
            )
        })
        .unwrap();
    let vcx = VisualTestContext::from_window(window.into(), cx);
    vcx.run_until_parked();
    (window, vcx, socket, dir)
}

#[gpui::test]
fn a_socket_that_cannot_be_opened_is_shown_in_the_bar(cx: &mut TestAppContext) {
    let (window, mut vcx, socket, _dir) = open_without_socket("sleep 100", cx);
    let notice = window
        .read_with(&vcx, |root, _| root.shown_socket_notice())
        .unwrap()
        .expect("the bar tells that the socket could not be opened");
    assert!(
        notice.contains(&socket.display().to_string()),
        "the bar names the path: {notice}"
    );
    assert!(
        notice.to_lowercase().contains("permission denied"),
        "the bar gives the reason: {notice}"
    );
    window
        .update(&mut vcx, |root, _, cx| root.dismiss_messages(cx))
        .unwrap();
    let shown = window
        .read_with(&vcx, |root, _| root.shown_socket_notice())
        .unwrap();
    assert_eq!(shown, None, "closing the bar closes the notice");
}

#[gpui::test]
fn a_window_without_its_socket_still_runs_its_panes(cx: &mut TestAppContext) {
    let (window, mut vcx, _socket, _dir) = open_without_socket(
        r#"printf 'socket=[%s] pane=%s.' "$BENTERM_SOCKET" "$BENTERM_PANE_ID"; sleep 100"#,
        cx,
    );
    let grid = window
        .update(&mut vcx, |root, _, cx| root.grid(cx))
        .unwrap();
    let pane = grid.read_with(&vcx, |grid, _| grid.focused());
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let text = grid.read_with(&vcx, |grid, cx| {
            grid.terminal(pane).map(|view| view.read(cx).text(None))
        });
        if let Some(text) = text.filter(|text| text.contains('.')) {
            assert_eq!(text.trim_end(), format!("socket=[] pane={pane}."));
            break;
        }
        assert!(Instant::now() < deadline, "the pane printed nothing");
        vcx.run_until_parked();
        std::thread::sleep(Duration::from_millis(10));
    }
}

const PAGE: &str = "http://127.0.0.1:8080/docs";

fn browse(
    api: &mut Api,
    url: &str,
    pane: Option<PaneId>,
    direction: SplitDirection,
) -> Result<PaneId, ClientError> {
    let request = benterm_ipc::OpenBrowser {
        url: url.into(),
        pane: pane.map(api_id),
        direction,
    };
    let created = api.call(request)?;
    Ok(api.pane_with_id(created.pane.get()))
}

#[gpui::test]
fn a_browser_opened_over_the_socket_adds_a_pane_that_loads_the_address(cx: &mut TestAppContext) {
    let pages = cx.update(pages::Pages::install);
    let mut api = open("sleep 100", cx);
    let first = api.focused();

    let new = browse(&mut api, PAGE, Some(first), SplitDirection::Right).unwrap();

    assert_eq!(api.all_panes(), [first, new]);
    assert_eq!(pages.len(), 1);
    assert_eq!(pages.page(0).borrow().loads, [PAGE]);
    let grid = api.grid();
    assert!(grid.read_with(&api.cx, |grid, _| grid.browser(new).is_some()));
    // A program opened it: the keyboard stays where it is.
    assert_eq!(api.focused(), first);
    assert!(!pages.page(0).borrow().has_keyboard());
}

#[gpui::test]
fn a_browser_opened_without_a_pane_goes_beside_the_focused_one(cx: &mut TestAppContext) {
    let pages = cx.update(pages::Pages::install);
    let mut api = open("sleep 100", cx);
    let first = api.focused();
    api.cx.simulate_keystrokes("cmd-d");
    let second = api.focused();

    let new = browse(&mut api, PAGE, None, SplitDirection::Down).unwrap();

    assert_eq!(api.all_panes(), [first, second, new]);
    assert_eq!(pages.len(), 1);
}

#[gpui::test]
fn a_browser_is_not_opened_for_what_is_no_web_page_or_no_pane(cx: &mut TestAppContext) {
    let pages = cx.update(pages::Pages::install);
    let mut api = open("sleep 100", cx);
    let first = api.focused();

    for url in [
        "file:///etc/passwd",
        "javascript:alert(1)",
        "example.com",
        "",
    ] {
        let refused = browse(&mut api, url, Some(first), SplitDirection::Right);
        assert_eq!(code(refused), ErrorCode::InvalidParams, "{url:?}");
    }
    let request = benterm_ipc::OpenBrowser {
        url: PAGE.into(),
        pane: Some(ApiPaneId::new(u64::MAX)),
        direction: SplitDirection::Right,
    };
    assert_eq!(code(api.call(request)), ErrorCode::UnknownPane);

    assert_eq!(api.all_panes(), [first]);
    assert_eq!(pages.len(), 0);
}

/// The requests that are for a terminal say so of a browser pane, and
/// the list of panes, which is of terminals, leaves it out.
#[gpui::test]
fn a_browser_pane_has_no_terminal_to_read_or_type_into(cx: &mut TestAppContext) {
    cx.update(pages::Pages::install);
    let mut api = open("sleep 100", cx);
    let first = api.focused();
    let new = browse(&mut api, PAGE, Some(first), SplitDirection::Right).unwrap();

    let read = api.call(ReadOutput {
        pane: api_id(new),
        lines: None,
    });
    let send = api.call(SendText {
        pane: api_id(new),
        text: "x".into(),
    });
    assert_eq!(code(read), ErrorCode::Failed);
    assert_eq!(code(send), ErrorCode::Failed);
    let listed = api.call(benterm_ipc::ListPanes {}).unwrap();
    let ids: Vec<_> = listed.panes.iter().map(|pane| pane.pane).collect();
    assert_eq!(ids, [api_id(first)]);

    // A terminal is split off it like off any pane.
    let shell = api.split(new, SplitDirection::Down).unwrap();
    assert_eq!(api.all_panes(), [first, new, shell]);
}

#[gpui::test]
fn send_text_refuses_with_input_queue_full_when_the_program_does_not_read(cx: &mut TestAppContext) {
    let mut api = open("stty -echo; printf ready; sleep 100", cx);
    let pane = api.focused();
    api.wait_for_text(pane, "ready", |text| text.contains("ready"));

    // One write of the full queue limit is accepted and blocks in the
    // writer thread while the child sleeps without reading.
    let full = "x".repeat(benterm_terminal::WRITE_QUEUE_LIMIT);
    api.call(SendText {
        pane: api_id(pane),
        text: full,
    })
    .expect("a write up to the limit fits");

    // Any further write is refused with InputQueueFull and a clear reason.
    let err = api
        .call(SendText {
            pane: api_id(pane),
            text: "more".into(),
        })
        .expect_err("a write past the limit must be refused");
    match err {
        ClientError::Api(error) => {
            assert_eq!(error.code, ErrorCode::InputQueueFull);
            assert!(
                error.message.contains("full") || error.message.contains("reading"),
                "unexpected message: {}",
                error.message
            );
        }
        other => panic!("expected Api error, got {other:?}"),
    }
}

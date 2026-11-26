//! Who a notification says it is from. One of an agent behind the window,
//! started with `agent run`, names the agent by the name it was started
//! under and the folder it works in, as it has no workspace or tab. One
//! of an agent in a pane says what it said before. The window is a real
//! one and the programs are real; the agents' states come from made-up
//! hook events over the real socket.

use std::cell::RefCell;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use benterm_ipc::{
    Client, ClientError, Method, PaneId as ApiPaneId, ReadOutput, ReportHook, SpawnAgent,
};
use benterm_layout::PaneId;
use benterm_terminal::PtyConfig;
use benterm_ui::attention::Notifier;
use benterm_ui::settings::ConfigFeed;
use benterm_ui::{BentermWindow, StartedTerminal, bind_keys, ipc};
use gpui::{App, SystemNotification, TestAppContext, VisualTestContext, WindowHandle};

const TIMEOUT: Duration = Duration::from_secs(10);

/// What every terminal runs: says `ready` and waits, echoing nothing.
const SCRIPT: &str = r#"stty -echo; printf ready:; while IFS= read -r line; do :; done"#;

/// Keeps what it was asked to post.
#[derive(Clone, Default)]
struct FakeNotifier(Rc<RefCell<Vec<SystemNotification>>>);

impl Notifier for FakeNotifier {
    fn show(&self, notification: SystemNotification, _: &mut App) {
        self.0.borrow_mut().push(notification);
    }
}

struct Benterm {
    window: WindowHandle<BentermWindow>,
    cx: VisualTestContext,
    socket: PathBuf,
    sent: FakeNotifier,
    dir: tempfile::TempDir,
}

/// Opens an active window as the app does, with a socket of its own and
/// a fake notifier.
fn open(cx: &mut TestAppContext) -> Benterm {
    cx.executor().allow_parking();
    cx.update(bind_keys);
    let dir = tempfile::Builder::new()
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap();
    let socket = dir.path().join("benterm.sock");
    let start: ipc::StartTerminal = Rc::new(StartedTerminal::spawn);
    let pty = PtyConfig::command("/bin/sh", ["-c", SCRIPT]);
    let feed = ConfigFeed::defaults();
    let window = cx
        .update(|cx| ipc::open_window(pty, start, Some(socket.clone()), feed, cx))
        .unwrap();
    let mut cx = VisualTestContext::from_window(window.into(), cx);
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();
    let sent = FakeNotifier::default();
    let notifier = Rc::new(sent.clone());
    window
        .update(&mut cx, |root, _, _| root.set_notifier(notifier))
        .unwrap();
    Benterm {
        window,
        cx,
        socket,
        sent,
        dir,
    }
}

impl Benterm {
    fn root<T>(&mut self, read: impl FnOnce(&BentermWindow, &App) -> T) -> T {
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
            self.cx.executor().advance_clock(Duration::from_millis(5));
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

    /// Has the agent of `pane` start a turn and finish it, and waits for
    /// its notification.
    fn work_and_finish(&mut self, pane: PaneId) {
        self.hook(pane, "UserPromptSubmit");
        self.wait_until("the row", |benterm| {
            benterm
                .root(|root, cx| root.session_rows(cx))
                .iter()
                .any(|row| row.pane == pane)
        });
        let before = self.sent.0.borrow().len();
        self.hook(pane, "Stop");
        self.wait_until("the notification", |benterm| {
            benterm.sent.0.borrow().len() > before
        });
    }

    /// Starts the agent `name` behind the window in `cwd`, as a child of
    /// `parent`, and returns the id of its terminal once its program
    /// runs.
    fn spawn_behind(&mut self, name: &str, cwd: &Path, parent: PaneId) -> PaneId {
        let request = SpawnAgent {
            name: name.into(),
            command: vec!["/bin/sh".into(), "-c".into(), SCRIPT.into()],
            parent: Some(ApiPaneId::new(parent.get())),
            cwd: Some(cwd.to_string_lossy().into_owned()),
            ..SpawnAgent::default()
        };
        let number = self.call(request).expect("the agent starts").pane.get();
        let pane = self.root(|root, cx| {
            root.agents(cx)
                .into_iter()
                .find(|agent| agent.pane.get() == number)
                .unwrap()
                .pane
        });
        self.wait_for_ready(pane);
        pane
    }

    fn wait_for_ready(&mut self, pane: PaneId) {
        self.wait_until("the program", |benterm| {
            let pane = ApiPaneId::new(pane.get());
            let screen = benterm.call(ReadOutput { pane, lines: None }).unwrap().text;
            screen.contains("ready:")
        });
    }

    fn focused(&mut self) -> PaneId {
        self.root(|root, cx| root.grid(cx).read(cx).focused())
    }

    /// The folder name of the directory the terminal with the id `pane`
    /// works in, as the app finds it: `~` for the home folder.
    fn folder_of(&mut self, pane: PaneId) -> String {
        let directory = self.root(|root, cx| {
            let view = root.terminal_with_id(pane, cx).expect("the terminal");
            view.read(cx).working_directory().expect("its directory")
        });
        let home = std::env::var_os("HOME").map(PathBuf::from);
        if home.as_deref() == Some(directory.as_path()) {
            return "~".into();
        }
        directory
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned()
    }

    fn toast_source(&mut self, pane: PaneId) -> String {
        let toasts = self.root(|root, cx| root.shown_toasts(cx));
        toasts
            .into_iter()
            .find(|toast| toast.pane == pane)
            .expect("the notification in the corner")
            .source
    }

    fn last_sent(&self) -> SystemNotification {
        self.sent
            .0
            .borrow()
            .last()
            .cloned()
            .expect("a notification")
    }

    /// A folder for an agent to work in, named `name`.
    fn folder(&self, name: &str) -> PathBuf {
        let folder = self.dir.path().join(name);
        fs::create_dir(&folder).unwrap();
        folder.canonicalize().unwrap()
    }
}

#[gpui::test]
fn the_notification_in_the_corner_of_an_agent_behind_the_window_names_it_and_its_folder(
    cx: &mut TestAppContext,
) {
    let mut benterm = open(cx);
    let pane = benterm.focused();
    benterm.wait_for_ready(pane);
    let folder = benterm.folder("issue-145");
    let w1 = benterm.spawn_behind("w1", &folder, pane);

    benterm.work_and_finish(w1);

    assert_eq!(benterm.toast_source(w1), "Claude Code · w1 · issue-145");
}

#[gpui::test]
fn the_macos_notification_of_an_agent_behind_the_window_names_it_and_its_folder(
    cx: &mut TestAppContext,
) {
    let mut benterm = open(cx);
    let pane = benterm.focused();
    benterm.wait_for_ready(pane);
    let folder = benterm.folder("issue-145");
    let w1 = benterm.spawn_behind("w1", &folder, pane);

    benterm.work_and_finish(w1);

    let sent = benterm.last_sent();
    assert_eq!(sent.title.as_ref(), "Claude Code finished");
    assert_eq!(sent.body.as_ref(), "w1 · issue-145");
}

#[gpui::test]
fn the_notifications_of_an_agent_in_a_pane_say_what_they_said_before(cx: &mut TestAppContext) {
    let mut benterm = open(cx);
    let agent = benterm.focused();
    benterm.wait_for_ready(agent);
    let folder = benterm.folder_of(agent);
    let (workspace, tab) = benterm.root(|root, cx| {
        let workspace = root.selected_workspace();
        let name = root.workspace_name(workspace).unwrap_or_default();
        let tabs = root.workspace_tabs(workspace).unwrap();
        let tabs = tabs.read(cx);
        (name, tabs.label(tabs.selected(), cx))
    });
    // Another tab in front, so that the user does not look at the agent.
    benterm.cx.simulate_keystrokes("cmd-t");
    let front = benterm.focused();
    assert_ne!(front, agent);
    benterm.wait_for_ready(front);

    benterm.work_and_finish(agent);

    assert_eq!(
        benterm.toast_source(agent),
        format!("Claude Code · {folder}")
    );
    let sent = benterm.last_sent();
    assert_eq!(sent.title.as_ref(), "Claude Code finished");
    assert_eq!(sent.body.as_ref(), format!("{workspace} › {tab}"));
}

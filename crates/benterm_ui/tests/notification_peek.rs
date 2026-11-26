//! A click on a notification, in the corner of the window or from macOS,
//! of an agent behind the window opens its peek, as it has no pane to go
//! to. One of an agent in a pane goes to the pane, as before. The window
//! is a real one and the programs are real; the agents' states come from
//! made-up hook events over the real socket.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use benterm_ipc::{
    Client, ClientError, Method, PaneId as ApiPaneId, ReadOutput, ReportHook, SpawnAgent,
};
use benterm_layout::PaneId;
use benterm_terminal::PtyConfig;
use benterm_ui::attention;
use benterm_ui::settings::ConfigFeed;
use benterm_ui::{BentermWindow, StartedTerminal, bind_keys, ipc};
use gpui::{
    Modifiers, SystemNotificationResponse, TestAppContext, VisualTestContext, WindowHandle,
};

const TIMEOUT: Duration = Duration::from_secs(10);

/// What every terminal runs: says `ready` and waits, echoing nothing.
const SCRIPT: &str = r#"stty -echo; printf ready:; while IFS= read -r line; do :; done"#;

struct Benterm {
    window: WindowHandle<BentermWindow>,
    cx: VisualTestContext,
    socket: PathBuf,
    _dir: tempfile::TempDir,
}

/// Opens an active window as the app does, with a socket of its own.
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
    Benterm {
        window,
        cx,
        socket,
        _dir: dir,
    }
}

impl Benterm {
    fn root<T>(&mut self, read: impl FnOnce(&BentermWindow, &gpui::App) -> T) -> T {
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

    /// Makes the program of `pane` an agent at work, which gives it a
    /// row in the session list.
    fn work_in(&mut self, pane: PaneId) {
        self.hook(pane, "UserPromptSubmit");
        self.wait_until("the row", |benterm| {
            benterm
                .root(|root, cx| root.session_rows(cx))
                .iter()
                .any(|row| row.pane == pane)
        });
    }

    /// Has the agent of `pane` finish its turn, and waits for its mark.
    fn finish_turn(&mut self, pane: PaneId) {
        self.hook(pane, "Stop");
        self.wait_until("the mark", |benterm| benterm.marked_rows().contains(&pane));
    }

    /// Starts the agent `name` behind the window, as a child of
    /// `parent`, and returns the id of its terminal once it has a row
    /// and its program runs.
    fn spawn_behind(&mut self, name: &str, parent: PaneId) -> PaneId {
        let request = SpawnAgent {
            name: name.into(),
            command: vec!["/bin/sh".into(), "-c".into(), SCRIPT.into()],
            parent: Some(ApiPaneId::new(parent.get())),
            ..SpawnAgent::default()
        };
        let number = self.call(request).expect("the agent starts").pane.get();
        self.wait_until("the row", |benterm| {
            benterm
                .root(|root, cx| root.session_rows(cx))
                .iter()
                .any(|row| row.pane.get() == number)
        });
        let rows = self.root(|root, cx| root.session_rows(cx));
        let pane = rows
            .iter()
            .find(|row| row.pane.get() == number)
            .unwrap()
            .pane;
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

    fn selected_tab(&mut self) -> benterm_layout::TabId {
        self.root(|root, cx| root.tabs().read(cx).selected())
    }

    fn peeked(&mut self) -> Option<PaneId> {
        self.root(|root, _| root.peeked())
    }

    fn terminal_has_keyboard(&mut self, pane: PaneId) -> bool {
        let handle = self.root(|root, cx| {
            let view = root.terminal_with_id(pane, cx).expect("the terminal");
            view.read(cx).focus_handle().clone()
        });
        self.cx.update(|window, _| handle.is_focused(window))
    }

    fn marked_rows(&mut self) -> Vec<PaneId> {
        let rows = self.root(|root, cx| root.session_rows(cx));
        rows.into_iter()
            .filter(|row| row.marked)
            .map(|row| row.pane)
            .collect()
    }

    fn toast_panes(&mut self) -> Vec<PaneId> {
        let toasts = self.root(|root, cx| root.shown_toasts(cx));
        toasts.into_iter().map(|toast| toast.pane).collect()
    }

    fn draw(&mut self) {
        self.cx.run_until_parked();
        let _ = self
            .window
            .update(&mut self.cx, |_, window, _| window.refresh());
        self.cx.run_until_parked();
    }

    /// Clicks the notification in the corner, the only one shown.
    fn click_toast(&mut self) {
        self.draw();
        let bounds = self.cx.debug_bounds("toast-0").expect("the notification");
        self.cx.simulate_click(bounds.center(), Modifiers::none());
        self.cx.run_until_parked();
    }

    /// Clicks the macOS notification of `pane`, as macOS hands the click
    /// to the app.
    fn click_system_notification(&mut self, pane: PaneId) {
        self.cx.update(|_, cx| attention::answer_notifications(cx));
        self.cx
            .simulate_system_notification_response(SystemNotificationResponse {
                tag: attention::tag_of(pane).into(),
                action_id: None,
            });
        self.cx.run_until_parked();
    }

    fn window_is_active(&mut self) -> bool {
        self.cx.update(|window, _| window.is_window_active())
    }

    /// An agent in the pane of the first tab, and a second tab in front
    /// of it whose pane has the keyboard. Returns the two panes.
    fn agent_in_a_tab_behind(&mut self) -> (PaneId, PaneId) {
        let agent = self.focused();
        self.wait_for_ready(agent);
        self.work_in(agent);
        self.cx.simulate_keystrokes("cmd-t");
        let front = self.focused();
        assert_ne!(front, agent);
        self.wait_for_ready(front);
        (agent, front)
    }
}

#[gpui::test]
fn clicking_the_notification_of_an_agent_behind_the_window_opens_its_peek(cx: &mut TestAppContext) {
    let mut benterm = open(cx);
    let pane = benterm.focused();
    let tab = benterm.selected_tab();
    let w1 = benterm.spawn_behind("w1", pane);
    benterm.work_in(w1);
    benterm.finish_turn(w1);
    assert_eq!(benterm.toast_panes(), vec![w1]);

    benterm.click_toast();

    assert_eq!(benterm.peeked(), Some(w1));
    assert!(benterm.terminal_has_keyboard(w1));
    assert_eq!(
        benterm.toast_panes(),
        Vec::<PaneId>::new(),
        "the clicked one goes"
    );
    assert_eq!(benterm.marked_rows(), Vec::<PaneId>::new());
    assert_eq!(benterm.selected_tab(), tab);
    assert_eq!(benterm.focused(), pane);
}

#[gpui::test]
fn clicking_the_macos_notification_of_an_agent_behind_the_window_opens_its_peek(
    cx: &mut TestAppContext,
) {
    let mut benterm = open(cx);
    let pane = benterm.focused();
    let w1 = benterm.spawn_behind("w1", pane);
    benterm.work_in(w1);
    benterm.cx.deactivate_window();
    benterm.finish_turn(w1);

    benterm.click_system_notification(w1);

    assert!(benterm.window_is_active());
    assert_eq!(benterm.peeked(), Some(w1));
    assert!(benterm.terminal_has_keyboard(w1));
    assert_eq!(benterm.marked_rows(), Vec::<PaneId>::new());
}

#[gpui::test]
fn clicking_the_notification_of_an_agent_in_a_pane_goes_to_the_pane_and_opens_no_peek(
    cx: &mut TestAppContext,
) {
    let mut benterm = open(cx);
    let (agent, _) = benterm.agent_in_a_tab_behind();
    benterm.finish_turn(agent);

    benterm.click_toast();

    assert_eq!(benterm.peeked(), None);
    assert_eq!(benterm.focused(), agent);
    assert_eq!(benterm.marked_rows(), Vec::<PaneId>::new());
}

#[gpui::test]
fn clicking_the_macos_notification_of_an_agent_in_a_pane_goes_to_the_pane_and_opens_no_peek(
    cx: &mut TestAppContext,
) {
    let mut benterm = open(cx);
    let (agent, _) = benterm.agent_in_a_tab_behind();
    benterm.cx.deactivate_window();
    benterm.finish_turn(agent);

    benterm.click_system_notification(agent);

    assert!(benterm.window_is_active());
    assert_eq!(benterm.peeked(), None);
    assert_eq!(benterm.focused(), agent);
    assert_eq!(benterm.marked_rows(), Vec::<PaneId>::new());
}

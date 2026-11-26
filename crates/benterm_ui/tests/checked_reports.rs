//! The reports the Stop hook checked: the app keeps the last one of
//! each pane, and its panes are told to check while it checks. The reports are made up.

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use benterm_ipc::{
    AgentTarget, Client, ClientError, ErrorCode, KillAgent, Method, PaneId as ApiPaneId,
    ReadOutput, ReportChecked, ReportStatus, SplitDirection, SplitPane,
};
use benterm_layout::PaneId;
use benterm_terminal::PtyConfig;
use benterm_ui::settings::ConfigFeed;
use benterm_ui::{BentermWindow, StartedTerminal, bind_keys, ipc};
use gpui::{TestAppContext, VisualTestContext, WindowHandle};
use serde_json::json;

const TIMEOUT: Duration = Duration::from_secs(10);

/// A window with the socket API on a socket in a private directory.
struct Api {
    window: WindowHandle<BentermWindow>,
    cx: VisualTestContext,
    socket: PathBuf,
    _dir: tempfile::TempDir,
}

/// Opens a window whose panes all run `script`, with the settings of
/// `config`.
fn open(script: &str, config: &str, cx: &mut TestAppContext) -> Api {
    open_with(script, config, true, cx)
}

/// Like [`open`], with or without the socket API. The programs start
/// from an environment of the test's own, not the one the test runs in.
fn open_with(script: &str, config: &str, with_socket: bool, cx: &mut TestAppContext) -> Api {
    cx.executor().allow_parking();
    cx.update(bind_keys);
    let dir = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap();
    let socket = dir.path().join("benterm.sock");
    let start: ipc::StartTerminal = Rc::new(StartedTerminal::spawn);
    let feed = ConfigFeed::new(benterm_config::parse(config).unwrap());
    let mut pty = PtyConfig::command("/bin/sh", ["-c", script]);
    pty.base_env = Some(std::sync::Arc::new(vec![(
        "PATH".into(),
        "/usr/bin:/bin".into(),
    )]));
    let listen = with_socket.then(|| socket.clone());
    let window = cx
        .update(|cx| ipc::open_window(pty, start, listen, feed, cx))
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
    fn focused(&mut self) -> PaneId {
        self.window
            .update(&mut self.cx, |root, _, cx| root.grid(cx).read(cx).focused())
            .unwrap()
    }

    fn pane_with_id(&mut self, id: u64) -> PaneId {
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
            .unwrap()
            .into_iter()
            .find(|pane| pane.get() == id)
            .unwrap_or_else(|| panic!("the window has no pane {id}"))
    }

    fn split(&mut self, pane: PaneId) -> PaneId {
        let split = SplitPane {
            pane: ApiPaneId::new(pane.get()),
            direction: SplitDirection::Right,
        };
        let created = self.call(split).unwrap();
        self.pane_with_id(created.pane.get())
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

    /// Waits until the pane shows `needle`, and gives what it shows.
    fn read_until(&mut self, pane: PaneId, needle: &str) -> String {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            let request = ReadOutput {
                pane: ApiPaneId::new(pane.get()),
                lines: None,
            };
            let text = self.call(request).expect("the pane can be read").text;
            if text.contains(needle) {
                return text;
            }
            assert!(Instant::now() < deadline, "the pane shows {text:?}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Waits until the screen of the pane shows `needle`, without the
    /// socket, and gives what it shows.
    fn screen_until(&mut self, pane: PaneId, needle: &str) -> String {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            self.cx.run_until_parked();
            let text = self
                .window
                .update(&mut self.cx, |root, _, cx| {
                    let terminal = root.grid(cx).read(cx).terminal(pane).cloned().unwrap();
                    terminal.read(cx).text(None)
                })
                .unwrap();
            if text.contains(needle) {
                return text;
            }
            assert!(Instant::now() < deadline, "the pane shows {text:?}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// The title and the problems of the report kept for `pane`.
    fn kept(&mut self, pane: PaneId) -> Option<(String, Vec<String>)> {
        self.window
            .update(&mut self.cx, |root, _, _| {
                root.checked_report(pane)
                    .map(|kept| (kept.report.title.clone(), kept.problems.clone()))
            })
            .unwrap()
    }
}

/// A made-up report that passes, under `title`.
fn checked(pane: PaneId, title: &str, problems: &[&str]) -> ReportChecked {
    checked_for(pane.get(), title, problems)
}

fn checked_for(pane: u64, title: &str, problems: &[&str]) -> ReportChecked {
    let report = json!({
        "title": title,
        "goal": "To have a made-up thing",
        "conclusion": "issues",
        "summary": "The made-up thing is there.",
        "decisions": [{
            "question": "Keep the old one?",
            "options": [
                {"label": "Keep", "effect": "Both stay."},
                {"label": "Drop", "effect": "Only the new one stays."},
            ],
            "recommend": "Drop",
            "reason": "Nothing uses the old one.",
        }],
        "verified": [],
        "unverified": ["Other platforms"],
        "deliverables": [{"path": "docs/plan.md", "summary": "The plan."}],
    });
    ReportChecked {
        pane: ApiPaneId::new(pane),
        agent: "claude".into(),
        session_id: Some("made-up-session".into()),
        report: serde_json::from_value(report).unwrap(),
        problems: problems.iter().map(|problem| problem.to_string()).collect(),
    }
}

#[gpui::test]
fn a_checked_report_is_kept_for_its_pane(cx: &mut TestAppContext) {
    let mut api = open("cat", "", cx);
    let first = api.focused();
    let second = api.split(first);
    assert_eq!(api.kept(first), None);

    api.call(checked(first, "First work", &[])).unwrap();

    assert_eq!(api.kept(first), Some(("First work".into(), vec![])));
    assert_eq!(api.kept(second), None, "a report is its pane's alone");
    let kept = api
        .window
        .update(&mut api.cx, |root, _, _| {
            root.checked_report(first).cloned()
        })
        .unwrap()
        .unwrap();
    assert_eq!(kept.agent, "claude");
    assert_eq!(kept.session_id.as_deref(), Some("made-up-session"));
    assert_eq!(kept.report.status(), Some(ReportStatus::Issues));
    assert_eq!(kept.report.decisions[0].question, "Keep the old one?");
    assert!(kept.passed());
}

#[gpui::test]
fn the_last_report_of_a_pane_takes_the_place_of_the_one_before(cx: &mut TestAppContext) {
    let mut api = open("cat", "", cx);
    let pane = api.focused();
    api.call(checked(pane, "First work", &[])).unwrap();

    api.call(checked(pane, "Second work", &["title is too plain."]))
        .unwrap();

    let problems = vec!["title is too plain.".to_owned()];
    assert_eq!(api.kept(pane), Some(("Second work".into(), problems)));
    let passed = api
        .window
        .update(&mut api.cx, |root, _, _| {
            root.checked_report(pane).unwrap().passed()
        })
        .unwrap();
    assert!(!passed, "a report with problems left did not pass");
}

#[gpui::test]
fn a_report_for_a_pane_the_app_does_not_have_is_refused(cx: &mut TestAppContext) {
    let mut api = open("cat", "", cx);
    let pane = api.focused();
    let stray = pane.get() + 1000;

    let error = api
        .call(checked_for(stray, "Nobody's work", &[]))
        .unwrap_err();

    match error {
        ClientError::Api(error) => assert_eq!(error.code, ErrorCode::UnknownPane),
        other => panic!("{other:?}"),
    }
    assert_eq!(api.kept(pane), None);
}

#[gpui::test]
fn a_report_goes_with_its_pane(cx: &mut TestAppContext) {
    let mut api = open("cat", "", cx);
    let first = api.focused();
    let second = api.split(first);
    api.call(checked(first, "First work", &[])).unwrap();
    api.call(checked(second, "Second work", &[])).unwrap();

    let kill = KillAgent {
        agent: AgentTarget::Pane(ApiPaneId::new(second.get())),
    };
    api.call(kill).unwrap();
    api.cx.run_until_parked();

    assert_eq!(api.kept(second), None);
    assert_eq!(api.kept(first), Some(("First work".into(), vec![])));
}

const SHOW_MODE: &str = "echo \"check=[${BENTERM_REPORT_CHECK-unset}]\"; cat";

/// A window whose panes show the variable, started from an environment
/// that does not have it.
fn open_showing(config: &str, socket: bool, cx: &mut TestAppContext) -> (Api, String) {
    let mut api = open_with(SHOW_MODE, config, socket, cx);
    let pane = api.focused();
    let shown = if socket {
        api.read_until(pane, "check=[")
    } else {
        api.screen_until(pane, "check=[")
    };
    (api, shown)
}

/// While the app checks reports, the programs of its panes are told so.
#[gpui::test]
fn the_panes_check_while_the_app_checks_reports(cx: &mut TestAppContext) {
    let (_api, shown) = open_showing("[report]\nenabled = true\n", true, cx);
    assert!(shown.contains("check=[on]"), "{shown:?}");
}

#[gpui::test]
fn the_panes_do_not_check_while_the_app_does_not(cx: &mut TestAppContext) {
    let (_api, shown) = open_showing("", true, cx);
    assert!(shown.contains("check=[unset]"), "{shown:?}");
}

/// Without its socket the app can take no report and its command does
/// nothing.
#[gpui::test]
fn the_panes_of_an_app_without_a_socket_do_not_check(cx: &mut TestAppContext) {
    let (_api, shown) = open_showing("[report]\nenabled = true\n", false, cx);
    assert!(shown.contains("check=[unset]"), "{shown:?}");
}

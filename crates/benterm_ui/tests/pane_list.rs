//! `pane.list` against a real window: the panes of every workspace and
//! tab, and the terminals behind the window, with real programs in them.

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use benterm_ipc::{
    AgentActivity, Client, ClientError, CreateWorkspace, ListPanes, Method, PaneId, PaneInfo,
    SpawnAgent, SplitDirection, SplitPane,
};
use benterm_terminal::PtyConfig;
use benterm_ui::settings::ConfigFeed;
use benterm_ui::{BentermWindow, StartedTerminal, bind_keys, ipc};
use gpui::{TestAppContext, VisualTestContext, WindowHandle};

const TIMEOUT: Duration = Duration::from_secs(10);

struct Api {
    window: WindowHandle<BentermWindow>,
    cx: VisualTestContext,
    socket: PathBuf,
    dir: tempfile::TempDir,
}

fn open(cx: &mut TestAppContext) -> Api {
    // The terminals' and the socket's threads wake the app from outside
    // the test scheduler.
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
                PtyConfig::command("/bin/sh", ["-c", "sleep 100"]),
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
        dir,
    }
}

impl Api {
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

    fn first_pane(&mut self) -> PaneId {
        let id = self
            .window
            .update(&mut self.cx, |root, _, cx| root.grid(cx).read(cx).focused())
            .unwrap();
        PaneId::new(id.get())
    }

    fn list(&mut self) -> Vec<PaneInfo> {
        self.call(ListPanes {})
            .expect("the app lists its panes")
            .panes
    }
}

#[gpui::test]
fn the_list_has_every_pane_of_every_workspace_then_the_terminals_behind(cx: &mut TestAppContext) {
    let mut api = open(cx);
    let cwd = api.dir.path().canonicalize().unwrap();
    let cwd_text = cwd.to_str().unwrap().to_owned();
    let first = api.first_pane();
    let beside = api
        .call(SplitPane {
            pane: first,
            direction: SplitDirection::Right,
        })
        .unwrap()
        .pane;
    let other = api
        .call(CreateWorkspace {
            cwd: Some(cwd_text.clone()),
            ..CreateWorkspace::default()
        })
        .unwrap();
    let behind = api
        .call(SpawnAgent {
            name: "w1".into(),
            command: vec!["/bin/sh".into(), "-c".into(), "sleep 100".into()],
            cwd: Some(cwd_text.clone()),
            ..SpawnAgent::default()
        })
        .unwrap()
        .pane;

    let panes = api.list();

    let ids: Vec<PaneId> = panes.iter().map(|pane| pane.pane).collect();
    assert_eq!(ids, [first, beside, other.pane, behind]);
    let workspace = panes[0].workspace.expect("a pane has a workspace");
    let places: Vec<_> = panes
        .iter()
        .map(|pane| (pane.workspace, pane.tab))
        .collect();
    assert_eq!(
        places,
        [
            (Some(workspace), Some(1)),
            (Some(workspace), Some(1)),
            (Some(other.workspace), Some(1)),
            (None, None),
        ]
    );
    let names: Vec<_> = panes.iter().map(|pane| pane.name.as_deref()).collect();
    assert_eq!(names, [None, None, None, Some("w1")]);
    assert_eq!(panes[2].cwd.as_deref(), Some(cwd_text.as_str()));
    assert_eq!(panes[3].cwd.as_deref(), Some(cwd_text.as_str()));
    assert!(
        panes
            .iter()
            .all(|pane| pane.agent.is_none() && pane.state == AgentActivity::Unknown),
        "{panes:?}"
    );
}

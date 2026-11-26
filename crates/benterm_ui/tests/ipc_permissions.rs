//! The credentials the app gives its terminals, with requests checked
//! (`[ipc] restricted`, the default): a pane of the window may use the
//! whole API, an agent started over the socket only its own pane's hooks,
//! its report and its peers, unless it was started with `control`.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use benterm_agent::HookEvent;
use benterm_ipc::{
    AgentTarget, Client, ClientError, ErrorCode, ListAgents, Method, PaneId as ApiPaneId,
    ReadOutput, SendText, SendToAgent, ShowAgent, SpawnAgent,
};
use benterm_session::{Node, Session};
use benterm_terminal::PtyConfig;
use benterm_ui::settings::ConfigFeed;
use benterm_ui::{BentermWindow, StartedTerminal, bind_keys, ipc};
use gpui::{TestAppContext, VisualTestContext, WindowHandle};

const TIMEOUT: Duration = Duration::from_secs(10);

/// Prints the names of the credentials the program was given, then waits.
const NAMES: &str = r#"for v in BENTERM_PANE_TOKEN BENTERM_CONTROL_TOKEN; do eval "[ -n \"\$$v\" ]" && echo "has $v"; done; echo listed; exec sleep 60"#;

struct App {
    window: WindowHandle<BentermWindow>,
    cx: VisualTestContext,
    socket: PathBuf,
    pane: u64,
    _dir: tempfile::TempDir,
}

fn open(cx: &mut TestAppContext) -> App {
    open_running(NAMES, cx)
}

/// Like [`open`], with panes that run `script`.
fn open_running(script: &str, cx: &mut TestAppContext) -> App {
    open_saving(script, false, cx)
}

fn open_saving(script: &str, save: bool, cx: &mut TestAppContext) -> App {
    cx.executor().allow_parking();
    cx.update(bind_keys);
    let dir = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap();
    let socket = dir.path().join("benterm.sock");
    let session = save.then(|| dir.path().join("session.json"));
    let (window, mut vcx) = open_at(
        cx,
        PtyConfig::command("/bin/sh", ["-c", script]),
        &socket,
        session.as_deref(),
    );
    let pane = window
        .update(&mut vcx, |root, _, cx| root.grid(cx).read(cx).focused())
        .unwrap()
        .get();
    App {
        window,
        cx: vcx,
        socket,
        pane,
        _dir: dir,
    }
}

fn open_at(
    cx: &mut TestAppContext,
    pty: PtyConfig,
    socket: &Path,
    session: Option<&Path>,
) -> (WindowHandle<BentermWindow>, VisualTestContext) {
    let start: ipc::StartTerminal = Rc::new(StartedTerminal::spawn);
    let window = cx
        .update(|cx| {
            ipc::open_session_window(
                pty,
                start,
                Some(socket.to_owned()),
                session.map(Path::to_owned),
                ConfigFeed::defaults(),
                cx,
            )
        })
        .unwrap();
    let vcx = VisualTestContext::from_window(window.into(), cx);
    vcx.run_until_parked();
    (window, vcx)
}

impl App {
    fn pass_time(&mut self) {
        self.cx.run_until_parked();
        let open = self.window.update(&mut self.cx, |_, _, _| ()).is_ok();
        if open {
            self.cx.update(|window, cx| window.draw(cx).clear(cx));
        }
        std::thread::sleep(Duration::from_millis(20));
        self.cx.executor().advance_clock(Duration::from_millis(20));
        self.cx.run_until_parked();
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
            if let Ok(result) = answer.try_recv() {
                return result;
            }
            assert!(Instant::now() < deadline, "no answer before the timeout");
            self.pass_time();
        }
    }

    /// What the program of `pane` printed, once it has listed its
    /// credentials.
    fn listed(&mut self, pane: u64) -> String {
        self.output_until(pane, "listed")
    }

    fn output_until(&mut self, pane: u64, marker: &str) -> String {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            let pane = ApiPaneId::new(pane);
            let text = self.call(ReadOutput { pane, lines: None }).unwrap().text;
            if text.contains(marker) {
                return text;
            }
            assert!(Instant::now() < deadline, "the program listed nothing");
            self.pass_time();
        }
    }

    fn spawn(&mut self, name: &str, control: bool) -> u64 {
        let spawned = self
            .call(SpawnAgent {
                name: name.into(),
                command: vec!["/bin/sh".into(), "-c".into(), NAMES.into()],
                parent: Some(ApiPaneId::new(self.pane)),
                control,
                ..SpawnAgent::default()
            })
            .unwrap();
        spawned.pane.get()
    }
}

#[gpui::test]
fn a_pane_of_the_window_gets_a_credential_for_the_whole_api(cx: &mut TestAppContext) {
    let mut app = open(cx);
    let pane = app.pane;
    let text = app.listed(pane);
    assert!(text.contains("has BENTERM_PANE_TOKEN"), "{text}");
    assert!(text.contains("has BENTERM_CONTROL_TOKEN"), "{text}");
}

#[gpui::test]
fn an_agent_gets_a_credential_for_its_own_pane_only(cx: &mut TestAppContext) {
    let mut app = open(cx);
    let worker = app.spawn("worker", false);
    let text = app.listed(worker);
    assert!(text.contains("has BENTERM_PANE_TOKEN"), "{text}");
    assert!(!text.contains("has BENTERM_CONTROL_TOKEN"), "{text}");
}

#[gpui::test]
fn a_child_shown_in_a_tab_resumes_without_control(cx: &mut TestAppContext) {
    let mut app = open_saving(NAMES, true, cx);
    let child = app.spawn("child", false);
    let initial = app.listed(child);
    assert!(initial.contains("has BENTERM_PANE_TOKEN"));
    assert!(!initial.contains("has BENTERM_CONTROL_TOKEN"));
    app.call(ShowAgent {
        agent: AgentTarget::Name("child".into()),
        select: true,
    })
    .unwrap();
    app.window
        .update(&mut app.cx, |root, _, cx| {
            root.terminal(cx).update(cx, |view, cx| {
                view.report_hook(
                    &HookEvent::new("claude", "SessionStart").with_session("child-conversation"),
                    cx,
                );
            });
        })
        .unwrap();

    let dir = app._dir.path().canonicalize().unwrap();
    let file = dir.join("session.json");
    let deadline = Instant::now() + TIMEOUT;
    loop {
        app.pass_time();
        if let Ok(text) = std::fs::read(&file) {
            let session = Session::from_json(&text).unwrap();
            if matches!(&session.workspaces[0].tabs[1..], [tab]
                if matches!(&tab.root, Node::Terminal(pane)
                    if pane.conversation.as_ref().is_some_and(|c| c.id() == "child-conversation")))
            {
                break;
            }
        }
        assert!(
            Instant::now() < deadline,
            "the child conversation was never saved"
        );
    }
    app.window
        .update(&mut app.cx, |_, window, _| window.remove_window())
        .unwrap();

    let fake = dir.join("claude");
    let script = NAMES.replace("echo listed", r#"printf 'resumed %s %s\n' "$1" "$2""#);
    std::fs::write(&fake, format!("#!/bin/sh\n{script}\n")).unwrap();
    std::fs::set_permissions(fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut pty = PtyConfig::command("/bin/sh", ["-i"]);
    pty.env.push((
        "PATH".into(),
        format!("{}:/usr/bin:/bin", dir.display()).into(),
    ));
    app.socket = dir.join("reopened.sock");
    (app.window, app.cx) = open_at(cx, pty, &app.socket, Some(&file));
    let resumed = app
        .window
        .update(&mut app.cx, |root, _, cx| {
            root.grid(cx).read(cx).focused().get()
        })
        .unwrap();
    let text = app.output_until(resumed, "resumed --resume child-conversation");
    assert!(text.contains("has BENTERM_PANE_TOKEN"), "{text}");
    assert!(!text.contains("has BENTERM_CONTROL_TOKEN"), "{text}");
}

#[gpui::test]
fn an_agent_started_with_control_gets_the_whole_api(cx: &mut TestAppContext) {
    let mut app = open(cx);
    let lead = app.spawn("lead", true);
    let text = app.listed(lead);
    assert!(text.contains("has BENTERM_PANE_TOKEN"), "{text}");
    assert!(text.contains("has BENTERM_CONTROL_TOKEN"), "{text}");
}

#[gpui::test]
fn an_agent_without_control_sends_only_to_its_peers(cx: &mut TestAppContext) {
    let mut app = open(cx);
    let worker = app.spawn("worker", false);
    let lead = app.spawn("lead", true);
    let child = app
        .call(SpawnAgent {
            name: "child".into(),
            command: vec!["/bin/sh".into(), "-c".into(), NAMES.into()],
            parent: Some(ApiPaneId::new(worker)),
            ..SpawnAgent::default()
        })
        .unwrap()
        .pane
        .get();
    app.listed(child);

    // To its own child, a direct route a worker has no right to.
    let refused = app.call(SendToAgent {
        agent: AgentTarget::Name("child".into()),
        text: "go".into(),
        caller: Some(ApiPaneId::new(worker)),
    });
    match refused {
        Err(ClientError::Api(error)) => assert_eq!(error.code, ErrorCode::Forbidden),
        other => panic!("expected forbidden, got {other:?}"),
    }

    // The same route from an agent with control is let through.
    app.call(SpawnAgent {
        name: "lead-child".into(),
        command: vec!["/bin/sh".into(), "-c".into(), NAMES.into()],
        parent: Some(ApiPaneId::new(lead)),
        ..SpawnAgent::default()
    })
    .unwrap();
    app.call(SendToAgent {
        agent: AgentTarget::Name("lead-child".into()),
        text: "go".into(),
        caller: Some(ApiPaneId::new(lead)),
    })
    .unwrap();
}

/// What stands in for an agent that leaves a job behind: the job sends
/// one `agent.send` from the agent's own pane, with the pane's own
/// credential, each time the test asks, as a program of the pane and not
/// as the app. The agent itself ends when it reads `quit`.
fn leaving_a_sender_behind(dir: &std::path::Path, target: u64, text: &str) -> String {
    let dir = dir.display();
    format!(
        r#"send() {{ printf '%s\n' "{{\"id\":1,\"method\":\"agent.send\",\"params\":{{\"agent\":{target},\"text\":\"{text}\",\"caller\":$BENTERM_PANE_ID}},\"token\":\"$BENTERM_PANE_TOKEN\"}}" | /usr/bin/nc -U -w 3 "$BENTERM_SOCKET"; }}
(for n in 1 2; do
  while [ ! -e '{dir}/go'$n ]; do sleep 0.05; done
  send > '{dir}/answer'$n.part 2>&1
  mv '{dir}/answer'$n.part '{dir}/answer'$n
done) &
echo started
while read line; do [ "$line" = quit ] && exit 0; done"#
    )
}

/// A role's agent has no control, and its pane goes on with a shell when
/// it ends. What the agent left running there still holds the pane's
/// credential, and is no agent any more: it sends to no pane all the same.
#[gpui::test]
fn what_an_agent_left_in_its_pane_sends_to_no_pane_once_a_shell_took_over(cx: &mut TestAppContext) {
    // The shell stays the program in front of its pane, as a user's does.
    let mut app = open_running("echo listed; sleep 60; :", cx);
    let first = app.pane;
    app.listed(first);
    let dir = tempfile::tempdir().unwrap();
    let dir = dir.path().canonicalize().unwrap();
    let script = leaving_a_sender_behind(&dir, first, "typed-by-the-worker");
    let worker = app
        .call(SpawnAgent {
            name: "worker".into(),
            command: vec!["/bin/sh".into(), "-c".into(), script],
            shell_after: true,
            ..SpawnAgent::default()
        })
        .unwrap()
        .pane;
    app.call(ShowAgent {
        agent: AgentTarget::Name("worker".into()),
        select: false,
    })
    .unwrap();
    let answer = |app: &mut App, n: u32| {
        std::fs::write(dir.join(format!("go{n}")), "").unwrap();
        let path = dir.join(format!("answer{n}"));
        let deadline = Instant::now() + TIMEOUT;
        loop {
            if let Ok(answer) = std::fs::read_to_string(&path) {
                return answer;
            }
            assert!(Instant::now() < deadline, "the pane's program sent nothing");
            app.pass_time();
        }
    };

    // While the agent runs it is refused, as an agent with no such peer.
    let running = answer(&mut app, 1);
    assert!(running.contains("peer_not_allowed"), "{running}");

    app.call(SendText {
        pane: worker,
        text: "quit\n".into(),
    })
    .unwrap();
    let deadline = Instant::now() + TIMEOUT;
    while !app.call(ListAgents::default()).unwrap().agents.is_empty() {
        assert!(Instant::now() < deadline, "the agent did not give way");
        app.pass_time();
    }

    let after = answer(&mut app, 2);
    assert!(after.contains("forbidden"), "{after}");
    let text = app
        .call(ReadOutput {
            pane: ApiPaneId::new(first),
            lines: None,
        })
        .unwrap()
        .text;
    assert!(!text.contains("typed-by-the-worker"), "{text}");
}

//! The graph of the progress board in a window: the agents as nodes, the
//! lines to the agents they started, and the arrows of what they told
//! each other. The agents are started and talk over the real socket.

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime};

use benterm_ipc::{
    AgentTarget, Client, ClientError, Method, PaneId as ApiPaneId, ReadOutput, ReportHook,
    ReportToParent, SendToAgent, SpawnAgent,
};
use benterm_terminal::PtyConfig;
use benterm_ui::graph::{Arrow, Message, MessageKind, Node};
use benterm_ui::settings::ConfigFeed;
use benterm_ui::timeline::BandState;
use benterm_ui::{BentermWindow, BoardMode, MainView, ProgressBoard, StartedTerminal, ipc};
use benterm_ui::{bind_keys, graph};
use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext, WindowHandle};

const TIMEOUT: Duration = Duration::from_secs(10);

/// What the terminals run: prints `> ` before every line it reads, and
/// exits when it reads `quit`.
const ANSWERS: &str = r#"stty -echo; echo ready; while read line; do [ "$line" = quit ] && exit 0; echo "> $line"; done"#;

struct App {
    window: WindowHandle<BentermWindow>,
    cx: VisualTestContext,
    socket: PathBuf,
    /// The first pane of the window, which stands for the conductor.
    pane: u64,
    _dir: tempfile::TempDir,
}

fn open(cx: &mut TestAppContext) -> App {
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
                PtyConfig::command("/bin/sh", ["-c", ANSWERS]),
                start,
                Some(socket.clone()),
                ConfigFeed::defaults(),
                cx,
            )
        })
        .unwrap();
    let mut vcx = VisualTestContext::from_window(window.into(), cx);
    vcx.run_until_parked();
    let pane = window
        .update(&mut vcx, |root, _, cx| root.grid(cx).read(cx).focused())
        .unwrap()
        .get();
    let mut app = App {
        window,
        cx: vcx,
        socket,
        pane,
        _dir: dir,
    };
    app.wait_until("the first pane's program", |app| {
        app.read_pane(app.pane).contains("ready")
    });
    app
}

fn now_ms() -> u64 {
    let since = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH);
    since.unwrap().as_millis() as u64
}

impl App {
    fn pass_time(&mut self) {
        self.cx.run_until_parked();
        self.cx.update(|window, cx| window.draw(cx).clear(cx));
        std::thread::sleep(Duration::from_millis(20));
        self.cx.executor().advance_clock(Duration::from_millis(20));
        self.cx.run_until_parked();
    }

    fn wait_until(&mut self, what: &str, mut done: impl FnMut(&mut Self) -> bool) {
        let deadline = Instant::now() + TIMEOUT;
        while !done(self) {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            self.pass_time();
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
            if let Ok(result) = answer.try_recv() {
                return result;
            }
            assert!(Instant::now() < deadline, "no answer before the timeout");
            self.pass_time();
        }
    }

    fn read_pane(&mut self, pane: u64) -> String {
        let pane = ApiPaneId::new(pane);
        self.call(ReadOutput { pane, lines: None }).unwrap().text
    }

    /// Starts an agent behind the window as a child of `parent`, and
    /// returns the id of its terminal.
    fn spawn(&mut self, name: &str, parent: u64, allow_peers: bool) -> u64 {
        let request = SpawnAgent {
            name: name.into(),
            command: vec!["/bin/sh".into(), "-c".into(), ANSWERS.into()],
            parent: Some(ApiPaneId::new(parent)),
            allow_peers,
            ..SpawnAgent::default()
        };
        let pane = self.call(request).expect("the agent starts").pane.get();
        self.wait_until("the agent's program", |app| {
            app.read_pane(pane).contains("ready")
        });
        pane
    }

    /// The conductor's pane with the workers a and b, and c, which a
    /// started.
    fn family(&mut self) -> (u64, u64, u64) {
        let a = self.spawn("a", self.pane, false);
        let b = self.spawn("b", self.pane, false);
        let c = self.spawn("c", a, false);
        (a, b, c)
    }

    fn hook(&mut self, pane: u64, event: &str) {
        let hook = ReportHook {
            pane: ApiPaneId::new(pane),
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

    fn report(&mut self, from: u64, text: &str) {
        let report = ReportToParent {
            from: ApiPaneId::new(from),
            text: text.into(),
            status: None,
        };
        self.call(report).expect("the report is taken");
    }

    fn send(&mut self, to: &str, text: &str, caller: Option<u64>) -> Result<(), ClientError> {
        self.call(SendToAgent {
            agent: AgentTarget::Name(to.into()),
            text: text.into(),
            caller: caller.map(ApiPaneId::new),
        })
    }

    fn root<T>(&mut self, read: impl FnOnce(&BentermWindow, &gpui::App) -> T) -> T {
        self.window.read_with(&self.cx, read).unwrap()
    }

    fn messages(&mut self) -> Vec<Message> {
        self.root(|root, _| root.agent_messages())
    }

    fn open_graph(&mut self) -> Entity<ProgressBoard> {
        self.cx.simulate_keystrokes("cmd-shift-b");
        self.click("board-mode-relations");
        self.board()
    }

    fn board(&mut self) -> Entity<ProgressBoard> {
        self.root(|root, _| root.progress_board())
            .expect("the board is shown")
    }

    fn nodes(&mut self) -> Vec<Node> {
        let board = self.board();
        board.read_with(&self.cx, |board, _| board.graph_nodes())
    }

    fn node(&mut self, id: u64) -> Option<Node> {
        self.nodes().into_iter().find(|node| node.id == id)
    }

    fn arrows(&mut self) -> Vec<Arrow> {
        let board = self.board();
        board.read_with(&self.cx, |board, _| board.graph_arrows())
    }

    fn draw(&mut self) {
        self.cx.run_until_parked();
        let _ = self
            .window
            .update(&mut self.cx, |_, window, _| window.refresh());
        self.cx.run_until_parked();
    }

    fn drawn(&mut self, selector: &str) -> bool {
        self.draw();
        let selector: &'static str = selector.to_owned().leak();
        self.cx.debug_bounds(selector).is_some()
    }

    fn click(&mut self, selector: &str) {
        self.draw();
        let selector: &'static str = selector.to_owned().leak();
        let bounds = self
            .cx
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("{selector} is drawn"));
        self.cx.simulate_click(bounds.center(), Modifiers::none());
        self.cx.run_until_parked();
    }
}

#[gpui::test]
fn the_board_shows_the_timeline_or_the_graph(cx: &mut TestAppContext) {
    let mut app = open(cx);
    app.cx.simulate_keystrokes("cmd-shift-b");
    let board = app.board();
    let mode = |app: &mut App| board.read_with(&app.cx, |board, _| board.mode());

    assert_eq!(mode(&mut app), BoardMode::Timeline);
    assert!(!app.drawn("board-graph"));

    app.click("board-mode-relations");
    assert_eq!(mode(&mut app), BoardMode::Relations);
    assert!(app.drawn("board-graph"));

    app.click("board-mode-timeline");
    assert_eq!(mode(&mut app), BoardMode::Timeline);
    assert!(!app.drawn("board-graph"));

    // With the keyboard too.
    app.cx.simulate_keystrokes("r");
    assert_eq!(mode(&mut app), BoardMode::Relations);
    app.cx.simulate_keystrokes("t");
    assert_eq!(mode(&mut app), BoardMode::Timeline);
}

#[gpui::test]
fn the_graph_has_a_node_for_the_conductor_and_each_agent_it_and_they_started(
    cx: &mut TestAppContext,
) {
    let mut app = open(cx);
    let (a, b, c) = app.family();

    app.open_graph();

    let nodes = app.nodes();
    let told: Vec<_> = nodes
        .iter()
        .map(|node| (node.id, node.parent, node.background))
        .collect();
    assert_eq!(
        told,
        [
            (app.pane, None, false),
            (a, Some(app.pane), true),
            (b, Some(app.pane), true),
            (c, Some(a), true),
        ]
    );
    let names: Vec<&str> = nodes[1..].iter().map(|node| node.name.as_str()).collect();
    assert_eq!(names, ["a", "b", "c"]);
    let ids: Vec<_> = nodes.iter().map(|node| (node.id, node.parent)).collect();
    let layout = graph::layout(&ids);
    assert_eq!(layout.links, [(app.pane, a), (app.pane, b), (a, c)]);
    for id in [app.pane, a, b, c] {
        assert!(app.drawn(&format!("graph-node-{id}")), "node {id}");
    }
}

#[gpui::test]
fn a_pane_with_an_agent_nobody_started_is_a_node_and_a_plain_shell_is_none(
    cx: &mut TestAppContext,
) {
    let mut app = open(cx);
    app.open_graph();
    assert_eq!(app.nodes(), []);

    app.hook(app.pane, "UserPromptSubmit");

    app.wait_until("the node", |app| app.node(app.pane).is_some());
    let node = app.node(app.pane).unwrap();
    assert_eq!(node.agent, Some("claude"));
    assert_eq!(node.state, Some(BandState::Working));
}

#[gpui::test]
fn a_change_of_state_reaches_the_graph_while_it_is_open(cx: &mut TestAppContext) {
    let mut app = open(cx);
    let (a, _, _) = app.family();
    app.open_graph();
    assert_eq!(app.node(a).unwrap().state, None);

    app.hook(a, "UserPromptSubmit");
    app.wait_until("a at work", |app| {
        app.node(a).unwrap().state == Some(BandState::Working)
    });
    app.hook(a, "Stop");
    app.wait_until("a done", |app| {
        app.node(a).unwrap().state == Some(BandState::Done)
    });
}

#[gpui::test]
fn a_report_is_an_arrow_from_the_child_to_its_parent_that_tells_its_time_and_first_line(
    cx: &mut TestAppContext,
) {
    let mut app = open(cx);
    let (a, _, _) = app.family();
    let board = app.open_graph();
    assert_eq!(app.arrows(), []);
    let before = now_ms();

    app.report(a, "done: PR 12 is up\nthe details follow");

    app.wait_until("the arrow", |app| app.arrows().len() == 1);
    let arrow = app.arrows().remove(0);
    assert_eq!((arrow.from, arrow.to, arrow.count), (a, app.pane, 1));
    assert_eq!(arrow.latest.kind, MessageKind::Report);
    assert!(!app.drawn("graph-detail"));

    app.click(&format!("graph-arrow-{a}-{}", app.pane));

    assert!(app.drawn("graph-detail"));
    let (selected, lines) = board.read_with(&app.cx, |board, _| {
        (board.selected_messages(), board.detail_lines())
    });
    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].line, "done: PR 12 is up");
    assert!((before..=now_ms()).contains(&selected[0].at_ms));
    let clock = board.read_with(&app.cx, |board, _| board.clock_of(&selected[0]));
    assert_eq!(clock.len(), "00:00:00".len());
    assert!(
        lines.iter().any(|line| line.contains(&clock)),
        "{lines:?} has {clock}"
    );
    assert!(lines.iter().any(|line| line == "done: PR 12 is up"));
}

#[gpui::test]
fn a_click_on_the_line_of_a_message_away_from_its_mark_selects_it(cx: &mut TestAppContext) {
    let mut app = open(cx);
    let (a, _, _) = app.family();
    let board = app.open_graph();
    app.report(a, "done");
    app.wait_until("the arrow", |app| app.arrows().len() == 1);
    app.draw();

    // A quarter of the way along the line, where no mark is.
    let on_the_line = board
        .read_with(&app.cx, |board, _| board.point_on_arrow(a, app.pane, 0.25))
        .expect("the arrow is drawn");
    app.cx.simulate_click(on_the_line, Modifiers::none());
    app.cx.run_until_parked();

    let selected = board.read_with(&app.cx, |board, _| board.selected_messages());
    assert_eq!(selected.len(), 1);

    // A click on nothing takes the selection away.
    let graph = app.cx.debug_bounds("board-graph").unwrap();
    app.cx.simulate_click(
        graph.bottom_right() - gpui::point(gpui::px(4.), gpui::px(4.)),
        Modifiers::none(),
    );
    app.cx.run_until_parked();
    let selected = board.read_with(&app.cx, |board, _| board.selected_messages());
    assert_eq!(selected, []);
}

#[gpui::test]
fn what_agent_send_gives_is_kept_with_the_pane_that_sent_it(cx: &mut TestAppContext) {
    let mut app = open(cx);
    let (_, b, _) = app.family();

    app.send(
        "b",
        "review the diff\nand say what is wrong",
        Some(app.pane),
    )
    .unwrap();

    let messages = app.messages();
    assert_eq!(messages.len(), 1);
    let message = &messages[0];
    assert_eq!(message.kind, MessageKind::Request);
    assert_eq!((message.from, message.to), (app.pane, b));
    assert_eq!(message.line, "review the diff");

    app.open_graph();
    let arrows = app.arrows();
    assert_eq!(arrows.len(), 1);
    assert_eq!((arrows[0].from, arrows[0].to), (app.pane, b));
}

#[gpui::test]
fn a_message_without_a_sender_or_from_a_pane_that_is_gone_is_given_and_not_kept(
    cx: &mut TestAppContext,
) {
    let mut app = open(cx);
    let (_, b, _) = app.family();

    app.send("b", "from outside", None).unwrap();
    app.send("b", "from a pane that closed", Some(9_999))
        .unwrap();

    app.wait_until("both to arrive", |app| {
        app.read_pane(b).contains("> from a pane that closed")
    });
    assert!(app.read_pane(b).contains("> from outside"));
    assert_eq!(app.messages(), []);
}

#[gpui::test]
fn a_report_that_is_refused_is_not_kept(cx: &mut TestAppContext) {
    let mut app = open(cx);
    app.family();

    // The first pane was started by nobody.
    let refused = app.call(ReportToParent {
        from: ApiPaneId::new(app.pane),
        text: "to nobody".into(),
        status: None,
    });

    assert!(refused.is_err());
    assert_eq!(app.messages(), []);
}

#[gpui::test]
fn a_message_sent_while_the_graph_is_open_reaches_it(cx: &mut TestAppContext) {
    let mut app = open(cx);
    let a = app.spawn("a", app.pane, true);
    let b = app.spawn("b", app.pane, true);
    app.open_graph();

    app.send("b", "go on", Some(a)).unwrap();

    app.wait_until("the arrow", |app| app.arrows().len() == 1);
    assert!(app.drawn(&format!("graph-arrow-{a}-{b}")));
}

#[gpui::test]
fn a_click_on_the_node_of_an_agent_behind_the_window_opens_its_peek(cx: &mut TestAppContext) {
    let mut app = open(cx);
    let (a, _, _) = app.family();
    app.open_graph();

    app.click(&format!("graph-node-{a}"));

    let peeked = app.root(|root, _| root.peeked());
    assert_eq!(peeked.map(|pane| pane.get()), Some(a));
    assert_eq!(
        app.root(|root, _| root.main_view()),
        MainView::ProgressBoard
    );
}

#[gpui::test]
fn a_click_on_the_node_of_a_pane_goes_to_the_pane(cx: &mut TestAppContext) {
    let mut app = open(cx);
    app.family();
    app.open_graph();

    app.click(&format!("graph-node-{}", app.pane));

    assert_eq!(app.root(|root, _| root.main_view()), MainView::Terminals);
    assert_eq!(app.root(|root, _| root.peeked()), None);
    let focused = app.root(|root, cx| root.grid(cx).read(cx).focused());
    assert_eq!(focused.get(), app.pane);
}

#[gpui::test]
fn ended_agents_are_shown_faint_or_hidden_as_the_user_chooses(cx: &mut TestAppContext) {
    let mut app = open(cx);
    let (a, b, c) = app.family();
    for pane in [a, b, c] {
        app.hook(pane, "UserPromptSubmit");
    }
    // b has done its work; a is done too, and c under it is at work.
    app.hook(b, "Stop");
    app.hook(a, "Stop");
    let board = app.open_graph();
    app.wait_until("b done", |app| {
        app.node(b)
            .is_some_and(|node| node.state == Some(BandState::Done))
    });
    let hides = |app: &mut App| board.read_with(&app.cx, |board, _| board.hides_ended());

    assert!(!hides(&mut app));
    assert!(app.drawn(&format!("graph-node-{b}")));

    app.click("graph-hide-ended");

    assert!(hides(&mut app));
    assert!(!app.drawn(&format!("graph-node-{b}")));
    // a stays, as c hangs under it.
    assert!(app.drawn(&format!("graph-node-{a}")));
    assert!(app.drawn(&format!("graph-node-{c}")));

    app.click("graph-hide-ended");
    assert!(app.drawn(&format!("graph-node-{b}")));
}

#[gpui::test]
fn an_agent_whose_terminal_closed_stays_in_the_graph_as_closed(cx: &mut TestAppContext) {
    let mut app = open(cx);
    let (_, b, _) = app.family();
    app.report(b, "done");
    app.open_graph();

    app.send("b", "quit", None).unwrap();

    app.wait_until("b closed", |app| {
        app.node(b).is_some_and(|node| node.closed)
    });
    let node = app.node(b).unwrap();
    assert_eq!((node.name.as_str(), node.parent), ("b", Some(app.pane)));
    assert!(node.ended());
    // Its report is still an arrow.
    assert_eq!(app.arrows().len(), 1);
    // There is nothing to go to.
    app.click(&format!("graph-node-{b}"));
    assert_eq!(app.root(|root, _| root.peeked()), None);
    assert_eq!(
        app.root(|root, _| root.main_view()),
        MainView::ProgressBoard
    );
}

#[gpui::test]
fn the_graph_is_not_drawn_again_while_nothing_in_it_changes(cx: &mut TestAppContext) {
    let mut app = open(cx);
    let (a, _, _) = app.family();
    let board = app.open_graph();
    app.report(a, "done");
    app.wait_until("the arrow", |app| app.arrows().len() == 1);
    app.draw();
    let drawn = Rc::new(std::cell::Cell::new(0));
    let count = drawn.clone();
    let _watch = app
        .cx
        .update(|_, cx| cx.observe(&board, move |_, _| count.set(count.get() + 1)));

    // Long enough for the board to look at the time more than once.
    for _ in 0..4 {
        app.cx.executor().advance_clock(Duration::from_secs(5));
        app.cx.run_until_parked();
    }

    assert_eq!(drawn.get(), 0);
}

#[gpui::test]
fn an_agent_that_closed_before_the_board_was_opened_is_drawn_as_it_was_last(
    cx: &mut TestAppContext,
) {
    let mut app = open(cx);
    let (_, b, _) = app.family();
    app.hook(b, "UserPromptSubmit");
    app.wait_until("b at work", |app| {
        let nodes = app.root(|root, cx| root.agent_graph(cx));
        nodes
            .iter()
            .any(|node| node.id == b && node.state == Some(BandState::Working))
    });

    app.send("b", "quit", None).unwrap();
    app.wait_until("b closed", |app| {
        let nodes = app.root(|root, cx| root.agent_graph(cx));
        nodes.iter().any(|node| node.id == b && node.closed)
    });
    app.open_graph();

    let nodes = app.nodes();
    let of_b: Vec<&Node> = nodes.iter().filter(|node| node.id == b).collect();
    assert_eq!(of_b.len(), 1);
    assert_eq!(of_b[0].name, "b");
    assert_eq!(of_b[0].agent, Some("claude"));
    assert_eq!(of_b[0].state, Some(BandState::Working));
}

#[gpui::test]
fn a_pane_whose_agent_nobody_started_stays_as_closed_when_the_pane_closes(cx: &mut TestAppContext) {
    let mut app = open(cx);
    app.cx.simulate_keystrokes("cmd-d");
    let second = app.root(|root, cx| root.grid(cx).read(cx).focused()).get();
    app.hook(second, "UserPromptSubmit");
    app.wait_until("the node", |app| {
        let nodes = app.root(|root, cx| root.agent_graph(cx));
        nodes.iter().any(|node| node.id == second)
    });

    app.cx.simulate_keystrokes("cmd-w");

    app.wait_until("the closed node", |app| {
        let nodes = app.root(|root, cx| root.agent_graph(cx));
        nodes.iter().any(|node| node.id == second && node.closed)
    });
}

#[gpui::test]
fn a_message_sent_before_the_graph_came_into_view_does_not_run_along_its_arrow(
    cx: &mut TestAppContext,
) {
    let mut app = open(cx);
    let (a, _, _) = app.family();
    // The board is shown, with the timeline in it.
    app.cx.simulate_keystrokes("cmd-shift-b");
    app.report(a, "done");
    app.wait_until("the message", |app| app.messages().len() == 1);
    std::thread::sleep(Duration::from_millis(5));

    app.click("board-mode-relations");
    assert!(!app.drawn("graph-spark"));

    app.report(a, "done again");
    app.wait_until("the second", |app| app.arrows()[0].count == 2);
    assert!(app.drawn("graph-spark"));
}

#[gpui::test]
fn the_children_of_a_wide_family_are_all_drawn_inside_the_graph(cx: &mut TestAppContext) {
    let mut app = open(cx);
    // Five boxes side by side are wider than the graph of this window.
    let names = ["a", "b", "c", "d", "e"];
    let children: Vec<u64> = names
        .iter()
        .map(|name| app.spawn(name, app.pane, false))
        .collect();
    app.open_graph();
    app.draw();
    app.draw();

    let graph = app.cx.debug_bounds("board-graph").unwrap();
    for child in children {
        let selector: &'static str = format!("graph-node-{child}").leak();
        let node = app.cx.debug_bounds(selector).expect("the node is drawn");
        assert!(
            node.left() >= graph.left() && node.right() <= graph.right(),
            "{node:?} is inside {graph:?}"
        );
    }
}

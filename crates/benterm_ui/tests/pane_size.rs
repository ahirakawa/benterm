//! The size of the PTY of a pane that is not on view: a pane in a tab or
//! workspace behind the shown one, which no frame measures.

use std::cell::RefCell;
use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use benterm_ipc::{
    Client, ClientError, CreateWorkspace, Method, PaneId as ApiPaneId, SplitDirection, SplitPane,
};
use benterm_layout::{PaneId, WorkspaceId};
use benterm_session::{Axis, Node, Session, Split, Tab, Terminal, Workspace};
use benterm_terminal::{PtyConfig, WindowSize};
use benterm_ui::settings::ConfigFeed;
use benterm_ui::theme::Theme;
use benterm_ui::viewport::grid_size;
use benterm_ui::{
    BentermWindow, PaneGrid, SelectPreviousWorkspace, StartedTerminal, bind_keys, ipc,
};
use gpui::{
    AnyWindowHandle, Bounds, Entity, Keystroke, Pixels, Size, TestAppContext, VisualTestContext,
    WindowHandle, px, size,
};

const TIMEOUT: Duration = Duration::from_secs(10);

/// Prints the size of its terminal in brackets when it starts and again
/// every time the size changes, and a tick for every file the test makes
/// for its pane. It ends when the test makes the file that ends it. The shell runs a trap before its next command, so a
/// signal sent before a file is made is reported before the tick for
/// that file: a test that has seen the tick has seen every report. The
/// shell is not interactive, so nothing writes a size back.
///
/// The ticks do not come over the terminal: the `read` of bash 3.2 loses
/// the line it is reading when a signal arrives. A report is one
/// pipeline, which the shell finishes before it runs a trap: an `echo` of
/// a command substitution is cut in two by the report of a signal that
/// arrives in the middle of it.
const REPORTS_ITS_SIZE: &str = "report() { stty size | sed 's/.*/[&]/'; }; \
     trap report WINCH; report; n=1; \
     while :; do sleep 0.02; \
       if [ -e \"$TICKS/$BENTERM_PANE_ID.$n\" ]; then echo \"tick $n.\"; n=$((n+1)); fi; \
       if [ -e \"$TICKS/$BENTERM_PANE_ID.end\" ]; then exit; fi; \
     done";

/// A window with the socket API on a socket in a private directory.
struct Api {
    window: WindowHandle<BentermWindow>,
    cx: VisualTestContext,
    socket: PathBuf,
    /// The size every terminal was started with, oldest first.
    started: Rc<RefCell<Vec<WindowSize>>>,
    /// How many ticks the test has asked of the program of each pane.
    ticks: HashMap<u64, usize>,
    dir: tempfile::TempDir,
}

/// Opens a window whose panes all report their sizes, in a size that
/// gives no pane 24 rows and 80 columns.
fn open(cx: &mut TestAppContext) -> Api {
    let mut api = open_session(None, cx);
    api.resize(1100., 700.);
    api
}

/// Opens a window with the workspaces, tabs and panes of `session`, as
/// the app does when it starts again, in the size a window opens with.
fn open_session(session: Option<&Session>, cx: &mut TestAppContext) -> Api {
    cx.executor().allow_parking();
    cx.update(bind_keys);
    let dir = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap();
    let socket = dir.path().join("benterm.sock");
    let started = Rc::new(RefCell::new(Vec::new()));
    let start: ipc::StartTerminal = {
        let started = started.clone();
        Rc::new(move |pty, theme| {
            started.borrow_mut().push(pty.size);
            StartedTerminal::spawn(pty, theme)
        })
    };
    let mut pty = PtyConfig::command("/bin/sh", ["-c", REPORTS_ITS_SIZE]);
    pty.env.push(("TICKS".into(), dir.path().into()));
    let file = session.map(|session| {
        let file = dir.path().join("session.json");
        std::fs::write(&file, session.to_json().unwrap()).unwrap();
        file
    });
    let feed = ConfigFeed::defaults();
    let window = cx
        .update(|cx| ipc::open_session_window(pty, start, Some(socket.clone()), file, feed, cx))
        .unwrap();
    let vcx = VisualTestContext::from_window(window.into(), cx);
    vcx.run_until_parked();
    Api {
        window,
        cx: vcx,
        socket,
        started,
        ticks: HashMap::new(),
        dir,
    }
}

impl Api {
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

    /// Makes a workspace that is not selected, and returns it with its
    /// pane.
    fn create_workspace(&mut self) -> (WorkspaceId, PaneId) {
        let created = self.call(CreateWorkspace::default()).unwrap();
        let workspace = self
            .window
            .update(&mut self.cx, |root, _, _| root.workspaces())
            .unwrap()
            .into_iter()
            .find(|workspace| workspace.get() == created.workspace.get())
            .expect("the workspace is in the window");
        let (_, pane) = self.find(created.pane.get());
        (workspace, pane)
    }

    fn split(&mut self, pane: PaneId, direction: SplitDirection) -> PaneId {
        let request = SplitPane {
            pane: ApiPaneId::new(pane.get()),
            direction,
        };
        let created = self.call(request).expect("the pane is split");
        self.find(created.pane.get()).1
    }

    /// The grid a pane is in, in any workspace and tab, and the pane.
    fn find(&mut self, id: u64) -> (Entity<PaneGrid>, PaneId) {
        self.window
            .update(&mut self.cx, |root, _, cx| {
                for workspace in root.workspaces() {
                    let tabs = root.workspace_tabs(workspace).unwrap();
                    let tabs = tabs.read(cx);
                    for tab in tabs.tabs() {
                        let grid = tabs.grid(tab).unwrap();
                        if let Some(pane) =
                            grid.read(cx).panes().into_iter().find(|p| p.get() == id)
                        {
                            return (grid.clone(), pane);
                        }
                    }
                }
                panic!("the window has no pane {id}");
            })
            .expect("the window is open")
    }

    /// The panes of the window, workspace by workspace and tab by tab.
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

    fn shown_grid(&mut self) -> Entity<PaneGrid> {
        self.window
            .update(&mut self.cx, |root, _, cx| root.grid(cx))
            .expect("the window is open")
    }

    fn focused(&mut self) -> PaneId {
        let grid = self.shown_grid();
        grid.read_with(&self.cx, |grid, _| grid.focused())
    }

    /// Where the window shows panes: the bounds of the shown grid.
    fn pane_area(&mut self) -> Bounds<Pixels> {
        let grid = self.shown_grid();
        grid.read_with(&self.cx, |grid, _| {
            grid.pane_bounds()
                .into_iter()
                .map(|(_, bounds)| bounds)
                .reduce(|all, bounds| all.union(&bounds))
                .expect("the grid has a pane")
        })
    }

    /// Where the grid of a pane puts it, whether the grid is shown or not.
    fn bounds_of(&mut self, pane: PaneId) -> Bounds<Pixels> {
        let (grid, pane) = self.find(pane.get());
        grid.read_with(&self.cx, |grid, _| {
            grid.pane_bounds()
                .into_iter()
                .find(|(id, _)| *id == pane)
                .expect("the pane is in its grid")
                .1
        })
    }

    fn cell(&mut self) -> Size<Pixels> {
        let grid = self.shown_grid();
        grid.read_with(&self.cx, |grid, cx| {
            grid.terminal(grid.focused()).unwrap().read(cx).cell_size()
        })
    }

    /// The rows and columns of a terminal that fills `bounds`.
    fn fitting(&mut self, bounds: Bounds<Pixels>) -> (u16, u16) {
        let cell = self.cell();
        let fitting = grid_size(
            (bounds.size.width.into(), bounds.size.height.into()),
            Theme::default().terminal.padding,
            (cell.width.into(), cell.height.into()),
        );
        (fitting.rows, fitting.cols)
    }

    /// The size a terminal that fills `bounds` is started with.
    fn starting_in(&mut self, bounds: Bounds<Pixels>) -> WindowSize {
        let (rows, cols) = self.fitting(bounds);
        let cell = self.cell();
        WindowSize {
            rows,
            cols,
            pixel_width: f32::from(cell.width * f32::from(cols)) as u16,
            pixel_height: f32::from(cell.height * f32::from(rows)) as u16,
        }
    }

    /// The size the terminal started last was started with.
    fn last_started(&self) -> WindowSize {
        *self
            .started
            .borrow()
            .last()
            .expect("a terminal was started")
    }

    /// Every size the program of a pane has reported so far, oldest
    /// first. A signal sent before the call is among them: the program is
    /// asked for a tick and has given it.
    fn reports(&mut self, pane: PaneId) -> Vec<(u16, u16)> {
        let ticks = self.ticks.entry(pane.get()).or_default();
        *ticks += 1;
        let tick = format!("tick {ticks}.");
        let file = self.dir.path().join(format!("{}.{ticks}", pane.get()));
        std::fs::write(file, "").unwrap();
        let (grid, pane) = self.find(pane.get());
        let view = grid.read_with(&self.cx, |grid, _| grid.terminal(pane).unwrap().clone());
        let deadline = Instant::now() + TIMEOUT;
        let text = loop {
            self.cx.run_until_parked();
            let text = view.read_with(&self.cx, |view, _| view.text(Some(1000)));
            if text.lines().any(|line| line.trim() == tick) {
                break text;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {tick:?}; the pane shows {text:?}"
            );
            std::thread::sleep(Duration::from_millis(5));
        };
        text.lines()
            .filter_map(|line| line.trim().strip_prefix('[')?.strip_suffix(']'))
            .map(|size| {
                let numbers = size
                    .split_once(' ')
                    .map(|(rows, cols)| (rows.parse(), cols.parse()));
                match numbers {
                    Some((Ok(rows), Ok(cols))) => (rows, cols),
                    _ => panic!("not a size: {size:?} in {text:?}"),
                }
            })
            .collect()
    }

    /// Ends the program of a pane and waits until its pane is closed.
    fn end(&mut self, pane: PaneId) {
        let file = self.dir.path().join(format!("{}.end", pane.get()));
        std::fs::write(file, "").unwrap();
        let deadline = Instant::now() + TIMEOUT;
        while self.all_panes().contains(&pane) {
            assert!(Instant::now() < deadline, "the pane was not closed");
            std::thread::sleep(Duration::from_millis(5));
            self.cx.run_until_parked();
        }
    }

    fn select(&mut self, workspace: WorkspaceId) {
        self.window
            .update(&mut self.cx, |root, window, cx| {
                root.select_workspace(workspace, window, cx);
            })
            .unwrap();
        self.cx.run_until_parked();
    }

    fn resize(&mut self, width: f32, height: f32) {
        self.cx.simulate_resize(size(px(width), px(height)));
        self.cx.run_until_parked();
    }
}

#[gpui::test]
fn a_workspace_made_from_the_socket_starts_at_the_size_of_the_pane_area(cx: &mut TestAppContext) {
    let mut api = open(cx);
    let area = api.pane_area();
    let whole = api.fitting(area);
    assert_ne!(whole, (24, 80), "the test needs a window of another size");
    // The shown tab is split, so the area differs from the focused pane.
    let shown = api.focused();
    api.split(shown, SplitDirection::Right);

    let (_, pane) = api.create_workspace();

    // The program started at that size: it was never told another one.
    assert_eq!(api.last_started(), api.starting_in(area));
    assert_eq!(api.reports(pane), [whole]);
}

#[gpui::test]
fn showing_a_workspace_made_from_the_socket_does_not_signal_its_program(cx: &mut TestAppContext) {
    let mut api = open(cx);
    let (workspace, pane) = api.create_workspace();
    let started = api.reports(pane);

    api.select(workspace);

    assert_eq!(api.pane_area(), api.bounds_of(pane));
    assert_eq!(api.reports(pane), started);
}

#[gpui::test]
fn splitting_a_pane_that_is_not_shown_gives_every_pty_the_size_of_its_pane(
    cx: &mut TestAppContext,
) {
    let mut api = open(cx);
    let area = api.pane_area();
    let whole = api.fitting(area);
    let (_, first) = api.create_workspace();
    // A program is split once it has said its size, so that it does not
    // miss the signal of the split while it starts.
    assert_eq!(api.reports(first), [whole]);

    let second = api.split(first, SplitDirection::Right);
    let right = api.bounds_of(second);
    let halved = api.fitting(right);
    assert_eq!(api.reports(second), [halved]);
    let third = api.split(second, SplitDirection::Down);

    // The panes share the area as the splits say: halves, then quarters.
    let [left, top, bottom] = [first, second, third].map(|pane| api.bounds_of(pane));
    assert_eq!(left.size.height, area.size.height);
    assert_eq!(left.size.width + right.size.width, area.size.width);
    assert!((left.size.width - right.size.width).abs() <= px(1.));
    assert_eq!(right.size.height, area.size.height);
    assert_eq!(top.size.height + bottom.size.height, area.size.height);
    assert!((top.size.height - bottom.size.height).abs() <= px(1.));
    assert_eq!(
        (top.size.width, bottom.size.width),
        (right.size.width, right.size.width)
    );

    // A pane that is split is told once, and a new pane starts at the
    // size it has when it is made.
    let [left, right, top, bottom] = [left, right, top, bottom].map(|bounds| api.fitting(bounds));
    assert_eq!(api.reports(first), [whole, left]);
    assert_eq!(api.reports(second), [right, top]);
    assert_eq!(api.reports(third), [bottom]);
}

#[gpui::test]
fn closing_a_pane_that_is_not_shown_gives_the_others_the_size_of_their_panes(
    cx: &mut TestAppContext,
) {
    let mut api = open(cx);
    let area = api.pane_area();
    let whole = api.fitting(area);
    let (_, first) = api.create_workspace();
    assert_eq!(api.reports(first), [whole]);
    let second = api.split(first, SplitDirection::Right);
    let left = api.bounds_of(first);
    let left = api.fitting(left);
    assert_eq!(api.reports(first), [whole, left]);

    api.end(second);

    assert_eq!(api.bounds_of(first), area);
    assert_eq!(api.reports(first), [whole, left, whole]);
}

/// Closing a pane goes by the window as it is now, as a split does: the
/// grid behind was laid out for the window before it was made smaller.
#[gpui::test]
fn closing_a_pane_that_is_not_shown_goes_by_the_size_the_window_has_now(cx: &mut TestAppContext) {
    let mut api = open(cx);
    let (_, first) = api.create_workspace();
    let started = api.reports(first);
    let second = api.split(first, SplitDirection::Down);
    let top = api.bounds_of(first);
    let top = api.fitting(top);
    assert_eq!(api.reports(first), [started[0], top]);
    api.resize(900., 600.);
    let area = api.pane_area();
    let now = api.fitting(area);
    assert_eq!(
        api.reports(first),
        [started[0], top],
        "the resize is not told"
    );

    api.end(second);

    assert_eq!(api.bounds_of(first), area);
    assert_eq!(api.reports(first), [started[0], top, now]);
}

#[gpui::test]
fn a_window_resize_reaches_a_pane_that_is_not_shown_when_it_is_shown(cx: &mut TestAppContext) {
    let mut api = open(cx);
    let (workspace, pane) = api.create_workspace();
    let started = api.reports(pane);

    // Sizes on the way, as when the edge of the window is dragged.
    api.resize(1000., 650.);
    api.resize(900., 600.);
    api.resize(800., 500.);
    assert_eq!(
        api.reports(pane),
        started,
        "a pane behind is not told the sizes on the way"
    );

    api.select(workspace);

    let area = api.pane_area();
    let now = api.fitting(area);
    assert_ne!(started, [now]);
    assert_eq!(api.reports(pane), [started[0], now]);
}

#[gpui::test]
fn a_split_of_a_pane_that_is_not_shown_goes_by_the_size_the_window_has_now(
    cx: &mut TestAppContext,
) {
    let mut api = open(cx);
    let (_, first) = api.create_workspace();
    let started = api.reports(first);
    api.resize(900., 600.);
    let area = api.pane_area();

    let second = api.split(first, SplitDirection::Down);

    let [top, bottom] = [first, second].map(|pane| api.bounds_of(pane));
    assert_eq!(top.size.width, area.size.width);
    assert_eq!(top.size.height + bottom.size.height, area.size.height);
    let [top, bottom] = [top, bottom].map(|bounds| api.fitting(bounds));
    assert_eq!(api.reports(first), [started[0], top]);
    assert_eq!(api.reports(second), [bottom]);
}

#[gpui::test]
fn a_new_tab_and_a_new_workspace_start_at_the_size_of_the_pane_area(cx: &mut TestAppContext) {
    let mut api = open(cx);
    let area = api.pane_area();
    let whole = api.fitting(area);
    let first = api.focused();
    api.split(first, SplitDirection::Right);

    api.cx.simulate_keystrokes("cmd-t");
    let tab = api.focused();
    assert_ne!(tab, first);
    // The size at the start is asked for as well as the first report: a
    // pane on view is fitted by its first frame, which may come before
    // the program looks.
    assert_eq!(api.last_started(), api.starting_in(area));
    assert_eq!(api.reports(tab), [whole]);

    api.cx.simulate_keystrokes("cmd-n");
    let workspace = api.focused();
    assert_ne!(workspace, tab);
    assert_eq!(api.last_started(), api.starting_in(area));
    assert_eq!(api.reports(workspace), [whole]);
}

#[gpui::test]
fn a_pane_split_from_the_keyboard_starts_at_the_size_of_its_pane(cx: &mut TestAppContext) {
    let mut api = open(cx);
    let first = api.focused();

    api.cx.simulate_keystrokes("cmd-d");

    let second = api.focused();
    assert_ne!(second, first);
    let right = api.bounds_of(second);
    assert_eq!(api.last_started(), api.starting_in(right));
    let right = api.fitting(right);
    assert_eq!(api.reports(second), [right]);
}

/// A grid keeps the place it was last drawn in, which is out of date
/// once the window has changed while another grid was shown. A pane made
/// before the next frame must not go by it.
#[gpui::test]
fn a_new_pane_goes_by_the_last_measure_of_the_window(cx: &mut TestAppContext) {
    let mut api = open(cx);
    let first = api
        .window
        .update(&mut api.cx, |root, _, _| root.selected_workspace());
    let first = first.unwrap();
    // The first workspace was last drawn in the wide window.
    api.cx.simulate_keystrokes("cmd-n");
    api.resize(700., 500.);
    let area = api.pane_area();
    let whole = api.fitting(area);

    // Both in one update, so no frame comes between them.
    api.window
        .update(&mut api.cx, |root, window, cx| {
            root.select_workspace(first, window, cx);
            root.new_workspace(window, cx);
        })
        .unwrap();
    api.cx.run_until_parked();

    let pane = api.focused();
    assert_eq!(api.last_started(), api.starting_in(area));
    assert_eq!(api.reports(pane), [whole]);
}

/// The menu bar hands its action to the app, and a key pressed right
/// after it may come before the next frame. The grid the key reaches was
/// last drawn for the window before it was made smaller. The window draws
/// a frame before it hands a key on, so the split goes by the window as
/// it is now.
#[gpui::test]
fn a_split_from_the_keyboard_right_after_a_workspace_is_chosen_from_the_menu_fits_its_pane(
    cx: &mut TestAppContext,
) {
    let mut api = open(cx);
    // The first workspace was last drawn in the wide window.
    api.cx.simulate_keystrokes("cmd-n");
    api.resize(700., 500.);
    let area = api.pane_area();
    api.cx.update(|window, _| window.activate_window());
    api.cx.run_until_parked();
    let shown = api.focused();

    // A test window draws whenever an update is over, so the item and the
    // key are handled in one update, the key after the item.
    let window = api.window;
    let stale = Rc::new(RefCell::new(None));
    cx.update(|cx| {
        // As the menu bar does: from the app, not from inside the window.
        cx.dispatch_action(&SelectPreviousWorkspace);
        let stale = stale.clone();
        // The item runs in a deferred call of its own; the key goes after
        // what that call leaves to do.
        cx.defer(move |cx| {
            cx.defer(move |cx| {
                let grid = window.read_with(cx, |root, cx| root.grid(cx)).unwrap();
                let (focused, places) =
                    grid.read_with(cx, |grid, _| (grid.focused(), grid.pane_bounds()));
                *stale.borrow_mut() = Some((focused, places));
                let key = Keystroke::parse("cmd-d").unwrap();
                let any: AnyWindowHandle = window.into();
                any.update(cx, |_, window, cx| window.dispatch_keystroke(key, cx))
                    .unwrap();
            });
        });
    });
    api.cx.run_until_parked();

    let (first, places) = stale.take().expect("the key was pressed");
    assert_ne!(first, shown, "the workspace was chosen before the key");
    assert_eq!(places.len(), 1);
    assert_ne!(
        places[0].1, area,
        "no frame came between the item and the key"
    );
    let second = api.focused();
    assert_ne!(second, first, "the pane was split");
    assert_eq!(api.pane_area(), area);
    let right = api.bounds_of(second);
    assert_eq!(api.last_started(), api.starting_in(right));
    let right = api.fitting(right);
    assert_eq!(api.reports(second), [right]);
}

fn one_pane() -> Node {
    Node::Terminal(Terminal::default())
}

/// The terminals of a session that is made again start before there is
/// a window, so none of them can start at the size of its place. The pane
/// on view is fitted by the first frame. The ones behind are fitted with
/// it, or they would keep 24 rows and 80 columns until they are shown.
#[gpui::test]
fn the_panes_behind_of_a_restored_session_are_fitted_with_the_first_frame(cx: &mut TestAppContext) {
    let tab = |root| Tab {
        name: None,
        focused_pane: 0,
        root,
    };
    let split = Node::Split(Split {
        axis: Axis::Horizontal,
        ratio: 0.25,
        first: Box::new(one_pane()),
        second: Box::new(one_pane()),
    });
    let session = Session {
        selected_workspace: 0,
        workspaces_made: 2,
        folded_repositories: Vec::new(),
        workspaces: vec![
            Workspace {
                task: None,
                name: "shown".into(),
                color: None,
                selected_tab: 0,
                tabs: vec![tab(one_pane()), tab(split)],
            },
            Workspace {
                task: None,
                name: "behind".into(),
                color: None,
                selected_tab: 0,
                tabs: vec![tab(one_pane())],
            },
        ],
    };

    let mut api = open_session(Some(&session), cx);

    let area = api.pane_area();
    let whole = api.fitting(area);
    assert_ne!(whole, (24, 80), "the test needs a window of another size");
    let panes = api.all_panes();
    let [_, left, right, behind] = panes[..] else {
        panic!("the session has four panes: {panes:?}");
    };
    // The split of the tab behind shares the area as the session says.
    let [left_place, right_place] = [left, right].map(|pane| api.bounds_of(pane));
    assert_eq!(
        left_place.size.width + right_place.size.width,
        area.size.width
    );
    assert!((left_place.size.width - area.size.width * 0.25).abs() <= px(1.));
    assert_eq!(left_place.size.height, area.size.height);
    for (pane, place) in [(left, left_place), (right, right_place), (behind, area)] {
        let fitted = api.fitting(place);
        // A program that has started by then is told the size, and one
        // that has not finds it when it starts. One that is told between
        // setting its trap and looking for the first time says it twice.
        let reports = api.reports(pane);
        let told = [vec![(24, 80), fitted], vec![fitted], vec![fitted, fitted]];
        assert!(told.contains(&reports), "{reports:?} for {fitted:?}");
    }
}

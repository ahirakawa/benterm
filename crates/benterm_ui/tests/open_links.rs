//! A command-click on a web address in a terminal opens it in a browser
//! pane at the right of the terminal. One in the peek opens it at the
//! right of the focused pane of the tab on view, so that it is seen.

#[path = "common/pages.rs"]
mod pages;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use benterm_ipc::{Client, PaneId as ApiPaneId, SpawnAgent};
use benterm_layout::PaneId;
use benterm_terminal::PtyConfig;
use benterm_ui::file_preview::Content;
use benterm_ui::links::Link;
use benterm_ui::selection::Point as Cell;
use benterm_ui::settings::ConfigFeed;
use benterm_ui::theme::Theme;
use benterm_ui::{
    BentermWindow, MainView, PaneGrid, StartedTerminal, TerminalView, bind_keys, ipc,
    open_terminal_window,
};
use gpui::{
    Entity, Modifiers, MouseButton, MouseDownEvent, MouseUpEvent, Pixels, Point, TestAppContext,
    VisualTestContext, WindowHandle, point, px,
};

use pages::Pages;

struct App {
    window: WindowHandle<BentermWindow>,
    cx: VisualTestContext,
    pages: Pages,
    /// The pane of the terminal the program runs in.
    first: PaneId,
}

impl App {
    /// Opens a window whose terminal runs `script` and waits until it
    /// shows `ready`.
    fn open(script: &str, cx: &mut TestAppContext) -> Self {
        Self::open_with_cwd(None, script, cx)
    }

    fn open_in(cwd: &Path, script: &str, cx: &mut TestAppContext) -> Self {
        Self::open_with_cwd(Some(cwd.to_owned()), script, cx)
    }

    fn open_with_cwd(cwd: Option<PathBuf>, script: &str, cx: &mut TestAppContext) -> Self {
        cx.executor().allow_parking();
        cx.update(bind_keys);
        let pages = cx.update(Pages::install);
        let pty = PtyConfig {
            cwd,
            ..PtyConfig::command("/bin/sh", ["-c", script])
        };
        let window = cx
            .update(|cx| open_terminal_window(pty, ConfigFeed::defaults(), cx))
            .unwrap();
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        cx.update(|window, _| window.activate_window());
        let first = window
            .update(&mut cx, |root, _, cx| root.grid(cx).read(cx).focused())
            .unwrap();
        let mut app = Self {
            window,
            cx,
            pages,
            first,
        };
        app.wait_for("ready");
        app
    }

    fn wait_until(&mut self, what: &str, done: impl Fn(&BentermWindow, &gpui::App) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            self.cx.run_until_parked();
            let _ = self
                .window
                .update(&mut self.cx, |_, window, _| window.refresh());
            self.cx.run_until_parked();
            if self
                .window
                .read_with(&self.cx, |w, cx| done(w, cx))
                .unwrap()
            {
                return;
            }
            assert!(Instant::now() < deadline, "timed out waiting until {what}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn grid(&mut self) -> Entity<PaneGrid> {
        self.window
            .update(&mut self.cx, |root, _, cx| root.grid(cx))
            .unwrap()
    }

    fn panes(&mut self) -> Vec<PaneId> {
        self.grid().read_with(&self.cx, |grid, _| grid.panes())
    }

    fn terminal(&mut self) -> Entity<TerminalView> {
        let first = self.first;
        self.grid()
            .read_with(&self.cx, |grid, _| grid.terminal(first).cloned().unwrap())
    }

    /// The address the browser pane `id` shows.
    fn url_of(&mut self, id: PaneId) -> Option<String> {
        self.grid()
            .read_with(&self.cx, |grid, cx| grid.browser(id)?.read(cx).url())
    }

    fn screen_text(&mut self) -> String {
        self.terminal().read_with(&self.cx, |view, _| {
            let rows = &view.screen().rows;
            rows.iter()
                .map(|row| row.text())
                .collect::<Vec<_>>()
                .join("\n")
        })
    }

    /// Waits until the screen shows `text`. The program runs in real
    /// time on another thread, hence the polling.
    fn wait_for(&mut self, text: &str) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            self.cx.run_until_parked();
            if self.screen_text().contains(text) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "the screen never showed {text:?}:\n{}",
                self.screen_text()
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// The window position of the middle of `col` on the `row`th row of
    /// the terminal.
    fn cell(&mut self, row: usize, col: usize) -> Point<Pixels> {
        let first = self.first;
        let pane = self.grid().read_with(&self.cx, |grid, _| {
            let all = grid.pane_bounds();
            all.into_iter().find(|(pane, _)| *pane == first).unwrap().1
        });
        let cell = self
            .terminal()
            .read_with(&self.cx, |view, _| view.cell_size());
        let padding = px(Theme::default().terminal.padding);
        pane.origin
            + point(
                padding + cell.width * (col as f32 + 0.5),
                padding + cell.height * (row as f32 + 0.5),
            )
    }

    fn click(&mut self, at: Point<Pixels>, modifiers: Modifiers) {
        click(&mut self.cx, at, modifiers);
    }

    fn hovered(&mut self) -> Option<Link> {
        self.terminal()
            .read_with(&self.cx, |view, _| view.hovered_link().cloned())
    }

    /// The bytes the program read, with each escape written as `ESC`.
    /// The program prints them in hex followed by `<end>`.
    fn received(&mut self) -> String {
        self.wait_for("<end>");
        let joined: String = self.screen_text().lines().map(str::trim_end).collect();
        let after_ready = &joined[joined.find("ready").unwrap() + "ready".len()..];
        let hex = &after_ready[..after_ready.find("<end>").unwrap()];
        let bytes: Vec<u8> = (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
            .collect();
        String::from_utf8_lossy(&bytes).replace('\x1b', "ESC")
    }
}

fn command() -> Modifiers {
    Modifiers {
        platform: true,
        ..Modifiers::none()
    }
}

/// A program that prints `text` and `ready`, and then waits.
fn printing(text: &str) -> String {
    format!("printf '{text}ready\\r\\n'; while :; do sleep 1; done")
}

const TWO_URLS: &str = "see https://example.com/a.\\r\\nand http://localhost:3000/b\\r\\n";

#[gpui::test]
fn a_command_click_on_an_address_opens_it_at_the_right(cx: &mut TestAppContext) {
    let mut app = App::open(&printing(TWO_URLS), cx);
    let first = app.first;

    let at = app.cell(0, 8);
    app.click(at, command());

    let panes = app.panes();
    assert_eq!(panes.len(), 2);
    assert_eq!(panes[0], first);
    let browser = panes[1];
    assert_eq!(
        app.url_of(browser).as_deref(),
        Some("https://example.com/a")
    );
    // The browser is at the right of the terminal, which keeps the
    // keyboard.
    let grid = app.grid();
    grid.read_with(&app.cx, |grid, _| {
        let bounds = grid.pane_bounds();
        assert_eq!(bounds[1].1.origin.x, bounds[0].1.right());
        assert_eq!(bounds[1].1.origin.y, bounds[0].1.origin.y);
        assert_eq!(grid.focused(), first);
    });
}

#[gpui::test]
fn the_next_address_opens_in_the_browser_already_at_the_right(cx: &mut TestAppContext) {
    let mut app = App::open(&printing(TWO_URLS), cx);
    let at = app.cell(0, 8);
    app.click(at, command());
    let browser = app.panes()[1];

    let at = app.cell(1, 8);
    app.click(at, command());

    assert_eq!(app.panes().len(), 2);
    assert_eq!(
        app.url_of(browser).as_deref(),
        Some("http://localhost:3000/b")
    );
    assert_eq!(app.pages.len(), 1);
    assert_eq!(
        app.pages.page(0).borrow().loads,
        ["https://example.com/a", "http://localhost:3000/b"]
    );
}

#[gpui::test]
fn a_click_without_command_opens_nothing(cx: &mut TestAppContext) {
    let mut app = App::open(&printing(TWO_URLS), cx);

    let at = app.cell(0, 8);
    app.click(at, Modifiers::none());

    assert_eq!(app.panes().len(), 1);
}

#[gpui::test]
fn a_command_click_beside_an_address_opens_nothing(cx: &mut TestAppContext) {
    let mut app = App::open(&printing(TWO_URLS), cx);

    // The full stop after the address.
    let at = app.cell(0, 25);
    app.click(at, command());

    assert_eq!(app.panes().len(), 1);
}

#[gpui::test]
fn a_command_click_on_a_hyperlink_opens_its_address(cx: &mut TestAppContext) {
    let osc8 = "a \\033]8;;https://example.com\\033\\\\link\\033]8;;\\033\\\\ b\\r\\n";
    let mut app = App::open(&printing(osc8), cx);

    let at = app.cell(0, 3);
    app.click(at, command());

    let panes = app.panes();
    assert_eq!(panes.len(), 2);
    assert_eq!(app.url_of(panes[1]).as_deref(), Some("https://example.com"));
}

#[gpui::test]
fn a_command_click_on_an_address_is_not_reported_to_the_program(cx: &mut TestAppContext) {
    // Turns on click reports in SGR, prints an address, and prints in
    // hex the first 18 bytes it reads: one click.
    let script = "stty raw -echo; \
        printf '\\033[?1000h\\033[?1006hsee https://example.com/a\\r\\nready\\r\\n'; \
        head -c 18 | od -An -tx1 | tr -d ' \\n'; printf '<end>'; \
        while :; do sleep 1; done";
    let mut app = App::open(script, cx);

    let at = app.cell(0, 8);
    app.click(at, command());
    let panes = app.panes();
    assert_eq!(panes.len(), 2);
    assert_eq!(
        app.url_of(panes[1]).as_deref(),
        Some("https://example.com/a")
    );

    let at = app.cell(3, 2);
    app.click(at, Modifiers::none());
    assert_eq!(app.received(), "ESC[<0;3;4MESC[<0;3;4m");
}

#[gpui::test]
fn an_address_under_the_pointer_is_marked_while_command_is_held(cx: &mut TestAppContext) {
    let mut app = App::open(&printing(TWO_URLS), cx);
    let expected = Link {
        url: "https://example.com/a".into(),
        start: Cell { line: 0, col: 4 },
        end: Cell { line: 0, col: 24 },
    };

    let at = app.cell(0, 8);
    app.cx.simulate_mouse_move(at, None, Modifiers::none());
    app.cx.run_until_parked();
    assert_eq!(app.hovered(), None);

    app.cx.simulate_mouse_move(at, None, command());
    app.cx.run_until_parked();
    assert_eq!(app.hovered(), Some(expected.clone()));

    // Off the address.
    let beside = app.cell(0, 1);
    app.cx.simulate_mouse_move(beside, None, command());
    app.cx.run_until_parked();
    assert_eq!(app.hovered(), None);

    // Pressing and letting go of command with the pointer still.
    app.cx.simulate_mouse_move(at, None, Modifiers::none());
    app.cx.simulate_modifiers_change(command());
    app.cx.run_until_parked();
    assert_eq!(app.hovered(), Some(expected));
    app.cx.simulate_modifiers_change(Modifiers::none());
    app.cx.run_until_parked();
    assert_eq!(app.hovered(), None);
}

/// The text of a hyperlink may be another address than the one it
/// opens. Where it goes is shown while command is held over it, before
/// the click.
#[gpui::test]
fn the_address_a_hyperlink_opens_is_shown_while_command_is_held_over_it(cx: &mut TestAppContext) {
    let osc8 = "a \\033]8;;https://other.example/login\\033\\\\https://example.com\\033]8;;\\033\\\\ b\\r\\n";
    let mut app = App::open(&printing(osc8), cx);
    let at = app.cell(0, 5);

    app.cx.simulate_mouse_move(at, None, Modifiers::none());
    app.cx.run_until_parked();
    assert!(app.cx.debug_bounds("terminal-link-target").is_none());

    app.cx.simulate_mouse_move(at, None, command());
    app.cx.run_until_parked();
    assert!(app.cx.debug_bounds("terminal-link-target").is_some());
    let shown = app
        .terminal()
        .read_with(&app.cx, |view, _| view.link_target());
    assert_eq!(
        shown.as_deref(),
        Some("other.example — https://other.example/login")
    );

    app.cx.simulate_modifiers_change(Modifiers::none());
    app.cx.run_until_parked();
    assert!(app.cx.debug_bounds("terminal-link-target").is_none());
}

/// A window as the app opens it, with a socket for agents to be started
/// behind it, where every terminal prints [`TWO_URLS`].
struct Peeking {
    window: WindowHandle<BentermWindow>,
    cx: VisualTestContext,
    pages: Pages,
    socket: PathBuf,
    _dir: tempfile::TempDir,
}

impl Peeking {
    fn open(cx: &mut TestAppContext) -> Self {
        cx.executor().allow_parking();
        cx.update(bind_keys);
        let pages = cx.update(Pages::install);
        let dir = tempfile::Builder::new()
            .permissions(fs::Permissions::from_mode(0o700))
            .tempdir()
            .unwrap();
        let socket = dir.path().join("benterm.sock");
        let start: ipc::StartTerminal = Rc::new(StartedTerminal::spawn);
        let pty = PtyConfig::command("/bin/sh", ["-c", &printing(TWO_URLS)]);
        let window = cx
            .update(|cx| {
                ipc::open_window(pty, start, Some(socket.clone()), ConfigFeed::defaults(), cx)
            })
            .unwrap();
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        cx.update(|window, _| window.activate_window());
        let mut benterm = Self {
            window,
            cx,
            pages,
            socket,
            _dir: dir,
        };
        let first = benterm.focused();
        benterm.wait_for_ready(first);
        benterm
    }

    fn root<T>(&mut self, read: impl FnOnce(&BentermWindow, &gpui::App) -> T) -> T {
        self.window.read_with(&self.cx, read).unwrap()
    }

    fn grid(&mut self) -> Entity<PaneGrid> {
        self.root(|root, cx| root.grid(cx))
    }

    fn focused(&mut self) -> PaneId {
        self.root(|root, cx| root.grid(cx).read(cx).focused())
    }

    fn panes(&mut self) -> Vec<PaneId> {
        self.grid().read_with(&self.cx, |grid, _| grid.panes())
    }

    fn url_of(&mut self, id: PaneId) -> Option<String> {
        self.grid()
            .read_with(&self.cx, |grid, cx| grid.browser(id)?.read(cx).url())
    }

    fn terminal(&mut self, pane: PaneId) -> Entity<TerminalView> {
        self.root(|root, cx| root.terminal_with_id(pane, cx).expect("the terminal"))
    }

    /// Waits until the program of `pane` has printed the addresses. It
    /// runs in real time on another thread, hence the polling.
    fn wait_for_ready(&mut self, pane: PaneId) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            self.cx.run_until_parked();
            let text = self
                .terminal(pane)
                .read_with(&self.cx, |view, _| view.text(None));
            if text.contains("ready") {
                return;
            }
            assert!(Instant::now() < deadline, "the program never got ready");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Starts the agent `name` behind the window and returns the id of
    /// its terminal once its program has printed the addresses.
    fn spawn_behind(&mut self, name: &str, parent: PaneId) -> PaneId {
        let request = SpawnAgent {
            name: name.into(),
            command: vec!["/bin/sh".into(), "-c".into(), printing(TWO_URLS)],
            parent: Some(ApiPaneId::new(parent.get())),
            ..SpawnAgent::default()
        };
        let (sender, answer) = mpsc::channel();
        let socket = self.socket.clone();
        std::thread::spawn(move || {
            let mut client = Client::connect(socket).unwrap();
            let _ = sender.send(client.call(&request));
        });
        let deadline = Instant::now() + Duration::from_secs(10);
        let number = loop {
            self.cx.run_until_parked();
            if let Ok(result) = answer.recv_timeout(Duration::from_millis(5)) {
                break result.expect("the agent starts").pane.get();
            }
            assert!(Instant::now() < deadline, "no answer before the timeout");
        };
        // Its row in the session list has the id as the layout knows it.
        let pane = loop {
            let rows = self.root(|root, cx| root.session_rows(cx));
            if let Some(row) = rows.iter().find(|row| row.pane.get() == number) {
                break row.pane;
            }
            assert!(Instant::now() < deadline, "the agent never got a row");
            std::thread::sleep(Duration::from_millis(5));
            self.cx.run_until_parked();
        };
        self.wait_for_ready(pane);
        pane
    }

    /// A second tab in front of the first, whose pane has the keyboard.
    /// Returns the pane of the first tab and the one of the second.
    fn tab_behind(&mut self) -> (PaneId, PaneId) {
        let behind = self.focused();
        self.cx.simulate_keystrokes("cmd-t");
        let front = self.focused();
        assert_ne!(front, behind);
        self.wait_for_ready(front);
        (behind, front)
    }

    fn peek(&mut self, pane: PaneId) {
        let peeked = self
            .window
            .update(&mut self.cx, |root, window, cx| root.peek(pane, window, cx))
            .unwrap();
        assert!(peeked);
        self.draw();
        assert_eq!(self.root(|root, _| root.peeked()), Some(pane));
    }

    fn draw(&mut self) {
        self.cx.run_until_parked();
        let _ = self
            .window
            .update(&mut self.cx, |_, window, _| window.refresh());
        self.cx.run_until_parked();
    }

    /// Command-clicks the middle of `col` on the `row`th row of the
    /// terminal of the peek, which is drawn under the peek's bar.
    fn click_in_peek(&mut self, row: usize, col: usize) {
        self.draw();
        let pane = self
            .root(|root, _| root.peeked())
            .expect("the peek is open");
        let bar = self.cx.debug_bounds("peek-bar").expect("the peek is drawn");
        let cell = self
            .terminal(pane)
            .read_with(&self.cx, |view, _| view.cell_size());
        let padding = px(Theme::default().terminal.padding);
        let at = bar.bottom_left()
            + point(
                padding + cell.width * (col as f32 + 0.5),
                padding + cell.height * (row as f32 + 0.5),
            );
        click(&mut self.cx, at, command());
    }

    /// Asserts that the tab on view has two panes, `left` and a browser
    /// at its right that shows `url`, and that `left` has the keyboard.
    fn assert_browser_right_of(&mut self, left: PaneId, url: &str) {
        let panes = self.panes();
        assert_eq!(panes.len(), 2, "{panes:?}");
        assert_eq!(panes[0], left);
        assert_eq!(self.url_of(panes[1]).as_deref(), Some(url));
        let grid = self.grid();
        grid.read_with(&self.cx, |grid, _| {
            let bounds = grid.pane_bounds();
            assert_eq!(bounds[1].1.origin.x, bounds[0].1.right());
            assert_eq!(grid.focused(), left);
        });
        let handle = self
            .terminal(left)
            .read_with(&self.cx, |view, _| view.focus_handle().clone());
        assert!(self.cx.update(|window, _| handle.is_focused(window)));
    }
}

fn click(cx: &mut VisualTestContext, at: Point<Pixels>, modifiers: Modifiers) {
    cx.simulate_event(MouseDownEvent {
        button: MouseButton::Left,
        position: at,
        modifiers,
        click_count: 1,
        first_mouse: false,
    });
    cx.simulate_event(MouseUpEvent {
        button: MouseButton::Left,
        position: at,
        modifiers,
        click_count: 1,
    });
    cx.run_until_parked();
}

#[gpui::test]
fn an_address_in_the_peek_of_a_tab_behind_opens_in_the_tab_on_view(cx: &mut TestAppContext) {
    let mut benterm = Peeking::open(cx);
    let (behind, front) = benterm.tab_behind();
    benterm.peek(behind);

    benterm.click_in_peek(0, 8);

    assert_eq!(benterm.root(|root, _| root.peeked()), None);
    benterm.assert_browser_right_of(front, "https://example.com/a");
    // The tab behind was left as it was.
    let grids = benterm.root(|root, cx| {
        let tabs = root.tabs().read(cx);
        tabs.tabs()
            .into_iter()
            .map(|tab| tabs.grid(tab).unwrap().read(cx).panes().len())
            .collect::<Vec<_>>()
    });
    assert_eq!(grids, [1, 2]);
}

#[gpui::test]
fn an_address_in_the_peek_of_an_agent_behind_the_window_opens_in_the_tab_on_view(
    cx: &mut TestAppContext,
) {
    let mut benterm = Peeking::open(cx);
    let front = benterm.focused();
    let w1 = benterm.spawn_behind("w1", front);
    benterm.peek(w1);

    benterm.click_in_peek(0, 8);

    assert_eq!(benterm.root(|root, _| root.peeked()), None);
    benterm.assert_browser_right_of(front, "https://example.com/a");
}

#[gpui::test]
fn an_address_in_the_peek_opens_in_the_browser_already_at_the_right(cx: &mut TestAppContext) {
    let mut benterm = Peeking::open(cx);
    let (behind, front) = benterm.tab_behind();
    benterm.peek(behind);
    benterm.click_in_peek(0, 8);
    let browser = benterm.panes()[1];

    benterm.peek(behind);
    benterm.click_in_peek(1, 8);

    benterm.assert_browser_right_of(front, "http://localhost:3000/b");
    assert_eq!(benterm.panes()[1], browser);
    assert_eq!(benterm.pages.len(), 1);
    assert_eq!(
        benterm.pages.page(0).borrow().loads,
        ["https://example.com/a", "http://localhost:3000/b"]
    );
}

#[gpui::test]
fn an_address_in_the_peek_over_the_progress_board_opens_in_the_panes(cx: &mut TestAppContext) {
    let mut benterm = Peeking::open(cx);
    let (behind, front) = benterm.tab_behind();
    benterm.cx.simulate_keystrokes("cmd-shift-b");
    assert_eq!(
        benterm.root(|root, _| root.main_view()),
        MainView::ProgressBoard
    );
    benterm.peek(behind);

    benterm.click_in_peek(0, 8);

    assert_eq!(benterm.root(|root, _| root.peeked()), None);
    assert_eq!(
        benterm.root(|root, _| root.main_view()),
        MainView::Terminals
    );
    benterm.assert_browser_right_of(front, "https://example.com/a");
}

fn sample_project() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();
    fs::create_dir_all(root.join("a/a")).unwrap();
    fs::write(root.join("a/a/nested.txt"), "nested\n").unwrap();
    (dir, root)
}

#[gpui::test]
fn a_command_click_on_an_existing_file_path_opens_it_in_the_file_browser(cx: &mut TestAppContext) {
    let (_dir, root) = sample_project();
    let mut app = App::open_in(&root, &printing("see src/main.rs\\r\\n"), cx);
    let main_path = root.join("src/main.rs");

    // Hovering with command marks the existing path as a link.
    let at = app.cell(0, 8);
    app.cx.simulate_mouse_move(at, None, command());
    app.wait_until("path link is marked on hover", |w, cx| {
        w.terminal(cx).read(cx).hovered_link().is_some()
    });
    let hovered = app.hovered().unwrap();
    assert_eq!(
        (hovered.start, hovered.end),
        (Cell { line: 0, col: 4 }, Cell { line: 0, col: 14 })
    );

    // Command-clicking opens the file browser and shows src/main.rs.
    app.click(at, command());
    app.wait_until("file browser opens src/main.rs", |w, cx| {
        w.file_browser()
            .and_then(|b| b.read(cx).opened().map(Path::to_owned))
            == Some(main_path.clone())
    });
    let browser = app
        .window
        .read_with(&app.cx, |w, _| w.file_browser().unwrap())
        .unwrap();
    assert_eq!(
        browser.read_with(&app.cx, |b, _| b.preview().unwrap().content.clone()),
        Content::Text("fn main() {}\n".into())
    );
    assert_eq!(app.panes().len(), 1);
}

#[gpui::test]
fn a_nonexistent_path_is_not_a_link_and_opens_nothing(cx: &mut TestAppContext) {
    let (_dir, root) = sample_project();
    let mut app = App::open_in(&root, &printing("see src/missing.rs\\r\\n"), cx);

    let at = app.cell(0, 8);
    app.cx.simulate_mouse_move(at, None, command());
    app.cx.run_until_parked();
    assert_eq!(app.hovered(), None);

    app.click(at, command());
    app.cx.run_until_parked();
    assert!(
        app.window
            .read_with(&app.cx, |w, _| w.file_browser())
            .unwrap()
            .is_none()
    );
}

#[gpui::test]
fn a_command_click_on_a_path_with_line_number_opens_the_file(cx: &mut TestAppContext) {
    let (_dir, root) = sample_project();
    let mut app = App::open_in(&root, &printing("error src/main.rs:12\\r\\n"), cx);
    let main_path = root.join("src/main.rs");

    let at = app.cell(0, 12);
    app.click(at, command());
    app.wait_until("file browser opens src/main.rs from path:12", |w, cx| {
        w.file_browser()
            .and_then(|b| b.read(cx).opened().map(Path::to_owned))
            == Some(main_path.clone())
    });
    let browser = app
        .window
        .read_with(&app.cx, |w, _| w.file_browser().unwrap())
        .unwrap();
    assert_eq!(
        browser.read_with(&app.cx, |b, _| b.preview().unwrap().content.clone()),
        Content::Text("fn main() {}\n".into())
    );
}

#[gpui::test]
fn a_command_click_on_a_folder_path_opens_that_folder_in_the_file_browser(cx: &mut TestAppContext) {
    let (_dir, root) = sample_project();
    let mut app = App::open_in(&root, &printing("dir src and a/a\\r\\n"), cx);
    let src_path = root.join("src");
    let nested_dir = root.join("a/a");

    let at_src = app.cell(0, 5);
    app.click(at_src, command());
    app.wait_until("src folder expands in file browser", |w, cx| {
        w.file_browser().is_some_and(|b| {
            let b = b.read(cx);
            b.selected() == Some(src_path.as_path())
                && b.rows().iter().any(|r| r.path == root.join("src/main.rs"))
        })
    });
    let browser = app
        .window
        .read_with(&app.cx, |w, _| w.file_browser().unwrap())
        .unwrap();
    assert_eq!(
        browser.read_with(&app.cx, |b, _| b.opened().map(Path::to_owned)),
        None
    );

    // Clicking an already-expanded folder keeps it expanded instead of toggling closed.
    app.click(at_src, command());
    app.cx.run_until_parked();
    assert!(browser.read_with(&app.cx, |b, _| {
        b.rows().iter().any(|r| r.path == root.join("src/main.rs"))
    }));

    // Clicking a nested unexpanded folder `a/a` expands `a` and `a/a` without NotRegularFile.
    let at_nested = app.cell(0, 13);
    app.click(at_nested, command());
    app.wait_until("a/a folder expands in file browser", |w, cx| {
        w.file_browser().is_some_and(|b| {
            let b = b.read(cx);
            b.selected() == Some(nested_dir.as_path())
                && b.rows()
                    .iter()
                    .any(|r| r.path == root.join("a/a/nested.txt"))
        })
    });
    assert_eq!(
        browser.read_with(&app.cx, |b, _| b.opened().map(Path::to_owned)),
        None
    );
}

//! The menu a right click opens on a tab, a row of the workspaces, a row
//! of the session list and a pane: what it lists, that an item acts on
//! what was clicked, and how the keyboard and the mouse close it. The
//! window is a real one and the programs are real; the agents behind the
//! window are started over the real socket.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use benterm_ipc::{
    Client, ClientError, Method, PaneId as ApiPaneId, ReadOutput, ReportHook, SpawnAgent,
};
use benterm_layout::{PaneId, TabId};
use benterm_terminal::PtyConfig;
use benterm_ui::settings::ConfigFeed;
use benterm_ui::theme::Theme;
use benterm_ui::{BentermWindow, MenuItemShown, StartedTerminal, bind_keys, ipc};
use gpui::{
    Bounds, Modifiers, MouseButton, Pixels, Point, TestAppContext, VisualTestContext, WindowHandle,
    point, px,
};

const TIMEOUT: Duration = Duration::from_secs(10);

/// What every terminal runs: says `ready:`, and then every byte it is
/// given, in hex with a dot after it. Nothing is echoed, so what is on
/// the screen is what the program was given.
const BYTES: &str = r#"stty raw -echo; printf ready:; while :; do b=$(dd bs=1 count=1 2>/dev/null | od -An -tx1 | tr -d ' \n'); [ -n "$b" ] || exit; printf '%s.' "$b"; done"#;

/// The same program, after asking for reports of clicks, as vim and
/// Claude Code do.
fn reporting() -> String {
    format!(r#"printf '\033[?1000h\033[?1006h'; {BYTES}"#)
}

struct Benterm {
    window: WindowHandle<BentermWindow>,
    cx: VisualTestContext,
    socket: PathBuf,
    _dir: tempfile::TempDir,
}

fn open(cx: &mut TestAppContext) -> Benterm {
    open_running(BYTES, cx)
}

/// Opens a window as the app does, with a socket of its own, whose
/// terminals run `script`.
fn open_running(script: &str, cx: &mut TestAppContext) -> Benterm {
    cx.executor().allow_parking();
    cx.update(bind_keys);
    let dir = tempfile::Builder::new()
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap();
    let socket = dir.path().join("benterm.sock");
    let start: ipc::StartTerminal = Rc::new(StartedTerminal::spawn);
    let pty = PtyConfig::command("/bin/sh", ["-c", script]);
    let feed = ConfigFeed::defaults();
    let window = cx
        .update(|cx| ipc::open_window(pty, start, Some(socket.clone()), feed, cx))
        .unwrap();
    let mut cx = VisualTestContext::from_window(window.into(), cx);
    cx.update(|window, _| window.activate_window());
    cx.run_until_parked();
    let mut benterm = Benterm {
        window,
        cx,
        socket,
        _dir: dir,
    };
    let pane = benterm.focused();
    benterm.wait_for_ready(pane);
    benterm
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

    /// Makes the program of `pane` an agent at work, which gives it a
    /// row in the session list.
    fn work_in(&mut self, pane: PaneId) {
        let hook = ReportHook {
            pane: ApiPaneId::new(pane.get()),
            agent: "claude".into(),
            event: "UserPromptSubmit".into(),
            tool_name: None,
            notification_type: None,
            session_id: Some("s1".into()),
            agent_id: None,
            tool_input: None,
            tool_response: None,
        };
        self.call(hook).unwrap();
        self.wait_until("the row", |benterm| benterm.has_row(pane));
    }

    fn has_row(&mut self, pane: PaneId) -> bool {
        self.root(|root, cx| root.session_rows(cx))
            .iter()
            .any(|row| row.pane == pane)
    }

    /// Starts the agent `name` behind the window, as a child of
    /// `parent`, and returns the id of its terminal once it has a row
    /// and its program runs.
    fn spawn_behind(&mut self, name: &str, parent: PaneId) -> PaneId {
        let request = SpawnAgent {
            name: name.into(),
            command: vec!["/bin/sh".into(), "-c".into(), BYTES.into()],
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

    fn screen(&mut self, pane: PaneId) -> String {
        let pane = ApiPaneId::new(pane.get());
        self.call(ReadOutput { pane, lines: None }).unwrap().text
    }

    fn wait_for_ready(&mut self, pane: PaneId) {
        self.wait_until("the program", |benterm| {
            benterm.screen(pane).contains("ready:")
        });
    }

    /// The bytes the program of `pane` says it was given.
    fn given(&mut self, pane: PaneId) -> String {
        let screen = self.screen(pane);
        let start = screen.find("ready:").expect("the program runs") + "ready:".len();
        screen[start..].split_whitespace().collect()
    }

    fn focused(&mut self) -> PaneId {
        self.root(|root, cx| root.grid(cx).read(cx).focused())
    }

    fn tabs(&mut self) -> Vec<TabId> {
        self.root(|root, cx| root.tabs().read(cx).tabs())
    }

    fn selected_tab(&mut self) -> TabId {
        self.root(|root, cx| root.tabs().read(cx).selected())
    }

    fn pane_bounds(&mut self) -> Vec<(PaneId, Bounds<Pixels>)> {
        self.root(|root, cx| root.grid(cx).read(cx).pane_bounds())
    }

    fn draw(&mut self) {
        self.cx.run_until_parked();
        let _ = self
            .window
            .update(&mut self.cx, |_, window, _| window.refresh());
        self.cx.run_until_parked();
    }

    fn bounds_of(&mut self, selector: &str) -> Bounds<Pixels> {
        self.draw();
        let selector: &'static str = selector.to_owned().leak();
        self.cx
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("{selector} is drawn"))
    }

    fn right_click_at(&mut self, at: Point<Pixels>, modifiers: Modifiers) {
        self.cx
            .simulate_mouse_down(at, MouseButton::Right, modifiers);
        self.cx.simulate_mouse_up(at, MouseButton::Right, modifiers);
        self.draw();
    }

    fn right_click(&mut self, selector: &str) {
        let at = self.bounds_of(selector).center();
        self.right_click_at(at, Modifiers::none());
    }

    fn click_at(&mut self, at: Point<Pixels>) {
        self.cx.simulate_click(at, Modifiers::none());
        self.draw();
    }

    /// The items of the open menu, or none if no menu is open.
    fn menu(&mut self) -> Option<Vec<MenuItemShown>> {
        let menu = self.root(|root, _| root.context_menu())?;
        Some(menu.read_with(&self.cx, |menu, _| menu.items().to_vec()))
    }

    fn menu_names(&mut self) -> Vec<String> {
        self.menu()
            .expect("the menu is open")
            .into_iter()
            .map(|item| item.name)
            .collect()
    }

    /// Clicks the item of the open menu named `name`.
    fn choose(&mut self, name: &str) {
        let names = self.menu_names();
        let index = names
            .iter()
            .position(|item| item == name)
            .unwrap_or_else(|| panic!("the menu has no {name}: {names:?}"));
        let at = self
            .bounds_of(&format!("context-menu-item-{index}"))
            .center();
        self.click_at(at);
    }

    /// The middle of `col` on the `row`th row of the pane `pane`.
    fn cell(&mut self, pane: PaneId, row: usize, col: usize) -> Point<Pixels> {
        let bounds = self
            .pane_bounds()
            .into_iter()
            .find(|(id, _)| *id == pane)
            .expect("the pane is on view")
            .1;
        let cell = self.root(|root, cx| {
            root.terminal_with_id(pane, cx)
                .unwrap()
                .read(cx)
                .cell_size()
        });
        let padding = px(Theme::default().terminal.padding);
        bounds.origin
            + point(
                padding + cell.width * (col as f32 + 0.5),
                padding + cell.height * (row as f32 + 0.5),
            )
    }
}

fn shift() -> Modifiers {
    Modifiers {
        shift: true,
        ..Modifiers::none()
    }
}

#[gpui::test]
fn a_tab_has_new_tab_close_close_other_tabs_and_rename_with_their_keys(cx: &mut TestAppContext) {
    let mut benterm = open(cx);
    benterm.right_click("tab-0");
    let items = benterm.menu().expect("the menu is open");
    let shown: Vec<(&str, &str, bool)> = items
        .iter()
        .map(|item| (item.name.as_str(), item.keys.as_str(), item.enabled))
        .collect();
    assert_eq!(
        shown,
        [
            ("New Tab", "⌘T", true),
            // No key closes a whole tab: Cmd-W closes the focused pane.
            ("Close", "", true),
            // There is no other tab to close.
            ("Close Other Tabs", "⌥⌘W", false),
            ("Rename", "⌥⌘R", true),
        ]
    );
    assert!(benterm.cx.debug_bounds("context-menu").is_some());
}

#[gpui::test]
fn close_other_tabs_leaves_only_the_tab_that_was_clicked(cx: &mut TestAppContext) {
    let mut benterm = open(cx);
    benterm.cx.simulate_keystrokes("cmd-t cmd-t cmd-1");
    let tabs = benterm.tabs();
    assert_eq!(tabs.len(), 3);

    benterm.right_click("tab-2");
    benterm.choose("Close Other Tabs");

    assert_eq!(benterm.tabs(), [tabs[2]]);
    assert_eq!(benterm.selected_tab(), tabs[2]);
    assert!(benterm.menu().is_none());
}

#[gpui::test]
fn close_closes_the_tab_that_was_clicked_and_not_the_selected_one(cx: &mut TestAppContext) {
    let mut benterm = open(cx);
    benterm.cx.simulate_keystrokes("cmd-t cmd-t cmd-1");
    let tabs = benterm.tabs();

    benterm.right_click("tab-1");
    benterm.choose("Close");

    assert_eq!(benterm.tabs(), [tabs[0], tabs[2]]);
    assert_eq!(benterm.selected_tab(), tabs[0]);
}

#[gpui::test]
fn a_workspace_row_has_new_workspace_close_and_rename(cx: &mut TestAppContext) {
    let mut benterm = open(cx);
    benterm.right_click("workspace-0");
    assert_eq!(benterm.menu_names(), ["New Workspace", "Close", "Rename"]);
}

#[gpui::test]
fn rename_starts_typing_the_name_of_the_row_that_was_clicked(cx: &mut TestAppContext) {
    let mut benterm = open(cx);
    benterm.cx.simulate_keystrokes("cmd-n");
    let workspaces = benterm.root(|root, _| root.workspaces());
    assert_eq!(
        benterm.root(|root, _| root.selected_workspace()),
        workspaces[1]
    );

    benterm.right_click("workspace-0");
    benterm.choose("Rename");

    assert_eq!(benterm.root(|root, _| root.renaming()), Some(workspaces[0]));
}

#[gpui::test]
fn an_agent_in_a_pane_has_go_to_pane_peek_and_close(cx: &mut TestAppContext) {
    let mut benterm = open(cx);
    let pane = benterm.focused();
    benterm.work_in(pane);
    benterm.right_click("session-0");
    assert_eq!(benterm.menu_names(), ["Go to Pane", "Peek", "Close"]);
}

#[gpui::test]
fn an_agent_behind_the_window_is_peeked_at_and_closed_from_its_row(cx: &mut TestAppContext) {
    let mut benterm = open(cx);
    let pane = benterm.focused();
    let w1 = benterm.spawn_behind("w1", pane);
    let row = benterm
        .root(|root, cx| root.session_rows(cx))
        .iter()
        .position(|row| row.pane == w1)
        .unwrap();
    let selector = format!("session-{row}");

    benterm.right_click(&selector);
    assert_eq!(benterm.menu_names(), ["Open as Tab", "Peek", "Close"]);
    benterm.choose("Peek");
    assert_eq!(benterm.root(|root, _| root.peeked()), Some(w1));

    benterm.right_click(&selector);
    benterm.choose("Close");
    benterm.wait_until("the terminal of w1 ends", |benterm| {
        !benterm.has_row(w1) && benterm.root(|root, cx| root.terminal_with_id(w1, cx).is_none())
    });
    assert_eq!(benterm.root(|root, _| root.peeked()), None);
    // The pane the agent was started from is still there.
    assert_eq!(benterm.focused(), pane);
}

#[gpui::test]
fn a_pane_has_split_close_copy_and_paste_and_copy_needs_a_selection(cx: &mut TestAppContext) {
    let mut benterm = open(cx);
    let pane = benterm.focused();
    let at = benterm.cell(pane, 3, 3);
    benterm.right_click_at(at, Modifiers::none());
    let items = benterm.menu().expect("the menu is open");
    let shown: Vec<(&str, bool)> = items
        .iter()
        .map(|item| (item.name.as_str(), item.enabled))
        .collect();
    assert_eq!(
        shown,
        [
            ("Split Right", true),
            ("Split Down", true),
            ("Close", true),
            ("Copy", false),
            ("Paste", true),
        ]
    );
    assert_eq!(items[0].keys, "⌘D");
    assert_eq!(items[3].keys, "⌘C");
}

#[gpui::test]
fn split_down_splits_the_pane_that_was_clicked(cx: &mut TestAppContext) {
    let mut benterm = open(cx);
    let left = benterm.focused();
    benterm.cx.simulate_keystrokes("cmd-d");
    let right = benterm.focused();
    assert_ne!(left, right);
    benterm.wait_for_ready(right);

    let at = benterm.cell(left, 1, 1);
    benterm.right_click_at(at, Modifiers::none());
    benterm.choose("Split Down");

    let bounds = benterm.pane_bounds();
    assert_eq!(bounds.len(), 3);
    let place = |id: PaneId| bounds.iter().find(|(pane, _)| *pane == id).unwrap().1;
    let new = bounds
        .iter()
        .find(|(id, _)| *id != left && *id != right)
        .unwrap()
        .1;
    // The new pane is under the left one, and the right one is as tall
    // as before.
    assert_eq!(new.origin.x, place(left).origin.x);
    assert!(new.origin.y > place(left).origin.y);
    assert!(place(right).size.height > new.size.height);
}

#[gpui::test]
fn copy_copies_the_selection_of_the_pane(cx: &mut TestAppContext) {
    let mut benterm = open(cx);
    let pane = benterm.focused();
    let from = benterm.cell(pane, 0, 0);
    // A cell is selected once the drag passes its middle.
    let to = benterm.cell(pane, 0, 5);
    benterm
        .cx
        .simulate_mouse_down(from, MouseButton::Left, Modifiers::none());
    benterm
        .cx
        .simulate_mouse_move(to, Some(MouseButton::Left), Modifiers::none());
    benterm
        .cx
        .simulate_mouse_up(to, MouseButton::Left, Modifiers::none());

    benterm.right_click_at(to, Modifiers::none());
    let copy = benterm
        .menu()
        .expect("the menu is open")
        .into_iter()
        .find(|item| item.name == "Copy")
        .unwrap();
    assert!(copy.enabled);
    benterm.choose("Copy");

    let copied = benterm
        .cx
        .update(|_, cx| cx.read_from_clipboard().and_then(|item| item.text()));
    assert_eq!(copied.as_deref(), Some("ready"));
}

#[gpui::test]
fn a_program_that_asks_for_clicks_gets_the_right_click_and_shift_opens_the_menu(
    cx: &mut TestAppContext,
) {
    let mut benterm = open_running(&reporting(), cx);
    let pane = benterm.focused();
    let at = benterm.cell(pane, 0, 0);

    benterm.right_click_at(at, Modifiers::none());
    // A press and a release of the right button, in SGR.
    benterm.wait_until("the program is given the click", |benterm| {
        benterm.given(pane).starts_with("1b.5b.3c.32.3b.")
    });
    assert!(benterm.menu().is_none());

    benterm.right_click_at(at, shift());
    assert!(benterm.menu().is_some());
}

#[gpui::test]
fn the_arrows_and_enter_run_an_item_and_escape_closes_the_menu(cx: &mut TestAppContext) {
    let mut benterm = open(cx);
    benterm.cx.simulate_keystrokes("cmd-t");
    let tabs = benterm.tabs();

    benterm.right_click("tab-0");
    benterm.cx.simulate_keystrokes("escape");
    benterm.draw();
    assert!(benterm.menu().is_none());
    assert_eq!(benterm.tabs(), tabs);

    // The second item is Close.
    benterm.right_click("tab-0");
    benterm.cx.simulate_keystrokes("down enter");
    benterm.draw();
    assert!(benterm.menu().is_none());
    assert_eq!(benterm.tabs(), [tabs[1]]);
}

#[gpui::test]
fn a_click_outside_closes_the_menu_and_runs_nothing(cx: &mut TestAppContext) {
    let mut benterm = open(cx);
    let pane = benterm.focused();
    benterm.right_click("tab-0");
    assert!(benterm.menu().is_some());

    let away = benterm.cell(pane, 10, 30);
    benterm.click_at(away);

    assert!(benterm.menu().is_none());
    assert_eq!(benterm.tabs().len(), 1);
    // The program was given no key.
    assert_eq!(benterm.given(pane), "");
}

#[gpui::test]
fn the_keys_of_close_other_tabs_keep_the_selected_tab(cx: &mut TestAppContext) {
    let mut benterm = open(cx);
    benterm.cx.simulate_keystrokes("cmd-t cmd-t cmd-2");
    let tabs = benterm.tabs();

    benterm.cx.simulate_keystrokes("cmd-alt-w");

    assert_eq!(benterm.tabs(), [tabs[1]]);
}

#[gpui::test]
fn the_keys_of_close_session_end_the_agent_in_the_peek(cx: &mut TestAppContext) {
    let mut benterm = open(cx);
    let pane = benterm.focused();
    let w1 = benterm.spawn_behind("w1", pane);
    let peeked = benterm
        .window
        .update(&mut benterm.cx, |root, window, cx| {
            root.peek(w1, window, cx)
        })
        .unwrap();
    assert!(peeked);

    benterm.cx.simulate_keystrokes("cmd-alt-.");

    benterm.wait_until("the terminal of w1 ends", |benterm| {
        !benterm.has_row(w1) && benterm.root(|root, cx| root.terminal_with_id(w1, cx).is_none())
    });
    assert_eq!(benterm.root(|root, _| root.peeked()), None);
    assert_eq!(
        benterm.root(|root, cx| root.grid(cx).read(cx).panes()),
        [pane]
    );
}

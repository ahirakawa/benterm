//! Browser panes: opening one, and treating it as any other pane.

#[path = "common/pages.rs"]
mod pages;

use benterm_layout::{PaneId, SplitDirection};
use benterm_terminal::PtyConfig;
use benterm_ui::browser::{BrowserError, OpenBrowser, PageEdit, PageEvent, PageFailure};
use benterm_ui::settings::ConfigFeed;
use benterm_ui::{BentermWindow, PaneGrid, bind_keys, open_terminal_window};
use gpui::{
    Bounds, Entity, Modifiers, MouseButton, Pixels, TestAppContext, VisualTestContext,
    WindowHandle, point, px,
};

use pages::Pages;

const URL: &str = "http://127.0.0.1:8080/docs";

struct App {
    window: WindowHandle<BentermWindow>,
    cx: VisualTestContext,
    pages: Pages,
}

/// Opens a window with one shell, the app's keys, and pages that only
/// remember what they are asked.
fn open(cx: &mut TestAppContext) -> App {
    cx.executor().allow_parking();
    cx.update(bind_keys);
    let pages = cx.update(Pages::install);
    let pty = PtyConfig::command("/bin/sh", ["-c", "sleep 100"]);
    let window = cx
        .update(|cx| open_terminal_window(pty, ConfigFeed::defaults(), cx))
        .unwrap();
    let cx = VisualTestContext::from_window(window.into(), cx);
    cx.run_until_parked();
    App { window, cx, pages }
}

impl App {
    fn grid(&mut self) -> Entity<PaneGrid> {
        self.window
            .update(&mut self.cx, |root, _, cx| root.grid(cx))
            .unwrap()
    }

    fn panes(&mut self) -> Vec<PaneId> {
        self.grid().read_with(&self.cx, |grid, _| grid.panes())
    }

    fn focused(&mut self) -> PaneId {
        self.grid().read_with(&self.cx, |grid, _| grid.focused())
    }

    fn bounds(&mut self, id: PaneId) -> Bounds<Pixels> {
        self.grid().read_with(&self.cx, |grid, _| {
            let all = grid.pane_bounds();
            all.into_iter().find(|(pane, _)| *pane == id).unwrap().1
        })
    }

    /// Opens a browser pane at the right of the focused pane.
    fn browse(&mut self, url: Option<&str>, focus: bool) -> Result<PaneId, BrowserError> {
        let opened = self.window.update(&mut self.cx, |root, window, cx| {
            root.open_browser(url, None, SplitDirection::Right, focus, window, cx)
        });
        self.cx.run_until_parked();
        opened.unwrap()
    }

    /// Whether GPUI's focus, which decides where the app's keys are
    /// looked up, is on the page of the browser pane `id`.
    fn page_is_focused(&mut self, id: PaneId) -> bool {
        let handle = self.grid().read_with(&self.cx, |grid, cx| {
            grid.browser(id).unwrap().read(cx).focus_handle().clone()
        });
        self.cx.update(|window, _| handle.is_focused(window))
    }

    fn page_is_focused_in(&mut self, grid: &Entity<PaneGrid>, id: PaneId) -> bool {
        let handle = grid.read_with(&self.cx, |grid, cx| {
            grid.browser(id).unwrap().read(cx).focus_handle().clone()
        });
        self.cx.update(|window, _| handle.is_focused(window))
    }

    fn terminal_is_focused(&mut self, id: PaneId) -> bool {
        let handle = self.grid().read_with(&self.cx, |grid, cx| {
            grid.terminal(id).unwrap().read(cx).focus_handle().clone()
        });
        self.cx.update(|window, _| handle.is_focused(window))
    }
}

#[gpui::test]
fn opening_a_url_adds_a_pane_that_loads_it(cx: &mut TestAppContext) {
    let mut app = open(cx);
    let first = app.focused();

    let new = app.browse(Some(URL), false).unwrap();

    assert_eq!(app.panes(), [first, new]);
    assert_eq!(app.pages.len(), 1);
    assert_eq!(app.pages.page(0).borrow().loads, [URL]);
    let grid = app.grid();
    grid.read_with(&app.cx, |grid, cx| {
        assert!(grid.terminal(new).is_none());
        assert_eq!(
            grid.browser(new).unwrap().read(cx).url().as_deref(),
            Some(URL)
        );
        assert!(grid.browser(first).is_none());
    });
    // The pane is at the right of the one it was opened from.
    let (left, right) = (app.bounds(first), app.bounds(new));
    assert_eq!(left.origin.y, right.origin.y);
    assert_eq!(right.origin.x, left.right());
}

#[gpui::test]
fn a_browser_opened_for_a_program_leaves_the_keyboard_where_it_is(cx: &mut TestAppContext) {
    let mut app = open(cx);
    let first = app.focused();

    let new = app.browse(Some(URL), false).unwrap();

    assert_eq!(app.focused(), first);
    assert!(app.terminal_is_focused(first));
    assert!(!app.page_is_focused(new));
    assert!(!app.pages.page(0).borrow().has_keyboard());
}

#[gpui::test]
fn a_browser_opened_by_the_user_gets_the_keyboard(cx: &mut TestAppContext) {
    let mut app = open(cx);

    let new = app.browse(Some(URL), true).unwrap();

    assert_eq!(app.focused(), new);
    assert!(app.page_is_focused(new));
    assert!(app.pages.page(0).borrow().has_keyboard());
}

#[gpui::test]
fn an_address_that_is_no_web_page_opens_no_pane(cx: &mut TestAppContext) {
    let mut app = open(cx);
    for url in ["file:///etc/passwd", "javascript:alert(1)", "not a url", ""] {
        assert_eq!(app.browse(Some(url), false), Err(BrowserError::NotAPage));
    }
    assert_eq!(app.panes().len(), 1);
    assert_eq!(app.pages.len(), 0);
}

#[gpui::test]
fn a_browser_is_opened_beside_the_pane_that_is_named(cx: &mut TestAppContext) {
    let mut app = open(cx);
    let first = app.focused();
    app.cx.simulate_keystrokes("cmd-d");
    let second = app.focused();

    let opened = app.window.update(&mut app.cx, |root, window, cx| {
        root.open_browser(
            Some(URL),
            Some(first),
            SplitDirection::Down,
            false,
            window,
            cx,
        )
    });
    let new = opened.unwrap().unwrap();
    app.cx.run_until_parked();

    assert_eq!(app.panes(), [first, new, second]);
    let (above, below) = (app.bounds(first), app.bounds(new));
    assert_eq!(below.origin.x, above.origin.x);
    assert_eq!(below.origin.y, above.bottom());

    // A pane that is gone.
    app.cx.simulate_keystrokes("cmd-w");
    assert_eq!(app.panes(), [first, new]);
    let opened = app.window.update(&mut app.cx, |root, window, cx| {
        root.open_browser(
            Some(URL),
            Some(second),
            SplitDirection::Right,
            false,
            window,
            cx,
        )
    });
    assert_eq!(opened.unwrap(), Err(BrowserError::UnknownPane));
}

#[gpui::test]
fn a_terminal_is_split_off_a_browser_and_closing_leaves_the_rest_the_room(cx: &mut TestAppContext) {
    let mut app = open(cx);
    let first = app.focused();
    let full = app.bounds(first);
    let browser = app.browse(Some(URL), true).unwrap();

    // Split from the browser pane: the new pane has a shell.
    app.cx.simulate_keystrokes("cmd-d");
    let shell = app.focused();
    assert_eq!(app.panes(), [first, browser, shell]);
    let grid = app.grid();
    assert!(grid.read_with(&app.cx, |grid, _| grid.terminal(shell).is_some()));
    assert!(app.terminal_is_focused(shell));
    assert!(!app.pages.page(0).borrow().has_keyboard());

    // Closing the shell gives the browser its room and the keyboard.
    let before = app.bounds(browser);
    app.cx.simulate_keystrokes("cmd-w");
    assert_eq!(app.panes(), [first, browser]);
    assert_eq!(app.focused(), browser);
    assert!(app.page_is_focused(browser));
    assert!(app.bounds(browser).size.width > before.size.width);

    // Closing the browser leaves the first pane the whole tab, and
    // the page is given up.
    app.cx.simulate_keystrokes("cmd-w");
    assert_eq!(app.panes(), [first]);
    assert_eq!(app.bounds(first), full);
    assert!(app.terminal_is_focused(first));
    assert!(app.pages.page(0).borrow().closed);
}

#[gpui::test]
fn cmd_alt_arrows_move_the_focus_to_a_browser_and_back(cx: &mut TestAppContext) {
    let mut app = open(cx);
    let first = app.focused();
    let browser = app.browse(Some(URL), false).unwrap();
    let page = app.pages.page(0);

    app.cx.simulate_keystrokes("cmd-alt-right");
    assert_eq!(app.focused(), browser);
    assert!(app.page_is_focused(browser));
    assert!(page.borrow().has_keyboard());

    app.cx.simulate_keystrokes("cmd-alt-left");
    assert_eq!(app.focused(), first);
    assert!(app.terminal_is_focused(first));
    assert!(!page.borrow().has_keyboard());
}

#[gpui::test]
fn dragging_the_border_resizes_a_browser_and_its_page(cx: &mut TestAppContext) {
    let mut app = open(cx);
    let first = app.focused();
    let browser = app.browse(Some(URL), false).unwrap();
    let page = app.pages.page(0);
    let before = app.bounds(first);
    let browser_before = app.bounds(browser);
    let page_before = page.borrow().place().expect("the page is shown");
    let border = point(before.right(), before.center().y);

    // Aim just beside the one-third snap point.
    let width = before.size.width + browser_before.size.width;
    let snapped_width = px((f32::from(width) / 3.).round());
    let growth = before.size.width - snapped_width;
    assert!(growth > px(0.));
    let target = point(before.left() + snapped_width + px(4.), border.y);
    app.cx
        .simulate_mouse_down(border, MouseButton::Left, Modifiers::none());
    app.cx
        .simulate_mouse_move(target, MouseButton::Left, Modifiers::none());
    app.cx
        .simulate_mouse_up(target, MouseButton::Left, Modifiers::none());
    app.cx.run_until_parked();

    assert_eq!(app.bounds(first).size.width, snapped_width);
    let browser_after = app.bounds(browser);
    assert_eq!(browser_after.origin.x, app.bounds(first).right());
    assert_eq!(browser_after.size.width, browser_before.size.width + growth);
    let page_after = page.borrow().place().expect("the page is shown");
    assert_eq!(page_after.size.width, page_before.size.width + growth);
    assert_eq!(page_after.origin.x, page_before.origin.x - growth);
}

/// The page is inside its pane, below the bar with the address, and
/// leaves the pane's outline free.
#[gpui::test]
fn the_page_is_put_inside_its_pane_below_the_bar(cx: &mut TestAppContext) {
    let mut app = open(cx);
    let browser = app.browse(Some(URL), false).unwrap();

    let pane = app.bounds(browser);
    let page = app
        .pages
        .page(0)
        .borrow()
        .place()
        .expect("the page is shown");
    assert!(
        page.origin.y >= pane.origin.y + px(20.),
        "{page:?} {pane:?}"
    );
    // The borders between panes are grabbed up to 3 pixels to each
    // side. The page takes every press on it, so it stays out of there.
    assert_eq!(page.origin.x, pane.origin.x + px(3.));
    assert_eq!(page.right(), pane.right() - px(3.));
    assert_eq!(page.bottom(), pane.bottom() - px(3.));
}

#[gpui::test]
fn cmd_t_in_a_browser_opens_a_tab_and_hides_the_page(cx: &mut TestAppContext) {
    let mut app = open(cx);
    app.browse(Some(URL), true).unwrap();
    let page = app.pages.page(0);
    assert!(page.borrow().place().is_some());
    let tabs = |app: &mut App| {
        app.window
            .update(&mut app.cx, |root, _, cx| root.tabs().read(cx).tabs().len())
            .unwrap()
    };
    assert_eq!(tabs(&mut app), 1);

    app.cx.simulate_keystrokes("cmd-t");
    app.cx.run_until_parked();

    assert_eq!(tabs(&mut app), 2);
    assert_eq!(page.borrow().place(), None);
    assert!(!page.borrow().has_keyboard());
    assert!(!page.borrow().closed);

    // Back in the first tab the page is shown where it was, with the
    // keyboard.
    app.cx.simulate_keystrokes("cmd-1");
    app.cx.run_until_parked();
    assert!(page.borrow().place().is_some());
    assert!(page.borrow().has_keyboard());
}

impl App {
    fn browser(&mut self, id: PaneId) -> Entity<benterm_ui::browser::BrowserView> {
        self.grid()
            .read_with(&self.cx, |grid, _| grid.browser(id).unwrap().clone())
    }

    fn address_is_focused(&mut self, id: PaneId) -> bool {
        let browser = self.browser(id);
        let handle = browser.read_with(&self.cx, |browser, cx| browser.address_focus(cx));
        self.cx.update(|window, _| handle.is_focused(window))
    }

    fn click(&mut self, selector: &'static str) {
        let bounds = self
            .cx
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("{selector} is not drawn"));
        self.cx.simulate_click(bounds.center(), Modifiers::none());
        self.cx.run_until_parked();
    }
}

#[test]
fn what_is_typed_as_an_address_is_made_the_address_of_a_page() {
    use benterm_ui::browser::address_url;
    let url = |text: &str| address_url(text);
    assert_eq!(url("example.com").as_deref(), Some("https://example.com"));
    assert_eq!(
        url("  example.com/a?b=c#d ").as_deref(),
        Some("https://example.com/a?b=c#d")
    );
    assert_eq!(
        url("localhost:3000/x").as_deref(),
        Some("http://localhost:3000/x")
    );
    assert_eq!(url("localhost").as_deref(), Some("http://localhost"));
    assert_eq!(
        url("127.0.0.1:8080").as_deref(),
        Some("http://127.0.0.1:8080")
    );
    assert_eq!(url("[::1]:3000").as_deref(), Some("http://[::1]:3000"));
    assert_eq!(url("devbox:8000").as_deref(), Some("http://devbox:8000"));
    assert_eq!(
        url("HTTP://Example.com/").as_deref(),
        Some("HTTP://Example.com/")
    );
    assert_eq!(url("https://a.b/c").as_deref(), Some("https://a.b/c"));
    // Not an address: nothing is looked up for it anywhere.
    for text in [
        "",
        "   ",
        "hello",
        "two words",
        "what is rust.rs",
        "file:///etc/passwd",
        "javascript:alert(1)",
        "about:blank",
        "ftp://example.com",
        "[::1]junk",
        "localhost:99999",
        "http://:80/",
    ] {
        assert_eq!(url(text), None, "{text:?}");
    }
}

#[gpui::test]
fn cmd_l_moves_to_the_address_and_enter_opens_what_is_typed(cx: &mut TestAppContext) {
    let mut app = open(cx);
    let pane = app.browse(Some(URL), true).unwrap();
    let page = app.pages.page(0);

    app.cx.simulate_keystrokes("cmd-l");
    app.cx.run_until_parked();
    assert!(app.address_is_focused(pane));
    assert!(!page.borrow().has_keyboard());
    // The address is there to be typed over.
    let browser = app.browser(pane);
    assert_eq!(
        browser.read_with(&app.cx, |browser, cx| browser.address_text(cx)),
        URL
    );

    app.cx.simulate_input("localhost:3000/new");
    app.cx.simulate_keystrokes("enter");
    app.cx.run_until_parked();

    assert_eq!(page.borrow().loads, [URL, "http://localhost:3000/new"]);
    assert!(app.page_is_focused(pane));
    assert!(page.borrow().has_keyboard());
}

#[gpui::test]
fn escape_in_the_address_gives_the_page_the_keyboard_back(cx: &mut TestAppContext) {
    let mut app = open(cx);
    let pane = app.browse(Some(URL), true).unwrap();
    let page = app.pages.page(0);

    app.cx.simulate_keystrokes("cmd-l");
    app.cx.simulate_input("example.com");
    app.cx.simulate_keystrokes("escape");
    app.cx.run_until_parked();

    assert_eq!(page.borrow().loads, [URL]);
    assert!(app.page_is_focused(pane));
    assert!(page.borrow().has_keyboard());
    let browser = app.browser(pane);
    assert_eq!(
        browser.read_with(&app.cx, |browser, cx| browser.address_text(cx)),
        URL
    );
}

#[gpui::test]
fn what_is_no_address_stays_in_the_field(cx: &mut TestAppContext) {
    let mut app = open(cx);
    let pane = app.browse(Some(URL), true).unwrap();

    app.cx.simulate_keystrokes("cmd-l");
    app.cx.simulate_input("two words");
    app.cx.simulate_keystrokes("enter");
    app.cx.run_until_parked();

    assert_eq!(app.pages.page(0).borrow().loads, [URL]);
    assert!(app.address_is_focused(pane));
    let browser = app.browser(pane);
    assert_eq!(
        browser.read_with(&app.cx, |browser, cx| browser.address_text(cx)),
        "two words"
    );
}

#[gpui::test]
fn the_keys_of_a_browser_go_back_forward_and_load_again(cx: &mut TestAppContext) {
    let mut app = open(cx);
    app.browse(Some(URL), true).unwrap();
    let page = app.pages.page(0);

    // Nowhere to go yet.
    app.cx.simulate_keystrokes("cmd-[ cmd-]");
    assert_eq!((page.borrow().backs, page.borrow().forwards), (0, 0));

    page.borrow_mut().state.can_go_back = true;
    page.borrow().tell(PageEvent::Changed);
    app.cx.run_until_parked();
    app.cx.simulate_keystrokes("cmd-[");
    assert_eq!((page.borrow().backs, page.borrow().forwards), (1, 0));

    page.borrow_mut().state.can_go_forward = true;
    page.borrow().tell(PageEvent::Changed);
    app.cx.run_until_parked();
    app.cx.simulate_keystrokes("cmd-]");
    assert_eq!((page.borrow().backs, page.borrow().forwards), (1, 1));

    app.cx.simulate_keystrokes("cmd-r");
    assert_eq!(page.borrow().reloads, 1);

    // The keys work from the address too.
    app.cx.simulate_keystrokes("cmd-l cmd-r");
    assert_eq!(page.borrow().reloads, 2);
}

#[gpui::test]
fn the_keys_of_a_browser_do_nothing_in_a_terminal(cx: &mut TestAppContext) {
    let mut app = open(cx);
    app.browse(Some(URL), false).unwrap();
    let page = app.pages.page(0);
    page.borrow_mut().state.can_go_back = true;
    page.borrow().tell(PageEvent::Changed);
    app.cx.run_until_parked();

    app.cx.simulate_keystrokes("cmd-[ cmd-r");

    assert_eq!((page.borrow().backs, page.borrow().reloads), (0, 0));
}

/// Chooses the items of the Edit menu that the system does not hand to
/// the view with its keyboard. The menu bar hands their actions to the
/// window, as it does those of other items.
fn choose_edit_items(app: &mut App) {
    use benterm_ui::menu::{Cut, Redo, SelectAll, Undo};
    let actions: [&dyn gpui::Action; 4] = [&Undo, &Redo, &Cut, &SelectAll];
    for action in actions {
        app.cx
            .update(|window, cx| window.dispatch_action(action.boxed_clone(), cx));
        app.cx.run_until_parked();
    }
}

/// Undo and Redo of the menu bar reach the page that has the keyboard,
/// and so do Cut and Select All when something other than the page's
/// own view asks for them, as the command palette does.
#[gpui::test]
fn the_edit_items_are_done_by_the_page_with_the_keyboard(cx: &mut TestAppContext) {
    let mut app = open(cx);
    app.browse(Some(URL), true).unwrap();
    let page = app.pages.page(0);

    choose_edit_items(&mut app);

    assert_eq!(
        page.borrow().edits,
        [
            PageEdit::Undo,
            PageEdit::Redo,
            PageEdit::Cut,
            PageEdit::SelectAll
        ]
    );
}

/// A page that was clicked has the keyboard before GPUI's focus follows
/// it, as in a real window: the items still reach the page.
#[gpui::test]
fn the_edit_items_reach_a_page_that_was_clicked(cx: &mut TestAppContext) {
    let mut app = open(cx);
    app.browse(Some(URL), false).unwrap();
    let page = app.pages.page(0);
    page.borrow_mut().take_keyboard();
    app.cx.run_until_parked();

    choose_edit_items(&mut app);

    assert_eq!(
        page.borrow().edits,
        [
            PageEdit::Undo,
            PageEdit::Redo,
            PageEdit::Cut,
            PageEdit::SelectAll
        ]
    );
}

/// The keys of the Edit menu are done by the page that was clicked
/// through the pane, and not left to the menu bar to send on.
#[gpui::test]
fn the_edit_keys_are_done_by_a_page_that_was_clicked(cx: &mut TestAppContext) {
    let mut app = open(cx);
    let first = app.focused();
    app.browse(Some(URL), false).unwrap();
    let page = app.pages.page(0);
    page.borrow_mut().take_keyboard();
    app.cx.run_until_parked();

    app.cx.simulate_keystrokes("cmd-a cmd-x cmd-z cmd-shift-z");
    app.cx.run_until_parked();

    assert_eq!(
        page.borrow().edits,
        [
            PageEdit::SelectAll,
            PageEdit::Cut,
            PageEdit::Undo,
            PageEdit::Redo
        ]
    );
    // The terminal beside it was not asked.
    assert!(!app.terminal_is_focused(first));
}

/// Copy and Paste go to the page the same way as the other keys of the
/// Edit menu.
#[gpui::test]
fn cmd_c_and_cmd_v_are_done_by_a_page_that_was_clicked(cx: &mut TestAppContext) {
    let mut app = open(cx);
    app.browse(Some(URL), false).unwrap();
    let page = app.pages.page(0);
    page.borrow_mut().take_keyboard();
    app.cx.run_until_parked();

    app.cx.simulate_keystrokes("cmd-c cmd-v");
    app.cx.run_until_parked();

    assert_eq!(page.borrow().edits, [PageEdit::Copy, PageEdit::Paste]);
}

/// Cmd-V in the address types the clipboard there, and not in the page.
#[gpui::test]
fn cmd_v_in_the_address_pastes_into_the_address(cx: &mut TestAppContext) {
    let mut app = open(cx);
    let pane = app.browse(Some(URL), true).unwrap();
    let page = app.pages.page(0);
    app.cx.simulate_keystrokes("cmd-l");
    app.cx.run_until_parked();
    app.cx
        .write_to_clipboard(gpui::ClipboardItem::new_string("example.com".into()));

    app.cx.simulate_keystrokes("cmd-v");
    app.cx.run_until_parked();

    assert_eq!(page.borrow().edits, []);
    let browser = app.browser(pane);
    assert_eq!(
        browser.read_with(&app.cx, |browser, cx| browser.address_text(cx)),
        "example.com"
    );
}

/// While the address is typed, the keys are the address's.
#[gpui::test]
fn the_edit_keys_leave_the_page_alone_while_the_address_is_typed(cx: &mut TestAppContext) {
    let mut app = open(cx);
    let pane = app.browse(Some(URL), true).unwrap();
    let page = app.pages.page(0);
    app.cx.simulate_keystrokes("cmd-l");
    app.cx.run_until_parked();

    app.cx
        .simulate_keystrokes("cmd-a cmd-x cmd-z cmd-shift-z cmd-c");
    app.cx.run_until_parked();

    assert_eq!(page.borrow().edits, []);
    assert!(app.address_is_focused(pane));
}

/// What is typed in the address is not the page's to undo.
#[gpui::test]
fn the_edit_items_leave_the_page_alone_while_the_address_is_typed(cx: &mut TestAppContext) {
    let mut app = open(cx);
    app.browse(Some(URL), true).unwrap();
    let page = app.pages.page(0);
    app.cx.simulate_keystrokes("cmd-l");

    choose_edit_items(&mut app);

    assert_eq!(page.borrow().edits, []);
}

#[gpui::test]
fn the_edit_items_leave_a_page_without_the_keyboard_alone(cx: &mut TestAppContext) {
    let mut app = open(cx);
    app.browse(Some(URL), false).unwrap();
    let page = app.pages.page(0);

    choose_edit_items(&mut app);

    assert_eq!(page.borrow().edits, []);
}

#[gpui::test]
fn the_buttons_of_the_bar_go_back_forward_and_load_again(cx: &mut TestAppContext) {
    let mut app = open(cx);
    let pane = app.browse(Some(URL), false).unwrap();
    let page = app.pages.page(0);
    {
        let mut page = page.borrow_mut();
        page.state.can_go_back = true;
        page.state.can_go_forward = true;
    }
    page.borrow().tell(PageEvent::Changed);
    app.cx.run_until_parked();

    app.click("browser-back");
    app.click("browser-forward");
    app.click("browser-reload");

    let asked = page.borrow();
    assert_eq!((asked.backs, asked.forwards, asked.reloads), (1, 1, 1));
    drop(asked);
    // A click in the bar takes the pane, as one anywhere in a pane does.
    assert_eq!(app.focused(), pane);
}

#[gpui::test]
fn a_click_on_the_address_moves_the_keyboard_to_it(cx: &mut TestAppContext) {
    let mut app = open(cx);
    let pane = app.browse(Some(URL), false).unwrap();

    app.click("browser-address");

    assert_eq!(app.focused(), pane);
    assert!(app.address_is_focused(pane));
    assert!(!app.pages.page(0).borrow().has_keyboard());
}

#[gpui::test]
fn the_bar_shows_where_the_page_went(cx: &mut TestAppContext) {
    let mut app = open(cx);
    let pane = app.browse(Some(URL), true).unwrap();
    let page = app.pages.page(0);
    {
        let mut page = page.borrow_mut();
        page.state.url = Some("http://127.0.0.1:8080/next".into());
        page.state.title = Some("Next page".into());
    }
    page.borrow().tell(PageEvent::Changed);
    app.cx.run_until_parked();

    let browser = app.browser(pane);
    browser.read_with(&app.cx, |browser, cx| {
        assert_eq!(browser.address_text(cx), "http://127.0.0.1:8080/next");
        assert_eq!(browser.title(), "Next page");
    });
    // The tab is called what its focused pane is.
    let label = app.window.update(&mut app.cx, |root, _, cx| {
        let tabs = root.tabs().read(cx);
        tabs.label(tabs.selected(), cx)
    });
    assert_eq!(label.unwrap(), "Next page");
}

/// A page that could not be loaded shows nothing, so the pane says what
/// happened in its place.
#[gpui::test]
fn a_page_that_failed_is_hidden_and_the_pane_says_why(cx: &mut TestAppContext) {
    let mut app = open(cx);
    let pane = app.browse(Some(URL), true).unwrap();
    let page = app.pages.page(0);
    let shown = page.borrow().place();
    assert!(shown.is_some());
    assert!(app.cx.debug_bounds("browser-failure").is_none());

    let failure = PageFailure {
        url: "http://127.0.0.1:9/".into(),
        reason: "Could not connect to the server.".into(),
    };
    page.borrow_mut().state.failure = Some(failure.clone());
    page.borrow().tell(PageEvent::Changed);
    app.cx.run_until_parked();

    assert_eq!(page.borrow().place(), None);
    assert!(app.cx.debug_bounds("browser-failure").is_some());
    let browser = app.browser(pane);
    browser.read_with(&app.cx, |browser, _| {
        assert_eq!(browser.failure(), Some(&failure));
    });
    // The keys of the pane still work: the address can be typed over.
    assert!(app.page_is_focused(pane));
    // The page that is hidden is not edited behind the note.
    choose_edit_items(&mut app);
    assert_eq!(page.borrow().edits, []);

    // Another page is asked for: the page shows again.
    page.borrow_mut().state.failure = None;
    page.borrow().tell(PageEvent::Changed);
    app.cx.run_until_parked();
    assert_eq!(page.borrow().place(), shown);
    assert!(page.borrow().has_keyboard());
    assert!(app.cx.debug_bounds("browser-failure").is_none());
}

#[gpui::test]
fn a_terminated_page_recovers_three_times_before_offering_manual_reload(cx: &mut TestAppContext) {
    let mut app = open(cx);
    let pane = app.browse(Some(URL), true).unwrap();
    let page = app.pages.page(0);
    for attempt in 1..=4 {
        page.borrow().tell(PageEvent::ProcessTerminated);
        app.cx.run_until_parked();
        assert_eq!(page.borrow().loads, vec![URL; 1 + attempt.min(3)]);
    }

    let browser = app.browser(pane);
    browser.read_with(&app.cx, |browser, _| {
        assert!(browser.failure().unwrap().reason.contains("Reload"));
    });
    assert!(app.cx.debug_bounds("browser-failure").is_some());
    app.click("browser-reload");
    assert_eq!(page.borrow().loads, [URL; 5]);
    assert!(app.cx.debug_bounds("browser-failure").is_none());
}

/// A page that waits for its server shows what it showed before, which
/// may be nothing: the bar says that it is still at it.
#[gpui::test]
fn the_bar_says_that_a_page_is_loading(cx: &mut TestAppContext) {
    let mut app = open(cx);
    app.browse(Some(URL), true).unwrap();
    let page = app.pages.page(0);
    assert!(app.cx.debug_bounds("browser-loading").is_none());

    page.borrow_mut().state.loading = true;
    page.borrow().tell(PageEvent::Changed);
    app.cx.run_until_parked();
    assert!(app.cx.debug_bounds("browser-loading").is_some());

    page.borrow_mut().state.loading = false;
    page.borrow().tell(PageEvent::Changed);
    app.cx.run_until_parked();
    assert!(app.cx.debug_bounds("browser-loading").is_none());
}

/// The window does not see a click on a page: the page takes the
/// keyboard itself and says so.
#[gpui::test]
fn a_page_that_took_the_keyboard_makes_its_pane_the_focused_one(cx: &mut TestAppContext) {
    let mut app = open(cx);
    let first = app.focused();
    let pane = app.browse(Some(URL), false).unwrap();
    let page = app.pages.page(0);
    assert_eq!(app.focused(), first);

    page.borrow_mut().take_keyboard();
    app.cx.run_until_parked();

    assert_eq!(app.focused(), pane);
    assert!(app.page_is_focused(pane));
    // The keyboard was not taken from the page on the way.
    assert!(!page.borrow().keyboard.contains(&false));
}

/// The page says that it took the keyboard a moment after it did. By
/// then the keyboard may be elsewhere, and stays there.
#[gpui::test]
fn a_page_that_no_longer_has_the_keyboard_does_not_take_the_focus(cx: &mut TestAppContext) {
    let mut app = open(cx);
    let first = app.focused();
    let pane = app.browse(Some(URL), false).unwrap();
    let page = app.pages.page(0);

    // It was clicked, and the user moved on before the pane heard.
    page.borrow().tell(PageEvent::TookKeyboard);
    app.cx.run_until_parked();
    assert_eq!(app.focused(), first);
    assert!(app.terminal_is_focused(first));

    // The same for a page in a tab behind.
    app.cx.simulate_keystrokes("cmd-t");
    app.cx.run_until_parked();
    page.borrow().tell(PageEvent::TookKeyboard);
    app.cx.run_until_parked();
    let grid = app.window.update(&mut app.cx, |root, _, cx| {
        let tabs = root.tabs().read(cx);
        tabs.grid(tabs.tabs()[0]).unwrap().clone()
    });
    let grid = grid.unwrap();
    assert_eq!(grid.read_with(&app.cx, |grid, _| grid.focused()), first);
    assert!(!app.page_is_focused_in(&grid, pane));
}

#[gpui::test]
fn the_page_is_hidden_while_the_command_palette_is_over_it(cx: &mut TestAppContext) {
    let mut app = open(cx);
    app.browse(Some(URL), true).unwrap();
    let page = app.pages.page(0);
    let shown = page.borrow().place();
    assert!(shown.is_some());

    app.cx.simulate_keystrokes("cmd-k");
    app.cx.run_until_parked();
    assert_eq!(page.borrow().place(), None);
    assert!(!page.borrow().has_keyboard());

    app.cx.simulate_keystrokes("escape");
    app.cx.run_until_parked();
    assert_eq!(page.borrow().place(), shown);
    assert!(page.borrow().has_keyboard());
}

#[gpui::test]
fn the_page_is_hidden_under_another_view_of_the_pane_area(cx: &mut TestAppContext) {
    let mut app = open(cx);
    app.browse(Some(URL), true).unwrap();
    let page = app.pages.page(0);

    app.cx.simulate_keystrokes("cmd-shift-b");
    app.cx.run_until_parked();
    assert_eq!(page.borrow().place(), None);

    app.cx.simulate_keystrokes("cmd-shift-b");
    app.cx.run_until_parked();
    assert!(page.borrow().place().is_some());
}

#[gpui::test]
fn the_command_palette_opens_a_browser_with_the_keyboard_in_its_address(cx: &mut TestAppContext) {
    let mut app = open(cx);
    let first = app.focused();

    app.cx.simulate_keystrokes("cmd-k");
    app.cx.simulate_input("open browser");
    app.cx.simulate_keystrokes("enter");
    app.cx.run_until_parked();

    let panes = app.panes();
    assert_eq!(panes.len(), 2);
    assert_eq!(panes[0], first);
    assert_eq!(app.focused(), panes[1]);
    assert!(app.address_is_focused(panes[1]));
    assert_eq!(app.pages.page(0).borrow().loads, Vec::<String>::new());

    // What is typed there is opened.
    app.cx.simulate_input("example.com");
    app.cx.simulate_keystrokes("enter");
    app.cx.run_until_parked();
    assert_eq!(app.pages.page(0).borrow().loads, ["https://example.com"]);
    assert!(app.page_is_focused(panes[1]));
}

#[gpui::test]
fn the_menu_bar_opens_a_browser(cx: &mut TestAppContext) {
    use gpui::Action as _;
    let mut app = open(cx);
    let menus = benterm_ui::menu::menus();
    let pane = menus.iter().find(|menu| menu.name.as_ref() == "Pane");
    let item = pane.unwrap().items.iter().find_map(|item| match item {
        gpui::MenuItem::Action { name, action, .. } if name.as_ref() == "Open Browser" => {
            Some(action.boxed_clone())
        }
        _ => None,
    });
    let action = item.expect("the Pane menu opens a browser");
    assert_eq!(action.name(), OpenBrowser.name());

    // The platform hands the action of a chosen item to the window.
    app.cx.dispatch_action(OpenBrowser);
    app.cx.run_until_parked();

    let panes = app.panes();
    assert_eq!(panes.len(), 2);
    assert!(app.address_is_focused(panes[1]));
}

#[gpui::test]
fn cmd_shift_l_opens_a_browser_from_a_terminal_and_from_a_browser(cx: &mut TestAppContext) {
    let mut app = open(cx);

    app.cx.simulate_keystrokes("cmd-shift-l");
    app.cx.run_until_parked();
    let panes = app.panes();
    assert_eq!(panes.len(), 2);
    assert!(app.address_is_focused(panes[1]));

    app.cx.simulate_keystrokes("cmd-shift-l");
    app.cx.run_until_parked();
    let panes = app.panes();
    assert_eq!(panes.len(), 3);
    assert!(app.address_is_focused(panes[2]));
}

/// As Orca does with its tabs: a page nobody has looked at yet is not
/// fetched.
#[gpui::test]
fn a_page_in_a_tab_behind_is_loaded_when_the_tab_is_first_shown(cx: &mut TestAppContext) {
    let mut app = open(cx);
    let first = app.focused();
    app.cx.simulate_keystrokes("cmd-t");
    app.cx.run_until_parked();

    // In the first tab, which is behind now.
    let opened = app.window.update(&mut app.cx, |root, window, cx| {
        root.open_browser(
            Some(URL),
            Some(first),
            SplitDirection::Right,
            false,
            window,
            cx,
        )
    });
    let pane = opened.unwrap().unwrap();
    app.cx.run_until_parked();
    let page = app.pages.page(0);
    assert_eq!(page.borrow().loads, Vec::<String>::new());
    // The pane knows its address all the same, as the session file needs.
    app.cx.simulate_keystrokes("cmd-1");
    app.cx.run_until_parked();
    let browser = app.browser(pane);
    assert_eq!(
        browser
            .read_with(&app.cx, |browser, _| browser.url())
            .as_deref(),
        Some(URL)
    );
    assert_eq!(page.borrow().loads, [URL]);

    // Shown again, it is not loaded again.
    app.cx.simulate_keystrokes("cmd-2 cmd-1");
    app.cx.run_until_parked();
    assert_eq!(page.borrow().loads, [URL]);
}

#[gpui::test]
fn a_page_behind_keeps_the_address_it_was_given_until_it_is_shown(cx: &mut TestAppContext) {
    let mut app = open(cx);
    let first = app.focused();
    app.cx.simulate_keystrokes("cmd-t");
    app.cx.run_until_parked();
    let opened = app.window.update(&mut app.cx, |root, window, cx| {
        root.open_browser(
            Some(URL),
            Some(first),
            SplitDirection::Right,
            false,
            window,
            cx,
        )
    });
    let pane = opened.unwrap().unwrap();
    app.cx.run_until_parked();

    let browser = app.window.update(&mut app.cx, |root, _, cx| {
        let tabs = root.tabs().read(cx);
        let grid = tabs.grid(tabs.tabs()[0]).unwrap().read(cx);
        grid.browser(pane).unwrap().clone()
    });
    let browser = browser.unwrap();
    browser.read_with(&app.cx, |browser, cx| {
        assert_eq!(browser.url().as_deref(), Some(URL));
        assert_eq!(browser.address_text(cx), URL);
        assert_eq!(browser.title(), URL);
    });
}

/// What a terminal beside a browser draws asks nothing of the page: the
/// page is only put somewhere when its place changes.
#[gpui::test]
fn frames_that_leave_the_page_where_it_is_ask_nothing_of_it(cx: &mut TestAppContext) {
    let mut app = open(cx);
    app.browse(Some(URL), false).unwrap();
    let page = app.pages.page(0);
    let (places, keyboard) = (page.borrow().places.len(), page.borrow().keyboard.len());
    assert_eq!(places, 1);

    // Typing into the terminal draws frames.
    for _ in 0..20 {
        app.cx.simulate_input("x");
        app.cx.run_until_parked();
    }
    app.window
        .update(&mut app.cx, |_, window, _| window.refresh())
        .unwrap();
    app.cx.run_until_parked();

    assert_eq!(page.borrow().places.len(), places);
    assert_eq!(page.borrow().keyboard.len(), keyboard);
    assert_eq!(page.borrow().loads, [URL]);
}

/// The bar of a browser pane has a cross at its right end that closes
/// that pane, whichever pane has the keyboard.
#[gpui::test]
fn the_cross_in_the_bar_closes_the_browser_pane(cx: &mut TestAppContext) {
    let mut app = open(cx);
    let pane = app.browse(Some(URL), false).unwrap();
    assert!(app.panes().contains(&pane));
    let address = app.cx.debug_bounds("browser-address").unwrap();
    let close = app.cx.debug_bounds("browser-close").unwrap();
    assert!(
        close.left() >= address.right(),
        "the cross is right of the address"
    );

    app.cx
        .simulate_click(close.center(), gpui::Modifiers::none());
    app.cx.run_until_parked();

    assert!(!app.panes().contains(&pane));
}

/// The address is edited as text is anywhere: select all, cut, undo.
#[gpui::test]
fn the_keys_of_the_edit_menu_edit_the_address(cx: &mut TestAppContext) {
    let mut app = open(cx);
    let pane = app.browse(Some(URL), true).unwrap();
    app.cx.simulate_keystrokes("cmd-l");
    app.cx.run_until_parked();
    let browser = app.browser(pane);
    let text = |app: &mut App| browser.read_with(&app.cx, |browser, cx| browser.address_text(cx));

    app.cx.simulate_keystrokes("right cmd-a cmd-x");
    assert_eq!(text(&mut app), "");
    assert_eq!(
        app.cx
            .read_from_clipboard()
            .and_then(|item| item.text())
            .as_deref(),
        Some(URL)
    );
    app.cx.simulate_keystrokes("cmd-z");
    assert_eq!(text(&mut app), URL);
    app.cx.simulate_keystrokes("cmd-a cmd-c right");
    app.cx.simulate_input("/x");
    assert_eq!(text(&mut app), format!("{URL}/x"));
}

#[gpui::test]
fn load_phase_events_record_four_timestamps_in_order(cx: &mut TestAppContext) {
    use std::time::{Duration, Instant};

    use benterm_ui::browser::LoadPhase;

    let mut app = open(cx);
    let pane = app.browse(Some(URL), false).unwrap();
    let page = app.pages.page(0);
    let browser = app.browser(pane);

    let t0 = Instant::now();
    let t1 = t0 + Duration::from_millis(12);
    let t2 = t0 + Duration::from_millis(45);
    let t3 = t0 + Duration::from_millis(110);

    page.borrow().tell(PageEvent::LoadPhase {
        phase: LoadPhase::Requested,
        at: t0,
    });
    page.borrow().tell(PageEvent::LoadPhase {
        phase: LoadPhase::Started,
        at: t1,
    });
    page.borrow().tell(PageEvent::LoadPhase {
        phase: LoadPhase::Committed,
        at: t2,
    });
    app.cx.run_until_parked();

    browser.read_with(&app.cx, |browser, _| {
        let current = browser.current_load_timing().expect("load in progress");
        assert_eq!(current.requested, Some(t0));
        assert_eq!(current.started, Some(t1));
        assert_eq!(current.committed, Some(t2));
        assert_eq!(current.finished, None);
        assert_eq!(current.failed, None);
        assert!(browser.last_load_timing().is_none());
    });

    page.borrow().tell(PageEvent::LoadPhase {
        phase: LoadPhase::Finished,
        at: t3,
    });
    app.cx.run_until_parked();

    browser.read_with(&app.cx, |browser, _| {
        assert!(browser.current_load_timing().is_none());
        let last = browser.last_load_timing().expect("completed load");
        assert_eq!(last.url.as_deref(), Some(URL));
        assert_eq!(last.requested, Some(t0));
        assert_eq!(last.started, Some(t1));
        assert_eq!(last.committed, Some(t2));
        assert_eq!(last.finished, Some(t3));
        assert_eq!(last.failed, None);
    });
}

#[gpui::test]
fn load_phase_records_failure_timestamp(cx: &mut TestAppContext) {
    use std::time::{Duration, Instant};

    use benterm_ui::browser::LoadPhase;

    let mut app = open(cx);
    let pane = app.browse(Some(URL), false).unwrap();
    let page = app.pages.page(0);
    let browser = app.browser(pane);

    let t0 = Instant::now();
    let t1 = t0 + Duration::from_millis(8);
    let t2 = t0 + Duration::from_millis(25);

    page.borrow().tell(PageEvent::LoadPhase {
        phase: LoadPhase::Requested,
        at: t0,
    });
    page.borrow().tell(PageEvent::LoadPhase {
        phase: LoadPhase::Started,
        at: t1,
    });
    page.borrow_mut().state.failure = Some(PageFailure {
        url: "http://127.0.0.1:9/".into(),
        reason: "Could not connect to the server.".into(),
    });
    page.borrow().tell(PageEvent::LoadPhase {
        phase: LoadPhase::Failed,
        at: t2,
    });
    app.cx.run_until_parked();

    browser.read_with(&app.cx, |browser, _| {
        assert!(browser.current_load_timing().is_none());
        let last = browser.last_load_timing().expect("failed load");
        assert_eq!(last.url.as_deref(), Some("http://127.0.0.1:9/"));
        assert_eq!(last.requested, Some(t0));
        assert_eq!(last.started, Some(t1));
        assert_eq!(last.committed, None);
        assert_eq!(last.finished, None);
        assert_eq!(last.failed, Some(t2));
    });
}

#[gpui::test]
fn in_page_navigation_without_requested_is_recorded(cx: &mut TestAppContext) {
    use std::time::{Duration, Instant};

    use benterm_ui::browser::LoadPhase;

    let mut app = open(cx);
    let pane = app.browse(Some(URL), false).unwrap();
    let page = app.pages.page(0);
    let browser = app.browser(pane);

    let next_url = "http://127.0.0.1:8080/next";
    page.borrow_mut().state.url = Some(next_url.into());

    let t1 = Instant::now();
    let t2 = t1 + Duration::from_millis(15);
    let t3 = t1 + Duration::from_millis(60);

    page.borrow().tell(PageEvent::LoadPhase {
        phase: LoadPhase::Started,
        at: t1,
    });
    page.borrow().tell(PageEvent::LoadPhase {
        phase: LoadPhase::Committed,
        at: t2,
    });
    page.borrow().tell(PageEvent::LoadPhase {
        phase: LoadPhase::Finished,
        at: t3,
    });
    app.cx.run_until_parked();

    browser.read_with(&app.cx, |browser, _| {
        assert!(browser.current_load_timing().is_none());
        let last = browser.last_load_timing().expect("in-page navigation");
        assert_eq!(last.url.as_deref(), Some(next_url));
        assert_eq!(last.requested, None);
        assert_eq!(last.started, Some(t1));
        assert_eq!(last.committed, Some(t2));
        assert_eq!(last.finished, Some(t3));
        assert_eq!(last.failed, None);
    });
}

#[gpui::test]
fn initial_page_bounds_matches_the_placed_pane_page_area(cx: &mut TestAppContext) {
    use benterm_ui::browser::initial_page_bounds;

    let mut app = open(cx);
    let pane = app.browse(Some(URL), false).unwrap();
    let pane_bounds = app.bounds(pane);
    let theme = app
        .grid()
        .read_with(&app.cx, |grid, _| grid.theme().clone());
    let initial = initial_page_bounds(pane_bounds, &theme).expect("non-zero initial bounds");
    let placed = app
        .browser(pane)
        .read_with(&app.cx, |browser, _| browser.page_bounds())
        .expect("page is placed");

    assert!(initial.size.width > px(0.));
    assert!(initial.size.height > px(0.));
    assert_eq!(initial, placed);
}

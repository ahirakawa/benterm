//! Tabs in a window, each with panes of its own, from the keyboard and the
//! mouse.

use std::time::{Duration, Instant};

use benterm_layout::TabId;
use benterm_terminal::PtyConfig;
use benterm_ui::settings::ConfigFeed;
use benterm_ui::{BentermWindow, PaneGrid, TabGroup, bind_keys, open_terminal_window};
use gpui::{
    Entity, Modifiers, MouseButton, Pixels, Point, TestAppContext, VisualTestContext, WindowHandle,
    point, px,
};

fn sh(script: &str) -> PtyConfig {
    PtyConfig::command("/bin/sh", ["-c", script])
}

/// Opens a window whose panes all run `script`, with the app's key
/// bindings. The terminals' worker threads wake the app from outside the
/// test scheduler, which the scheduler has to be told to accept.
fn open(script: &str, cx: &mut TestAppContext) -> (WindowHandle<BentermWindow>, VisualTestContext) {
    cx.executor().allow_parking();
    cx.update(bind_keys);
    let window = cx
        .update(|cx| open_terminal_window(sh(script), ConfigFeed::defaults(), cx))
        .unwrap();
    let vcx = VisualTestContext::from_window(window.into(), cx);
    vcx.run_until_parked();
    (window, vcx)
}

fn tabs(window: WindowHandle<BentermWindow>, cx: &mut VisualTestContext) -> Entity<TabGroup> {
    window
        .update(cx, |root, _, _| root.tabs().clone())
        .expect("the window is open")
}

fn ids(window: WindowHandle<BentermWindow>, cx: &mut VisualTestContext) -> Vec<TabId> {
    tabs(window, cx).read_with(cx, |tabs, _| tabs.tabs())
}

fn selected(window: WindowHandle<BentermWindow>, cx: &mut VisualTestContext) -> TabId {
    tabs(window, cx).read_with(cx, |tabs, _| tabs.selected())
}

fn grid_of(
    window: WindowHandle<BentermWindow>,
    id: TabId,
    cx: &mut VisualTestContext,
) -> Entity<PaneGrid> {
    tabs(window, cx).read_with(cx, |tabs, _| {
        tabs.grid(id).expect("the tab is open").clone()
    })
}

/// Whether GPUI's focus, which decides where typed keys go, is on the
/// focused pane of tab `id`.
fn has_focus(window: WindowHandle<BentermWindow>, id: TabId, cx: &mut VisualTestContext) -> bool {
    let grid = grid_of(window, id, cx);
    let handle = grid.read_with(cx, |grid, cx| {
        grid.terminal(grid.focused())
            .unwrap()
            .read(cx)
            .focus_handle()
            .clone()
    });
    cx.update(|window, _| handle.is_focused(window))
}

/// Lets the app run until `done` holds. The programs run in real time on
/// other threads, hence the polling.
fn wait_until(
    cx: &mut VisualTestContext,
    what: &str,
    mut done: impl FnMut(&mut VisualTestContext) -> bool,
) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        cx.run_until_parked();
        if done(cx) {
            return;
        }
        assert!(Instant::now() < deadline, "timed out waiting until {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn tab_center(position: usize, cx: &mut VisualTestContext) -> Point<Pixels> {
    let selector = ["tab-0", "tab-1", "tab-2", "tab-3"][position];
    cx.run_until_parked();
    cx.debug_bounds(selector)
        .expect("the tab is in the tab bar")
        .center()
}

#[gpui::test]
fn a_window_starts_with_one_tab(cx: &mut TestAppContext) {
    let (window, mut cx) = open("sleep 100", cx);
    let ids = ids(window, &mut cx);
    assert_eq!(ids.len(), 1);
    assert_eq!(selected(window, &mut cx), ids[0]);
    assert!(has_focus(window, ids[0], &mut cx));
    assert!(cx.debug_bounds("tab-0").is_some());
}

#[gpui::test]
fn cmd_t_opens_a_tab_with_its_own_shell_and_focuses_it(cx: &mut TestAppContext) {
    let (window, mut cx) = open("echo pid=$$; sleep 100", cx);
    let first = selected(window, &mut cx);

    cx.simulate_keystrokes("cmd-t");

    let ids = ids(window, &mut cx);
    assert_eq!(ids.len(), 2);
    assert_eq!(ids[0], first);
    assert_eq!(selected(window, &mut cx), ids[1]);
    assert!(has_focus(window, ids[1], &mut cx));
    // Each tab has panes of its own.
    let (a, b) = (
        grid_of(window, ids[0], &mut cx),
        grid_of(window, ids[1], &mut cx),
    );
    assert_ne!(a.entity_id(), b.entity_id());
    let first_pane = a.read_with(&cx, |grid, _| grid.focused());
    let second_pane = b.read_with(&cx, |grid, _| grid.focused());
    assert_ne!(first_pane, second_pane);
    assert_eq!(
        window
            .update(&mut cx, |root, _, cx| root.grid(cx).entity_id())
            .unwrap(),
        b.entity_id()
    );
}

#[gpui::test]
fn cmd_t_opens_the_new_tab_right_of_the_selected_one(cx: &mut TestAppContext) {
    let (window, mut cx) = open("sleep 100", cx);
    cx.simulate_keystrokes("cmd-t cmd-t cmd-1 cmd-t");
    let ids = ids(window, &mut cx);
    assert_eq!(ids.len(), 4);
    assert_eq!(selected(window, &mut cx), ids[1]);
}

#[gpui::test]
fn cmd_w_closes_the_focused_pane_then_the_tab_then_the_window(cx: &mut TestAppContext) {
    let (window, mut cx) = open("sleep 100", cx);
    cx.simulate_keystrokes("cmd-t cmd-d");
    let ids = ids(window, &mut cx);
    let grid = grid_of(window, ids[1], &mut cx);
    assert_eq!(grid.read_with(&cx, |grid, _| grid.panes().len()), 2);

    // A tab with two panes loses the focused one.
    cx.simulate_keystrokes("cmd-w");
    assert_eq!(self::ids(window, &mut cx), ids);
    assert_eq!(grid.read_with(&cx, |grid, _| grid.panes().len()), 1);
    assert!(has_focus(window, ids[1], &mut cx));

    // With its last pane goes the tab, and the one left of it is selected.
    cx.simulate_keystrokes("cmd-w");
    assert_eq!(self::ids(window, &mut cx), vec![ids[0]]);
    assert_eq!(selected(window, &mut cx), ids[0]);
    assert!(has_focus(window, ids[0], &mut cx));

    // With the last tab goes the window.
    cx.simulate_keystrokes("cmd-w");
    cx.run_until_parked();
    assert!(window.update(&mut cx, |_, _, _| ()).is_err());
}

#[gpui::test]
fn cmd_digits_select_tabs_and_cmd_9_the_last(cx: &mut TestAppContext) {
    let (window, mut cx) = open("sleep 100", cx);
    cx.simulate_keystrokes("cmd-t cmd-t");
    let ids = ids(window, &mut cx);

    cx.simulate_keystrokes("cmd-1");
    assert_eq!(selected(window, &mut cx), ids[0]);
    assert!(has_focus(window, ids[0], &mut cx));
    cx.simulate_keystrokes("cmd-2");
    assert_eq!(selected(window, &mut cx), ids[1]);
    // A number past the last tab selects the last one, as in Ghostty.
    cx.simulate_keystrokes("cmd-1 cmd-8");
    assert_eq!(selected(window, &mut cx), ids[2]);
    cx.simulate_keystrokes("cmd-1 cmd-9");
    assert_eq!(selected(window, &mut cx), ids[2]);
    assert!(has_focus(window, ids[2], &mut cx));
}

#[gpui::test]
fn cmd_shift_brackets_move_to_the_previous_and_next_tab(cx: &mut TestAppContext) {
    let (window, mut cx) = open("sleep 100", cx);
    cx.simulate_keystrokes("cmd-t cmd-t");
    let ids = ids(window, &mut cx);

    cx.simulate_keystrokes("cmd-shift-[");
    assert_eq!(selected(window, &mut cx), ids[1]);
    assert!(has_focus(window, ids[1], &mut cx));
    cx.simulate_keystrokes("cmd-shift-]");
    assert_eq!(selected(window, &mut cx), ids[2]);
    // Both ends wrap around.
    cx.simulate_keystrokes("cmd-shift-]");
    assert_eq!(selected(window, &mut cx), ids[0]);
    cx.simulate_keystrokes("cmd-shift-[");
    assert_eq!(selected(window, &mut cx), ids[2]);
}

#[gpui::test]
fn clicking_a_tab_selects_it(cx: &mut TestAppContext) {
    let (window, mut cx) = open("sleep 100", cx);
    cx.simulate_keystrokes("cmd-t cmd-t");
    let ids = ids(window, &mut cx);

    let center = tab_center(0, &mut cx);
    cx.simulate_click(center, Modifiers::none());
    assert_eq!(selected(window, &mut cx), ids[0]);
    assert!(has_focus(window, ids[0], &mut cx));
}

/// The middle of the button for a new tab at the end of the tab bar.
fn new_tab_button(cx: &mut VisualTestContext) -> Point<Pixels> {
    cx.run_until_parked();
    cx.debug_bounds("tab-new")
        .expect("the tab bar has a button for a new tab")
        .center()
}

#[gpui::test]
fn the_new_tab_button_sits_right_of_the_last_tab(cx: &mut TestAppContext) {
    let (_, mut cx) = open("sleep 100", cx);
    cx.simulate_keystrokes("cmd-t");
    cx.run_until_parked();
    let last = cx.debug_bounds("tab-1").expect("the second tab is shown");
    let button = cx.debug_bounds("tab-new").expect("the button is shown");
    assert!(button.left() >= last.right());
    assert!(button.left() - last.right() < px(8.));
}

#[gpui::test]
fn clicking_the_new_tab_button_opens_a_tab_right_of_the_selected_one(cx: &mut TestAppContext) {
    let (window, mut cx) = open("sleep 100", cx);
    cx.simulate_keystrokes("cmd-t cmd-1");
    let before = ids(window, &mut cx);

    let button = new_tab_button(&mut cx);
    cx.simulate_click(button, Modifiers::none());
    cx.run_until_parked();

    let after = ids(window, &mut cx);
    assert_eq!(after.len(), 3);
    assert_eq!(after[0], before[0]);
    assert_eq!(after[2], before[1]);
    assert_eq!(selected(window, &mut cx), after[1]);
    assert!(has_focus(window, after[1], &mut cx));
}

#[gpui::test]
fn the_new_tab_button_stays_in_the_bar_when_the_tabs_overflow_it(cx: &mut TestAppContext) {
    let (window, mut cx) = open("sleep 100", cx);
    for _ in 0..15 {
        cx.simulate_keystrokes("cmd-t");
    }
    cx.run_until_parked();
    let bar = cx.debug_bounds("tab-bar").expect("the tab bar is shown");
    let first = cx.debug_bounds("tab-0").expect("the first tab is laid out");
    let last = cx.debug_bounds("tab-15").expect("the last tab is laid out");
    assert!(
        last.right() - first.left() > bar.size.width,
        "the tabs do not overflow the bar"
    );
    let button = cx.debug_bounds("tab-new").expect("the button is shown");
    assert!(
        button.right() <= bar.right(),
        "{button:?} is outside {bar:?}"
    );

    cx.simulate_click(button.center(), Modifiers::none());
    cx.run_until_parked();
    let ids = ids(window, &mut cx);
    assert_eq!(ids.len(), 17);
    assert_eq!(selected(window, &mut cx), ids[16]);
    assert!(has_focus(window, ids[16], &mut cx));
}

/// Whether the tab with debug selector `name` is in full view, between
/// the left end of the bar and the button for a new tab.
fn in_view(name: &'static str, cx: &mut VisualTestContext) -> bool {
    cx.run_until_parked();
    // A frame for the scroll to be laid out with.
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
    let bar = cx.debug_bounds("tab-bar").expect("the tab bar is shown");
    let button = cx.debug_bounds("tab-new").expect("the button is shown");
    let tab = cx.debug_bounds(name).expect("the tab is laid out");
    tab.left() >= bar.left() && tab.right() <= button.left()
}

#[gpui::test]
fn the_tab_bar_scrolls_to_the_selected_tab_when_the_tabs_overflow_it(cx: &mut TestAppContext) {
    let (_, mut cx) = open("sleep 100", cx);
    for _ in 0..15 {
        cx.simulate_keystrokes("cmd-t");
    }
    // The tab just opened, at the right end.
    assert!(in_view("tab-15", &mut cx));
    cx.simulate_keystrokes("cmd-1");
    assert!(in_view("tab-0", &mut cx));
}

#[gpui::test]
fn dragging_a_tab_onto_another_moves_it_there(cx: &mut TestAppContext) {
    let (window, mut cx) = open("sleep 100", cx);
    cx.simulate_keystrokes("cmd-t cmd-t");
    let ids = ids(window, &mut cx);

    let from = tab_center(0, &mut cx);
    let to = tab_center(2, &mut cx);
    let none = Modifiers::none();
    cx.simulate_mouse_down(from, MouseButton::Left, none);
    cx.simulate_mouse_move(from + point(px(10.), px(0.)), MouseButton::Left, none);
    cx.simulate_mouse_move(to, MouseButton::Left, none);
    cx.simulate_mouse_up(to, MouseButton::Left, none);
    cx.run_until_parked();

    assert_eq!(self::ids(window, &mut cx), vec![ids[1], ids[2], ids[0]]);
    // The dragged tab is selected by the press that picked it up.
    assert_eq!(selected(window, &mut cx), ids[0]);
}

#[gpui::test]
fn a_tab_closes_when_its_shell_exits(cx: &mut TestAppContext) {
    let (window, mut cx) = open("stty -echo; read line; exit 0", cx);
    cx.simulate_keystrokes("cmd-t cmd-t cmd-2");
    let ids = ids(window, &mut cx);

    cx.simulate_input("bye");
    cx.simulate_keystrokes("enter");
    wait_until(&mut cx, "the middle tab closes", |cx| {
        self::ids(window, cx) == vec![ids[0], ids[2]]
    });
    // The tab that took its place is selected and has the keys.
    assert_eq!(selected(window, &mut cx), ids[2]);
    assert!(has_focus(window, ids[2], &mut cx));
}

#[gpui::test]
fn a_shell_exiting_in_another_tab_leaves_the_focus_alone(cx: &mut TestAppContext) {
    // The shell exits a second after it reads a line.
    let (window, mut cx) = open("stty -echo; read line; sleep 1; exit 0", cx);
    // The first tab gets a second pane, whose shell is told to exit just
    // before the second tab is opened.
    cx.simulate_keystrokes("cmd-d");
    let first = selected(window, &mut cx);
    let grid = grid_of(window, first, &mut cx);
    cx.simulate_input("bye");
    cx.simulate_keystrokes("enter cmd-t");
    let second = selected(window, &mut cx);
    assert_ne!(first, second);
    wait_until(&mut cx, "the pane closes", |cx| {
        grid.read_with(cx, |grid, _| grid.panes().len()) == 1
    });
    assert_eq!(selected(window, &mut cx), second);
    assert!(has_focus(window, second, &mut cx));
}

#[gpui::test]
fn the_window_title_follows_the_selected_tab(cx: &mut TestAppContext) {
    let (window, mut cx) = open("printf '\\033]0;%s\\007' \"t$$\"; sleep 100", cx);
    let title = |cx: &mut VisualTestContext| {
        window
            .update(cx, |root, _, cx| root.tabs().read(cx).window_title(cx))
            .unwrap()
    };
    let first = selected(window, &mut cx);
    wait_until(&mut cx, "the first title is set", |cx| {
        title(cx).starts_with('t')
    });
    let first_title = title(&mut cx);
    cx.simulate_keystrokes("cmd-t");
    wait_until(&mut cx, "the second title is set", |cx| {
        let shown = title(cx);
        shown.starts_with('t') && shown != first_title
    });
    let tabs = tabs(window, &mut cx);
    let label = tabs.read_with(&cx, |tabs, cx| tabs.label(first, cx));
    assert_eq!(label, first_title);
}

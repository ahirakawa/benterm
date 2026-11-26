//! Names the user gives tabs: typed from the right-click menu, the
//! command palette or the menu bar, shown in place of the title the
//! program sets, and taken away with an empty name.

use std::time::{Duration, Instant};

use benterm_layout::TabId;
use benterm_terminal::PtyConfig;
use benterm_ui::settings::ConfigFeed;
use benterm_ui::{BentermWindow, RenameTab, TabGroup, bind_keys, menu, open_terminal_window};
use gpui::{Entity, Modifiers, MouseButton, TestAppContext, VisualTestContext, WindowHandle};

/// What every pane runs: sets the title to `first`, then to each line it
/// is given, with OSC 2. Nothing is echoed.
const TITLES: &str = r#"stty -echo; printf '\033]2;first\007'; while read -r t; do printf '\033]2;%s\007' "$t"; done"#;

/// Opens a window whose panes run [`TITLES`], with the app's keys and
/// menu bar, and waits until the first pane has set its title.
fn open(cx: &mut TestAppContext) -> (WindowHandle<BentermWindow>, VisualTestContext) {
    cx.executor().allow_parking();
    cx.update(|cx| {
        bind_keys(cx);
        menu::install(cx);
    });
    let pty = PtyConfig::command("/bin/sh", ["-c", TITLES]);
    let window = cx
        .update(|cx| open_terminal_window(pty, ConfigFeed::defaults(), cx))
        .unwrap();
    let mut cx = VisualTestContext::from_window(window.into(), cx);
    cx.update(|window, _| window.activate_window());
    let first = selected(window, &mut cx);
    wait_for_title(window, first, "first", &mut cx);
    (window, cx)
}

fn tabs(window: WindowHandle<BentermWindow>, cx: &mut VisualTestContext) -> Entity<TabGroup> {
    window
        .update(cx, |root, _, _| root.tabs().clone())
        .expect("the window is open")
}

fn selected(window: WindowHandle<BentermWindow>, cx: &mut VisualTestContext) -> TabId {
    tabs(window, cx).read_with(cx, |tabs, _| tabs.selected())
}

fn label(window: WindowHandle<BentermWindow>, id: TabId, cx: &mut VisualTestContext) -> String {
    tabs(window, cx).read_with(cx, |tabs, cx| tabs.label(id, cx))
}

fn renaming(window: WindowHandle<BentermWindow>, cx: &mut VisualTestContext) -> Option<TabId> {
    tabs(window, cx).read_with(cx, |tabs, _| tabs.renaming())
}

/// The title the program of a tab's focused pane set last.
fn program_title(
    window: WindowHandle<BentermWindow>,
    id: TabId,
    cx: &mut VisualTestContext,
) -> Option<String> {
    tabs(window, cx).read_with(cx, |tabs, cx| tabs.grid(id).unwrap().read(cx).title(cx))
}

fn wait_for_title(
    window: WindowHandle<BentermWindow>,
    id: TabId,
    title: &str,
    cx: &mut VisualTestContext,
) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        cx.run_until_parked();
        if program_title(window, id, cx).as_deref() == Some(title) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for the title {title}"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Whether the keyboard is in the focused pane of the tab `id`.
fn terminal_has_focus(
    window: WindowHandle<BentermWindow>,
    id: TabId,
    cx: &mut VisualTestContext,
) -> bool {
    let handle = tabs(window, cx).read_with(cx, |tabs, cx| {
        let grid = tabs.grid(id).unwrap().read(cx);
        grid.terminal(grid.focused())
            .unwrap()
            .read(cx)
            .focus_handle()
            .clone()
    });
    cx.update(|window, _| handle.is_focused(window))
}

fn draw(window: WindowHandle<BentermWindow>, cx: &mut VisualTestContext) {
    cx.run_until_parked();
    let _ = window.update(cx, |_, window, _| window.refresh());
    cx.run_until_parked();
}

/// Right-clicks the tab at `position` and chooses `item` in its menu.
fn choose_in_tab_menu(
    window: WindowHandle<BentermWindow>,
    position: usize,
    item: &str,
    cx: &mut VisualTestContext,
) {
    draw(window, cx);
    let selector = ["tab-0", "tab-1", "tab-2"][position];
    let at = cx
        .debug_bounds(selector)
        .expect("the tab is drawn")
        .center();
    cx.simulate_mouse_down(at, MouseButton::Right, Modifiers::none());
    cx.simulate_mouse_up(at, MouseButton::Right, Modifiers::none());
    draw(window, cx);
    let menu = window
        .read_with(cx, |root, _| root.context_menu())
        .unwrap()
        .expect("the menu is open");
    let names: Vec<String> = menu.read_with(cx, |menu, _| {
        menu.items().iter().map(|item| item.name.clone()).collect()
    });
    let index = names
        .iter()
        .position(|name| name == item)
        .unwrap_or_else(|| panic!("the menu has no {item}: {names:?}"));
    let selector: &'static str = format!("context-menu-item-{index}").leak();
    let at = cx
        .debug_bounds(selector)
        .expect("the item is drawn")
        .center();
    cx.simulate_click(at, Modifiers::none());
    draw(window, cx);
}

/// Names the tab at `position` from its right-click menu.
fn name_tab(
    window: WindowHandle<BentermWindow>,
    position: usize,
    name: &str,
    cx: &mut VisualTestContext,
) {
    choose_in_tab_menu(window, position, "Rename", cx);
    cx.simulate_input(name);
    cx.simulate_keystrokes("enter");
    draw(window, cx);
}

#[gpui::test]
fn rename_in_the_tab_menu_names_the_tab(cx: &mut TestAppContext) {
    let (window, mut cx) = open(cx);
    let id = selected(window, &mut cx);

    choose_in_tab_menu(window, 0, "Rename", &mut cx);
    assert_eq!(renaming(window, &mut cx), Some(id));
    assert!(cx.debug_bounds("tab-name-editor").is_some());
    // What is typed takes the place of the label.
    cx.simulate_input("review");
    cx.simulate_keystrokes("enter");
    draw(window, &mut cx);

    assert_eq!(label(window, id, &mut cx), "review");
    assert_eq!(renaming(window, &mut cx), None);
    assert!(cx.debug_bounds("tab-name-editor").is_none());
    // The keys are back with the terminal.
    assert!(terminal_has_focus(window, id, &mut cx));
}

#[gpui::test]
fn rename_names_the_tab_that_was_clicked(cx: &mut TestAppContext) {
    let (window, mut cx) = open(cx);
    cx.simulate_keystrokes("cmd-t");
    let ids = tabs(window, &mut cx).read_with(&cx, |tabs, _| tabs.tabs());
    wait_for_title(window, ids[1], "first", &mut cx);

    name_tab(window, 0, "review", &mut cx);

    assert_eq!(label(window, ids[0], &mut cx), "review");
    assert_eq!(label(window, ids[1], &mut cx), "first");
}

#[gpui::test]
fn a_named_tab_keeps_its_name_when_the_program_sets_a_title(cx: &mut TestAppContext) {
    let (window, mut cx) = open(cx);
    let id = selected(window, &mut cx);
    name_tab(window, 0, "review", &mut cx);

    cx.simulate_input("second");
    cx.simulate_keystrokes("enter");
    wait_for_title(window, id, "second", &mut cx);

    assert_eq!(label(window, id, &mut cx), "review");
    let title = window
        .update(&mut cx, |root, _, cx| root.tabs().read(cx).window_title(cx))
        .unwrap();
    assert_eq!(title, "review");
}

#[gpui::test]
fn an_empty_name_shows_the_title_of_the_program_again(cx: &mut TestAppContext) {
    let (window, mut cx) = open(cx);
    let id = selected(window, &mut cx);
    name_tab(window, 0, "review", &mut cx);
    assert_eq!(label(window, id, &mut cx), "review");

    // The field starts with the name, all of it selected.
    choose_in_tab_menu(window, 0, "Rename", &mut cx);
    cx.simulate_keystrokes("backspace enter");
    draw(window, &mut cx);

    assert_eq!(label(window, id, &mut cx), "first");
    assert_eq!(renaming(window, &mut cx), None);
}

#[gpui::test]
fn escape_keeps_the_old_label(cx: &mut TestAppContext) {
    let (window, mut cx) = open(cx);
    let id = selected(window, &mut cx);

    choose_in_tab_menu(window, 0, "Rename", &mut cx);
    cx.simulate_input("review");
    cx.simulate_keystrokes("escape");
    draw(window, &mut cx);

    assert_eq!(label(window, id, &mut cx), "first");
    assert_eq!(renaming(window, &mut cx), None);
    assert!(terminal_has_focus(window, id, &mut cx));
}

#[gpui::test]
fn the_command_palette_renames_the_selected_tab(cx: &mut TestAppContext) {
    let (window, mut cx) = open(cx);
    let id = selected(window, &mut cx);

    cx.simulate_keystrokes("cmd-k");
    cx.simulate_input("rename tab");
    cx.simulate_keystrokes("enter");
    draw(window, &mut cx);
    assert_eq!(renaming(window, &mut cx), Some(id));
    cx.simulate_input("review");
    cx.simulate_keystrokes("enter");
    draw(window, &mut cx);

    assert_eq!(label(window, id, &mut cx), "review");
}

#[gpui::test]
fn the_menu_bar_renames_the_selected_tab(cx: &mut TestAppContext) {
    let (window, mut cx) = open(cx);
    cx.simulate_keystrokes("cmd-t");
    let id = selected(window, &mut cx);
    wait_for_title(window, id, "first", &mut cx);

    // The menu bar of macOS hands a chosen item's action to the app,
    // which dispatches it in the active window.
    cx.cx.update(|cx| cx.dispatch_action(&RenameTab));
    draw(window, &mut cx);
    assert_eq!(renaming(window, &mut cx), Some(id));
    cx.simulate_input("review");
    cx.simulate_keystrokes("enter");
    draw(window, &mut cx);

    assert_eq!(label(window, id, &mut cx), "review");
}

#[gpui::test]
fn cmd_alt_r_renames_the_selected_tab(cx: &mut TestAppContext) {
    let (window, mut cx) = open(cx);
    let id = selected(window, &mut cx);

    cx.simulate_keystrokes("cmd-alt-r");
    draw(window, &mut cx);
    assert_eq!(renaming(window, &mut cx), Some(id));
    cx.simulate_input("review");
    cx.simulate_keystrokes("enter");
    draw(window, &mut cx);

    assert_eq!(label(window, id, &mut cx), "review");
}

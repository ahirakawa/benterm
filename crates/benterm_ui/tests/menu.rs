//! The menu bar: what it lists, the keys its items show, and that choosing
//! an item does what its keys do.

use std::time::{Duration, Instant};

use benterm_terminal::PtyConfig;
use benterm_ui::command_match::command_name;
use benterm_ui::menu::{self, About, Cut, Quit, Redo, SelectAll, Undo};
use benterm_ui::settings::ConfigFeed;
use benterm_ui::{
    APP_NAME, BentermWindow, Close, CloseOtherTabs, CloseSession, Copy, NewTab, NewWorkspace,
    Paste, RenameTab, RenameWorkspace, SelectNextTab, SelectNextWorkspace, SelectPreviousTab,
    SelectPreviousWorkspace, SplitDown, SplitRight, ToggleFileBrowser, bind_keys,
    open_terminal_window,
};
use gpui::{
    Action, OsAction, OwnedMenu, OwnedMenuItem, TestAppContext, VisualTestContext, WindowHandle,
};

fn sh(script: &str) -> PtyConfig {
    PtyConfig::command("/bin/sh", ["-c", script])
}

/// Binds the app's keys and sets the menu bar, as the app does at start.
fn start(cx: &mut TestAppContext) {
    cx.update(|cx| {
        bind_keys(cx);
        menu::install(cx);
    });
}

/// Opens a window whose panes all run a shell that waits. The terminals'
/// worker threads wake the app from outside the test scheduler, which the
/// scheduler has to be told to accept.
fn open(cx: &mut TestAppContext) -> (WindowHandle<BentermWindow>, VisualTestContext) {
    cx.executor().allow_parking();
    let window = cx
        .update(|cx| open_terminal_window(sh("sleep 100"), ConfigFeed::defaults(), cx))
        .unwrap();
    let vcx = VisualTestContext::from_window(window.into(), cx);
    vcx.run_until_parked();
    (window, vcx)
}

/// The items of `menus` that run an action, as the menu's name, the
/// item's name and the action.
fn items(menus: &[OwnedMenu]) -> Vec<(String, String, Box<dyn Action>)> {
    fn walk(
        menu: &str,
        items: &[OwnedMenuItem],
        found: &mut Vec<(String, String, Box<dyn Action>)>,
    ) {
        for item in items {
            match item {
                OwnedMenuItem::Action { name, action, .. } => {
                    found.push((menu.to_owned(), name.to_string(), action.boxed_clone()))
                }
                OwnedMenuItem::Submenu(submenu) => walk(menu, &submenu.items, found),
                _ => {}
            }
        }
    }
    let mut found = Vec::new();
    for menu in menus {
        walk(&menu.name, &menu.items, &mut found);
    }
    found
}

fn menu_items(cx: &mut TestAppContext) -> Vec<(String, String, Box<dyn Action>)> {
    let menus = cx.update(|cx| cx.get_menus()).expect("the menus are set");
    items(&menus)
}

/// The menu and the item whose action is `action`.
fn find(cx: &mut TestAppContext, action: &dyn Action) -> Option<(String, String)> {
    menu_items(cx)
        .into_iter()
        .find(|(_, _, item)| item.partial_eq(action))
        .map(|(menu, name, _)| (menu, name))
}

#[gpui::test]
fn the_menu_bar_lists_the_app_edit_view_tab_pane_and_workspace_menus(cx: &mut TestAppContext) {
    start(cx);
    let menus = cx.update(|cx| cx.get_menus()).expect("the menus are set");
    let names: Vec<String> = menus.iter().map(|menu| menu.name.to_string()).collect();
    assert_eq!(
        names,
        [APP_NAME, "Edit", "View", "Tab", "Pane", "Workspace"]
    );
}

#[gpui::test]
fn each_operation_is_in_its_menu(cx: &mut TestAppContext) {
    start(cx);
    let expected: [(&str, &dyn Action); 25] = [
        (APP_NAME, &About),
        (APP_NAME, &Quit),
        ("Edit", &Undo),
        ("Edit", &Redo),
        ("Edit", &Cut),
        ("Edit", &Copy),
        ("Edit", &Paste),
        ("Edit", &SelectAll),
        ("View", &ToggleFileBrowser),
        ("Tab", &NewTab),
        ("Tab", &Close),
        ("Tab", &CloseOtherTabs),
        ("Tab", &RenameTab),
        ("Tab", &SelectNextTab),
        ("Tab", &SelectPreviousTab),
        ("Pane", &SplitRight),
        ("Pane", &SplitDown),
        ("Pane", &benterm_ui::browser::OpenBrowser),
        ("Pane", &CloseSession),
        ("Workspace", &NewWorkspace),
        ("Workspace", &benterm_ui::NewTask),
        ("Workspace", &RenameWorkspace),
        ("Workspace", &SelectNextWorkspace),
        ("Workspace", &SelectPreviousWorkspace),
        ("Workspace", &benterm_ui::CloseWorkspace),
    ];
    for (menu, action) in expected {
        let found = find(cx, action);
        assert_eq!(
            found.as_ref().map(|(menu, _)| menu.as_str()),
            Some(menu),
            "{} is not in the {menu} menu",
            action.name()
        );
    }
}

/// The command of the system an item is tied to, by name, as `OsAction`
/// cannot be printed.
fn os_name(action: Option<OsAction>) -> Option<&'static str> {
    action.map(|action| match action {
        OsAction::Undo => "undo",
        OsAction::Redo => "redo",
        OsAction::Cut => "cut",
        OsAction::Copy => "copy",
        OsAction::Paste => "paste",
        OsAction::SelectAll => "select all",
    })
}

/// The Edit menu is that of every Mac app. Each item is tied to the
/// command of the system it stands for, so that the view with the
/// keyboard of the system, such as the page of a browser pane, can do it
/// in its own way.
#[gpui::test]
fn the_edit_menu_lists_undo_redo_cut_copy_paste_and_select_all(cx: &mut TestAppContext) {
    start(cx);
    let menus = cx.update(|cx| cx.get_menus()).expect("the menus are set");
    let edit = menus.iter().find(|menu| menu.name.as_ref() == "Edit");
    let items: Vec<(String, String, Option<&str>)> = edit
        .expect("there is an Edit menu")
        .items
        .iter()
        .filter_map(|item| match item {
            OwnedMenuItem::Action {
                name,
                action,
                os_action,
                ..
            } => Some((
                name.to_string(),
                action.name().to_owned(),
                os_name(*os_action),
            )),
            _ => None,
        })
        .collect();
    let expected: [(&str, &dyn Action, OsAction); 6] = [
        ("Undo", &Undo, OsAction::Undo),
        ("Redo", &Redo, OsAction::Redo),
        ("Cut", &Cut, OsAction::Cut),
        ("Copy", &Copy, OsAction::Copy),
        ("Paste", &Paste, OsAction::Paste),
        ("Select All", &SelectAll, OsAction::SelectAll),
    ];
    let expected: Vec<(String, String, Option<&str>)> = expected
        .into_iter()
        .map(|(name, action, os)| (name.to_owned(), action.name().to_owned(), os_name(Some(os))))
        .collect();
    assert_eq!(items, expected);
}

/// The items show the keys of every Mac app. A key may be bound to the
/// item's action in more than one view, as the terminal and the browser
/// pane each take the keys for themselves; it is the same key in each.
#[gpui::test]
fn the_edit_items_show_the_keys_of_a_mac(cx: &mut TestAppContext) {
    start(cx);
    let expected: [(&dyn Action, &str); 6] = [
        (&Undo, "cmd-z"),
        (&Redo, "cmd-shift-z"),
        (&Cut, "cmd-x"),
        (&Copy, "cmd-c"),
        (&Paste, "cmd-v"),
        (&SelectAll, "cmd-a"),
    ];
    for (action, keys) in expected {
        let mut bound = keys_of(cx, action);
        bound.dedup();
        assert_eq!(bound, [keys], "{}", action.name());
    }
}

/// The menu shows an item's keys from the bindings of its action, so an
/// action without a binding would show none. About is the one item that
/// has no keys, as on other Mac apps.
#[gpui::test]
fn every_item_but_about_runs_an_action_that_keys_are_bound_to(cx: &mut TestAppContext) {
    start(cx);
    let keymap = cx.update(|cx| cx.key_bindings());
    for (menu, name, action) in menu_items(cx) {
        if action.partial_eq(&About) {
            continue;
        }
        let bound = keymap
            .borrow()
            .bindings_for_action(action.as_ref())
            .next()
            .is_some();
        assert!(bound, "{menu} > {name} has no keys");
    }
}

#[gpui::test]
fn quit_is_bound_to_cmd_q(cx: &mut TestAppContext) {
    start(cx);
    let keymap = cx.update(|cx| cx.key_bindings());
    let keys: Vec<String> = keymap
        .borrow()
        .bindings_for_action(&Quit)
        .map(|binding| {
            binding
                .keystrokes()
                .iter()
                .map(|keystroke| keystroke.unparse(gpui::OperatingSystem::Mac))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect();
    assert_eq!(keys, ["cmd-q"]);
}

/// The command palette lists the actions the focused pane can run. The
/// menu runs the same actions, so every item is in the palette under the
/// name of its action.
#[gpui::test]
fn every_item_is_a_command_of_the_palette(cx: &mut TestAppContext) {
    start(cx);
    let (window, mut vcx) = open(cx);
    window
        .update(&mut vcx, |root, window, cx| {
            root.toggle_command_palette(window, cx)
        })
        .unwrap();
    vcx.run_until_parked();
    let commands: Vec<String> = window
        .update(&mut vcx, |root, _, cx| {
            let palette = root.command_palette().expect("the palette is open");
            palette
                .read(cx)
                .shown(cx)
                .into_iter()
                .map(|command| command.name)
                .collect()
        })
        .unwrap();
    for (menu, name, action) in menu_items(cx) {
        let command = command_name(action.name());
        assert!(
            commands.contains(&command),
            "{menu} > {name} runs {command}, which the palette does not list: {commands:?}"
        );
    }
}

/// What the operations change in a window.
#[derive(Debug, PartialEq, Eq)]
struct State {
    workspaces: usize,
    selected_workspace: usize,
    tabs: usize,
    selected_tab: usize,
    panes: usize,
    renaming: bool,
}

fn state(window: WindowHandle<BentermWindow>, cx: &mut VisualTestContext) -> State {
    window
        .update(cx, |root, _, cx| {
            let workspaces = root.workspaces();
            let tabs = root.tabs().read(cx);
            let tab_ids = tabs.tabs();
            State {
                workspaces: workspaces.len(),
                selected_workspace: workspaces
                    .iter()
                    .position(|id| *id == root.selected_workspace())
                    .unwrap(),
                tabs: tab_ids.len(),
                selected_tab: tab_ids
                    .iter()
                    .position(|id| *id == tabs.selected())
                    .unwrap(),
                panes: root.grid(cx).read(cx).panes().len(),
                renaming: root.renaming().is_some(),
            }
        })
        .expect("the window is open")
}

/// Lets the app run until the window's state stops changing for a while.
/// The terminals of new panes start on other threads.
fn settle(window: WindowHandle<BentermWindow>, cx: &mut VisualTestContext) -> State {
    let deadline = Instant::now() + Duration::from_millis(300);
    let mut last = state(window, cx);
    while Instant::now() < deadline {
        cx.run_until_parked();
        std::thread::sleep(Duration::from_millis(5));
        let now = state(window, cx);
        if now != last {
            last = now;
        }
    }
    last
}

/// Presses `keys` in one window and chooses the items of `actions` from
/// the menu bar in another, and checks that both windows end up alike.
fn same_as_keys(cx: &mut TestAppContext, keys: &str, actions: &[&dyn Action]) {
    start(cx);
    let (by_keys, mut keys_cx) = open(cx);
    for key in keys.split(' ') {
        keys_cx.simulate_keystrokes(key);
        keys_cx.run_until_parked();
    }
    let expected = settle(by_keys, &mut keys_cx);

    let (by_menu, mut menu_cx) = open(cx);
    menu_cx.update(|window, _| window.activate_window());
    menu_cx.run_until_parked();
    let active = menu_cx.update(|_, cx| cx.active_window());
    assert_eq!(active, Some(by_menu.into()), "the menu acts on this window");
    for action in actions {
        // The menu bar of macOS hands a chosen item's action to the app,
        // which dispatches it in the active window.
        assert!(
            find(cx, *action).is_some(),
            "{} is not in the menu bar",
            action.name()
        );
        let action = action.boxed_clone();
        cx.update(|cx| cx.dispatch_action(action.as_ref()));
        menu_cx.run_until_parked();
    }
    assert_eq!(settle(by_menu, &mut menu_cx), expected);
}

#[gpui::test]
fn new_tab_from_the_menu_is_cmd_t(cx: &mut TestAppContext) {
    same_as_keys(cx, "cmd-t", &[&NewTab]);
}

#[gpui::test]
fn previous_and_next_tab_from_the_menu_are_their_keys(cx: &mut TestAppContext) {
    same_as_keys(
        cx,
        "cmd-t cmd-t cmd-shift-[ cmd-shift-[ cmd-shift-]",
        &[
            &NewTab,
            &NewTab,
            &SelectPreviousTab,
            &SelectPreviousTab,
            &SelectNextTab,
        ],
    );
}

#[gpui::test]
fn splits_from_the_menu_are_cmd_d_and_cmd_shift_d(cx: &mut TestAppContext) {
    same_as_keys(cx, "cmd-d cmd-shift-d", &[&SplitRight, &SplitDown]);
}

#[gpui::test]
fn close_from_the_menu_is_cmd_w(cx: &mut TestAppContext) {
    same_as_keys(cx, "cmd-d cmd-t cmd-w", &[&SplitRight, &NewTab, &Close]);
}

#[gpui::test]
fn workspaces_from_the_menu_are_their_keys(cx: &mut TestAppContext) {
    same_as_keys(
        cx,
        "cmd-n cmd-n ctrl-cmd-[ ctrl-cmd-[ ctrl-cmd-] cmd-shift-r",
        &[
            &NewWorkspace,
            &NewWorkspace,
            &SelectPreviousWorkspace,
            &SelectPreviousWorkspace,
            &SelectNextWorkspace,
            &RenameWorkspace,
        ],
    );
}

fn keys_of(cx: &mut TestAppContext, action: &dyn Action) -> Vec<String> {
    let keymap = cx.update(|cx| cx.key_bindings());
    keymap
        .borrow()
        .bindings_for_action(action)
        .map(|binding| {
            binding
                .keystrokes()
                .iter()
                .map(|keystroke| keystroke.unparse(gpui::OperatingSystem::Mac))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect()
}

/// The item shows Cmd-Shift-E, and no other item or command of the
/// window has those keys.
#[gpui::test]
fn the_file_browser_item_shows_cmd_shift_e_which_nothing_else_has(cx: &mut TestAppContext) {
    start(cx);
    assert_eq!(
        find(cx, &ToggleFileBrowser),
        Some(("View".to_owned(), "File Browser".to_owned()))
    );
    assert_eq!(keys_of(cx, &ToggleFileBrowser), ["cmd-shift-e"]);
    for (menu, name, action) in menu_items(cx) {
        if action.partial_eq(&ToggleFileBrowser) {
            continue;
        }
        let keys = keys_of(cx, action.as_ref());
        assert!(
            !keys.iter().any(|keys| keys == "cmd-shift-e"),
            "{menu} > {name} also has cmd-shift-e"
        );
    }
    let keymap = cx.update(|cx| cx.key_bindings());
    let with_the_keys = keymap
        .borrow()
        .bindings()
        .filter(|binding| {
            binding
                .keystrokes()
                .iter()
                .map(|keystroke| keystroke.unparse(gpui::OperatingSystem::Mac))
                .collect::<Vec<_>>()
                .join(" ")
                == "cmd-shift-e"
        })
        .count();
    assert_eq!(with_the_keys, 1);
}

/// Choosing the item opens the file browser, as Cmd-Shift-E does, and
/// choosing it again closes it.
#[gpui::test]
fn file_browser_from_the_menu_is_cmd_shift_e(cx: &mut TestAppContext) {
    start(cx);
    let (window, mut vcx) = open(cx);
    vcx.update(|window, _| window.activate_window());
    vcx.run_until_parked();
    let shown = |vcx: &mut VisualTestContext| {
        window
            .read_with(vcx, |root, _| root.file_browser().is_some())
            .unwrap()
    };

    cx.update(|cx| cx.dispatch_action(&ToggleFileBrowser));
    vcx.run_until_parked();
    assert!(shown(&mut vcx));

    cx.update(|cx| cx.dispatch_action(&ToggleFileBrowser));
    vcx.run_until_parked();
    assert!(!shown(&mut vcx));
}

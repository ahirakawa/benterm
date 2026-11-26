//! The menu bar of macOS: the operations on tabs, panes and workspaces,
//! and the panels beside them, each with the keys bound to it now.
//!
//! An item runs the same action as its keys and the command palette. The
//! platform hands a chosen item's action to the app, which dispatches it
//! in the active window from the focused element, as a key would be. The
//! keys an item shows are read from the bindings of its action when the
//! menu bar is set.

use benterm_config::Role;
use gpui::{App, KeyBinding, Menu, MenuItem, OsAction, PromptButton, PromptLevel, actions};

use crate::browser::OpenBrowser;
use crate::roles::role_label;
use crate::{
    APP_NAME, Close, CloseOtherTabs, CloseSession, CloseWorkspace, Copy, LaunchRole, NewTab,
    NewTask, NewWorkspace, Paste, RenameTab, RenameWorkspace, SelectNextTab, SelectNextWorkspace,
    SelectPreviousTab, SelectPreviousWorkspace, SplitDown, SplitRight, ToggleFileBrowser,
};

actions!(
    app,
    [
        About,
        /// Quits the app.
        Quit,
    ]
);

actions!(
    edit,
    [
        Undo,
        Redo,
        Cut,
        /// Selects everything that can be typed in.
        SelectAll,
    ]
);

/// Binds the keys of the app's own commands, which work in any window.
pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([KeyBinding::new("cmd-q", Quit, None)]);
}

/// The menus of the menu bar, from left to right.
pub fn menus() -> Vec<Menu> {
    menus_with_roles(&[])
}

/// [`menus`] with the roles the Tab menu starts, if there are any.
pub fn menus_with_roles(roles: &[Role]) -> Vec<Menu> {
    let mut tab = vec![MenuItem::action("New Tab", NewTab)];
    if !roles.is_empty() {
        let items = roles
            .iter()
            .map(|role| MenuItem::action(role_label(role), LaunchRole(role.name.clone())));
        tab.push(MenuItem::submenu(
            Menu::new("New Tab with Role").items(items),
        ));
    }
    tab.extend([
        MenuItem::action("Close", Close),
        MenuItem::action("Close Other Tabs", CloseOtherTabs),
        MenuItem::action("Rename Tab", RenameTab),
        MenuItem::separator(),
        MenuItem::action("Next Tab", SelectNextTab),
        MenuItem::action("Previous Tab", SelectPreviousTab),
    ]);
    vec![
        Menu::new(APP_NAME).items([
            MenuItem::action(format!("About {APP_NAME}"), About),
            MenuItem::separator(),
            MenuItem::action(format!("Quit {APP_NAME}"), Quit),
        ]),
        // Each item is tied to the command of the system it stands for.
        // AppKit sends Cut, Copy, Paste and Select All to the view with
        // its keyboard first, so a page does them itself. Undo and Redo
        // come to the app as actions whatever has the keyboard.
        Menu::new("Edit").items([
            MenuItem::os_action("Undo", Undo, OsAction::Undo),
            MenuItem::os_action("Redo", Redo, OsAction::Redo),
            MenuItem::separator(),
            MenuItem::os_action("Cut", Cut, OsAction::Cut),
            MenuItem::os_action("Copy", Copy, OsAction::Copy),
            MenuItem::os_action("Paste", Paste, OsAction::Paste),
            MenuItem::os_action("Select All", SelectAll, OsAction::SelectAll),
        ]),
        Menu::new("View").items([MenuItem::action("File Browser", ToggleFileBrowser)]),
        Menu::new("Tab").items(tab),
        Menu::new("Pane").items([
            MenuItem::action("Split Right", SplitRight),
            MenuItem::action("Split Down", SplitDown),
            MenuItem::separator(),
            MenuItem::action("Open Browser", OpenBrowser),
            MenuItem::separator(),
            MenuItem::action("Close Session", CloseSession),
        ]),
        Menu::new("Workspace").items([
            MenuItem::action("New Workspace", NewWorkspace),
            MenuItem::action("New Task…", NewTask),
            MenuItem::action("Rename Workspace", RenameWorkspace),
            MenuItem::action("Close Workspace", CloseWorkspace),
            MenuItem::separator(),
            MenuItem::action("Next Workspace", SelectNextWorkspace),
            MenuItem::action("Previous Workspace", SelectPreviousWorkspace),
        ]),
    ]
}

/// Sets the menu bar and handles the app's own commands. Call it after the
/// keys are bound, as each item shows the keys bound to its action then.
pub fn install(cx: &mut App) {
    cx.on_action(|_: &Quit, cx| cx.quit());
    cx.on_action(|_: &About, cx| show_about(cx));
    cx.set_menus(menus());
}

/// Tells the name and the version of the app in a dialog over the active
/// window. Without a window there is nothing to put it over.
fn show_about(cx: &mut App) {
    let Some(window) = cx.active_window() else {
        return;
    };
    let version = format!("Version {}", env!("CARGO_PKG_VERSION"));
    let _ = window.update(cx, |_, window, cx| {
        // The answer only closes the dialog.
        let _answer = window.prompt(
            PromptLevel::Info,
            APP_NAME,
            Some(&version),
            &[PromptButton::ok("OK")],
            cx,
        );
    });
}

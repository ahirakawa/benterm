//! The command palette: Cmd-K opens it, typing narrows the commands, and
//! Enter or a click runs one.

use benterm_terminal::PtyConfig;
use benterm_ui::settings::ConfigFeed;
use benterm_ui::{BentermWindow, NewTab, bind_keys, open_terminal_window};
use gpui::{
    EntityInputHandler, KeyBinding, Modifiers, TestAppContext, VisualTestContext, WindowHandle,
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

fn is_open(window: WindowHandle<BentermWindow>, cx: &mut VisualTestContext) -> bool {
    window
        .update(cx, |root, _, _| root.command_palette().is_some())
        .unwrap()
}

/// The commands the palette shows, as their names and keys.
fn shown(window: WindowHandle<BentermWindow>, cx: &mut VisualTestContext) -> Vec<(String, String)> {
    cx.run_until_parked();
    window
        .update(cx, |root, _, cx| {
            let palette = root.command_palette().expect("the palette is open");
            palette
                .read(cx)
                .shown(cx)
                .into_iter()
                .map(|command| (command.name, command.keys))
                .collect()
        })
        .unwrap()
}

fn names(window: WindowHandle<BentermWindow>, cx: &mut VisualTestContext) -> Vec<String> {
    shown(window, cx)
        .into_iter()
        .map(|(name, _)| name)
        .collect()
}

fn selected(window: WindowHandle<BentermWindow>, cx: &mut VisualTestContext) -> Option<usize> {
    window
        .update(cx, |root, _, cx| {
            root.command_palette()
                .expect("the palette is open")
                .read(cx)
                .selected(cx)
        })
        .unwrap()
}

fn keys_of(window: WindowHandle<BentermWindow>, name: &str, cx: &mut VisualTestContext) -> String {
    shown(window, cx)
        .into_iter()
        .find(|(shown, _)| shown == name)
        .unwrap_or_else(|| panic!("{name} is in the palette"))
        .1
}

fn tab_count(window: WindowHandle<BentermWindow>, cx: &mut VisualTestContext) -> usize {
    window
        .update(cx, |root, _, cx| root.tabs().read(cx).tabs().len())
        .unwrap()
}

fn pane_count(window: WindowHandle<BentermWindow>, cx: &mut VisualTestContext) -> usize {
    window
        .update(cx, |root, _, cx| root.grid(cx).read(cx).panes().len())
        .unwrap()
}

/// Whether the keys go to the focused pane of the selected tab.
fn terminal_has_focus(window: WindowHandle<BentermWindow>, cx: &mut VisualTestContext) -> bool {
    let handle = window
        .update(cx, |root, _, cx| {
            root.terminal(cx).read(cx).focus_handle().clone()
        })
        .unwrap();
    cx.update(|window, _| handle.is_focused(window))
}

#[gpui::test]
fn cmd_k_opens_the_palette_and_takes_the_keys(cx: &mut TestAppContext) {
    let (window, mut cx) = open("sleep 100", cx);
    assert!(!is_open(window, &mut cx));
    cx.simulate_keystrokes("cmd-k");
    assert!(is_open(window, &mut cx));
    assert!(!terminal_has_focus(window, &mut cx));
}

#[gpui::test]
fn the_tab_pane_and_workspace_commands_are_listed_with_their_keys(cx: &mut TestAppContext) {
    let (window, mut cx) = open("sleep 100", cx);
    cx.simulate_keystrokes("cmd-k");
    let shown = shown(window, &mut cx);
    for (name, keys) in [
        ("Tab: New Tab", "⌘T"),
        ("Tab: Close", "⌘W"),
        ("Tab: Select Next Tab", "⇧⌘]"),
        ("Tab: Select Previous Tab", "⇧⌘["),
        ("Tab: Select Last Tab", "⌘9"),
        ("Tab: Rename Tab", "⌥⌘R"),
        ("Pane: Split Right", "⌘D"),
        ("Pane: Split Down", "⇧⌘D"),
        ("Pane: Focus Left", "⌥⌘←"),
        ("Pane: Focus Right", "⌥⌘→"),
        ("Pane: Focus Up", "⌥⌘↑"),
        ("Pane: Focus Down", "⌥⌘↓"),
        ("Pane: Resize Left", "⌃⌘←"),
        ("Pane: Resize Right", "⌃⌘→"),
        ("Pane: Resize Up", "⌃⌘↑"),
        ("Pane: Resize Down", "⌃⌘↓"),
        ("Pane: Toggle Zoom", "⇧⌘↩"),
        ("Workspace: New Workspace", "⌘N"),
        ("Workspace: Close Workspace", "⇧⌘W"),
        ("Workspace: Select Next Workspace", "⌃⌘]"),
        ("Workspace: Select Previous Workspace", "⌃⌘["),
        ("Workspace: Rename Workspace", "⇧⌘R"),
    ] {
        assert!(
            shown.contains(&(name.to_owned(), keys.to_owned())),
            "{name} {keys} is not in {shown:?}"
        );
    }
}

#[gpui::test]
fn the_palette_does_not_list_itself_and_starts_in_order_of_name(cx: &mut TestAppContext) {
    let (window, mut cx) = open("sleep 100", cx);
    cx.simulate_keystrokes("cmd-k");
    let names = names(window, &mut cx);
    assert!(
        names
            .iter()
            .all(|name| !name.starts_with("Command Palette"))
    );
    let mut sorted = names.clone();
    sorted.sort();
    assert_eq!(names, sorted);
    assert_eq!(selected(window, &mut cx), Some(0));
}

#[gpui::test]
fn the_keys_shown_are_the_keys_bound_now(cx: &mut TestAppContext) {
    let (window, mut cx) = open("sleep 100", cx);
    // A binding added later takes precedence, and the palette shows it.
    cx.update(|_, cx| {
        cx.bind_keys([KeyBinding::new("cmd-alt-t", NewTab, Some("TabGroup"))]);
    });
    cx.simulate_keystrokes("cmd-k");
    assert_eq!(keys_of(window, "Tab: New Tab", &mut cx), "⌥⌘T");
}

#[gpui::test]
fn typing_narrows_the_commands(cx: &mut TestAppContext) {
    let (window, mut cx) = open("sleep 100", cx);
    cx.simulate_keystrokes("cmd-k");
    cx.simulate_input("split");
    assert_eq!(
        names(window, &mut cx),
        vec!["Pane: Split Down", "Pane: Split Right"]
    );
    cx.simulate_input("zzz");
    assert!(names(window, &mut cx).is_empty());
    assert_eq!(selected(window, &mut cx), None);
    // Enter with nothing to run leaves the palette open.
    cx.simulate_keystrokes("enter");
    assert!(is_open(window, &mut cx));
    cx.simulate_keystrokes("backspace backspace backspace");
    assert_eq!(names(window, &mut cx).len(), 2);
}

#[gpui::test]
fn enter_runs_a_tab_command_and_gives_the_keys_back(cx: &mut TestAppContext) {
    let (window, mut cx) = open("sleep 100", cx);
    cx.simulate_keystrokes("cmd-k");
    cx.simulate_input("new tab");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(tab_count(window, &mut cx), 2);
    assert!(!is_open(window, &mut cx));
    assert!(terminal_has_focus(window, &mut cx));
}

#[gpui::test]
fn the_arrows_choose_the_command_enter_runs(cx: &mut TestAppContext) {
    let (window, mut cx) = open("sleep 100", cx);
    cx.simulate_keystrokes("cmd-k");
    cx.simulate_input("split");
    cx.simulate_keystrokes("down");
    assert_eq!(selected(window, &mut cx), Some(1));
    // The selection stops at the ends.
    cx.simulate_keystrokes("down");
    assert_eq!(selected(window, &mut cx), Some(1));
    cx.simulate_keystrokes("up up");
    assert_eq!(selected(window, &mut cx), Some(0));
    cx.simulate_keystrokes("down");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert_eq!(pane_count(window, &mut cx), 2);
    assert!(terminal_has_focus(window, &mut cx));
}

#[gpui::test]
fn typing_selects_the_best_match_again(cx: &mut TestAppContext) {
    let (window, mut cx) = open("sleep 100", cx);
    cx.simulate_keystrokes("cmd-k");
    cx.simulate_keystrokes("down down");
    cx.simulate_input("tab");
    assert_eq!(selected(window, &mut cx), Some(0));
}

#[gpui::test]
fn enter_runs_a_workspace_command(cx: &mut TestAppContext) {
    let (window, mut cx) = open("sleep 100", cx);
    cx.simulate_keystrokes("cmd-k");
    cx.simulate_input("new workspace");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    let count = window
        .update(&mut cx, |root, _, _| root.workspaces().len())
        .unwrap();
    assert_eq!(count, 2);
    assert!(terminal_has_focus(window, &mut cx));
}

#[gpui::test]
fn escape_closes_the_palette_without_running_anything(cx: &mut TestAppContext) {
    let (window, mut cx) = open("sleep 100", cx);
    cx.simulate_keystrokes("cmd-k");
    cx.simulate_input("new tab");
    cx.simulate_keystrokes("escape");
    assert!(!is_open(window, &mut cx));
    assert_eq!(tab_count(window, &mut cx), 1);
    assert!(terminal_has_focus(window, &mut cx));
}

#[gpui::test]
fn cmd_k_again_closes_the_palette(cx: &mut TestAppContext) {
    let (window, mut cx) = open("sleep 100", cx);
    cx.simulate_keystrokes("cmd-k");
    cx.simulate_keystrokes("cmd-k");
    assert!(!is_open(window, &mut cx));
    assert!(terminal_has_focus(window, &mut cx));
    // And it opens again empty.
    cx.simulate_keystrokes("cmd-k");
    assert!(is_open(window, &mut cx));
}

#[gpui::test]
fn clicking_a_command_runs_it(cx: &mut TestAppContext) {
    let (window, mut cx) = open("sleep 100", cx);
    cx.simulate_keystrokes("cmd-k");
    cx.simulate_input("split");
    cx.run_until_parked();
    let row = cx
        .debug_bounds("command-1")
        .expect("the second command is drawn");
    cx.simulate_click(row.center(), Modifiers::none());
    cx.run_until_parked();
    assert_eq!(pane_count(window, &mut cx), 2);
    assert!(!is_open(window, &mut cx));
}

#[gpui::test]
fn the_palette_uses_the_name_field_of_the_input_method(cx: &mut TestAppContext) {
    let (window, mut cx) = open("sleep 100", cx);
    cx.simulate_keystrokes("cmd-k");
    cx.run_until_parked();
    assert!(cx.debug_bounds("name-editor").is_some());
    // Enter while the input method converts belongs to it.
    window
        .update(&mut cx, |root, window, cx| {
            let editor = root.command_palette().unwrap().read(cx).editor().clone();
            editor.update(cx, |editor, cx| {
                editor.replace_and_mark_text_in_range(None, "すぷ", None, window, cx)
            });
        })
        .unwrap();
    cx.simulate_keystrokes("enter");
    assert!(is_open(window, &mut cx));
}

#[gpui::test]
fn palette_runs_pane_resize_and_toggle_zoom(cx: &mut TestAppContext) {
    let (window, mut cx) = open("sleep 100", cx);
    cx.simulate_keystrokes("cmd-d");
    cx.run_until_parked();

    let left_width = |cx: &mut VisualTestContext| {
        window
            .read_with(cx, |root, cx| {
                root.grid(cx).read(cx).pane_bounds()[0].1.size.width
            })
            .unwrap()
    };
    let before_width = left_width(&mut cx);

    // Run "Pane: Resize Right" from the command palette.
    cx.simulate_keystrokes("cmd-k");
    cx.simulate_input("resize right");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    assert!(left_width(&mut cx) > before_width);

    // Run "Pane: Toggle Zoom" from the command palette.
    cx.simulate_keystrokes("cmd-k");
    cx.simulate_input("toggle zoom");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    let zoomed = window
        .read_with(&cx, |root, cx| root.grid(cx).read(cx).zoomed())
        .unwrap();
    assert!(zoomed.is_some());
}

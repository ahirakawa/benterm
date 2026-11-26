//! Workspaces in a window, each with tabs of its own, and the sidebar that
//! lists them, from the keyboard and the mouse.

use std::time::{Duration, Instant};

use benterm_config::{ConfigEvent, parse};
use benterm_layout::WorkspaceId;
use benterm_terminal::PtyConfig;
use benterm_ui::settings::ConfigFeed;
use benterm_ui::{BentermWindow, TabGroup, bind_keys, open_terminal_window};
use gpui::{
    Bounds, Entity, Modifiers, MouseButton, Pixels, TestAppContext, VisualTestContext,
    WindowHandle, px,
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

/// A shell that exits a second after it reads a line.
const EXITS_WHEN_TOLD: &str = "stty -echo; read line; sleep 1; exit 0";

fn ids(window: WindowHandle<BentermWindow>, cx: &mut VisualTestContext) -> Vec<WorkspaceId> {
    window
        .update(cx, |root, _, _| root.workspaces())
        .expect("the window is open")
}

fn selected(window: WindowHandle<BentermWindow>, cx: &mut VisualTestContext) -> WorkspaceId {
    window
        .update(cx, |root, _, _| root.selected_workspace())
        .expect("the window is open")
}

fn names(window: WindowHandle<BentermWindow>, cx: &mut VisualTestContext) -> Vec<String> {
    window
        .update(cx, |root, _, _| {
            root.workspaces()
                .into_iter()
                .map(|id| root.workspace_name(id).unwrap())
                .collect()
        })
        .unwrap()
}

fn tabs_of(
    window: WindowHandle<BentermWindow>,
    id: WorkspaceId,
    cx: &mut VisualTestContext,
) -> Entity<TabGroup> {
    window
        .update(cx, |root, _, _| {
            root.workspace_tabs(id).expect("the workspace is open")
        })
        .unwrap()
}

fn tab_count(
    window: WindowHandle<BentermWindow>,
    id: WorkspaceId,
    cx: &mut VisualTestContext,
) -> usize {
    tabs_of(window, id, cx).read_with(cx, |tabs, _| tabs.tabs().len())
}

/// Whether GPUI's focus, which decides where typed keys go, is on the
/// focused pane of the selected tab of workspace `id`.
fn has_focus(
    window: WindowHandle<BentermWindow>,
    id: WorkspaceId,
    cx: &mut VisualTestContext,
) -> bool {
    let tabs = tabs_of(window, id, cx);
    let handle = tabs.read_with(cx, |tabs, cx| {
        let grid = tabs.selected_grid().read(cx);
        grid.terminal(grid.focused())
            .unwrap()
            .read(cx)
            .focus_handle()
            .clone()
    });
    cx.update(|window, _| handle.is_focused(window))
}

fn renaming(
    window: WindowHandle<BentermWindow>,
    cx: &mut VisualTestContext,
) -> Option<WorkspaceId> {
    window.update(cx, |root, _, _| root.renaming()).unwrap()
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

fn bounds(selector: &'static str, cx: &mut VisualTestContext) -> Bounds<Pixels> {
    cx.run_until_parked();
    cx.debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} is drawn"))
}

/// The first line of the focused pane of the selected workspace.
fn first_line(window: WindowHandle<BentermWindow>, cx: &mut VisualTestContext) -> String {
    window
        .update(cx, |root, _, cx| {
            root.terminal(cx).read(cx).screen().rows[0].text()
        })
        .unwrap()
}

#[gpui::test]
fn a_window_starts_with_one_workspace_in_the_sidebar(cx: &mut TestAppContext) {
    let (window, mut cx) = open("sleep 100", cx);
    let ids = ids(window, &mut cx);
    assert_eq!(ids.len(), 1);
    assert_eq!(selected(window, &mut cx), ids[0]);
    assert_eq!(names(window, &mut cx), vec!["Workspace 1"]);
    assert!(has_focus(window, ids[0], &mut cx));

    // The expanded sidebar is 240 points wide. The panel border sits
    // between the sidebar and the tab bar.
    let sidebar = bounds("sidebar", &mut cx);
    let tab_bar = bounds("tab-bar", &mut cx);
    assert_eq!(sidebar.left(), px(0.));
    assert_eq!(sidebar.size.width, px(240.));
    assert_eq!(tab_bar.left(), sidebar.right() + px(1.));
    let row = bounds("workspace-0", &mut cx);
    assert!(sidebar.contains(&row.center()));
}

#[gpui::test]
fn cmd_n_makes_a_workspace_with_tabs_of_its_own_and_focuses_it(cx: &mut TestAppContext) {
    let (window, mut cx) = open("sleep 100", cx);
    let first = selected(window, &mut cx);

    cx.simulate_keystrokes("cmd-n");

    let ids = ids(window, &mut cx);
    assert_eq!(ids.len(), 2);
    assert_eq!(ids[0], first);
    assert_eq!(selected(window, &mut cx), ids[1]);
    assert_eq!(names(window, &mut cx), vec!["Workspace 1", "Workspace 2"]);
    assert!(has_focus(window, ids[1], &mut cx));
    let (a, b) = (
        tabs_of(window, ids[0], &mut cx),
        tabs_of(window, ids[1], &mut cx),
    );
    assert_ne!(a.entity_id(), b.entity_id());
    // The window's tabs are those of the selected workspace.
    let shown = window
        .update(&mut cx, |root, _, _| root.tabs().entity_id())
        .unwrap();
    assert_eq!(shown, b.entity_id());
    assert!(cx.debug_bounds("workspace-1").is_some());
}

#[gpui::test]
fn every_workspace_gets_a_color_of_its_own(cx: &mut TestAppContext) {
    let (window, mut cx) = open("sleep 100", cx);
    cx.simulate_keystrokes("cmd-n cmd-n");
    let colors = window
        .update(&mut cx, |root, _, _| {
            root.workspaces()
                .into_iter()
                .map(|id| root.workspace_color(id).unwrap().index())
                .collect::<Vec<_>>()
        })
        .unwrap();
    assert_eq!(colors, vec![0, 1, 2]);
}

#[gpui::test]
fn a_new_tab_opens_in_the_selected_workspace_only(cx: &mut TestAppContext) {
    let (window, mut cx) = open("sleep 100", cx);
    cx.simulate_keystrokes("cmd-n cmd-t cmd-t");
    let ids = ids(window, &mut cx);
    assert_eq!(tab_count(window, ids[0], &mut cx), 1);
    assert_eq!(tab_count(window, ids[1], &mut cx), 3);
}

#[gpui::test]
fn ctrl_cmd_brackets_move_to_the_previous_and_next_workspace(cx: &mut TestAppContext) {
    let (window, mut cx) = open("sleep 100", cx);
    cx.simulate_keystrokes("cmd-n cmd-n");
    let ids = ids(window, &mut cx);
    assert_eq!(selected(window, &mut cx), ids[2]);

    cx.simulate_keystrokes("ctrl-cmd-[");
    assert_eq!(selected(window, &mut cx), ids[1]);
    assert!(has_focus(window, ids[1], &mut cx));
    cx.simulate_keystrokes("ctrl-cmd-] ctrl-cmd-]");
    assert_eq!(selected(window, &mut cx), ids[0]);
    assert!(has_focus(window, ids[0], &mut cx));
    cx.simulate_keystrokes("ctrl-cmd-[");
    assert_eq!(selected(window, &mut cx), ids[2]);
}

#[gpui::test]
fn a_workspace_comes_back_with_the_tab_it_was_left_on(cx: &mut TestAppContext) {
    let (window, mut cx) = open("sleep 100", cx);
    cx.simulate_keystrokes("cmd-t cmd-t cmd-2");
    let first = selected(window, &mut cx);
    let tabs = tabs_of(window, first, &mut cx);
    let left_on = tabs.read_with(&cx, |tabs, _| tabs.selected());

    cx.simulate_keystrokes("cmd-n ctrl-cmd-[");

    assert_eq!(selected(window, &mut cx), first);
    assert_eq!(tabs.read_with(&cx, |tabs, _| tabs.selected()), left_on);
    assert!(has_focus(window, first, &mut cx));
}

#[gpui::test]
fn typed_text_goes_to_the_selected_workspace(cx: &mut TestAppContext) {
    let (window, mut cx) = open("stty -echo; cat", cx);
    cx.simulate_keystrokes("cmd-n");
    let ids = ids(window, &mut cx);
    cx.simulate_input("second");
    cx.simulate_keystrokes("enter ctrl-cmd-[");
    cx.simulate_input("first");
    cx.simulate_keystrokes("enter");
    // The terminal may show the first letters twice, if they came before
    // the program turned the echo off.
    wait_until(&mut cx, "the first workspace shows its text", |cx| {
        first_line(window, cx).contains("first")
    });
    assert!(!first_line(window, &mut cx).contains("second"));
    cx.simulate_keystrokes("ctrl-cmd-]");
    assert_eq!(selected(window, &mut cx), ids[1]);
    wait_until(&mut cx, "the second workspace shows its text", |cx| {
        first_line(window, cx).contains("second")
    });
    assert!(!first_line(window, &mut cx).contains("first"));
}

#[gpui::test]
fn clicking_a_workspace_selects_it(cx: &mut TestAppContext) {
    let (window, mut cx) = open("sleep 100", cx);
    cx.simulate_keystrokes("cmd-n cmd-n");
    let ids = ids(window, &mut cx);

    let row = bounds("workspace-0", &mut cx);
    cx.simulate_click(row.center(), Modifiers::none());
    assert_eq!(selected(window, &mut cx), ids[0]);
    assert!(has_focus(window, ids[0], &mut cx));
    assert_eq!(renaming(window, &mut cx), None);
}

#[gpui::test]
fn the_plus_button_makes_a_workspace(cx: &mut TestAppContext) {
    let (window, mut cx) = open("sleep 100", cx);
    let button = bounds("workspace-new", &mut cx);
    cx.simulate_click(button.center(), Modifiers::none());
    let ids = ids(window, &mut cx);
    assert_eq!(ids.len(), 2);
    assert_eq!(selected(window, &mut cx), ids[1]);
    assert!(has_focus(window, ids[1], &mut cx));
}

#[gpui::test]
fn cmd_shift_w_closes_the_workspace_then_the_window(cx: &mut TestAppContext) {
    let (window, mut cx) = open("sleep 100", cx);
    cx.simulate_keystrokes("cmd-n cmd-n ctrl-cmd-[");
    let ids = ids(window, &mut cx);
    // The middle one has two tabs; all of it closes.
    cx.simulate_keystrokes("cmd-t");

    cx.simulate_keystrokes("cmd-shift-w");
    assert_eq!(self::ids(window, &mut cx), vec![ids[0], ids[2]]);
    // The one below takes its place.
    assert_eq!(selected(window, &mut cx), ids[2]);
    assert!(has_focus(window, ids[2], &mut cx));

    cx.simulate_keystrokes("cmd-shift-w");
    assert_eq!(self::ids(window, &mut cx), vec![ids[0]]);
    assert!(has_focus(window, ids[0], &mut cx));

    cx.simulate_keystrokes("cmd-shift-w");
    cx.run_until_parked();
    assert!(cx.windows().is_empty());
}

#[gpui::test]
fn the_cross_on_a_row_closes_that_workspace_only(cx: &mut TestAppContext) {
    let (window, mut cx) = open("sleep 100", cx);
    cx.simulate_keystrokes("cmd-n cmd-n");
    let ids = ids(window, &mut cx);

    // The cross shows on the row under the mouse.
    let row = bounds("workspace-0", &mut cx);
    cx.simulate_mouse_move(row.center(), None, Modifiers::none());
    let cross = bounds("workspace-close-0", &mut cx);
    cx.simulate_click(cross.center(), Modifiers::none());

    assert_eq!(self::ids(window, &mut cx), vec![ids[1], ids[2]]);
    // The click did not select the row it was on.
    assert_eq!(selected(window, &mut cx), ids[2]);
    assert!(has_focus(window, ids[2], &mut cx));
}

#[gpui::test]
fn a_workspace_closes_when_its_last_shell_exits(cx: &mut TestAppContext) {
    let (window, mut cx) = open(EXITS_WHEN_TOLD, cx);
    cx.simulate_keystrokes("cmd-n cmd-n ctrl-cmd-[");
    let ids = ids(window, &mut cx);

    cx.simulate_input("bye");
    cx.simulate_keystrokes("enter");
    wait_until(&mut cx, "the middle workspace closes", |cx| {
        self::ids(window, cx) == vec![ids[0], ids[2]]
    });
    // The workspace that took its place is selected and has the keys.
    assert_eq!(selected(window, &mut cx), ids[2]);
    assert!(has_focus(window, ids[2], &mut cx));
}

#[gpui::test]
fn a_workspace_closing_in_the_background_leaves_the_focus_alone(cx: &mut TestAppContext) {
    let (window, mut cx) = open(EXITS_WHEN_TOLD, cx);
    cx.simulate_keystrokes("cmd-n");
    let ids = ids(window, &mut cx);
    // The second workspace is told to exit just before the first is
    // selected again.
    cx.simulate_input("bye");
    cx.simulate_keystrokes("enter ctrl-cmd-[");
    assert_eq!(selected(window, &mut cx), ids[0]);

    wait_until(&mut cx, "the second workspace closes", |cx| {
        self::ids(window, cx) == vec![ids[0]]
    });
    assert_eq!(selected(window, &mut cx), ids[0]);
    assert!(has_focus(window, ids[0], &mut cx));
}

#[gpui::test]
fn a_tab_closing_in_a_workspace_not_shown_leaves_the_focus_alone(cx: &mut TestAppContext) {
    let (window, mut cx) = open(EXITS_WHEN_TOLD, cx);
    // The first workspace gets a second tab, which is its selected one and
    // is told to exit just before another workspace is made.
    cx.simulate_keystrokes("cmd-t");
    let first = selected(window, &mut cx);
    cx.simulate_input("bye");
    cx.simulate_keystrokes("enter cmd-n");
    let second = selected(window, &mut cx);
    assert_ne!(first, second);

    wait_until(&mut cx, "the tab closes", |cx| {
        tab_count(window, first, cx) == 1
    });
    // The keys stay with the workspace on screen.
    assert_eq!(selected(window, &mut cx), second);
    assert!(has_focus(window, second, &mut cx));
    assert!(!has_focus(window, first, &mut cx));
}

#[gpui::test]
fn cmd_shift_r_renames_the_selected_workspace(cx: &mut TestAppContext) {
    let (window, mut cx) = open("stty -echo; cat", cx);
    cx.simulate_keystrokes("cmd-n");
    let ids = ids(window, &mut cx);

    cx.simulate_keystrokes("cmd-shift-r");
    assert_eq!(renaming(window, &mut cx), Some(ids[1]));
    assert!(!has_focus(window, ids[1], &mut cx));
    // What is typed takes the place of the old name.
    cx.simulate_input("web");
    cx.simulate_keystrokes("enter");

    assert_eq!(names(window, &mut cx), vec!["Workspace 1", "web"]);
    assert_eq!(renaming(window, &mut cx), None);
    // The keys are back with the terminal, which saw none of the name.
    assert!(has_focus(window, ids[1], &mut cx));
    cx.simulate_input("after");
    cx.simulate_keystrokes("enter");
    wait_until(&mut cx, "the terminal shows what was typed after", |cx| {
        first_line(window, cx).contains("after")
    });
    assert!(!first_line(window, &mut cx).contains("web"));
}

#[gpui::test]
fn backspace_edits_the_name_being_typed(cx: &mut TestAppContext) {
    let (window, mut cx) = open("sleep 100", cx);
    cx.simulate_keystrokes("cmd-shift-r");
    cx.simulate_input("apix");
    cx.simulate_keystrokes("backspace");
    let typed = window
        .update(&mut cx, |root, _, cx| root.renaming_text(cx))
        .unwrap();
    assert_eq!(typed.as_deref(), Some("api"));
    cx.simulate_keystrokes("enter");
    assert_eq!(names(window, &mut cx), vec!["api"]);
}

#[gpui::test]
fn backspace_right_after_starting_clears_the_old_name(cx: &mut TestAppContext) {
    let (window, mut cx) = open("sleep 100", cx);
    cx.simulate_keystrokes("cmd-shift-r");
    let typed = |cx: &mut VisualTestContext| {
        window
            .update(cx, |root, _, cx| root.renaming_text(cx))
            .unwrap()
    };
    assert_eq!(typed(&mut cx).as_deref(), Some("Workspace 1"));
    cx.simulate_keystrokes("backspace");
    assert_eq!(typed(&mut cx).as_deref(), Some(""));
}

#[gpui::test]
fn escape_keeps_the_old_name(cx: &mut TestAppContext) {
    let (window, mut cx) = open("sleep 100", cx);
    let id = selected(window, &mut cx);
    cx.simulate_keystrokes("cmd-shift-r");
    cx.simulate_input("web");
    cx.simulate_keystrokes("escape");
    assert_eq!(names(window, &mut cx), vec!["Workspace 1"]);
    assert_eq!(renaming(window, &mut cx), None);
    assert!(has_focus(window, id, &mut cx));
}

#[gpui::test]
fn an_empty_name_keeps_the_old_name(cx: &mut TestAppContext) {
    let (window, mut cx) = open("sleep 100", cx);
    let id = selected(window, &mut cx);
    cx.simulate_keystrokes("cmd-shift-r backspace enter");
    assert_eq!(names(window, &mut cx), vec!["Workspace 1"]);
    assert_eq!(renaming(window, &mut cx), None);
    assert!(has_focus(window, id, &mut cx));
}

#[gpui::test]
fn a_double_click_on_a_row_renames_it(cx: &mut TestAppContext) {
    let (window, mut cx) = open("sleep 100", cx);
    cx.simulate_keystrokes("cmd-n");
    let ids = ids(window, &mut cx);

    let row = bounds("workspace-0", &mut cx).center();
    let none = Modifiers::none();
    cx.simulate_click(row, none);
    cx.simulate_event(gpui::MouseDownEvent {
        position: row,
        modifiers: none,
        button: MouseButton::Left,
        click_count: 2,
        first_mouse: false,
    });
    cx.simulate_mouse_up(row, MouseButton::Left, none);

    assert_eq!(selected(window, &mut cx), ids[0]);
    assert_eq!(renaming(window, &mut cx), Some(ids[0]));
    cx.simulate_input("api");
    cx.simulate_keystrokes("enter");
    assert_eq!(names(window, &mut cx), vec!["api", "Workspace 2"]);
}

#[gpui::test]
fn selecting_another_workspace_gives_up_the_renaming(cx: &mut TestAppContext) {
    let (window, mut cx) = open("sleep 100", cx);
    cx.simulate_keystrokes("cmd-n cmd-shift-r");
    let ids = ids(window, &mut cx);
    cx.simulate_input("web");

    let row = bounds("workspace-0", &mut cx);
    cx.simulate_click(row.center(), Modifiers::none());

    assert_eq!(renaming(window, &mut cx), None);
    assert_eq!(names(window, &mut cx), vec!["Workspace 1", "Workspace 2"]);
    assert_eq!(selected(window, &mut cx), ids[0]);
    assert!(has_focus(window, ids[0], &mut cx));
}

#[gpui::test]
fn a_tab_closing_while_a_name_is_typed_leaves_the_keys_with_the_name(cx: &mut TestAppContext) {
    let (window, mut cx) = open(EXITS_WHEN_TOLD, cx);
    // The second tab is told to exit just before the renaming starts.
    cx.simulate_keystrokes("cmd-t");
    let id = selected(window, &mut cx);
    cx.simulate_input("bye");
    cx.simulate_keystrokes("enter cmd-shift-r");
    cx.simulate_input("we");

    wait_until(&mut cx, "the tab closes", |cx| {
        tab_count(window, id, cx) == 1
    });
    assert_eq!(renaming(window, &mut cx), Some(id));
    assert!(!has_focus(window, id, &mut cx));
    cx.simulate_input("b");
    cx.simulate_keystrokes("enter");
    assert_eq!(names(window, &mut cx), vec!["web"]);
    assert!(has_focus(window, id, &mut cx));
}

#[gpui::test]
fn a_workspace_closing_while_it_is_renamed_ends_the_renaming(cx: &mut TestAppContext) {
    let (window, mut cx) = open(EXITS_WHEN_TOLD, cx);
    cx.simulate_keystrokes("cmd-n");
    let ids = ids(window, &mut cx);
    cx.simulate_input("bye");
    cx.simulate_keystrokes("enter cmd-shift-r");
    assert_eq!(renaming(window, &mut cx), Some(ids[1]));

    wait_until(&mut cx, "the workspace closes", |cx| {
        self::ids(window, cx) == vec![ids[0]]
    });
    assert_eq!(renaming(window, &mut cx), None);
    // The keys do not stay with a name that is gone.
    assert!(has_focus(window, ids[0], &mut cx));
}

#[gpui::test]
fn an_applied_config_reaches_the_workspaces_not_shown(cx: &mut TestAppContext) {
    let (window, mut cx) = open("sleep 100", cx);
    cx.simulate_keystrokes("cmd-n");
    let ids = ids(window, &mut cx);
    let event = ConfigEvent::Applied(parse("[terminal]\npadding = 7").unwrap());
    window
        .update(&mut cx, |root, _, cx| root.apply_config(event, cx))
        .unwrap();
    for id in ids {
        let tabs = tabs_of(window, id, &mut cx);
        let padding = tabs.read_with(&cx, |tabs, cx| {
            let grid = tabs.selected_grid().read(cx);
            let terminal = grid.terminal(grid.focused()).unwrap().read(cx);
            terminal.theme().terminal.padding
        });
        assert_eq!(padding, 7.0);
    }
}

#[gpui::test]
fn the_window_title_follows_the_selected_workspace(cx: &mut TestAppContext) {
    // Each shell sets a title, and another one a second after it reads a
    // line.
    let (window, mut cx) = open(
        "stty -echo; printf '\\033]0;%s\\007' \"t$$\"; read line; sleep 1; \
         printf '\\033]0;%s\\007' \"u$$\"; sleep 100",
        cx,
    );
    // What the window itself was told to show.
    let shown = |cx: &mut VisualTestContext| cx.window_title().unwrap_or_default();
    let title_of = |id: WorkspaceId, cx: &mut VisualTestContext| {
        tabs_of(window, id, cx).read_with(cx, |tabs, cx| tabs.window_title(cx))
    };
    wait_until(&mut cx, "the first title is set", |cx| {
        shown(cx).starts_with('t')
    });
    let first_title = shown(&mut cx);
    cx.simulate_keystrokes("cmd-n");
    wait_until(&mut cx, "the second title is set", |cx| {
        let shown = shown(cx);
        shown.starts_with('t') && shown != first_title
    });
    let ids = ids(window, &mut cx);

    // The second workspace changes its title while the first is shown,
    // which leaves the window's title alone.
    cx.simulate_input("go");
    cx.simulate_keystrokes("enter ctrl-cmd-[");
    assert_eq!(shown(&mut cx), first_title);
    wait_until(&mut cx, "the second workspace has its new title", |cx| {
        title_of(ids[1], cx).starts_with('u')
    });
    assert_eq!(shown(&mut cx), first_title);
    cx.simulate_keystrokes("ctrl-cmd-]");
    assert_eq!(shown(&mut cx), title_of(ids[1], &mut cx));
    assert!(shown(&mut cx).starts_with('u'));

    // Closing it shows the title of the one that takes its place.
    cx.simulate_keystrokes("cmd-shift-w");
    assert_eq!(shown(&mut cx), first_title);
}

#[gpui::test]
fn renaming_again_keeps_what_was_typed(cx: &mut TestAppContext) {
    let (window, mut cx) = open("sleep 100", cx);
    let id = selected(window, &mut cx);
    cx.simulate_keystrokes("cmd-shift-r");
    cx.simulate_input("draft");
    cx.simulate_keystrokes("cmd-shift-r");
    assert_eq!(renaming(window, &mut cx), Some(id));
    let typed = window
        .update(&mut cx, |root, _, cx| root.renaming_text(cx))
        .unwrap();
    assert_eq!(typed.as_deref(), Some("draft"));
    cx.simulate_keystrokes("enter");
    assert_eq!(names(window, &mut cx), vec!["draft"]);
    assert!(has_focus(window, id, &mut cx));
}

fn label(window: WindowHandle<BentermWindow>, cx: &mut VisualTestContext) -> String {
    window
        .update(cx, |root, _, cx| {
            let id = root.selected_workspace();
            root.workspace_label(id, cx).unwrap()
        })
        .unwrap()
}

/// A workspace nobody named is shown by the directory of its first
/// shell, and follows the shell when it goes elsewhere.
#[gpui::test]
fn a_workspace_without_a_name_is_shown_by_its_directory(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    std::fs::create_dir_all(root.join("api/docs")).unwrap();
    let script = format!(
        "cd {}/api; stty -echo; echo ready; read line; cd docs; echo moved; sleep 100",
        root.display()
    );
    let (window, mut cx) = open(&script, cx);

    wait_until(&mut cx, "the shell is in api", |cx| {
        label(window, cx) == "api"
    });
    // The name kept for the workspace is still the numbered one.
    assert_eq!(names(window, &mut cx), ["Workspace 1"]);

    cx.simulate_input("go\n");
    wait_until(&mut cx, "the shell is in docs", |cx| {
        label(window, cx) == "docs"
    });
}

/// A name the user gave stays, wherever the shell goes.
#[gpui::test]
fn a_workspace_with_a_name_keeps_it(cx: &mut TestAppContext) {
    let (window, mut cx) = open("echo ready; sleep 100", cx);
    let id = selected(window, &mut cx);
    window
        .update(&mut cx, |root, _, cx| {
            root.rename_workspace(id, "notes", cx)
        })
        .unwrap()
        .unwrap();

    assert_eq!(label(window, &mut cx), "notes");

    // Also a name of the form of the numbered one, when it was given.
    window
        .update(&mut cx, |root, _, cx| {
            root.rename_workspace(id, "Workspace 9", cx)
        })
        .unwrap()
        .unwrap();
    assert_eq!(label(window, &mut cx), "Workspace 9");
}

#[test]
fn a_directory_is_shown_by_its_last_part_and_home_by_a_tilde() {
    use benterm_ui::directory_label;
    use std::path::Path;
    let home = Some(Path::new("/Users/me"));
    assert_eq!(
        directory_label(Path::new("/Users/me/src/benterm"), home),
        "benterm"
    );
    assert_eq!(directory_label(Path::new("/Users/me"), home), "~");
    assert_eq!(directory_label(Path::new("/"), home), "/");
    assert_eq!(directory_label(Path::new("/tmp"), None), "tmp");
}

/// The field for a name has a caret that the arrows move, and the keys
/// of the Edit menu.
#[gpui::test]
fn a_name_is_edited_in_the_middle_selected_cut_and_undone(cx: &mut TestAppContext) {
    let (window, mut cx) = open("sleep 100", cx);
    let id = selected(window, &mut cx);
    cx.simulate_keystrokes("cmd-shift-r");
    cx.simulate_input("abc");
    cx.simulate_keystrokes("left");
    cx.simulate_input("x");
    cx.simulate_keystrokes("cmd-left");
    cx.simulate_input(">");
    cx.simulate_keystrokes("enter");
    assert_eq!(
        window
            .update(&mut cx, |root, _, _| root.workspace_name(id))
            .unwrap(),
        Some(">abxc".to_owned())
    );

    // Select all and cut leave the field empty and the text on the
    // clipboard; undo brings it back.
    cx.simulate_keystrokes("cmd-shift-r right cmd-a cmd-x");
    assert_eq!(
        cx.read_from_clipboard().and_then(|item| item.text()),
        Some(">abxc".to_owned())
    );
    cx.simulate_input("new");
    // Every key typed is an edit of its own.
    cx.simulate_keystrokes("cmd-z cmd-z cmd-z cmd-z enter");
    assert_eq!(
        window
            .update(&mut cx, |root, _, _| root.workspace_name(id))
            .unwrap(),
        Some(">abxc".to_owned())
    );
}

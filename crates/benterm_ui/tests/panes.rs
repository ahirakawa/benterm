//! Splitting the window into panes from the keyboard and the mouse.

use std::time::{Duration, Instant};

use benterm_core::Rgb;
use benterm_layout::PaneId;
use benterm_terminal::PtyConfig;
use benterm_ui::settings::ConfigFeed;
use benterm_ui::theme::Theme;
use benterm_ui::viewport::grid_size;
use benterm_ui::{BentermWindow, PaneGrid, TerminalView, bind_keys, open_terminal_window};
use gpui::{
    Bounds, Entity, Modifiers, MouseButton, Pixels, TestAppContext, VisualTestContext,
    WindowHandle, point, px, size,
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

fn grid(window: WindowHandle<BentermWindow>, cx: &mut VisualTestContext) -> Entity<PaneGrid> {
    window
        .update(cx, |root, _, cx| root.grid(cx))
        .expect("the window is open")
}

/// Lets the app run until `done` holds for the grid. The programs run in
/// real time on other threads, hence the polling.
fn wait_until(
    window: WindowHandle<BentermWindow>,
    cx: &mut VisualTestContext,
    what: &str,
    done: impl Fn(&PaneGrid, &gpui::App) -> bool,
) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        cx.run_until_parked();
        let grid = grid(window, cx);
        if cx.update(|_, cx| done(grid.read(cx), cx)) {
            return;
        }
        assert!(Instant::now() < deadline, "timed out waiting until {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn screen_text(view: &TerminalView) -> String {
    view.screen()
        .rows
        .iter()
        .map(|row| row.text())
        .collect::<Vec<_>>()
        .join("\n")
}

fn bounds_of(grid: &PaneGrid, id: PaneId) -> Bounds<Pixels> {
    grid.pane_bounds()
        .into_iter()
        .find(|(pane, _)| *pane == id)
        .expect("the pane is on the screen")
        .1
}

/// Whether GPUI's focus, which decides where typed keys go, is on the
/// terminal of pane `id`.
fn is_focused(grid: &Entity<PaneGrid>, id: PaneId, cx: &mut VisualTestContext) -> bool {
    let handle = grid.read_with(cx, |grid, cx| {
        grid.terminal(id).unwrap().read(cx).focus_handle().clone()
    });
    cx.update(|window, _| handle.is_focused(window))
}

#[gpui::test]
fn cmd_d_splits_right_and_focuses_a_new_shell(cx: &mut TestAppContext) {
    let (window, mut cx) = open("echo pid=$$; sleep 100", cx);
    let first = grid(window, &mut cx).read_with(&cx, |grid, _| grid.focused());

    cx.simulate_keystrokes("cmd-d");

    let grid = grid(window, &mut cx);
    let (panes, focused) = grid.read_with(&cx, |grid, _| (grid.panes(), grid.focused()));
    assert_eq!(panes.len(), 2);
    assert_eq!(panes[0], first);
    assert_eq!(focused, panes[1]);
    assert!(is_focused(&grid, focused, &mut cx));
    let (left, right) = grid.read_with(&cx, |grid, _| {
        (bounds_of(grid, panes[0]), bounds_of(grid, panes[1]))
    });
    assert_eq!(left.origin.y, right.origin.y);
    assert!(left.origin.x < right.origin.x);

    // Each pane runs a shell of its own.
    wait_until(window, &mut cx, "both panes show a pid", |grid, cx| {
        grid.panes()
            .iter()
            .all(|id| screen_text(grid.terminal(*id).unwrap().read(cx)).contains("pid="))
    });
    let texts = grid.read_with(&cx, |grid, cx| {
        grid.panes()
            .iter()
            .map(|id| screen_text(grid.terminal(*id).unwrap().read(cx)))
            .collect::<Vec<_>>()
    });
    assert_ne!(texts[0], texts[1]);
}

#[gpui::test]
fn cmd_shift_d_splits_down(cx: &mut TestAppContext) {
    let (window, mut cx) = open("sleep 100", cx);
    cx.simulate_keystrokes("cmd-shift-d");
    let grid = grid(window, &mut cx);
    let panes = grid.read_with(&cx, |grid, _| grid.panes());
    assert_eq!(panes.len(), 2);
    let (top, bottom) = grid.read_with(&cx, |grid, _| {
        (bounds_of(grid, panes[0]), bounds_of(grid, panes[1]))
    });
    assert_eq!(top.origin.x, bottom.origin.x);
    assert!(top.origin.y < bottom.origin.y);
    assert_eq!(grid.read_with(&cx, |grid, _| grid.focused()), panes[1]);
}

#[gpui::test]
fn cmd_alt_arrows_move_the_focus_between_panes(cx: &mut TestAppContext) {
    let (window, mut cx) = open("sleep 100", cx);
    cx.simulate_keystrokes("cmd-d");
    cx.simulate_keystrokes("cmd-shift-d");
    let grid = grid(window, &mut cx);
    let panes = grid.read_with(&cx, |grid, _| grid.panes());
    let [left, top_right, bottom_right] = panes[..] else {
        panic!("expected three panes, got {panes:?}");
    };
    assert_eq!(grid.read_with(&cx, |grid, _| grid.focused()), bottom_right);

    cx.simulate_keystrokes("cmd-alt-up");
    assert_eq!(grid.read_with(&cx, |grid, _| grid.focused()), top_right);
    assert!(is_focused(&grid, top_right, &mut cx));

    cx.simulate_keystrokes("cmd-alt-left");
    assert_eq!(grid.read_with(&cx, |grid, _| grid.focused()), left);
    assert!(is_focused(&grid, left, &mut cx));

    // Nothing is left of the left pane.
    cx.simulate_keystrokes("cmd-alt-left");
    assert_eq!(grid.read_with(&cx, |grid, _| grid.focused()), left);

    cx.simulate_keystrokes("cmd-alt-right cmd-alt-down");
    assert_eq!(grid.read_with(&cx, |grid, _| grid.focused()), bottom_right);
}

#[gpui::test]
fn a_click_focuses_the_pane_under_the_mouse(cx: &mut TestAppContext) {
    let (window, mut cx) = open("sleep 100", cx);
    cx.simulate_keystrokes("cmd-d");
    let grid = grid(window, &mut cx);
    let left = grid.read_with(&cx, |grid, _| grid.panes()[0]);
    let center = grid.read_with(&cx, |grid, _| bounds_of(grid, left).center());

    cx.simulate_click(center, Modifiers::none());

    assert_eq!(grid.read_with(&cx, |grid, _| grid.focused()), left);
    assert!(is_focused(&grid, left, &mut cx));
}

#[gpui::test]
fn dragging_the_border_resizes_the_panes_beside_it(cx: &mut TestAppContext) {
    let (window, mut cx) = open("sleep 100", cx);
    cx.simulate_keystrokes("cmd-d");
    let grid = grid(window, &mut cx);
    let [left, right] = grid.read_with(&cx, |grid, _| grid.panes())[..] else {
        panic!("expected two panes");
    };
    let (before, before_right) = grid.read_with(&cx, |grid, _| {
        (bounds_of(grid, left), bounds_of(grid, right))
    });
    let border = point(before.right(), before.center().y);

    // Aim just beside the one-third snap point.
    let width = before.size.width + before_right.size.width;
    let snapped_width = px((f32::from(width) / 3.).round());
    assert!(snapped_width < before.size.width);
    let target = point(before.left() + snapped_width + px(4.), border.y);
    cx.simulate_mouse_down(border, MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_move(target, MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_up(target, MouseButton::Left, Modifiers::none());
    cx.run_until_parked();

    let (after_left, after_right) = grid.read_with(&cx, |grid, _| {
        (bounds_of(grid, left), bounds_of(grid, right))
    });
    assert_eq!(after_left.size.width, snapped_width);
    assert_eq!(after_right.size.width, width - snapped_width);
    assert_eq!(after_right.origin.x, after_left.right());

    // Moving the mouse after the button is up changes nothing.
    cx.simulate_mouse_move(border, None, Modifiers::none());
    assert_eq!(
        grid.read_with(&cx, |grid, _| bounds_of(grid, left)),
        after_left
    );
}

#[gpui::test]
fn a_pane_whose_shell_exits_closes_and_its_neighbor_grows(cx: &mut TestAppContext) {
    let (window, mut cx) = open("stty -echo; read line; exit 0", cx);
    let first = grid(window, &mut cx).read_with(&cx, |grid, _| grid.focused());
    let full = grid(window, &mut cx).read_with(&cx, |grid, _| bounds_of(grid, first));
    cx.simulate_keystrokes("cmd-d");
    let second = grid(window, &mut cx).read_with(&cx, |grid, _| grid.focused());
    assert_ne!(first, second);

    // The new pane is focused, so Enter goes to its shell.
    cx.simulate_keystrokes("enter");
    wait_until(window, &mut cx, "the pane closes", |grid, _| {
        grid.panes() == vec![first]
    });

    let grid = grid(window, &mut cx);
    assert_eq!(grid.read_with(&cx, |grid, _| grid.focused()), first);
    assert!(grid.read_with(&cx, |grid, _| grid.terminal(second).is_none()));
    assert!(is_focused(&grid, first, &mut cx));
    assert_eq!(grid.read_with(&cx, |grid, _| bounds_of(grid, first)), full);
}

/// The size each pane's PTY reports, read from the `stty size` the pane's
/// shell prints whenever the size changes.
fn last_stty_size(view: &TerminalView) -> Option<(u16, u16)> {
    let text = screen_text(view);
    let line = text.lines().rev().find(|line| !line.trim().is_empty())?;
    let mut numbers = line.split_whitespace().map(|n| n.parse::<u16>().ok());
    let rows = numbers.next()??;
    let cols = numbers.next()??;
    Some((rows, cols))
}

#[gpui::test]
fn resizing_the_window_gives_every_pty_the_size_of_its_pane(cx: &mut TestAppContext) {
    let script = "stty -echo; trap 'stty size' WINCH; stty size; \
                  while :; do sleep 0.05; done";
    let (window, mut cx) = open(script, cx);
    cx.simulate_keystrokes("cmd-d");
    cx.simulate_keystrokes("cmd-shift-d");

    for new_size in [size(px(1200.), px(700.)), size(px(700.), px(500.))] {
        cx.simulate_resize(new_size);
        let padding = Theme::default().terminal.padding;
        wait_until(
            window,
            &mut cx,
            "every PTY has its pane's size",
            |grid, cx| {
                grid.pane_bounds().iter().all(|(id, bounds)| {
                    let view = grid.terminal(*id).unwrap().read(cx);
                    let cell = view.cell_size();
                    let wanted = grid_size(
                        (bounds.size.width.into(), bounds.size.height.into()),
                        padding,
                        (cell.width.into(), cell.height.into()),
                    );
                    view.screen().size == wanted
                        && last_stty_size(view) == Some((wanted.rows, wanted.cols))
                })
            },
        );
        // The panes fill the framed panel below the tab bar. The panel has
        // a 1 px border and 8 px of padding outside it on the right and bottom.
        let bar = cx.debug_bounds("tab-bar").expect("the tab bar is drawn");
        let sidebar = cx.debug_bounds("sidebar").expect("the sidebar is drawn");
        assert_eq!(bar.left(), sidebar.right() + px(1.));
        assert_eq!(bar.top(), sidebar.top() + px(5.));
        assert_eq!(bar.right(), new_size.width - px(9.));
        let area = Bounds::from_corners(
            point(bar.left(), bar.bottom()),
            point(bar.right(), new_size.height - px(9.)),
        );
        let grid = grid(window, &mut cx);
        let total: f32 = grid.read_with(&cx, |grid, _| {
            let panes = grid.pane_bounds();
            assert_eq!(panes.len(), 3);
            panes
                .iter()
                .map(|(_, b)| {
                    assert!(b.left() >= area.left() && b.right() <= area.right());
                    assert!(b.top() >= area.top() && b.bottom() <= area.bottom());
                    f32::from(b.size.width) * f32::from(b.size.height)
                })
                .sum()
        });
        let pane_area = f32::from(area.size.width) * f32::from(area.size.height);
        assert!((total - pane_area).abs() < 1.0, "{total} != {pane_area}");
    }
}

#[gpui::test]
fn a_config_change_reaches_every_pane_and_the_panes_split_off_later(cx: &mut TestAppContext) {
    let (window, mut cx) = open("sleep 100", cx);
    cx.simulate_keystrokes("cmd-d");
    let config = benterm_config::parse("[terminal]\npadding = 7").unwrap();
    window
        .update(&mut cx, |root, _, cx| {
            root.apply_config(benterm_config::ConfigEvent::Applied(config), cx)
        })
        .unwrap();
    cx.simulate_keystrokes("cmd-shift-d");

    let grid = grid(window, &mut cx);
    let paddings = grid.read_with(&cx, |grid, cx| {
        grid.panes()
            .iter()
            .map(|id| {
                grid.terminal(*id)
                    .unwrap()
                    .read(cx)
                    .theme()
                    .terminal
                    .padding
            })
            .collect::<Vec<_>>()
    });
    assert_eq!(paddings, vec![7.0, 7.0, 7.0]);
}

#[gpui::test]
fn saving_the_config_file_recolors_the_borders_and_the_focus_outline(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let colors = |divider: &str, focus: &str| {
        format!("[colors.pane]\ndivider = \"{divider}\"\nfocus_outline = \"{focus}\"\n")
    };
    std::fs::write(&path, colors("#010203", "#040506")).unwrap();
    cx.executor().allow_parking();
    cx.update(bind_keys);
    let window = cx
        .update(|cx| open_terminal_window(sh("sleep 100"), ConfigFeed::watch(&path), cx))
        .unwrap();
    let mut cx = VisualTestContext::from_window(window.into(), cx);
    cx.run_until_parked();
    cx.simulate_keystrokes("cmd-d");

    let painted = |grid: &PaneGrid| {
        let panes = &grid.theme().panes;
        (panes.divider, panes.focus_outline)
    };
    let grid_now = grid(window, &mut cx);
    assert_eq!(
        grid_now.read_with(&cx, |grid, _| painted(grid)),
        (Rgb(1, 2, 3), Rgb(4, 5, 6))
    );

    std::fs::write(&path, colors("#0a0b0c", "#0d0e0f")).unwrap();
    wait_until(
        window,
        &mut cx,
        "the new pane colors are used",
        |grid, _| painted(grid) == (Rgb(10, 11, 12), Rgb(13, 14, 15)),
    );
}

#[gpui::test]
fn ctrl_cmd_arrows_resize_the_focused_pane_and_stop_at_the_minimum(cx: &mut TestAppContext) {
    let (window, mut cx) = open("sleep 100", cx);
    cx.simulate_keystrokes("cmd-d");
    let grid = grid(window, &mut cx);
    let [left, right] = grid.read_with(&cx, |grid, _| grid.panes())[..] else {
        panic!("expected two panes");
    };
    let before_left = grid.read_with(&cx, |grid, _| bounds_of(grid, left));
    let before_right = grid.read_with(&cx, |grid, _| bounds_of(grid, right));

    // Moving the border right widens the left pane and narrows the right pane.
    cx.simulate_keystrokes("ctrl-cmd-right");
    cx.run_until_parked();
    let (after_left, after_right) = grid.read_with(&cx, |grid, _| {
        (bounds_of(grid, left), bounds_of(grid, right))
    });
    assert!(after_left.size.width > before_left.size.width);
    assert!(after_right.size.width < before_right.size.width);

    // Pushing far to the left stops at the minimum pane width and never shrinks
    // below it.
    for _ in 0..40 {
        cx.simulate_keystrokes("ctrl-cmd-left");
    }
    cx.run_until_parked();
    let min_left = grid.read_with(&cx, |grid, _| bounds_of(grid, left));
    assert!(min_left.size.width >= px(80.));
    cx.simulate_keystrokes("ctrl-cmd-left");
    cx.run_until_parked();
    assert_eq!(
        grid.read_with(&cx, |grid, _| bounds_of(grid, left))
            .size
            .width,
        min_left.size.width
    );
}

#[gpui::test]
fn cmd_shift_enter_toggles_zoom_and_focus_move_unzooms(cx: &mut TestAppContext) {
    let (window, mut cx) = open("sleep 100", cx);
    let first = grid(window, &mut cx).read_with(&cx, |grid, _| grid.focused());
    let full = grid(window, &mut cx).read_with(&cx, |grid, _| bounds_of(grid, first));

    cx.simulate_keystrokes("cmd-d");
    cx.simulate_keystrokes("ctrl-cmd-right");
    cx.run_until_parked();

    let grid = grid(window, &mut cx);
    let [left, right] = grid.read_with(&cx, |grid, _| grid.panes())[..] else {
        panic!("expected two panes");
    };
    let before = grid.read_with(&cx, |grid, _| grid.pane_bounds());
    assert_eq!(before.len(), 2);

    // Zooming shows only the focused pane at the full tab bounds.
    cx.simulate_keystrokes("cmd-shift-enter");
    cx.run_until_parked();
    assert_eq!(grid.read_with(&cx, |grid, _| grid.zoomed()), Some(right));
    assert_eq!(
        grid.read_with(&cx, |grid, _| grid.pane_bounds()),
        vec![(right, full)]
    );

    // Pressing again restores the original split bounds.
    cx.simulate_keystrokes("cmd-shift-enter");
    cx.run_until_parked();
    assert_eq!(grid.read_with(&cx, |grid, _| grid.zoomed()), None);
    assert_eq!(grid.read_with(&cx, |grid, _| grid.pane_bounds()), before);

    // Moving focus to another pane while zoomed unzooms and focuses that pane.
    cx.simulate_keystrokes("cmd-shift-enter");
    cx.run_until_parked();
    assert_eq!(grid.read_with(&cx, |grid, _| grid.zoomed()), Some(right));
    cx.simulate_keystrokes("cmd-alt-left");
    cx.run_until_parked();
    assert_eq!(grid.read_with(&cx, |grid, _| grid.zoomed()), None);
    assert_eq!(grid.read_with(&cx, |grid, _| grid.focused()), left);
    assert!(is_focused(&grid, left, &mut cx));
    assert_eq!(grid.read_with(&cx, |grid, _| grid.pane_bounds()), before);
}

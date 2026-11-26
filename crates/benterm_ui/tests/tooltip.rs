//! Hover tooltips: resting the mouse on an icon or button shows its
//! description after a short delay, and moving away hides it.

#[path = "common/pages.rs"]
mod pages;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::rc::Rc;
use std::time::{Duration, Instant};

use benterm_layout::SplitDirection;
use benterm_terminal::PtyConfig;
use benterm_ui::settings::ConfigFeed;
use benterm_ui::{BentermWindow, StartedTerminal, bind_keys, ipc, open_terminal_window};
use gpui::{Modifiers, TestAppContext, VisualTestContext, WindowHandle, point, px, size};

use pages::Pages;

const TIMEOUT: Duration = Duration::from_secs(10);
/// Longer than GPUI's tooltip hover delay (500ms).
const TOOLTIP_WAIT: Duration = Duration::from_millis(700);

fn hover_and_wait(cx: &mut VisualTestContext, target: gpui::Point<gpui::Pixels>) {
    cx.simulate_mouse_move(target, None, Modifiers::none());
    cx.run_until_parked();
    cx.executor().advance_clock(TOOLTIP_WAIT);
    cx.run_until_parked();
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
}

fn move_away(cx: &mut VisualTestContext) {
    cx.simulate_mouse_move(point(px(1.), px(1.)), None, Modifiers::none());
    cx.run_until_parked();
    cx.update(|window, _| window.refresh());
    cx.run_until_parked();
}

/// Opens a window whose first pane runs a fake `claude` printing a prompt so
/// that it appears in the session list with the Claude Code icon.
fn open_with_claude_session(
    cx: &mut TestAppContext,
) -> (
    WindowHandle<BentermWindow>,
    VisualTestContext,
    tempfile::TempDir,
) {
    cx.executor().allow_parking();
    cx.update(bind_keys);
    let dir = tempfile::Builder::new()
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap();
    let script = dir.path().join("claude");
    fs::write(
        &script,
        "#!/bin/sh\nprintf '\\033[H\\033[2J● Done.\\r\\n\\r\\n❯ \\r\\n  ? for shortcuts'\nwhile :; do sleep 1; done\n",
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    let pty = PtyConfig::command(&script, Vec::<String>::new());
    let start: ipc::StartTerminal = Rc::new(StartedTerminal::spawn);
    let window = cx
        .update(|cx| ipc::open_window(pty, start, None, ConfigFeed::defaults(), cx))
        .unwrap();
    let mut vcx = VisualTestContext::from_window(window.into(), cx);
    vcx.simulate_resize(size(px(1200.), px(800.)));
    vcx.update(|window, _| window.activate_window());
    vcx.run_until_parked();

    let deadline = Instant::now() + TIMEOUT;
    loop {
        vcx.run_until_parked();
        vcx.update(|window, _| window.refresh());
        vcx.run_until_parked();
        if vcx.debug_bounds("session-icon-0").is_some() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for session row"
        );
        std::thread::sleep(Duration::from_millis(5));
        vcx.executor().advance_clock(Duration::from_millis(5));
    }
    (window, vcx, dir)
}

#[gpui::test]
fn hovering_session_kind_icon_and_peek_button_shows_tooltip_and_leaving_hides_it(
    cx: &mut TestAppContext,
) {
    let (_window, mut vcx, _dir) = open_with_claude_session(cx);

    let icon = vcx
        .debug_bounds("session-icon-0")
        .expect("session kind icon is drawn");
    assert!(vcx.debug_bounds("tooltip").is_none());

    hover_and_wait(&mut vcx, icon.center());
    assert!(
        vcx.debug_bounds("tooltip").is_some(),
        "tooltip should appear after hovering on the session kind icon"
    );
    assert!(
        vcx.debug_bounds("tooltip:Claude Code").is_some(),
        "tooltip should describe the agent kind"
    );

    move_away(&mut vcx);
    assert!(
        vcx.debug_bounds("tooltip").is_none(),
        "tooltip should disappear after moving the mouse away"
    );

    let peek = vcx
        .debug_bounds("session-peek-0")
        .expect("session peek eye button is drawn");
    hover_and_wait(&mut vcx, peek.center());
    assert!(
        vcx.debug_bounds("tooltip").is_some(),
        "tooltip should appear after hovering on the peek eye button"
    );
    assert!(
        vcx.debug_bounds("tooltip:Peek").is_some(),
        "tooltip should describe the peek button"
    );

    move_away(&mut vcx);
    assert!(
        vcx.debug_bounds("tooltip").is_none(),
        "tooltip should disappear after moving away from the peek button"
    );
}

#[gpui::test]
fn hovering_browser_close_button_shows_close_tooltip_and_leaving_hides_it(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    cx.update(bind_keys);
    let _pages = cx.update(Pages::install);
    let pty = PtyConfig::command("/bin/sh", ["-c", "sleep 100"]);
    let window = cx
        .update(|cx| open_terminal_window(pty, ConfigFeed::defaults(), cx))
        .unwrap();
    let mut vcx = VisualTestContext::from_window(window.into(), cx);
    vcx.simulate_resize(size(px(1200.), px(800.)));
    vcx.run_until_parked();

    window
        .update(&mut vcx, |root, window, cx| {
            root.open_browser(
                Some("http://127.0.0.1:8080/docs"),
                None,
                SplitDirection::Right,
                false,
                window,
                cx,
            )
            .unwrap()
        })
        .unwrap();
    vcx.run_until_parked();
    vcx.update(|window, _| window.refresh());
    vcx.run_until_parked();

    let close = vcx
        .debug_bounds("browser-close")
        .expect("browser close button is drawn");
    assert!(vcx.debug_bounds("tooltip").is_none());

    hover_and_wait(&mut vcx, close.center());
    assert!(
        vcx.debug_bounds("tooltip").is_some(),
        "tooltip should appear after hovering on browser close button"
    );
    assert!(
        vcx.debug_bounds("tooltip:Close").is_some(),
        "tooltip should say Close"
    );

    move_away(&mut vcx);
    assert!(
        vcx.debug_bounds("tooltip").is_none(),
        "tooltip should disappear after moving away from browser close button"
    );
}

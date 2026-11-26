//! Mouse reports and file drops reach the terminal program in the window.
//!
//! Each program turns reports on, reads a known number of bytes in raw
//! mode and prints them in hex, so the test reads what was sent from the
//! screen.

use std::time::{Duration, Instant};

use benterm_terminal::PtyConfig;
use benterm_ui::settings::ConfigFeed;
use benterm_ui::theme::Theme;
use benterm_ui::{BentermWindow, PaneGrid, TerminalView, bind_keys, open_terminal_window};
use gpui::{
    Entity, ExternalPaths, FileDropEvent, Modifiers, MouseButton, MouseDownEvent, MouseUpEvent,
    Pixels, Point, ScrollDelta, ScrollEvent, ScrollSource, TestAppContext, TouchPhase,
    VisualTestContext, WindowHandle, point, px,
};

struct Harness {
    window: WindowHandle<BentermWindow>,
    cx: VisualTestContext,
}

#[gpui::test]
fn a_file_drop_pastes_quoted_paths_into_the_terminal(cx: &mut TestAppContext) {
    let expected = "'/tmp/a b.txt' '/tmp/it'\\''s.txt'";
    for (mode, prefix, suffix) in [("", "", ""), ("\\033[?2004h", "\x1b[200~", "\x1b[201~")] {
        let input = format!("{prefix}{expected}{suffix}");
        let mut app = Harness::open(mode, input.len(), cx);
        let position = app.cell(2, 2);
        let paths = ExternalPaths(
            ["/tmp/a b.txt", "/tmp/it's.txt"]
                .into_iter()
                .map(Into::into)
                .collect(),
        );
        app.cx
            .simulate_event(FileDropEvent::Entered { position, paths });
        app.cx.simulate_event(FileDropEvent::Submit { position });
        assert_eq!(app.received(), input.replace('\x1b', "ESC"));
    }
}

impl Harness {
    /// Opens a window whose pane runs `script` and waits until `ready`
    /// appears on the screen.
    fn open_script(script: &str, ready: &str, cx: &mut TestAppContext) -> Self {
        // The terminal's worker thread wakes the app from outside the test
        // scheduler, which the scheduler has to be told to accept.
        cx.executor().allow_parking();
        cx.update(bind_keys);
        let pty = PtyConfig::command("/bin/sh", ["-c", script]);
        let window = cx
            .update(|cx| open_terminal_window(pty, ConfigFeed::defaults(), cx))
            .unwrap();
        let mut harness = Self {
            window,
            cx: VisualTestContext::from_window(window.into(), cx),
        };
        harness.cx.update(|window, _| window.activate_window());
        harness.wait_for(ready);
        harness
    }

    /// Opens a window whose pane turns on the modes in `modes`, then
    /// prints in hex the first `count` bytes it reads.
    fn open(modes: &str, count: usize, cx: &mut TestAppContext) -> Self {
        let script = format!(
            "stty raw -echo; printf '{modes}ready\\r\\n'; \
             head -c {count} | od -An -tx1 | tr -d ' \\n'; printf '<end>'; \
             while :; do sleep 1; done"
        );
        Self::open_script(&script, "ready", cx)
    }

    fn grid(&mut self) -> Entity<PaneGrid> {
        self.window
            .update(&mut self.cx, |root, _, cx| root.grid(cx))
            .expect("the window is open")
    }

    fn terminal(&mut self) -> Entity<TerminalView> {
        let grid = self.grid();
        grid.read_with(&self.cx, |grid, _| {
            grid.terminal(grid.focused()).cloned().unwrap()
        })
    }

    fn screen_text(&mut self) -> String {
        self.terminal().read_with(&self.cx, |view, _| {
            let rows = &view.screen().rows;
            rows.iter()
                .map(|row| row.text())
                .collect::<Vec<_>>()
                .join("\n")
        })
    }

    /// Waits until the screen shows `text`. The program runs in real time
    /// on another thread, hence the polling.
    fn wait_for(&mut self, text: &str) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            self.cx.run_until_parked();
            if self.screen_text().contains(text) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "the screen never showed {text:?}:\n{}",
                self.screen_text()
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// Waits until the program has read its bytes and returns them as
    /// text, with each escape written as `ESC`.
    fn received(&mut self) -> String {
        self.wait_for("<end>");
        // The hex may wrap onto the next rows, which are then full.
        let joined: String = self.screen_text().lines().map(str::trim_end).collect();
        let after_ready = &joined[joined.find("ready").unwrap() + "ready".len()..];
        let hex = &after_ready[..after_ready.find("<end>").unwrap()];
        let bytes: Vec<u8> = (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
            .collect();
        String::from_utf8_lossy(&bytes).replace('\x1b', "ESC")
    }

    /// The window position of the middle of `col` on the `row`th row.
    fn cell(&mut self, row: usize, col: usize) -> Point<Pixels> {
        let grid = self.grid();
        let pane = grid.read_with(&self.cx, |grid, _| grid.pane_bounds()[0].1);
        let cell = self
            .terminal()
            .read_with(&self.cx, |view, _| view.cell_size());
        let padding = px(Theme::default().terminal.padding);
        pane.origin
            + point(
                padding + cell.width * (col as f32 + 0.5),
                padding + cell.height * (row as f32 + 0.5),
            )
    }

    fn click(&mut self, at: Point<Pixels>, button: MouseButton, modifiers: Modifiers) {
        self.cx.simulate_event(MouseDownEvent {
            button,
            position: at,
            modifiers,
            click_count: 1,
            first_mouse: false,
        });
        self.cx.simulate_event(MouseUpEvent {
            button,
            position: at,
            modifiers,
            click_count: 1,
        });
    }

    fn drag(&mut self, from: Point<Pixels>, to: Point<Pixels>, modifiers: Modifiers) {
        self.cx
            .simulate_mouse_down(from, MouseButton::Left, modifiers);
        self.cx
            .simulate_mouse_move(to, Some(MouseButton::Left), modifiers);
        self.cx.simulate_mouse_up(to, MouseButton::Left, modifiers);
    }

    fn scroll(&mut self, at: Point<Pixels>, lines: f32) {
        self.cx.simulate_event(ScrollEvent {
            position: at,
            delta: ScrollDelta::Lines(point(0., lines)),
            modifiers: Modifiers::none(),
            touch_phase: TouchPhase::Moved,
            source: ScrollSource::Pointer,
        });
    }
}

fn shift() -> Modifiers {
    Modifiers {
        shift: true,
        ..Modifiers::none()
    }
}

const CLICKS_SGR: &str = "\\033[?1000h\\033[?1006h";

#[gpui::test]
fn a_click_is_reported_in_sgr(cx: &mut TestAppContext) {
    let mut h = Harness::open(CLICKS_SGR, 18, cx);
    let at = h.cell(3, 2);
    h.click(at, MouseButton::Left, Modifiers::none());
    assert_eq!(h.received(), "ESC[<0;3;4MESC[<0;3;4m");
}

#[gpui::test]
fn a_right_click_with_control_is_reported(cx: &mut TestAppContext) {
    let mut h = Harness::open(CLICKS_SGR, 20, cx);
    let at = h.cell(0, 0);
    let control = Modifiers {
        control: true,
        ..Modifiers::none()
    };
    h.click(at, MouseButton::Right, control);
    assert_eq!(h.received(), "ESC[<18;1;1MESC[<18;1;1m");
}

#[gpui::test]
fn a_click_is_reported_in_x10_without_a_format_mode(cx: &mut TestAppContext) {
    let mut h = Harness::open("\\033[?1000h", 12, cx);
    let at = h.cell(1, 4);
    h.click(at, MouseButton::Left, Modifiers::none());
    assert_eq!(h.received(), "ESC[M %\"ESC[M#%\"");
}

#[gpui::test]
fn a_drag_is_reported_as_press_moves_and_release(cx: &mut TestAppContext) {
    let mut h = Harness::open("\\033[?1002h\\033[?1006h", 28, cx);
    let (from, to) = (h.cell(3, 2), h.cell(3, 5));
    h.drag(from, to, Modifiers::none());
    assert_eq!(h.received(), "ESC[<0;3;4MESC[<32;6;4MESC[<0;6;4m");
}

#[gpui::test]
fn the_wheel_is_reported_as_steps(cx: &mut TestAppContext) {
    let mut h = Harness::open(CLICKS_SGR, 30, cx);
    let at = h.cell(1, 1);
    h.scroll(at, 2.);
    h.scroll(at, -1.);
    assert_eq!(h.received(), "ESC[<64;2;2MESC[<64;2;2MESC[<65;2;2M");
}

#[gpui::test]
fn a_shift_drag_is_not_reported(cx: &mut TestAppContext) {
    let mut h = Harness::open(CLICKS_SGR, 18, cx);
    let (from, to) = (h.cell(0, 0), h.cell(0, 3));
    h.drag(from, to, shift());
    let at = h.cell(2, 2);
    h.click(at, MouseButton::Left, Modifiers::none());
    assert_eq!(h.received(), "ESC[<0;3;3MESC[<0;3;3m");
}

#[gpui::test]
fn the_wheel_sends_arrow_keys_on_the_alternate_screen_without_mouse_tracking(
    cx: &mut TestAppContext,
) {
    let mut h = Harness::open("\\033[?1049h", 18, cx);
    let at = h.cell(1, 1);
    h.scroll(at, 3.);
    h.scroll(at, -3.);
    assert_eq!(h.received(), "ESC[AESC[AESC[AESC[BESC[BESC[B");
}

#[gpui::test]
fn the_wheel_follows_application_cursor_mode_on_the_alternate_screen(cx: &mut TestAppContext) {
    let mut h = Harness::open("\\033[?1049h\\033[?1h", 18, cx);
    let at = h.cell(1, 1);
    h.scroll(at, 3.);
    h.scroll(at, -3.);
    assert_eq!(h.received(), "ESCOAESCOAESCOAESCOBESCOBESCOB");
}

#[gpui::test]
fn fractional_wheel_scrolls_carry_over_on_the_alternate_screen(cx: &mut TestAppContext) {
    let mut h = Harness::open("\\033[?1049h", 6, cx);
    let at = h.cell(1, 1);
    h.scroll(at, 0.4);
    h.scroll(at, 0.4);
    h.scroll(at, 0.4);
    h.scroll(at, -1.2);
    assert_eq!(h.received(), "ESC[AESC[B");
}

#[gpui::test]
fn the_wheel_still_reports_mouse_steps_on_the_alternate_screen_when_tracking_is_on(
    cx: &mut TestAppContext,
) {
    let mut h = Harness::open("\\033[?1049h\\033[?1000h\\033[?1006h", 20, cx);
    let at = h.cell(1, 1);
    h.scroll(at, 1.);
    h.scroll(at, -1.);
    assert_eq!(h.received(), "ESC[<64;2;2MESC[<65;2;2M");
}

#[gpui::test]
fn the_wheel_scrolls_history_on_the_primary_screen(cx: &mut TestAppContext) {
    let script = "seq 1 60; printf 'ready\\r\\n'; while :; do sleep 1; done";
    let mut h = Harness::open_script(script, "ready", cx);
    let view = h.terminal();
    assert!(!view.read_with(&h.cx, |view, _| view.is_scrolled_back()));
    let at = h.cell(1, 1);
    h.scroll(at, 3.);
    assert!(view.read_with(&h.cx, |view, _| view.is_scrolled_back()));
}

#[gpui::test]
fn leaving_the_alternate_screen_resets_fractional_wheel_scrolling(cx: &mut TestAppContext) {
    let script = "stty raw -echo; \
                  printf '\\033[?1049halt1\\r\\n'; head -c 1 > /dev/null; \
                  printf '\\033[?1049lprimary\\r\\n'; head -c 1 > /dev/null; \
                  printf '\\033[?1049hready\\r\\n'; \
                  head -c 3 | od -An -tx1 | tr -d ' \\n'; printf '<end>'; \
                  while :; do sleep 1; done";
    let mut h = Harness::open_script(script, "alt1", cx);
    let at = h.cell(1, 1);
    h.scroll(at, 0.5);
    assert!(h.terminal().read_with(&h.cx, |view, _| view.send_text("x")));
    h.wait_for("primary");
    assert!(h.terminal().read_with(&h.cx, |view, _| view.send_text("y")));
    h.wait_for("ready");
    h.scroll(at, 0.6);
    h.scroll(at, -1.7);
    assert_eq!(h.received(), "ESC[B");
}

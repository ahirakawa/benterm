//! Selecting with the mouse in the window and copying with cmd-c.

use std::time::{Duration, Instant};

use benterm_terminal::PtyConfig;
use benterm_ui::settings::ConfigFeed;
use benterm_ui::theme::Theme;
use benterm_ui::{BentermWindow, PaneGrid, TerminalView, bind_keys, open_terminal_window};
use gpui::{
    ClipboardItem, Entity, Modifiers, MouseButton, MouseDownEvent, MouseUpEvent, Pixels, Point,
    ScrollDelta, ScrollEvent, ScrollSource, TestAppContext, TouchPhase, VisualTestContext,
    WindowHandle, point, px,
};

/// Put on the clipboard before a copy, to tell when nothing was copied.
const UNTOUCHED: &str = "<nothing copied>";

struct Harness {
    window: WindowHandle<BentermWindow>,
    cx: VisualTestContext,
}

impl Harness {
    /// Opens a window whose pane runs `script`, and waits until the
    /// screen shows `ready`.
    fn open(script: &str, ready: &str, cx: &mut TestAppContext) -> Self {
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

    /// The window position of `col` on the `row`th row of the pane, a
    /// quarter of a cell into its left or right half.
    fn cell(&mut self, row: usize, col: usize, right: bool) -> Point<Pixels> {
        let grid = self.grid();
        let pane = grid.read_with(&self.cx, |grid, _| grid.pane_bounds()[0].1);
        let cell = self
            .terminal()
            .read_with(&self.cx, |view, _| view.cell_size());
        let padding = px(Theme::default().terminal.padding);
        let x = col as f32 + if right { 0.75 } else { 0.25 };
        pane.origin
            + point(
                padding + cell.width * x,
                padding + cell.height * (row as f32 + 0.5),
            )
    }

    fn drag(&mut self, from: Point<Pixels>, to: Point<Pixels>, modifiers: Modifiers) {
        self.cx
            .simulate_mouse_down(from, MouseButton::Left, modifiers);
        self.cx
            .simulate_mouse_move(to, Some(MouseButton::Left), modifiers);
        self.cx.simulate_mouse_up(to, MouseButton::Left, modifiers);
    }

    fn click(&mut self, at: Point<Pixels>, count: usize) {
        self.cx.simulate_event(MouseDownEvent {
            button: MouseButton::Left,
            position: at,
            modifiers: Modifiers::none(),
            click_count: count,
            first_mouse: false,
        });
        self.cx.simulate_event(MouseUpEvent {
            button: MouseButton::Left,
            position: at,
            modifiers: Modifiers::none(),
            click_count: count,
        });
    }

    /// Presses cmd-c and returns what is on the clipboard.
    fn copy(&mut self) -> String {
        self.cx
            .write_to_clipboard(ClipboardItem::new_string(UNTOUCHED.into()));
        self.cx.simulate_keystrokes("cmd-c");
        self.cx
            .read_from_clipboard()
            .and_then(|item| item.text())
            .unwrap_or_default()
    }

    fn scroll_up(&mut self, lines: f32) {
        let at = self.cell(0, 0, false);
        self.cx.simulate_event(ScrollEvent {
            position: at,
            delta: ScrollDelta::Lines(point(0., lines)),
            modifiers: Modifiers::none(),
            touch_phase: TouchPhase::Moved,
            source: ScrollSource::Pointer,
        });
    }

    fn rows(&mut self) -> usize {
        self.terminal()
            .read_with(&self.cx, |view, _| view.screen().rows.len())
    }

    fn scrollback_len(&mut self) -> usize {
        self.terminal()
            .read_with(&self.cx, |view, _| view.screen().scrollback_len)
    }
}

/// Prints `text` and then waits, reading nothing.
fn show(text: &str) -> String {
    format!("stty -echo; printf '{text}'; while :; do sleep 1; done")
}

fn shift() -> Modifiers {
    Modifiers {
        shift: true,
        ..Modifiers::none()
    }
}

#[gpui::test]
fn a_drag_selects_and_cmd_c_copies(cx: &mut TestAppContext) {
    let mut h = Harness::open(&show("hello world\\r\\nsecond line"), "second", cx);
    let (from, to) = (h.cell(0, 0, false), h.cell(0, 4, true));
    h.drag(from, to, Modifiers::none());
    assert_eq!(h.copy(), "hello");

    let (from, to) = (h.cell(0, 6, false), h.cell(1, 5, true));
    h.drag(from, to, Modifiers::none());
    assert_eq!(h.copy(), "world\nsecond");
}

#[gpui::test]
fn cmd_c_without_a_selection_leaves_the_clipboard_alone(cx: &mut TestAppContext) {
    let mut h = Harness::open(&show("hello"), "hello", cx);
    assert_eq!(h.copy(), UNTOUCHED);
    // A click clears the selection.
    let (from, to) = (h.cell(0, 0, false), h.cell(0, 4, true));
    h.drag(from, to, Modifiers::none());
    let at = h.cell(0, 2, false);
    h.click(at, 1);
    assert_eq!(h.copy(), UNTOUCHED);
}

#[gpui::test]
fn a_double_click_selects_a_word_and_a_triple_click_the_line(cx: &mut TestAppContext) {
    let mut h = Harness::open(&show("cargo test --workspace\\r\\nnext"), "next", cx);
    let at = h.cell(0, 14, false);
    h.click(at, 2);
    assert_eq!(h.copy(), "--workspace");
    h.click(at, 3);
    assert_eq!(h.copy(), "cargo test --workspace");
}

#[gpui::test]
fn a_selection_never_cuts_a_wide_character(cx: &mut TestAppContext) {
    // 日 covers columns 0-1, 本 2-3, 語 4-5.
    let mut h = Harness::open(&show("日本語テキスト"), "テキスト", cx);
    let (from, to) = (h.cell(0, 1, false), h.cell(0, 4, true));
    h.drag(from, to, Modifiers::none());
    assert_eq!(h.copy(), "日本語");
    let at = h.cell(0, 3, true);
    h.click(at, 2);
    assert_eq!(h.copy(), "日本語テキスト");
}

#[gpui::test]
fn a_selection_reaches_from_the_scrollback_onto_the_screen(cx: &mut TestAppContext) {
    let script = "stty -echo; i=0; while [ $i -lt 80 ]; do printf 'line %d\\r\\n' $i; \
                  i=$((i+1)); done; printf end; while :; do sleep 1; done";
    let mut h = Harness::open(script, "end", cx);
    let rows = h.rows();
    let history = h.scrollback_len();
    assert!(history >= 3, "the test needs a few rows of scrollback");
    h.scroll_up(3.);
    h.cx.run_until_parked();

    // The top row of the pane is now scrollback line `history - 3`, and
    // the last full row is screen row `rows - 4`.
    let (from, to) = (h.cell(0, 0, false), h.cell(rows - 1, 7, true));
    h.drag(from, to, Modifiers::none());
    let first = history - 3;
    let last = history + rows - 4;
    let expected: Vec<String> = (first..=last).map(|i| format!("line {i}")).collect();
    assert_eq!(h.copy(), expected.join("\n"));
}

#[gpui::test]
fn output_does_not_move_a_selection_in_the_scrollback(cx: &mut TestAppContext) {
    // Each line typed makes the program print ten more lines.
    let script = "stty -echo; i=0; while [ $i -lt 60 ]; do printf 'line %d\\r\\n' $i; \
                  i=$((i+1)); done; printf ready; \
                  while read x; do j=0; while [ $j -lt 10 ]; do printf '\\r\\nmore %d' $j; \
                  j=$((j+1)); done; done";
    let mut h = Harness::open(script, "ready", cx);
    h.scroll_up(5.);
    h.cx.run_until_parked();
    let (from, to) = (h.cell(1, 0, false), h.cell(2, 7, true));
    h.drag(from, to, Modifiers::none());
    let before = h.copy();
    assert!(before.starts_with("line "), "copied {before:?}");

    h.cx.simulate_keystrokes("enter");
    h.wait_for("more 9");
    assert_eq!(h.copy(), before);
}

#[gpui::test]
fn output_over_a_selection_on_the_screen_clears_it(cx: &mut TestAppContext) {
    let script = "stty -echo; printf 'first\\r\\nsecond'; \
                  while read x; do printf '\\033[1;1Hreplaced'; done";
    let mut h = Harness::open(script, "second", cx);
    let (from, to) = (h.cell(0, 0, false), h.cell(0, 4, true));
    h.drag(from, to, Modifiers::none());
    assert_eq!(h.copy(), "first");
    h.cx.simulate_keystrokes("enter");
    h.wait_for("replaced");
    assert_eq!(h.copy(), UNTOUCHED);
}

#[gpui::test]
fn when_the_program_takes_the_mouse_only_a_shift_drag_selects(cx: &mut TestAppContext) {
    let mut h = Harness::open(&show("\\033[?1049h\\033[?1000hfull screen app"), "app", cx);
    let (from, to) = (h.cell(0, 0, false), h.cell(0, 3, true));
    h.drag(from, to, Modifiers::none());
    assert_eq!(h.copy(), UNTOUCHED);
    h.drag(from, to, shift());
    assert_eq!(h.copy(), "full");
}

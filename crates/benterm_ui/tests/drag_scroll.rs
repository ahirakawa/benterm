//! Auto-scrolling when a selection drag is held above or below a pane.

use std::ops::Range;
use std::time::{Duration, Instant};

use benterm_terminal::PtyConfig;
use benterm_ui::selection::DRAG_SCROLL_INTERVAL;
use benterm_ui::settings::ConfigFeed;
use benterm_ui::theme::Theme;
use benterm_ui::{BentermWindow, PaneGrid, TerminalView, bind_keys, open_terminal_window};
use gpui::{
    Bounds, ClipboardItem, Entity, Modifiers, MouseButton, Pixels, Point, ScrollDelta, ScrollEvent,
    ScrollSource, Size, TestAppContext, TouchPhase, VisualTestContext, WindowHandle, point, px,
};

const UNTOUCHED: &str = "<nothing copied>";

const SCROLLBACK_SCRIPT: &str = "stty -echo; i=0; while [ $i -lt 80 ]; do printf 'line %d\\r\\n' $i; \
                                 i=$((i+1)); done; printf end; while :; do sleep 1; done";

struct Harness {
    window: WindowHandle<BentermWindow>,
    cx: VisualTestContext,
}

impl Harness {
    fn open(script: &str, ready: &str, cx: &mut TestAppContext) -> Self {
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
            view.screen()
                .rows
                .iter()
                .map(|row| row.text())
                .collect::<Vec<_>>()
                .join("\n")
        })
    }

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

    fn focused_pane_bounds(&mut self) -> Bounds<Pixels> {
        let grid = self.grid();
        grid.read_with(&self.cx, |grid, _| {
            let focused = grid.focused();
            grid.pane_bounds()
                .into_iter()
                .find(|(id, _)| *id == focused)
                .unwrap()
                .1
        })
    }

    fn cell_size(&mut self) -> Size<Pixels> {
        self.terminal()
            .read_with(&self.cx, |view, _| view.cell_size())
    }

    fn cell(&mut self, row: usize, col: usize, right: bool) -> Point<Pixels> {
        let pane = self.focused_pane_bounds();
        let cell = self.cell_size();
        let padding = px(Theme::default().terminal.padding);
        let x = col as f32 + if right { 0.75 } else { 0.25 };
        pane.origin
            + point(
                padding + cell.width * x,
                padding + cell.height * (row as f32 + 0.5),
            )
    }

    fn visible_lines(&mut self) -> Range<usize> {
        self.terminal()
            .read_with(&self.cx, |view, _| view.visible_lines())
    }

    fn is_drag_scrolling(&mut self) -> bool {
        self.terminal()
            .read_with(&self.cx, |view, _| view.is_drag_scrolling())
    }

    fn advance_ticks(&mut self, count: u32) {
        for _ in 0..count {
            self.cx.executor().advance_clock(DRAG_SCROLL_INTERVAL);
            self.cx.run_until_parked();
        }
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
        self.cx.run_until_parked();
    }

    fn copy(&mut self) -> String {
        self.cx
            .write_to_clipboard(ClipboardItem::new_string(UNTOUCHED.into()));
        self.cx.simulate_keystrokes("cmd-c");
        self.cx
            .read_from_clipboard()
            .and_then(|item| item.text())
            .unwrap_or_default()
    }
}

#[gpui::test]
fn dragging_above_the_pane_scrolls_up_and_extends_the_selection(cx: &mut TestAppContext) {
    let mut h = Harness::open(SCROLLBACK_SCRIPT, "end", cx);
    let pane = h.focused_pane_bounds();
    let cell = h.cell_size();
    let start = h.cell(5, 4, false);
    let above = point(start.x, pane.top() - cell.height * 0.5);

    let before = h.visible_lines().start;
    assert!(before >= 10, "need enough scrollback for auto-scroll");

    h.cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::none());
    h.cx.simulate_mouse_move(above, Some(MouseButton::Left), Modifiers::none());
    h.cx.run_until_parked();
    assert!(h.is_drag_scrolling());

    h.advance_ticks(1);
    let after_one = h.visible_lines().start;
    assert_eq!(before - after_one, 1);

    h.advance_ticks(2);
    let after_three = h.visible_lines().start;
    assert_eq!(before - after_three, 3);

    h.cx.simulate_mouse_up(above, MouseButton::Left, Modifiers::none());
    h.cx.run_until_parked();
    let copied = h.copy();
    assert!(
        copied.contains(&format!("line {after_three}")),
        "selection should extend into scrolled-up lines, got: {copied:?}"
    );
}

#[gpui::test]
fn dragging_below_the_pane_scrolls_down_and_extends_the_selection(cx: &mut TestAppContext) {
    let mut h = Harness::open(SCROLLBACK_SCRIPT, "end", cx);
    h.scroll_up(20.);
    let pane = h.focused_pane_bounds();
    let cell = h.cell_size();
    let start = h.cell(2, 0, false);
    let below = point(start.x, pane.bottom() + cell.height * 0.5);

    let before = h.visible_lines().start;
    h.cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::none());
    h.cx.simulate_mouse_move(below, Some(MouseButton::Left), Modifiers::none());
    h.cx.run_until_parked();
    assert!(h.is_drag_scrolling());

    h.advance_ticks(3);
    let after = h.visible_lines().start;
    assert_eq!(after - before, 3);
}

#[gpui::test]
fn dragging_further_outside_scrolls_more_lines_per_tick(cx: &mut TestAppContext) {
    let mut h = Harness::open(SCROLLBACK_SCRIPT, "end", cx);
    let pane = h.focused_pane_bounds();
    let cell = h.cell_size();
    let start = h.cell(5, 0, false);
    let near = point(start.x, pane.top() - cell.height * 0.5);
    let far = point(start.x, pane.top() - cell.height * 4.5);

    h.cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::none());
    h.cx.simulate_mouse_move(near, Some(MouseButton::Left), Modifiers::none());
    h.cx.run_until_parked();

    let before_near = h.visible_lines().start;
    h.advance_ticks(1);
    let near_step = before_near - h.visible_lines().start;

    h.cx.simulate_mouse_move(far, Some(MouseButton::Left), Modifiers::none());
    h.cx.run_until_parked();

    let before_far = h.visible_lines().start;
    h.advance_ticks(1);
    let far_step = before_far - h.visible_lines().start;

    assert_eq!(near_step, 1);
    assert_eq!(far_step, 5);
    assert!(far_step > near_step);
}

#[gpui::test]
fn releasing_the_button_stops_auto_scrolling(cx: &mut TestAppContext) {
    let mut h = Harness::open(SCROLLBACK_SCRIPT, "end", cx);
    let pane = h.focused_pane_bounds();
    let cell = h.cell_size();
    let start = h.cell(5, 0, false);
    let above = point(start.x, pane.top() - cell.height * 0.5);

    h.cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::none());
    h.cx.simulate_mouse_move(above, Some(MouseButton::Left), Modifiers::none());
    h.cx.run_until_parked();
    h.advance_ticks(2);

    h.cx.simulate_mouse_up(above, MouseButton::Left, Modifiers::none());
    h.cx.run_until_parked();
    assert!(!h.is_drag_scrolling());

    let stopped = h.visible_lines();
    h.advance_ticks(5);
    assert_eq!(h.visible_lines(), stopped);
}

#[gpui::test]
fn closing_the_pane_while_auto_scrolling_stops_the_timer(cx: &mut TestAppContext) {
    let mut h = Harness::open(SCROLLBACK_SCRIPT, "end", cx);
    // Split so closing the dragged pane leaves the window open.
    h.cx.simulate_keystrokes("cmd-d");
    h.cx.run_until_parked();
    h.cx.simulate_keystrokes("cmd-alt-left");
    h.cx.run_until_parked();

    let terminal = h.terminal();
    let weak = terminal.downgrade();
    let pane = h.focused_pane_bounds();
    let cell = h.cell_size();
    let start = h.cell(5, 0, false);
    let above = point(start.x, pane.top() - cell.height * 0.5);

    h.cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::none());
    h.cx.simulate_mouse_move(above, Some(MouseButton::Left), Modifiers::none());
    h.cx.run_until_parked();
    h.advance_ticks(1);
    assert!(terminal.read_with(&h.cx, |view, _| view.is_drag_scrolling()));
    drop(terminal);

    h.cx.simulate_keystrokes("cmd-w");
    h.cx.run_until_parked();
    h.advance_ticks(5);
    assert!(
        weak.upgrade().is_none(),
        "closed pane should be released with no remaining auto-scroll timer"
    );
}

#[gpui::test]
fn hiding_the_pane_stops_auto_scrolling(cx: &mut TestAppContext) {
    let mut h = Harness::open(SCROLLBACK_SCRIPT, "end", cx);
    let terminal = h.terminal();
    let pane = h.focused_pane_bounds();
    let cell = h.cell_size();
    let start = h.cell(5, 0, false);
    let above = point(start.x, pane.top() - cell.height * 0.5);

    h.cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::none());
    h.cx.simulate_mouse_move(above, Some(MouseButton::Left), Modifiers::none());
    h.cx.run_until_parked();
    h.advance_ticks(1);
    assert!(terminal.read_with(&h.cx, |view, _| view.is_drag_scrolling()));

    // Switching to a new tab hides the original pane without closing it.
    h.cx.simulate_keystrokes("cmd-t");
    h.cx.run_until_parked();
    assert!(!terminal.read_with(&h.cx, |view, _| view.is_drag_scrolling()));

    let hidden_visible = terminal.read_with(&h.cx, |view, _| view.visible_lines());
    h.advance_ticks(5);
    assert_eq!(
        terminal.read_with(&h.cx, |view, _| view.visible_lines()),
        hidden_visible
    );
}

#[gpui::test]
fn dragging_that_starts_outside_the_pane_does_not_auto_scroll(cx: &mut TestAppContext) {
    let mut h = Harness::open(SCROLLBACK_SCRIPT, "end", cx);
    h.cx.simulate_keystrokes("cmd-d");
    h.cx.run_until_parked();

    let grid = h.grid();
    let (left_terminal, left_bounds, right_bounds) = grid.read_with(&h.cx, |grid, _| {
        let panes = grid.pane_bounds();
        assert_eq!(panes.len(), 2);
        let (left_id, left_bounds) = panes[0];
        let (_, right_bounds) = panes[1];
        (
            grid.terminal(left_id).cloned().unwrap(),
            left_bounds,
            right_bounds,
        )
    });
    let cell = left_terminal.read_with(&h.cx, |view, _| view.cell_size());
    let before = left_terminal.read_with(&h.cx, |view, _| view.visible_lines());

    // Start the drag in the right pane, then drag above the left pane.
    let outside_start = right_bounds.center();
    let above_left = point(
        left_bounds.center().x,
        left_bounds.top() - cell.height * 2.0,
    );
    h.cx.simulate_mouse_down(outside_start, MouseButton::Left, Modifiers::none());
    h.cx.simulate_mouse_move(above_left, Some(MouseButton::Left), Modifiers::none());
    h.cx.run_until_parked();

    h.advance_ticks(5);
    assert!(!left_terminal.read_with(&h.cx, |view, _| view.is_drag_scrolling()));
    assert_eq!(
        left_terminal.read_with(&h.cx, |view, _| view.visible_lines()),
        before
    );
}

//! In-pane terminal search (Cmd-F): matching across scrollback and screen,
//! navigating between matches, scrolling to matches, and isolating input.

use std::time::{Duration, Instant};

use benterm_core::{Cell, CellWidth, Row, RowEnd};
use benterm_layout::PaneId;
use benterm_terminal::PtyConfig;
use benterm_ui::settings::ConfigFeed;
use benterm_ui::terminal_search::{SearchMatch, find_matches};
use benterm_ui::viewport::Scroll;
use benterm_ui::{BentermWindow, PaneGrid, TerminalView, bind_keys, open_terminal_window};
use gpui::{Entity, TestAppContext, VisualTestContext, WindowHandle};

/// Builds a single-line `Row` from ASCII/Unicode text, padding with spaces to `cols`.
fn make_row(text: &str, cols: usize, end: RowEnd) -> Row {
    let mut cells = Vec::with_capacity(cols);
    for ch in text.chars() {
        let width = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(1);
        if width == 2 {
            cells.push(Cell {
                ch,
                width: CellWidth::Wide,
                ..Cell::default()
            });
            cells.push(Cell {
                ch: ' ',
                width: CellWidth::WideSpacer,
                ..Cell::default()
            });
        } else {
            cells.push(Cell {
                ch,
                width: CellWidth::Narrow,
                ..Cell::default()
            });
        }
    }
    while cells.len() < cols {
        cells.push(Cell::default());
    }
    Row { cells, end }
}

#[test]
fn finds_case_insensitive_matches_across_rows() {
    let rows = vec![
        make_row("first foo here", 20, RowEnd::LineEnd),
        make_row("nothing to see", 20, RowEnd::LineEnd),
        make_row("FOO and Foo", 20, RowEnd::LineEnd),
    ];
    let matches = find_matches(&rows, 0, "foo", false);
    assert_eq!(
        matches,
        vec![
            SearchMatch {
                start_line: 0,
                spans: vec![(0, 6..9)],
            },
            SearchMatch {
                start_line: 2,
                spans: vec![(2, 0..3)],
            },
            SearchMatch {
                start_line: 2,
                spans: vec![(2, 8..11)],
            },
        ]
    );
}

#[test]
fn case_sensitive_search_filters_by_case() {
    let rows = vec![make_row("foo FOO Foo", 20, RowEnd::LineEnd)];
    let matches = find_matches(&rows, 0, "FOO", true);
    assert_eq!(
        matches,
        vec![SearchMatch {
            start_line: 0,
            spans: vec![(0, 4..7)],
        }]
    );
}

#[test]
fn empty_query_returns_no_matches() {
    let rows = vec![make_row("foo bar", 20, RowEnd::LineEnd)];
    assert!(find_matches(&rows, 0, "", false).is_empty());
}

#[test]
fn finds_matches_with_wide_characters() {
    // "あ" takes cols 0..2, "い" takes 2..4, "foo" takes 4..7.
    let rows = vec![make_row("あいfoo", 20, RowEnd::LineEnd)];
    let matches = find_matches(&rows, 5, "foo", false);
    assert_eq!(
        matches,
        vec![SearchMatch {
            start_line: 5,
            spans: vec![(5, 4..7)],
        }]
    );
    let wide_matches = find_matches(&rows, 5, "あい", false);
    assert_eq!(
        wide_matches,
        vec![SearchMatch {
            start_line: 5,
            spans: vec![(5, 0..4)],
        }]
    );
}

#[test]
fn finds_matches_across_wrapped_lines() {
    let rows = vec![
        make_row("abcdef", 6, RowEnd::Wrapped),
        make_row("ghij", 6, RowEnd::LineEnd),
    ];
    let matches = find_matches(&rows, 10, "efgh", false);
    assert_eq!(
        matches,
        vec![SearchMatch {
            start_line: 10,
            spans: vec![(10, 4..6), (11, 0..2)],
        }]
    );
}

#[test]
fn scroll_to_line_moves_viewport_to_target_line() {
    let line_height = 20.0;
    let rows = 10u16;
    let scrollback_len = 100usize;
    let mut scroll = Scroll::default();
    assert_eq!(
        scroll.visible(line_height, rows, scrollback_len).first_line,
        100
    );

    scroll.scroll_to_line(10, line_height, rows, scrollback_len);
    let vis = scroll.visible(line_height, rows, scrollback_len);
    assert!(
        (vis.first_line..vis.first_line + vis.line_count).contains(&10),
        "line 10 should be visible, got {vis:?}"
    );
}

const READY: &str = "ready ";

fn hex_dump(setup: &str) -> PtyConfig {
    let script = format!(
        "stty raw -echo; printf '{setup}{READY}'; \
         exec perl -e '$| = 1; while (sysread(STDIN, $b, 1)) {{ printf \"%02x \", ord $b }}'"
    );
    PtyConfig::command("/bin/sh", ["-c", script.as_str()])
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x} ")).collect()
}

fn received(view: &TerminalView) -> Option<String> {
    let text: String = view.screen().rows.iter().map(|row| row.text()).collect();
    let (_, dump) = text.split_once(READY)?;
    Some(dump.trim_end().to_owned())
}

struct Harness {
    window: WindowHandle<BentermWindow>,
    cx: VisualTestContext,
}

impl Harness {
    fn open(pty: PtyConfig, cx: &mut TestAppContext) -> Self {
        cx.executor().allow_parking();
        cx.update(bind_keys);
        let window = cx
            .update(|cx| open_terminal_window(pty, ConfigFeed::defaults(), cx))
            .unwrap();
        let mut harness = Self {
            window,
            cx: VisualTestContext::from_window(window.into(), cx),
        };
        harness.cx.update(|window, _| window.activate_window());
        harness.cx.run_until_parked();
        harness
    }

    fn grid(&mut self) -> Entity<PaneGrid> {
        self.window
            .update(&mut self.cx, |root, _, cx| root.grid(cx))
            .expect("the window is open")
    }

    fn focused(&mut self) -> PaneId {
        self.grid().read_with(&self.cx, |grid, _| grid.focused())
    }

    fn panes(&mut self) -> Vec<PaneId> {
        self.grid().read_with(&self.cx, |grid, _| grid.panes())
    }

    fn terminal(&mut self, pane: PaneId) -> Entity<TerminalView> {
        self.grid()
            .read_with(&self.cx, |grid, _| grid.terminal(pane).cloned())
            .expect("the pane has a terminal")
    }

    fn keys(&mut self, keystrokes: &str) {
        self.cx.simulate_keystrokes(keystrokes);
        self.cx.run_until_parked();
    }

    fn wait_until(&mut self, what: &str, mut done: impl FnMut(&mut Self) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            self.cx.run_until_parked();
            if done(self) {
                return;
            }
            assert!(Instant::now() < deadline, "timed out waiting until {what}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn expect_received(&mut self, pane: PaneId, bytes: &[u8]) {
        let wanted = hex(bytes).trim_end().to_owned();
        let terminal = self.terminal(pane);
        self.wait_until(&format!("program received {wanted:?}"), |h| {
            let got = terminal.read_with(&h.cx, |view, _| received(view));
            got.as_deref() == Some(wanted.as_str())
        });
    }
}

#[gpui::test]
fn cmd_f_searches_scrollback_and_enter_jumps_to_third_match(cx: &mut TestAppContext) {
    // Emit 120 lines with "foo" on lines 1, 50, and 90 so all three are in
    // scrollback well above the bottom of the screen.
    let script = "\
        i=1; \
        while [ $i -le 120 ]; do \
            if [ $i -eq 1 ] || [ $i -eq 50 ] || [ $i -eq 90 ]; then \
                printf 'line %03d foo\\r\\n' $i; \
            else \
                printf 'line %03d bar\\r\\n' $i; \
            fi; \
            i=$((i + 1)); \
        done; \
        printf 'DONE_OUTPUT\\r\\n'; \
        sleep 100";
    let pty = PtyConfig::command("/bin/sh", ["-c", script]);
    let mut h = Harness::open(pty, cx);
    let pane = h.focused();
    let terminal = h.terminal(pane);

    h.wait_until("all 120 lines are printed", |h| {
        terminal.read_with(&h.cx, |view, _| view.text(None).contains("DONE_OUTPUT"))
    });

    h.keys("cmd-f");
    assert!(terminal.read_with(&h.cx, |view, _| view.is_search_open()));
    h.keys("f o o");

    let (count, current, status, matches) = terminal.read_with(&h.cx, |view, _| {
        (
            view.search_match_count(),
            view.search_current_index(),
            view.search_status_label(),
            view.search_matches().to_vec(),
        )
    });
    assert_eq!(count, 3, "should find 3 matches of 'foo'");
    assert_eq!(current, Some(0));
    assert_eq!(status, "1/3");

    // Press Enter twice to move to the 3rd match and verify viewport scrolled there.
    h.keys("enter enter");
    let (current, status, visible) = terminal.read_with(&h.cx, |view, _| {
        (
            view.search_current_index(),
            view.search_status_label(),
            view.visible_lines(),
        )
    });
    assert_eq!(current, Some(2));
    assert_eq!(status, "3/3");
    let third_line = matches[2].start_line;
    assert!(
        visible.contains(&third_line),
        "viewport {visible:?} should contain 3rd match at line {third_line}"
    );

    // Shift-Enter moves back to the 2nd match.
    h.keys("shift-enter");
    assert_eq!(
        terminal.read_with(&h.cx, |view, _| view.search_current_index()),
        Some(1)
    );

    // Cmd-G moves forward to the 3rd match, Cmd-Shift-G moves back to the 2nd
    // (without toggling Diff view).
    h.keys("cmd-g");
    assert_eq!(
        terminal.read_with(&h.cx, |view, _| view.search_current_index()),
        Some(2)
    );
    h.keys("cmd-shift-g");
    assert_eq!(
        terminal.read_with(&h.cx, |view, _| view.search_current_index()),
        Some(1)
    );
    assert!(terminal.read_with(&h.cx, |view, _| view.is_search_open()));
}

#[gpui::test]
fn typing_in_search_does_not_write_to_terminal_and_esc_returns_keyboard(cx: &mut TestAppContext) {
    let mut h = Harness::open(hex_dump(""), cx);
    let pane = h.focused();
    h.expect_received(pane, b"");
    let terminal = h.terminal(pane);

    // Open search and type text + navigation keys: nothing should reach the PTY.
    h.keys("cmd-f");
    assert!(terminal.read_with(&h.cx, |view, _| view.is_search_open()));
    h.keys("h e l l o enter shift-enter backspace");
    assert_eq!(
        terminal.read_with(&h.cx, |view, cx| view.search_query(cx)),
        Some("hell".to_owned())
    );

    // Close search with Escape: highlights clear and keyboard returns to terminal.
    h.keys("escape");
    assert!(!terminal.read_with(&h.cx, |view, _| view.is_search_open()));
    assert_eq!(
        terminal.read_with(&h.cx, |view, _| view.search_match_count()),
        0
    );

    // Now typing 'a' reaches the terminal, and nothing from search was sent before it.
    h.keys("a");
    h.expect_received(pane, b"a");
}

#[gpui::test]
fn no_matches_shows_zero_of_zero(cx: &mut TestAppContext) {
    let pty = PtyConfig::command("/bin/sh", ["-c", "printf 'hello world\\r\\n'; sleep 100"]);
    let mut h = Harness::open(pty, cx);
    let pane = h.focused();
    let terminal = h.terminal(pane);
    h.wait_until("hello world printed", |h| {
        terminal.read_with(&h.cx, |view, _| view.text(None).contains("hello world"))
    });

    h.keys("cmd-f");
    h.keys("z z z");
    assert_eq!(
        terminal.read_with(&h.cx, |view, _| view.search_match_count()),
        0
    );
    assert_eq!(
        terminal.read_with(&h.cx, |view, _| view.search_status_label()),
        "0/0"
    );
}

#[gpui::test]
fn cmd_f_only_opens_on_focused_pane(cx: &mut TestAppContext) {
    let pty = PtyConfig::command("/bin/sh", ["-c", "printf 'hello\\r\\n'; sleep 100"]);
    let mut h = Harness::open(pty, cx);
    let first = h.focused();
    h.keys("cmd-d");
    let panes = h.panes();
    assert_eq!(panes.len(), 2);
    let second = h.focused();
    assert_ne!(first, second);

    h.keys("cmd-f");
    let first_view = h.terminal(first);
    let second_view = h.terminal(second);
    assert!(!first_view.read_with(&h.cx, |view, _| view.is_search_open()));
    assert!(second_view.read_with(&h.cx, |view, _| view.is_search_open()));
}

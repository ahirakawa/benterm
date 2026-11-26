//! Keys, the input method and pastes, from the window to the program.
//!
//! Every pane runs a program that prints each byte it reads in hex, so
//! the screen shows exactly what the PTY was sent.

use std::time::{Duration, Instant};

use benterm_config::{Config, Loaded};
use benterm_layout::PaneId;
use benterm_terminal::PtyConfig;
use benterm_ui::settings::ConfigFeed;
use benterm_ui::theme::Theme;
use benterm_ui::{BentermWindow, PaneGrid, TerminalView, bind_keys, open_terminal_window};
use gpui::{
    Bounds, ClipboardItem, Entity, EntityInputHandler, Image, ImageFormat, TestAppContext,
    VisualTestContext, WindowHandle, point, px,
};

/// Shown once the terminal is in raw mode and the modes are set.
const READY: &str = "ready ";

/// A program that sends `setup` to the terminal and then prints every
/// byte it reads as two hex digits and a space.
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

/// What the program of `view` has read so far, in hex, or `None` before
/// it is ready.
fn received(view: &TerminalView) -> Option<String> {
    // The dump wraps at the right edge, so the rows join without a break.
    let text: String = view.screen().rows.iter().map(|row| row.text()).collect();
    let (_, dump) = text.split_once(READY)?;
    Some(dump.trim_end().to_owned())
}

struct Harness {
    window: WindowHandle<BentermWindow>,
    cx: VisualTestContext,
}

impl Harness {
    /// Opens a window whose panes all run [`hex_dump`], with the app's
    /// key bindings, and waits until the first pane is ready.
    fn open(setup: &str, cx: &mut TestAppContext) -> Self {
        Self::open_with(setup, ConfigFeed::defaults(), cx)
    }

    /// Like [`open`](Self::open), with the settings of `feed`.
    fn open_with(setup: &str, feed: ConfigFeed, cx: &mut TestAppContext) -> Self {
        // The terminals' worker threads wake the app from outside the
        // test scheduler, which the scheduler has to be told to accept.
        cx.executor().allow_parking();
        cx.update(bind_keys);
        let window = cx
            .update(|cx| open_terminal_window(hex_dump(setup), feed, cx))
            .unwrap();
        let mut harness = Self {
            window,
            cx: VisualTestContext::from_window(window.into(), cx),
        };
        // Focus changes are only reported in the active window.
        harness.cx.update(|window, _| window.activate_window());
        let first = harness.focused();
        harness.expect(first, b"");
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
    }

    /// Waits until the program of `pane` has read exactly `bytes`. The
    /// program runs in real time on another thread, hence the polling.
    fn expect(&mut self, pane: PaneId, bytes: &[u8]) {
        let wanted = hex(bytes).trim_end().to_owned();
        let terminal = self.terminal(pane);
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            self.cx.run_until_parked();
            let got = terminal.read_with(&self.cx, |view, _| received(view));
            if got.as_deref() == Some(wanted.as_str()) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "the program read {got:?}, expected {wanted:?}"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// The input method shows `text` as being composed in `pane`.
    fn compose(&mut self, pane: PaneId, text: &str) {
        self.terminal(pane)
            .update_in(&mut self.cx, |view, window, cx| {
                view.replace_and_mark_text_in_range(None, text, None, window, cx)
            });
    }

    /// The input method commits `text` in `pane`.
    fn commit(&mut self, pane: PaneId, text: &str) {
        self.terminal(pane)
            .update_in(&mut self.cx, |view, window, cx| {
                view.replace_text_in_range(None, text, window, cx)
            });
    }

    fn marked(&mut self, pane: PaneId) -> Option<String> {
        self.terminal(pane)
            .read_with(&self.cx, |view, _| view.marked_text().map(str::to_owned))
    }
}

#[gpui::test]
fn text_and_keys_reach_the_program_once_each(cx: &mut TestAppContext) {
    let mut h = Harness::open("", cx);
    let pane = h.focused();
    h.keys("a shift-b space enter tab backspace escape up ctrl-c alt-left f5 shift-tab");
    h.expect(pane, b"aB \r\t\x7f\x1b\x1b[A\x03\x1bb\x1b[15~\x1b[Z");
}

#[gpui::test]
fn cursor_keys_follow_the_mode_the_program_set(cx: &mut TestAppContext) {
    let mut h = Harness::open(r"\033[?1h", cx);
    let pane = h.focused();
    h.keys("up home shift-up");
    h.expect(pane, b"\x1bOA\x1bOH\x1b[1;2A");
}

#[gpui::test]
fn composing_sends_nothing_and_committing_sends_the_text_once(cx: &mut TestAppContext) {
    let mut h = Harness::open("", cx);
    let pane = h.focused();

    for marked in ["に", "にほ", "にほんご", "日本語"] {
        h.compose(pane, marked);
        assert_eq!(h.marked(pane).as_deref(), Some(marked));
    }
    h.commit(pane, "日本語");
    assert_eq!(h.marked(pane), None);
    // Enter comes after everything the composition could have sent, so
    // the program read the committed text once and nothing else.
    h.keys("enter");
    h.expect(pane, "日本語\r".as_bytes());

    // A second composition starts from nothing.
    h.compose(pane, "かな");
    h.commit(pane, "仮名");
    h.keys("enter");
    h.expect(pane, "日本語\r仮名\r".as_bytes());
}

#[gpui::test]
fn a_composition_that_is_given_up_sends_nothing(cx: &mut TestAppContext) {
    let mut h = Harness::open("", cx);
    let pane = h.focused();

    h.compose(pane, "にほ");
    h.terminal(pane)
        .update_in(&mut h.cx, |view, window, cx| view.unmark_text(window, cx));
    assert_eq!(h.marked(pane), None);

    h.compose(pane, "にほ");
    h.compose(pane, "");
    assert_eq!(h.marked(pane), None);

    h.keys("enter");
    h.expect(pane, b"\r");
}

#[gpui::test]
fn the_input_method_is_told_what_is_marked(cx: &mut TestAppContext) {
    let mut h = Harness::open("", cx);
    let pane = h.focused();
    let terminal = h.terminal(pane);

    let marked_range = |h: &mut Harness| {
        terminal.update_in(&mut h.cx, |view, window, cx| {
            view.marked_text_range(window, cx)
        })
    };
    assert_eq!(marked_range(&mut h), None);
    h.compose(pane, "にほんご");
    assert_eq!(marked_range(&mut h), Some(0..4));
    let (selection, text) = terminal.update_in(&mut h.cx, |view, window, cx| {
        let selection = view.selected_text_range(false, window, cx).unwrap();
        let text = view.text_for_range(1..3, &mut None, window, cx);
        (selection.range, text)
    });
    assert_eq!(selection, 4..4);
    assert_eq!(text.as_deref(), Some("ほん"));
}

#[gpui::test]
fn the_candidate_window_is_placed_at_the_cursor(cx: &mut TestAppContext) {
    let mut h = Harness::open("", cx);
    let pane = h.focused();
    let terminal = h.terminal(pane);
    // The cursor is after "ready a ", which the program printed.
    h.keys("a");
    h.expect(pane, b"a");
    let cursor_col = (READY.len() + "61 ".len()) as f32;

    let pane_bounds = h.grid().read_with(&h.cx, |grid, _| grid.pane_bounds()[0].1);
    let cell = terminal.read_with(&h.cx, |view, _| view.cell_size());
    let padding = px(Theme::default().terminal.padding);
    let cursor = Bounds::new(
        pane_bounds.origin + point(padding + cell.width * cursor_col, padding),
        cell,
    );

    let bounds_for = |h: &mut Harness, offset: usize| {
        terminal.update_in(&mut h.cx, |view, window, cx| {
            view.bounds_for_range(offset..offset, pane_bounds, window, cx)
        })
    };
    assert_eq!(bounds_for(&mut h, 0), Some(cursor));

    // Within a composition, the place moves right by the cells of the
    // marked text before it: two for each kana.
    h.compose(pane, "にほん");
    h.cx.run_until_parked();
    assert_eq!(bounds_for(&mut h, 0), Some(cursor));
    let after_two = Bounds::new(cursor.origin + point(cell.width * 4., px(0.)), cell);
    assert_eq!(bounds_for(&mut h, 2), Some(after_two));
}

#[gpui::test]
fn cmd_v_pastes_the_clipboard_as_typed_text(cx: &mut TestAppContext) {
    let mut h = Harness::open("", cx);
    let pane = h.focused();
    h.cx.write_to_clipboard(ClipboardItem::new_string("echo 日本\nls".into()));
    h.keys("cmd-v");
    h.expect(pane, "echo 日本\rls".as_bytes());
}

#[gpui::test]
fn cmd_v_brackets_the_paste_when_the_program_asked_for_it(cx: &mut TestAppContext) {
    let mut h = Harness::open(r"\033[?2004h", cx);
    let pane = h.focused();
    h.cx.write_to_clipboard(ClipboardItem::new_string("a\nb".into()));
    h.keys("cmd-v");
    h.expect(pane, b"\x1b[200~a\nb\x1b[201~");
}

#[gpui::test]
fn cmd_v_with_an_image_sends_nothing_without_an_agent(cx: &mut TestAppContext) {
    let mut h = Harness::open("", cx);
    let pane = h.focused();
    let image = Image::from_bytes(ImageFormat::Png, Vec::new());
    h.cx.write_to_clipboard(ClipboardItem::new_image(&image));

    h.keys("cmd-v enter");
    h.expect(pane, b"\r");
}

#[gpui::test]
fn cmd_v_with_an_image_asks_claude_code_to_attach_it(cx: &mut TestAppContext) {
    cmd_v_with_an_image_asks_the_agent_to_attach_it("claude", cx);
}

#[gpui::test]
fn cmd_v_with_an_image_asks_codex_to_attach_it(cx: &mut TestAppContext) {
    cmd_v_with_an_image_asks_the_agent_to_attach_it("codex", cx);
}

fn cmd_v_with_an_image_asks_the_agent_to_attach_it(agent: &str, cx: &mut TestAppContext) {
    let mut h = Harness::open("", cx);
    let pane = h.focused();
    let image = Image::from_bytes(ImageFormat::Png, Vec::new());
    h.cx.write_to_clipboard(ClipboardItem::new_image(&image));
    h.terminal(pane).update(&mut h.cx, |terminal, cx| {
        terminal.report_hook(&benterm_agent::HookEvent::new(agent, "SessionStart"), cx);
    });

    h.keys("cmd-v");
    h.expect(pane, b"\x16");
}

#[gpui::test]
fn cmd_v_with_nothing_to_paste_sends_nothing(cx: &mut TestAppContext) {
    let mut h = Harness::open(r"\033[?2004h", cx);
    let pane = h.focused();
    h.keys("cmd-v enter");
    h.expect(pane, b"\r");
}

#[gpui::test]
fn keys_with_command_are_not_sent_to_the_program(cx: &mut TestAppContext) {
    let mut h = Harness::open("", cx);
    let pane = h.focused();
    h.keys("cmd-c cmd-a cmd-shift-c cmd-shift-left cmd-up cmd-enter cmd-ctrl-c cmd-alt-x");
    h.keys("enter");
    h.expect(pane, b"\r");
}

/// The keys of the Edit menu that a terminal has no use for send nothing,
/// and neither do their items when chosen from the menu bar.
#[gpui::test]
fn the_edit_keys_and_items_send_nothing_to_the_program(cx: &mut TestAppContext) {
    use benterm_ui::menu::{Cut, Redo, SelectAll, Undo};
    let mut h = Harness::open("", cx);
    let pane = h.focused();
    h.keys("cmd-a cmd-x cmd-z cmd-shift-z");
    let actions: [&dyn gpui::Action; 4] = [&Undo, &Redo, &Cut, &SelectAll];
    for action in actions {
        // The menu bar hands a chosen item's action to the window.
        h.cx.update(|window, cx| window.dispatch_action(action.boxed_clone(), cx));
        h.cx.run_until_parked();
    }
    h.keys("enter");
    h.expect(pane, b"\r");
}

#[gpui::test]
fn the_pane_keys_still_work_and_send_nothing(cx: &mut TestAppContext) {
    let mut h = Harness::open("", cx);
    let first = h.focused();

    h.keys("cmd-d");
    let [_, right] = h.panes()[..] else {
        panic!("cmd-d did not split the pane");
    };
    assert_eq!(h.focused(), right);
    h.keys("x");
    h.expect(right, b"x");

    h.keys("cmd-shift-d");
    let [_, _, below] = h.panes()[..] else {
        panic!("cmd-shift-d did not split the pane");
    };
    assert_eq!(h.focused(), below);
    h.keys("y");
    h.expect(below, b"y");

    h.keys("cmd-alt-up");
    assert_eq!(h.focused(), right);
    h.keys("cmd-alt-left");
    assert_eq!(h.focused(), first);
    h.keys("cmd-alt-right");
    assert_eq!(h.focused(), right);
    h.keys("cmd-alt-down");
    assert_eq!(h.focused(), below);
    h.keys("cmd-alt-left z");
    assert_eq!(h.focused(), first);

    // Each program read only what was typed while its pane had the focus.
    h.expect(first, b"z");
    h.expect(right, b"x");
    h.expect(below, b"y");
}

#[gpui::test]
fn the_pane_keys_work_while_composing_and_the_composition_stays_behind(cx: &mut TestAppContext) {
    let mut h = Harness::open("", cx);
    let first = h.focused();

    h.compose(first, "にほ");
    h.keys("cmd-d");
    let second = h.focused();
    assert_ne!(first, second);
    // The pane that lost the focus drops what was being composed, once
    // the next frame has told it so.
    h.cx.run_until_parked();
    assert_eq!(h.marked(first), None);

    // What the input method commits next goes to the focused pane.
    h.commit(second, "日本");
    h.keys("cmd-alt-left enter");
    h.expect(second, "日本".as_bytes());
    h.expect(first, b"\r");
}

#[gpui::test]
fn closing_a_pane_does_not_wait_for_a_child_that_ignores_hangup(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    cx.update(bind_keys);
    let window = cx
        .update(|cx| {
            let pty = PtyConfig::command(
                "/bin/sh",
                [
                    "-c",
                    "trap '' HUP; printf 'ready %s\n' \"$$\"; exec sleep 100",
                ],
            );
            open_terminal_window(pty, ConfigFeed::defaults(), cx)
        })
        .unwrap();
    let mut h = Harness {
        window,
        cx: VisualTestContext::from_window(window.into(), cx),
    };
    h.cx.update(|window, _| window.activate_window());
    let first = h.focused();
    h.keys("cmd-d");
    let second = h.focused();
    let view = h.terminal(second);
    let deadline = Instant::now() + Duration::from_secs(10);
    let pid = loop {
        h.cx.run_until_parked();
        if let Some(pid) = view
            .read_with(&h.cx, |view, _| received(view))
            .and_then(|text| text.parse::<u32>().ok())
        {
            break pid.to_string();
        }
        assert!(
            Instant::now() < deadline,
            "the child did not report its pid"
        );
        std::thread::sleep(Duration::from_millis(5));
    };
    drop(view);

    let start = Instant::now();
    h.keys("cmd-w");
    h.cx.run_until_parked();
    assert_eq!(h.panes(), [first]);
    assert!(start.elapsed() < Duration::from_secs(1));
    let deadline = Instant::now() + Duration::from_secs(5);
    while std::process::Command::new("/bin/kill")
        .args(["-0", &pid])
        .output()
        .unwrap()
        .status
        .success()
    {
        assert!(Instant::now() < deadline, "the closed child was not reaped");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[gpui::test]
fn an_ime_commit_after_opening_the_palette_reaches_only_the_palette(cx: &mut TestAppContext) {
    let mut h = Harness::open("", cx);
    let pane = h.focused();

    h.compose(pane, "にほん");
    h.keys("cmd-k");
    h.cx.run_until_parked();
    assert_eq!(h.marked(pane), None);
    h.cx.simulate_input("日本");

    h.window
        .read_with(&h.cx, |root, cx| {
            let palette = root.command_palette().unwrap().read(cx);
            assert_eq!(palette.editor().read(cx).text(), "日本");
        })
        .unwrap();
    h.keys("escape enter");
    h.expect(pane, b"\r");
}

#[gpui::test]
fn opening_and_closing_a_peek_discards_the_previous_terminal_composition(cx: &mut TestAppContext) {
    let mut h = Harness::open("", cx);
    let first = h.focused();
    h.keys("cmd-d");
    let second = h.focused();
    h.keys("cmd-alt-left");
    h.compose(first, "にほん");

    assert!(
        h.window
            .update(&mut h.cx, |root, window, cx| root.peek(second, window, cx))
            .unwrap()
    );
    h.cx.run_until_parked();
    h.window
        .update(&mut h.cx, |root, window, cx| root.close_peek(window, cx))
        .unwrap();
    h.cx.run_until_parked();
    assert_eq!(h.marked(first), None);
    h.keys("enter");
    h.expect(first, b"\r");
    h.expect(second, b"");
}

#[gpui::test]
fn shift_enter_is_a_return_for_a_program_that_did_not_ask_for_more(cx: &mut TestAppContext) {
    let mut h = Harness::open("", cx);
    let pane = h.focused();
    h.keys("shift-enter");
    h.expect(pane, b"\r");
}

#[gpui::test]
fn shift_enter_reaches_a_program_that_turned_on_the_kitty_protocol(cx: &mut TestAppContext) {
    // What Claude Code and Codex push when they start.
    let mut h = Harness::open("\\033[>5u", cx);
    let pane = h.focused();
    h.keys("shift-enter enter escape");
    h.expect(pane, b"\x1b[13;2u\r\x1b[27u");
}

#[gpui::test]
fn command_left_right_and_backspace_edit_the_line(cx: &mut TestAppContext) {
    let mut h = Harness::open("", cx);
    let pane = h.focused();
    h.keys("cmd-left cmd-right cmd-backspace");
    h.expect(pane, b"\x01\x05\x15");
}

#[gpui::test]
fn option_as_meta_sends_the_key_with_an_escape_in_front(cx: &mut TestAppContext) {
    let mut config = Config::default();
    config.terminal.option_as_meta = true;
    let feed = ConfigFeed::new(Loaded {
        config,
        warnings: Vec::new(),
    });
    let mut h = Harness::open_with("", feed, cx);
    let pane = h.focused();
    h.keys("alt-b alt-f");
    h.expect(pane, b"\x1bb\x1bf");
}

#[gpui::test]
fn shift_page_up_and_down_scroll_the_scrollback_without_sending(cx: &mut TestAppContext) {
    // Enough lines to fill the screen and leave some above it.
    let mut h = Harness::open(&"\\n".repeat(200), cx);
    let pane = h.focused();
    let terminal = h.terminal(pane);
    let scrolled_back =
        |h: &mut Harness| terminal.read_with(&h.cx, |view, _| view.is_scrolled_back());
    assert!(!scrolled_back(&mut h));
    h.keys("shift-pageup");
    assert!(scrolled_back(&mut h));
    h.keys("shift-pagedown");
    assert!(!scrolled_back(&mut h));
    h.keys("shift-pageup enter");
    h.expect(pane, b"\r");
    // Typing brings the view back to the cursor.
    assert!(!scrolled_back(&mut h));
}

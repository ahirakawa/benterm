//! What the socket API is for: a shell in one pane of the app runs
//! `benterm` to split its pane, type into the new pane and read what
//! the program there printed.
//!
//! The window is a real one without a display, the panes run real shells
//! on PTYs, and the commands are the real `benterm` binary, found on
//! the `PATH` and typed at the prompt as a user would.

use std::ffi::OsString;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::rc::Rc;
use std::time::{Duration, Instant};

use benterm_terminal::PtyConfig;
use benterm_ui::settings::ConfigFeed;
use benterm_ui::{BentermWindow, StartedTerminal, bind_keys, ipc};
use gpui::{TestAppContext, VisualTestContext, WindowHandle};

const TIMEOUT: Duration = Duration::from_secs(30);

/// An interactive shell with a plain prompt, no startup files, and the
/// built `benterm` first on its `PATH`.
fn shell() -> PtyConfig {
    let bin = Path::new(env!("CARGO_BIN_EXE_benterm"))
        .parent()
        .expect("the binary is in a directory");
    let mut path = OsString::from(bin);
    path.push(":/usr/bin:/bin");
    let mut pty = PtyConfig::command("/bin/sh", ["-i"]);
    pty.env = vec![
        ("PATH".into(), path),
        ("PS1".into(), "$ ".into()),
        ("ENV".into(), "".into()),
    ];
    pty
}

/// The text of every pane of the selected tab, by pane number.
fn panes(window: WindowHandle<BentermWindow>, cx: &mut VisualTestContext) -> Vec<(u64, String)> {
    window
        .update(cx, |root, _, cx| {
            let grid = root.grid(cx);
            let grid = grid.read(cx);
            grid.panes()
                .into_iter()
                .map(|pane| {
                    let view = grid.terminal(pane).expect("a pane has a terminal");
                    (pane.get(), view.read(cx).text(None))
                })
                .collect()
        })
        .expect("the window is open")
}

fn text_of(window: WindowHandle<BentermWindow>, cx: &mut VisualTestContext, pane: u64) -> String {
    panes(window, cx)
        .into_iter()
        .find(|(id, _)| *id == pane)
        .map(|(_, text)| text)
        .unwrap_or_else(|| panic!("the window has no pane {pane}"))
}

/// Lets the app run until `done` holds for the text of a pane. The shells
/// and the commands run in real time, hence the polling.
fn wait_for(
    window: WindowHandle<BentermWindow>,
    cx: &mut VisualTestContext,
    pane: u64,
    what: &str,
    done: impl Fn(&str) -> bool,
) -> String {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        cx.run_until_parked();
        let text = text_of(window, cx, pane);
        if done(&text) {
            return text;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {what}; the panes show {:#?}",
            panes(window, cx)
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Types a line at the prompt of the pane that has the keyboard.
fn type_line(cx: &mut VisualTestContext, line: &str) {
    cx.simulate_input(line);
    cx.simulate_keystrokes("enter");
}

#[gpui::test]
fn a_shell_splits_its_pane_types_into_the_new_one_and_reads_it(cx: &mut TestAppContext) {
    // The terminals' and the socket's threads wake the app from outside
    // the test scheduler.
    cx.executor().allow_parking();
    cx.update(bind_keys);
    let dir = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap();
    let socket = dir.path().join("benterm.sock");
    let start: ipc::StartTerminal = Rc::new(StartedTerminal::spawn);
    let window = cx
        .update(|cx| ipc::open_window(shell(), start, Some(socket), ConfigFeed::defaults(), cx))
        .unwrap();
    let cx = &mut VisualTestContext::from_window(window.into(), cx);
    cx.run_until_parked();

    let first = panes(window, cx)[0].0;
    wait_for(window, cx, first, "the prompt", |text| text.ends_with('$'));

    // The shell splits the pane it runs in: `split` without `--pane`
    // takes the pane from BENTERM_PANE_ID.
    type_line(cx, r#"B=$(benterm split --down); echo "new=$B.""#);
    let text = wait_for(window, cx, first, "the new pane's id", |text| {
        text.contains("\nnew=")
    });
    let line = text.lines().find(|line| line.starts_with("new=")).unwrap();
    let second: u64 = line
        .trim_start_matches("new=")
        .trim_end_matches('.')
        .parse()
        .unwrap_or_else(|_| panic!("split printed no pane id: {text:?}"));
    assert_ne!(second, first);
    let ids: Vec<u64> = panes(window, cx).into_iter().map(|(id, _)| id).collect();
    assert_eq!(ids, [first, second]);
    wait_for(window, cx, second, "the prompt of the new pane", |text| {
        text.ends_with('$')
    });

    // It types a command into the other pane. The command prints a sum,
    // so what comes back differs from what was typed.
    type_line(
        cx,
        r#"benterm send --pane $B --enter 'echo "sum=$((40+2))"'; echo "sent=$?.""#,
    );
    wait_for(window, cx, first, "send to finish", |text| {
        text.contains("\nsent=0.")
    });
    wait_for(
        window,
        cx,
        second,
        "the other shell to run the command",
        |text| text.contains("\nsum=42"),
    );

    // And it reads what the other pane shows.
    type_line(
        cx,
        r#"OUT=$(benterm read --pane $B); echo "read=$?."; echo "got: $(echo "$OUT" | grep '^sum=')""#,
    );
    wait_for(window, cx, first, "read to finish", |text| {
        text.contains("\nread=0.")
    });
    let text = wait_for(
        window,
        cx,
        first,
        "the text read from the other pane",
        |text| text.contains("\ngot: "),
    );
    assert!(text.contains("\ngot: sum=42"), "{text:?}");
}

#[gpui::test]
fn an_agents_hook_run_in_a_pane_tells_the_app_the_state_of_that_pane(cx: &mut TestAppContext) {
    use benterm_agent::AgentState;

    cx.executor().allow_parking();
    cx.update(bind_keys);
    let dir = tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap();
    let socket = dir.path().join("benterm.sock");
    let start: ipc::StartTerminal = Rc::new(StartedTerminal::spawn);
    let window = cx
        .update(|cx| ipc::open_window(shell(), start, Some(socket), ConfigFeed::defaults(), cx))
        .unwrap();
    let cx = &mut VisualTestContext::from_window(window.into(), cx);
    cx.run_until_parked();
    let pane = panes(window, cx)[0].0;
    wait_for(window, cx, pane, "the prompt", |text| text.ends_with('$'));
    let status = |cx: &mut VisualTestContext| {
        window
            .update(cx, |root, _, cx| root.terminal(cx).read(cx).agent_status())
            .unwrap()
    };
    assert_eq!(status(cx).agent, None);

    // What an agent does with the hook settings of the README: it runs
    // the command with the event's JSON on standard input. The command
    // finds the app and the pane in its environment.
    type_line(
        cx,
        r#"printf '{"session_id":"s","hook_event_name":"UserPromptSubmit"}' | benterm hook --agent claude; echo "hooked=$?.""#,
    );
    wait_for(window, cx, pane, "the hook to finish", |text| {
        text.contains("\nhooked=0.")
    });
    assert_eq!(status(cx).agent, Some("claude"));
    assert_eq!(status(cx).state, AgentState::Working);

    type_line(
        cx,
        r#"printf '{"session_id":"s"}' | benterm hook Stop --agent claude; echo "stopped=$?.""#,
    );
    wait_for(window, cx, pane, "the second hook to finish", |text| {
        text.contains("\nstopped=0.")
    });
    assert_eq!(status(cx).state, AgentState::Done);
}

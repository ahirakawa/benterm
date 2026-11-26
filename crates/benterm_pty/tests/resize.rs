mod common;

use std::sync::mpsc::Receiver;

use benterm_pty::{PtyConfig, PtyEvent, PtySession, WindowSize};
use common::{Collected, TestDir, collect_until};

/// Starts `/bin/sh -c script` on a terminal of 24 rows and 80 columns.
///
/// The shell is not interactive on purpose. An interactive `/bin/sh` is
/// bash with readline, and readline writes the size it has just read
/// back to the terminal each time it returns to the prompt. A resize that
/// falls between that read and that write is undone. The
/// scripts here read a line instead, so the test decides when they go on.
fn sh(script: &str) -> (PtySession, Receiver<PtyEvent>) {
    let config = PtyConfig {
        size: WindowSize::new(24, 80),
        ..PtyConfig::command("/bin/sh", ["-c", script])
    };
    PtySession::spawn(config).unwrap()
}

#[test]
fn resize_changes_the_size_the_child_sees() {
    let (session, events) = sh("stty size && IFS= read -r go && stty size");
    let mut collected = Collected::default();

    collect_until(&events, &mut collected, |c| c.text().contains("24 80\r\n"));
    assert!(collected.text().contains("24 80"), "{:?}", collected.text());

    session.resize(WindowSize::new(40, 120)).unwrap();
    session.write(b"go\n").unwrap();
    collect_until(&events, &mut collected, |_| false);
    assert!(
        collected.text().ends_with("40 120\r\n"),
        "{:?}",
        collected.text()
    );
    assert!(collected.exit.unwrap().success());
}

/// The size belongs to the terminal. A program on the terminal can set it too,
/// and the last write wins, so a program that writes back a size it read
/// earlier undoes a resize made in between.
#[test]
fn a_program_that_writes_back_the_size_it_read_undoes_a_resize_in_between() {
    let (session, events) = sh("old=$(stty size) && echo \"read $old\" && IFS= read -r go \
         && stty size && stty rows \"${old% *}\" cols \"${old#* }\" && stty size");
    let mut collected = Collected::default();

    collect_until(&events, &mut collected, |c| {
        c.text().contains("read 24 80\r\n")
    });
    session.resize(WindowSize::new(40, 120)).unwrap();
    session.write(b"go\n").unwrap();
    collect_until(&events, &mut collected, |_| false);
    assert!(
        collected.text().ends_with("40 120\r\n24 80\r\n"),
        "{:?}",
        collected.text()
    );
    assert!(collected.exit.unwrap().success());
}

/// Says `winch` for every SIGWINCH and `tick N` once the file `N` is in
/// the directory it is given, so the test knows that the signals sent
/// before it made the file have been counted: the shell runs a trap
/// before its next command. The question does not come over the terminal,
/// because the `read` of bash 3.2 loses the line it is reading when a
/// signal arrives.
const COUNTS_WINCH: &str = "trap 'echo winch' WINCH; echo ready; n=1; \
     while :; do sleep 0.02; \
       if [ -e \"$1/$n\" ]; then echo \"tick $n\"; n=$((n+1)); fi; \
     done";

/// The kernel signals a change of the size, not a call: setting the size
/// the terminal already has tells the program nothing. The pixel size is
/// part of the size, so a change of it alone is signalled. The app relies
/// on this when it starts a terminal at the size its pane will have: the
/// first frame then sets the same size again, and the program is not
/// disturbed.
#[test]
fn only_a_size_that_differs_is_signalled() {
    let dir = TestDir::new("winch");
    let config = PtyConfig {
        size: WindowSize::new(24, 80),
        ..PtyConfig::command(
            "/bin/sh",
            ["-c", COUNTS_WINCH, "sh", dir.0.to_str().unwrap()],
        )
    };
    let (session, events) = PtySession::spawn(config).unwrap();
    let mut collected = Collected::default();
    collect_until(&events, &mut collected, |c| c.text().contains("ready\r\n"));
    let mut tick = 0;
    let mut winches_after = |resize: WindowSize, collected: &mut Collected| {
        session.resize(resize).unwrap();
        tick += 1;
        std::fs::write(dir.0.join(tick.to_string()), "").unwrap();
        let mark = format!("tick {tick}\r\n");
        collect_until(&events, collected, |c| c.text().contains(&mark));
        collected.text().matches("winch").count()
    };

    assert_eq!(winches_after(WindowSize::new(24, 80), &mut collected), 0);
    assert_eq!(winches_after(WindowSize::new(40, 120), &mut collected), 1);
    assert_eq!(winches_after(WindowSize::new(40, 120), &mut collected), 1);
    let with_pixels = WindowSize {
        pixel_width: 960,
        pixel_height: 640,
        ..WindowSize::new(40, 120)
    };
    assert_eq!(winches_after(with_pixels, &mut collected), 2);
    assert_eq!(winches_after(with_pixels, &mut collected), 2);
}

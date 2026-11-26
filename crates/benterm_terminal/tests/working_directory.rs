//! A terminal knows the directory its shell is in, and where a terminal
//! opened beside it should start.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use benterm_core::GridSize;
use benterm_terminal::{PtyConfig, Screen, Terminal, TerminalConfig};

const TIMEOUT: Duration = Duration::from_secs(10);

/// The shells of the tests, started without the user's startup files:
/// (name, program, arguments).
const SHELLS: &[(&str, &str, &[&str])] = &[
    ("zsh", "/bin/zsh", &["-f", "-i"]),
    ("bash", "/bin/bash", &["--noprofile", "--norc", "-i"]),
];

struct Shell {
    name: &'static str,
    terminal: Terminal,
    wakes: Receiver<()>,
    screen: Screen,
}

/// Runs `body` with each shell started in a new directory that has the
/// directories `a` and `b` in it. The paths have their symbolic links
/// resolved, as the kernel reports directories.
fn each_shell(body: impl Fn(Shell, &Path)) {
    for (name, program, args) in SHELLS {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        fs::create_dir(root.join("a")).unwrap();
        fs::create_dir(root.join("b")).unwrap();
        let (wake_tx, wakes) = mpsc::channel();
        let config = TerminalConfig {
            pty: PtyConfig {
                cwd: Some(root.clone()),
                ..PtyConfig::command(*program, args.iter().copied())
            },
            // Wide, so that no typed line wraps.
            size: GridSize::new(24, 200),
            scrollback_limit: 100,
            colors: None,
            log: None,
        };
        let waker = move || {
            let _ = wake_tx.send(());
        };
        let shell = Shell {
            name,
            terminal: Terminal::spawn(config, waker).unwrap(),
            wakes,
            screen: Screen::default(),
        };
        body(shell, &root);
    }
}

impl Shell {
    /// Types `line` and waits until `mark` is on the screen. Marks are
    /// written with shell arithmetic, so the echo of the typed line does
    /// not match them.
    fn type_line(&mut self, line: &str, mark: &str) {
        self.terminal
            .write(format!("{line}\n").into_bytes())
            .unwrap();
        let deadline = Instant::now() + TIMEOUT;
        loop {
            self.terminal.sync(&mut self.screen);
            let rows: Vec<String> = self.screen.rows.iter().map(|row| row.text()).collect();
            if rows.iter().any(|row| row.contains(mark)) {
                return;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if self.wakes.recv_timeout(left).is_err() {
                panic!("{}: no {mark:?} before the timeout: {rows:?}", self.name);
            }
        }
    }
}

fn some(path: PathBuf) -> Option<PathBuf> {
    Some(path)
}

#[test]
fn the_working_directory_follows_cd() {
    each_shell(|mut shell, root| {
        let name = shell.name;
        shell.type_line("echo ready-$((40 + 2))", "ready-42");
        assert_eq!(
            shell.terminal.working_directory(),
            some(root.into()),
            "{name}"
        );

        shell.type_line("cd a; echo moved-$((40 + 2))", "moved-42");
        assert_eq!(
            shell.terminal.working_directory(),
            some(root.join("a")),
            "{name}"
        );
        assert_eq!(
            shell.terminal.directory_for_new_terminal(),
            some(root.join("a")),
            "{name}"
        );
    });
}

#[test]
fn a_new_terminal_starts_where_the_program_in_front_is() {
    each_shell(|mut shell, root| {
        let name = shell.name;
        shell.type_line(
            "cd a; /bin/sh -c 'cd ../b && echo in-$((20 + 1)) && exec /bin/sleep 100'",
            "in-21",
        );
        assert_eq!(
            shell.terminal.working_directory(),
            some(root.join("a")),
            "{name}"
        );
        assert_eq!(
            shell.terminal.foreground_working_directory(),
            some(root.join("b")),
            "{name}"
        );
        assert_eq!(
            shell.terminal.directory_for_new_terminal(),
            some(root.join("b")),
            "{name}"
        );
    });
}

#[test]
fn a_new_terminal_starts_where_the_shell_is_when_the_program_in_front_does_not_tell() {
    each_shell(|mut shell, root| {
        let name = shell.name;
        // `true` leads the job's process group and ends at once.
        shell.type_line(
            "cd a; true | (cd ../b; echo in-$((20 + 1)); exec /bin/sleep 100)",
            "in-21",
        );
        let deadline = Instant::now() + TIMEOUT;
        while shell.terminal.foreground_working_directory().is_some() {
            assert!(Instant::now() < deadline, "{name}: `true` has not ended");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            shell.terminal.directory_for_new_terminal(),
            some(root.join("a")),
            "{name}"
        );
    });
}

#[test]
fn a_new_terminal_does_not_start_in_a_directory_that_was_removed() {
    each_shell(|mut shell, root| {
        let name = shell.name;
        shell.type_line(
            "cd a; /bin/sh -c 'cd ../b && echo in-$((20 + 1)) && exec /bin/sleep 100'",
            "in-21",
        );

        // The kernel still reports the removed directory.
        fs::remove_dir(root.join("b")).unwrap();
        assert_eq!(
            shell.terminal.foreground_working_directory(),
            some(root.join("b")),
            "{name}"
        );
        assert_eq!(
            shell.terminal.directory_for_new_terminal(),
            some(root.join("a")),
            "{name}"
        );

        fs::remove_dir(root.join("a")).unwrap();
        assert_eq!(shell.terminal.directory_for_new_terminal(), None, "{name}");
    });
}

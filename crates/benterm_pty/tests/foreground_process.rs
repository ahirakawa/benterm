//! The session tells which program is in front of the terminal, by the
//! command line its first process was started with.

mod common;

use std::ffi::OsString;
use std::os::unix::fs::symlink;
use std::sync::mpsc::Receiver;

use benterm_pty::{PtyConfig, PtyEvent, PtySession};
use common::{Collected, TestDir, collect_to_exit, collect_until, wait_for};

/// A shell started without the user's startup files, in `dir`.
struct Shell {
    session: PtySession,
    events: Receiver<PtyEvent>,
    collected: Collected,
}

impl Shell {
    fn start(dir: &TestDir) -> Self {
        let config = PtyConfig {
            cwd: Some(dir.0.clone()),
            ..PtyConfig::command("/bin/zsh", ["-f", "-i"])
        };
        let (session, events) = PtySession::spawn(config).unwrap();
        let mut shell = Self {
            session,
            events,
            collected: Collected::default(),
        };
        shell.type_line("echo ready-$((40 + 2))");
        shell.wait_for_output("ready-42");
        shell
    }

    fn type_line(&self, line: &str) {
        self.session.write(format!("{line}\n").as_bytes()).unwrap();
    }

    fn wait_for_output(&mut self, mark: &str) {
        collect_until(&self.events, &mut self.collected, |c| {
            c.text().contains(mark)
        });
    }

    /// The command line of the program in front, as text.
    fn front(&self) -> Option<Vec<String>> {
        let front = self.session.foreground_process()?;
        Some(
            front
                .args
                .iter()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect(),
        )
    }
}

fn strings(args: &[&str]) -> Option<Vec<String>> {
    Some(args.iter().map(|arg| (*arg).to_owned()).collect())
}

#[test]
fn at_the_prompt_the_shell_is_in_front() {
    let dir = TestDir::new("front-prompt");
    let shell = Shell::start(&dir);
    let front = shell.session.foreground_process().unwrap();
    assert_eq!(front.pid, shell.session.pid());
    assert_eq!(
        front.args,
        vec![OsString::from("/bin/zsh"), "-f".into(), "-i".into()]
    );
}

#[test]
fn a_job_in_front_is_told_by_the_name_it_was_started_with() {
    // A program whose file has another name than the command that
    // started it, as Claude Code's file is named after its version.
    let dir = TestDir::new("front-job");
    symlink("/bin/sleep", dir.0.join("claude")).unwrap();
    let mut shell = Shell::start(&dir);
    shell.type_line("echo started-$((20 + 1)); ./claude 100");
    shell.wait_for_output("started-21");
    wait_for("the job is in front", strings(&["./claude", "100"]), || {
        shell.front()
    });
    let front = shell.session.foreground_process().unwrap();
    assert_ne!(front.pid, shell.session.pid());

    shell.session.write(b"\x03").unwrap();
    wait_for(
        "the shell is in front again",
        strings(&["/bin/zsh", "-f", "-i"]),
        || shell.front(),
    );
}

#[test]
fn a_script_in_front_is_told_with_its_interpreter() {
    let dir = TestDir::new("front-script");
    let script = dir.0.join("codex");
    // Without `exec`, so that the interpreter stays in front.
    std::fs::write(&script, "#!/bin/sh\n/bin/sleep 100\n").unwrap();
    std::fs::set_permissions(&script, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let mut shell = Shell::start(&dir);
    shell.type_line("echo started-$((20 + 1)); ./codex");
    shell.wait_for_output("started-21");
    wait_for(
        "the script is in front",
        strings(&["/bin/sh", "./codex"]),
        || shell.front(),
    );
    shell.session.write(b"\x03").unwrap();
}

#[test]
fn nothing_is_in_front_after_the_program_has_exited() {
    let (session, events) =
        PtySession::spawn(PtyConfig::command("/bin/sh", ["-c", "exit 3"])).unwrap();
    let collected = collect_to_exit(&events);
    assert_eq!(collected.exit.and_then(|status| status.code()), Some(3));
    assert!(session.foreground_process().is_none());
}

#[test]
fn a_child_that_has_not_taken_its_terminal_yet_is_in_front() {
    // Right after the start the child may not have made the terminal
    // its own yet. Nothing else can be in front then.
    for _ in 0..20 {
        let (session, _events) =
            PtySession::spawn(PtyConfig::command("/bin/sleep", ["100"])).unwrap();
        let front = session.foreground_process().expect("the child is in front");
        assert_eq!(front.pid, session.pid());
    }
}

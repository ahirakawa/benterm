//! While another program runs in front of the shell, the session tells
//! the shell's directory and that program's directory apart.

mod common;

use std::path::Path;
use std::sync::mpsc::Receiver;

use benterm_pty::{PtyConfig, PtyEvent, PtySession};
use common::{Collected, TestDir, collect_until, wait_for};

/// The shells of the tests, started without the user's startup files:
/// (name, program, arguments).
const SHELLS: &[(&str, &str, &[&str])] = &[
    ("zsh", "/bin/zsh", &["-f", "-i"]),
    ("bash", "/bin/bash", &["--noprofile", "--norc", "-i"]),
];

/// A shell in `dir`, which has the directory `other` in it.
struct Shell {
    name: &'static str,
    session: PtySession,
    events: Receiver<PtyEvent>,
    collected: Collected,
}

fn each_shell(test: &str, body: impl Fn(Shell, &Path, &Path)) {
    for (name, program, args) in SHELLS {
        let dir = TestDir::new(&format!("{test}-{name}"));
        let other = dir.sub("other");
        let config = PtyConfig {
            cwd: Some(dir.0.clone()),
            ..PtyConfig::command(*program, args.iter().copied())
        };
        let (session, events) = PtySession::spawn(config).unwrap();
        let mut shell = Shell {
            name,
            session,
            events,
            collected: Collected::default(),
        };
        // The shell is reading commands.
        shell.type_line("echo ready-$((40 + 2))");
        shell.wait_for_output("ready-42");
        body(shell, &dir.0, &other);
    }
}

impl Shell {
    fn type_line(&self, line: &str) {
        self.session.write(format!("{line}\n").as_bytes()).unwrap();
    }

    /// Waits for `mark` in the output. Marks are written with shell
    /// arithmetic, so the echo of the typed line does not match them.
    fn wait_for_output(&mut self, mark: &str) {
        collect_until(&self.events, &mut self.collected, |c| {
            c.text().contains(mark)
        });
        assert!(self.collected.exit.is_none(), "{:?}", self.collected.text());
    }
}

#[test]
fn at_the_prompt_the_foreground_directory_is_the_shells() {
    each_shell("prompt", |mut shell, dir, other| {
        let name = shell.name;
        assert_eq!(
            shell.session.foreground_working_directory(),
            Some(dir.to_owned()),
            "{name}"
        );

        shell.type_line("cd other; echo moved-$((40 + 2))");
        shell.wait_for_output("moved-42");
        assert_eq!(
            shell.session.foreground_working_directory(),
            Some(other.to_owned()),
            "{name}"
        );
    });
}

#[test]
fn a_program_in_front_has_a_directory_of_its_own() {
    each_shell("program", |mut shell, dir, other| {
        let name = shell.name;
        shell.type_line("/bin/sh -c 'cd other && echo in-$((20 + 1)) && exec /bin/sleep 100'");
        shell.wait_for_output("in-21");
        assert_eq!(
            shell.session.foreground_working_directory(),
            Some(other.to_owned()),
            "{name}"
        );
        assert_eq!(
            shell.session.working_directory(),
            Some(dir.to_owned()),
            "{name}"
        );

        // Ctrl-C ends the program, and the shell is in front again.
        shell.session.write(b"\x03").unwrap();
        wait_for(
            &format!("{name}: the shell is in front again"),
            Some(dir.to_owned()),
            || shell.session.foreground_working_directory(),
        );
    });
}

#[test]
fn a_shell_started_from_the_shell_is_followed() {
    each_shell("nested", |mut shell, dir, other| {
        let name = shell.name;
        // The inner shell's prompt tells that it reads what is typed
        // next. The outer shell works out the 21, so the echo of this
        // line does not match.
        shell.type_line("PS1=\"inner-$((20 + 1))> \" /bin/sh");
        shell.wait_for_output("inner-21> ");
        shell.type_line("cd other; echo inner-$((40 + 2))");
        shell.wait_for_output("inner-42");
        assert_eq!(
            shell.session.foreground_working_directory(),
            Some(other.to_owned()),
            "{name}"
        );
        assert_eq!(
            shell.session.working_directory(),
            Some(dir.to_owned()),
            "{name}"
        );
    });
}

#[test]
fn vim_changing_its_directory_does_not_move_the_shell() {
    if !Path::new("/usr/bin/vim").exists() {
        eprintln!("skipped: no /usr/bin/vim");
        return;
    }
    each_shell("vim", |shell, dir, other| {
        let name = shell.name;
        shell.type_line("/usr/bin/vim -u NONE -i NONE -n -c 'cd other'");
        wait_for(
            &format!("{name}: vim is in front, in the directory it changed to"),
            Some(other.to_owned()),
            || shell.session.foreground_working_directory(),
        );
        assert_eq!(
            shell.session.working_directory(),
            Some(dir.to_owned()),
            "{name}"
        );

        shell.session.write(b"\x1b:q!\r").unwrap();
        wait_for(
            &format!("{name}: the shell is in front again"),
            Some(dir.to_owned()),
            || shell.session.foreground_working_directory(),
        );
    });
}

#[test]
fn a_foreground_job_whose_first_process_has_ended_has_no_directory() {
    each_shell("pipeline", |mut shell, dir, _| {
        let name = shell.name;
        // `true` leads the job's process group and ends at once; the
        // rest of the job runs on in another directory.
        shell.type_line("true | (cd other; echo in-$((20 + 1)); exec /bin/sleep 100)");
        shell.wait_for_output("in-21");
        wait_for(
            &format!("{name}: the job's first process has ended"),
            None,
            || shell.session.foreground_working_directory(),
        );
        assert_eq!(
            shell.session.working_directory(),
            Some(dir.to_owned()),
            "{name}"
        );
    });
}

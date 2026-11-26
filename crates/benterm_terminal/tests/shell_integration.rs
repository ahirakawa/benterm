//! Benterm puts its shell integration into zsh and bash as they start, so
//! that they tell where each prompt starts and where each command ends
//! without any setting of the user's. The user's own startup files are
//! still read.
//!
//! Each shell here runs with a home of its own, so the startup files of
//! whoever runs the tests stay out. What each shell prints is recorded,
//! so that the marks it gave can be read back in order.

use std::fs::{self, File};
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use benterm_core::GridSize;
use benterm_log::{LogReader, Record, RecordingConfig};
use benterm_terminal::{CommandEnd, PtyConfig, Screen, Terminal, TerminalConfig};

const TIMEOUT: Duration = Duration::from_secs(10);

struct Shell {
    terminal: Terminal,
    wakes: Receiver<()>,
    screen: Screen,
    log: PathBuf,
    _home: tempfile::TempDir,
}

fn home(files: &[(&str, &str)]) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    for (name, text) in files {
        let path = root.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }
    (dir, root)
}

fn start(program: &str, args: &[&str], home: (tempfile::TempDir, PathBuf)) -> Shell {
    start_with(program, args, home, &[])
}

fn start_with(
    program: &str,
    args: &[&str],
    (dir, root): (tempfile::TempDir, PathBuf),
    env: &[(&str, &Path)],
) -> Shell {
    let mut pty = PtyConfig::command(program, args.iter().copied());
    pty.cwd = Some(root.clone());
    pty.env = vec![("HOME".into(), root.into())];
    pty.env.extend(
        env.iter()
            .map(|(name, value)| ((*name).into(), value.into())),
    );
    spawn(pty, dir)
}

fn spawn(pty: PtyConfig, dir: tempfile::TempDir) -> Shell {
    let (wake_tx, wakes) = mpsc::channel();
    let size = GridSize::new(10, 60);
    let recording = RecordingConfig {
        enabled: true,
        dir: PathBuf::from("recording"),
    };
    let recorder = recording.start(dir.path(), size).unwrap().unwrap();
    let log = recorder.path().to_owned();
    let config = TerminalConfig {
        pty,
        size,
        scrollback_limit: 100,
        colors: None,
        log: Some(recorder),
    };
    let waker = move || {
        let _ = wake_tx.send(());
    };
    Shell {
        terminal: Terminal::spawn(config, waker).unwrap(),
        wakes,
        screen: Screen::default(),
        log,
        _home: dir,
    }
}

impl Shell {
    fn wait_until(&mut self, what: &str, mut done: impl FnMut(&mut Self) -> bool) {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            self.terminal.sync(&mut self.screen);
            if done(self) {
                return;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if self.wakes.recv_timeout(left).is_err() {
                panic!("{what}: not before the timeout; screen: {:?}", self.text());
            }
        }
    }

    fn text(&self) -> String {
        let rows: Vec<String> = self.screen.rows.iter().map(|row| row.text()).collect();
        rows.join("\n")
    }

    fn shows(&mut self, text: &str) -> bool {
        self.text().contains(text)
    }

    /// Whether a row that is `line` alone has a row starting with
    /// `prompt` right after it.
    fn prompts_after(&self, line: &str, prompt: &str) -> bool {
        let rows: Vec<String> = self.screen.rows.iter().map(|row| row.text()).collect();
        rows.windows(2)
            .any(|pair| pair[0].trim_end() == line && pair[1].starts_with(prompt))
    }

    fn run(&mut self, line: &str) {
        assert!(
            self.terminal
                .write(format!("{line}\r").into_bytes())
                .is_ok()
        );
    }

    /// Runs `line` and waits for the end of the command.
    fn ends(&mut self, line: &str) -> Vec<CommandEnd> {
        self.run(line);
        let mut ends = Vec::new();
        self.wait_until(&format!("the end of {line:?}"), |shell| {
            ends.extend(shell.terminal.take_command_ends());
            !ends.is_empty()
        });
        ends
    }

    /// The OSC 133 marks the shell printed so far, in order, without
    /// their introducer and terminator: `A`, `D;0` and so on.
    fn marks(&self) -> Vec<String> {
        let reader = LogReader::new(BufReader::new(File::open(&self.log).unwrap())).unwrap();
        let mut output = Vec::new();
        for record in reader {
            if let Record::Output(bytes) = record.unwrap() {
                output.extend(bytes);
            }
        }
        let output = String::from_utf8_lossy(&output);
        let mut marks = Vec::new();
        let mut rest = output.as_ref();
        while let Some(start) = rest.find("\x1b]133;") {
            rest = &rest[start + "\x1b]133;".len()..];
            let end = rest.find(['\x07', '\x1b']).unwrap_or(rest.len());
            marks.push(rest[..end].to_owned());
            rest = &rest[end..];
        }
        marks
    }

    /// Waits until the shell has printed `marks`, and no other, in all.
    fn wait_for_marks(&mut self, marks: &[&str]) {
        self.wait_until(&format!("the marks {marks:?}"), |shell| {
            shell.marks().len() >= marks.len()
        });
        assert_eq!(self.marks(), marks);
    }

    fn wait_for_prompt(&mut self, prompt: &str) {
        self.wait_until("the prompt", |shell| shell.shows(prompt));
    }
}

fn end(exit_code: i32) -> CommandEnd {
    CommandEnd {
        exit_code: Some(exit_code),
    }
}

const ZSHRC: (&str, &str) = (".zshrc", "PS1='zsh> '\n");
const BASH_PROFILE: (&str, &str) = (".bash_profile", "PS1='bash> '\n");

#[test]
fn zsh_tells_the_exit_codes_of_false_and_true() {
    let mut zsh = start("/bin/zsh", &["-i"], home(&[ZSHRC]));
    zsh.wait_for_prompt("zsh> ");
    assert_eq!(zsh.ends("false"), [end(1)]);
    assert_eq!(zsh.ends("true"), [end(0)]);
    assert_eq!(zsh.ends("sh -c 'exit 7'"), [end(7)]);
}

#[test]
fn bash_tells_the_exit_codes_of_false_and_true() {
    let mut bash = start("/bin/bash", &["--login", "-i"], home(&[BASH_PROFILE]));
    bash.wait_for_prompt("bash> ");
    assert_eq!(bash.ends("false"), [end(1)]);
    assert_eq!(bash.ends("true"), [end(0)]);
    assert_eq!(bash.ends("sh -c 'exit 7'"), [end(7)]);
}

#[test]
fn zsh_tells_where_each_prompt_starts() {
    let mut zsh = start("/bin/zsh", &["-i"], home(&[ZSHRC]));
    zsh.wait_for_prompt("zsh> ");
    zsh.wait_for_marks(&["A"]);
    assert_eq!(zsh.ends("true"), [end(0)]);
    zsh.wait_for_marks(&["A", "D;0", "A"]);
}

#[test]
fn bash_tells_where_each_prompt_starts() {
    let mut bash = start("/bin/bash", &["--login", "-i"], home(&[BASH_PROFILE]));
    bash.wait_for_prompt("bash> ");
    bash.wait_for_marks(&["A"]);
    assert_eq!(bash.ends("true"), [end(0)]);
    bash.wait_for_marks(&["A", "D;0", "A"]);
}

#[test]
fn no_end_is_told_before_the_first_command() {
    let mut zsh = start("/bin/zsh", &["-i"], home(&[ZSHRC]));
    zsh.wait_for_prompt("zsh> ");
    let mut bash = start("/bin/bash", &["--login", "-i"], home(&[BASH_PROFILE]));
    bash.wait_for_prompt("bash> ");
    // The prompt after the output of a command shows that everything
    // before it was read, the end of the command too: the shell tells
    // the end before it prompts. The arithmetic keeps the typed line
    // from matching.
    zsh.run("echo mark-$((40 + 2))");
    zsh.wait_until("the prompt after the mark", |zsh| {
        zsh.prompts_after("mark-42", "zsh>")
    });
    bash.run("echo mark-$((40 + 2))");
    bash.wait_until("the prompt after the mark", |bash| {
        bash.prompts_after("mark-42", "bash>")
    });
    assert_eq!(zsh.terminal.take_command_ends(), [end(0)]);
    assert_eq!(bash.terminal.take_command_ends(), [end(0)]);
}

#[test]
fn zsh_still_reads_the_users_startup_files() {
    let files = [
        (".zshenv", "export FROM_ZSHENV=env-ok\n"),
        (".zprofile", "export FROM_ZPROFILE=profile-ok\n"),
        ZSHRC,
    ];
    let mut zsh = start("/bin/zsh", &["-l", "-i"], home(&files));
    zsh.wait_for_prompt("zsh> ");
    zsh.run("echo \"$FROM_ZSHENV $FROM_ZPROFILE ${ZDOTDIR-unset}\"");
    zsh.wait_until("the variables", |zsh| zsh.shows("env-ok profile-ok unset"));
}

#[test]
fn zsh_reads_the_startup_files_of_the_zdotdir_it_was_given() {
    let files = [
        ("conf/.zshenv", "export FROM_ZSHENV=env-ok\n"),
        ("conf/.zshrc", "PS1='conf> '\n"),
    ];
    let home = home(&files);
    let conf = home.1.join("conf");
    let mut zsh = start_with("/bin/zsh", &["-i"], home, &[("ZDOTDIR", &conf)]);
    zsh.wait_for_prompt("conf> ");
    assert_eq!(zsh.ends("echo \"$FROM_ZSHENV ${ZDOTDIR:t}\""), [end(0)]);
    zsh.wait_until("the variables", |zsh| zsh.shows("env-ok conf"));
    assert_eq!(zsh.ends("false"), [end(1)]);
}

#[test]
fn a_prompt_command_of_bashs_own_still_runs() {
    let profile = (
        ".bash_profile",
        "PS1='bash> '\nPROMPT_COMMAND=\"${PROMPT_COMMAND:+$PROMPT_COMMAND; }echo -n pc-ran:\\$?' '\"\n",
    );
    let mut bash = start("/bin/bash", &["--login", "-i"], home(&[profile]));
    bash.wait_for_prompt("bash> ");
    assert_eq!(bash.ends("false"), [end(1)]);
    // The user's command still sees the exit code of the command.
    bash.wait_until("the user's prompt command", |bash| {
        bash.shows("pc-ran:1 bash> ")
    });
}

#[test]
fn zsh_keeps_the_zdotdir_of_the_login_environment_it_was_given() {
    // The app opened from Finder starts its shells in the environment a
    // login shell printed, which may hold the user's ZDOTDIR.
    let files = [
        ("conf/.zshenv", "export FROM_ZSHENV=env-ok\n"),
        ("conf/.zshrc", "PS1='conf> '\n"),
    ];
    let (dir, root) = home(&files);
    let mut base: Vec<(std::ffi::OsString, std::ffi::OsString)> = std::env::vars_os().collect();
    base.push(("ZDOTDIR".into(), root.join("conf").into()));
    base.push(("HOME".into(), root.clone().into()));
    let mut pty = PtyConfig::command("/bin/zsh", ["-i"]);
    pty.cwd = Some(root);
    pty.base_env = Some(std::sync::Arc::new(base));
    let mut zsh = spawn(pty, dir);
    zsh.wait_for_prompt("conf> ");
    assert_eq!(zsh.ends("echo \"$FROM_ZSHENV ${ZDOTDIR:t}\""), [end(0)]);
    zsh.wait_until("the variables", |zsh| zsh.shows("env-ok conf"));
    assert_eq!(zsh.ends("false"), [end(1)]);
}

#[test]
fn a_precmd_function_of_zshs_own_still_runs() {
    let zshrc = (
        ".zshrc",
        "__user_precmd() { PS1=\"pc-ran:$? zsh> \" }\nprecmd_functions+=(__user_precmd)\n",
    );
    let mut zsh = start("/bin/zsh", &["-i"], home(&[zshrc]));
    zsh.wait_for_prompt("pc-ran:0 zsh> ");
    assert_eq!(zsh.ends("false"), [end(1)]);
    // The user's function still sees the exit code of the command.
    zsh.wait_until("the user's precmd function", |zsh| {
        zsh.shows("pc-ran:1 zsh> ")
    });
}

/// A home whose `bin` has a `codex` that prints its arguments, and whose
/// `--help` lists `--no-daemon` or not.
fn home_with_codex(files: &[(&str, &str)], takes_no_daemon: bool) -> (tempfile::TempDir, PathBuf) {
    use std::os::unix::fs::PermissionsExt;
    let help = if takes_no_daemon {
        "--no-daemon"
    } else {
        "--oss"
    };
    let (dir, root) = home(files);
    let bin = root.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let codex = bin.join("codex");
    fs::write(
        &codex,
        format!("#!/bin/sh\nif [ \"$1\" = --help ]; then echo '  {help}'; exit 0; fi\necho \"ran codex $*.\"\n"),
    )
    .unwrap();
    fs::set_permissions(&codex, fs::Permissions::from_mode(0o755)).unwrap();
    (dir, root)
}

fn start_with_codex(program: &str, args: &[&str], home: (tempfile::TempDir, PathBuf)) -> Shell {
    let path = format!("{}:/usr/bin:/bin", home.1.join("bin").display());
    let path = PathBuf::from(path);
    start_with(program, args, home, &[("PATH", &path)])
}

#[test]
fn zsh_gives_codex_no_daemon_when_it_takes_it() {
    let mut zsh = start_with_codex("/bin/zsh", &["-i"], home_with_codex(&[ZSHRC], true));
    zsh.wait_for_prompt("zsh> ");
    zsh.ends("codex fix it");
    zsh.wait_until("codex", |zsh| zsh.shows("ran codex --no-daemon fix it."));
    zsh.ends("codex agents");
    zsh.wait_until("codex agents", |zsh| zsh.shows("ran codex agents."));
    zsh.ends("codex --remote ws://x");
    zsh.wait_until("codex --remote", |zsh| {
        zsh.shows("ran codex --remote ws://x.")
    });
}

#[test]
fn zsh_leaves_no_daemon_out_for_a_codex_that_does_not_take_it() {
    let mut zsh = start_with_codex("/bin/zsh", &["-i"], home_with_codex(&[ZSHRC], false));
    zsh.wait_for_prompt("zsh> ");
    zsh.ends("codex fix it");
    zsh.wait_until("codex", |zsh| zsh.shows("ran codex fix it."));
}

#[test]
fn a_codex_function_of_the_users_wins_in_zsh() {
    let zshrc = (
        ".zshrc",
        "PS1='zsh> '\nfunction codex { echo \"mine $*.\" }\n",
    );
    let mut zsh = start_with_codex("/bin/zsh", &["-i"], home_with_codex(&[zshrc], true));
    zsh.wait_for_prompt("zsh> ");
    zsh.ends("codex fix it");
    zsh.wait_until("the user's codex", |zsh| zsh.shows("mine fix it."));
}

#[test]
fn bash_gives_codex_no_daemon_when_it_takes_it() {
    let mut bash = start_with_codex(
        "/bin/bash",
        &["--login", "-i"],
        // /etc/profile puts the system's directories first again.
        home_with_codex(
            &[(".bash_profile", "PS1='bash> '\nPATH=$HOME/bin:$PATH\n")],
            true,
        ),
    );
    bash.wait_for_prompt("bash> ");
    bash.ends("codex fix it");
    bash.wait_until("codex", |bash| bash.shows("ran codex --no-daemon fix it."));
    bash.ends("codex queue");
    bash.wait_until("codex queue", |bash| bash.shows("ran codex queue."));
}

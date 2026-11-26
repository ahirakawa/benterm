//! Runs an agent on a PTY, drives it from a script, and saves the PTY output
//! as benterm logs. It is for checking real screens on the developer's
//! machine; save the logs outside the repository, as CONTRIBUTING.md says.
//!
//! Usage: `cargo run -p benterm_agent --example record -- <script> <out-dir>`
//!
//! A script has one command per line. Blank lines and lines starting with
//! `#` are ignored.
//!
//! ```text
//! size <rows> <cols>     grid size; must come before `run`
//! cwd <dir>              working directory; must come before `run`
//! run <program> <args>   start the program
//! wait <text>            wait until the screen contains <text>
//! send <text>            write <text>; \r, \n, \t, \e and \\ are escapes
//! sleep <ms>             keep reading output for <ms> milliseconds
//! save <name>            write the log so far to <out-dir>/<name>.bentermlog
//! show                   print the screen to stderr
//! ```
//!
//! The working directory and the prompts are chosen by the script, so pick
//! ones that put nothing personal on the screen. Before saving, the recorder
//! refuses output that contains the home directory or the user name.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use benterm_core::{AlacrittyCore, GridSize, TerminalCore};
use benterm_log::{LogWriter, Record, screen_text};
use benterm_pty::{PtyConfig, PtyEvent, PtySession, WindowSize};

const WAIT_TIMEOUT: Duration = Duration::from_secs(180);
const SCROLLBACK_LIMIT: usize = 10_000;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [script, out_dir] = args.as_slice() else {
        eprintln!("usage: record <script> <out-dir>");
        return ExitCode::from(2);
    };
    match run(Path::new(script), Path::new(out_dir)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("record: {error}");
            ExitCode::FAILURE
        }
    }
}

struct Recording {
    session: PtySession,
    events: Receiver<PtyEvent>,
    core: AlacrittyCore,
    size: GridSize,
    records: Vec<Record>,
    exited: bool,
}

impl Recording {
    /// The log of everything read so far.
    fn log_bytes(&self) -> Result<Vec<u8>, String> {
        let mut log = LogWriter::new(Vec::new(), self.size).map_err(|error| error.to_string())?;
        for record in &self.records {
            log.write_record(record)
                .map_err(|error| error.to_string())?;
        }
        Ok(log.into_inner())
    }

    /// Reads output until `deadline` or until `done` holds for the screen.
    fn pump(&mut self, deadline: Instant, done: impl Fn(&str) -> bool) -> Result<bool, String> {
        loop {
            if done(&screen_text(&self.core.snapshot())) {
                return Ok(true);
            }
            let now = Instant::now();
            if now >= deadline || self.exited {
                return Ok(false);
            }
            match self.events.recv_timeout(deadline - now) {
                Ok(PtyEvent::Output(bytes)) => {
                    self.core.feed(&bytes);
                    self.records.push(Record::Output(bytes));
                    let replies = self.core.take_pty_replies();
                    if !replies.is_empty() {
                        self.session
                            .write(&replies)
                            .map_err(|error| error.to_string())?;
                    }
                }
                Ok(PtyEvent::Exited(_)) | Err(RecvTimeoutError::Disconnected) => {
                    self.exited = true;
                }
                Err(RecvTimeoutError::Timeout) => {}
            }
        }
    }
}

fn run(script: &Path, out_dir: &Path) -> Result<(), String> {
    let script = fs::read_to_string(script).map_err(|error| error.to_string())?;
    fs::create_dir_all(out_dir).map_err(|error| error.to_string())?;
    let forbidden = forbidden_strings();

    let mut size = GridSize::new(40, 120);
    let mut cwd: Option<PathBuf> = None;
    let mut recording: Option<Recording> = None;

    for (number, line) in script.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (command, rest) = line.split_once(' ').unwrap_or((line, ""));
        let context = |message: String| format!("line {}: {message}", number + 1);
        match command {
            "size" => {
                let mut parts = rest.split_whitespace().map(str::parse::<u16>);
                let (Some(Ok(rows)), Some(Ok(cols))) = (parts.next(), parts.next()) else {
                    return Err(context("size needs <rows> <cols>".into()));
                };
                size = GridSize::new(rows, cols);
            }
            "cwd" => {
                fs::create_dir_all(rest).map_err(|error| context(error.to_string()))?;
                cwd = Some(PathBuf::from(rest));
            }
            "run" => {
                let mut parts = rest.split_whitespace();
                let program = parts
                    .next()
                    .ok_or_else(|| context("run needs a program".into()))?;
                let mut config = PtyConfig::command(program, parts);
                config.cwd = cwd.clone();
                config.size = WindowSize::new(size.rows, size.cols);
                config.env = vec![("TERM".into(), "xterm-256color".into())];
                let (session, events) =
                    PtySession::spawn(config).map_err(|error| context(error.to_string()))?;
                recording = Some(Recording {
                    session,
                    events,
                    core: AlacrittyCore::new(size, SCROLLBACK_LIMIT),
                    size,
                    records: Vec::new(),
                    exited: false,
                });
            }
            _ => {
                let recording = recording
                    .as_mut()
                    .ok_or_else(|| context(format!("{command} before run")))?;
                match command {
                    "wait" => {
                        let found = recording.pump(Instant::now() + WAIT_TIMEOUT, |screen| {
                            screen.contains(rest)
                        })?;
                        if !found {
                            eprint!("{}", screen_text(&recording.core.snapshot()));
                            return Err(context(format!("timed out waiting for {rest:?}")));
                        }
                    }
                    "send" => {
                        recording
                            .session
                            .write(unescape(rest).as_bytes())
                            .map_err(|error| context(error.to_string()))?;
                    }
                    "sleep" => {
                        let ms: u64 = rest
                            .parse()
                            .map_err(|_| context("sleep needs milliseconds".into()))?;
                        recording.pump(Instant::now() + Duration::from_millis(ms), |_| false)?;
                    }
                    "save" => {
                        let bytes = recording.log_bytes().map_err(context)?;
                        if let Some(found) = forbidden.iter().find(|s| contains(&bytes, s)) {
                            return Err(context(format!(
                                "the output contains {found:?}; choose another directory or prompt"
                            )));
                        }
                        let path = out_dir.join(format!("{rest}.bentermlog"));
                        fs::write(&path, &bytes).map_err(|error| context(error.to_string()))?;
                        eprintln!("saved {}", path.display());
                    }
                    "show" => eprint!("{}", screen_text(&recording.core.snapshot())),
                    _ => return Err(context(format!("unknown command {command}"))),
                }
            }
        }
    }
    if let Some(recording) = recording {
        recording.session.shutdown();
    }
    Ok(())
}

/// Strings that must not appear in a saved log: the home directory and the
/// user name.
fn forbidden_strings() -> Vec<String> {
    let mut strings = Vec::new();
    if let Some(home) = std::env::var_os("HOME") {
        strings.push(home.to_string_lossy().into_owned());
    }
    if let Some(user) = std::env::var_os("USER") {
        strings.push(user.to_string_lossy().into_owned());
    }
    strings.retain(|s| s.len() > 1);
    strings
}

fn contains(haystack: &[u8], needle: &str) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle.as_bytes())
}

fn unescape(text: &str) -> String {
    let mut out = String::new();
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('r') => out.push('\r'),
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('e') => out.push('\x1b'),
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    out
}

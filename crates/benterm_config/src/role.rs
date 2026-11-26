//! Roles: what an agent is for, one TOML file each in the `roles`
//! directory beside the config file.

use crate::file::{config_path_from, decode};
use crate::read::{Problem, Warning};
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread::{self, JoinHandle};
use std::time::Duration;
use std::{fs, io};
use toml::{Table, Value};

/// How long the directory must go without changes before it is read, as
/// for the config file.
const SETTLE: Duration = Duration::from_millis(100);

/// The keys a role file may have.
const KEYS: [&str; 8] = [
    "name",
    "description",
    "agent",
    "model",
    "permission_mode",
    "prompt",
    "env",
    "args",
];

/// The modes of `claude --permission-mode`.
const CLAUDE_MODES: [&str; 6] = [
    "acceptEdits",
    "auto",
    "bypassPermissions",
    "dontAsk",
    "manual",
    "plan",
];

/// The policies of `codex --sandbox`.
const CODEX_MODES: [&str; 3] = ["read-only", "workspace-write", "danger-full-access"];

/// The longest name of an agent, as the app takes it.
const NAME_LIMIT: usize = 64;

/// The program a role starts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Agent {
    Claude,
    Codex,
}

/// One role, as its file says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Role {
    /// Also the name of the agents started with it, unless they are
    /// given one of their own.
    pub name: String,
    pub description: String,
    pub agent: Agent,
    /// Without one the agent picks its own.
    pub model: Option<String>,
    /// Claude Code's `--permission-mode`, or Codex's `--sandbox`. Without
    /// one the agent's own setting stays.
    pub permission_mode: Option<String>,
    /// The instructions the agent is given first.
    pub prompt: String,
    /// Variables for the agent's process, in order of name. They are put
    /// over the ones every pane gets.
    pub env: Vec<(String, String)>,
    /// More arguments for the agent, after its model and mode and before
    /// the prompt.
    pub args: Vec<String>,
    /// The file of the rules for workers, which the first prompt asks the
    /// agent to read first if it is there when the agent starts. See
    /// [`worker_rules_line`].
    pub worker_rules: Option<PathBuf>,
}

/// The roles of a directory, in order of name, and the files that were
/// left out.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Roles {
    pub roles: Vec<Role>,
    pub warnings: Vec<Warning>,
}

impl Agent {
    /// The name of its program, which is also how a role file names it.
    pub fn program(self) -> &'static str {
        match self {
            Agent::Claude => "claude",
            Agent::Codex => "codex",
        }
    }

    /// The program and the options Benterm starts every agent of this
    /// kind with. Codex runs without its shared background server: the
    /// server runs the commands of every Codex with the environment of the
    /// Codex that started it, so they would report for another pane.
    pub fn launch(self) -> Vec<String> {
        match self {
            Agent::Claude => vec!["claude".to_owned()],
            Agent::Codex => vec!["codex".to_owned(), "--no-daemon".to_owned()],
        }
    }

    /// The option that takes [`Role::permission_mode`], and its values.
    fn mode_option(self) -> (&'static str, &'static [&'static str]) {
        match self {
            Agent::Claude => ("--permission-mode", &CLAUDE_MODES),
            Agent::Codex => ("--sandbox", &CODEX_MODES),
        }
    }
}

impl Role {
    /// The first message to the agent: the instructions, then the task
    /// after a blank line. When the file of [`Role::worker_rules`] is
    /// there, the line that asks to read it comes first, on a line of its
    /// own.
    pub fn first_prompt(&self, task: &str) -> String {
        let instructions = self.prompt.trim_end();
        let prompt = match task.trim() {
            "" => instructions.to_owned(),
            task => format!("{instructions}\n\n{task}"),
        };
        match self.worker_rules.as_deref().and_then(worker_rules_line) {
            Some(line) => format!("{line}\n{prompt}"),
            None => prompt,
        }
    }

    /// The program and its arguments that start an agent of this role on
    /// `task`. The prompt comes after `--`, so a prompt that starts with
    /// `-` is not taken for an option.
    pub fn command(&self, task: &str) -> Vec<String> {
        let mut command = self.agent.launch();
        command.extend(self.launch_options());
        command.extend(["--".to_owned(), self.first_prompt(task)]);
        command
    }

    /// The model, permission mode and extra arguments for this role.
    pub fn launch_options(&self) -> Vec<String> {
        let mut options = Vec::new();
        if let Some(model) = &self.model {
            options.extend(["--model".to_owned(), model.clone()]);
        }
        if let Some(mode) = &self.permission_mode {
            let (option, _) = self.agent.mode_option();
            options.extend([option.to_owned(), mode.clone()]);
        }
        options.extend(self.args.iter().cloned());
        options
    }
}

/// Takes `--no-daemon` out of `command`, as [`Agent::launch`] puts it in,
/// when the Codex that `path` finds does not take it: 0.155 and older end
/// with an error on it, and a later version may drop it. `codex --help`
/// is asked at every start, as an upgrade would make a kept answer wrong;
/// it takes a few milliseconds.
pub fn fit_codex_launch(command: &mut Vec<String>, path: Option<&OsStr>) {
    let launches_codex = command.first().is_some_and(|program| program == "codex")
        && command.get(1).is_some_and(|option| option == "--no-daemon");
    if launches_codex && !codex_takes_no_daemon(path) {
        command.remove(1);
    }
}

/// Whether `codex --help`, run with `path` for `PATH`, lists `--no-daemon`.
/// A Codex that has not answered within two seconds is taken not to, and
/// is ended.
pub fn codex_takes_no_daemon(path: Option<&OsStr>) -> bool {
    let mut help = Command::new("codex");
    help.arg("--help")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if let Some(path) = path {
        help.env("PATH", path);
    }
    let Ok(mut child) = help.spawn() else {
        return false;
    };
    let Some(mut stdout) = child.stdout.take() else {
        return false;
    };
    // The help is read on a thread of its own, so that a full pipe does not
    // keep Codex from ending while the deadline is watched here.
    let reader = thread::spawn(move || {
        let mut text = String::new();
        let _ = io::Read::read_to_string(&mut stdout, &mut text);
        text
    });
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if std::time::Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(5));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
    reader.join().is_ok_and(|text| text.contains("--no-daemon"))
}

/// The roles directory for the current environment. See
/// [`roles_dir_from`].
pub fn roles_dir() -> Option<PathBuf> {
    roles_dir_from(
        std::env::var_os("XDG_CONFIG_HOME").as_deref(),
        std::env::var_os("HOME").as_deref(),
    )
}

/// The `roles` directory beside the file of
/// [`config_path_from`](crate::config_path_from).
pub fn roles_dir_from(xdg_config_home: Option<&OsStr>, home: Option<&OsStr>) -> Option<PathBuf> {
    let config = config_path_from(xdg_config_home, home)?;
    Some(config.parent()?.join("roles"))
}

/// The file of the rules for workers that go with the roles of
/// `roles_dir`: `worker.md` beside the directory, where the config file
/// is.
pub fn worker_rules_file(roles_dir: &Path) -> Option<PathBuf> {
    Some(roles_dir.parent()?.join("worker.md"))
}

/// The line that asks an agent to read the rules for workers in `file`
/// before it starts, and to put the same line first when it starts a
/// worker itself, so that the rules reach the workers of workers too.
/// None when there is no such file.
pub fn worker_rules_line(file: &Path) -> Option<String> {
    file.is_file().then(|| {
        format!(
            "Read {} before you start. When you start a worker, put this line first in its first message.",
            file.display()
        )
    })
}

/// Reads every `*.toml` file of `dir`, in order of file name. A file that
/// is not a role, or names a role an earlier file has, is left out with a
/// warning. A missing directory has no roles.
pub fn load_roles(dir: &Path) -> Roles {
    let mut files: Vec<PathBuf> = match fs::read_dir(dir) {
        Ok(entries) => entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| is_role_file(path))
            .collect(),
        Err(_) => Vec::new(),
    };
    files.sort();
    let mut roles = Roles::default();
    for path in files {
        let file_name = path.file_name().unwrap_or_default().to_string_lossy();
        let key = format!("roles/{file_name}");
        let warn = |problem| Warning {
            key: key.clone(),
            problem,
        };
        let read = match decode(fs::read(&path)) {
            // Gone since the directory was listed.
            Ok(None) => continue,
            Ok(Some(text)) => parse_role(&text, worker_rules_file(dir)).map_err(warn),
            Err(w) => Err(warn(w.problem)),
        };
        match read {
            Ok(role) if roles.roles.iter().any(|r| r.name == role.name) => {
                let reason = format!("another file has the role {:?}", role.name);
                roles.warnings.push(warn(Problem::InvalidRole(reason)));
            }
            Ok(role) => roles.roles.push(role),
            Err(w) => roles.warnings.push(w),
        }
    }
    roles.roles.sort_by(|a, b| a.name.cmp(&b.name));
    roles
}

/// Watches the roles directory and calls back on a background thread
/// with the roles after each change. Dropping it stops the watching.
pub struct RolesWatcher {
    watcher: Option<RecommendedWatcher>,
    thread: Option<JoinHandle<()>>,
}

impl RolesWatcher {
    /// Starts watching `dir`, made if it does not exist, and returns the
    /// roles in it now. The callback is called when the roles or their
    /// warnings differ from the last ones it was given.
    pub fn start(
        dir: impl Into<PathBuf>,
        mut on_change: impl FnMut(Roles) + Send + 'static,
    ) -> io::Result<(Self, Roles)> {
        let dir = dir.into();
        fs::create_dir_all(&dir)?;
        let dir = dir.canonicalize()?;
        let (tx, rx) = mpsc::channel();
        let mut watcher = notify::recommended_watcher(tx).map_err(io::Error::other)?;
        watcher
            .watch(&dir, RecursiveMode::NonRecursive)
            .map_err(io::Error::other)?;
        // Read only after the watch starts, so no save is missed in between.
        let roles = load_roles(&dir);
        let mut last = roles.clone();
        let thread = thread::Builder::new()
            .name("benterm-roles-watch".into())
            .spawn(move || {
                while let Ok(event) = rx.recv() {
                    if matches!(&event, Ok(e) if matches!(e.kind, EventKind::Access(_))) {
                        continue;
                    }
                    // An editor writes a file in pieces; read once it is quiet.
                    loop {
                        match rx.recv_timeout(SETTLE) {
                            Ok(_) => {}
                            Err(RecvTimeoutError::Timeout) => break,
                            Err(RecvTimeoutError::Disconnected) => return,
                        }
                    }
                    let roles = load_roles(&dir);
                    if roles != last {
                        last = roles.clone();
                        on_change(roles);
                    }
                }
            })?;
        Ok((
            Self {
                watcher: Some(watcher),
                thread: Some(thread),
            },
            roles,
        ))
    }
}

impl Drop for RolesWatcher {
    fn drop(&mut self) {
        // Dropping the watcher closes the channel, which ends the thread.
        drop(self.watcher.take());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Whether `path` is a file a role may be in: `*.toml`, and not hidden,
/// as editors keep their backups.
pub(crate) fn is_role_file(path: &Path) -> bool {
    let hidden = path
        .file_name()
        .is_some_and(|name| name.as_encoded_bytes().starts_with(b"."));
    !hidden && path.extension() == Some(OsStr::new("toml")) && !path.is_dir()
}

fn parse_role(text: &str, worker_rules: Option<PathBuf>) -> Result<Role, Problem> {
    let table: Table =
        toml::from_str(text).map_err(|e| Problem::Syntax(e.to_string().trim_end().to_owned()))?;
    let invalid = |reason: String| Problem::InvalidRole(reason);
    if let Some(key) = table.keys().find(|key| !KEYS.contains(&key.as_str())) {
        return Err(invalid(format!("unknown key {key:?}")));
    }
    let text = |key: &str, required: bool| -> Result<Option<String>, Problem> {
        match table.get(key) {
            None if required => Err(invalid(format!("{key} is missing"))),
            None => Ok(None),
            Some(Value::String(s)) if required && s.trim().is_empty() => {
                Err(invalid(format!("{key} is empty")))
            }
            Some(Value::String(s)) => Ok(Some(s.clone())),
            Some(_) => Err(invalid(format!("{key} must be a string"))),
        }
    };
    let name = text("name", true)?.unwrap_or_default();
    let word = name
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'));
    let number = name.chars().all(|ch| ch.is_ascii_digit());
    if name.len() > NAME_LIMIT || !word || number {
        return Err(invalid(format!(
            "name must be 1 to {NAME_LIMIT} letters, digits, '-', '_' or '.', and not a \
             number: {name:?}"
        )));
    }
    let agent = match text("agent", true)?.unwrap_or_default().as_str() {
        "claude" => Agent::Claude,
        "codex" => Agent::Codex,
        other => {
            return Err(invalid(format!(
                "agent must be \"claude\" or \"codex\", not {other:?}"
            )));
        }
    };
    let model = text("model", false)?.filter(|model| !model.trim().is_empty());
    let permission_mode = text("permission_mode", false)?;
    if let Some(mode) = &permission_mode {
        let (option, modes) = agent.mode_option();
        if !modes.contains(&mode.as_str()) {
            return Err(invalid(format!(
                "permission_mode {mode:?} is not one of {} {option}: {}",
                agent.program(),
                modes.join(", ")
            )));
        }
    }
    Ok(Role {
        name,
        description: text("description", false)?.unwrap_or_default(),
        agent,
        model,
        permission_mode,
        prompt: text("prompt", true)?.unwrap_or_default(),
        env: env(table.get("env"))?,
        args: args(table.get("args"))?,
        worker_rules,
    })
}

/// The `env` table: names a shell can set, each with a string.
fn env(value: Option<&Value>) -> Result<Vec<(String, String)>, Problem> {
    let invalid = |reason: String| Err(Problem::InvalidRole(reason));
    let table = match value {
        None => return Ok(Vec::new()),
        Some(Value::Table(table)) => table,
        Some(_) => return invalid("env must be a table of names and strings".into()),
    };
    let mut vars = Vec::with_capacity(table.len());
    for (name, value) in table {
        if name.is_empty() {
            return invalid("env has an empty name".into());
        }
        if name.contains('=') || name.starts_with(|ch: char| ch.is_ascii_digit()) {
            return invalid(format!("env name {name:?} has '=' or starts with a digit"));
        }
        let Value::String(value) = value else {
            return invalid(format!("env {name:?} must be a string"));
        };
        vars.push((name.clone(), value.clone()));
    }
    Ok(vars)
}

/// The `args` list, which may only have strings.
fn args(value: Option<&Value>) -> Result<Vec<String>, Problem> {
    let invalid = || Problem::InvalidRole("args must be a list of strings".into());
    match value {
        None => Ok(Vec::new()),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| item.as_str().map(str::to_owned).ok_or_else(invalid))
            .collect(),
        Some(_) => Err(invalid()),
    }
}

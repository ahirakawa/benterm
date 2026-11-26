//! `benterm setup`: puts in, checks and takes out what Benterm needs from
//! the user's agents: the hooks that run `benterm hook` and `benterm
//! report-check`, `benterm` on
//! the PATH, and the skill.
//!
//! It does not talk to the app, so it works when the app is not running.
//! What it writes is shown first and written only when the user agrees,
//! and a settings file is copied before it is changed.

use std::ffi::OsString;
use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

/// The Claude Code events `benterm hook --agent claude` is run for, in
/// the order of the README.
pub const CLAUDE_HOOK_EVENTS: [&str; 10] = [
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "PermissionRequest",
    "PostToolUse",
    "PostToolUseFailure",
    "Notification",
    "Stop",
    "StopFailure",
    "SessionEnd",
];

/// The Codex events `benterm hook --agent codex` is run for, in the
/// order of the README.
pub const CODEX_HOOK_EVENTS: [&str; 8] = [
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "PermissionRequest",
    "PostToolUse",
    "Stop",
    "Interrupt",
    "SessionEnd",
];

const USAGE: &str = "\
usage: benterm setup install [--yes] [--bin-dir DIR]
       benterm setup status [--bin-dir DIR]
       benterm setup uninstall [--yes] [--bin-dir DIR]

install adds the hooks that run benterm hook, and benterm report-check
for the Stop and after a tool call, to ~/.claude/settings.json and
~/.codex/hooks.json, links DIR/benterm to this benterm, and links
the benterm skill into ~/.claude/skills and ~/.codex/skills. DIR is
~/.local/bin unless --bin-dir names another. Hooks and settings already
there are kept as they are. What changes is shown first and written only
after y is answered, or right away with --yes. A settings file is copied
beside itself, with the date and time in its name, before it is changed.

status shows what of this is in place, and changes nothing.

The hooks run the link DIR/benterm by its whole path, so that no other
benterm on the PATH is run in its place. A hook already there that
names only benterm is rewritten to run the link.

uninstall takes out what install puts in, and only that: the hooks
exactly as install writes them, and links to this benterm and its skill.
";

/// The exit status for a failure, or changes that were not agreed to.
const FAILED: u8 = 1;
/// The exit status for arguments that make no sense.
const USAGE_ERROR: u8 = 2;

/// What `setup` reads from its environment.
#[derive(Clone, Debug)]
pub struct SetupEnv {
    /// The home directory, where the agents keep their settings.
    pub home: PathBuf,
    /// `PATH`, to tell whether agents find `benterm`.
    pub path: Option<OsString>,
    /// The `benterm` the link on the PATH points to: this one.
    pub exe: PathBuf,
    /// The skill directory the skill links point to, if one was found.
    pub skill: Option<PathBuf>,
}

impl SetupEnv {
    /// The environment of this process.
    pub fn from_process() -> Result<Self, String> {
        let home = std::env::var_os("HOME")
            .filter(|home| !home.is_empty())
            .map(PathBuf::from)
            .ok_or("HOME is not set")?;
        let exe = std::env::current_exe()
            .and_then(fs::canonicalize)
            .map_err(|error| format!("cannot tell where this benterm is: {error}"))?;
        let skill = skill_source(&exe);
        Ok(Self {
            home,
            path: std::env::var_os("PATH"),
            exe,
            skill,
        })
    }
}

/// Where the skill is for the binary at `exe`: in the Resources of
/// Benterm.app when `exe` is the app's binary, or else in the `skills`
/// directory of the checkout `exe` was built in.
///
/// The checkout is the nearest directory above `exe` that holds a
/// `Cargo.toml`, and the search ends there. A directory further up may
/// be one that others can write to, and a skill of theirs would be
/// linked for every agent. Nor is that directory taken for a checkout
/// unless its `Cargo.toml` is a workspace's and `exe` is in its
/// `target` folder, where cargo builds: a binary put alone in a shared
/// folder may be under a `Cargo.toml` someone else wrote.
pub fn skill_source(exe: &Path) -> Option<PathBuf> {
    let is_skill = |dir: &Path| dir.join("SKILL.md").is_file();
    let in_app = exe
        .parent()
        .and_then(Path::parent)
        .map(|contents| contents.join("Resources/skills/benterm"));
    if let Some(dir) = in_app.filter(|dir| is_skill(dir)) {
        return Some(dir);
    }
    exe.ancestors()
        .skip(1)
        .find(|dir| dir.join("Cargo.toml").is_file())
        .filter(|checkout| exe.starts_with(checkout.join("target")))
        .filter(|checkout| is_workspace_manifest(&checkout.join("Cargo.toml")))
        .map(|checkout| checkout.join("skills/benterm"))
        .filter(|dir| is_skill(dir))
}

/// Whether the manifest at `path` has a `[workspace]` table.
fn is_workspace_manifest(path: &Path) -> bool {
    fs::read_to_string(path)
        .is_ok_and(|manifest| manifest.lines().any(|line| line.trim() == "[workspace]"))
}

/// Runs `setup` with the arguments after it, and returns the exit status:
/// 0 when it did what was asked, 1 when it failed or the changes were
/// not agreed to, 2 when the arguments were wrong.
pub fn run(
    args: &[String],
    env: &SetupEnv,
    stdin: impl Read,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> u8 {
    let options = match Options::parse(args, env) {
        Ok(options) => options,
        Err(message) => {
            let _ = writeln!(stderr, "benterm: setup: {message}");
            return USAGE_ERROR;
        }
    };
    let setup = Setup {
        env,
        bin_dir: options.bin_dir,
    };
    let outcome = match options.command {
        Command::Help => write!(stdout, "{USAGE}").map(|()| 0),
        Command::Status => setup.status(stdout, stderr),
        Command::Install => setup.change(Direction::Install, options.yes, stdin, stdout, stderr),
        Command::Uninstall => {
            setup.change(Direction::Uninstall, options.yes, stdin, stdout, stderr)
        }
    };
    outcome.unwrap_or(FAILED)
}

enum Command {
    Help,
    Install,
    Status,
    Uninstall,
}

struct Options {
    command: Command,
    yes: bool,
    bin_dir: PathBuf,
}

impl Options {
    fn parse(args: &[String], env: &SetupEnv) -> Result<Self, String> {
        let mut command = None;
        let mut yes = false;
        let mut bin_dir = None;
        let mut args = args.iter();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--help" | "-h" => command = Some(Command::Help),
                "--yes" | "-y" => yes = true,
                "--bin-dir" => {
                    let dir = args.next().ok_or("--bin-dir needs a directory")?;
                    let dir = std::path::absolute(dir)
                        .map_err(|error| format!("--bin-dir {dir:?}: {error}"))?;
                    bin_dir = Some(dir);
                }
                word if command.is_none() && !word.starts_with('-') => {
                    command = Some(match word {
                        "install" => Command::Install,
                        "status" => Command::Status,
                        "uninstall" => Command::Uninstall,
                        _ => {
                            return Err(format!(
                                "unknown subcommand {word:?}: setup needs install, status or uninstall"
                            ));
                        }
                    });
                }
                other if other.starts_with('-') && other != "-" => {
                    return Err(format!("unknown option {other}"));
                }
                other => return Err(format!("unexpected argument {other:?}")),
            }
        }
        let command = command.ok_or("setup needs install, status or uninstall")?;
        if yes && matches!(command, Command::Status) {
            return Err("status takes no --yes: it changes nothing".into());
        }
        Ok(Self {
            command,
            yes,
            bin_dir: bin_dir.unwrap_or_else(|| env.home.join(".local/bin")),
        })
    }
}

/// A settings file that holds hooks of one agent.
struct HookFile {
    /// The agent's name, as people know it.
    agent_name: &'static str,
    /// The file, from the home directory.
    relative: &'static str,
    /// The agent, as `benterm hook --agent` takes it.
    agent: &'static str,
    events: &'static [&'static str],
}

const HOOK_FILES: [HookFile; 2] = [
    HookFile {
        agent_name: "Claude Code",
        relative: ".claude/settings.json",
        agent: "claude",
        events: &CLAUDE_HOOK_EVENTS,
    },
    HookFile {
        agent_name: "Codex",
        relative: ".codex/hooks.json",
        agent: "codex",
        events: &CODEX_HOOK_EVENTS,
    },
];

/// The command that tells the app of an event.
const HOOK: &str = "hook";
/// The command that checks the report of a turn. At the Stop it takes
/// the place of [`HOOK`] and tells the app of the Stop itself, once the
/// agent may stop: a Stop both were run for would be told before the
/// check has decided, and a blocked agent would show as done.
const REPORT_CHECK: &str = "report-check";

/// The commands install runs for an event of `file`, in the order of
/// its events: the event and the subcommand of `benterm`.
fn wanted(file: &HookFile) -> Vec<(&'static str, &'static str)> {
    let mut wanted = Vec::new();
    for event in file.events.iter().copied() {
        match event {
            "Stop" => wanted.push((event, REPORT_CHECK)),
            "PostToolUse" => wanted.extend([(event, HOOK), (event, REPORT_CHECK)]),
            _ => wanted.push((event, HOOK)),
        }
    }
    wanted
}

/// The command of the hooks install writes: `program`, the link to
/// `benterm` by its whole path, with the subcommand. A hook that named
/// only `benterm` would look it up on the PATH each time it runs, and a
/// directory a repository adds in front of the PATH could put another
/// program there, which would run with the token of the pane.
fn hook_command(program: &Path, subcommand: &str, agent: &str) -> String {
    format!(
        "{} {subcommand} --agent {agent}",
        shell_word(&program.to_string_lossy())
    )
}

/// The command install wrote before it named the link by its path.
fn earlier_hook_command(subcommand: &str, agent: &str) -> String {
    format!("benterm {subcommand} --agent {agent}")
}

/// `word` as a shell reads one word. A word of letters, digits and
/// `/ . _ - +` is written as it is, so that it also runs where a command
/// is split at its blanks and not read by a shell; any other is put
/// between single quotes.
fn shell_word(word: &str) -> String {
    let plain = |ch: char| ch.is_ascii_alphanumeric() || "/._-+".contains(ch);
    if !word.is_empty() && word.chars().all(plain) {
        return word.to_owned();
    }
    format!("'{}'", word.replace('\'', r"'\''"))
}

/// The words a shell makes of `command`, for the quoting a command of a
/// hook has: single and double quotes and the backslash. `None` when a
/// quote is not closed.
fn shell_words(command: &str) -> Option<Vec<String>> {
    let mut words = Vec::new();
    let mut word: Option<String> = None;
    let mut chars = command.chars();
    while let Some(ch) = chars.next() {
        match ch {
            '\'' => {
                let word = word.get_or_insert_with(String::new);
                loop {
                    match chars.next()? {
                        '\'' => break,
                        ch => word.push(ch),
                    }
                }
            }
            '"' => {
                let word = word.get_or_insert_with(String::new);
                loop {
                    match chars.next()? {
                        '"' => break,
                        '\\' => word.push(chars.next()?),
                        ch => word.push(ch),
                    }
                }
            }
            '\\' => word.get_or_insert_with(String::new).push(chars.next()?),
            ch if ch.is_whitespace() => words.extend(word.take()),
            ch => word.get_or_insert_with(String::new).push(ch),
        }
    }
    words.extend(word);
    Some(words)
}

/// The entry of an event's list that install adds.
fn hook_entry(program: &Path, subcommand: &str, agent: &str) -> Value {
    command_entry(hook_command(program, subcommand, agent))
}

/// The entry install added before it named the link by its path.
fn earlier_hook_entry(subcommand: &str, agent: &str) -> Value {
    command_entry(earlier_hook_command(subcommand, agent))
}

fn command_entry(command: String) -> Value {
    json!({"hooks": [{"type": "command", "command": command}]})
}

/// The program of `command` when it runs `benterm SUBCOMMAND` for
/// `agent`, from wherever `benterm` is: a hook written by hand counts as
/// well.
fn hook_program(command: &str, subcommand: &str, agent: &str) -> Option<String> {
    let words = shell_words(command)?;
    let [program, given, flag, named] = &words[..] else {
        return None;
    };
    (given == subcommand
        && flag == "--agent"
        && named == agent
        && Path::new(program).file_name() == Some("benterm".as_ref()))
    .then(|| program.clone())
}

/// Whether `command` runs `benterm SUBCOMMAND` for `agent`.
fn runs_hook(command: &str, subcommand: &str, agent: &str) -> bool {
    hook_program(command, subcommand, agent).is_some()
}

/// Whether `command` runs a hook of `agent` by the name `benterm` alone,
/// which is looked up on the PATH each time the hook runs, or by a
/// relative path such as `./benterm`, which is found in the folder the
/// agent runs in.
fn looks_up_benterm(command: &str, agent: &str) -> bool {
    [HOOK, REPORT_CHECK]
        .iter()
        .any(|subcommand| looked_up_hook(command, subcommand, agent))
}

/// Whether `command` runs `benterm SUBCOMMAND` for `agent` by a program
/// that is not a whole path.
fn looked_up_hook(command: &str, subcommand: &str, agent: &str) -> bool {
    hook_program(command, subcommand, agent).is_some_and(|program| !program.starts_with('/'))
}

/// Whether an entry of an event's list runs `benterm SUBCOMMAND` for
/// `agent`.
fn entry_runs_hook(entry: &Value, subcommand: &str, agent: &str) -> bool {
    entry
        .get("hooks")
        .and_then(Value::as_array)
        .is_some_and(|hooks| {
            hooks.iter().any(|hook| {
                hook.get("command")
                    .and_then(Value::as_str)
                    .is_some_and(|command| runs_hook(command, subcommand, agent))
            })
        })
}

/// A command an event misses, as status names it.
fn missing_name((event, subcommand): (&str, &str)) -> String {
    if subcommand == HOOK {
        event.to_owned()
    } else {
        format!("{event} ({subcommand})")
    }
}

/// A settings file as it was read.
struct Settings {
    path: PathBuf,
    /// The bytes of the file as it was read, or `None` when there is no file.
    raw: Option<String>,
    /// What the file holds, or `None` when there is no file.
    value: Option<Map<String, Value>>,
}

impl Settings {
    /// Reads the file, and fails, with the reason, when it is not a JSON
    /// object whose hooks are an object of lists.
    fn read(path: PathBuf, file: &HookFile, home: &Path) -> Result<Self, String> {
        let shown = show(&path, home);
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(Self {
                    path,
                    raw: None,
                    value: None,
                });
            }
            Err(error) => return Err(format!("{shown}: cannot read it: {error}")),
        };
        let value: Value = serde_json::from_str(&text)
            .map_err(|error| format!("{shown} is not JSON ({error}), so it is left as it is"))?;
        let Value::Object(value) = value else {
            return Err(format!(
                "{shown} is not a JSON object, so it is left as it is"
            ));
        };
        match value.get("hooks") {
            None => {}
            Some(Value::Object(hooks)) => {
                for event in file.events {
                    if hooks.get(*event).is_some_and(|list| !list.is_array()) {
                        return Err(format!(
                            "{shown}: hooks.{event} is not a list, so the file is left as it is"
                        ));
                    }
                }
            }
            Some(_) => {
                return Err(format!(
                    "{shown}: hooks is not an object, so the file is left as it is"
                ));
            }
        }
        Ok(Self {
            path,
            raw: Some(text),
            value: Some(value),
        })
    }

    /// The commands of `file` that no hook of their event runs.
    fn missing(&self, file: &HookFile) -> Vec<(&'static str, &'static str)> {
        let hooks = self
            .value
            .as_ref()
            .and_then(|value| value.get("hooks"))
            .and_then(Value::as_object);
        wanted(file)
            .into_iter()
            .filter(|(event, subcommand)| {
                !hooks
                    .and_then(|hooks| hooks.get(*event))
                    .and_then(Value::as_array)
                    .is_some_and(|list| {
                        list.iter()
                            .any(|entry| entry_runs_hook(entry, subcommand, file.agent))
                    })
            })
            .collect()
    }

    /// Whether the Stop still runs `benterm hook` in some form: beside
    /// the check, it tells the app of a Stop the check may yet block.
    fn stop_runs_hook(&self, file: &HookFile) -> bool {
        self.value
            .as_ref()
            .and_then(|value| value.get("hooks"))
            .and_then(|hooks| hooks.get("Stop"))
            .and_then(Value::as_array)
            .is_some_and(|list| {
                list.iter()
                    .any(|entry| entry_runs_hook(entry, HOOK, file.agent))
            })
    }

    /// How many hooks of `file` run `benterm` without its whole path, and
    /// so look it up on the PATH or in the folder the agent runs in.
    fn looked_up(&self, file: &HookFile) -> usize {
        let hooks = self
            .value
            .as_ref()
            .and_then(|value| value.get("hooks"))
            .and_then(Value::as_object);
        file.events
            .iter()
            .filter_map(|event| hooks?.get(*event)?.as_array())
            .flatten()
            .filter_map(|entry| entry.get("hooks")?.as_array())
            .flatten()
            .filter_map(|hook| hook.get("command")?.as_str())
            .filter(|command| looks_up_benterm(command, file.agent))
            .count()
    }

    /// What the file holds with the hooks it misses added, behind the
    /// ones there, and with `program` in the hooks that looked `benterm`
    /// up on the PATH; `None` when there is nothing to change. Where the
    /// check of the report is added to the Stop, the hook an earlier
    /// install wrote there goes.
    fn installed(&self, file: &HookFile, program: &Path) -> Option<Map<String, Value>> {
        let missing = self.missing(file);
        let earlier = earlier_hook_entry(HOOK, file.agent);
        let has_earlier_stop = self
            .value
            .as_ref()
            .and_then(|value| value.get("hooks"))
            .and_then(|hooks| hooks.get("Stop"))
            .and_then(Value::as_array)
            .is_some_and(|list| list.contains(&earlier));
        if missing.is_empty() && !has_earlier_stop && self.looked_up(file) == 0 {
            return None;
        }
        let mut value = self.value.clone().unwrap_or_default();
        let hooks = value
            .entry("hooks")
            .or_insert_with(|| Value::Object(Map::new()))
            .as_object_mut()?;
        // The hook an earlier install wrote for the Stop goes: only the
        // entry exactly as install wrote it, which is install's own.
        if let Some(list) = hooks.get_mut("Stop").and_then(Value::as_array_mut) {
            list.retain(|item| *item != earlier);
        }
        // A hook that names `benterm` without its whole path is given the
        // path of the link, and is otherwise left as it is.
        for event in file.events {
            let entries = hooks.get_mut(*event).and_then(Value::as_array_mut);
            let commands = entries
                .into_iter()
                .flatten()
                .filter_map(|entry| entry.get_mut("hooks")?.as_array_mut())
                .flatten()
                .filter_map(|hook| hook.get_mut("command"));
            for command in commands {
                let with_path = [HOOK, REPORT_CHECK].into_iter().find(|subcommand| {
                    command
                        .as_str()
                        .is_some_and(|command| looked_up_hook(command, subcommand, file.agent))
                });
                if let Some(subcommand) = with_path {
                    *command = Value::String(hook_command(program, subcommand, file.agent));
                }
            }
        }
        for (event, subcommand) in missing {
            hooks
                .entry(event)
                .or_insert_with(|| Value::Array(Vec::new()))
                .as_array_mut()?
                .push(hook_entry(program, subcommand, file.agent));
        }
        Some(value)
    }

    /// What the file holds without the hooks install writes, or wrote
    /// before the check of the report or before it named the link by
    /// its path; `None` when it has none. An event left with no hooks
    /// goes, and so do the hooks when no event is left.
    fn uninstalled(&self, file: &HookFile, program: &Path) -> Option<Map<String, Value>> {
        let mut value = self.value.clone()?;
        let entries = [
            hook_entry(program, HOOK, file.agent),
            hook_entry(program, REPORT_CHECK, file.agent),
            earlier_hook_entry(HOOK, file.agent),
            earlier_hook_entry(REPORT_CHECK, file.agent),
        ];
        let hooks = value.get_mut("hooks")?.as_object_mut()?;
        let mut removed = false;
        for event in file.events {
            let Some(list) = hooks.get_mut(*event).and_then(Value::as_array_mut) else {
                continue;
            };
            let before = list.len();
            list.retain(|item| !entries.contains(item));
            if list.len() < before {
                removed = true;
                if list.is_empty() {
                    hooks.shift_remove(*event);
                }
            }
        }
        if !removed {
            return None;
        }
        if hooks.is_empty() {
            value.shift_remove("hooks");
        }
        Some(value)
    }
}

/// The text a settings file is written with.
fn json_text(value: &Map<String, Value>) -> String {
    let mut text = serde_json::to_string_pretty(value).expect("a JSON map always serializes");
    text.push('\n');
    text
}

/// A symbolic link setup makes or takes out.
struct Link {
    /// What the link is for, as people know it.
    what: &'static str,
    path: PathBuf,
    target: PathBuf,
}

/// What is at the place of a link.
enum Found {
    Nothing,
    Link(PathBuf),
    /// A file or a directory that is not a link, which setup leaves alone.
    Other,
}

impl Link {
    fn found(&self) -> Found {
        match fs::symlink_metadata(&self.path) {
            Err(_) => Found::Nothing,
            Ok(metadata) if metadata.file_type().is_symlink() => fs::read_link(&self.path)
                .map(Found::Link)
                .unwrap_or(Found::Other),
            Ok(_) => Found::Other,
        }
    }
}

/// One change to make.
enum Change {
    /// Write a settings file.
    File {
        path: PathBuf,
        /// The bytes read when this change was built, or `None` when there was no file.
        raw: Option<String>,
        after: String,
        /// Whether this is the hooks file of Codex, which asks for new
        /// hooks to be reviewed.
        codex: bool,
    },
    /// Make the link point to the target, or take it out with `None`.
    Link {
        path: PathBuf,
        old: Option<PathBuf>,
        new: Option<PathBuf>,
    },
}

#[derive(Clone, Copy, PartialEq)]
enum Direction {
    Install,
    Uninstall,
}

struct Setup<'a> {
    env: &'a SetupEnv,
    bin_dir: PathBuf,
}

impl Setup<'_> {
    fn show(&self, path: &Path) -> String {
        show(path, &self.env.home)
    }

    fn hook_files(&self) -> impl Iterator<Item = (&'static HookFile, PathBuf)> + '_ {
        HOOK_FILES
            .iter()
            .map(|file| (file, self.env.home.join(file.relative)))
    }

    /// The link to this `benterm`, which the hooks run by its path.
    fn benterm_link(&self) -> PathBuf {
        self.bin_dir.join("benterm")
    }

    /// The links install makes. The skill links are missing when no skill
    /// was found.
    fn links(&self) -> Vec<Link> {
        let mut links = vec![Link {
            what: "benterm on the PATH",
            path: self.benterm_link(),
            target: self.env.exe.clone(),
        }];
        if let Some(skill) = &self.env.skill {
            for (what, dir) in [
                ("Claude Code skill", ".claude/skills"),
                ("Codex skill", ".codex/skills"),
            ] {
                links.push(Link {
                    what,
                    path: self.env.home.join(dir).join("benterm"),
                    target: skill.clone(),
                });
            }
        }
        links
    }

    fn bin_dir_on_path(&self) -> bool {
        let Some(path) = &self.env.path else {
            return false;
        };
        let real = fs::canonicalize(&self.bin_dir).ok();
        std::env::split_paths(path).any(|dir| {
            dir == self.bin_dir || (real.is_some() && fs::canonicalize(&dir).ok() == real)
        })
    }

    /// The first `benterm` on the PATH.
    fn benterm_on_path(&self) -> Option<PathBuf> {
        std::env::split_paths(self.env.path.as_ref()?)
            .map(|dir| dir.join("benterm"))
            .find(|candidate| candidate.is_file())
    }

    fn is_this_benterm(&self, path: &Path) -> bool {
        fs::canonicalize(path).ok() == fs::canonicalize(&self.env.exe).ok()
    }

    fn write_path_note(&self, stdout: &mut dyn Write) -> io::Result<()> {
        if self.bin_dir_on_path() {
            return Ok(());
        }
        let dir = match self.bin_dir.strip_prefix(&self.env.home) {
            Ok(relative) => format!("$HOME/{}", relative.display()),
            Err(_) => self.bin_dir.display().to_string(),
        };
        writeln!(
            stdout,
            "{} is not on your PATH, so agents do not find benterm there. \
             Add this line to the startup file of your shell (~/.zshrc for zsh) \
             and open a new shell:\n  export PATH=\"{dir}:$PATH\"",
            self.show(&self.bin_dir)
        )
    }

    fn status(&self, stdout: &mut dyn Write, stderr: &mut dyn Write) -> io::Result<u8> {
        let mut complete = true;
        let mut code = 0;
        for (file, path) in self.hook_files() {
            let line = match Settings::read(path.clone(), file, &self.env.home) {
                Err(reason) => {
                    writeln!(stderr, "benterm: setup: {reason}")?;
                    code = FAILED;
                    complete = false;
                    "cannot be read".to_string()
                }
                Ok(settings) => {
                    let missing = settings.missing(file);
                    if missing.is_empty() && settings.stop_runs_hook(file) {
                        complete = false;
                        format!(
                            "all {} events, but Stop also runs benterm hook, which tells \
                             the app of a Stop before its report is checked: take that hook \
                             out, or run setup install if setup wrote it",
                            file.events.len()
                        )
                    } else if missing.is_empty() && settings.looked_up(file) > 0 {
                        complete = false;
                        format!(
                            "all {} events, but {} of the hooks do not name benterm by \
                             its whole path, so the PATH or the folder the agent runs in \
                             picks the program that runs: run setup install to have them \
                             run {}",
                            file.events.len(),
                            settings.looked_up(file),
                            self.show(&self.benterm_link())
                        )
                    } else if missing.is_empty() {
                        format!("all {} events", file.events.len())
                    } else {
                        complete = false;
                        let names: Vec<String> = missing.into_iter().map(missing_name).collect();
                        format!("missing {}", names.join(", "))
                    }
                }
            };
            writeln!(
                stdout,
                "{} hooks ({}): {line}",
                file.agent_name,
                self.show(&path)
            )?;
        }

        let exe = self.env.exe.display();
        match self.benterm_on_path() {
            Some(found) if self.is_this_benterm(&found) => writeln!(
                stdout,
                "benterm on the PATH: {} is this benterm ({exe})",
                self.show(&found)
            )?,
            Some(found) => {
                complete = false;
                writeln!(
                    stdout,
                    "benterm on the PATH: {} is another one than this benterm ({exe})",
                    self.show(&found)
                )?;
            }
            None => {
                complete = false;
                writeln!(stdout, "benterm on the PATH: none")?;
            }
        }

        for link in self.links() {
            let state = match link.found() {
                Found::Link(target) if target == link.target => {
                    format!("links to {}", target.display())
                }
                Found::Link(target) => {
                    complete = false;
                    format!(
                        "links to {} instead of {}",
                        target.display(),
                        link.target.display()
                    )
                }
                Found::Nothing => {
                    complete = false;
                    "missing".to_string()
                }
                Found::Other => {
                    complete = false;
                    "is there but is not a link".to_string()
                }
            };
            writeln!(stdout, "{} ({}): {state}", link.what, self.show(&link.path))?;
        }
        if self.env.skill.is_none() {
            complete = false;
            writeln!(stdout, "skill: not found beside this benterm ({exe})")?;
        }

        if !self.bin_dir_on_path() {
            complete = false;
            self.write_path_note(stdout)?;
        }
        if complete {
            writeln!(stdout, "everything is set up.")?;
        }
        Ok(code)
    }

    /// Maximum automatic retries in `--yes` mode if a settings file keeps
    /// changing between read and write.
    const MAX_YES_RETRIES: usize = 3;

    fn plan(&self, direction: Direction) -> Result<(Vec<Change>, Vec<String>), String> {
        // Every file is read before anything is written, so a file that
        // cannot be read stops it all.
        let mut changes = Vec::new();
        for (file, path) in self.hook_files() {
            let settings = Settings::read(path, file, &self.env.home)?;
            let after = match direction {
                Direction::Install => settings.installed(file, &self.benterm_link()),
                Direction::Uninstall => settings.uninstalled(file, &self.benterm_link()),
            };
            if let Some(after) = after {
                changes.push(Change::File {
                    after: json_text(&after),
                    raw: settings.raw,
                    path: settings.path,
                    codex: file.agent == "codex",
                });
            }
        }

        // What is in the way of a link is left alone, and the rest is
        // done all the same.
        let mut problems = Vec::new();
        for link in self.links() {
            let found = link.found();
            let change = match (direction, found) {
                (Direction::Install, Found::Nothing) => Some((None, Some(link.target))),
                (Direction::Install, Found::Link(old)) if old != link.target => {
                    Some((Some(old), Some(link.target)))
                }
                (Direction::Install, Found::Other) => {
                    problems.push(format!(
                        "{} is there and is not a link, so it is left as it is; \
                         move it away for the {} to be linked",
                        self.show(&link.path),
                        link.what
                    ));
                    None
                }
                (Direction::Uninstall, Found::Link(old)) if old == link.target => {
                    Some((Some(old), None))
                }
                _ => None,
            };
            if let Some((old, new)) = change {
                changes.push(Change::Link {
                    path: link.path,
                    old,
                    new,
                });
            }
        }
        if direction == Direction::Install && self.env.skill.is_none() {
            problems.push(format!(
                "the skill is not beside this benterm ({}), so it is not linked",
                self.env.exe.display()
            ));
        }
        Ok((changes, problems))
    }

    fn change(
        &self,
        direction: Direction,
        yes: bool,
        stdin: impl Read,
        stdout: &mut dyn Write,
        stderr: &mut dyn Write,
    ) -> io::Result<u8> {
        let mut stdin = BufReader::new(stdin);
        let mut reported_problems = Vec::new();
        let mut yes_retries = 0;

        let (changes, problems) = loop {
            let (changes, problems) = match self.plan(direction) {
                Ok(planned) => planned,
                Err(reason) => {
                    writeln!(stderr, "benterm: setup: {reason}")?;
                    writeln!(stderr, "benterm: setup: nothing was written")?;
                    return Ok(FAILED);
                }
            };
            if problems != reported_problems {
                for problem in &problems {
                    writeln!(stderr, "benterm: setup: {problem}")?;
                }
                reported_problems = problems.clone();
            }

            if changes.is_empty() {
                writeln!(stdout, "Nothing to change.")?;
                break (changes, problems);
            }

            for change in &changes {
                self.write_change(change, stdout)?;
            }
            if !yes {
                write!(stdout, "Write these changes? [y/N] ")?;
                stdout.flush()?;
                let mut answer = String::new();
                // No answer is a no.
                let _ = stdin.read_line(&mut answer);
                writeln!(stdout)?;
                if !matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
                    writeln!(stdout, "Nothing was written.")?;
                    return Ok(FAILED);
                }
            }

            let changed = changed_files(&changes);
            if !changed.is_empty() {
                if yes {
                    yes_retries += 1;
                    if yes_retries > Self::MAX_YES_RETRIES {
                        writeln!(
                            stderr,
                            "benterm: setup: {} kept changing; nothing was written",
                            self.show(changed[0])
                        )?;
                        return Ok(FAILED);
                    }
                }
                for path in changed {
                    writeln!(
                        stdout,
                        "{} changed since it was read; showing the new changes.",
                        self.show(path)
                    )?;
                }
                continue;
            }

            for change in &changes {
                if let Err(error) = self.apply(change, stdout) {
                    writeln!(stderr, "benterm: setup: {error}")?;
                    return Ok(FAILED);
                }
            }
            break (changes, problems);
        };

        if direction == Direction::Install {
            self.write_path_note(stdout)?;
            let codex_hooks = changes
                .iter()
                .any(|change| matches!(change, Change::File { codex: true, .. }));
            if codex_hooks {
                writeln!(
                    stdout,
                    "Codex runs new hooks only after you review them: \
                     open Codex and type /hooks."
                )?;
            }
        }
        Ok(if problems.is_empty() { 0 } else { FAILED })
    }

    fn write_change(&self, change: &Change, stdout: &mut dyn Write) -> io::Result<()> {
        match change {
            Change::File {
                path, raw, after, ..
            } => {
                writeln!(stdout, "{}:", self.show(path))?;
                write!(
                    stdout,
                    "{}",
                    diff(raw.as_deref().unwrap_or_default(), after)
                )?;
            }
            Change::Link { path, old, new } => {
                let path = self.show(path);
                match (old, new) {
                    (None, Some(new)) => writeln!(stdout, "{path}:\n+ link to {}", new.display())?,
                    (Some(old), Some(new)) => writeln!(
                        stdout,
                        "{path}:\n- link to {}\n+ link to {}",
                        old.display(),
                        new.display()
                    )?,
                    (Some(old), None) => writeln!(stdout, "{path}:\n- link to {}", old.display())?,
                    (None, None) => {}
                }
            }
        }
        Ok(())
    }

    fn apply(&self, change: &Change, stdout: &mut dyn Write) -> io::Result<()> {
        match change {
            Change::File {
                path, raw, after, ..
            } => {
                let exists = raw.is_some();
                // A settings file that is a link, as in a repository of
                // dotfiles, stays one: the file it points to is written.
                let real = if exists {
                    fs::canonicalize(path)?
                } else {
                    path.clone()
                };
                if exists {
                    let copy = backup(&real)?;
                    writeln!(stdout, "Copied {} to {}", self.show(path), self.show(&copy))?;
                }
                write_replacing(&real, after.as_bytes())
            }
            Change::Link { path, old, new } => {
                if old.is_some() {
                    fs::remove_file(path)?;
                }
                if let Some(new) = new {
                    if let Some(parent) = path.parent() {
                        fs::create_dir_all(parent)?;
                    }
                    std::os::unix::fs::symlink(new, path)?;
                }
                Ok(())
            }
        }
    }
}

/// The raw bytes of `path`, or `None` when there is no file.
fn read_raw(path: &Path) -> io::Result<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// The settings files in `changes` whose contents on disk no longer match
/// what was read when the changes were built.
fn changed_files(changes: &[Change]) -> Vec<&Path> {
    changes
        .iter()
        .filter_map(|change| match change {
            Change::File { path, raw, .. } => match read_raw(path) {
                Ok(current) if current.as_deref() == raw.as_deref().map(str::as_bytes) => None,
                _ => Some(path.as_path()),
            },
            Change::Link { .. } => None,
        })
        .collect()
}

/// `path` with the home directory written as `~`.
fn show(path: &Path, home: &Path) -> String {
    match path.strip_prefix(home) {
        Ok(relative) => format!("~/{}", relative.display()),
        Err(_) => path.display().to_string(),
    }
}

/// Copies the file at `path` beside itself, with the local date and time
/// in the name, and returns the copy.
fn backup(path: &Path) -> io::Result<PathBuf> {
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::other("a file has a name"))?
        .to_string_lossy()
        .into_owned();
    let stamp = local_time_stamp();
    let mut copy = path.with_file_name(format!("{name}.benterm-{stamp}"));
    let mut n = 1;
    while fs::symlink_metadata(&copy).is_ok() {
        copy = path.with_file_name(format!("{name}.benterm-{stamp}-{n}"));
        n += 1;
    }
    fs::copy(path, &copy)?;
    Ok(copy)
}

/// The local time as `YYYYMMDD-HHMMSS`.
fn local_time_stamp() -> String {
    // SAFETY: time and localtime_r only write to the values given them.
    unsafe {
        let now = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&now, &mut tm).is_null() {
            return now.to_string();
        }
        format!(
            "{:04}{:02}{:02}-{:02}{:02}{:02}",
            tm.tm_year + 1900,
            tm.tm_mon + 1,
            tm.tm_mday,
            tm.tm_hour,
            tm.tm_min,
            tm.tm_sec
        )
    }
}

/// Writes `path` as a whole: to a file beside it first, which then takes
/// its place, so that it is never half written. The file keeps its
/// permissions, and a new one is its owner's alone.
///
/// The file beside it is made here, under a name that cannot be told in
/// advance, and for its owner alone from the start: nothing put at the
/// name before is written to or through, and the contents are at no
/// time open to others.
fn write_replacing(path: &Path, contents: &[u8]) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("a file has a directory"))?;
    fs::create_dir_all(parent)?;
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let (mut file, temporary) = create_beside(parent, &name)?;
    let written = (|| {
        file.write_all(contents)?;
        if let Ok(metadata) = fs::metadata(path) {
            file.set_permissions(metadata.permissions())?;
        }
        file.sync_all()?;
        fs::rename(&temporary, path)
    })();
    if written.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    written
}

/// Makes a new file in `parent` for the contents of the file `name`, and
/// returns it with its path. The file is the owner's alone (0600).
fn create_beside(parent: &Path, name: &str) -> io::Result<(fs::File, PathBuf)> {
    use std::hash::{BuildHasher, Hasher};
    use std::os::unix::fs::OpenOptionsExt;
    // A name that is taken is not opened: another one is tried.
    for _ in 0..16 {
        // The keys of a new hasher come from the system's random numbers.
        let random = std::collections::hash_map::RandomState::new()
            .build_hasher()
            .finish();
        let temporary = parent.join(format!(".{name}.benterm-{random:016x}"));
        let made = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&temporary);
        match made {
            Ok(file) => return Ok((file, temporary)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        format!(
            "no free name for a file beside {name} in {}",
            parent.display()
        ),
    ))
}

/// The lines that differ between `before` and `after`, `-` for a line
/// taken out and `+` for a line put in, each group after the line above
/// it.
fn diff(before: &str, after: &str) -> String {
    let old: Vec<&str> = before.lines().collect();
    let new: Vec<&str> = after.lines().collect();
    // The longest common subsequence, from the ends. Settings files are a
    // few hundred lines at most.
    let mut common = vec![vec![0u32; new.len() + 1]; old.len() + 1];
    for i in (0..old.len()).rev() {
        for j in (0..new.len()).rev() {
            common[i][j] = if old[i] == new[j] {
                common[i + 1][j + 1] + 1
            } else {
                common[i + 1][j].max(common[i][j + 1])
            };
        }
    }
    let mut lines: Vec<(char, &str)> = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < old.len() || j < new.len() {
        if i < old.len() && j < new.len() && old[i] == new[j] {
            lines.push((' ', old[i]));
            i += 1;
            j += 1;
        } else if j < new.len() && (i == old.len() || common[i][j + 1] >= common[i + 1][j]) {
            lines.push(('+', new[j]));
            j += 1;
        } else {
            lines.push(('-', old[i]));
            i += 1;
        }
    }
    let mut out = String::new();
    for (index, (mark, line)) in lines.iter().enumerate() {
        let next_changes = lines.get(index + 1).is_some_and(|(mark, _)| *mark != ' ');
        if *mark != ' ' || next_changes {
            out.push_str(&format!("{mark} {line}\n"));
        }
    }
    out
}

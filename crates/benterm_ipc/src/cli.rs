//! The command line of the socket API: what `benterm <command>` does.
//!
//! [`run`] takes the arguments after the program name and does all the
//! work, so the `benterm` binary only has to call it when it is given
//! arguments.

use benterm_agent::PlanUpdate;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use serde_json::Value;

use crate::client::{Client, ClientError};
use crate::protocol::{
    AgentActivity, AgentTarget, CloseTask, CreateTask, CreateWorkspace, ErrorCode, KillAgent,
    ListAgents, ListPanes, ListTasks, Method, OpenBrowser, PaneId, ReadAgent, ReadAgentHistory,
    ReadOutput, ReportHook, ReportStatus, ReportToParent, SendKeys, SendText, SendToAgent,
    ShowAgent, SpawnAgent, SplitDirection, SplitPane, WaitForAgents, WaitingFor,
};
use crate::setup::{self, SetupEnv};
use crate::socket::{CONTROL_TOKEN_ENV, MAILBOX_DIR_ENV, PANE_ID_ENV, PANE_TOKEN_ENV, SOCKET_ENV};

const USAGE: &str = "\
usage: benterm [--socket PATH] [--timeout SECONDS] <command>

Without arguments, benterm opens the app.

commands:
  list [--json]
      Print a line for each pane with a terminal, which a browser pane has
      not, and for each terminal behind the window:
      its id, where it is (WORKSPACE:TAB, or background), the name it was
      started under or -, the agent in it or -, its state (working,
      waiting, done, error or unknown) and its working directory or -.
      With --json, print one JSON object on a line with the fields of
      pane.list: pane, workspace, tab, name, cwd, agent, state and reason.
  split [--pane ID] [--right | --down]
      Split a pane and print the id of the new pane. Without --pane, split
      the pane this command runs in. The default direction is --right.
  browse [--pane ID] [--right | --down] URL
      Split a pane, show the web page at URL in the new pane and print the
      id of the new pane. URL starts with http:// or https://. Without
      --pane, split the pane this command runs in, or outside a pane the
      one in use. The default direction is --right.
  new-workspace [--name NAME] [--cwd DIR]
      Open a workspace and print its id and the id of its first pane. Its
      panes start in DIR, which may be relative to where the command runs.
  send --pane ID [--enter] [--] [TEXT...]
      Type TEXT into a pane. Several TEXT arguments are joined with spaces.
      Without TEXT, type what is read from standard input. --enter presses
      Enter afterwards.
  read --pane ID [--lines N]
      Print the screen of a pane, or with --lines the last N lines of its
      scrollback and screen.
  history [--since MS]
      Print the changes of the agents' states that the app has recorded,
      in all panes, oldest first, one JSON object on a line: the time in
      milliseconds since 1970 (at_ms), the pane's key, which it keeps
      across restarts of the app, the pane's id, the agent, the state
      (working, waiting, done, error or unknown) and what a waiting agent
      waits for. With --since, only the records after that time.
  hook [EVENT] --agent claude|codex
      Report a hook event of the agent in this pane; made to be the command
      of a Claude Code or Codex hook. The hook's JSON is read from standard
      input, and EVENT is taken from it when left out. It always exits 0
      and prints nothing on standard output, and outside a Benterm pane it
      does nothing at all, so it never stands in the agent's way.
  report-check --agent claude|codex
      Check the report of the turn that ends; made to be the command of a
      Stop hook and a PostToolUse hook of Claude Code or Codex, beside
      hook. A turn that edited many files or called many tools has to
      have written a report, a JSON file in .benterm/ of its working
      directory. At a Stop, a missing report or one that does not follow
      the rules holds the agent, with what to fix on standard output; a
      report that passes goes to the app. After a tool call, the form of
      the report is handed over once the turn has come to need one. It
      always exits 0. Outside a Benterm pane, with BENTERM_REPORT_CHECK=off
      or with enabled = false in [report] of the settings it does nothing.
  agent run --name NAME [--cwd DIR] [--outlive-parent] [--allow-peers] [--max-peer-rounds N] [--control] [--] [COMMAND [ARG...]]
      Start COMMAND, or a shell, in a terminal behind the window, as the
      agent NAME, and print its name and the id of its terminal. It starts
      where this command runs, or in DIR, and finds programs on this
      command's PATH. It is a child of the pane this command runs in: it
      ends when that pane closes, unless --outlive-parent is given. With
      --allow-peers, it may exchange messages directly with siblings under
      the same parent that also opted in, up to --max-peer-rounds N rounds
      (10 by default). The agent may report its hooks and to its parent
      but not start or direct agents of its own, unless --control gives
      it the whole API, as the panes of the window have. Every agent ends
      with the app. Options of COMMAND go after --.
  agent list [--children | --siblings] [--json]
      Print a line for each agent: name, terminal id, parent pane or -,
      background or tab, state, and outlive-parent if it was started so.
      --children lists the agents this pane started. --siblings lists the
      other agents started by the same parent with --allow-peers. An agent at work
      ends its line with changed_at_ms=MS, the last change of its screen
      or of the files of its directory, in milliseconds since 1970, and
      with stalled before it when nothing has changed for longer than
      after_seconds of the [stall] settings. An agent whose report had a
      status ends its line with report=STATUS, after all of these. An
      agent that is waiting or in error and told a msg with OSC 7501
      ends its line with msg=TEXT. With --json, print one JSON object
      on a line with the fields of agent.list: name, pane, parent,
      background, outlive_parent, state, report_status, for an agent at
      work stalled and changed_at_ms, and for one waiting or in error
      msg when set.
  agent send AGENT [--] [TEXT...]
      Give an agent a message: paste TEXT, or standard input, and press
      Enter. AGENT is a name or a terminal id.
  agent read AGENT [--lines N]
      Print the screen of an agent, as read does for a pane.
  agent keys AGENT KEY...
      Press keys in the terminal of an agent, as they are pressed in its
      pane: escape, enter, tab, backspace, space, up, down, left, right,
      home, end, pageup, pagedown, insert, delete, f1 to f20, or a
      character, after ctrl-, alt- or shift- as in ctrl-c. agent keys
      AGENT escape interrupts Claude Code and Codex. A name that is no
      key sends none of the keys. Text goes with agent send.
  agent kill AGENT
      Close the terminal of an agent and hang its program up, in a tab or
      behind the window. Its children end with it, unless started with
      --outlive-parent. A wait for it ends with the agent closed.
  agent wait AGENT | --children [--until STATES] [--wait SECONDS] [--settle SECONDS]
      Wait until the agent, or every agent this pane started, has stopped
      working, and print a line for each: name and state. STATES are the
      states to wait for, separated by commas: done, waiting, error (the
      default is these three), working, unknown. A state counts once it has lasted
      --settle seconds, 1 by default. An agent that was given a message
      with agent send is waited for until the turn the message starts
      has ended. An agent whose terminal closed is printed as closed. After --wait seconds, 180 by default, the command
      gives up with exit status 3; the agents are as they were.
  agent report [--status pass|issues|blocked] [--] [TEXT...]
      Tell the pane that started this agent TEXT, or standard input. It
      arrives there as input, between <benterm from=NAME pane=ID> and
      </benterm>. While the user has a draft there it is held back, and
      the pane is marked, until the draft is sent off or erased.
      --status says what the work came to: pass when the result can be
      taken as it is, issues when it has problems the report names,
      blocked when the agent cannot go on without help. The text then
      starts with PASS:, ISSUES: or BLOCKED:, and agent list, agent show
      and the session list show the status until a report with another
      one comes; a report without --status leaves it as it is.
  agent show AGENT [--select]
      Open the terminal of an agent as a tab, behind the tab on view, or
      with --select on view, and print its id, followed by what agent list
      ends the agent's line with: the stall words of an agent at work, and
      report=STATUS.
  role run ROLE [--name NAME] [--cwd DIR] [--outlive-parent] [--control] [--] [TASK...]
      Start an agent of ROLE as agent run does, named NAME or the name of
      the role. Its first message is the role's instructions, a blank
      line, and TASK.
  role list
      Print a line for each role: name, agent and description. A file of
      the roles directory that is not a role is named on standard error.
  task create NAME [--branch BRANCH] [--role ROLE] [--cwd DIR] [--] [WORK...]
      Make the task NAME: a branch and a git worktree of its own, and a
      workspace in the worktree. The branch starts at the commit that the
      checkout this command runs in, or the one DIR is in, is on. It is
      named NAME unless --branch names it, as issue-93-kanban. With --role
      an agent of ROLE runs in the workspace's first pane, and its first
      message is the role's instructions, a blank line, and WORK; without,
      a shell. Print the task's number, its workspace, its first pane and
      its worktree. A branch or a worktree that exists is never used:
      nothing is made, and the reason is given.
  task list
      Print the tasks, closed ones too, oldest first, one JSON object on a
      line: id, name, repository (the main checkout), base_branch,
      base_commit (where the task's branch started), branch, worktree,
      role, created_at_ms, closed (at_ms, and worktree: kept or removed)
      and workspace, the id of the workspace that shows the task now.
  task close TASK [--remove [--force]]
      Close the task with the number TASK, and its workspace. The worktree
      and the branch stay unless --remove is given. --remove is refused,
      with the numbers, when files in the worktree have changes that are
      in no commit or the branch has commits that are on no other branch;
      --force removes them all the same. Files that git ignores are not
      counted and go with the worktree. A task closed with its worktree
      kept can be closed again with --remove.
  setup install [--yes] [--bin-dir DIR]
  setup status [--bin-dir DIR]
  setup uninstall [--yes] [--bin-dir DIR]
      Put in, show or take out what Benterm needs from Claude Code and
      Codex: the hooks that run benterm hook and benterm
      report-check, benterm on the PATH
      (a link in DIR, ~/.local/bin by default) and the benterm skill.
      setup goes first and does not need the app; benterm setup --help
      says more.

A role is a TOML file in the roles directory, roles/ beside the config
file: $XDG_CONFIG_HOME/benterm/roles, or ~/.config/benterm/roles. It
has a name, an agent (claude or codex), a prompt with the instructions,
and may have a description, a model and a permission_mode, which is
claude's --permission-mode or codex's --sandbox. args is a list of more
arguments for the agent, put after its model and mode and before the
prompt. An [env] table gives the agent variables, put over the ones every
pane gets.

The socket is taken from --socket, or from BENTERM_SOCKET, which is set in
every pane. BENTERM_PANE_ID is the id of the pane a shell runs in.

A command waits for the app for --timeout seconds, 10 by default, and then
gives up with exit status 124. The app may have carried the command out
before the time ran out, but does not start it after. A send that timed
out may have typed its text, so sending it again may type it twice: read
the pane first. The same goes for agent send and agent report. agent wait
gives the app --timeout seconds to start waiting, and --wait on top.
";

const COMMANDS: [&str; 13] = [
    "list",
    "split",
    "browse",
    "new-workspace",
    "send",
    "read",
    "history",
    "hook",
    "report-check",
    "agent",
    "role",
    "task",
    "setup",
];

/// The exit status for a command that failed.
const FAILED: u8 = 1;
/// The exit status for arguments that make no sense.
const USAGE_ERROR: u8 = 2;
/// The exit status when the app did not answer in time, the same as
/// timeout(1) uses.
const TIMED_OUT: u8 = 124;

/// The exit status of `agent wait` when its time ran out. Not 124: the
/// app answered, and what was waited for has not happened yet.
const WAIT_RAN_OUT: u8 = 3;

/// How long `agent wait` waits without --wait. Long enough for most
/// turns, and short enough that an agent that waits looks up now and
/// then; hc waits as long.
const DEFAULT_WAIT: Duration = Duration::from_secs(180);

/// How long a command waits for the app without --timeout. The app
/// answers in well under a second unless its window has stopped.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

/// How long `hook` takes at most, whatever the agent and the app do. The
/// agent waits for its hooks, so this is time taken from the agent.
const HOOK_BUDGET: Duration = Duration::from_millis(1000);
/// How long `hook` waits for the agent to finish writing the hook's input
/// before it reports the event without it.
const HOOK_INPUT_WAIT: Duration = Duration::from_millis(300);
/// The most `hook` reads of its input. The input holds prompts and tool
/// output, of which only a few short fields are used.
const HOOK_INPUT_LIMIT: u64 = 4 << 20;
/// The most `hook` passes on of a plan, as the length of the tool's input
/// and answer in JSON. A plan is a few short lines; one longer than this
/// is not passed on, and the event is reported without it.
const PLAN_LIMIT: usize = 64 << 10;

/// The parts of the environment the command line reads.
#[derive(Clone, Default)]
pub struct CliEnv {
    /// `BENTERM_SOCKET`. Empty in the panes of an app that started
    /// without its socket API.
    pub socket: Option<OsString>,
    /// `BENTERM_PANE_ID`
    pub pane: Option<String>,
    /// The directory the command runs in, which a relative `--cwd` is
    /// taken from.
    pub cwd: Option<PathBuf>,
    /// `PATH`, which an agent started from here finds its programs on.
    pub path: Option<OsString>,
    /// `BENTERM_REPORT_CHECK`: `off` turns the check of reports off
    /// for the terminal.
    pub report_check: Option<String>,
    /// Where the commands keep their state: the counts of `report-check`
    /// go in a folder of it.
    pub state_dir: Option<PathBuf>,
    /// The settings file.
    pub config_file: Option<PathBuf>,
    /// `BENTERM_MAILBOX_DIR`: file-based mailbox directory used as a fallback
    /// when the socket cannot be reached (e.g. inside a sandbox).
    pub mailbox_dir: Option<PathBuf>,
    /// `BENTERM_PANE_TOKEN`: opaque credential for own-pane hooks and reports.
    pub pane_token: Option<String>,
    /// `BENTERM_CONTROL_TOKEN`: opaque credential for full control operations.
    pub control_token: Option<String>,
}

impl fmt::Debug for CliEnv {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CliEnv")
            .field("socket", &self.socket)
            .field("pane", &self.pane)
            .field("cwd", &self.cwd)
            .field("path", &self.path)
            .field("report_check", &self.report_check)
            .field("state_dir", &self.state_dir)
            .field("config_file", &self.config_file)
            .field("mailbox_dir", &self.mailbox_dir)
            .field(
                "pane_token",
                &self.pane_token.as_ref().map(|_| "<redacted>"),
            )
            .field(
                "control_token",
                &self.control_token.as_ref().map(|_| "<redacted>"),
            )
            .finish()
    }
}

impl CliEnv {
    /// The environment of this process.
    pub fn from_process() -> Self {
        Self {
            // Empty is not the same as unset: it is how an app tells its
            // panes that it has no socket.
            socket: std::env::var_os(SOCKET_ENV),
            pane: std::env::var_os(PANE_ID_ENV)
                .filter(|pane| !pane.is_empty())
                .map(|pane| pane.to_string_lossy().into_owned()),
            cwd: std::env::current_dir().ok(),
            path: std::env::var_os("PATH"),
            report_check: std::env::var(crate::REPORT_CHECK_ENV).ok(),
            state_dir: crate::socket::state_dir(
                std::env::var_os("XDG_STATE_HOME").as_deref(),
                std::env::var_os("HOME").as_deref(),
            ),
            config_file: benterm_config::config_path(),
            mailbox_dir: std::env::var_os(MAILBOX_DIR_ENV)
                .filter(|dir| !dir.is_empty())
                .map(PathBuf::from),
            pane_token: std::env::var_os(PANE_TOKEN_ENV)
                .filter(|token| !token.is_empty())
                .map(|token| token.to_string_lossy().into_owned()),
            control_token: std::env::var_os(CONTROL_TOKEN_ENV)
                .filter(|token| !token.is_empty())
                .map(|token| token.to_string_lossy().into_owned()),
        }
    }

    /// The credential to attach to outgoing IPC requests, preferring
    /// `BENTERM_CONTROL_TOKEN` over `BENTERM_PANE_TOKEN` when both are set.
    pub fn token(&self) -> Option<&str> {
        self.control_token
            .as_deref()
            .filter(|token| !token.is_empty())
            .or_else(|| self.pane_token.as_deref().filter(|token| !token.is_empty()))
    }
}

/// Whether `name` is one of the commands [`run`] understands.
pub fn is_command(name: &str) -> bool {
    COMMANDS.contains(&name)
}

enum Failure {
    Usage(String),
    Failed(String),
    TimedOut(String),
    WaitRanOut(String),
}

impl Failure {
    fn into_message(self) -> String {
        match self {
            Self::Usage(message)
            | Self::Failed(message)
            | Self::TimedOut(message)
            | Self::WaitRanOut(message) => message,
        }
    }
}

fn usage(message: impl Into<String>) -> Failure {
    Failure::Usage(message.into())
}

/// Runs one command and returns the exit status: 0 on success, 1 when the
/// command failed, 2 when the arguments were wrong, 124 when the app did
/// not answer in time. `args` are the
/// arguments after the program name.
///
/// `hook` and `report-check` are the exceptions: they return 0 whatever
/// happens, because an agent takes a failing hook for a reason to stop
/// or to complain.
pub fn run(
    args: &[String],
    env: &CliEnv,
    stdin: impl Read + Send + 'static,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> u8 {
    let roles = benterm_config::roles_dir();
    run_with_roles(args, env, roles.as_deref(), stdin, stdout, stderr)
}

/// [`run`] with the roles of `roles`, the roles directory, instead of the
/// one of this environment.
pub fn run_with_roles(
    args: &[String],
    env: &CliEnv,
    roles: Option<&Path>,
    mut stdin: impl Read + Send + 'static,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> u8 {
    match hook_command(args) {
        Some(HookCommand::Hook) => return run_hook(args, env, stdin, stderr),
        Some(HookCommand::ReportCheck) => {
            return crate::report_check::run(args, env, stdin, stdout, stderr);
        }
        None => {}
    }
    if args.first().is_some_and(|first| first == "setup") {
        return match SetupEnv::from_process() {
            Ok(setup_env) => setup::run(&args[1..], &setup_env, stdin, stdout, stderr),
            Err(message) => {
                let _ = writeln!(stderr, "benterm: setup: {message}");
                FAILED
            }
        };
    }
    let outcome = Arguments::parse(args).and_then(|arguments| {
        if arguments.help {
            return Ok(USAGE.to_string());
        }
        if arguments.command.is_none() {
            return Err(usage(USAGE.trim_end()));
        }
        execute(arguments, env, roles, &mut stdin, stderr)
    });
    match outcome {
        Ok(output) => match stdout.write_all(output.as_bytes()) {
            Ok(()) => 0,
            Err(_) => FAILED,
        },
        Err(Failure::Usage(message)) => {
            // Nothing useful can be done when stderr itself is broken.
            let _ = writeln!(stderr, "benterm: {message}");
            USAGE_ERROR
        }
        Err(Failure::Failed(message)) => {
            let _ = writeln!(stderr, "benterm: {message}");
            FAILED
        }
        Err(Failure::TimedOut(message)) => {
            let _ = writeln!(stderr, "benterm: {message}");
            TIMED_OUT
        }
        Err(Failure::WaitRanOut(message)) => {
            let _ = writeln!(stderr, "benterm: {message}");
            WAIT_RAN_OUT
        }
    }
}

/// The options that take a value, as [`Arguments::parse`] reads them.
const VALUE_OPTIONS: [&str; 15] = [
    "--socket",
    "--timeout",
    "--pane",
    "--name",
    "--cwd",
    "--lines",
    "--agent",
    "--since",
    "--until",
    "--wait",
    "--settle",
    "--branch",
    "--role",
    "--status",
    "--max-peer-rounds",
];

/// Parses the arguments of `report-check`: `--agent`, plus the global
/// `--socket` and `--timeout` options.
pub(crate) fn parse_report_check(
    args: &[String],
) -> Result<(Option<String>, Option<String>), String> {
    let arguments = Arguments::parse(args).map_err(Failure::into_message)?;
    arguments
        .allow("report-check", &["--agent"], false)
        .map_err(Failure::into_message)?;
    Ok((arguments.agent, arguments.socket))
}

/// A command an agent runs as a hook, which never fails.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HookCommand {
    Hook,
    ReportCheck,
}

/// Whether the command is one an agent runs as a hook, `hook` or
/// `report-check`: the first argument that is neither an option nor the
/// value of one. This is told before the arguments are checked, so that
/// wrong arguments to a hook, even ones that are not UTF-8, do not fail
/// either.
pub fn is_hook<S: AsRef<std::ffi::OsStr>>(args: &[S]) -> bool {
    hook_command(args).is_some()
}

fn hook_command<S: AsRef<std::ffi::OsStr>>(args: &[S]) -> Option<HookCommand> {
    let mut args = args.iter().map(AsRef::as_ref);
    while let Some(arg) = args.next() {
        if arg == "--" {
            break;
        }
        if VALUE_OPTIONS.iter().any(|option| arg == *option) {
            // The value is not the command, whatever it reads.
            args.next();
        } else if !arg.as_encoded_bytes().starts_with(b"-") || arg == "-" {
            return if arg == "hook" {
                Some(HookCommand::Hook)
            } else if arg == "report-check" {
                Some(HookCommand::ReportCheck)
            } else {
                None
            };
        }
    }
    None
}

/// `hook`: reports one hook event and returns 0.
///
/// Outside a pane it does nothing, not even read its input: the same hook
/// settings serve agents that run in other terminals. In a pane, what
/// went wrong is written to stderr, which the agent does not show for a
/// hook that exits with 0.
fn run_hook(
    args: &[String],
    env: &CliEnv,
    stdin: impl Read + Send + 'static,
    stderr: &mut dyn Write,
) -> u8 {
    // An empty socket variable is an app that has no socket API: there is
    // nobody to tell either.
    if env.socket.as_ref().is_none_or(|socket| socket.is_empty()) || env.pane.is_none() {
        return 0;
    }
    if let Err(
        Failure::Usage(message)
        | Failure::Failed(message)
        | Failure::TimedOut(message)
        | Failure::WaitRanOut(message),
    ) = hook(args, env, stdin)
    {
        let _ = writeln!(stderr, "benterm: hook: {message}");
    }
    0
}

fn hook(args: &[String], env: &CliEnv, stdin: impl Read + Send + 'static) -> Result<(), Failure> {
    let arguments = Arguments::parse(args)?;
    arguments.allow("hook", &["--agent"], true)?;
    // The first word is the command itself only when options came first.
    let event = match &arguments.words[..] {
        [] => None,
        [event] => Some(event.clone()),
        [_, extra, ..] => return Err(usage(format!("unexpected argument {extra:?}"))),
    };
    let agent = arguments
        .agent
        .clone()
        .ok_or_else(|| usage("hook needs --agent claude or --agent codex"))?;
    let pane = callers_pane(env)?;
    let socket = socket_path(arguments.socket.as_deref(), env)?;
    let token = env.token().map(str::to_owned);

    // Everything that can wait on somebody else, the agent closing the
    // input or the app answering, happens on a thread this one gives up on.
    let (done, outcome) = mpsc::channel();
    let report = move || {
        let input = hook_input(stdin);
        let field = |name: &str| {
            input
                .as_ref()
                .and_then(|input| input.get(name))
                .and_then(Value::as_str)
                .map(str::to_owned)
        };
        let event = event.or_else(|| field("hook_event_name"));
        let (tool_input, tool_response) = input
            .as_ref()
            .zip(event.as_deref())
            .and_then(|(input, event)| plan_call(&agent, event, input))
            .unwrap_or_default();
        let outcome = match event {
            Some(event) => Client::connect(&socket)
                .map(|client| client.with_token(token))
                .map_err(|error| format!("cannot connect to the app: {error}"))
                .and_then(|mut client| {
                    // The app is given less time than this command
                    // waits, so it does not take a hook the command has
                    // given up on.
                    client
                        .call_within(
                            &ReportHook {
                                pane,
                                agent,
                                event,
                                tool_name: field("tool_name"),
                                notification_type: field("notification_type"),
                                session_id: field("session_id"),
                                agent_id: field("agent_id"),
                                tool_input,
                                tool_response,
                            },
                            HOOK_BUDGET,
                        )
                        .map_err(|error| error.to_string())
                }),
            None => Err("no event: name one, or pass the hook's JSON on standard input".into()),
        };
        let _ = done.send(outcome);
    };
    thread::Builder::new()
        .name("benterm-hook".into())
        .spawn(report)
        .map_err(|error| Failure::Failed(error.to_string()))?;
    match outcome.recv_timeout(HOOK_BUDGET) {
        Ok(outcome) => outcome.map_err(Failure::Failed),
        // Out of time. The agent matters more than this one event, and
        // the screen tells the state too.
        Err(_) => Ok(()),
    }
}

/// What a tool the agent keeps its plan with was given and what it
/// answered, from the input of the hook: `None` unless the app would
/// read a plan from them. The inputs of all other tools stay here, and
/// so do the plan of a subagent, which the app leaves out, and a plan
/// longer than [`PLAN_LIMIT`].
fn plan_call(agent: &str, event: &str, input: &Value) -> Option<(Option<Value>, Option<Value>)> {
    if input.get("agent_id").is_some_and(|id| !id.is_null()) {
        return None;
    }
    let tool = input.get("tool_name").and_then(Value::as_str);
    let tool_input = input.get("tool_input");
    let tool_response = input.get("tool_response");
    PlanUpdate::from_hook(agent, event, tool, tool_input, tool_response)?;
    let length = |value: Option<&Value>| value.map_or(0, |value| value.to_string().len());
    (length(tool_input) + length(tool_response) <= PLAN_LIMIT)
        .then(|| (tool_input.cloned(), tool_response.cloned()))
}

/// The hook's input as a JSON object, if the agent wrote one and finished
/// in time.
fn hook_input(stdin: impl Read + Send + 'static) -> Option<Value> {
    let (read, input) = mpsc::channel();
    thread::Builder::new()
        .name("benterm-hook-input".into())
        .spawn(move || {
            let mut text = String::new();
            if stdin
                .take(HOOK_INPUT_LIMIT)
                .read_to_string(&mut text)
                .is_ok()
            {
                let _ = read.send(text);
            }
        })
        .ok()?;
    let text = input.recv_timeout(HOOK_INPUT_WAIT).ok()?;
    serde_json::from_str(&text).ok().filter(Value::is_object)
}

/// The arguments, sorted into what they are but not yet checked against
/// the command.
#[derive(Default)]
struct Arguments {
    help: bool,
    command: Option<String>,
    /// The options that were given, to tell the ones a command has no use
    /// for.
    given: Vec<&'static str>,
    socket: Option<String>,
    timeout: Option<Duration>,
    pane: Option<String>,
    direction: Option<(&'static str, SplitDirection)>,
    name: Option<String>,
    cwd: Option<String>,
    lines: Option<String>,
    agent: Option<String>,
    until: Option<String>,
    wait: Option<String>,
    settle: Option<String>,
    since: Option<String>,
    branch: Option<String>,
    role: Option<String>,
    status: Option<String>,
    max_peer_rounds: Option<String>,
    enter: bool,
    words: Vec<String>,
}

impl Arguments {
    fn parse(args: &[String]) -> Result<Self, Failure> {
        let mut parsed = Self::default();
        let mut args = args.iter();
        let mut options_ended = false;

        while let Some(arg) = args.next() {
            if options_ended || !arg.starts_with('-') || arg == "-" {
                if parsed.command.is_none() && !options_ended {
                    parsed.command = Some(arg.clone());
                } else {
                    parsed.words.push(arg.clone());
                }
                continue;
            }
            let mut value = |option: &str| {
                args.next()
                    .cloned()
                    .ok_or_else(|| usage(format!("{option} needs a value")))
            };
            let option = match arg.as_str() {
                "--" => {
                    options_ended = true;
                    continue;
                }
                "--help" | "-h" => {
                    parsed.help = true;
                    continue;
                }
                "--socket" => {
                    parsed.socket = Some(value("--socket")?);
                    continue;
                }
                "--timeout" => {
                    parsed.timeout = Some(parse_timeout(&value("--timeout")?)?);
                    continue;
                }
                "--pane" => {
                    parsed.pane = Some(value("--pane")?);
                    "--pane"
                }
                "--name" => {
                    parsed.name = Some(value("--name")?);
                    "--name"
                }
                "--cwd" => {
                    parsed.cwd = Some(value("--cwd")?);
                    "--cwd"
                }
                "--lines" => {
                    parsed.lines = Some(value("--lines")?);
                    "--lines"
                }
                "--agent" => {
                    parsed.agent = Some(value("--agent")?);
                    "--agent"
                }
                "--until" => {
                    parsed.until = Some(value("--until")?);
                    "--until"
                }
                "--wait" => {
                    parsed.wait = Some(value("--wait")?);
                    "--wait"
                }
                "--settle" => {
                    parsed.settle = Some(value("--settle")?);
                    "--settle"
                }
                "--since" => {
                    parsed.since = Some(value("--since")?);
                    "--since"
                }
                "--branch" => {
                    parsed.branch = Some(value("--branch")?);
                    "--branch"
                }
                "--role" => {
                    parsed.role = Some(value("--role")?);
                    "--role"
                }
                "--status" => {
                    parsed.status = Some(value("--status")?);
                    "--status"
                }
                "--max-peer-rounds" => {
                    parsed.max_peer_rounds = Some(value("--max-peer-rounds")?);
                    "--max-peer-rounds"
                }
                "--enter" => {
                    parsed.enter = true;
                    "--enter"
                }
                "--remove" => "--remove",
                "--force" => "--force",
                "--outlive-parent" => "--outlive-parent",
                "--allow-peers" => "--allow-peers",
                "--control" => "--control",
                "--children" => "--children",
                "--siblings" => "--siblings",
                "--select" => "--select",
                "--json" => "--json",
                "--right" => parsed.direct("--right", SplitDirection::Right)?,
                "--down" => parsed.direct("--down", SplitDirection::Down)?,
                unknown => return Err(usage(format!("unknown option {unknown}"))),
            };
            parsed.given.push(option);
        }

        parsed.help |= parsed.command.as_deref() == Some("help");
        Ok(parsed)
    }

    fn direct(
        &mut self,
        option: &'static str,
        direction: SplitDirection,
    ) -> Result<&'static str, Failure> {
        match self.direction {
            Some((other, _)) if other != option => {
                Err(usage(format!("{option} cannot be combined with {other}")))
            }
            _ => {
                self.direction = Some((option, direction));
                Ok(option)
            }
        }
    }

    /// Rejects options and extra arguments the command has no use for.
    fn allow(&self, command: &str, options: &[&str], words: bool) -> Result<(), Failure> {
        if let Some(option) = self.given.iter().find(|option| !options.contains(option)) {
            return Err(usage(format!("{option} does not apply to {command}")));
        }
        match self.words.first() {
            Some(word) if !words => Err(usage(format!("unexpected argument {word:?}"))),
            _ => Ok(()),
        }
    }

    fn has(&self, option: &str) -> bool {
        self.given.contains(&option)
    }

    fn pane(&self) -> Result<Option<PaneId>, Failure> {
        self.pane
            .as_ref()
            .map(|pane| {
                pane.parse()
                    .map_err(|_| usage(format!("{pane:?} is not a pane id")))
            })
            .transpose()
    }

    fn lines(&self) -> Result<Option<u32>, Failure> {
        self.lines
            .as_ref()
            .map(|lines| {
                lines
                    .parse()
                    .map_err(|_| usage(format!("{lines:?} is not a number of lines")))
            })
            .transpose()
    }

    fn max_peer_rounds(&self) -> Result<Option<u32>, Failure> {
        self.max_peer_rounds
            .as_ref()
            .map(|rounds| {
                rounds
                    .parse::<u32>()
                    .ok()
                    .filter(|rounds| *rounds >= 1)
                    .ok_or_else(|| {
                        usage(format!(
                            "--max-peer-rounds needs a positive integer, not {rounds:?}"
                        ))
                    })
            })
            .transpose()
    }

    fn required_pane(&self, command: &str) -> Result<PaneId, Failure> {
        self.pane()?
            .ok_or_else(|| usage(format!("{command} needs --pane ID")))
    }
}

fn parse_timeout(value: &str) -> Result<Duration, Failure> {
    value
        .parse::<f64>()
        .ok()
        .filter(|seconds| *seconds > 0.0)
        .and_then(|seconds| Duration::try_from_secs_f64(seconds).ok())
        .ok_or_else(|| {
            usage(format!(
                "--timeout needs a positive number of seconds, not {value:?}"
            ))
        })
}

fn execute(
    arguments: Arguments,
    env: &CliEnv,
    roles: Option<&Path>,
    stdin: &mut dyn Read,
    stderr: &mut dyn Write,
) -> Result<String, Failure> {
    // Everything about the arguments is checked before anything is sent.
    let command = arguments.command.as_deref().unwrap_or_default();
    let timeout = arguments.timeout.unwrap_or(DEFAULT_TIMEOUT);
    let socket =
        || connect(arguments.socket.as_deref(), env).map(|client| Connection { client, timeout });
    match command {
        "split" => {
            arguments.allow(command, &["--pane", "--right", "--down"], false)?;
            let pane = match arguments.pane()? {
                Some(pane) => pane,
                None => callers_pane(env)?,
            };
            let direction = arguments
                .direction
                .map_or(SplitDirection::Right, |(_, direction)| direction);
            let created = call(socket()?.call(&SplitPane { pane, direction }))?;
            Ok(format!("{}\n", created.pane))
        }
        "browse" => {
            arguments.allow(command, &["--pane", "--right", "--down"], true)?;
            let url = match &arguments.words[..] {
                [url] => url.clone(),
                [] => return Err(usage("browse needs URL")),
                [_, extra, ..] => return Err(usage(format!("unexpected argument {extra:?}"))),
            };
            let pane = match arguments.pane()? {
                Some(pane) => Some(pane),
                None => asking_pane(arguments.socket.as_deref(), env)?,
            };
            let direction = arguments
                .direction
                .map_or(SplitDirection::Right, |(_, direction)| direction);
            let request = OpenBrowser {
                url,
                pane,
                direction,
            };
            let created = call(socket()?.call(&request))?;
            Ok(format!("{}\n", created.pane))
        }
        "new-workspace" => {
            arguments.allow(command, &["--name", "--cwd"], false)?;
            let cwd = arguments
                .cwd
                .as_deref()
                .map(|cwd| absolute(cwd, env))
                .transpose()?;
            let request = CreateWorkspace {
                name: arguments.name.clone(),
                cwd,
                caller: asking_pane(arguments.socket.as_deref(), env)?,
            };
            let created = call(socket()?.call(&request))?;
            Ok(format!("{} {}\n", created.workspace, created.pane))
        }
        "send" => {
            arguments.allow(command, &["--pane", "--enter"], true)?;
            let pane = arguments.required_pane(command)?;
            let mut text = if arguments.words.is_empty() {
                let mut text = String::new();
                stdin.read_to_string(&mut text).map_err(|error| {
                    Failure::Failed(format!("cannot read standard input: {error}"))
                })?;
                text
            } else {
                arguments.words.join(" ")
            };
            if arguments.enter {
                text.push('\r');
            }
            call(socket()?.call(&SendText { pane, text }))?;
            Ok(String::new())
        }
        "list" => {
            arguments.allow(command, &["--json"], false)?;
            let list = call(socket()?.call(&ListPanes {}))?;
            if arguments.has("--json") {
                return json_lines(&list.panes);
            }
            let mut lines = String::new();
            for pane in list.panes {
                let place = match (pane.workspace, pane.tab) {
                    (Some(workspace), Some(tab)) => format!("{workspace}:{tab}"),
                    _ => "background".to_owned(),
                };
                let or_dash = |text: Option<String>| text.unwrap_or_else(|| "-".to_owned());
                lines.push_str(&format!(
                    "{} {place} {} {} {} {}\n",
                    pane.pane,
                    or_dash(pane.name),
                    or_dash(pane.agent),
                    activity_name(pane.state),
                    or_dash(pane.cwd),
                ));
            }
            Ok(lines)
        }
        "read" => {
            arguments.allow(command, &["--pane", "--lines"], false)?;
            let pane = arguments.required_pane(command)?;
            let lines = arguments.lines()?;
            let mut text = call(socket()?.call(&ReadOutput { pane, lines }))?.text;
            if !text.ends_with('\n') {
                text.push('\n');
            }
            Ok(text)
        }
        "agent" => agent(&arguments, env, roles, stdin, timeout),
        "role" => role(&arguments, env, roles, timeout, stderr),
        "task" => task(&arguments, env, timeout, stderr),
        "history" => {
            arguments.allow(command, &["--since"], false)?;
            let since_ms = arguments
                .since
                .as_ref()
                .map(|since| {
                    since.parse().map_err(|_| {
                        usage(format!(
                            "--since needs a time in milliseconds since 1970, not {since:?}"
                        ))
                    })
                })
                .transpose()?;
            let history = call(socket()?.call(&ReadAgentHistory { since_ms }))?;
            json_lines(&history.records)
        }
        // `setup` is told before the arguments are read, when it comes
        // first; options of the socket mean nothing to it.
        "setup" => Err(usage(
            "setup goes first: benterm setup install|status|uninstall",
        )),
        unknown => Err(usage(format!(
            "unknown command {unknown:?}; see benterm --help"
        ))),
    }
}

/// The request of `agent run` and `role run`: `command` started where the
/// command runs or in `--cwd`, with its `PATH` and then `vars`, as a child
/// of its pane.
fn spawn_request(
    arguments: &Arguments,
    env: &CliEnv,
    name: String,
    command: Vec<String>,
    vars: &[(String, String)],
) -> Result<SpawnAgent, Failure> {
    let cwd = match arguments.cwd.as_deref() {
        Some(cwd) => Some(absolute(cwd, env)?),
        None => env
            .cwd
            .as_ref()
            .and_then(|cwd| cwd.to_str())
            .map(str::to_owned),
    };
    let path = env.path.as_ref().and_then(|path| path.to_str());
    Ok(SpawnAgent {
        name,
        command,
        cwd,
        env: path
            .map(|path| ("PATH".to_owned(), path.to_owned()))
            .into_iter()
            .chain(vars.iter().cloned())
            .collect(),
        parent: env.pane.as_ref().map(|_| callers_pane(env)).transpose()?,
        outlive_parent: arguments.has("--outlive-parent"),
        first_message_line: None,
        shell_after: false,
        allow_peers: arguments.has("--allow-peers"),
        max_peer_rounds: arguments.max_peer_rounds()?,
        control: arguments.has("--control"),
    })
}

/// `role SUBCOMMAND ...`: agents started from the role files of `roles`.
fn role(
    arguments: &Arguments,
    env: &CliEnv,
    roles: Option<&Path>,
    timeout: Duration,
    stderr: &mut dyn Write,
) -> Result<String, Failure> {
    let Some((subcommand, words)) = arguments.words.split_first() else {
        return Err(usage("role needs a subcommand: run or list"));
    };
    let allow = |options: &[&str]| match arguments.given.iter().find(|o| !options.contains(o)) {
        Some(option) => Err(usage(format!(
            "{option} does not apply to role {subcommand}"
        ))),
        None => Ok(()),
    };
    let loaded = || match roles {
        Some(dir) => Ok(benterm_config::load_roles(dir)),
        None => Err(Failure::Failed(
            "no roles directory: neither XDG_CONFIG_HOME nor HOME is an absolute path".into(),
        )),
    };
    match subcommand.as_str() {
        "run" => {
            allow(&["--name", "--cwd", "--outlive-parent", "--control"])?;
            let Some((wanted, task)) = words.split_first() else {
                return Err(usage("role run needs ROLE: the name of a role"));
            };
            let loaded = loaded()?;
            let Some(role) = loaded.roles.iter().find(|role| &role.name == wanted) else {
                let mut message = format!("there is no role {wanted:?}");
                for warning in &loaded.warnings {
                    message.push_str(&format!("\n  {warning}"));
                }
                return Err(Failure::Failed(message));
            };
            let name = arguments.name.clone().unwrap_or_else(|| role.name.clone());
            let mut command = role.command(&task.join(" "));
            benterm_config::fit_codex_launch(&mut command, env.path.as_deref());
            let request = SpawnAgent {
                shell_after: true,
                ..spawn_request(arguments, env, name, command, &role.env)?
            };
            let client = connect(arguments.socket.as_deref(), env)?;
            let spawned = call(Connection { client, timeout }.call(&request))?;
            Ok(format!("{} {}\n", spawned.name, spawned.pane))
        }
        "list" => {
            allow(&[])?;
            if let Some(word) = words.first() {
                return Err(usage(format!("unexpected argument {word:?}")));
            }
            let loaded = loaded()?;
            let mut lines = String::new();
            for role in &loaded.roles {
                let line = [role.name.as_str(), role.agent.program(), &role.description].join(" ");
                lines.push_str(line.trim_end());
                lines.push('\n');
            }
            // The roles that are there are printed all the same.
            for warning in &loaded.warnings {
                let _ = writeln!(stderr, "benterm: {warning}");
            }
            Ok(lines)
        }
        unknown => Err(usage(format!(
            "unknown subcommand role {unknown}; role takes run or list"
        ))),
    }
}

/// `agent SUBCOMMAND ...`: the first word is the subcommand. `roles` is
/// the roles directory, beside which the rules for workers are.
fn agent(
    arguments: &Arguments,
    env: &CliEnv,
    roles: Option<&Path>,
    stdin: &mut dyn Read,
    timeout: Duration,
) -> Result<String, Failure> {
    const SUBCOMMANDS: &str = "run, list, send, keys, read, wait, report, show or kill";
    let Some((subcommand, words)) = arguments.words.split_first() else {
        return Err(usage(format!("agent needs a subcommand: {SUBCOMMANDS}")));
    };
    let command = format!("agent {subcommand}");
    // Rejects options the subcommand has no use for, and more words than
    // it takes.
    let allow = |options: &[&str], takes: usize| {
        if let Some(option) = arguments.given.iter().find(|o| !options.contains(o)) {
            return Err(usage(format!("{option} does not apply to {command}")));
        }
        match words.get(takes) {
            Some(word) => Err(usage(format!("unexpected argument {word:?}"))),
            None => Ok(()),
        }
    };
    let target = || {
        let Some(target) = words.first() else {
            return Err(usage(format!(
                "{command} needs AGENT: a name or a terminal id"
            )));
        };
        let Ok(target) = target.parse::<AgentTarget>();
        Ok(target)
    };
    let socket =
        || connect(arguments.socket.as_deref(), env).map(|client| Connection { client, timeout });
    // The text of a message: the words, or standard input without any.
    let text = |words: &[String], stdin: &mut dyn Read| {
        if !words.is_empty() {
            return Ok(words.join(" "));
        }
        let mut text = String::new();
        stdin
            .read_to_string(&mut text)
            .map_err(|error| Failure::Failed(format!("cannot read standard input: {error}")))?;
        Ok(text)
    };

    match subcommand.as_str() {
        "run" => {
            allow(
                &[
                    "--name",
                    "--cwd",
                    "--outlive-parent",
                    "--allow-peers",
                    "--max-peer-rounds",
                    "--control",
                ],
                usize::MAX,
            )?;
            let name = arguments
                .name
                .clone()
                .ok_or_else(|| usage("agent run needs --name NAME"))?;
            let mut request = spawn_request(arguments, env, name, words.to_vec(), &[])?;
            // A shell would take the line for a command, and a parent that
            // put the line in the task has given it already.
            if !request.command.is_empty() {
                request.first_message_line = roles
                    .and_then(benterm_config::worker_rules_file)
                    .as_deref()
                    .and_then(benterm_config::worker_rules_line)
                    .filter(|line| !request.command.iter().any(|arg| arg.contains(line)));
            }
            let spawned = call(socket()?.call(&request))?;
            Ok(format!("{} {}\n", spawned.name, spawned.pane))
        }
        "list" => {
            allow(&["--children", "--siblings", "--json"], 0)?;
            if arguments.has("--children") && arguments.has("--siblings") {
                return Err(usage("--children and --siblings cannot be combined"));
            }
            let parent = match arguments.has("--children") {
                true => Some(callers_pane(env)?),
                false => None,
            };
            let siblings_of = match arguments.has("--siblings") {
                true => Some(callers_pane(env)?),
                false => None,
            };
            let list = call(socket()?.call(&ListAgents {
                parent,
                siblings_of,
            }))?;
            if arguments.has("--json") {
                return json_lines(&list.agents);
            }
            let mut lines = String::new();
            for agent in list.agents {
                let parent = agent
                    .parent
                    .map_or_else(|| "-".to_owned(), |parent| parent.to_string());
                let place = if agent.background {
                    "background"
                } else {
                    "tab"
                };
                let outlive = if agent.outlive_parent {
                    " outlive-parent"
                } else {
                    ""
                };
                let peers = if agent.allow_peers {
                    " allow-peers"
                } else {
                    ""
                };
                lines.push_str(&format!(
                    "{} {} {parent} {place} {}{outlive}{peers}{}{}{}\n",
                    agent.name,
                    agent.pane,
                    activity_name(agent.state),
                    stall_words(agent.stalled, agent.changed_at_ms),
                    report_word(agent.report_status),
                    msg_word(agent.msg.as_deref()),
                ));
            }
            Ok(lines)
        }
        "send" => {
            allow(&[], usize::MAX)?;
            let agent = target()?;
            let text = text(&words[1..], stdin)?;
            let caller = asking_pane(arguments.socket.as_deref(), env)?;
            match socket() {
                Ok(connection) => {
                    let request = SendToAgent {
                        agent,
                        text,
                        caller,
                    };
                    call(connection.call(&request))?;
                    Ok(String::new())
                }
                Err(connect_err) => {
                    if let Some(mailbox_dir) = env.mailbox_dir.as_deref() {
                        let to = match &agent {
                            AgentTarget::Name(name) => name.clone(),
                            AgentTarget::Pane(pane) => pane.to_string(),
                        };
                        write_mailbox_outbox(mailbox_dir, &to, &text, None)?;
                        Ok(String::new())
                    } else {
                        Err(connect_err)
                    }
                }
            }
        }
        "keys" => {
            allow(&[], usize::MAX)?;
            let agent = target()?;
            if words.len() < 2 {
                return Err(usage(
                    "agent keys needs AGENT and at least one KEY, as in escape",
                ));
            }
            let keys = words[1..].to_vec();
            call(socket()?.call(&SendKeys { agent, keys }))?;
            Ok(String::new())
        }
        "kill" => {
            allow(&[], 1)?;
            let agent = target()?;
            call(socket()?.call(&KillAgent { agent }))?;
            Ok(String::new())
        }
        "read" => {
            allow(&["--lines"], 1)?;
            let agent = target()?;
            let lines = arguments.lines()?;
            let mut text = call(socket()?.call(&ReadAgent { agent, lines }))?.text;
            if !text.ends_with('\n') {
                text.push('\n');
            }
            Ok(text)
        }
        "wait" => {
            allow(&["--children", "--until", "--wait", "--settle"], 1)?;
            let (agent, children_of) = match (words.first(), arguments.has("--children")) {
                (Some(_), false) => (Some(target()?), None),
                (None, true) => (None, Some(callers_pane(env)?)),
                _ => return Err(usage("agent wait needs AGENT or --children, not both")),
            };
            let until = arguments
                .until
                .as_deref()
                .map(parse_states)
                .transpose()?
                .unwrap_or_default();
            let wait = match &arguments.wait {
                Some(wait) => parse_seconds("--wait", wait, false)?,
                None => DEFAULT_WAIT,
            };
            let settle = arguments
                .settle
                .as_ref()
                .map(|settle| parse_seconds("--settle", settle, true))
                .transpose()?;
            let request = WaitForAgents {
                agent,
                children_of,
                until,
                wait_ms: Some(millis(wait)),
                settle_ms: settle.map(millis),
            };
            let mut connection = socket()?;
            let waited = connection
                .client
                .call_long(&request, timeout, wait)
                .map_err(|error| match error {
                    ClientError::Api(error) if error.code == ErrorCode::WaitTimedOut => {
                        Failure::WaitRanOut(error.message)
                    }
                    error => failure(error),
                })?;
            let mut lines = String::new();
            for agent in waited.agents {
                let name = agent.name.unwrap_or_else(|| agent.pane.to_string());
                let state = match (agent.closed, agent.reason) {
                    (true, _) => "closed".to_owned(),
                    (false, Some(WaitingFor::Permission)) => "waiting permission".to_owned(),
                    (false, Some(WaitingFor::Question)) => "waiting question".to_owned(),
                    (false, None) => activity_name(agent.state).to_owned(),
                };
                lines.push_str(&format!("{name} {state}\n"));
            }
            Ok(lines)
        }
        "report" => {
            allow(&["--status"], usize::MAX)?;
            let status = arguments
                .status
                .as_deref()
                .map(|word| {
                    word.parse::<ReportStatus>()
                        .map_err(|e| usage(e.to_string()))
                })
                .transpose()?;
            let from = if env.pane.is_some() || env.mailbox_dir.is_none() {
                Some(callers_pane(env)?)
            } else {
                None
            };
            let text = text(words, stdin)?;
            match socket() {
                Ok(connection) => {
                    let from = match from {
                        Some(pane) => pane,
                        None => callers_pane(env)?,
                    };
                    call(connection.call(&ReportToParent { from, text, status }))?;
                    Ok(String::new())
                }
                Err(connect_err) => {
                    if let Some(mailbox_dir) = env.mailbox_dir.as_deref() {
                        write_mailbox_outbox(mailbox_dir, "parent", &text, status)?;
                        Ok(String::new())
                    } else {
                        Err(connect_err)
                    }
                }
            }
        }
        "show" => {
            allow(&["--select"], 1)?;
            let request = ShowAgent {
                agent: target()?,
                select: arguments.has("--select"),
            };
            let shown = call(socket()?.call(&request))?;
            let stall = stall_words(shown.stalled, shown.changed_at_ms);
            let report = report_word(shown.report_status);
            Ok(format!("{}{stall}{report}\n", shown.pane))
        }
        unknown => Err(usage(format!(
            "unknown subcommand {unknown:?}: agent needs {SUBCOMMANDS}"
        ))),
    }
}

/// The status of an agent's last report as `agent list` and `agent show`
/// end their line with it: nothing for none.
fn report_word(status: Option<ReportStatus>) -> String {
    status.map_or_else(String::new, |status| format!(" report={status}"))
}

/// What an agent in `waiting` or `error` told of itself with OSC 7501
/// (`msg`), as `agent list` ends its line with it: one line without
/// control characters, or nothing for none.
fn msg_word(msg: Option<&str>) -> String {
    let Some(msg) = msg else {
        return String::new();
    };
    let words: Vec<String> = msg
        .split(char::is_whitespace)
        .map(|word| word.chars().filter(|ch| !ch.is_control()).collect())
        .filter(|word: &String| !word.is_empty())
        .collect();
    if words.is_empty() {
        String::new()
    } else {
        format!(" msg={}", words.join(" "))
    }
}

/// What `agent list` and `agent show` add for an agent at work: ` stalled`
/// when it is marked, and the time of its last change.
fn stall_words(stalled: bool, changed_at_ms: Option<u64>) -> String {
    let mut words = String::new();
    if stalled {
        words.push_str(" stalled");
    }
    if let Some(at_ms) = changed_at_ms {
        words.push_str(&format!(" changed_at_ms={at_ms}"));
    }
    words
}

/// `task SUBCOMMAND ...`: pieces of work with a worktree of their own.
fn task(
    arguments: &Arguments,
    env: &CliEnv,
    timeout: Duration,
    stderr: &mut dyn Write,
) -> Result<String, Failure> {
    let Some((subcommand, words)) = arguments.words.split_first() else {
        return Err(usage("task needs a subcommand: create, list or close"));
    };
    let allow = |options: &[&str]| match arguments.given.iter().find(|o| !options.contains(o)) {
        Some(option) => Err(usage(format!(
            "{option} does not apply to task {subcommand}"
        ))),
        None => Ok(()),
    };
    let socket =
        || connect(arguments.socket.as_deref(), env).map(|client| Connection { client, timeout });
    match subcommand.as_str() {
        "create" => {
            allow(&["--branch", "--role", "--cwd"])?;
            let Some((name, work)) = words.split_first() else {
                return Err(usage("task create needs NAME: the name of the task"));
            };
            let cwd = match arguments.cwd.as_deref() {
                Some(cwd) => absolute(cwd, env)?,
                None => absolute(".", env).map(|cwd| cwd.trim_end_matches("/.").to_owned())?,
            };
            let request = CreateTask {
                name: name.clone(),
                branch: arguments.branch.clone(),
                cwd,
                role: arguments.role.clone(),
                work: (!work.is_empty()).then(|| work.join(" ")),
                caller: asking_pane(arguments.socket.as_deref(), env)?,
            };
            let created = call(socket()?.call(&request))?;
            let workspace = created
                .task
                .workspace
                .map_or_else(|| "-".to_owned(), |workspace| workspace.to_string());
            Ok(format!(
                "{} {workspace} {} {}\n",
                created.task.id, created.pane, created.task.worktree
            ))
        }
        "list" => {
            allow(&[])?;
            if let Some(word) = words.first() {
                return Err(usage(format!("unexpected argument {word:?}")));
            }
            let list = call(socket()?.call(&ListTasks {}))?;
            json_lines(&list.tasks)
        }
        "close" => {
            allow(&["--remove", "--force"])?;
            let task = match words {
                [] => return Err(usage("task close needs TASK: the number of a task")),
                [task] => task.parse::<u64>().map_err(|_| {
                    usage(format!(
                        "{task:?} is not the number of a task; see benterm task list"
                    ))
                })?,
                [_, extra, ..] => return Err(usage(format!("unexpected argument {extra:?}"))),
            };
            let (remove, force) = (arguments.has("--remove"), arguments.has("--force"));
            if force && !remove {
                return Err(usage("--force goes with --remove"));
            }
            let request = CloseTask {
                task,
                remove,
                force,
            };
            let closed = socket()?.call(&request).map_err(|error| match error {
                ClientError::Api(error) if error.code == ErrorCode::AtRisk => Failure::Failed(
                    format!("{}; give --force to remove it all the same", error.message),
                ),
                error => failure(error),
            })?;
            if let Some(why) = closed.branch_left {
                let _ = writeln!(stderr, "benterm: {why}");
            }
            Ok(String::new())
        }
        unknown => Err(usage(format!(
            "unknown subcommand {unknown:?}: task needs create, list or close"
        ))),
    }
}

/// One JSON object on a line, ending in a newline; empty for an empty list.
fn json_lines<T: serde::Serialize>(items: &[T]) -> Result<String, Failure> {
    let mut text = String::new();
    for item in items {
        let line =
            serde_json::to_string(item).map_err(|error| Failure::Failed(error.to_string()))?;
        text.push_str(&line);
        text.push('\n');
    }
    Ok(text)
}

fn activity_name(state: AgentActivity) -> &'static str {
    match state {
        AgentActivity::Working => "working",
        AgentActivity::Waiting => "waiting",
        AgentActivity::Done => "done",
        AgentActivity::Error => "error",
        AgentActivity::Unknown => "unknown",
    }
}

fn parse_states(states: &str) -> Result<Vec<AgentActivity>, Failure> {
    states
        .split(',')
        .map(|state| match state.trim() {
            "working" => Ok(AgentActivity::Working),
            "waiting" => Ok(AgentActivity::Waiting),
            "done" => Ok(AgentActivity::Done),
            "error" => Ok(AgentActivity::Error),
            "unknown" => Ok(AgentActivity::Unknown),
            other => Err(usage(format!(
                "--until takes working, waiting, done, error or unknown, not {other:?}"
            ))),
        })
        .collect()
}

fn parse_seconds(option: &str, value: &str, zero: bool) -> Result<Duration, Failure> {
    value
        .parse::<f64>()
        .ok()
        .filter(|seconds| *seconds > 0.0 || (zero && *seconds == 0.0))
        .and_then(|seconds| Duration::try_from_secs_f64(seconds).ok())
        .ok_or_else(|| {
            let kind = if zero {
                "a number"
            } else {
                "a positive number"
            };
            usage(format!("{option} needs {kind} of seconds, not {value:?}"))
        })
}

fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

/// The app does not run where the command does, so it is sent an
/// absolute path.
fn absolute(cwd: &str, env: &CliEnv) -> Result<String, Failure> {
    let path = Path::new(cwd);
    if path.is_absolute() {
        return Ok(cwd.to_owned());
    }
    let base = env.cwd.as_ref().ok_or_else(|| {
        Failure::Failed(format!(
            "cannot tell the current directory; give an absolute path for --cwd, not {cwd:?}"
        ))
    })?;
    base.join(path)
        .into_os_string()
        .into_string()
        .map_err(|_| Failure::Failed("the current directory is not UTF-8".into()))
}

fn callers_pane(env: &CliEnv) -> Result<PaneId, Failure> {
    let pane = env.pane.as_ref().ok_or_else(|| {
        usage(format!(
            "outside a pane, {PANE_ID_ENV} is not set; give --pane ID"
        ))
    })?;
    pane.parse()
        .map_err(|_| usage(format!("{PANE_ID_ENV} is not a pane id: {pane:?}")))
}

/// The pane the command runs in, when the command goes to the app of
/// that pane. The app refuses the call if it no longer has the pane, so
/// a shell it has thrown away cannot change its window. Another app has
/// its own panes, in which the same number is a different pane.
fn asking_pane(socket: Option<&str>, env: &CliEnv) -> Result<Option<PaneId>, Failure> {
    let own_app = socket.is_none_or(|socket| env.socket.as_deref() == Some(OsStr::new(socket)));
    if !own_app || env.pane.is_none() {
        return Ok(None);
    }
    callers_pane(env).map(Some)
}

fn socket_path(socket: Option<&str>, env: &CliEnv) -> Result<OsString, Failure> {
    let path = socket
        .map(OsString::from)
        .or_else(|| env.socket.clone())
        .ok_or_else(|| {
            Failure::Failed(format!(
                "no socket: {SOCKET_ENV} is not set, so this is not a Benterm pane; give --socket PATH"
            ))
        })?;
    if path.is_empty() {
        return Err(Failure::Failed(format!(
            "no socket: the app this pane runs in started without its socket API, because it \
             could not open its socket ({SOCKET_ENV} is empty); its window says why. Restart \
             the app, or give --socket PATH"
        )));
    }
    Ok(path)
}

static OUTBOX_SEQ: AtomicU64 = AtomicU64::new(1);

#[derive(serde::Serialize)]
struct OutboxPayload<'a> {
    to: &'a str,
    text: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    status: Option<ReportStatus>,
}

/// Writes one mailbox outbox message atomically (`<seq>-<pid>.json.tmp` ->
/// `<seq>-<pid>.json`) as a fallback when the socket cannot be reached.
fn write_mailbox_outbox(
    mailbox_dir: &Path,
    to: &str,
    text: &str,
    status: Option<ReportStatus>,
) -> Result<(), Failure> {
    let outbox = mailbox_dir.join("outbox");
    std::fs::create_dir_all(&outbox).map_err(|error| {
        Failure::Failed(format!(
            "cannot create mailbox outbox {}: {error}",
            outbox.display()
        ))
    })?;
    let payload = serde_json::to_vec(&OutboxPayload { to, text, status })
        .map_err(|error| Failure::Failed(error.to_string()))?;
    let seq = OUTBOX_SEQ.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let file_name = format!("{seq:016}-{pid}.json");
    let tmp_path = outbox.join(format!("{file_name}.tmp"));
    let final_path = outbox.join(&file_name);
    std::fs::write(&tmp_path, payload).map_err(|error| {
        Failure::Failed(format!(
            "cannot write mailbox message {}: {error}",
            tmp_path.display()
        ))
    })?;
    if let Err(error) = std::fs::rename(&tmp_path, &final_path) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(Failure::Failed(format!(
            "cannot publish mailbox message {}: {error}",
            final_path.display()
        )));
    }
    Ok(())
}

fn connect(socket: Option<&str>, env: &CliEnv) -> Result<Client, Failure> {
    let path = socket_path(socket, env)?;
    Client::connect(&path)
        .map(|client| client.with_token(env.token().map(str::to_owned)))
        .map_err(|error| {
            Failure::Failed(format!(
                "cannot connect to {}: {error}",
                path.to_string_lossy()
            ))
        })
}

/// A connection and how long each call on it may wait.
struct Connection {
    client: Client,
    timeout: Duration,
}

impl Connection {
    fn call<M: Method>(mut self, request: &M) -> Result<M::Output, ClientError> {
        self.client.call_within(request, self.timeout)
    }
}

fn call<T>(result: Result<T, ClientError>) -> Result<T, Failure> {
    result.map_err(failure)
}

fn failure(error: ClientError) -> Failure {
    match error {
        ClientError::TimedOut(_) => Failure::TimedOut(error.to_string()),
        error => Failure::Failed(error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    /// Extracts `(top_level_commands, value_options, flag_options)` from
    /// [`USAGE`], excluding `setup` which has its own parser.
    fn parse_usage_tables() -> (BTreeSet<String>, BTreeSet<String>, BTreeSet<String>) {
        let mut commands = BTreeSet::new();
        let mut value_options = BTreeSet::new();
        let mut flag_options = BTreeSet::new();

        fn tokenize(text: &str) -> Vec<&str> {
            let mut out = Vec::new();
            for mut word in text.split_whitespace() {
                while let Some(rest) = word.strip_prefix('[') {
                    out.push("[");
                    word = rest;
                }
                let mut closings = 0;
                while let Some(rest) = word.strip_suffix(']') {
                    closings += 1;
                    word = rest;
                }
                if !word.is_empty() {
                    out.push(word);
                }
                out.extend(std::iter::repeat_n("]", closings));
            }
            out
        }

        let collect =
            |tokens: &[&str], values: &mut BTreeSet<String>, flags: &mut BTreeSet<String>| {
                let mut i = 0;
                while i < tokens.len() {
                    let token = tokens[i];
                    if token.starts_with("--") && token != "--" {
                        let has_value = matches!(
                            tokens.get(i + 1).copied(),
                            Some(next) if !matches!(next, "[" | "]" | "|") && !next.starts_with('-')
                        );
                        if has_value {
                            values.insert(token.to_string());
                            i += 1;
                        } else {
                            flags.insert(token.to_string());
                        }
                    }
                    i += 1;
                }
            };

        let usage_line = USAGE
            .lines()
            .find(|line| line.starts_with("usage: benterm "))
            .expect("USAGE has a usage line");
        let header = usage_line.strip_prefix("usage: benterm ").unwrap();
        collect(&tokenize(header), &mut value_options, &mut flag_options);

        let mut in_commands = false;
        for line in USAGE.lines() {
            if !in_commands {
                if line == "commands:" {
                    in_commands = true;
                }
                continue;
            }
            if line.trim().is_empty() {
                break;
            }
            if let Some(synopsis) = line.strip_prefix("  ")
                && !synopsis.starts_with(' ')
            {
                let tokens = tokenize(synopsis);
                let top = tokens[0];
                commands.insert(top.to_string());
                if top != "setup" {
                    collect(&tokens, &mut value_options, &mut flag_options);
                }
            }
        }

        (commands, value_options, flag_options)
    }

    #[test]
    fn value_options_matches_usage_and_excludes_flags() {
        let (_commands, value_options, flag_options) = parse_usage_tables();
        let table: BTreeSet<String> = VALUE_OPTIONS.iter().map(|s| (*s).to_string()).collect();

        assert_eq!(
            table, value_options,
            "VALUE_OPTIONS must contain exactly the value-taking options from USAGE"
        );
        for flag in &flag_options {
            assert!(
                !VALUE_OPTIONS.contains(&flag.as_str()),
                "flag option {flag} must not be in VALUE_OPTIONS"
            );
        }
    }

    #[test]
    fn commands_table_matches_usage() {
        let (usage_commands, _, _) = parse_usage_tables();
        let table: BTreeSet<String> = COMMANDS.iter().map(|s| (*s).to_string()).collect();
        assert_eq!(table, usage_commands);
    }

    #[test]
    fn arguments_parse_agrees_with_value_and_flag_options_in_usage() {
        let (_commands, value_options, flag_options) = parse_usage_tables();

        for opt in &value_options {
            let missing = Arguments::parse(std::slice::from_ref(opt));
            assert!(
                matches!(missing, Err(Failure::Usage(ref msg)) if msg.contains("needs a value")),
                "value option {opt} without a value should fail with 'needs a value'"
            );
            let sample = if opt == "--timeout" { "1" } else { "x" };
            let parsed = Arguments::parse(&[opt.clone(), sample.to_string()]);
            assert!(
                parsed.is_ok(),
                "value option {opt} with a value should parse cleanly"
            );
        }

        for flag in &flag_options {
            let parsed = Arguments::parse(std::slice::from_ref(flag));
            assert!(
                parsed.is_ok(),
                "flag option {flag} without a value should parse cleanly"
            );
        }
    }

    #[test]
    fn cli_env_prefers_control_token_and_redacts_debug() {
        let mut env = CliEnv::default();
        assert_eq!(env.token(), None);

        env.pane_token = Some("pane-secret-abc".into());
        assert_eq!(env.token(), Some("pane-secret-abc"));

        env.control_token = Some(String::new());
        assert_eq!(env.token(), Some("pane-secret-abc"));

        env.control_token = Some("control-secret-xyz".into());
        assert_eq!(env.token(), Some("control-secret-xyz"));

        let debug = format!("{env:?}");
        assert!(!debug.contains("pane-secret-abc"));
        assert!(!debug.contains("control-secret-xyz"));
        assert!(debug.contains("<redacted>"));
    }
}

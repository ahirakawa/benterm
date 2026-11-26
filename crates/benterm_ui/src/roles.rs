//! Roles in the window: the menu bar and the command palette list the
//! roles of the roles directory, and choosing one starts an agent of the
//! role in a new tab.
//!
//! The agent is one of the window's agents, as `agent.spawn` starts them,
//! without a parent, and it is shown as a tab at once. The window has no
//! task to give it, so its first message is the role's instructions.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use benterm_config::{Role, Roles};
use benterm_ipc::{AgentTarget, CONTROL_TOKEN_ENV, PANE_TOKEN_ENV, ShowAgent, SpawnAgent};
use benterm_session::Conversation;
use benterm_terminal::PtyConfig;
use gpui::{Action, Context, Window};

use crate::agents::executable;
use crate::{BentermWindow, menu};

/// Starts an agent of the role with this name in a new tab.
#[derive(Clone, Debug, PartialEq, Action)]
#[action(namespace = role, no_json)]
pub struct LaunchRole(pub String);

/// How the menu bar and the palette name a role.
pub(crate) fn role_label(role: &Role) -> String {
    match role.description.trim() {
        "" => role.name.clone(),
        description => format!("{} — {description}", role.name),
    }
}

/// The first of `name`, `name-2`, `name-3`, ... that is not `taken`.
pub(crate) fn free_name(name: &str, taken: &[String]) -> String {
    (1..)
        .map(|n| match n {
            1 => name.to_owned(),
            n => format!("{name}-{n}"),
        })
        .find(|candidate| !taken.contains(candidate))
        .expect("some number is free")
}

/// What starts `command`: itself if its program is on `path`, and
/// otherwise the login shell `shell`, which looks it up on the user's
/// `PATH`. An app opened from the Finder has a short `PATH` until the
/// user's shell has given its environment (see [`crate::login_env`]). If
/// that shell gave none, in time or at all, the login shell here still
/// reads the login files, without the interactive ones that may have
/// kept it.
///
/// The login files may set a name of `vars`, the role's variables, which
/// the agent is also given as variables of its own. `env` sets them again
/// after those files, so the role's values are the ones the program gets.
pub(crate) fn launch_command(
    command: Vec<String>,
    vars: &[(String, String)],
    path: Option<&OsStr>,
    shell: &Path,
) -> Vec<String> {
    let found = path.is_some_and(|path| {
        std::env::split_paths(path).any(|dir| executable(&dir.join(&command[0])).is_some())
    });
    if found {
        return command;
    }
    let mut wrapped = vec![
        shell.to_string_lossy().into_owned(),
        "-l".to_owned(),
        "-c".to_owned(),
        r#"exec "$0" "$@""#.to_owned(),
    ];
    if !vars.is_empty() {
        wrapped.extend(["/usr/bin/env".to_owned(), "--".to_owned()]);
        wrapped.extend(vars.iter().map(|(name, value)| format!("{name}={value}")));
    }
    wrapped.extend(command);
    wrapped
}

/// What a terminal runs once its agent has ended, when the agent was
/// started to give way to a shell: the program `shell`, the
/// configuration of a new pane, runs, with its arguments. A pane that
/// runs the login shell gets it as a login shell.
pub(crate) fn shell_argv(shell: &PtyConfig) -> Vec<OsString> {
    match &shell.program {
        Some(program) => std::iter::once(program.clone().into_os_string())
            .chain(shell.args.iter().cloned())
            .collect(),
        None => vec![
            benterm_terminal::login_shell().into_os_string(),
            "-l".into(),
        ],
    }
}

/// What runs the agent and then the shell: `"$@"` is the agent. It runs
/// as a job of its own in front, so that the agent is the program in
/// front, has the terminal, and gets control-C alone. A job that stops,
/// as an agent does that is told control-Z, is let go on at once: the
/// shell after it could not take it up. The shell after the agent takes
/// the place of this one, in the same process.
const THEN_SHELL: &str = "\"$@\"\nwhile jobs %% >/dev/null 2>&1; do fg >/dev/null 2>&1; done";

/// `agent`, started so that its terminal goes on with `shell`, a program
/// and its arguments, once it ends.
///
/// The shell around the agent is interactive, because only such a shell
/// sees that its job stopped, and zsh gives the terminal to its job only
/// then. It reads none of the user's files. It is zsh when the shell
/// after is zsh, so that Benterm's shell integration, which works from the
/// variables of the first program of the terminal, is set up for zsh and
/// kept for the shell after: `-f` keeps zsh from reading the file that
/// takes the variables back. Any other shell after gets bash around the
/// agent, whose integration is a variable the shell after keeps too.
pub(crate) fn then_shell(agent: PtyConfig, shell: &[OsString]) -> PtyConfig {
    let zsh = shell
        .first()
        .filter(|program| Path::new(program).file_name() == Some(OsStr::new("zsh")));
    let (program, options): (OsString, &[&str]) = match zsh {
        Some(zsh) => (zsh.clone(), &["-f", "-i", "-c"]),
        None => ("/bin/bash".into(), &["--norc", "--noprofile", "-i", "-c"]),
    };
    let mut script = OsString::from(THEN_SHELL);
    script.push("\nunset BENTERM_MAILBOX_DIR\nexec");
    for arg in shell {
        script.push(" ");
        script.push(quoted(arg));
    }
    let args = options
        .iter()
        .map(OsString::from)
        .chain([script, "benterm".into()])
        .chain(agent.program.clone().map(PathBuf::into_os_string))
        .chain(agent.args.iter().cloned())
        .collect();
    PtyConfig {
        program: Some(PathBuf::from(program)),
        args,
        ..agent
    }
}

/// `arg` as one word of a shell's command line.
fn quoted(arg: &OsStr) -> OsString {
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    let mut quoted = vec![b'\''];
    for &byte in arg.as_bytes() {
        match byte {
            b'\'' => quoted.extend_from_slice(br"'\''"),
            byte => quoted.push(byte),
        }
    }
    quoted.push(b'\'');
    OsString::from_vec(quoted)
}

pub(crate) fn login_shell() -> PathBuf {
    std::env::var_os("SHELL")
        .filter(|shell| !shell.is_empty())
        .map_or_else(|| PathBuf::from("/bin/zsh"), PathBuf::from)
}

/// The `PATH` programs are looked up on, as in the panes.
pub(crate) fn programs_path(shells: &crate::ipc::Shells) -> Option<OsString> {
    shells
        .pty
        .env
        .iter()
        .rev()
        .find(|(name, _)| name == "PATH")
        .map(|(_, path)| path.clone())
        .or_else(|| shells.login_env.path())
        .or_else(|| std::env::var_os("PATH"))
}

fn role_request(
    shells: &crate::ipc::Shells,
    role: &Role,
    name: String,
    mut command: Vec<String>,
) -> SpawnAgent {
    let path = programs_path(shells);
    benterm_config::fit_codex_launch(&mut command, path.as_deref());
    // The login-shell wrapper also sets role variables. Keep tokens out of
    // its arguments, as well as out of the environment overrides.
    let env: Vec<_> = role
        .env
        .iter()
        .filter(|(name, _)| name != PANE_TOKEN_ENV && name != CONTROL_TOKEN_ENV)
        .cloned()
        .collect();
    let command = launch_command(command, &env, path.as_deref(), &login_shell());
    SpawnAgent {
        name,
        command,
        env: path
            .and_then(|path| path.into_string().ok())
            .map(|path| ("PATH".to_owned(), path))
            .into_iter()
            .chain(env)
            .collect(),
        shell_after: true,
        ..SpawnAgent::default()
    }
}

pub(crate) fn resume_program(
    shells: &crate::ipc::Shells,
    role: &Role,
    conversation: &Conversation,
) -> Option<PtyConfig> {
    if role.agent.program() != conversation.agent.id() {
        return None;
    }
    let mut command = role.agent.launch();
    let [resume, id] = conversation.resume_args();
    match conversation.agent {
        benterm_session::Agent::Claude => {
            command.extend(role.launch_options());
            command.extend([resume.to_owned(), id.to_owned()]);
        }
        benterm_session::Agent::Codex => {
            command.push(resume.to_owned());
            command.extend(role.launch_options());
            command.extend(["--".to_owned(), id.to_owned()]);
        }
    }
    let request = role_request(shells, role, role.name.clone(), command);
    crate::agents::program(&request, &shells.pty, shells.login_env.path()).ok()
}

impl BentermWindow {
    /// Takes the roles of the roles directory after a change: the menu
    /// bar and the palette list them, and the bar names the files that
    /// are not roles.
    pub(crate) fn apply_roles(&mut self, roles: Roles, cx: &mut Context<Self>) {
        self.settings.set_roles(roles);
        cx.set_menus(menu::menus_with_roles(self.settings.roles()));
        self.dismissed = false;
        cx.notify();
    }

    /// Starts an agent of the role named `name` in a new tab, which takes
    /// the keyboard. It starts in the directory a new tab would. Why it
    /// could not start is said in the bar.
    ///
    /// Chosen before the user's shell has given its environment, the role
    /// starts once it has.
    pub fn launch_role(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(shells) = &self.shells
            && shells.login_env.is_pending()
        {
            let taken = shells.login_env.taken();
            let name = name.to_owned();
            cx.spawn_in(window, async move |this, cx| {
                taken.await;
                let _ = this.update_in(cx, |this, window, cx| this.launch_role(&name, window, cx));
            })
            .detach();
            return;
        }
        if let Err(message) = self.try_launch_role(name, window, cx) {
            self.role_notice = Some(message);
            self.dismissed = false;
            cx.notify();
        }
    }

    fn try_launch_role(
        &mut self,
        name: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let role = self
            .settings
            .roles()
            .iter()
            .find(|role| role.name == name)
            .cloned()
            .ok_or_else(|| format!("there is no role {name:?}"))?;
        let shells = self
            .shells
            .clone()
            .ok_or_else(|| "this window cannot start agents".to_owned())?;
        let taken: Vec<String> = self.agents(cx).into_iter().map(|row| row.name).collect();
        let agent = free_name(&role.name, &taken);
        let mut request = role_request(&shells, &role, agent.clone(), role.command(""));
        request.cwd = self
            .grid(cx)
            .read(cx)
            .directory_for_new_terminal(cx)
            .and_then(|cwd| cwd.to_str().map(str::to_owned));
        let failed = |error: benterm_ipc::ApiError| {
            format!("the role {:?} did not start: {}", role.name, error.message)
        };
        self.spawn_agent(request, Some(role.name.clone()), &shells, window, cx)
            .map_err(failed)?;
        let show = ShowAgent {
            agent: AgentTarget::Name(agent),
            select: true,
        };
        self.show_agent(&show, window, cx).map_err(failed)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn a_second_agent_of_a_role_is_numbered() {
        let taken = |names: &[&str]| names.iter().map(|n| n.to_string()).collect::<Vec<_>>();
        assert_eq!(free_name("r", &taken(&[])), "r");
        assert_eq!(free_name("r", &taken(&["r"])), "r-2");
        assert_eq!(free_name("r", &taken(&["r", "r-2"])), "r-3");
    }

    #[test]
    fn a_program_that_is_not_on_the_path_is_started_by_the_login_shell() {
        let dir = tempfile::tempdir().unwrap();
        let tool = dir.path().join("claude");
        std::fs::write(&tool, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
        let command = vec!["claude".to_owned(), "--".to_owned(), "p".to_owned()];
        let shell = Path::new("/bin/zsh");

        assert_eq!(
            launch_command(command.clone(), &[], Some(dir.path().as_os_str()), shell),
            command
        );
        assert_eq!(
            launch_command(command.clone(), &[], Some(OsStr::new("/nowhere")), shell),
            [
                "/bin/zsh",
                "-l",
                "-c",
                r#"exec "$0" "$@""#,
                "claude",
                "--",
                "p"
            ]
        );
        assert_eq!(launch_command(command, &[], None, shell)[0], "/bin/zsh");
    }

    fn vars(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(name, value)| (name.to_string(), value.to_string()))
            .collect()
    }

    #[test]
    fn the_login_shell_starts_the_program_with_the_roles_variables_set_again() {
        let command = vec!["claude".to_owned(), "--".to_owned(), "p".to_owned()];
        let shell = Path::new("/bin/zsh");
        let role = vars(&[("FOO", "bar"), ("A", "x=y")]);

        assert_eq!(
            launch_command(command.clone(), &role, None, shell),
            [
                "/bin/zsh",
                "-l",
                "-c",
                r#"exec "$0" "$@""#,
                "/usr/bin/env",
                "--",
                "FOO=bar",
                "A=x=y",
                "claude",
                "--",
                "p"
            ]
        );
        // Found on the path, the program gets them as any variable.
        let dir = tempfile::tempdir().unwrap();
        let tool = dir.path().join("claude");
        std::fs::write(&tool, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            launch_command(command.clone(), &role, Some(dir.path().as_os_str()), shell),
            command
        );
    }

    fn strings(args: &[OsString]) -> Vec<String> {
        args.iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn a_pane_goes_on_with_what_a_new_pane_runs() {
        assert_eq!(
            shell_argv(&PtyConfig::command("/bin/sh", ["-i"])),
            [OsString::from("/bin/sh"), "-i".into()]
        );
        assert_eq!(
            shell_argv(&PtyConfig::login_shell()),
            [
                benterm_terminal::login_shell().into_os_string(),
                "-l".into()
            ]
        );
    }

    /// The agent runs as a job of a shell that reads none of the user's
    /// files, and the shell becomes the one a new pane runs after it.
    /// The agent's arguments and variables are passed as they were.
    #[test]
    fn an_agent_that_gives_way_to_a_shell_runs_as_a_job_of_one() {
        let mut agent = PtyConfig::command("/opt/bin/claude", ["--add-dir", "../d", "--", "p q"]);
        agent.env = vec![("FOO".into(), "bar".into())];
        agent.cwd = Some(PathBuf::from("/work"));

        let bash = then_shell(agent.clone(), &["/bin/sh".into(), "-i".into()]);
        assert_eq!(bash.program, Some(PathBuf::from("/bin/bash")));
        let args = strings(&bash.args);
        assert_eq!(args[..4], ["--norc", "--noprofile", "-i", "-c"]);
        assert!(args[4].ends_with("\nexec '/bin/sh' '-i'"), "{:?}", args[4]);
        assert_eq!(
            args[5..],
            [
                "benterm",
                "/opt/bin/claude",
                "--add-dir",
                "../d",
                "--",
                "p q"
            ]
        );
        assert_eq!(bash.env, agent.env);
        assert_eq!(bash.cwd, agent.cwd);

        // zsh keeps what Benterm's shell integration gives it for the
        // shell after the agent.
        let zsh = then_shell(agent, &["/bin/zsh".into(), "-l".into()]);
        assert_eq!(zsh.program, Some(PathBuf::from("/bin/zsh")));
        let args = strings(&zsh.args);
        assert_eq!(args[..3], ["-f", "-i", "-c"]);
        assert!(args[3].ends_with("\nexec '/bin/zsh' '-l'"), "{:?}", args[3]);
        assert_eq!(args[4..6], ["benterm", "/opt/bin/claude"]);
    }

    #[test]
    fn a_quote_in_the_shell_s_arguments_stays_one() {
        let pty = then_shell(
            PtyConfig::command("/bin/echo", ["x"]),
            &["/bin/sh".into(), "-c".into(), "echo it's".into()],
        );
        let script = &strings(&pty.args)[4];
        assert!(
            script.ends_with(r"exec '/bin/sh' '-c' 'echo it'\''s'"),
            "{script:?}"
        );
    }

    /// A made-up agent that tells whether it is in front and has the
    /// terminal, catches control-C, stops itself on `stop`, echoes the
    /// lines it reads and ends on `quit`.
    const AGENT: &str = r#"trap 'echo got-int' INT
set -- $(ps -o pgid=,tpgid= -p $$)
echo "front $1 $2 $$"
tty -s && echo has-a-tty
while read line; do
  case $line in
    quit) echo bye; exit 0 ;;
    stop) kill -TSTP $$; echo resumed ;;
    *) echo "read $line" ;;
  esac
done"#;

    /// Runs the made-up agent to give way to `shell` in a terminal, and
    /// checks that the agent has the terminal to itself, that control-C
    /// and control-Z reach it and not the shell around it, and that
    /// `shell` runs in the terminal after it.
    fn gives_way_to(shell: &[&str]) {
        use benterm_core::GridSize;
        use benterm_terminal::{Terminal, TerminalConfig};
        use std::time::{Duration, Instant};

        let shell: Vec<OsString> = shell.iter().map(OsString::from).collect();
        let mut pty = then_shell(PtyConfig::command("/bin/sh", ["-c", AGENT]), &shell);
        pty.env = vec![
            ("PS1".into(), "after$ ".into()),
            ("ENV".into(), "/dev/null".into()),
        ];
        let terminal = Terminal::spawn(
            TerminalConfig {
                pty,
                size: GridSize::new(40, 80),
                scrollback_limit: 100,
                colors: None,
                log: None,
            },
            || {},
        )
        .unwrap();
        let wait_for = |what: &str| {
            let deadline = Instant::now() + Duration::from_secs(10);
            while !terminal.text(None).contains(what) {
                assert!(
                    Instant::now() < deadline,
                    "no {what:?} on the screen: {}",
                    terminal.text(None)
                );
                std::thread::sleep(Duration::from_millis(10));
            }
        };

        wait_for("has-a-tty");
        let text = terminal.text(None);
        let line = text.lines().find(|l| l.starts_with("front ")).unwrap();
        let ids: Vec<&str> = line.split_whitespace().skip(1).collect();
        assert!(ids[0] == ids[1] && ids[1] == ids[2], "{line}");
        let front = terminal.foreground_process().unwrap();
        assert_eq!(front.pid.to_string(), ids[2]);

        terminal.write(b"one\r".to_vec()).unwrap();
        wait_for("read one");
        terminal.write(b"\x03".to_vec()).unwrap();
        wait_for("got-int");
        terminal.write(b"stop\r".to_vec()).unwrap();
        wait_for("resumed");
        terminal.write(b"two\r".to_vec()).unwrap();
        wait_for("read two");
        terminal.write(b"quit\r".to_vec()).unwrap();
        wait_for("bye");

        let deadline = Instant::now() + Duration::from_secs(10);
        while terminal.foreground_process().map(|front| front.args) != Some(shell.clone()) {
            assert!(
                Instant::now() < deadline,
                "the shell never came to the front"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        terminal
            .write(b"echo shell-$((40 + 2))\r".to_vec())
            .unwrap();
        wait_for("shell-42");
    }

    /// Zsh and Bash run their jobs differently.
    #[test]
    fn under_zsh_the_agent_has_the_terminal_and_the_shell_comes_after_it() {
        gives_way_to(&["/bin/zsh", "-f", "-i"]);
    }

    #[test]
    fn under_bash_the_agent_has_the_terminal_and_the_shell_comes_after_it() {
        gives_way_to(&["/bin/sh", "-i"]);
    }

    /// The login files may set a name the role has; the role's value is
    /// the one the program gets.
    #[test]
    fn a_login_file_does_not_put_its_value_over_the_roles() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join(".zprofile"), "export FOO=login\n").unwrap();
        let role = vars(&[("FOO", "role")]);
        let command = ["/bin/sh", "-c", "printf %s \"$FOO\""]
            .map(String::from)
            .to_vec();

        let launch = launch_command(command, &role, None, Path::new("/bin/zsh"));
        let output = std::process::Command::new(&launch[0])
            .args(&launch[1..])
            .env("ZDOTDIR", home.path())
            .env("FOO", "role")
            .output()
            .unwrap();

        assert_eq!(String::from_utf8_lossy(&output.stdout), "role");
    }
}

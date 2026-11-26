//! The command line for roles: `benterm role ...` starts agents from the
//! role files of the roles directory.

mod common;

use std::path::{Path, PathBuf};

use benterm_ipc::cli::{self, CliEnv};
use benterm_ipc::{PaneId, Request, SpawnAgent};
use common::{TAKEN_NAME, TestServer, start_server};

const REVIEWER: &str = r#"
name = "reviewer"
description = "PR を読む"
agent = "claude"
model = "opus"
permission_mode = "auto"
prompt = "差分を読んで指摘する"
"#;

struct Run {
    code: u8,
    stdout: String,
    stderr: String,
}

fn run_in(roles: &Path, env: &CliEnv, args: &[&str]) -> Run {
    let args: Vec<String> = args.iter().map(|arg| arg.to_string()).collect();
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let code = cli::run_with_roles(
        &args,
        env,
        Some(roles),
        std::io::empty(),
        &mut stdout,
        &mut stderr,
    );
    Run {
        code,
        stdout: String::from_utf8(stdout).unwrap(),
        stderr: String::from_utf8(stderr).unwrap(),
    }
}

/// The environment of a shell in pane 3 of the server's app.
fn pane_env(server: &TestServer) -> CliEnv {
    CliEnv {
        socket: Some(server.path.clone().into_os_string()),
        pane: Some("3".into()),
        cwd: Some("/work/here".into()),
        path: Some("/opt/bin:/bin".into()),
        ..CliEnv::default()
    }
}

/// A roles directory in a config directory of its own, so that the
/// rules for workers beside it are only there when a test puts them.
struct Roles {
    config: tempfile::TempDir,
    roles: PathBuf,
}

impl Roles {
    fn path(&self) -> &Path {
        &self.roles
    }

    /// Puts the rules for workers beside the roles directory, and returns
    /// the line that asks to read them.
    fn with_worker_rules(&self) -> String {
        let file = self.config.path().join("worker.md");
        std::fs::write(&file, "- 報告は agent report で返す\n").unwrap();
        benterm_config::worker_rules_line(&file).unwrap()
    }
}

fn roles(files: &[(&str, &str)]) -> Roles {
    let config = tempfile::tempdir().unwrap();
    let roles = config.path().join("roles");
    std::fs::create_dir(&roles).unwrap();
    for (name, text) in files {
        std::fs::write(roles.join(name), text).unwrap();
    }
    Roles { config, roles }
}

fn ok(run: &Run) -> &str {
    assert_eq!((run.code, run.stderr.as_str()), (0, ""));
    &run.stdout
}

#[test]
fn role_is_a_command() {
    assert!(cli::is_command("role"));
}

#[test]
fn run_starts_an_agent_of_the_role_on_the_task_as_a_child_of_the_callers_pane() {
    let server = start_server();
    let dir = roles(&[("reviewer.toml", REVIEWER)]);

    let run = run_in(
        dir.path(),
        &pane_env(&server),
        &["role", "run", "reviewer", "--", "PR", "#1", "を見る"],
    );

    assert_eq!(ok(&run), "reviewer 20\n");
    assert_eq!(
        server.app.requests(),
        [Request::SpawnAgent(SpawnAgent {
            name: "reviewer".into(),
            command: vec![
                "claude".into(),
                "--model".into(),
                "opus".into(),
                "--permission-mode".into(),
                "auto".into(),
                "--".into(),
                "差分を読んで指摘する\n\nPR #1 を見る".into(),
            ],
            cwd: Some("/work/here".into()),
            env: vec![("PATH".into(), "/opt/bin:/bin".into())],
            parent: Some(PaneId::new(3)),
            outlive_parent: false,
            first_message_line: None,
            // The agent's pane goes on with a shell when it ends.
            shell_after: true,
            allow_peers: false,
            max_peer_rounds: None,
            control: false,
        })]
    );
}

/// The line goes in the first prompt itself, so the first message sent
/// later is not given it again.
#[test]
fn run_puts_the_line_that_asks_to_read_the_rules_for_workers_first() {
    let server = start_server();
    let dir = roles(&[("reviewer.toml", REVIEWER)]);
    let line = dir.with_worker_rules();

    let run = run_in(
        dir.path(),
        &pane_env(&server),
        &["role", "run", "reviewer", "--", "PR", "#1", "を見る"],
    );

    ok(&run);
    let [Request::SpawnAgent(request)] = &server.app.requests()[..] else {
        panic!("one request");
    };
    let (prompt, options) = request.command.split_last().unwrap();
    assert_eq!(options.last().map(String::as_str), Some("--"));
    assert_eq!(
        prompt,
        &format!("{line}\n差分を読んで指摘する\n\nPR #1 を見る")
    );
    assert_eq!(request.first_message_line, None);
}

#[test]
fn run_gives_the_agent_the_roles_variables_after_the_path_and_its_arguments() {
    let server = start_server();
    let planner = r#"
name = "planner"
agent = "codex"
prompt = "計画を立てる"
args = ["-c", "tools.update_plan.enabled=true"]

[env]
FOO = "bar"
PATH = "/role/bin"
"#;
    let dir = roles(&[("planner.toml", planner)]);

    let run = run_in(dir.path(), &pane_env(&server), &["role", "run", "planner"]);

    assert_eq!(ok(&run), "planner 20\n");
    let [Request::SpawnAgent(request)] = &server.app.requests()[..] else {
        panic!("one request");
    };
    assert_eq!(
        request.command,
        // No codex on the PATH says it takes --no-daemon, so it is left out.
        [
            "codex",
            "-c",
            "tools.update_plan.enabled=true",
            "--",
            "計画を立てる"
        ]
    );
    // The last of a name is the one the agent gets.
    assert_eq!(
        request.env,
        [
            ("PATH".to_owned(), "/opt/bin:/bin".to_owned()),
            ("FOO".to_owned(), "bar".to_owned()),
            ("PATH".to_owned(), "/role/bin".to_owned()),
        ]
    );
}

#[test]
fn run_takes_a_name_a_directory_and_outlive_parent_as_agent_run_does() {
    let server = start_server();
    let dir = roles(&[("reviewer.toml", REVIEWER)]);

    let run = run_in(
        dir.path(),
        &pane_env(&server),
        &[
            "role",
            "run",
            "reviewer",
            "--name",
            "r2",
            "--cwd",
            "sub",
            "--outlive-parent",
        ],
    );

    assert_eq!(ok(&run), "r2 20\n");
    let [Request::SpawnAgent(request)] = &server.app.requests()[..] else {
        panic!("one request");
    };
    assert_eq!(request.name, "r2");
    assert_eq!(request.cwd.as_deref(), Some("/work/here/sub"));
    assert!(request.outlive_parent);
    assert_eq!(request.command.last().unwrap(), "差分を読んで指摘する");
}

#[test]
fn an_unknown_role_fails_and_names_the_files_that_are_not_roles() {
    let server = start_server();
    let dir = roles(&[
        ("reviewer.toml", REVIEWER),
        (
            "bad.toml",
            "name = \"bad\"\nagent = \"gemini\"\nprompt = \"p\"\n",
        ),
    ]);

    let run = run_in(dir.path(), &pane_env(&server), &["role", "run", "bad"]);

    assert_eq!((run.code, run.stdout.as_str()), (1, ""));
    assert!(run.stderr.contains("no role \"bad\""), "{}", run.stderr);
    assert!(run.stderr.contains("roles/bad.toml"), "{}", run.stderr);
    assert!(run.stderr.contains("gemini"), "{}", run.stderr);
    assert_eq!(server.app.requests(), []);
}

#[test]
fn a_taken_name_fails_as_agent_run_does() {
    let server = start_server();
    let dir = roles(&[("reviewer.toml", REVIEWER)]);

    let run = run_in(
        dir.path(),
        &pane_env(&server),
        &["role", "run", "reviewer", "--name", TAKEN_NAME],
    );

    assert_eq!((run.code, run.stdout.as_str()), (1, ""));
    assert!(run.stderr.contains("taken"), "{}", run.stderr);
}

#[test]
fn run_needs_a_role() {
    let dir = roles(&[]);
    let run = run_in(dir.path(), &CliEnv::default(), &["role", "run"]);
    assert_eq!(run.code, 2, "{}", run.stderr);
    assert!(run.stderr.contains("ROLE"), "{}", run.stderr);
}

#[test]
fn list_prints_each_role_and_names_the_files_that_are_not_roles_on_stderr() {
    let coder = "name = \"coder\"\nagent = \"codex\"\nprompt = \"p\"\n";
    let dir = roles(&[
        ("reviewer.toml", REVIEWER),
        ("coder.toml", coder),
        ("bad.toml", "agent = 1"),
    ]);

    let run = run_in(dir.path(), &CliEnv::default(), &["role", "list"]);

    assert_eq!(run.code, 0);
    assert_eq!(run.stdout, "coder codex\nreviewer claude PR を読む\n");
    assert!(run.stderr.contains("roles/bad.toml"), "{}", run.stderr);
}

#[test]
fn role_needs_a_known_subcommand_and_rejects_what_it_has_no_use_for() {
    let dir = roles(&[("reviewer.toml", REVIEWER)]);
    for (args, needle) in [
        (&["role"][..], "subcommand"),
        (&["role", "go"][..], "go"),
        (&["role", "list", "--name", "x"][..], "--name"),
        (&["role", "run", "reviewer", "--pane", "1"][..], "--pane"),
    ] {
        let run = run_in(dir.path(), &CliEnv::default(), args);
        assert_eq!(run.code, 2, "{args:?}: {}", run.stderr);
        assert!(run.stderr.contains(needle), "{args:?}: {}", run.stderr);
    }
}

#[test]
fn the_help_describes_the_role_commands() {
    let dir = roles(&[]);
    let run = run_in(dir.path(), &CliEnv::default(), &["--help"]);
    for needle in ["role run", "role list", "roles"] {
        assert!(run.stdout.contains(needle), "{needle}");
    }
}

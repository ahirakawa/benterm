//! The command line for pressing keys in an agent's terminal and for
//! killing an agent: `benterm agent keys` and `benterm agent kill`.

mod common;

use benterm_ipc::cli::{self, CliEnv};
use benterm_ipc::{AgentTarget, KillAgent, PaneId, Request, SendKeys};
use common::{MISSING_AGENT, TestServer, start_server};

struct Run {
    code: u8,
    stdout: String,
    stderr: String,
}

fn run_with(env: &CliEnv, args: &[&str]) -> Run {
    let args: Vec<String> = args.iter().map(|arg| arg.to_string()).collect();
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let code = cli::run(
        &args,
        env,
        std::io::Cursor::new(Vec::new()),
        &mut stdout,
        &mut stderr,
    );
    Run {
        code,
        stdout: String::from_utf8(stdout).unwrap(),
        stderr: String::from_utf8(stderr).unwrap(),
    }
}

/// A shell in pane 3 of the server's app.
fn run(server: &TestServer, args: &[&str]) -> Run {
    let env = CliEnv {
        socket: Some(server.path.clone().into_os_string()),
        pane: Some("3".into()),
        cwd: Some("/work/here".into()),
        path: Some("/bin".into()),
        ..CliEnv::default()
    };
    run_with(&env, args)
}

fn ok(run: &Run) {
    assert_eq!((run.code, run.stderr.as_str()), (0, ""));
    assert_eq!(run.stdout, "");
}

fn assert_usage_error(run: &Run, needle: &str) {
    assert_eq!(run.code, 2, "{}", run.stderr);
    assert_eq!(run.stdout, "");
    assert!(run.stderr.contains(needle), "{}", run.stderr);
}

fn named(name: &str) -> AgentTarget {
    AgentTarget::Name(name.into())
}

#[test]
fn keys_presses_the_named_keys_in_the_order_given() {
    let server = start_server();

    ok(&run(&server, &["agent", "keys", "w1", "escape"]));
    ok(&run(
        &server,
        &["agent", "keys", "20", "up", "ctrl-c", "enter"],
    ));

    assert_eq!(
        server.app.requests(),
        [
            Request::SendKeys(SendKeys {
                agent: named("w1"),
                keys: vec!["escape".into()],
            }),
            Request::SendKeys(SendKeys {
                agent: AgentTarget::Pane(PaneId::new(20)),
                keys: vec!["up".into(), "ctrl-c".into(), "enter".into()],
            }),
        ]
    );
}

#[test]
fn keys_needs_an_agent_and_at_least_one_key() {
    let server = start_server();

    assert_usage_error(&run(&server, &["agent", "keys"]), "AGENT");
    assert_usage_error(&run(&server, &["agent", "keys", "w1"]), "KEY");
    assert_usage_error(
        &run(&server, &["agent", "keys", "w1", "escape", "--select"]),
        "--select",
    );
    assert!(server.app.requests().is_empty());
}

#[test]
fn kill_kills_the_agent() {
    let server = start_server();

    ok(&run(&server, &["agent", "kill", "w1"]));
    ok(&run(&server, &["agent", "kill", "20"]));

    assert_eq!(
        server.app.requests(),
        [
            Request::KillAgent(KillAgent { agent: named("w1") }),
            Request::KillAgent(KillAgent {
                agent: AgentTarget::Pane(PaneId::new(20)),
            }),
        ]
    );
}

#[test]
fn kill_takes_one_agent_and_nothing_else() {
    let server = start_server();

    assert_usage_error(&run(&server, &["agent", "kill"]), "AGENT");
    assert_usage_error(&run(&server, &["agent", "kill", "w1", "w2"]), "\"w2\"");
    assert_usage_error(
        &run(&server, &["agent", "kill", "w1", "--children"]),
        "--children",
    );
    assert!(server.app.requests().is_empty());
}

#[test]
fn what_the_app_refuses_fails_with_1_and_says_why() {
    let server = start_server();

    let keys = run(&server, &["agent", "keys", MISSING_AGENT, "escape"]);
    let kill = run(&server, &["agent", "kill", MISSING_AGENT]);

    for run in [keys, kill] {
        assert_eq!(run.code, 1, "{}", run.stderr);
        assert_eq!(run.stdout, "");
        assert!(run.stderr.contains(MISSING_AGENT), "{}", run.stderr);
    }
}

#[test]
fn the_help_describes_keys_and_kill() {
    let run = run_with(&CliEnv::default(), &["--help"]);

    for needle in ["agent keys AGENT KEY...", "agent kill AGENT", "ctrl-c"] {
        assert!(run.stdout.contains(needle), "{needle}");
    }
}

//! The command line for agents: `benterm agent ...`.

mod common;

use std::path::Path;
use std::time::{Duration, Instant};

use benterm_ipc::cli::{self, CliEnv};
use benterm_ipc::{
    AgentActivity, AgentInfo, AgentTarget, ListAgents, PaneId, ReadAgent, ReportStatus,
    ReportToParent, Request, SendToAgent, ShowAgent, SpawnAgent, WaitForAgents,
};
use common::{ASKING_AGENT, HELD_AGENT, SLOW_AGENT, TAKEN_NAME, TestServer, start_server};
use serde_json::json;
use std::sync::atomic::Ordering;

struct Run {
    code: u8,
    stdout: String,
    stderr: String,
}

/// Runs the command with the roles directory of a config directory of its
/// own, which has no rules for workers.
fn run_with(env: &CliEnv, stdin: &str, args: &[&str]) -> Run {
    let config = tempfile::tempdir().unwrap();
    run_in(config.path(), env, stdin, args)
}

/// Runs the command with the roles directory of the config directory
/// `config`.
fn run_in(config: &Path, env: &CliEnv, stdin: &str, args: &[&str]) -> Run {
    let args: Vec<String> = args.iter().map(|arg| arg.to_string()).collect();
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let code = cli::run_with_roles(
        &args,
        env,
        Some(&config.join("roles")),
        std::io::Cursor::new(stdin.as_bytes().to_vec()),
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

fn run(server: &TestServer, args: &[&str]) -> Run {
    run_with(&pane_env(server), "", args)
}

fn ok(run: &Run) -> &str {
    assert_eq!((run.code, run.stderr.as_str()), (0, ""));
    &run.stdout
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
fn agent_is_a_command() {
    assert!(cli::is_command("agent"));
}

#[test]
fn run_starts_a_named_agent_as_a_child_of_the_callers_pane() {
    let server = start_server();

    let run = run(
        &server,
        &["agent", "run", "--name", "w1", "claude", "fix it"],
    );

    assert_eq!(ok(&run), "w1 20\n");
    assert_eq!(
        server.app.requests(),
        [Request::SpawnAgent(SpawnAgent {
            name: "w1".into(),
            command: vec!["claude".into(), "fix it".into()],
            // Where the command runs, and what it finds programs with.
            cwd: Some("/work/here".into()),
            env: vec![("PATH".into(), "/opt/bin:/bin".into())],
            parent: Some(PaneId::new(3)),
            outlive_parent: false,
            first_message_line: None,
            shell_after: false,
            allow_peers: false,
            max_peer_rounds: None,
            control: false,
        })]
    );
}

/// A config directory with the rules for workers, and the line that asks
/// to read them.
fn config_with_worker_rules() -> (tempfile::TempDir, String) {
    let config = tempfile::tempdir().unwrap();
    let file = config.path().join("worker.md");
    std::fs::write(&file, "- 報告は agent report で返す\n").unwrap();
    let line = benterm_config::worker_rules_line(&file).unwrap();
    (config, line)
}

/// The command is anyone's, so the line cannot go in it: the app puts it
/// before the first message the agent is sent.
#[test]
fn run_gives_the_line_that_asks_to_read_the_rules_for_workers_for_the_first_message() {
    let server = start_server();
    let (config, line) = config_with_worker_rules();

    let run = run_in(
        config.path(),
        &pane_env(&server),
        "",
        &["agent", "run", "--name", "w1", "claude", "fix it"],
    );

    ok(&run);
    let [Request::SpawnAgent(request)] = &server.app.requests()[..] else {
        panic!("one request");
    };
    assert_eq!(request.command, ["claude", "fix it"]);
    assert_eq!(request.first_message_line, Some(line));
}

/// A parent that follows the rules puts the line in the task it starts
/// the agent on, and the agent is not to read it twice.
#[test]
fn run_whose_command_has_the_line_gives_none_for_the_first_message() {
    let server = start_server();
    let (config, line) = config_with_worker_rules();
    let task = format!("{line}\nfix it");

    let run = run_in(
        config.path(),
        &pane_env(&server),
        "",
        &["agent", "run", "--name", "w1", "claude", &task],
    );

    ok(&run);
    let [Request::SpawnAgent(request)] = &server.app.requests()[..] else {
        panic!("one request");
    };
    assert_eq!(request.first_message_line, None);
}

/// A shell would take the line for a command.
#[test]
fn run_without_a_command_gives_no_line_for_the_first_message() {
    let server = start_server();
    let (config, _) = config_with_worker_rules();

    let run = run_in(
        config.path(),
        &pane_env(&server),
        "",
        &["agent", "run", "--name", "w1"],
    );

    ok(&run);
    let [Request::SpawnAgent(request)] = &server.app.requests()[..] else {
        panic!("one request");
    };
    assert_eq!(request.first_message_line, None);
}

#[test]
fn run_takes_a_directory_options_for_the_command_and_outlive_parent() {
    let server = start_server();

    let run = run(
        &server,
        &[
            "agent",
            "run",
            "--name",
            "w1",
            "--cwd",
            "sub",
            "--outlive-parent",
            "--",
            "codex",
            "--name",
            "x",
        ],
    );

    ok(&run);
    let [Request::SpawnAgent(request)] = &server.app.requests()[..] else {
        panic!("one request");
    };
    assert_eq!(request.command, ["codex", "--name", "x"]);
    assert_eq!(request.cwd.as_deref(), Some("/work/here/sub"));
    assert!(request.outlive_parent);
}

#[test]
fn run_outside_a_pane_starts_an_agent_without_a_parent_and_without_a_command_a_shell() {
    let server = start_server();
    let env = CliEnv {
        pane: None,
        path: None,
        ..pane_env(&server)
    };

    let run = run_with(&env, "", &["agent", "run", "--name", "w1"]);

    ok(&run);
    let [Request::SpawnAgent(request)] = &server.app.requests()[..] else {
        panic!("one request");
    };
    assert_eq!(request.parent, None);
    assert!(request.command.is_empty() && request.env.is_empty());
}

#[test]
fn run_needs_a_name_and_a_taken_name_fails() {
    let server = start_server();

    assert_usage_error(&run(&server, &["agent", "run", "claude"]), "--name");
    let taken = run(&server, &["agent", "run", "--name", TAKEN_NAME]);

    assert_eq!((taken.code, taken.stdout.as_str()), (1, ""));
    assert!(taken.stderr.contains("taken"), "{}", taken.stderr);
}

#[test]
fn list_prints_one_line_for_each_agent() {
    let server = start_server();

    let all = run(&server, &["agent", "list"]);
    let children = run(&server, &["agent", "list", "--children"]);

    assert_eq!(
        ok(&all),
        "w1 20 3 background working stalled changed_at_ms=1700000000000\n\
         w2 21 - tab done outlive-parent report=issues\n"
    );
    ok(&children);
    assert_eq!(
        server.app.requests(),
        [
            Request::ListAgents(ListAgents {
                parent: None,
                siblings_of: None,
            }),
            Request::ListAgents(ListAgents {
                parent: Some(PaneId::new(3)),
                siblings_of: None,
            }),
        ]
    );
}

#[test]
fn list_with_json_prints_one_json_object_per_agent() {
    let server = start_server();

    let all = run(&server, &["agent", "list", "--json"]);
    let children = run(&server, &["agent", "list", "--children", "--json"]);

    let lines: Vec<serde_json::Value> = ok(&all)
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(
        lines,
        vec![
            json!({
                "name": "w1", "pane": 20, "parent": 3, "background": true,
                "outlive_parent": false, "state": "working", "report_status": null,
                "stalled": true, "changed_at_ms": 1_700_000_000_000_u64,
            }),
            json!({
                "name": "w2", "pane": 21, "parent": null, "background": false,
                "outlive_parent": true, "state": "done", "report_status": "issues",
            }),
        ]
    );
    for line in ok(&all).lines() {
        let _: AgentInfo = serde_json::from_str(line).unwrap();
    }
    assert_eq!(ok(&children).lines().count(), 2);
    assert_eq!(
        server.app.requests(),
        [
            Request::ListAgents(ListAgents {
                parent: None,
                siblings_of: None,
            }),
            Request::ListAgents(ListAgents {
                parent: Some(PaneId::new(3)),
                siblings_of: None,
            }),
        ]
    );
}

#[test]
fn list_with_json_prints_nothing_when_there_are_no_agents() {
    let server = start_server();
    server.app.no_agents.store(true, Ordering::Relaxed);

    let listed = run(&server, &["agent", "list", "--json"]);

    assert_eq!(ok(&listed), "");
    assert_eq!(
        server.app.requests(),
        [Request::ListAgents(ListAgents {
            parent: None,
            siblings_of: None,
        })]
    );
}

#[test]
fn list_prints_sanitized_msg_only_when_present() {
    let server = start_server();
    *server.app.agent_list.lock().unwrap() = Some(vec![
        AgentInfo {
            name: "w1".into(),
            pane: PaneId::new(20),
            parent: Some(PaneId::new(3)),
            background: true,
            outlive_parent: false,
            state: AgentActivity::Waiting,
            report_status: None,
            stalled: false,
            changed_at_ms: None,
            msg: Some("Allow\u{1b} Bash:\n\tcargo\u{07} test".into()),
            allow_peers: false,
        },
        AgentInfo {
            name: "w2".into(),
            pane: PaneId::new(21),
            parent: None,
            background: false,
            outlive_parent: false,
            state: AgentActivity::Working,
            report_status: None,
            stalled: false,
            changed_at_ms: None,
            msg: None,
            allow_peers: false,
        },
    ]);

    let text = run(&server, &["agent", "list"]);
    assert_eq!(
        ok(&text),
        "w1 20 3 background waiting msg=Allow Bash: cargo test\n\
         w2 21 - tab working\n"
    );

    let json_out = run(&server, &["agent", "list", "--json"]);
    let lines: Vec<serde_json::Value> = ok(&json_out)
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(
        lines[0]["msg"],
        json!("Allow\u{1b} Bash:\n\tcargo\u{07} test")
    );
    assert!(lines[1].get("msg").is_none());
}

#[test]
fn send_gives_an_agent_a_message_from_the_arguments_or_from_standard_input() {
    let server = start_server();

    ok(&run(&server, &["agent", "send", "w1", "go", "on"]));
    ok(&run_with(
        &pane_env(&server),
        "from a file\n",
        &["agent", "send", "20"],
    ));

    assert_eq!(
        server.app.requests(),
        [
            Request::SendToAgent(SendToAgent {
                agent: named("w1"),
                text: "go on".into(),
                caller: Some(PaneId::new(3)),
            }),
            Request::SendToAgent(SendToAgent {
                agent: AgentTarget::Pane(PaneId::new(20)),
                text: "from a file\n".into(),
                caller: Some(PaneId::new(3)),
            }),
        ]
    );
}

#[test]
fn send_to_an_agent_that_waits_for_the_user_fails_and_tells_why() {
    let server = start_server();

    let refused = run(&server, &["agent", "send", ASKING_AGENT, "next"]);

    assert_eq!(refused.code, 1, "{}", refused.stderr);
    assert_eq!(refused.stdout, "");
    assert!(
        refused
            .stderr
            .contains("asking is waiting for a permission"),
        "{}",
        refused.stderr
    );
}

/// The sender of each `agent.send` the server's app was asked.
fn senders(server: &TestServer) -> Vec<Option<PaneId>> {
    let requests = server.app.requests().into_iter();
    requests
        .map(|request| match request {
            Request::SendToAgent(request) => request.caller,
            other => panic!("not a send: {other:?}"),
        })
        .collect()
}

#[test]
fn send_names_the_pane_it_was_sent_from_as_the_sender() {
    let server = start_server();

    ok(&run(&server, &["agent", "send", "w1", "go"]));

    assert_eq!(senders(&server), [Some(PaneId::new(3))]);
}

#[test]
fn send_from_outside_a_pane_or_to_another_app_names_no_sender() {
    let server = start_server();
    let socket = server.path.to_str().unwrap().to_owned();
    let outside = CliEnv {
        pane: None,
        ..pane_env(&server)
    };
    // The pane of another app, in which pane 3 is a different pane.
    let elsewhere = CliEnv {
        socket: Some("/another/app.sock".into()),
        ..pane_env(&server)
    };

    ok(&run_with(&outside, "", &["agent", "send", "w1", "go"]));
    ok(&run_with(
        &elsewhere,
        "",
        &["--socket", &socket, "agent", "send", "w1", "go"],
    ));

    assert_eq!(senders(&server), [None, None]);
}

#[test]
fn read_prints_the_screen_of_an_agent() {
    let server = start_server();

    let screen = run(&server, &["agent", "read", "w1"]);
    let lines = run(&server, &["agent", "read", "w1", "--lines", "50"]);

    assert_eq!(ok(&screen), "screen of agent w1\n");
    ok(&lines);
    assert_eq!(
        server.app.requests()[1],
        Request::ReadAgent(ReadAgent {
            agent: named("w1"),
            lines: Some(50),
        })
    );
}

#[test]
fn wait_waits_three_minutes_for_an_agent_to_stop_working_and_prints_its_state() {
    let server = start_server();

    let run = run(&server, &["agent", "wait", "w1"]);

    assert_eq!(ok(&run), "w1 done\n");
    assert_eq!(
        server.app.requests(),
        [Request::WaitForAgents(WaitForAgents {
            agent: Some(named("w1")),
            children_of: None,
            until: Vec::new(),
            wait_ms: Some(180_000),
            settle_ms: None,
        })]
    );
}

#[test]
fn wait_takes_the_states_the_time_and_the_children_of_the_caller() {
    let server = start_server();

    let run = run(
        &server,
        &[
            "agent",
            "wait",
            "--children",
            "--until",
            "working,unknown",
            "--wait",
            "2.5",
            "--settle",
            "0",
        ],
    );

    ok(&run);
    assert_eq!(
        server.app.requests(),
        [Request::WaitForAgents(WaitForAgents {
            agent: None,
            children_of: Some(PaneId::new(3)),
            until: vec![AgentActivity::Working, AgentActivity::Unknown],
            wait_ms: Some(2500),
            settle_ms: Some(0),
        })]
    );
}

#[test]
fn a_wait_that_runs_out_exits_with_3_and_says_so() {
    let server = start_server();

    let run = run(&server, &["agent", "wait", SLOW_AGENT, "--wait", "1"]);

    assert_eq!((run.code, run.stdout.as_str()), (3, ""));
    assert!(run.stderr.contains("still working"), "{}", run.stderr);
}

#[test]
fn a_wait_outlasts_the_timeout_for_the_app_by_the_time_it_waits() {
    let server = start_server();
    let started = Instant::now();

    // The app holds the wait and never answers it: the command gives up
    // after the time for the app to start it and the time to wait.
    let run = run(
        &server,
        &[
            "--timeout",
            "0.3",
            "agent",
            "wait",
            HELD_AGENT,
            "--wait",
            "0.5",
        ],
    );

    let took = started.elapsed();
    assert_eq!(run.code, 124, "{}", run.stderr);
    assert!(took >= Duration::from_millis(800), "gave up after {took:?}");
    assert!(took < Duration::from_secs(5), "gave up after {took:?}");
}

#[test]
fn wait_needs_one_agent_or_the_children_and_known_states() {
    let server = start_server();

    assert_usage_error(&run(&server, &["agent", "wait"]), "AGENT or --children");
    assert_usage_error(
        &run(&server, &["agent", "wait", "w1", "--children"]),
        "AGENT or --children",
    );
    assert_usage_error(
        &run(&server, &["agent", "wait", "w1", "--until", "asleep"]),
        "asleep",
    );
    assert_usage_error(
        &run(&server, &["agent", "wait", "w1", "--wait", "0"]),
        "--wait",
    );
    assert!(server.app.requests().is_empty());
}

#[test]
fn report_sends_a_report_from_the_callers_pane() {
    let server = start_server();

    ok(&run(&server, &["agent", "report", "done:", "PR #1"]));
    ok(&run_with(
        &pane_env(&server),
        "two\nlines",
        &["agent", "report"],
    ));

    assert_eq!(
        server.app.requests(),
        [
            Request::ReportToParent(ReportToParent {
                from: PaneId::new(3),
                text: "done: PR #1".into(),
                status: None,
            }),
            Request::ReportToParent(ReportToParent {
                from: PaneId::new(3),
                text: "two\nlines".into(),
                status: None,
            }),
        ]
    );
}

#[test]
fn report_takes_the_status_of_the_result() {
    let server = start_server();

    ok(&run(
        &server,
        &["agent", "report", "--status", "pass", "PR #1"],
    ));
    ok(&run(
        &server,
        &["agent", "report", "--status", "issues", "--", "-x"],
    ));
    ok(&run_with(
        &pane_env(&server),
        "no access",
        &["agent", "report", "--status", "blocked"],
    ));

    let report = |status, text: &str| {
        Request::ReportToParent(ReportToParent {
            from: PaneId::new(3),
            text: text.into(),
            status: Some(status),
        })
    };
    assert_eq!(
        server.app.requests(),
        [
            report(ReportStatus::Pass, "PR #1"),
            report(ReportStatus::Issues, "-x"),
            report(ReportStatus::Blocked, "no access"),
        ]
    );
}

#[test]
fn report_refuses_a_status_it_does_not_know_and_sends_nothing() {
    let server = start_server();

    assert_usage_error(
        &run(&server, &["agent", "report", "--status", "done", "PR #1"]),
        "pass, issues or blocked",
    );
    assert_usage_error(
        &run(&server, &["agent", "report", "--status", "PASS", "PR #1"]),
        "PASS",
    );
    assert_usage_error(&run(&server, &["agent", "report", "--status"]), "--status");
    assert_usage_error(
        &run(&server, &["agent", "send", "w1", "--status", "pass", "hi"]),
        "--status",
    );
    assert!(server.app.requests().is_empty());
}

#[test]
fn report_outside_a_pane_has_nobody_to_report_for() {
    let server = start_server();
    let env = CliEnv {
        pane: None,
        ..pane_env(&server)
    };

    let run = run_with(&env, "", &["agent", "report", "done"]);

    assert_usage_error(&run, "BENTERM_PANE_ID");
}

#[test]
fn show_opens_an_agent_as_a_tab_and_prints_its_pane() {
    let server = start_server();

    let shown = run(&server, &["agent", "show", "w1"]);
    ok(&run(&server, &["agent", "show", "w1", "--select"]));

    assert_eq!(ok(&shown), "20 stalled changed_at_ms=1700000000000\n");
    assert_eq!(
        ok(&run(&server, &["agent", "show", "w2"])),
        "21 report=blocked\n"
    );
    assert_eq!(
        server.app.requests(),
        [
            Request::ShowAgent(ShowAgent {
                agent: named("w1"),
                select: false,
            }),
            Request::ShowAgent(ShowAgent {
                agent: named("w1"),
                select: true,
            }),
            Request::ShowAgent(ShowAgent {
                agent: named("w2"),
                select: false,
            }),
        ]
    );
}

#[test]
fn agent_needs_a_known_subcommand_and_rejects_what_it_has_no_use_for() {
    let server = start_server();

    assert_usage_error(&run(&server, &["agent"]), "agent needs");
    assert_usage_error(&run(&server, &["agent", "dance"]), "dance");
    assert_usage_error(&run(&server, &["agent", "list", "w1"]), "unexpected");
    assert_usage_error(
        &run(&server, &["agent", "read", "w1", "--children"]),
        "--children",
    );
    assert_usage_error(&run(&server, &["agent", "read", "w1", "--json"]), "--json");
    assert_usage_error(&run(&server, &["agent", "send"]), "AGENT");
    assert!(server.app.requests().is_empty());
}

#[test]
fn the_help_describes_the_agent_commands() {
    let run = run_with(&CliEnv::default(), "", &["--help"]);

    for needle in [
        "agent run",
        "agent list [--children | --siblings] [--json]",
        "agent wait",
        "agent report",
        "--status pass|issues|blocked",
        "--outlive-parent",
        "--allow-peers",
        "--max-peer-rounds N",
        "--siblings",
        "--control",
    ] {
        assert!(run.stdout.contains(needle), "{needle}");
    }
}

#[test]
fn run_takes_allow_peers_and_max_peer_rounds() {
    let server = start_server();

    let ran = run(
        &server,
        &[
            "agent",
            "run",
            "--name",
            "w1",
            "--allow-peers",
            "--max-peer-rounds",
            "4",
            "claude",
        ],
    );

    ok(&ran);
    let [Request::SpawnAgent(request)] = &server.app.requests()[..] else {
        panic!("one request");
    };
    assert!(request.allow_peers);
    assert_eq!(request.max_peer_rounds, Some(4));
}

#[test]
fn run_gives_the_whole_api_only_with_control() {
    let server = start_server();

    ok(&run(&server, &["agent", "run", "--name", "w1", "claude"]));
    ok(&run(
        &server,
        &["agent", "run", "--name", "lead", "--control", "claude"],
    ));

    let requests = server.app.requests();
    let [Request::SpawnAgent(worker), Request::SpawnAgent(lead)] = &requests[..] else {
        panic!("two requests");
    };
    assert!(!worker.control);
    assert!(lead.control);
}

#[test]
fn run_rejects_invalid_max_peer_rounds() {
    let server = start_server();

    assert_usage_error(
        &run(
            &server,
            &["agent", "run", "--name", "w1", "--max-peer-rounds", "0"],
        ),
        "--max-peer-rounds",
    );
    assert_usage_error(
        &run(
            &server,
            &["agent", "run", "--name", "w1", "--max-peer-rounds", "abc"],
        ),
        "--max-peer-rounds",
    );
    assert!(server.app.requests().is_empty());
}

#[test]
fn list_siblings_asks_for_the_callers_siblings_and_rejects_children_together() {
    let server = start_server();

    let siblings = run(&server, &["agent", "list", "--siblings"]);
    ok(&siblings);
    assert_eq!(
        server.app.requests(),
        [Request::ListAgents(ListAgents {
            parent: None,
            siblings_of: Some(PaneId::new(3)),
        })]
    );

    assert_usage_error(
        &run(&server, &["agent", "list", "--children", "--siblings"]),
        "--siblings",
    );

    let outside = CliEnv {
        pane: None,
        ..pane_env(&server)
    };
    assert_usage_error(
        &run_with(&outside, "", &["agent", "list", "--siblings"]),
        "BENTERM_PANE_ID",
    );
}

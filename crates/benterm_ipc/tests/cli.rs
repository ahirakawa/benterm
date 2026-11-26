//! The command line: arguments in, one request out, the answer printed.

mod common;

use benterm_ipc::cli::{self, CliEnv};
use benterm_ipc::{
    CreateWorkspace, PaneId, ReadAgentHistory, ReadOutput, ReportHook, Request, SendText,
    SplitDirection, SplitPane,
};
use common::{MISSING_PANE, TestServer, private_dir, start_server};

struct Run {
    code: u8,
    stdout: String,
    stderr: String,
}

fn run_with(env: &CliEnv, stdin: &str, args: &[&str]) -> Run {
    let args: Vec<String> = args.iter().map(|arg| arg.to_string()).collect();
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let code = cli::run(
        &args,
        env,
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
        cwd: None,
        path: None,
        ..CliEnv::default()
    }
}

fn run(server: &TestServer, args: &[&str]) -> Run {
    run_with(&pane_env(server), "", args)
}

fn assert_usage_error(run: &Run, needle: &str) {
    assert_eq!(run.code, 2, "{}", run.stderr);
    assert_eq!(run.stdout, "");
    assert!(run.stderr.contains(needle), "{}", run.stderr);
}

#[test]
fn split_splits_the_callers_pane_to_the_right_and_prints_the_new_pane() {
    let server = start_server();

    let run = run(&server, &["split"]);

    assert_eq!(
        (run.code, run.stdout.as_str(), run.stderr.as_str()),
        (0, "4\n", "")
    );
    assert_eq!(
        server.app.requests(),
        [Request::SplitPane(SplitPane {
            pane: PaneId::new(3),
            direction: SplitDirection::Right,
        })]
    );
}

#[test]
fn split_takes_a_pane_and_a_direction() {
    let server = start_server();

    let run = run(&server, &["split", "--pane", "8", "--down"]);

    assert_eq!((run.code, run.stdout.as_str()), (0, "9\n"));
    assert_eq!(
        server.app.requests(),
        [Request::SplitPane(SplitPane {
            pane: PaneId::new(8),
            direction: SplitDirection::Down,
        })]
    );
}

#[test]
fn split_outside_a_pane_needs_a_pane() {
    let server = start_server();
    let env = CliEnv {
        pane: None,
        ..pane_env(&server)
    };

    let run = run_with(&env, "", &["split"]);

    assert_usage_error(&run, "--pane");
    assert_eq!(server.app.requests(), []);
}

#[test]
fn new_workspace_prints_the_workspace_and_its_pane() {
    let server = start_server();

    let plain = run(&server, &["new-workspace"]);
    let named = run(
        &server,
        &["new-workspace", "--name", "review", "--cwd", "/tmp/w"],
    );

    assert_eq!((plain.code, plain.stdout.as_str()), (0, "2 10\n"));
    assert_eq!(named.code, 0);
    assert_eq!(
        server.app.requests(),
        [
            Request::CreateWorkspace(CreateWorkspace {
                caller: Some(PaneId::new(3)),
                ..CreateWorkspace::default()
            }),
            Request::CreateWorkspace(CreateWorkspace {
                name: Some("review".into()),
                cwd: Some("/tmp/w".into()),
                caller: Some(PaneId::new(3)),
            }),
        ]
    );
}

/// The `caller` of each `workspace.create` the server's app was asked.
fn callers(server: &TestServer) -> Vec<Option<PaneId>> {
    server
        .app
        .requests()
        .into_iter()
        .map(|request| match request {
            Request::CreateWorkspace(request) => request.caller,
            other => panic!("unexpected request {other:?}"),
        })
        .collect()
}

#[test]
fn new_workspace_outside_a_pane_names_no_caller() {
    let server = start_server();
    let env = CliEnv {
        pane: None,
        ..pane_env(&server)
    };
    let path = server.path.to_str().unwrap();

    let from_env = run_with(&env, "", &["new-workspace"]);
    let no_env = run_with(&CliEnv::default(), "", &["--socket", path, "new-workspace"]);

    assert_eq!((from_env.code, no_env.code), (0, 0), "{}", no_env.stderr);
    assert_eq!(callers(&server), [None, None]);
}

#[test]
fn new_workspace_names_the_pane_only_to_the_app_the_pane_is_in() {
    let server = start_server();
    let other = start_server();
    let path = server.path.to_str().unwrap();
    let other_path = other.path.to_str().unwrap();

    // --socket naming the socket of the pane's own app changes nothing.
    let same = run(&server, &["--socket", path, "new-workspace"]);
    // Another app has its own panes, and pane 3 there is a different one.
    let elsewhere = run(&server, &["--socket", other_path, "new-workspace"]);

    assert_eq!((same.code, elsewhere.code), (0, 0), "{}", elsewhere.stderr);
    assert_eq!(callers(&server), [Some(PaneId::new(3))]);
    assert_eq!(callers(&other), [None]);
}

#[test]
fn new_workspace_refuses_a_pane_id_that_is_not_one() {
    let server = start_server();
    let env = CliEnv {
        pane: Some("three".into()),
        ..pane_env(&server)
    };

    let run = run_with(&env, "", &["new-workspace"]);

    assert_usage_error(&run, "BENTERM_PANE_ID");
    assert_eq!(server.app.requests(), []);
}

#[test]
fn new_workspace_makes_a_relative_cwd_absolute() {
    let server = start_server();
    let env = CliEnv {
        cwd: Some("/home/me/project".into()),
        ..pane_env(&server)
    };

    let relative = run_with(&env, "", &["new-workspace", "--cwd", "crates/ui"]);
    let absolute = run_with(&env, "", &["new-workspace", "--cwd", "/tmp/w"]);

    assert_eq!((relative.code, absolute.code), (0, 0));
    let cwds: Vec<Option<String>> = server
        .app
        .requests()
        .into_iter()
        .map(|request| match request {
            Request::CreateWorkspace(request) => request.cwd,
            other => panic!("unexpected request {other:?}"),
        })
        .collect();
    assert_eq!(
        cwds,
        [
            Some("/home/me/project/crates/ui".into()),
            Some("/tmp/w".into())
        ]
    );
}

#[test]
fn new_workspace_refuses_a_relative_cwd_when_it_does_not_know_where_it_runs() {
    let server = start_server();
    let env = CliEnv {
        cwd: None,
        ..pane_env(&server)
    };

    let run = run_with(&env, "", &["new-workspace", "--cwd", "crates/ui"]);

    assert_eq!(run.code, 1, "{}", run.stderr);
    assert!(run.stderr.contains("absolute"), "{}", run.stderr);
    assert!(server.app.requests().is_empty());
}

fn sent_text(server: &TestServer) -> Vec<(u64, String)> {
    server
        .app
        .requests()
        .into_iter()
        .map(|request| match request {
            Request::SendText(SendText { pane, text }) => (pane.get(), text),
            other => panic!("{other:?}"),
        })
        .collect()
}

#[test]
fn send_types_the_text_as_given_and_prints_nothing() {
    let server = start_server();

    let run = run(&server, &["send", "--pane", "5", "git status"]);

    assert_eq!(
        (run.code, run.stdout.as_str(), run.stderr.as_str()),
        (0, "", "")
    );
    assert_eq!(sent_text(&server), [(5, "git status".to_string())]);
}

#[test]
fn send_joins_several_words_and_presses_enter_when_asked() {
    let server = start_server();

    let run = run(
        &server,
        &["send", "--enter", "--pane", "5", "git", "status"],
    );

    assert_eq!(run.code, 0);
    assert_eq!(sent_text(&server), [(5, "git status\r".to_string())]);
}

#[test]
fn send_takes_everything_after_two_dashes_as_text() {
    let server = start_server();

    let run = run(&server, &["send", "--pane", "5", "--", "--enter", "--pane"]);

    assert_eq!(run.code, 0);
    assert_eq!(sent_text(&server), [(5, "--enter --pane".to_string())]);
}

#[test]
fn send_without_text_reads_it_from_standard_input() {
    let server = start_server();

    let run = run_with(
        &pane_env(&server),
        "line one\nline two\n",
        &["send", "--pane", "5"],
    );

    assert_eq!(run.code, 0);
    assert_eq!(
        sent_text(&server),
        [(5, "line one\nline two\n".to_string())]
    );
}

#[test]
fn send_and_read_do_not_fall_back_to_the_callers_pane() {
    let server = start_server();

    assert_usage_error(&run(&server, &["send", "hello"]), "--pane");
    assert_usage_error(&run(&server, &["read"]), "--pane");
    assert_eq!(server.app.requests(), []);
}

#[test]
fn read_prints_the_text_of_the_pane() {
    let server = start_server();

    let screen = run(&server, &["read", "--pane", "5"]);
    let tail = run(&server, &["read", "--pane", "5", "--lines", "40"]);

    assert_eq!(
        (screen.code, screen.stdout.as_str()),
        (0, "screen of pane 5\n")
    );
    assert_eq!(
        (tail.code, tail.stdout.as_str()),
        (0, "last 40 lines of pane 5\n")
    );
    assert_eq!(
        server.app.requests()[1],
        Request::ReadOutput(ReadOutput {
            pane: PaneId::new(5),
            lines: Some(40),
        })
    );
}

#[test]
fn an_error_from_the_app_is_printed_and_fails() {
    let server = start_server();

    let run = run(&server, &["read", "--pane", &MISSING_PANE.to_string()]);

    assert_eq!(run.code, 1);
    assert_eq!(run.stdout, "");
    assert_eq!(run.stderr, "benterm: no pane 404\n");
}

#[test]
fn the_socket_option_overrides_the_environment() {
    let server = start_server();
    let env = CliEnv {
        socket: Some("/nonexistent/benterm.sock".into()),
        pane: None,
        cwd: None,
        path: None,
        ..CliEnv::default()
    };
    let socket = server.path.to_str().unwrap();

    let before = run_with(&env, "", &["--socket", socket, "read", "--pane", "1"]);
    let after = run_with(&env, "", &["read", "--socket", socket, "--pane", "1"]);

    assert_eq!(
        (before.code, before.stdout.as_str()),
        (0, "screen of pane 1\n")
    );
    assert_eq!(
        (after.code, after.stdout.as_str()),
        (0, "screen of pane 1\n")
    );
}

#[test]
fn without_a_socket_the_command_says_how_to_give_one() {
    let run = run_with(&CliEnv::default(), "", &["read", "--pane", "1"]);

    assert_eq!(run.code, 1);
    assert!(run.stderr.contains("BENTERM_SOCKET"), "{}", run.stderr);
    assert!(run.stderr.contains("--socket"), "{}", run.stderr);
}

#[test]
fn a_socket_nobody_listens_on_fails_with_its_path() {
    let dir = private_dir();
    let path = dir.path().join("gone.sock");
    let env = CliEnv {
        socket: Some(path.clone().into_os_string()),
        pane: None,
        cwd: None,
        path: None,
        ..CliEnv::default()
    };

    let run = run_with(&env, "", &["read", "--pane", "1"]);

    assert_eq!(run.code, 1);
    assert!(run.stderr.contains("gone.sock"), "{}", run.stderr);
}

#[test]
fn bad_arguments_are_usage_errors_and_send_nothing() {
    let server = start_server();
    let cases: &[(&[&str], &str)] = &[
        (&[], "usage"),
        (&["explode"], "explode"),
        (&["split", "--sideways"], "--sideways"),
        (&["split", "--right", "--down"], "--down"),
        (&["split", "extra"], "extra"),
        (&["split", "--pane"], "--pane"),
        (&["split", "--pane", "left"], "left"),
        (&["split", "--lines", "3"], "--lines"),
        (&["read", "--pane", "1", "--lines", "many"], "many"),
        (&["read", "--pane", "1", "--enter"], "--enter"),
        (&["new-workspace", "--pane", "1"], "--pane"),
        (&["send", "--pane", "1", "--name", "x", "hi"], "--name"),
        (&["split", "read"], "read"),
    ];

    for (args, needle) in cases {
        assert_usage_error(&run(&server, args), needle);
    }
    assert_eq!(server.app.requests(), []);
}

#[test]
fn a_pane_variable_that_is_not_a_number_is_reported() {
    let server = start_server();
    let env = CliEnv {
        pane: Some("main".into()),
        ..pane_env(&server)
    };

    let run = run_with(&env, "", &["split"]);

    assert_usage_error(&run, "BENTERM_PANE_ID");
}

#[test]
fn help_prints_the_commands_and_succeeds() {
    for args in [&["--help"][..], &["-h"], &["help"]] {
        let run = run_with(&CliEnv::default(), "", args);

        assert_eq!(run.code, 0);
        assert_eq!(run.stderr, "");
        for command in ["split", "new-workspace", "send", "read", "--socket"] {
            assert!(run.stdout.contains(command), "{command}");
        }
    }
}

#[test]
fn the_app_can_tell_cli_commands_from_other_arguments() {
    for command in ["split", "new-workspace", "send", "read"] {
        assert!(cli::is_command(command), "{command}");
    }
    assert!(!cli::is_command("--socket"));
    assert!(!cli::is_command("some-dir"));
}

#[test]
fn an_empty_socket_variable_says_the_app_started_without_its_socket() {
    let env = CliEnv {
        socket: Some("".into()),
        pane: Some("3".into()),
        cwd: None,
        path: None,
        ..CliEnv::default()
    };

    let run = run_with(&env, "", &["read", "--pane", "1"]);

    assert_eq!(run.code, 1);
    assert!(
        run.stderr.contains("started without its socket API"),
        "{}",
        run.stderr
    );
    assert!(!run.stderr.contains("not a Benterm pane"), "{}", run.stderr);
}

#[test]
fn an_empty_socket_variable_gives_way_to_the_socket_option() {
    let server = start_server();
    let path = server.path.to_str().unwrap();
    let env = CliEnv {
        socket: Some("".into()),
        pane: None,
        cwd: None,
        path: None,
        ..CliEnv::default()
    };

    let run = run_with(&env, "", &["--socket", path, "read", "--pane", "1"]);

    assert_eq!(run.code, 0, "{}", run.stderr);
}

/// The one hook report the app got.
fn reported(server: &TestServer) -> ReportHook {
    match &server.app.requests()[..] {
        [Request::ReportHook(hook)] => hook.clone(),
        other => panic!("{other:?}"),
    }
}

/// A hook must never stand in the agent's way: it prints nothing the agent
/// would read and never fails, whatever happened.
fn assert_quiet(run: &Run) {
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert_eq!(run.stdout, "");
}

const STOP_INPUT: &str = r#"{"session_id":"0b1c","transcript_path":"/x/y.jsonl","cwd":"/x","hook_event_name":"Stop","stop_hook_active":false}"#;

#[test]
fn hook_reports_the_event_of_the_callers_pane_with_what_the_agent_passed() {
    let server = start_server();

    let run = run_with(
        &pane_env(&server),
        STOP_INPUT,
        &["hook", "Stop", "--agent", "claude"],
    );

    assert_quiet(&run);
    assert_eq!(run.stderr, "");
    assert_eq!(
        reported(&server),
        ReportHook {
            pane: PaneId::new(3),
            agent: "claude".into(),
            event: "Stop".into(),
            tool_name: None,
            notification_type: None,
            session_id: Some("0b1c".into()),
            agent_id: None,
            tool_input: None,
            tool_response: None,
        }
    );
}

#[test]
fn hook_passes_the_tool_the_notification_type_and_the_subagent() {
    let server = start_server();
    let input = r#"{"session_id":"s","hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"ls"},"notification_type":"idle_prompt","agent_id":"a1"}"#;

    let run = run_with(
        &pane_env(&server),
        input,
        &["hook", "PreToolUse", "--agent", "codex"],
    );

    assert_quiet(&run);
    let hook = reported(&server);
    assert_eq!(hook.agent, "codex");
    assert_eq!(hook.tool_name.as_deref(), Some("Bash"));
    assert_eq!(hook.notification_type.as_deref(), Some("idle_prompt"));
    assert_eq!(hook.agent_id.as_deref(), Some("a1"));
}

const TODO_INPUT: &str = r#"{"session_id":"s","hook_event_name":"PostToolUse","tool_name":"TodoWrite","tool_input":{"todos":[{"content":"Read","status":"pending"}]},"tool_response":{"oldTodos":[],"newTodos":[]}}"#;

#[test]
fn hook_passes_what_a_plan_tool_was_given_and_answered() {
    let server = start_server();

    let run = run_with(
        &pane_env(&server),
        TODO_INPUT,
        &["hook", "PostToolUse", "--agent", "claude"],
    );

    assert_quiet(&run);
    let hook = reported(&server);
    let input = hook.tool_input.expect("the input of the tool");
    assert_eq!(input["todos"][0]["content"], "Read");
    let response = hook.tool_response.expect("the answer of the tool");
    assert!(response["newTodos"].is_array());
}

#[test]
fn hook_passes_the_plan_of_codex() {
    let server = start_server();
    let input = r#"{"session_id":"s","hook_event_name":"PostToolUse","tool_name":"update_plan","tool_input":{"plan":[{"step":"Look","status":"pending"}]},"tool_response":"Plan updated"}"#;

    let run = run_with(
        &pane_env(&server),
        input,
        &["hook", "PostToolUse", "--agent", "codex"],
    );

    assert_quiet(&run);
    let hook = reported(&server);
    assert_eq!(hook.tool_input.unwrap()["plan"][0]["step"], "Look");
    assert_eq!(hook.tool_response.unwrap(), "Plan updated");
}

#[test]
fn hook_keeps_what_other_tools_were_given_to_itself() {
    let server = start_server();
    let input = r#"{"session_id":"s","hook_event_name":"PostToolUse","tool_name":"Bash","tool_input":{"command":"ls"},"tool_response":{"stdout":"a"}}"#;

    let run = run_with(
        &pane_env(&server),
        input,
        &["hook", "PostToolUse", "--agent", "claude"],
    );

    assert_quiet(&run);
    let hook = reported(&server);
    assert_eq!(hook.tool_name.as_deref(), Some("Bash"));
    assert_eq!((hook.tool_input, hook.tool_response), (None, None));
}

#[test]
fn hook_keeps_the_plan_of_a_subagent_to_itself() {
    let server = start_server();
    let input = TODO_INPUT.replace(r#""session_id":"s""#, r#""session_id":"s","agent_id":"a1""#);

    let run = run_with(
        &pane_env(&server),
        &input,
        &["hook", "PostToolUse", "--agent", "claude"],
    );

    assert_quiet(&run);
    let hook = reported(&server);
    assert_eq!(hook.agent_id.as_deref(), Some("a1"));
    assert_eq!((hook.tool_input, hook.tool_response), (None, None));
}

#[test]
fn hook_passes_a_plan_only_once_the_tool_has_run() {
    let server = start_server();
    let input = TODO_INPUT.replace("PostToolUse", "PreToolUse");

    let run = run_with(
        &pane_env(&server),
        &input,
        &["hook", "PreToolUse", "--agent", "claude"],
    );

    assert_quiet(&run);
    let hook = reported(&server);
    assert_eq!((hook.tool_input, hook.tool_response), (None, None));
}

#[test]
fn hook_reports_the_event_without_a_plan_too_long_to_show() {
    let server = start_server();
    let long = "x".repeat(300_000);
    let input = TODO_INPUT.replace("Read", &long);

    let run = run_with(
        &pane_env(&server),
        &input,
        &["hook", "PostToolUse", "--agent", "claude"],
    );

    assert_quiet(&run);
    let hook = reported(&server);
    assert_eq!(hook.tool_name.as_deref(), Some("TodoWrite"));
    assert_eq!((hook.tool_input, hook.tool_response), (None, None));
}

#[test]
fn hook_takes_the_event_from_the_input_when_it_is_not_named() {
    let server = start_server();

    let run = run_with(
        &pane_env(&server),
        STOP_INPUT,
        &["hook", "--agent", "claude"],
    );

    assert_quiet(&run);
    assert_eq!(reported(&server).event, "Stop");
}

#[test]
fn hook_reports_the_named_event_when_the_input_is_missing_or_broken() {
    for input in ["", "not json", "[1, 2]"] {
        let server = start_server();

        let run = run_with(
            &pane_env(&server),
            input,
            &["hook", "Stop", "--agent", "claude"],
        );

        assert_quiet(&run);
        let hook = reported(&server);
        assert_eq!((hook.event.as_str(), hook.session_id), ("Stop", None));
    }
}

#[test]
fn hook_is_silent_outside_a_pane() {
    let server = start_server();
    let no_socket = CliEnv {
        socket: None,
        ..pane_env(&server)
    };
    let no_pane = CliEnv {
        pane: None,
        ..pane_env(&server)
    };

    // Not even wrong arguments make a sound out there.
    for env in [&no_socket, &no_pane, &CliEnv::default()] {
        for args in [
            &["hook", "Stop", "--agent", "claude"][..],
            &["hook"],
            &["hook", "--bogus"],
        ] {
            let run = run_with(env, STOP_INPUT, args);
            assert_eq!(
                (run.code, run.stdout.as_str(), run.stderr.as_str()),
                (0, "", "")
            );
        }
    }
    assert_eq!(server.app.requests(), []);
}

#[test]
fn hook_never_fails_when_the_app_is_gone_or_refuses() {
    let dir = private_dir();
    let gone = CliEnv {
        socket: Some(dir.path().join("gone.sock").into_os_string()),
        pane: Some("3".into()),
        cwd: None,
        path: None,
        ..CliEnv::default()
    };
    assert_quiet(&run_with(
        &gone,
        STOP_INPUT,
        &["hook", "Stop", "--agent", "claude"],
    ));

    let server = start_server();
    let closed_pane = CliEnv {
        pane: Some(MISSING_PANE.to_string()),
        ..pane_env(&server)
    };
    assert_quiet(&run_with(
        &closed_pane,
        STOP_INPUT,
        &["hook", "Stop", "--agent", "claude"],
    ));
}

#[test]
fn hook_never_fails_on_arguments_it_cannot_use() {
    let server = start_server();
    for args in [
        &["hook", "Stop"][..],
        &["hook", "--agent"],
        &["hook", "--agent", "claude"],
        &["hook", "Stop", "--agent", "claude", "--bogus"],
        &["hook", "Stop", "Start", "--agent", "claude"],
    ] {
        let run = run_with(&pane_env(&server), "", args);
        assert_quiet(&run);
        // In a pane, the mistake is told where the agent does not read.
        assert!(!run.stderr.is_empty(), "{args:?}");
    }
    assert_eq!(server.app.requests(), []);
}

/// An input that is never finished, as from an agent that keeps the pipe
/// open.
struct Endless;

impl std::io::Read for Endless {
    fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        std::thread::sleep(std::time::Duration::from_secs(30));
        Ok(0)
    }
}

#[test]
fn hook_does_not_wait_for_an_input_that_never_ends() {
    let server = start_server();
    let args: Vec<String> = ["hook", "Stop", "--agent", "claude"]
        .map(String::from)
        .to_vec();
    let started = std::time::Instant::now();

    let code = cli::run(
        &args,
        &pane_env(&server),
        Endless,
        &mut Vec::new(),
        &mut Vec::new(),
    );

    assert_eq!(code, 0);
    assert!(started.elapsed() < std::time::Duration::from_secs(3));
    // The event itself is still reported, by its name.
    assert_eq!(reported(&server).event, "Stop");
}

#[test]
fn hook_is_listed_in_the_help() {
    let server = start_server();
    let run = run(&server, &["--help"]);
    assert!(
        run.stdout.contains("hook [EVENT] --agent"),
        "{}",
        run.stdout
    );
}

#[test]
fn a_value_that_reads_hook_is_not_the_hook_command() {
    let server = start_server();

    let run = run(&server, &["--name", "hook", "new-workspace"]);

    assert_eq!(
        (run.code, run.stdout.as_str()),
        (0, "2 10\n"),
        "{}",
        run.stderr
    );
    assert_eq!(
        server.app.requests(),
        [Request::CreateWorkspace(CreateWorkspace {
            name: Some("hook".into()),
            cwd: None,
            caller: Some(PaneId::new(3)),
        })]
    );
}

#[test]
fn hook_is_the_hook_command_wherever_its_options_stand() {
    let server = start_server();

    let run = run_with(
        &pane_env(&server),
        STOP_INPUT,
        &["--agent", "claude", "hook", "Stop"],
    );

    assert_quiet(&run);
    assert_eq!(reported(&server).event, "Stop");
}

#[test]
fn history_prints_one_line_of_json_for_each_record() {
    let server = start_server();

    let run = run(&server, &["history"]);

    assert_eq!(run.code, 0, "{}", run.stderr);
    let lines: Vec<serde_json::Value> = run
        .stdout
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0]["at_ms"], 1_000);
    assert_eq!(lines[0]["state"], "working");
    assert_eq!(lines[1]["state"], "done");
    assert_eq!(lines[1]["key"], "0123456789abcdef0123456789abcdef");
    assert_eq!(
        server.app.requests(),
        [Request::ReadAgentHistory(ReadAgentHistory::default())]
    );
}

#[test]
fn history_takes_the_time_to_start_after() {
    let server = start_server();

    let run_since = run(&server, &["history", "--since", "1000"]);

    assert_eq!(run_since.code, 0, "{}", run_since.stderr);
    assert_eq!(run_since.stdout.lines().count(), 1);
    assert_eq!(
        server.app.requests(),
        [Request::ReadAgentHistory(ReadAgentHistory {
            since_ms: Some(1_000)
        })]
    );
    assert_usage_error(&run(&server, &["history", "--since", "soon"]), "--since");
    assert_usage_error(&run(&server, &["history", "--pane", "3"]), "--pane");
}

#[test]
fn browse_opens_the_address_beside_the_callers_pane_and_prints_the_new_pane() {
    let server = start_server();

    let run = run(&server, &["browse", "http://127.0.0.1:8080/docs"]);

    assert_eq!(
        (run.code, run.stdout.as_str(), run.stderr.as_str()),
        (0, "4\n", "")
    );
    assert_eq!(
        server.app.requests(),
        [Request::OpenBrowser(benterm_ipc::OpenBrowser {
            url: "http://127.0.0.1:8080/docs".into(),
            pane: Some(PaneId::new(3)),
            direction: SplitDirection::Right,
        })]
    );
}

#[test]
fn browse_takes_a_pane_and_a_direction() {
    let server = start_server();

    let run = run(
        &server,
        &["browse", "--pane", "8", "--down", "https://example.com/"],
    );

    assert_eq!((run.code, run.stdout.as_str()), (0, "9\n"));
    assert_eq!(
        server.app.requests(),
        [Request::OpenBrowser(benterm_ipc::OpenBrowser {
            url: "https://example.com/".into(),
            pane: Some(PaneId::new(8)),
            direction: SplitDirection::Down,
        })]
    );
}

/// Outside a pane the app opens the browser beside the pane in use.
#[test]
fn browse_outside_a_pane_leaves_the_pane_to_the_app() {
    let server = start_server();
    let env = CliEnv {
        pane: None,
        ..pane_env(&server)
    };

    let run = run_with(&env, "", &["browse", "https://example.com/"]);

    assert_eq!((run.code, run.stdout.as_str()), (0, "1\n"));
    assert_eq!(
        server.app.requests(),
        [Request::OpenBrowser(benterm_ipc::OpenBrowser {
            url: "https://example.com/".into(),
            pane: None,
            direction: SplitDirection::Right,
        })]
    );
}

#[test]
fn browse_needs_one_address() {
    let server = start_server();
    assert_usage_error(&run(&server, &["browse"]), "browse needs URL");
    assert_usage_error(
        &run(
            &server,
            &["browse", "https://a.example/", "https://b.example/"],
        ),
        "https://b.example/",
    );
    assert_usage_error(
        &run(&server, &["browse", "--lines", "3", "https://a.example/"]),
        "--lines",
    );
    assert_eq!(server.app.requests(), []);
}

#[test]
fn the_help_tells_how_to_open_a_browser() {
    let server = start_server();
    let run = run(&server, &["--help"]);
    assert!(
        run.stdout
            .contains("browse [--pane ID] [--right | --down] URL")
    );
}

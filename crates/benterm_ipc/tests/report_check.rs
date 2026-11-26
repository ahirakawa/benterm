//! `benterm report-check`: the command of the Stop and PostToolUse
//! hooks that checks an agent's report. The inputs and the transcripts
//! are made up.

mod common;
#[path = "../../benterm_report/tests/common/mod.rs"]
mod made_up;

use std::io::Read;
use std::path::PathBuf;

use benterm_ipc::cli::{self, CliEnv};
use benterm_ipc::{PaneId, ReportChecked, ReportHook, ReportStatus, Request, RequestFrame};
use common::{TestServer, start_server};
use made_up::{Claude, Codex, good_report, touch};
use serde_json::{Value, json};

struct Run {
    code: u8,
    stdout: String,
    stderr: String,
}

/// A working directory, a folder for the counts and a settings file,
/// all in a folder of the test's own.
struct Session {
    dir: tempfile::TempDir,
}

impl Session {
    fn new() -> Self {
        let dir = made_up::folder();
        std::fs::create_dir(dir.path().join("work")).unwrap();
        Self { dir }
    }

    fn work(&self) -> PathBuf {
        self.dir.path().canonicalize().unwrap().join("work")
    }

    fn files(&self, names: &[&str]) -> Vec<PathBuf> {
        touch(&self.work(), names)
    }

    fn report_path(&self) -> PathBuf {
        self.work().join(".benterm/work.report.json")
    }

    fn write_report(&self, report: &Value) {
        let path = self.report_path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, report.to_string()).unwrap();
    }

    fn settings(&self, text: &str) {
        std::fs::write(self.dir.path().join("config.toml"), text).unwrap();
    }

    /// The environment of an agent in pane 3 of the server's app, a pane
    /// that was opened while the app checked reports.
    fn env(&self, server: &TestServer) -> CliEnv {
        CliEnv {
            socket: Some(server.path.clone().into_os_string()),
            pane: Some("3".into()),
            report_check: Some("on".into()),
            state_dir: Some(self.dir.path().join("state")),
            config_file: Some(self.dir.path().join("config.toml")),
            ..CliEnv::default()
        }
    }

    fn input(&self, event: &str, transcript: &str, turn: Value) -> String {
        let path = self.dir.path().join("transcript.jsonl");
        std::fs::write(&path, transcript).unwrap();
        let mut input = json!({
            "session_id": "made-up-session",
            "transcript_path": path,
            "cwd": self.work(),
            "hook_event_name": event,
            "last_assistant_message": "Done.",
        });
        input
            .as_object_mut()
            .unwrap()
            .extend(turn.as_object().unwrap().clone());
        input.to_string()
    }

    /// A turn of Claude Code that edited three files, which needs a
    /// report.
    fn large_turn(&self) -> Claude {
        let files = self.files(&["src/a.rs", "docs/plan.md", "docs/more.md"]);
        Claude::default()
            .prompt("p1", "do it")
            .edit(&files[0])
            .write(&files[1])
            .write(&files[2])
    }

    fn stop(&self, transcript: &str) -> String {
        self.input("Stop", transcript, json!({"prompt_id": "p1"}))
    }
}

fn report_for_three() -> Value {
    let mut report = good_report();
    let more = json!({"path": "docs/more.md", "summary": "More."});
    report["deliverables"].as_array_mut().unwrap().push(more);
    report
}

fn run_on(env: &CliEnv, stdin: impl Read + Send + 'static, args: &[&str]) -> Run {
    let args: Vec<String> = args.iter().map(|arg| arg.to_string()).collect();
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let code = cli::run_with_roles(&args, env, None, stdin, &mut stdout, &mut stderr);
    Run {
        code,
        stdout: String::from_utf8(stdout).unwrap(),
        stderr: String::from_utf8(stderr).unwrap(),
    }
}

fn run(env: &CliEnv, input: &str, agent: &str) -> Run {
    let stdin = std::io::Cursor::new(input.as_bytes().to_vec());
    run_on(env, stdin, &["report-check", "--agent", agent])
}

/// Standard output as the one JSON object a hook may print.
#[track_caller]
fn output(run: &Run) -> Value {
    assert_eq!(run.code, 0, "{}", run.stderr);
    assert_eq!(run.stdout.lines().count(), 1, "{:?}", run.stdout);
    serde_json::from_str(&run.stdout).expect("standard output is JSON")
}

/// The hook let the agent stop and said nothing.
#[track_caller]
fn assert_silent(run: &Run) {
    assert_eq!((run.code, run.stdout.as_str()), (0, ""), "{}", run.stderr);
}

fn checked(server: &TestServer) -> Vec<ReportChecked> {
    let requests = server.app.requests();
    requests
        .into_iter()
        .filter_map(|request| match request {
            Request::ReportChecked(checked) => Some(checked),
            _ => None,
        })
        .collect()
}

/// What the app was told, in order: `report` for a report, and the
/// name of the event for an event of a hook.
fn told(server: &TestServer) -> Vec<String> {
    let requests = server.app.requests();
    requests
        .into_iter()
        .map(|request| match request {
            Request::ReportChecked(_) => "report".to_owned(),
            Request::ReportHook(ReportHook { event, pane, .. }) => {
                assert_eq!(pane, PaneId::new(3));
                event
            }
            other => panic!("the app got {other:?}"),
        })
        .collect()
}

fn nothing() -> Vec<String> {
    Vec::new()
}

/// A reader that must not be read from.
struct Untouched;

impl Read for Untouched {
    fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        panic!("the input was read");
    }
}

#[test]
fn outside_the_app_it_reads_nothing_and_says_nothing() {
    let session = Session::new();
    let server = start_server();
    let inside = session.env(&server);
    let no_socket = CliEnv {
        socket: None,
        ..inside.clone()
    };
    let empty_socket = CliEnv {
        socket: Some("".into()),
        ..inside.clone()
    };
    let no_pane = CliEnv {
        pane: None,
        ..inside
    };
    for env in [no_socket, empty_socket, no_pane] {
        assert_silent(&run_on(
            &env,
            Untouched,
            &["report-check", "--agent", "claude"],
        ));
    }
    assert_eq!(told(&server), nothing());
}

/// Where the check is off, the command is the hook of the Stop and no
/// more: the agent stops, and the app is told so.
#[test]
fn a_terminal_turns_the_check_off_with_a_variable() {
    let session = Session::new();
    let server = start_server();
    let env = CliEnv {
        report_check: Some("off".into()),
        ..session.env(&server)
    };
    let turn = session.large_turn().text();
    assert_silent(&run(&env, &session.stop(&turn), "claude"));
    let after_tool = session.input("PostToolUse", &turn, json!({"prompt_id": "p1"}));
    assert_silent(&run(&env, &after_tool, "claude"));
    assert_eq!(told(&server), ["Stop"]);
}

/// The pane decides, once, when it opens: the app sets the variable in
/// the panes it opens while it checks reports. A pane opened before does
/// not check, whatever the settings say by now.
#[test]
fn a_pane_opened_while_the_check_was_off_does_not_check() {
    let session = Session::new();
    let server = start_server();
    session.settings("[report]\nenabled = true\n");
    let env = CliEnv {
        report_check: None,
        ..session.env(&server)
    };
    let stop = session.stop(&session.large_turn().text());
    assert_silent(&run(&env, &stop, "claude"));
    assert_eq!(told(&server), ["Stop"]);
}

/// And a pane opened while it was on goes on checking.
#[test]
fn a_pane_opened_while_the_check_was_on_checks_whatever_the_settings_say_now() {
    let session = Session::new();
    let server = start_server();
    session.settings("[report]\nenabled = false\n");
    let stop = session.stop(&session.large_turn().text());
    let out = output(&run(&session.env(&server), &stop, "claude"));
    assert_eq!(out["decision"], "block");
}

#[test]
fn a_small_turn_stops_without_a_word() {
    let session = Session::new();
    let server = start_server();
    let files = session.files(&["a.rs"]);
    let turn = Claude::default().prompt("p1", "fix it").edit(&files[0]);
    assert_silent(&run(
        &session.env(&server),
        &session.stop(&turn.text()),
        "claude",
    ));
    assert_eq!(told(&server), ["Stop"]);
}

/// The output that holds Claude Code at a Stop.
#[test]
fn a_large_turn_of_claude_code_without_a_report_is_blocked() {
    let session = Session::new();
    let server = start_server();
    let ran = run(
        &session.env(&server),
        &session.stop(&session.large_turn().text()),
        "claude",
    );
    let out = output(&ran);
    assert_eq!(out["decision"], "block");
    let reason = out["reason"].as_str().unwrap();
    assert!(
        reason.starts_with("The report does not follow the rules"),
        "{reason}"
    );
    assert!(reason.contains("No report JSON was written"), "{reason}");
    assert_eq!(out.as_object().unwrap().len(), 2, "{out}");
    assert_eq!(told(&server), nothing(), "a blocked agent has not stopped");
}

/// The same for Codex, which names the turn `turn_id` and patches its
/// files.
#[test]
fn a_large_turn_of_codex_without_a_report_is_blocked() {
    let session = Session::new();
    let server = start_server();
    session.files(&["src/a.rs", "docs/plan.md", "docs/more.md"]);
    let turn = Codex::default()
        .session(&session.work())
        .turn("t1", "do it")
        .patch(&["src/a.rs", "docs/plan.md", "docs/more.md"]);
    let stop = session.input("Stop", &turn.text(), json!({"turn_id": "t1"}));
    let out = output(&run(&session.env(&server), &stop, "codex"));
    assert_eq!(out["decision"], "block");
    assert!(
        out["reason"].as_str().unwrap().contains("No report JSON"),
        "{out}"
    );
}

#[test]
fn a_report_with_a_problem_is_blocked_with_the_problem() {
    let session = Session::new();
    let server = start_server();
    let mut report = report_for_three();
    report["decisions"][0]["recommend"] = json!("Neither");
    session.write_report(&report);
    let turn = session.large_turn().write(&session.report_path());
    let out = output(&run(
        &session.env(&server),
        &session.stop(&turn.text()),
        "claude",
    ));
    assert_eq!(out["decision"], "block");
    assert!(
        out["reason"].as_str().unwrap().contains("recommend"),
        "{out}"
    );
}

#[test]
fn a_report_that_passes_goes_to_the_app_for_the_agents_pane() {
    let session = Session::new();
    let server = start_server();
    session.write_report(&report_for_three());
    let turn = session.large_turn().write(&session.report_path());
    assert_silent(&run(
        &session.env(&server),
        &session.stop(&turn.text()),
        "claude",
    ));
    let got = checked(&server);
    assert_eq!(got.len(), 1);
    // The report first, so that it is there when the agent shows as done.
    assert_eq!(told(&server), ["report", "Stop"]);
    assert_eq!(got[0].session_id.as_deref(), Some("made-up-session"));
    assert_eq!(got[0].pane, PaneId::new(3));
    assert_eq!(got[0].agent, "claude");
    assert_eq!(got[0].report.title, "Made-up work");
    assert_eq!(got[0].report.status(), Some(ReportStatus::Pass));
    assert_eq!(got[0].report.decisions[0].question, "Keep the old one?");
    assert_eq!(got[0].problems, Vec::<String>::new());
}

/// At the limit the agent stops, and what is still wrong is shown with
/// the report: to the user in the terminal, and to the app.
#[test]
fn at_the_limit_the_agent_stops_and_the_problems_go_with_the_report() {
    let session = Session::new();
    let server = start_server();
    session.settings("[report]\nmax_rounds = 1\n");
    let mut report = report_for_three();
    report["title"] = json!("");
    session.write_report(&report);
    let stop = session.stop(&session.large_turn().write(&session.report_path()).text());
    let env = session.env(&server);
    assert_eq!(output(&run(&env, &stop, "claude"))["decision"], "block");

    let out = output(&run(&env, &stop, "claude"));
    assert_eq!(out.get("decision"), None, "{out}");
    let message = out["systemMessage"].as_str().unwrap();
    assert!(message.contains("title is empty"), "{message}");
    let got = checked(&server);
    assert_eq!(told(&server), ["report", "Stop"]);
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].problems.len(), 1);
    assert!(
        got[0].problems[0].contains("title"),
        "{:?}",
        got[0].problems
    );
}

#[test]
fn the_thresholds_are_the_ones_of_the_settings() {
    let session = Session::new();
    let server = start_server();
    session.settings("[report]\nedited_files = 1\n");
    let files = session.files(&["a.rs"]);
    let turn = Claude::default().prompt("p1", "fix it").edit(&files[0]);
    let out = output(&run(
        &session.env(&server),
        &session.stop(&turn.text()),
        "claude",
    ));
    assert_eq!(out["decision"], "block");
    assert!(
        out["reason"]
            .as_str()
            .unwrap()
            .contains("It edited 1 or more files"),
        "{out}"
    );
}

#[test]
fn after_a_tool_call_the_form_is_handed_over_once() {
    let session = Session::new();
    let server = start_server();
    let env = session.env(&server);
    let turn = session.large_turn().text();
    let after_tool = session.input(
        "PostToolUse",
        &turn,
        json!({"prompt_id": "p1", "tool_name": "Write"}),
    );
    let out = output(&run(&env, &after_tool, "claude"));
    assert_eq!(out["hookSpecificOutput"]["hookEventName"], "PostToolUse");
    let context = out["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap();
    assert!(context.contains("\"deliverables\""), "{context}");
    assert_eq!(out.as_object().unwrap().len(), 1, "{out}");

    assert_silent(&run(&env, &after_tool, "claude"));
    assert_eq!(told(&server), nothing());
}

#[test]
fn the_tool_calls_of_a_subagent_are_left_alone() {
    let session = Session::new();
    let server = start_server();
    let turn = session.large_turn().text();
    let input = session.input(
        "PostToolUse",
        &turn,
        json!({"prompt_id": "p1", "agent_id": "a1"}),
    );
    assert_silent(&run(&session.env(&server), &input, "claude"));
}

#[test]
fn another_event_is_left_alone() {
    let session = Session::new();
    let server = start_server();
    let input = session.input(
        "PreToolUse",
        &session.large_turn().text(),
        json!({"prompt_id": "p1"}),
    );
    assert_silent(&run(&session.env(&server), &input, "claude"));
}

/// Whatever goes wrong with the check itself, the agent stops: nothing
/// on standard output, exit status 0, and the reason on standard error.
#[test]
fn a_check_that_cannot_work_lets_the_agent_stop() {
    let session = Session::new();
    let server = start_server();
    let env = session.env(&server);
    let no_transcript =
        json!({"session_id": "s", "hook_event_name": "Stop", "cwd": session.work()});
    for input in ["", "not json", "[1]", &no_transcript.to_string()] {
        let ran = run(&env, input, "claude");
        assert_silent(&ran);
        assert!(
            ran.stderr.starts_with("benterm: report-check: "),
            "{input:?}: {:?}",
            ran.stderr
        );
    }
    // A Stop whose check could not work is a Stop all the same.
    assert_eq!(told(&server), ["Stop"]);
    let ran = run_on(&env, std::io::empty(), &["report-check", "--agent"]);
    assert_silent(&ran);
    assert!(cli::is_hook(&["report-check"]));
    assert!(cli::is_hook(&[
        "--socket",
        "x",
        "report-check",
        "--agent",
        "codex"
    ]));
}

/// The app being gone does not hold the agent either: the report passed.
#[test]
fn a_report_the_app_does_not_take_still_lets_the_agent_stop() {
    let session = Session::new();
    let server = start_server();
    session.write_report(&report_for_three());
    let stop = session.stop(&session.large_turn().write(&session.report_path()).text());
    let env = CliEnv {
        socket: Some(session.dir.path().join("no.sock").into_os_string()),
        ..session.env(&server)
    };
    let ran = run(&env, &stop, "claude");
    assert_silent(&ran);
    assert!(ran.stderr.contains("report-check"), "{:?}", ran.stderr);
}

#[test]
fn the_request_is_one_line_on_the_wire_and_reads_back() {
    let report = serde_json::from_value(report_for_three()).unwrap();
    let request = Request::ReportChecked(ReportChecked {
        pane: PaneId::new(7),
        agent: "codex".into(),
        session_id: Some("made-up-session".into()),
        report,
        problems: vec!["title is empty.".into()],
    });
    let line = RequestFrame::new(1, request.clone()).to_line();
    let value: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(value["method"], "pane.report_checked");
    assert_eq!(value["params"]["pane"], 7);
    assert_eq!(value["params"]["report"]["conclusion"], "pass");
    assert_eq!(RequestFrame::parse(&line).unwrap().request, request);

    // Without problems the field is left out, and reads back as none.
    let mut params = value["params"].clone();
    params.as_object_mut().unwrap().remove("problems");
    let line = json!({"id": 2, "method": "pane.report_checked", "params": params}).to_string();
    match RequestFrame::parse(&line).unwrap().request {
        Request::ReportChecked(checked) => assert_eq!(checked.problems, Vec::<String>::new()),
        other => panic!("{other:?}"),
    }
}

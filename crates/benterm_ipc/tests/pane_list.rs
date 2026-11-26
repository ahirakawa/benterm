//! Listing the panes: `pane.list` on the wire and `benterm list` on
//! the command line.

mod common;

use benterm_ipc::cli::{self, CliEnv};
use benterm_ipc::{
    AgentActivity, ListPanes, PaneId, PaneInfo, PaneList, Request, RequestFrame, ServerMessage,
    WaitingFor, WorkspaceId,
};
use common::start_server;
use serde_json::json;

#[test]
fn pane_list_takes_no_params() {
    let line = json!({"id": 1, "method": "pane.list"}).to_string();

    assert_eq!(
        RequestFrame::parse(&line).unwrap().request,
        Request::ListPanes(ListPanes {})
    );
}

#[test]
fn a_pane_behind_the_window_has_no_workspace_or_tab_on_the_wire() {
    let list = PaneList {
        panes: vec![
            PaneInfo {
                pane: PaneId::new(3),
                workspace: Some(WorkspaceId::new(1)),
                tab: Some(2),
                name: None,
                cwd: Some("/work".into()),
                agent: Some("claude".into()),
                state: AgentActivity::Waiting,
                reason: Some(WaitingFor::Permission),
            },
            PaneInfo {
                pane: PaneId::new(20),
                workspace: None,
                tab: None,
                name: Some("w1".into()),
                cwd: None,
                agent: None,
                state: AgentActivity::Unknown,
                reason: None,
            },
        ],
    };

    let line = ServerMessage::result(7, &list).to_line();

    let value: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(
        value["result"],
        json!({"panes": [
            {"pane": 3, "workspace": 1, "tab": 2, "name": null, "cwd": "/work",
             "agent": "claude", "state": "waiting", "reason": "permission"},
            {"pane": 20, "workspace": null, "tab": null, "name": "w1", "cwd": null,
             "agent": null, "state": "unknown", "reason": null},
        ]})
    );
}

#[test]
fn list_prints_a_line_for_each_pane() {
    let server = start_server();
    let env = CliEnv {
        socket: Some(server.path.clone().into_os_string()),
        pane: Some("3".into()),
        cwd: None,
        path: None,
        ..CliEnv::default()
    };
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    let code = cli::run(
        &["list".to_owned()],
        &env,
        std::io::empty(),
        &mut stdout,
        &mut stderr,
    );

    assert_eq!(
        (code, String::from_utf8(stderr).unwrap()),
        (0, String::new())
    );
    // The pane, where it is (workspace:tab, or background), the name it
    // was started under, the agent, the state and the working directory,
    // which goes last because it may hold spaces.
    assert_eq!(
        String::from_utf8(stdout).unwrap(),
        "3 1:1 - claude working /work/my repo\n\
         4 1:2 - - unknown -\n\
         20 background w1 codex waiting /work\n"
    );
    assert_eq!(server.app.requests(), [Request::ListPanes(ListPanes {})]);
}

#[test]
fn list_takes_no_arguments() {
    let server = start_server();
    let env = CliEnv {
        socket: Some(server.path.clone().into_os_string()),
        ..CliEnv::default()
    };
    let mut stderr = Vec::new();

    let code = cli::run(
        &["list".to_owned(), "--pane".to_owned(), "3".to_owned()],
        &env,
        std::io::empty(),
        &mut Vec::new(),
        &mut stderr,
    );

    assert_eq!(code, 2);
    assert!(String::from_utf8(stderr).unwrap().contains("--pane"));
    assert_eq!(server.app.requests(), []);
}

#[test]
fn list_with_json_prints_one_json_object_per_pane() {
    let server = start_server();
    let env = CliEnv {
        socket: Some(server.path.clone().into_os_string()),
        pane: Some("3".into()),
        ..CliEnv::default()
    };
    let run = |args: &[&str]| {
        let args: Vec<String> = args.iter().map(|arg| (*arg).to_owned()).collect();
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let code = cli::run(&args, &env, std::io::empty(), &mut stdout, &mut stderr);
        (
            code,
            String::from_utf8(stdout).unwrap(),
            String::from_utf8(stderr).unwrap(),
        )
    };

    let (plain_code, plain_out, plain_err) = run(&["list"]);
    let (json_code, json_out, json_err) = run(&["list", "--json"]);

    assert_eq!((plain_code, plain_err), (0, String::new()));
    assert_eq!((json_code, json_err), (0, String::new()));
    let lines: Vec<serde_json::Value> = json_out
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(lines.len(), plain_out.lines().count());
    assert_eq!(
        lines,
        vec![
            json!({
                "pane": 3, "workspace": 1, "tab": 1, "name": null,
                "cwd": "/work/my repo", "agent": "claude",
                "state": "working", "reason": null,
            }),
            json!({
                "pane": 4, "workspace": 1, "tab": 2, "name": null,
                "cwd": null, "agent": null,
                "state": "unknown", "reason": null,
            }),
            json!({
                "pane": 20, "workspace": null, "tab": null, "name": "w1",
                "cwd": "/work", "agent": "codex",
                "state": "waiting", "reason": "question",
            }),
        ]
    );
    for line in json_out.lines() {
        let _: PaneInfo = serde_json::from_str(line).unwrap();
    }
    assert_eq!(
        server.app.requests(),
        [
            Request::ListPanes(ListPanes {}),
            Request::ListPanes(ListPanes {}),
        ]
    );
}

#[test]
fn list_help_mentions_json_and_other_commands_reject_it() {
    let server = start_server();
    let env = CliEnv {
        socket: Some(server.path.clone().into_os_string()),
        pane: Some("3".into()),
        ..CliEnv::default()
    };

    let mut help_out = Vec::new();
    let help_code = cli::run(
        &["--help".to_owned()],
        &CliEnv::default(),
        std::io::empty(),
        &mut help_out,
        &mut Vec::new(),
    );
    assert_eq!(help_code, 0);
    assert!(
        String::from_utf8(help_out)
            .unwrap()
            .contains("list [--json]")
    );

    let mut stderr = Vec::new();
    let code = cli::run(
        &["split".to_owned(), "--json".to_owned()],
        &env,
        std::io::empty(),
        &mut Vec::new(),
        &mut stderr,
    );
    assert_eq!(code, 2);
    assert!(String::from_utf8(stderr).unwrap().contains("--json"));
    assert_eq!(server.app.requests(), []);
}

//! The client calls one operation at a time and waits for its answer.

mod common;

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::thread;

use benterm_ipc::{
    AgentActivity, AgentStateChanged, Client, ClientError, CreateWorkspace, ErrorCode,
    PaneAgentState, PaneClosed, PaneCreated, PaneId, PaneOutput, ReadAgentState, ReadOutput,
    ReportHook, Request, SendText, ServerMessage, SplitDirection, SplitPane, SubscribeAgentState,
    WorkspaceCreated, WorkspaceId,
};
use common::{MISSING_PANE, agent_state_of, private_dir, start_server};

#[test]
fn each_operation_returns_its_typed_answer() {
    let server = start_server();
    let mut client = Client::connect(&server.path).unwrap();

    let split = client
        .call(&SplitPane {
            pane: PaneId::new(3),
            direction: SplitDirection::Right,
        })
        .unwrap();
    let workspace = client.call(&CreateWorkspace::default()).unwrap();
    client
        .call(&SendText {
            pane: PaneId::new(4),
            text: "make test\r".into(),
        })
        .unwrap();
    let output = client
        .call(&ReadOutput {
            pane: PaneId::new(4),
            lines: Some(5),
        })
        .unwrap();

    assert_eq!(
        split,
        PaneCreated {
            pane: PaneId::new(4)
        }
    );
    assert_eq!(
        workspace,
        WorkspaceCreated {
            workspace: WorkspaceId::new(2),
            pane: PaneId::new(10)
        }
    );
    assert_eq!(
        output,
        PaneOutput {
            text: "last 5 lines of pane 4".into()
        }
    );
    assert_eq!(server.app.requests().len(), 4);
    assert!(matches!(server.app.requests()[2], Request::SendText(_)));
}

#[test]
fn an_error_from_the_app_is_returned_with_its_code() {
    let server = start_server();
    let mut client = Client::connect(&server.path).unwrap();

    let error = client
        .call(&ReadOutput {
            pane: PaneId::new(MISSING_PANE),
            lines: None,
        })
        .unwrap_err();

    let ClientError::Api(error) = error else {
        panic!("{error:?}");
    };
    assert_eq!(error.code, ErrorCode::UnknownPane);
    assert_eq!(error.message, "no pane 404");
}

/// A server that reads one request and answers with the given lines, where
/// `{id}` stands for the id of the request.
fn scripted_server(lines: &'static [&'static str]) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = private_dir();
    let path = dir.path().join("fake.sock");
    let listener = UnixListener::bind(&path).unwrap();
    thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = String::new();
        BufReader::new(stream.try_clone().unwrap())
            .read_line(&mut request)
            .unwrap();
        let request: serde_json::Value = serde_json::from_str(&request).unwrap();
        for line in lines {
            let line = line.replace("{id}", &request["id"].to_string());
            stream.write_all(line.as_bytes()).unwrap();
            stream.write_all(b"\n").unwrap();
        }
    });
    (dir, path)
}

fn read_pane_one(path: &std::path::Path) -> Result<PaneOutput, ClientError> {
    Client::connect(path).unwrap().call(&ReadOutput {
        pane: PaneId::new(1),
        lines: None,
    })
}

#[test]
fn notifications_and_answers_to_other_requests_are_skipped() {
    let (_dir, path) = scripted_server(&[
        r#"{"event":"agent.state","params":{"pane":2}}"#,
        r#"{"id":987654,"result":{"text":"not yours"}}"#,
        r#"{"id":{id},"result":{"text":"yours"}}"#,
    ]);

    assert_eq!(read_pane_one(&path).unwrap().text, "yours");
}

#[test]
fn a_server_that_closes_before_answering_is_an_error() {
    let (_dir, path) = scripted_server(&[]);

    assert!(matches!(read_pane_one(&path), Err(ClientError::Closed)));
}

#[test]
fn an_answer_of_the_wrong_shape_is_an_error() {
    let (_dir, path) = scripted_server(&[r#"{"id":{id},"result":{"words":3}}"#]);

    assert!(matches!(
        read_pane_one(&path),
        Err(ClientError::Protocol(_))
    ));
}

#[test]
fn a_line_that_is_not_a_message_is_an_error() {
    let (_dir, path) = scripted_server(&["welcome to the wrong server"]);

    assert!(matches!(
        read_pane_one(&path),
        Err(ClientError::Protocol(_))
    ));
}

#[test]
fn connecting_to_a_missing_socket_fails() {
    let dir = private_dir();

    assert!(Client::connect(dir.path().join("nothing.sock")).is_err());
}

#[test]
fn many_clients_calling_at_once_each_get_their_own_answers() {
    let server = start_server();
    let callers: Vec<_> = (0..8u64)
        .map(|pane| {
            let path = server.path.clone();
            thread::spawn(move || {
                let mut client = Client::connect(&path).unwrap();
                for lines in 0..100u32 {
                    let output = client
                        .call(&ReadOutput {
                            pane: PaneId::new(pane),
                            lines: Some(lines),
                        })
                        .unwrap();
                    assert_eq!(output.text, format!("last {lines} lines of pane {pane}"));
                }
            })
        })
        .collect();

    for caller in callers {
        caller.join().unwrap();
    }
}

#[test]
fn the_agent_operations_return_their_typed_answers() {
    let server = start_server();
    let mut client = Client::connect(&server.path).unwrap();

    client
        .call(&ReportHook {
            pane: PaneId::new(3),
            agent: "codex".into(),
            event: "Stop".into(),
            tool_name: None,
            notification_type: None,
            session_id: None,
            agent_id: None,
            tool_input: None,
            tool_response: None,
        })
        .unwrap();
    let state = client
        .call(&ReadAgentState {
            pane: PaneId::new(3),
        })
        .unwrap();

    assert_eq!(state, agent_state_of(PaneId::new(3)));
    assert!(matches!(
        server.app.requests()[0],
        Request::ReportHook(ReportHook { .. })
    ));
}

#[test]
fn events_are_read_in_order_also_those_that_came_before_an_answer() {
    let server = start_server();
    let mut client = Client::connect(&server.path).unwrap();
    client.call(&SubscribeAgentState {}).unwrap();
    let sinks = server.app.wait_for_sinks(1);
    let changed = |pane| {
        AgentStateChanged(PaneAgentState {
            pane: PaneId::new(pane),
            agent: None,
            state: AgentActivity::Unknown,
            reason: None,
        })
    };

    // The first event is on the wire before the answer to the read.
    assert!(sinks[0].send(&changed(1)));
    client
        .call(&ReadAgentState {
            pane: PaneId::new(3),
        })
        .unwrap();
    assert!(sinks[0].send(&PaneClosed {
        pane: PaneId::new(2)
    }));

    let first = client.next_event().unwrap();
    assert_eq!(first.as_event(), Some(changed(1)));
    let second = client.next_event().unwrap();
    assert_eq!(
        second.as_event(),
        Some(PaneClosed {
            pane: PaneId::new(2)
        })
    );
    assert!(matches!(first, ServerMessage::Event { .. }));
}

#[test]
fn waiting_for_an_event_ends_when_the_app_closes_the_connection() {
    let server = start_server();
    let mut client = Client::connect(&server.path).unwrap();
    client.call(&SubscribeAgentState {}).unwrap();

    server.server.shutdown();

    assert!(client.next_event().is_err());
}

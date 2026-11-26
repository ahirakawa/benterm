//! A request line has a limit on its length, and once it has begun, a time
//! to arrive in. Ordinary idle clients and the connection count are also bounded.

mod common;

use std::io::Write;
use std::thread;
use std::time::Duration;

use benterm_ipc::cli::{self, CliEnv};
use benterm_ipc::{AgentActivity, AgentStateChanged, PaneAgentState, PaneId, ServerLimits};
use common::{RawClient, TestServer, start_server_with, wait_until};
use serde_json::Value;

/// Short enough to keep the tests quick, long enough that no line is cut
/// while it is being written.
const SHORT: Duration = Duration::from_millis(200);

/// A request for the fake app.
const REQUEST: &str = r#"{"id":1,"method":"pane.read","params":{"pane":1}}"#;

/// [`REQUEST`] padded with spaces to `length` bytes.
fn read_request(length: usize) -> String {
    assert!(REQUEST.len() <= length);
    format!("{REQUEST:<length$}")
}

/// Asserts that the server sent an error without an id and then closed
/// the connection, and that the connection's threads have ended.
fn assert_cut_off(server: &TestServer, client: &mut RawClient, reason: &str) {
    let error = client.read().expect("the server sent no error");
    assert_eq!(error["id"], Value::Null, "{error}");
    assert_eq!(error["error"]["code"], "invalid_request", "{error}");
    let message = error["error"]["message"].as_str().unwrap();
    assert!(message.contains(reason), "{message}");
    assert_eq!(client.read(), None);
    wait_until(|| server.server.connection_count() == 0);
}

#[test]
fn a_line_one_byte_over_the_limit_is_refused_and_the_connection_closed() {
    let server = start_server_with(ServerLimits {
        max_line: 64,
        ..ServerLimits::default()
    });
    let mut client = RawClient::connect(&server.path);

    client.stream.write_all(&[b'x'; 65]).unwrap();

    assert_cut_off(&server, &mut client, "64 bytes");
    assert!(server.app.requests().is_empty());
}

#[test]
fn a_whole_line_over_the_limit_is_refused_too() {
    let server = start_server_with(ServerLimits {
        max_line: 64,
        ..ServerLimits::default()
    });
    let mut client = RawClient::connect(&server.path);

    client.send(&read_request(65));

    assert_cut_off(&server, &mut client, "64 bytes");
    assert!(server.app.requests().is_empty());
}

#[test]
fn a_request_exactly_as_long_as_the_limit_is_answered() {
    let server = start_server_with(ServerLimits {
        max_line: 64,
        ..ServerLimits::default()
    });
    let mut client = RawClient::connect(&server.path);

    let response = client.call(&read_request(64));

    assert_eq!(response["result"]["text"], "screen of pane 1");
}

#[test]
fn a_client_that_stops_in_the_middle_of_a_line_is_cut_off_after_the_time() {
    let server = start_server_with(ServerLimits {
        partial_line_timeout: SHORT,
        ..ServerLimits::default()
    });
    let mut client = RawClient::connect(&server.path);

    client.stream.write_all(b"{\"i").unwrap();

    assert_cut_off(&server, &mut client, "did not come");
}

#[test]
fn the_time_for_a_line_counts_from_its_own_first_byte() {
    let server = start_server_with(ServerLimits {
        partial_line_timeout: SHORT,
        ..ServerLimits::default()
    });
    let mut client = RawClient::connect(&server.path);
    let (head, tail) = REQUEST.split_at(10);

    // A whole line and the start of the next arrive together; the rest of
    // the next comes well within its time.
    client
        .stream
        .write_all(format!("{REQUEST}\n{head}").as_bytes())
        .unwrap();
    assert_eq!(client.read().unwrap()["id"], 1);
    client.send(tail);

    assert_eq!(client.read().unwrap()["id"], 1);
}

#[test]
fn a_watcher_that_keeps_quiet_longer_than_the_time_still_gets_events() {
    let server = start_server_with(ServerLimits {
        partial_line_timeout: SHORT,
        idle_timeout: SHORT,
        ..ServerLimits::default()
    });
    let mut watcher = RawClient::connect(&server.path);
    watcher.call(r#"{"id":1,"method":"pane.subscribe_agent_state"}"#);
    let sinks = server.app.wait_for_sinks(1);

    thread::sleep(SHORT * 3);
    assert!(sinks[0].send(&AgentStateChanged(PaneAgentState {
        pane: PaneId::new(2),
        agent: Some("claude".into()),
        state: AgentActivity::Done,
        reason: None,
    })));

    assert_eq!(watcher.read().unwrap()["params"]["pane"], 2);
    assert_eq!(server.server.connection_count(), 1);
}

#[test]
fn an_idle_client_is_disconnected_and_frees_a_connection_slot() {
    let server = start_server_with(ServerLimits {
        max_connections: 1,
        idle_timeout: Duration::from_secs(1),
        ..ServerLimits::default()
    });
    let mut idle = RawClient::connect(&server.path);
    wait_until(|| server.server.connection_count() == 1);
    let mut excess = RawClient::connect(&server.path);
    assert_eq!(excess.read(), None);
    assert_eq!(server.server.connection_count(), 1);

    assert_eq!(idle.read(), None);
    wait_until(|| server.server.connection_count() == 0);
    let mut replacement = RawClient::connect(&server.path);
    assert_eq!(replacement.call(REQUEST)["id"], 1);
    assert_eq!(replacement.read(), None);
}

#[test]
fn blank_lines_do_not_keep_a_connection_slot_open() {
    let server = start_server_with(ServerLimits {
        max_connections: 1,
        idle_timeout: SHORT * 2,
        ..ServerLimits::default()
    });
    let mut client = RawClient::connect(&server.path);
    wait_until(|| server.server.connection_count() == 1);

    for _ in 0..8 {
        if client.stream.write_all(b" \t\r\n").is_err() {
            break;
        }
        thread::sleep(SHORT / 2);
    }

    assert_eq!(server.server.connection_count(), 0);
    assert_eq!(client.read(), None);
    let mut replacement = RawClient::connect(&server.path);
    assert_eq!(replacement.call(REQUEST)["id"], 1);
}

#[test]
fn a_client_that_keeps_quiet_between_requests_is_not_cut_off() {
    let server = start_server_with(ServerLimits {
        partial_line_timeout: SHORT,
        ..ServerLimits::default()
    });
    let mut client = RawClient::connect(&server.path);
    assert_eq!(client.call(REQUEST)["id"], 1);

    thread::sleep(SHORT * 3);
    let response = client.call(r#"{"id":2,"method":"pane.read","params":{"pane":1}}"#);

    assert_eq!(response["id"], 2);
}

#[test]
fn the_limits_are_well_above_the_longest_request_the_cli_sends() {
    let limits = ServerLimits::default();
    // The hook forwards at most a plan of 64 KiB; a message for an agent
    // is typed or pasted by a person.
    assert!(limits.max_line >= 8 << 20);
    assert!(limits.partial_line_timeout >= Duration::from_secs(10));
}

#[test]
fn the_cli_tells_why_the_app_cut_it_off_and_fails() {
    let server = start_server_with(ServerLimits {
        max_line: 64,
        ..ServerLimits::default()
    });
    let env = CliEnv {
        socket: Some(server.path.clone().into_os_string()),
        pane: Some("3".into()),
        cwd: None,
        path: None,
        ..CliEnv::default()
    };
    // Far more than the socket's buffers hold, so the app stops reading
    // while the command is still writing.
    let text = "x".repeat(4 << 20);
    let args: Vec<String> = ["send", "--pane", "5", "--"]
        .iter()
        .map(|arg| arg.to_string())
        .collect();
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    let code = cli::run(
        &args,
        &env,
        std::io::Cursor::new(text.into_bytes()),
        &mut stdout,
        &mut stderr,
    );

    let stderr = String::from_utf8(stderr).unwrap();
    assert_ne!(code, 0);
    assert!(stderr.contains("64 bytes"), "{stderr}");
    assert!(server.app.requests().is_empty());
}

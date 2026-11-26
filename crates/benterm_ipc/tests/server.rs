//! The server answers requests from many clients at once.

mod common;

use std::io::Write;
use std::net::Shutdown;
use std::thread;

use benterm_ipc::{
    AgentActivity, AgentStateChanged, CreateWorkspace, PaneAgentState, PaneClosed, PaneId,
    PaneOutput, ReadOutput, Request, SendText, ServerLimits, SplitDirection, SplitPane,
};
use common::{
    DROPPED_PANE, HELD_PANE, MISSING_PANE, RawClient, agent_state_of, start_server,
    start_server_with, wait_until,
};
use serde_json::{Value, json};

#[test]
fn splitting_a_pane_answers_with_the_new_pane() {
    let server = start_server();
    let mut client = RawClient::connect(&server.path);

    let response =
        client.call(r#"{"id":1,"method":"pane.split","params":{"pane":3,"direction":"down"}}"#);

    assert_eq!(response, json!({"id": 1, "result": {"pane": 4}}));
    assert_eq!(
        server.app.requests(),
        [Request::SplitPane(SplitPane {
            pane: PaneId::new(3),
            direction: SplitDirection::Down,
        })]
    );
}

#[test]
fn creating_a_workspace_answers_with_the_workspace_and_its_pane() {
    let server = start_server();
    let mut client = RawClient::connect(&server.path);

    let response = client
        .call(r#"{"id":2,"method":"workspace.create","params":{"name":"review","cwd":"/tmp"}}"#);

    assert_eq!(
        response,
        json!({"id": 2, "result": {"workspace": 2, "pane": 10}})
    );
    assert_eq!(
        server.app.requests(),
        [Request::CreateWorkspace(CreateWorkspace {
            name: Some("review".into()),
            cwd: Some("/tmp".into()),
            caller: None,
        })]
    );
}

#[test]
fn sending_text_passes_it_on_unchanged() {
    let server = start_server();
    let mut client = RawClient::connect(&server.path);

    let response =
        client.call(r#"{"id":3,"method":"pane.send_text","params":{"pane":5,"text":"ls\r"}}"#);

    assert_eq!(response, json!({"id": 3, "result": null}));
    assert_eq!(
        server.app.requests(),
        [Request::SendText(SendText {
            pane: PaneId::new(5),
            text: "ls\r".into(),
        })]
    );
}

#[test]
fn reading_a_pane_answers_with_its_text() {
    let server = start_server();
    let mut client = RawClient::connect(&server.path);

    let screen = client.call(r#"{"id":4,"method":"pane.read","params":{"pane":5}}"#);
    let tail = client.call(r#"{"id":5,"method":"pane.read","params":{"pane":5,"lines":30}}"#);

    assert_eq!(
        screen,
        json!({"id": 4, "result": {"text": "screen of pane 5"}})
    );
    assert_eq!(
        tail,
        json!({"id": 5, "result": {"text": "last 30 lines of pane 5"}})
    );
}

#[test]
fn an_error_from_the_app_comes_back_with_the_id() {
    let server = start_server();
    let mut client = RawClient::connect(&server.path);

    let response = client.call(&format!(
        r#"{{"id":6,"method":"pane.read","params":{{"pane":{MISSING_PANE}}}}}"#
    ));

    assert_eq!(
        response,
        json!({"id": 6, "error": {"code": "unknown_pane", "message": "no pane 404"}})
    );
}

#[test]
fn a_bad_line_gets_an_error_and_the_connection_stays_usable() {
    let server = start_server();
    let mut client = RawClient::connect(&server.path);

    let not_json = client.call("hello");
    let unknown = client.call(r#"{"id":8,"method":"pane.explode"}"#);
    let good = client.call(r#"{"id":9,"method":"pane.read","params":{"pane":1}}"#);

    assert_eq!(not_json["id"], Value::Null);
    assert_eq!(not_json["error"]["code"], "invalid_request");
    assert_eq!(unknown["id"], 8);
    assert_eq!(unknown["error"]["code"], "unknown_method");
    assert_eq!(good["result"]["text"], "screen of pane 1");
    assert_eq!(server.app.requests().len(), 1);
}

#[test]
fn blank_lines_are_skipped() {
    let server = start_server();
    let mut client = RawClient::connect(&server.path);

    client.send("");
    let response = client.call(r#"{"id":1,"method":"pane.read","params":{"pane":1}}"#);

    assert_eq!(response["id"], 1);
}

#[test]
fn requests_sent_in_one_write_are_all_answered_in_order() {
    let server = start_server();
    let mut client = RawClient::connect(&server.path);

    let mut batch = String::new();
    for id in 0..100 {
        batch.push_str(&format!(
            "{{\"id\":{id},\"method\":\"pane.read\",\"params\":{{\"pane\":{id}}}}}\n"
        ));
    }
    client.stream.write_all(batch.as_bytes()).unwrap();

    for id in 0..100 {
        let response = client.read().unwrap();
        assert_eq!(response["id"], id);
        assert_eq!(response["result"]["text"], format!("screen of pane {id}"));
    }
}

#[test]
fn a_request_may_be_answered_later_and_out_of_order() {
    let server = start_server();
    let mut client = RawClient::connect(&server.path);
    let held = |id: u64, lines: u32| {
        format!(
            r#"{{"id":{id},"method":"pane.read","params":{{"pane":{HELD_PANE},"lines":{lines}}}}}"#
        )
    };

    client.send(&held(1, 10));
    client.send(&held(2, 20));
    server.app.wait_for_held(2);
    // A request sent while others are waiting is answered right away.
    let quick = client.call(r#"{"id":3,"method":"pane.read","params":{"pane":7}}"#);
    assert_eq!(quick["id"], 3);

    // Answer the two waiting requests from another thread, newest first.
    let mut waiting = server.app.take_held();
    waiting.reverse();
    thread::spawn(move || {
        for (request, reply) in waiting {
            reply.ok(PaneOutput {
                text: format!("answer to lines {}", request.lines.unwrap()),
            });
        }
    })
    .join()
    .unwrap();

    assert_eq!(
        client.read().unwrap(),
        json!({"id": 2, "result": {"text": "answer to lines 20"}})
    );
    assert_eq!(
        client.read().unwrap(),
        json!({"id": 1, "result": {"text": "answer to lines 10"}})
    );
}

#[test]
fn a_request_the_app_drops_gets_an_error_instead_of_silence() {
    let server = start_server();
    let mut client = RawClient::connect(&server.path);

    let response = client.call(&format!(
        r#"{{"id":4,"method":"pane.read","params":{{"pane":{DROPPED_PANE}}}}}"#
    ));

    assert_eq!(response["id"], 4);
    assert_eq!(response["error"]["code"], "internal");
}

#[test]
fn clients_using_the_same_ids_at_once_each_get_their_own_answers() {
    let server = start_server();
    let clients: Vec<_> = (0..8u64)
        .map(|pane| {
            let path = server.path.clone();
            thread::spawn(move || {
                let mut client = RawClient::connect(&path);
                for id in 0..200u64 {
                    let response = client.call(&format!(
                        r#"{{"id":{id},"method":"pane.read","params":{{"pane":{pane},"lines":{id}}}}}"#
                    ));
                    assert_eq!(response["id"], id);
                    assert_eq!(
                        response["result"]["text"],
                        format!("last {id} lines of pane {pane}")
                    );
                }
            })
        })
        .collect();

    for client in clients {
        client.join().unwrap();
    }
    assert_eq!(server.app.requests().len(), 8 * 200);
}

#[test]
fn held_requests_from_two_clients_with_the_same_id_are_not_mixed_up() {
    let server = start_server();
    let mut first = RawClient::connect(&server.path);
    let mut second = RawClient::connect(&server.path);
    let held = |lines: u32| {
        format!(
            r#"{{"id":1,"method":"pane.read","params":{{"pane":{HELD_PANE},"lines":{lines}}}}}"#
        )
    };

    first.send(&held(111));
    server.app.wait_for_held(1);
    second.send(&held(222));
    server.app.wait_for_held(2);
    for (request, reply) in server.app.take_held().into_iter().rev() {
        reply.ok(PaneOutput {
            text: format!("for {}", request.lines.unwrap()),
        });
    }

    assert_eq!(first.read().unwrap()["result"]["text"], "for 111");
    assert_eq!(second.read().unwrap()["result"]["text"], "for 222");
}

#[test]
fn a_client_that_leaves_with_requests_waiting_does_not_disturb_the_others() {
    let server = start_server();
    let mut leaver = RawClient::connect(&server.path);
    let mut stayer = RawClient::connect(&server.path);

    leaver.send(&format!(
        r#"{{"id":1,"method":"pane.read","params":{{"pane":{HELD_PANE}}}}}"#
    ));
    // The leaver dies in the middle of writing its next request.
    leaver
        .stream
        .write_all(br#"{"id":2,"method":"pane.re"#)
        .unwrap();
    server.app.wait_for_held(1);
    drop(leaver);

    let before = stayer.call(r#"{"id":1,"method":"pane.read","params":{"pane":1}}"#);
    // Answering the client that is gone is harmless.
    for (request, reply) in server.app.take_held() {
        reply.ok(PaneOutput {
            text: common::output_of(&request),
        });
    }
    let after = stayer.call(r#"{"id":2,"method":"pane.read","params":{"pane":2}}"#);

    assert_eq!(before["result"]["text"], "screen of pane 1");
    assert_eq!(after["result"]["text"], "screen of pane 2");
    // The half-written request never reached the app.
    assert_eq!(server.app.requests().len(), 3);
}

#[test]
fn a_client_that_stops_reading_blocks_neither_the_app_nor_the_others() {
    let server = start_server_with(ServerLimits {
        max_pending_bytes: 384 << 10,
        ..ServerLimits::default()
    });
    let mut stalled = RawClient::connect(&server.path);
    let mut other = RawClient::connect(&server.path);

    // The answers exceed the socket buffer while staying within the outbox budget.
    let count = 12;
    for id in 0..count {
        stalled.send(&format!(
            r#"{{"id":{id},"method":"pane.read","params":{{"pane":{HELD_PANE}}}}}"#
        ));
    }
    server.app.wait_for_held(count);
    let big = "x".repeat(24 * 1024);
    // Answering runs on this thread, like on the app's main thread. It must
    // return although the stalled client's socket is full.
    for (_, reply) in server.app.take_held() {
        reply.ok(PaneOutput { text: big.clone() });
    }

    let response = other.call(r#"{"id":1,"method":"pane.read","params":{"pane":1}}"#);
    assert_eq!(response["result"]["text"], "screen of pane 1");

    // The stalled client can still catch up.
    for _ in 0..count {
        assert_eq!(stalled.read().unwrap()["result"]["text"], big.as_str());
    }
}

#[test]
fn a_client_that_only_stops_writing_still_gets_its_answers() {
    let server = start_server();
    let mut client = RawClient::connect(&server.path);

    client.send(r#"{"id":1,"method":"pane.read","params":{"pane":1}}"#);
    client.stream.shutdown(Shutdown::Write).unwrap();

    assert_eq!(client.read().unwrap()["id"], 1);
    assert_eq!(client.read(), None);
}

#[test]
fn shutting_down_closes_every_connection() {
    let server = start_server();
    let mut idle = RawClient::connect(&server.path);
    let mut waiting = RawClient::connect(&server.path);
    waiting.send(&format!(
        r#"{{"id":1,"method":"pane.read","params":{{"pane":{HELD_PANE}}}}}"#
    ));
    server.app.wait_for_held(1);

    server.server.shutdown();

    assert_eq!(idle.read(), None);
    assert_eq!(waiting.read(), None);
    // The app may still answer after the server is gone.
    for (_, reply) in server.app.take_held() {
        reply.ok(PaneOutput {
            text: String::new(),
        });
    }
}

#[test]
fn the_server_keeps_serving_after_many_clients_came_and_went() {
    let server = start_server();

    for round in 0..200u64 {
        let mut client = RawClient::connect(&server.path);
        let response = client.call(&format!(
            r#"{{"id":{round},"method":"pane.read","params":{{"pane":{round}}}}}"#
        ));
        assert_eq!(response["id"], round);
    }

    wait_until(|| server.server.connection_count() == 0);
}

#[test]
fn a_request_struct_reaches_the_app_as_sent() {
    let server = start_server();
    let mut client = RawClient::connect(&server.path);

    client.call(r#"{"id":1,"method":"pane.read","params":{"pane":8,"lines":2}}"#);

    assert_eq!(
        server.app.requests(),
        [Request::ReadOutput(ReadOutput {
            pane: PaneId::new(8),
            lines: Some(2),
        })]
    );
}

fn done(pane: u64) -> AgentStateChanged {
    AgentStateChanged(PaneAgentState {
        pane: PaneId::new(pane),
        agent: Some("claude".into()),
        state: AgentActivity::Done,
        reason: None,
    })
}

#[test]
fn a_hook_report_reaches_the_app_and_is_answered() {
    let server = start_server();
    let mut client = RawClient::connect(&server.path);

    let response = client.call(
        r#"{"id":1,"method":"pane.report_hook","params":{"pane":3,"agent":"claude","event":"Stop","session_id":"s1"}}"#,
    );

    assert_eq!(response, json!({"id": 1, "result": null}));
    let [Request::ReportHook(hook)] = &server.app.requests()[..] else {
        panic!("{:?}", server.app.requests());
    };
    assert_eq!(
        (hook.pane.get(), hook.agent.as_str(), hook.event.as_str()),
        (3, "claude", "Stop")
    );
    assert_eq!(hook.session_id.as_deref(), Some("s1"));
}

#[test]
fn the_agent_state_of_a_pane_can_be_read() {
    let server = start_server();
    let mut client = RawClient::connect(&server.path);

    let response = client.call(r#"{"id":1,"method":"pane.agent_state","params":{"pane":3}}"#);
    assert_eq!(
        response,
        json!({"id": 1, "result": serde_json::to_value(agent_state_of(PaneId::new(3))).unwrap()})
    );

    let missing = client.call(&format!(
        r#"{{"id":2,"method":"pane.agent_state","params":{{"pane":{MISSING_PANE}}}}}"#
    ));
    assert_eq!(missing["error"]["code"], "unknown_pane");
}

#[test]
fn events_go_to_the_connection_that_subscribed_after_the_answer_and_in_order() {
    let server = start_server();
    let mut watcher = RawClient::connect(&server.path);
    let mut other = RawClient::connect(&server.path);

    watcher.send(r#"{"id":7,"method":"pane.subscribe_agent_state"}"#);
    let sinks = server.app.wait_for_sinks(1);
    assert!(sinks[0].send(&done(1)));
    assert!(sinks[0].send(&PaneClosed {
        pane: PaneId::new(1)
    }));
    assert!(sinks[0].send(&done(2)));

    assert_eq!(watcher.read().unwrap(), json!({"id": 7, "result": null}));
    assert_eq!(watcher.read().unwrap()["params"]["pane"], 1);
    assert_eq!(
        watcher.read().unwrap(),
        json!({"event": "pane.closed", "params": {"pane": 1}})
    );
    assert_eq!(watcher.read().unwrap()["params"]["pane"], 2);

    // The other connection gets its answers and nothing else.
    let response = other.call(r#"{"id":1,"method":"pane.read","params":{"pane":3}}"#);
    assert_eq!(response["id"], 1);
    other.stream.shutdown(Shutdown::Write).unwrap();
    assert_eq!(other.read(), None);
}

#[test]
fn a_watcher_still_gets_answers_between_events() {
    let server = start_server();
    let mut watcher = RawClient::connect(&server.path);
    watcher.call(r#"{"id":1,"method":"pane.subscribe_agent_state"}"#);
    let sinks = server.app.wait_for_sinks(1);

    assert!(sinks[0].send(&done(1)));
    assert_eq!(watcher.read().unwrap()["event"], "pane.agent_state_changed");
    let response = watcher.call(r#"{"id":2,"method":"pane.agent_state","params":{"pane":3}}"#);
    assert_eq!(response["id"], 2);
    assert!(sinks[0].send(&done(2)));
    assert_eq!(watcher.read().unwrap()["params"]["pane"], 2);
}

#[test]
fn a_sink_tells_when_its_connection_is_gone() {
    let server = start_server();
    let mut watcher = RawClient::connect(&server.path);
    watcher.call(r#"{"id":1,"method":"pane.subscribe_agent_state"}"#);
    let sinks = server.app.wait_for_sinks(1);
    assert!(!sinks[0].is_closed());

    drop(watcher);
    wait_until(|| server.server.connection_count() == 0);

    assert!(sinks[0].is_closed());
    assert!(!sinks[0].send(&done(1)));
}

#[test]
fn a_watcher_that_stops_reading_is_disconnected_and_nobody_else_is() {
    let server = start_server();
    let mut stuck = RawClient::connect(&server.path);
    stuck.call(r#"{"id":1,"method":"pane.subscribe_agent_state"}"#);
    let mut reading = RawClient::connect(&server.path);
    reading.call(r#"{"id":1,"method":"pane.subscribe_agent_state"}"#);
    let sinks = server.app.wait_for_sinks(2);

    // Events are queued without waiting for the client, but not without
    // end: a client this far behind is cut off.
    let mut sent = 0;
    while sinks[0].send(&done(sent)) {
        sent += 1;
        assert!(sent < 1_000_000, "the queue of a stuck watcher has no end");
        assert!(sinks[1].send(&done(sent)));
        assert_eq!(reading.read().unwrap()["params"]["pane"], sent);
    }
    assert!(sinks[0].is_closed());
    // Its threads end although it never reads and never leaves.
    wait_until(|| server.server.connection_count() == 1);

    assert!(sinks[1].send(&done(0)));
    assert_eq!(reading.read().unwrap()["params"]["pane"], 0);
    let response = reading.call(r#"{"id":2,"method":"pane.read","params":{"pane":3}}"#);
    assert_eq!(response["id"], 2);
    drop(stuck);
}

#[test]
fn sinks_tell_whether_they_send_to_the_same_connection() {
    let server = start_server();
    let mut twice = RawClient::connect(&server.path);
    twice.call(r#"{"id":1,"method":"pane.subscribe_agent_state"}"#);
    twice.call(r#"{"id":2,"method":"pane.subscribe_agent_state"}"#);
    let mut other = RawClient::connect(&server.path);
    other.call(r#"{"id":1,"method":"pane.subscribe_agent_state"}"#);
    let sinks = server.app.wait_for_sinks(3);

    assert!(sinks[0].same_connection(&sinks[1]));
    assert!(!sinks[0].same_connection(&sinks[2]));
}

#[test]
fn a_sink_does_not_keep_a_connection_that_has_ended() {
    let server = start_server();
    let mut watcher = RawClient::connect(&server.path);
    watcher.call(r#"{"id":1,"method":"pane.subscribe_agent_state"}"#);
    let sinks = server.app.wait_for_sinks(1);
    let peer = watcher.stream.try_clone().unwrap();

    // The client stops writing; the server answers by closing its end.
    // Only the sink is left of the connection, and it holds no socket: the
    // client reads the end of the stream without anything being sent.
    watcher.stream.shutdown(Shutdown::Write).unwrap();
    assert_eq!(watcher.read(), None);
    wait_until(|| server.server.connection_count() == 0);
    assert!(sinks[0].is_closed());
    drop(peer);
}

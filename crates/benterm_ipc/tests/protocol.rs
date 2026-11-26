//! The wire format: one JSON object per line.

use benterm_ipc::{
    AgentActivity, AgentHistory, AgentRecord, AgentStateChanged, ApiError, CreateWorkspace,
    ErrorCode, OpenBrowser, PaneAgentState, PaneClosed, PaneCreated, PaneId, ReadAgentHistory,
    ReadAgentState, ReadOutput, ReportHook, Request, RequestFrame, SendText, ServerMessage,
    SplitDirection, SplitPane, SubscribeAgentState, WaitingFor, WorkspaceId,
};
use serde_json::{Value, json};

fn value(line: &str) -> Value {
    serde_json::from_str(line).unwrap()
}

fn split(pane: u64, direction: SplitDirection) -> Request {
    Request::SplitPane(SplitPane {
        pane: PaneId::new(pane),
        direction,
    })
}

#[test]
fn a_request_is_one_line_with_id_method_and_params() {
    let line = RequestFrame::new(7, split(3, SplitDirection::Right)).to_line();

    assert_eq!(
        value(&line),
        json!({"id": 7, "method": "pane.split", "params": {"pane": 3, "direction": "right"}})
    );
    assert!(line.ends_with('\n'));
    assert_eq!(line.matches('\n').count(), 1);
}

#[test]
fn text_with_newlines_and_control_bytes_stays_on_one_line() {
    let text = "echo hi\r\n\u{3}日本語".to_string();
    let frame = RequestFrame::new(
        1,
        Request::SendText(SendText {
            pane: PaneId::new(2),
            text: text.clone(),
        }),
    );

    let line = frame.to_line();

    assert_eq!(line.matches('\n').count(), 1);
    assert_eq!(RequestFrame::parse(&line).unwrap(), frame);
}

fn stop_hook() -> ReportHook {
    ReportHook {
        pane: PaneId::new(4),
        agent: "claude".into(),
        event: "Stop".into(),
        tool_name: None,
        notification_type: None,
        session_id: None,
        agent_id: None,
        tool_input: None,
        tool_response: None,
    }
}

#[test]
fn every_request_survives_a_round_trip() {
    let requests = [
        split(1, SplitDirection::Right),
        split(u64::MAX, SplitDirection::Down),
        Request::CreateWorkspace(CreateWorkspace {
            name: Some("review".into()),
            cwd: Some("/tmp/work".into()),
            caller: Some(PaneId::new(7)),
        }),
        Request::CreateWorkspace(CreateWorkspace::default()),
        Request::SendText(SendText {
            pane: PaneId::new(4),
            text: "ls\r".into(),
        }),
        Request::ReadOutput(ReadOutput {
            pane: PaneId::new(4),
            lines: None,
        }),
        Request::ReadOutput(ReadOutput {
            pane: PaneId::new(4),
            lines: Some(200),
        }),
        Request::ReportHook(stop_hook()),
        Request::ReportHook(ReportHook {
            tool_name: Some("Bash".into()),
            notification_type: Some("idle_prompt".into()),
            session_id: Some("0b1c".into()),
            agent_id: Some("a1".into()),
            tool_input: None,
            tool_response: None,
            ..stop_hook()
        }),
        Request::ReadAgentState(ReadAgentState {
            pane: PaneId::new(4),
        }),
        Request::SubscribeAgentState(SubscribeAgentState {}),
        Request::ReadAgentHistory(ReadAgentHistory::default()),
        Request::ReadAgentHistory(ReadAgentHistory { since_ms: Some(5) }),
    ];

    for (id, request) in requests.into_iter().enumerate() {
        let frame = RequestFrame::new(id as u64, request);
        assert_eq!(RequestFrame::parse(&frame.to_line()).unwrap(), frame);
    }
}

#[test]
fn the_methods_have_fixed_names() {
    let method =
        |request: Request| value(&RequestFrame::new(0, request).to_line())["method"].clone();

    assert_eq!(method(split(1, SplitDirection::Down)), "pane.split");
    assert_eq!(
        method(Request::CreateWorkspace(CreateWorkspace::default())),
        "workspace.create"
    );
    assert_eq!(
        method(Request::SendText(SendText {
            pane: PaneId::new(1),
            text: String::new()
        })),
        "pane.send_text"
    );
    assert_eq!(
        method(Request::ReadOutput(ReadOutput {
            pane: PaneId::new(1),
            lines: None
        })),
        "pane.read"
    );
}

#[test]
fn optional_params_may_be_left_out() {
    let frame = RequestFrame::parse(r#"{"id":1,"method":"workspace.create"}"#).unwrap();
    assert_eq!(
        frame.request,
        Request::CreateWorkspace(CreateWorkspace::default())
    );

    let frame =
        RequestFrame::parse(r#"{"id":1,"method":"pane.read","params":{"pane":9}}"#).unwrap();
    assert_eq!(
        frame.request,
        Request::ReadOutput(ReadOutput {
            pane: PaneId::new(9),
            lines: None
        })
    );
}

#[test]
fn unknown_fields_are_ignored_so_fields_can_be_added_later() {
    let frame = RequestFrame::parse(
        r#"{"id":1,"method":"pane.read","params":{"pane":9,"future":true},"trace":"x"}"#,
    )
    .unwrap();

    assert_eq!(frame.id, 1);
}

#[test]
fn text_that_is_not_json_is_an_invalid_request_without_an_id() {
    let error = RequestFrame::parse("split 3\n").unwrap_err();

    assert_eq!(error.id, None);
    assert_eq!(error.error.code, ErrorCode::InvalidRequest);
}

#[test]
fn a_request_without_an_id_is_invalid() {
    for line in [
        r#"{"method":"pane.read","params":{"pane":1}}"#,
        r#"{"id":"one","method":"pane.read","params":{"pane":1}}"#,
        r#"{"id":-1,"method":"pane.read","params":{"pane":1}}"#,
        r#"[1]"#,
    ] {
        let error = RequestFrame::parse(line).unwrap_err();
        assert_eq!(error.id, None, "{line}");
        assert_eq!(error.error.code, ErrorCode::InvalidRequest, "{line}");
    }
}

#[test]
fn errors_after_the_id_was_read_carry_the_id() {
    let cases = [
        (r#"{"id":5}"#, ErrorCode::InvalidRequest),
        (r#"{"id":5,"method":7}"#, ErrorCode::InvalidRequest),
        (
            r#"{"id":5,"method":"pane.explode"}"#,
            ErrorCode::UnknownMethod,
        ),
        (
            r#"{"id":5,"method":"pane.split"}"#,
            ErrorCode::InvalidParams,
        ),
        (
            r#"{"id":5,"method":"pane.split","params":{"pane":1,"direction":"up"}}"#,
            ErrorCode::InvalidParams,
        ),
        (
            r#"{"id":5,"method":"pane.read","params":{"pane":"1"}}"#,
            ErrorCode::InvalidParams,
        ),
    ];

    for (line, code) in cases {
        let error = RequestFrame::parse(line).unwrap_err();
        assert_eq!(error.id, Some(5), "{line}");
        assert_eq!(error.error.code, code, "{line}");
        assert!(!error.error.message.is_empty(), "{line}");
    }
}

#[test]
fn a_response_carries_the_id_and_a_result() {
    let message = ServerMessage::result(
        7,
        &PaneCreated {
            pane: PaneId::new(4),
        },
    );
    let line = message.to_line();

    assert_eq!(value(&line), json!({"id": 7, "result": {"pane": 4}}));
    assert_eq!(line.matches('\n').count(), 1);
    assert_eq!(ServerMessage::parse(&line).unwrap(), message);
}

#[test]
fn a_response_carries_the_id_and_an_error() {
    let message = ServerMessage::error(Some(7), ApiError::new(ErrorCode::UnknownPane, "no pane 9"));
    let line = message.to_line();

    assert_eq!(
        value(&line),
        json!({"id": 7, "error": {"code": "unknown_pane", "message": "no pane 9"}})
    );
    assert_eq!(ServerMessage::parse(&line).unwrap(), message);
}

#[test]
fn an_error_for_an_unreadable_request_has_a_null_id() {
    let message = ServerMessage::error(None, ApiError::new(ErrorCode::InvalidRequest, "not JSON"));
    let line = message.to_line();

    assert_eq!(value(&line)["id"], Value::Null);
    assert_eq!(ServerMessage::parse(&line).unwrap(), message);
}

#[test]
fn a_line_with_an_event_and_no_id_is_a_notification() {
    let line = r#"{"event":"agent.state","params":{"pane":2,"state":"waiting"}}"#;

    assert_eq!(
        ServerMessage::parse(line).unwrap(),
        ServerMessage::Event {
            event: "agent.state".into(),
            params: json!({"pane": 2, "state": "waiting"}),
        }
    );
    assert_eq!(
        value(&ServerMessage::parse(line).unwrap().to_line()),
        value(line)
    );
}

#[test]
fn an_error_code_from_a_newer_server_is_still_an_error() {
    let line = r#"{"id":1,"error":{"code":"agent_busy","message":"later"}}"#;

    let ServerMessage::Response { result, .. } = ServerMessage::parse(line).unwrap() else {
        panic!("not a response");
    };
    let error = result.unwrap_err();
    assert_eq!(error.code, ErrorCode::Other);
    assert_eq!(error.message, "later");
}

#[test]
fn a_server_line_that_is_neither_a_response_nor_an_event_is_rejected() {
    for line in ["", "nope", r#"{"params":{}}"#, r#"{"id":1}"#] {
        assert!(ServerMessage::parse(line).is_err(), "{line}");
    }
}

#[test]
fn ids_are_plain_numbers_and_print_as_numbers() {
    assert_eq!(PaneId::new(12).get(), 12);
    assert_eq!(PaneId::new(12).to_string(), "12");
    assert_eq!("12".parse::<PaneId>().unwrap(), PaneId::new(12));
    assert!("pane-12".parse::<PaneId>().is_err());
    assert_eq!(WorkspaceId::new(3).to_string(), "3");
    assert_eq!(serde_json::to_value(PaneId::new(12)).unwrap(), json!(12));
}

#[test]
fn a_request_may_carry_how_long_the_app_has_to_start_it() {
    let frame = RequestFrame::new(7, split(3, SplitDirection::Right))
        .with_timeout(std::time::Duration::from_millis(1500));

    let line = frame.to_line();

    assert_eq!(value(&line)["timeout_ms"], 1500);
    assert_eq!(RequestFrame::parse(&line).unwrap(), frame);
    assert_eq!(
        RequestFrame::new(7, split(3, SplitDirection::Right)).timeout,
        None
    );
    assert!(
        value(&RequestFrame::new(7, split(3, SplitDirection::Right)).to_line())
            .get("timeout_ms")
            .is_none()
    );
}

#[test]
fn a_timeout_that_is_not_a_whole_number_of_milliseconds_is_an_invalid_request() {
    for timeout in [r#""soon""#, "-1", "1.5", "null"] {
        let line = format!(
            r#"{{"id":4,"method":"pane.read","params":{{"pane":1}},"timeout_ms":{timeout}}}"#
        );

        let error = RequestFrame::parse(&line).unwrap_err();

        assert_eq!(error.id, Some(4), "{timeout}");
        assert_eq!(error.error.code, ErrorCode::InvalidRequest, "{timeout}");
    }
}

#[test]
fn an_expired_request_has_its_own_error_code() {
    let message = ServerMessage::error(Some(7), ApiError::new(ErrorCode::Expired, "too late"));

    assert_eq!(value(&message.to_line())["error"]["code"], "expired");
    assert_eq!(ServerMessage::parse(&message.to_line()).unwrap(), message);
}

#[test]
fn the_agent_methods_have_fixed_names() {
    let method =
        |request: Request| value(&RequestFrame::new(0, request).to_line())["method"].clone();

    assert_eq!(method(Request::ReportHook(stop_hook())), "pane.report_hook");
    assert_eq!(
        method(Request::ReadAgentState(ReadAgentState {
            pane: PaneId::new(1)
        })),
        "pane.agent_state"
    );
    assert_eq!(
        method(Request::SubscribeAgentState(SubscribeAgentState {})),
        "pane.subscribe_agent_state"
    );
}

#[test]
fn a_hook_report_carries_only_the_fields_the_hook_gave() {
    let line = RequestFrame::new(1, Request::ReportHook(stop_hook())).to_line();
    assert_eq!(
        value(&line)["params"],
        json!({"pane": 4, "agent": "claude", "event": "Stop"})
    );

    let parsed = RequestFrame::parse(
        r#"{"id":1,"method":"pane.report_hook","params":{"pane":4,"agent":"codex","event":"PreToolUse","tool_name":"shell","session_id":"s"}}"#,
    )
    .unwrap();
    assert_eq!(
        parsed.request,
        Request::ReportHook(ReportHook {
            agent: "codex".into(),
            event: "PreToolUse".into(),
            tool_name: Some("shell".into()),
            session_id: Some("s".into()),
            ..stop_hook()
        })
    );
}

#[test]
fn a_hook_report_carries_what_a_tool_was_given_and_answered() {
    let hook = ReportHook {
        event: "PostToolUse".into(),
        tool_name: Some("TodoWrite".into()),
        tool_input: Some(json!({"todos": [{"content": "Read", "status": "pending"}]})),
        tool_response: Some(json!("done")),
        ..stop_hook()
    };

    let line = RequestFrame::new(1, Request::ReportHook(hook.clone())).to_line();

    assert_eq!(
        value(&line)["params"]["tool_input"]["todos"][0]["content"],
        "Read"
    );
    assert_eq!(
        RequestFrame::parse(&line).unwrap().request,
        Request::ReportHook(hook)
    );
}

#[test]
fn subscribing_needs_no_params() {
    let parsed = RequestFrame::parse(r#"{"id":1,"method":"pane.subscribe_agent_state"}"#).unwrap();
    assert_eq!(
        parsed.request,
        Request::SubscribeAgentState(SubscribeAgentState {})
    );
}

#[test]
fn an_agent_state_names_the_agent_the_state_and_what_is_waited_for() {
    let waiting = PaneAgentState {
        pane: PaneId::new(4),
        agent: Some("claude".into()),
        state: AgentActivity::Waiting,
        reason: Some(WaitingFor::Permission),
    };
    assert_eq!(
        serde_json::to_value(&waiting).unwrap(),
        json!({"pane": 4, "agent": "claude", "state": "waiting", "reason": "permission"})
    );

    let unknown = PaneAgentState {
        pane: PaneId::new(5),
        agent: None,
        state: AgentActivity::Unknown,
        reason: None,
    };
    assert_eq!(
        serde_json::to_value(&unknown).unwrap(),
        json!({"pane": 5, "agent": null, "state": "unknown", "reason": null})
    );
    for (state, name) in [
        (AgentActivity::Working, "working"),
        (AgentActivity::Done, "done"),
        (AgentActivity::Error, "error"),
    ] {
        assert_eq!(serde_json::to_value(state).unwrap(), json!(name));
    }
    assert_eq!(
        serde_json::to_value(WaitingFor::Question).unwrap(),
        json!("question")
    );
}

#[test]
fn the_events_are_notifications_with_fixed_names() {
    let state = PaneAgentState {
        pane: PaneId::new(4),
        agent: Some("codex".into()),
        state: AgentActivity::Done,
        reason: None,
    };
    let changed = ServerMessage::event(&AgentStateChanged(state.clone()));
    assert_eq!(
        value(&changed.to_line()),
        json!({
            "event": "pane.agent_state_changed",
            "params": {"pane": 4, "agent": "codex", "state": "done", "reason": null},
        })
    );
    assert_eq!(
        ServerMessage::parse(&changed.to_line()).unwrap().as_event(),
        Some(AgentStateChanged(state))
    );

    let closed = ServerMessage::event(&PaneClosed {
        pane: PaneId::new(4),
    });
    assert_eq!(
        value(&closed.to_line()),
        json!({"event": "pane.closed", "params": {"pane": 4}})
    );
    // An event is read as the kind it is and as no other.
    assert_eq!(closed.as_event::<AgentStateChanged>(), None);
    assert_eq!(
        closed.as_event(),
        Some(PaneClosed {
            pane: PaneId::new(4)
        })
    );
}

#[test]
fn the_history_is_read_with_agent_history_and_may_start_after_a_time() {
    let line = |request| value(&RequestFrame::new(0, Request::ReadAgentHistory(request)).to_line());
    let all = line(ReadAgentHistory::default());
    assert_eq!(all["method"], "agent.history");
    assert_eq!(all["params"], json!({}));
    let later = line(ReadAgentHistory { since_ms: Some(5) });
    assert_eq!(later["params"], json!({"since_ms": 5}));
    let parsed = RequestFrame::parse(r#"{"id":1,"method":"agent.history"}"#).unwrap();
    assert_eq!(
        parsed.request,
        Request::ReadAgentHistory(ReadAgentHistory::default())
    );
}

#[test]
fn a_record_of_the_history_names_the_time_the_pane_the_agent_and_the_state() {
    let history = AgentHistory {
        records: vec![
            AgentRecord {
                at_ms: 1_800_000_000_000,
                key: "0123456789abcdef0123456789abcdef".into(),
                pane: PaneId::new(4),
                agent: Some("claude".into()),
                state: AgentActivity::Waiting,
                reason: Some(WaitingFor::Permission),
            },
            AgentRecord {
                at_ms: 1_800_000_000_001,
                key: "0123456789abcdef0123456789abcdef".into(),
                pane: PaneId::new(4),
                agent: None,
                state: AgentActivity::Unknown,
                reason: None,
            },
        ],
    };
    let wire = json!({"records": [
        {
            "at_ms": 1_800_000_000_000u64, "key": "0123456789abcdef0123456789abcdef",
            "pane": 4, "agent": "claude", "state": "waiting", "reason": "permission",
        },
        {
            "at_ms": 1_800_000_000_001u64, "key": "0123456789abcdef0123456789abcdef",
            "pane": 4, "agent": null, "state": "unknown", "reason": null,
        },
    ]});
    assert_eq!(serde_json::to_value(&history).unwrap(), wire);
    assert_eq!(
        serde_json::from_value::<AgentHistory>(wire).unwrap(),
        history
    );
}

#[test]
fn opening_a_browser_names_the_address_and_where_the_pane_goes() {
    let request = Request::OpenBrowser(OpenBrowser {
        url: "http://127.0.0.1:8080/".into(),
        pane: Some(PaneId::new(3)),
        direction: SplitDirection::Down,
    });
    let line = RequestFrame::new(7, request.clone()).to_line();
    assert_eq!(
        value(&line),
        json!({"id": 7, "method": "browser.open", "params":
            {"url": "http://127.0.0.1:8080/", "pane": 3, "direction": "down"}})
    );
    assert_eq!(RequestFrame::parse(&line).unwrap().request, request);
}

/// Without a pane the app picks one, and the pane goes to the right.
#[test]
fn a_browser_is_opened_with_the_address_alone() {
    let line = r#"{"id":1,"method":"browser.open","params":{"url":"https://example.com/"}}"#;
    let request = Request::OpenBrowser(OpenBrowser {
        url: "https://example.com/".into(),
        pane: None,
        direction: SplitDirection::Right,
    });
    assert_eq!(RequestFrame::parse(line).unwrap().request, request);
    let written = RequestFrame::new(1, request).to_line();
    assert_eq!(
        value(&written)["params"],
        json!({"url": "https://example.com/", "direction": "right"})
    );
}

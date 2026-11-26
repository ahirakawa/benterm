//! The wire format of the requests that start and conduct agents.

use benterm_ipc::{
    AGENT_NAME_ENV, AgentActivity, AgentInfo, AgentShown, AgentTarget, ApiError,
    DEFAULT_MAX_PEER_ROUNDS, ErrorCode, KillAgent, ListAgents, PARENT_PANE_ID_ENV, PaneId,
    ReadAgent, ReportStatus, ReportToParent, Request, RequestFrame, SendKeys, SendToAgent,
    ServerMessage, ShowAgent, SpawnAgent, WaitForAgents,
};
use serde_json::{Value, json};

fn value(line: &str) -> Value {
    serde_json::from_str(line).unwrap()
}

fn parse(method: &str, params: Value) -> Request {
    let line = json!({"id": 1, "method": method, "params": params}).to_string();
    RequestFrame::parse(&line).unwrap().request
}

fn named(name: &str) -> AgentTarget {
    AgentTarget::Name(name.into())
}

#[test]
fn an_agent_is_named_by_its_name_or_by_the_number_of_its_pane() {
    assert_eq!(
        parse("agent.read", json!({"agent": "w1"})),
        Request::ReadAgent(ReadAgent {
            agent: named("w1"),
            lines: None,
        })
    );
    assert_eq!(
        parse("agent.read", json!({"agent": 7, "lines": 5})),
        Request::ReadAgent(ReadAgent {
            agent: AgentTarget::Pane(PaneId::new(7)),
            lines: Some(5),
        })
    );
}

#[test]
fn a_target_typed_as_digits_is_a_pane_and_anything_else_a_name() {
    assert_eq!("12".parse(), Ok(AgentTarget::Pane(PaneId::new(12))));
    assert_eq!("w12".parse(), Ok(named("w12")));
    assert_eq!(named("w1").to_string(), "w1");
    assert_eq!(AgentTarget::Pane(PaneId::new(4)).to_string(), "4");
}

#[test]
fn starting_an_agent_needs_only_its_name() {
    let request = parse("agent.spawn", json!({"name": "w1"}));

    assert_eq!(
        request,
        Request::SpawnAgent(SpawnAgent {
            name: "w1".into(),
            command: Vec::new(),
            cwd: None,
            env: Vec::new(),
            parent: None,
            outlive_parent: false,
            first_message_line: None,
            shell_after: false,
            allow_peers: false,
            max_peer_rounds: None,
            control: false,
        })
    );
}

#[test]
fn a_request_to_start_an_agent_says_everything_it_was_given() {
    let request = SpawnAgent {
        name: "w1".into(),
        command: vec!["claude".into(), "-p".into()],
        cwd: Some("/work".into()),
        env: vec![("PATH".into(), "/bin".into())],
        parent: Some(PaneId::new(3)),
        outlive_parent: true,
        first_message_line: Some("Read /c/worker.md before you start.".into()),
        shell_after: true,
        allow_peers: false,
        max_peer_rounds: None,
        control: false,
    };

    let line = RequestFrame::new(2, Request::SpawnAgent(request.clone())).to_line();

    assert_eq!(
        value(&line),
        json!({"id": 2, "method": "agent.spawn", "params": {
            "name": "w1", "command": ["claude", "-p"], "cwd": "/work",
            "env": [["PATH", "/bin"]], "parent": 3, "outlive_parent": true,
            "first_message_line": "Read /c/worker.md before you start.",
            "shell_after": true,
        }})
    );
    assert_eq!(
        RequestFrame::parse(&line).unwrap().request,
        Request::SpawnAgent(request)
    );
}

#[test]
fn a_wait_says_whom_it_waits_for_what_for_and_how_long() {
    assert_eq!(
        parse("agent.wait", json!({"agent": "w1"})),
        Request::WaitForAgents(WaitForAgents {
            agent: Some(named("w1")),
            children_of: None,
            until: Vec::new(),
            wait_ms: None,
            settle_ms: None,
        })
    );
    assert_eq!(
        parse(
            "agent.wait",
            json!({"children_of": 3, "until": ["working"], "wait_ms": 1000, "settle_ms": 0})
        ),
        Request::WaitForAgents(WaitForAgents {
            agent: None,
            children_of: Some(PaneId::new(3)),
            until: vec![AgentActivity::Working],
            wait_ms: Some(1000),
            settle_ms: Some(0),
        })
    );
}

#[test]
fn keys_go_to_an_agent_by_their_names_in_the_order_given() {
    let request = SendKeys {
        agent: named("w1"),
        keys: vec!["escape".into(), "ctrl-c".into()],
    };

    let line = RequestFrame::new(3, Request::SendKeys(request.clone())).to_line();

    assert_eq!(
        value(&line),
        json!({"id": 3, "method": "agent.keys", "params": {
            "agent": "w1", "keys": ["escape", "ctrl-c"],
        }})
    );
    assert_eq!(
        RequestFrame::parse(&line).unwrap().request,
        Request::SendKeys(request)
    );
    assert_eq!(
        parse("agent.keys", json!({"agent": 7, "keys": ["up", "enter"]})),
        Request::SendKeys(SendKeys {
            agent: AgentTarget::Pane(PaneId::new(7)),
            keys: vec!["up".into(), "enter".into()],
        })
    );
}

#[test]
fn keys_without_their_list_are_no_request() {
    let line = json!({"id": 4, "method": "agent.keys", "params": {"agent": "w1"}}).to_string();

    let error = RequestFrame::parse(&line).unwrap_err();

    assert_eq!(error.id, Some(4));
    assert_eq!(error.error.code, ErrorCode::InvalidParams);
}

#[test]
fn an_agent_is_killed_by_its_name_or_the_number_of_its_pane() {
    let request = KillAgent { agent: named("w1") };

    let line = RequestFrame::new(5, Request::KillAgent(request.clone())).to_line();

    assert_eq!(
        value(&line),
        json!({"id": 5, "method": "agent.kill", "params": {"agent": "w1"}})
    );
    assert_eq!(
        RequestFrame::parse(&line).unwrap().request,
        Request::KillAgent(request)
    );
    assert_eq!(
        parse("agent.kill", json!({"agent": 7})),
        Request::KillAgent(KillAgent {
            agent: AgentTarget::Pane(PaneId::new(7)),
        })
    );
}

#[test]
fn the_other_agent_requests_have_their_methods() {
    assert_eq!(
        parse("agent.list", json!({})),
        Request::ListAgents(ListAgents {
            parent: None,
            siblings_of: None,
        })
    );
    assert_eq!(
        parse("agent.show", json!({"agent": "w1"})),
        Request::ShowAgent(ShowAgent {
            agent: named("w1"),
            select: false,
        })
    );
    assert_eq!(
        parse("agent.send", json!({"agent": "w1", "text": "go on"})),
        Request::SendToAgent(SendToAgent {
            agent: named("w1"),
            text: "go on".into(),
            caller: None,
        })
    );
    assert_eq!(
        parse(
            "agent.send",
            json!({"agent": "w1", "text": "go on", "caller": 7})
        ),
        Request::SendToAgent(SendToAgent {
            agent: named("w1"),
            text: "go on".into(),
            caller: Some(PaneId::new(7)),
        })
    );
    assert_eq!(
        parse("agent.report", json!({"from": 7, "text": "done"})),
        Request::ReportToParent(ReportToParent {
            from: PaneId::new(7),
            text: "done".into(),
            status: None,
        })
    );
}

#[test]
fn the_errors_of_agents_have_codes_of_their_own() {
    let code = |code: ErrorCode| serde_json::to_value(code).unwrap();
    assert_eq!(code(ErrorCode::UnknownAgent), json!("unknown_agent"));
    assert_eq!(code(ErrorCode::NameTaken), json!("name_taken"));
    assert_eq!(code(ErrorCode::WaitTimedOut), json!("wait_timed_out"));
    assert_eq!(code(ErrorCode::AgentWaiting), json!("agent_waiting"));
    assert_eq!(code(ErrorCode::InputQueueFull), json!("input_queue_full"));
}

/// A send refused because the agent waits for the user goes over the
/// wire and back with its code and its reason.
#[test]
fn a_send_refused_for_an_agent_that_waits_for_the_user_is_an_error_of_its_own() {
    let refused = ApiError::new(ErrorCode::AgentWaiting, "w1 is waiting for a permission");
    let line = ServerMessage::error(Some(4), refused.clone()).to_line();

    assert_eq!(
        value(&line),
        json!({"id": 4, "error": {"code": "agent_waiting", "message": "w1 is waiting for a permission"}})
    );
    assert_eq!(
        ServerMessage::parse(&line).unwrap(),
        ServerMessage::error(Some(4), refused)
    );
}

#[test]
fn a_report_status_is_one_of_three_words() {
    let word = |status: ReportStatus| serde_json::to_value(status).unwrap();
    assert_eq!(word(ReportStatus::Pass), json!("pass"));
    assert_eq!(word(ReportStatus::Issues), json!("issues"));
    assert_eq!(word(ReportStatus::Blocked), json!("blocked"));
    for status in [
        ReportStatus::Pass,
        ReportStatus::Issues,
        ReportStatus::Blocked,
    ] {
        assert_eq!(status.as_str().parse::<ReportStatus>(), Ok(status));
    }
    assert!("done".parse::<ReportStatus>().is_err());
    assert!("PASS".parse::<ReportStatus>().is_err());
}

#[test]
fn a_report_with_a_status_goes_over_the_wire_and_back() {
    let request = ReportToParent {
        from: PaneId::new(7),
        text: "cannot build".into(),
        status: Some(ReportStatus::Blocked),
    };

    let line = RequestFrame::new(6, Request::ReportToParent(request.clone())).to_line();

    assert_eq!(
        value(&line)["params"],
        json!({"from": 7, "text": "cannot build", "status": "blocked"})
    );
    assert_eq!(
        RequestFrame::parse(&line).unwrap().request,
        Request::ReportToParent(request)
    );
}

/// A report without a status is written as it was before there were
/// any, so an app that knows no status still takes it.
#[test]
fn a_report_without_a_status_is_written_in_the_old_form() {
    let request = ReportToParent {
        from: PaneId::new(7),
        text: "done".into(),
        status: None,
    };

    let line = RequestFrame::new(6, Request::ReportToParent(request.clone())).to_line();

    assert_eq!(value(&line)["params"], json!({"from": 7, "text": "done"}));
    assert_eq!(
        RequestFrame::parse(&line).unwrap().request,
        Request::ReportToParent(request)
    );
}

#[test]
fn a_report_with_a_status_that_is_no_status_is_invalid() {
    let line = json!({"id": 1, "method": "agent.report",
        "params": {"from": 7, "text": "done", "status": "ok"}})
    .to_string();

    let error = RequestFrame::parse(&line).unwrap_err();

    assert_eq!(error.error.code, ErrorCode::InvalidParams);
}

#[test]
fn an_agent_in_the_list_and_a_shown_agent_carry_the_status_of_the_last_report() {
    let info = AgentInfo {
        name: "w1".into(),
        pane: PaneId::new(20),
        parent: Some(PaneId::new(3)),
        background: true,
        outlive_parent: false,
        state: AgentActivity::Done,
        stalled: false,
        changed_at_ms: None,
        report_status: Some(ReportStatus::Issues),
        msg: None,
        allow_peers: false,
    };
    let json = serde_json::to_value(&info).unwrap();
    assert_eq!(json["report_status"], json!("issues"));
    assert_eq!(serde_json::from_value::<AgentInfo>(json).unwrap(), info);

    let shown = AgentShown {
        pane: PaneId::new(20),
        stalled: false,
        changed_at_ms: None,
        report_status: Some(ReportStatus::Pass),
    };
    let json = serde_json::to_value(&shown).unwrap();
    assert_eq!(json, json!({"pane": 20, "report_status": "pass"}));
    assert_eq!(serde_json::from_value::<AgentShown>(json).unwrap(), shown);
}

/// The answers of an app from before the statuses are read as agents
/// without one.
#[test]
fn an_answer_without_report_statuses_reads_as_none() {
    let info: AgentInfo = serde_json::from_value(json!({
        "name": "w1", "pane": 20, "parent": null, "background": false,
        "outlive_parent": false, "state": "working"
    }))
    .unwrap();
    let shown: AgentShown = serde_json::from_value(json!({"pane": 20})).unwrap();

    assert_eq!(info.report_status, None);
    assert_eq!(shown.report_status, None);
}

#[test]
fn an_agent_says_whether_it_has_stalled_and_when_it_last_changed() {
    let line = json!({
        "name": "w1", "pane": 7, "parent": null, "background": true,
        "outlive_parent": false, "state": "working",
        "stalled": true, "changed_at_ms": 1_700_000_000_000_u64,
    });
    let info: benterm_ipc::AgentInfo = serde_json::from_value(line).unwrap();
    assert!(info.stalled);
    assert_eq!(info.changed_at_ms, Some(1_700_000_000_000));

    // A line from before the mark has neither.
    let line = json!({
        "name": "w1", "pane": 7, "parent": null, "background": true,
        "outlive_parent": false, "state": "done",
    });
    let info: benterm_ipc::AgentInfo = serde_json::from_value(line).unwrap();
    assert!(!info.stalled);
    assert_eq!(info.changed_at_ms, None);

    let shown: benterm_ipc::AgentShown = serde_json::from_value(json!({"pane": 7})).unwrap();
    assert_eq!((shown.stalled, shown.changed_at_ms), (false, None));
    let shown = serde_json::to_value(benterm_ipc::AgentShown {
        stalled: true,
        changed_at_ms: Some(5),
        ..shown
    })
    .unwrap();
    assert_eq!(
        shown,
        json!({"pane": 7, "stalled": true, "changed_at_ms": 5})
    );
}

#[test]
fn an_agent_carries_msg_only_when_set() {
    let waiting = AgentInfo {
        name: "w1".into(),
        pane: PaneId::new(20),
        parent: None,
        background: true,
        outlive_parent: false,
        state: AgentActivity::Waiting,
        report_status: None,
        stalled: false,
        changed_at_ms: None,
        msg: Some("Allow Bash: cargo test".into()),
        allow_peers: false,
    };
    let json = serde_json::to_value(&waiting).unwrap();
    assert_eq!(json["msg"], json!("Allow Bash: cargo test"));
    assert_eq!(serde_json::from_value::<AgentInfo>(json).unwrap(), waiting);

    let working = AgentInfo {
        state: AgentActivity::Working,
        msg: None,
        ..waiting
    };
    let json = serde_json::to_value(&working).unwrap();
    assert!(json.get("msg").is_none());

    // Older JSON without `msg` deserializes as `None`.
    let legacy: AgentInfo = serde_json::from_value(json!({
        "name": "w1", "pane": 20, "parent": null, "background": true,
        "outlive_parent": false, "state": "waiting"
    }))
    .unwrap();
    assert_eq!(legacy.msg, None);
}

#[test]
fn peer_opt_in_fields_and_error_codes_round_trip_with_backward_compat() {
    assert_eq!(DEFAULT_MAX_PEER_ROUNDS, 10);
    assert_eq!(AGENT_NAME_ENV, "BENTERM_AGENT_NAME");
    assert_eq!(PARENT_PANE_ID_ENV, "BENTERM_PARENT_PANE_ID");

    let code = |code: ErrorCode| serde_json::to_value(code).unwrap();
    assert_eq!(code(ErrorCode::PeerNotAllowed), json!("peer_not_allowed"));
    assert_eq!(
        code(ErrorCode::RoundLimitReached),
        json!("round_limit_reached")
    );

    // Older SpawnAgent JSON without peer fields defaults cleanly.
    let legacy_spawn = parse("agent.spawn", json!({"name": "w1"}));
    let Request::SpawnAgent(legacy_spawn) = legacy_spawn else {
        panic!("expected SpawnAgent");
    };
    assert!(!legacy_spawn.allow_peers);
    assert_eq!(legacy_spawn.max_peer_rounds, None);

    // SpawnAgent with allow_peers and max_peer_rounds round-trips.
    let peer_spawn = SpawnAgent {
        name: "w2".into(),
        allow_peers: true,
        max_peer_rounds: Some(5),
        ..SpawnAgent::default()
    };
    let line = RequestFrame::new(9, Request::SpawnAgent(peer_spawn.clone())).to_line();
    assert_eq!(
        value(&line)["params"],
        json!({"name": "w2", "allow_peers": true, "max_peer_rounds": 5})
    );
    assert_eq!(
        RequestFrame::parse(&line).unwrap().request,
        Request::SpawnAgent(peer_spawn)
    );

    // ListAgents with siblings_of round-trips and defaults to None.
    assert_eq!(
        parse("agent.list", json!({"siblings_of": 7})),
        Request::ListAgents(ListAgents {
            parent: None,
            siblings_of: Some(PaneId::new(7)),
        })
    );

    // AgentInfo allow_peers defaults to false and is omitted when false.
    let legacy_info: AgentInfo = serde_json::from_value(json!({
        "name": "w1", "pane": 20, "parent": 3, "background": true,
        "outlive_parent": false, "state": "done"
    }))
    .unwrap();
    assert!(!legacy_info.allow_peers);
    assert!(
        serde_json::to_value(&legacy_info)
            .unwrap()
            .get("allow_peers")
            .is_none()
    );

    let peer_info = AgentInfo {
        allow_peers: true,
        ..legacy_info
    };
    let peer_json = serde_json::to_value(&peer_info).unwrap();
    assert_eq!(peer_json["allow_peers"], json!(true));
    assert_eq!(
        serde_json::from_value::<AgentInfo>(peer_json).unwrap(),
        peer_info
    );
}

//! Authorization and opt-in restricted mode tests for the IPC server.

mod common;

use std::sync::Arc;

use benterm_ipc::{
    Client, ClientError, ErrorCode, ListPanes, PaneId, PermissionTable, ReadOutput, ReportChecked,
    ReportHook, ReportToParent, SendText, Server, ServerLimits, SpawnAgent,
};
use common::{FakeApp, RawClient, private_dir};
use serde_json::json;

struct RestrictedServer {
    app: Arc<FakeApp>,
    server: Server,
    path: std::path::PathBuf,
    _dir: tempfile::TempDir,
}

fn start_server(restricted: bool) -> RestrictedServer {
    let dir = private_dir();
    let path = dir.path().join("benterm.sock");
    let app = Arc::new(FakeApp::default());
    let permissions = PermissionTable::new(restricted);
    let server =
        Server::bind_with_permissions(&path, app.clone(), ServerLimits::default(), permissions)
            .unwrap();
    RestrictedServer {
        app,
        server,
        path,
        _dir: dir,
    }
}

fn report_hook(pane: u64) -> ReportHook {
    ReportHook {
        pane: PaneId::new(pane),
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

fn report_checked(pane: u64) -> ReportChecked {
    ReportChecked {
        pane: PaneId::new(pane),
        agent: "claude".into(),
        session_id: None,
        report: benterm_report::Report {
            title: "done".into(),
            goal: String::new(),
            conclusion: "pass".into(),
            summary: "done".into(),
            decisions: Vec::new(),
            verified: Vec::new(),
            unverified: Vec::new(),
            deliverables: Vec::new(),
        },
        problems: Vec::new(),
    }
}

fn report_to_parent(from: u64) -> ReportToParent {
    ReportToParent {
        from: PaneId::new(from),
        text: "done".into(),
        status: None,
    }
}

#[test]
fn unrestricted_mode_preserves_legacy_behavior_without_tokens() {
    let server = start_server(false);
    let mut client = Client::connect(&server.path).unwrap().with_token(None);

    let output = client
        .call(&ReadOutput {
            pane: PaneId::new(1),
            lines: None,
        })
        .unwrap();
    assert_eq!(output.text, "screen of pane 1");
}

#[test]
fn restricted_mode_rejects_missing_and_invalid_tokens_without_leaking_secret() {
    let server = start_server(true);
    let mut client = Client::connect(&server.path).unwrap().with_token(None);

    let err = client.call(&ListPanes {}).unwrap_err();
    assert!(matches!(
        err,
        ClientError::Api(ref api) if api.code == ErrorCode::Unauthorized
    ));

    let bogus = "bogus-secret-token-987654321";
    let mut client = Client::connect(&server.path)
        .unwrap()
        .with_token(Some(bogus.into()));
    let err = client.call(&report_hook(1)).unwrap_err();
    match err {
        ClientError::Api(api) => {
            assert_eq!(api.code, ErrorCode::Unauthorized);
            assert!(!api.message.contains(bogus));
        }
        other => panic!("expected Api error, got {other:?}"),
    }

    assert!(server.app.requests().is_empty());
}

#[test]
fn restricted_mode_pane_token_allows_own_reports_and_rejects_other_panes_and_control_methods() {
    let server = start_server(true);
    let token = server.server.permissions().issue_pane_token(PaneId::new(5));
    let mut client = Client::connect(&server.path)
        .unwrap()
        .with_token(Some(token.clone()));

    // Allowed own-pane methods
    client.call(&report_hook(5)).unwrap();
    client.call(&report_checked(5)).unwrap();
    client.call(&report_to_parent(5)).unwrap();
    assert_eq!(server.app.requests().len(), 3);

    // Mismatched pane IDs are rejected with Forbidden
    let err = client.call(&report_hook(6)).unwrap_err();
    assert!(matches!(
        err,
        ClientError::Api(ref api) if api.code == ErrorCode::Forbidden && !api.message.contains(&token)
    ));
    let err = client.call(&report_checked(6)).unwrap_err();
    assert!(matches!(
        err,
        ClientError::Api(ref api) if api.code == ErrorCode::Forbidden && !api.message.contains(&token)
    ));
    let err = client.call(&report_to_parent(6)).unwrap_err();
    assert!(matches!(
        err,
        ClientError::Api(ref api) if api.code == ErrorCode::Forbidden && !api.message.contains(&token)
    ));

    // Control methods are rejected with Forbidden
    let err = client
        .call(&ReadOutput {
            pane: PaneId::new(5),
            lines: None,
        })
        .unwrap_err();
    assert!(matches!(
        err,
        ClientError::Api(ref api) if api.code == ErrorCode::Forbidden && !api.message.contains(&token)
    ));
    let err = client
        .call(&SendText {
            pane: PaneId::new(5),
            text: "ls\r".into(),
        })
        .unwrap_err();
    assert!(matches!(
        err,
        ClientError::Api(ref api) if api.code == ErrorCode::Forbidden && !api.message.contains(&token)
    ));
    let err = client
        .call(&SpawnAgent {
            name: "worker".into(),
            ..SpawnAgent::default()
        })
        .unwrap_err();
    assert!(matches!(
        err,
        ClientError::Api(ref api) if api.code == ErrorCode::Forbidden && !api.message.contains(&token)
    ));

    // Unknown method is rejected and never reaches the app
    let mut raw = RawClient::connect(&server.path);
    let reply = raw.call(&json!({"id": 99, "method": "pane.explode", "token": token}).to_string());
    assert_eq!(reply["id"], 99);
    assert_eq!(reply["error"]["code"], "unknown_method");
    assert!(!reply["error"]["message"].as_str().unwrap().contains(&token));

    // None of the rejected requests reached FakeApp
    assert_eq!(server.app.requests().len(), 3);
}

#[test]
fn restricted_mode_control_token_allows_all_operations() {
    let server = start_server(true);
    let token = server.server.permissions().issue_control_token(None);
    let mut client = Client::connect(&server.path)
        .unwrap()
        .with_token(Some(token));

    let output = client
        .call(&ReadOutput {
            pane: PaneId::new(1),
            lines: None,
        })
        .unwrap();
    assert_eq!(output.text, "screen of pane 1");

    client
        .call(&SendText {
            pane: PaneId::new(2),
            text: "hi".into(),
        })
        .unwrap();
    client
        .call(&SpawnAgent {
            name: "worker".into(),
            ..SpawnAgent::default()
        })
        .unwrap();
    client.call(&report_hook(9)).unwrap();
}

#[test]
fn restricted_mode_rechecks_revoked_pane_token_on_existing_connection() {
    let server = start_server(true);
    let token = server.server.permissions().issue_pane_token(PaneId::new(3));
    let mut client = Client::connect(&server.path)
        .unwrap()
        .with_token(Some(token.clone()));

    // First request succeeds on the connection.
    client.call(&report_hook(3)).unwrap();
    assert_eq!(server.app.requests().len(), 1);

    // Revoke the pane's token while the connection stays open.
    server.server.permissions().revoke_pane(PaneId::new(3));

    // Subsequent request on the exact same connection is rejected.
    let err = client.call(&report_hook(3)).unwrap_err();
    match err {
        ClientError::Api(api) => {
            assert_eq!(api.code, ErrorCode::Unauthorized);
            assert!(!api.message.contains(&token));
        }
        other => panic!("expected Unauthorized Api error, got {other:?}"),
    }
    assert_eq!(server.app.requests().len(), 1);
}

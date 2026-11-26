//! CLI mailbox fallback tests: `benterm agent send` and `benterm agent report`
//! fall back to `$BENTERM_MAILBOX_DIR/outbox/<seq>-<pid>.json` when the socket
//! cannot be reached (e.g. inside a sandbox).

use std::path::Path;

use benterm_ipc::MAILBOX_DIR_ENV;
use benterm_ipc::cli::{self, CliEnv};
use serde_json::Value;

struct Run {
    code: u8,
    stdout: String,
    stderr: String,
}

fn run_with(env: &CliEnv, stdin: &str, args: &[&str]) -> Run {
    let config = tempfile::tempdir().unwrap();
    let args: Vec<String> = args.iter().map(|arg| arg.to_string()).collect();
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let code = cli::run_with_roles(
        &args,
        env,
        Some(&config.path().join("roles")),
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

fn outbox_entries(mailbox_dir: &Path) -> Vec<(String, Value)> {
    let outbox = mailbox_dir.join("outbox");
    let mut entries: Vec<(String, Value)> = std::fs::read_dir(&outbox)
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            let name = entry.file_name().into_string().unwrap();
            let raw = std::fs::read_to_string(entry.path()).unwrap();
            let json: Value = serde_json::from_str(&raw).unwrap();
            (name, json)
        })
        .collect();
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    entries
}

#[test]
fn mailbox_dir_env_constant_is_exported() {
    assert_eq!(MAILBOX_DIR_ENV, "BENTERM_MAILBOX_DIR");
}

#[test]
fn agent_send_falls_back_to_mailbox_when_socket_cannot_be_reached() {
    let tmp = tempfile::tempdir().unwrap();
    let mailbox_dir = tmp.path().join("mailbox");
    let missing_socket = tmp.path().join("no-such.sock");
    let env = CliEnv {
        socket: Some(missing_socket.into_os_string()),
        pane: Some("3".into()),
        mailbox_dir: Some(mailbox_dir.clone()),
        ..CliEnv::default()
    };

    let res = run_with(
        &env,
        "",
        &["agent", "send", "bob", "hello", "from", "sandbox"],
    );
    assert_eq!(
        (res.code, res.stdout.as_str(), res.stderr.as_str()),
        (0, "", "")
    );

    let entries = outbox_entries(&mailbox_dir);
    assert_eq!(entries.len(), 1);
    let (name, json) = &entries[0];
    assert!(
        name.ends_with(&format!("-{}.json", std::process::id())),
        "unexpected outbox filename: {name}"
    );
    assert_eq!(json["to"], "bob");
    assert_eq!(json["text"], "hello from sandbox");
    assert!(json.get("status").is_none());
}

#[test]
fn agent_report_falls_back_to_mailbox_with_status_and_stdin() {
    let tmp = tempfile::tempdir().unwrap();
    let mailbox_dir = tmp.path().join("mailbox");
    let missing_socket = tmp.path().join("no-such.sock");
    let env = CliEnv {
        socket: Some(missing_socket.into_os_string()),
        pane: Some("4".into()),
        mailbox_dir: Some(mailbox_dir.clone()),
        ..CliEnv::default()
    };

    let first = run_with(
        &env,
        "",
        &["agent", "report", "--status", "pass", "all", "green"],
    );
    assert_eq!(
        (first.code, first.stdout.as_str(), first.stderr.as_str()),
        (0, "", "")
    );

    let second = run_with(
        &env,
        "need help\nline 2",
        &["agent", "report", "--status", "blocked"],
    );
    assert_eq!(
        (second.code, second.stdout.as_str(), second.stderr.as_str()),
        (0, "", "")
    );

    let entries = outbox_entries(&mailbox_dir);
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].1["to"], "parent");
    assert_eq!(entries[0].1["text"], "all green");
    assert_eq!(entries[0].1["status"], "pass");
    assert_eq!(entries[1].1["to"], "parent");
    assert_eq!(entries[1].1["text"], "need help\nline 2");
    assert_eq!(entries[1].1["status"], "blocked");
    // Sequence numbers strictly increase so lexical order matches send order.
    assert!(entries[0].0 < entries[1].0);
}

#[test]
fn agent_send_without_mailbox_dir_fails_when_socket_unavailable() {
    let tmp = tempfile::tempdir().unwrap();
    let missing_socket = tmp.path().join("no-such.sock");
    let env = CliEnv {
        socket: Some(missing_socket.into_os_string()),
        pane: Some("3".into()),
        mailbox_dir: None,
        ..CliEnv::default()
    };

    let res = run_with(&env, "", &["agent", "send", "bob", "hi"]);
    assert_eq!(res.code, 1);
    assert!(res.stderr.contains("cannot connect"));
}

#[test]
fn agent_send_fallback_still_rejects_usage_errors() {
    let tmp = tempfile::tempdir().unwrap();
    let mailbox_dir = tmp.path().join("mailbox");
    let missing_socket = tmp.path().join("no-such.sock");
    let env = CliEnv {
        socket: Some(missing_socket.into_os_string()),
        pane: Some("3".into()),
        mailbox_dir: Some(mailbox_dir.clone()),
        ..CliEnv::default()
    };

    let res = run_with(&env, "", &["agent", "send"]);
    assert_eq!(res.code, 2);
    assert!(!mailbox_dir.join("outbox").exists());
}

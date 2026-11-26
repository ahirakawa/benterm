//! The subcommands of the `benterm` binary, as a shell inside a pane
//! would run them, against a server with a fake app.

#[path = "../../benterm_ipc/tests/common/mod.rs"]
mod common;

use std::process::{Command, Output, Stdio};

use benterm_ipc::{
    CreateWorkspace, PANE_ID_ENV, PaneId, Request, SOCKET_ENV, SplitDirection, SplitPane,
};
use common::{TestServer, start_server};

fn benterm(server: &TestServer, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_benterm"))
        .args(args)
        .env(SOCKET_ENV, &server.path)
        .env(PANE_ID_ENV, "3")
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

#[test]
fn it_finds_the_socket_and_the_pane_in_the_environment() {
    let server = start_server();

    let output = benterm(&server, &["split", "--down"]);

    assert!(output.status.success(), "{output:?}");
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "4\n");
    assert_eq!(
        server.app.requests(),
        [Request::SplitPane(SplitPane {
            pane: PaneId::new(3),
            direction: SplitDirection::Down,
        })]
    );
}

#[test]
fn it_sends_text_and_reads_it_back_from_another_pane() {
    let server = start_server();

    let send = benterm(&server, &["send", "--pane", "7", "--enter", "echo hi"]);
    let read = benterm(&server, &["read", "--pane", "7", "--lines", "2"]);

    assert!(send.status.success(), "{send:?}");
    assert_eq!(
        String::from_utf8(read.stdout).unwrap(),
        "last 2 lines of pane 7\n"
    );
    assert_eq!(server.app.requests().len(), 2);
}

#[test]
fn it_exits_with_2_on_a_usage_error_and_1_on_a_failure() {
    let server = start_server();

    let usage = benterm(&server, &["explode"]);
    let failure = benterm(&server, &["read", "--pane", "404"]);

    assert_eq!(usage.status.code(), Some(2));
    assert_eq!(failure.status.code(), Some(1));
    assert_eq!(
        String::from_utf8(failure.stderr).unwrap(),
        "benterm: no pane 404\n"
    );
}

#[test]
fn help_prints_the_usage_without_opening_the_app() {
    let server = start_server();

    let help = benterm(&server, &["--help"]);

    assert!(help.status.success(), "{help:?}");
    let usage = String::from_utf8(help.stdout).unwrap();
    assert!(usage.starts_with("usage: benterm"), "{usage}");
    assert!(usage.contains("Without arguments"), "{usage}");
    assert!(server.app.requests().is_empty());
}

#[test]
fn an_explicit_socket_comes_before_the_command() {
    let server = start_server();
    let path = server.path.to_str().unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_benterm"))
        .args(["--socket", path, "split", "--pane", "5"])
        .env_remove(SOCKET_ENV)
        .env_remove(PANE_ID_ENV)
        .stdin(Stdio::null())
        .output()
        .unwrap();

    assert!(output.status.success(), "{output:?}");
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "6\n");
}

#[test]
fn a_relative_working_directory_is_taken_from_where_the_command_runs() {
    let server = start_server();
    let dir = tempfile::tempdir().unwrap();
    let here = dir.path().canonicalize().unwrap();
    std::fs::create_dir(here.join("work")).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_benterm"))
        .args(["new-workspace", "--cwd", "work"])
        .current_dir(&here)
        .env(SOCKET_ENV, &server.path)
        .env_remove(PANE_ID_ENV)
        .stdin(Stdio::null())
        .output()
        .unwrap();

    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        server.app.requests(),
        [Request::CreateWorkspace(CreateWorkspace {
            name: None,
            cwd: Some(here.join("work").to_str().unwrap().into()),
            caller: None,
        })]
    );
}

#[test]
fn in_a_pane_of_an_app_without_its_socket_it_says_the_app_could_not_open_it() {
    // An app that could not listen on its socket gives its panes an
    // empty BENTERM_SOCKET.
    let output = Command::new(env!("CARGO_BIN_EXE_benterm"))
        .args(["split"])
        .env(SOCKET_ENV, "")
        .env(PANE_ID_ENV, "3")
        .stdin(Stdio::null())
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("started without its socket API"),
        "{stderr}"
    );
    assert!(stderr.contains("could not open its socket"), "{stderr}");
}

/// Runs `benterm hook` with the hook's JSON on its standard input, in
/// the given environment.
fn hook(env: &[(&str, &std::ffi::OsStr)], args: &[&str], input: &str) -> Output {
    use std::io::Write;

    let mut command = Command::new(env!("CARGO_BIN_EXE_benterm"));
    command
        .arg("hook")
        .args(args)
        .env_remove(SOCKET_ENV)
        .env_remove(PANE_ID_ENV)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (name, value) in env {
        command.env(name, value);
    }
    let mut child = command.spawn().unwrap();
    // Outside a pane the hook does not read its input, so the pipe may
    // already be closed.
    let _ = child.stdin.take().unwrap().write_all(input.as_bytes());
    child.wait_with_output().unwrap()
}

const HOOK_INPUT: &str =
    r#"{"session_id":"0b1c","hook_event_name":"PreToolUse","tool_name":"Bash"}"#;

#[test]
fn hook_reports_to_the_app_of_its_pane() {
    let server = start_server();
    let env = [
        (SOCKET_ENV, server.path.as_os_str()),
        (PANE_ID_ENV, "3".as_ref()),
    ];

    let output = hook(&env, &["PreToolUse", "--agent", "claude"], HOOK_INPUT);

    assert!(output.status.success(), "{output:?}");
    assert!(
        output.stdout.is_empty() && output.stderr.is_empty(),
        "{output:?}"
    );
    let [Request::ReportHook(report)] = &server.app.requests()[..] else {
        panic!("{:?}", server.app.requests());
    };
    assert_eq!(report.pane, PaneId::new(3));
    assert_eq!(
        (report.agent.as_str(), report.event.as_str()),
        ("claude", "PreToolUse")
    );
    assert_eq!(report.tool_name.as_deref(), Some("Bash"));
    assert_eq!(report.session_id.as_deref(), Some("0b1c"));
}

#[test]
fn hook_outside_the_app_ends_quietly_and_at_once() {
    let started = std::time::Instant::now();
    for args in [&["Stop", "--agent", "claude"][..], &[], &["--bogus"]] {
        let output = hook(&[], args, HOOK_INPUT);

        assert_eq!(output.status.code(), Some(0), "{output:?}");
        assert!(
            output.stdout.is_empty() && output.stderr.is_empty(),
            "{output:?}"
        );
    }
    assert!(started.elapsed() < std::time::Duration::from_secs(2));
}

#[test]
fn hook_ends_quietly_when_the_app_of_its_pane_is_gone() {
    let dir = common::private_dir();
    let socket = dir.path().join("gone.sock");
    let env = [
        (SOCKET_ENV, socket.as_os_str()),
        (PANE_ID_ENV, "3".as_ref()),
    ];

    let output = hook(&env, &["Stop", "--agent", "codex"], HOOK_INPUT);

    // Never a status an agent takes for a reason to stop, and nothing on
    // the output the agent reads.
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(output.stdout.is_empty(), "{output:?}");
}

#[test]
fn hook_ends_quietly_on_an_argument_that_is_not_utf8() {
    use std::os::unix::ffi::OsStrExt;

    let output = Command::new(env!("CARGO_BIN_EXE_benterm"))
        .args(["hook", "--agent"])
        .arg(std::ffi::OsStr::from_bytes(b"cl\xffude"))
        .stdin(Stdio::null())
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(output.stdout.is_empty(), "{output:?}");
}

#[test]
fn setup_works_without_the_app_and_links_this_binary_and_the_skill_of_its_checkout() {
    let home = tempfile::tempdir().unwrap();
    let home = home.path().canonicalize().unwrap();
    let bin = home.join(".local/bin");
    let setup = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_benterm"))
            .arg("setup")
            .args(args)
            .env("HOME", &home)
            .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
            // No app: setup does not use the socket.
            .env(SOCKET_ENV, home.join("no-app.sock"))
            .env_remove(PANE_ID_ENV)
            .stdin(Stdio::null())
            .output()
            .unwrap()
    };

    let install = setup(&["install", "--yes"]);
    let status = setup(&["status"]);

    assert_eq!(install.status.code(), Some(0), "{install:?}");
    let exe = std::path::Path::new(env!("CARGO_BIN_EXE_benterm"))
        .canonicalize()
        .unwrap();
    assert_eq!(std::fs::read_link(bin.join("benterm")).unwrap(), exe);
    let skill = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../skills/benterm")
        .canonicalize()
        .unwrap();
    assert_eq!(
        std::fs::read_link(home.join(".claude/skills/benterm")).unwrap(),
        skill
    );
    let status = String::from_utf8(status.stdout).unwrap();
    assert!(status.contains("everything is set up"), "{status}");
}

#[test]
fn setup_is_in_the_help() {
    let output = Command::new(env!("CARGO_BIN_EXE_benterm"))
        .arg("--help")
        .stdin(Stdio::null())
        .output()
        .unwrap();

    let help = String::from_utf8(output.stdout).unwrap();
    assert!(help.contains("setup install"), "{help}");
}

#[test]
fn subcommand_does_not_raise_or_log_nofile_limit() {
    use std::os::unix::process::CommandExt;

    let mut current = std::mem::MaybeUninit::<libc::rlimit>::uninit();
    assert_eq!(
        unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, current.as_mut_ptr()) },
        0
    );
    let current = unsafe { current.assume_init() };
    let lowered = libc::rlimit {
        rlim_cur: current.rlim_cur.min(256),
        rlim_max: current.rlim_max,
    };

    let server = start_server();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_benterm"));
    cmd.args(["split", "--down"])
        .env(SOCKET_ENV, &server.path)
        .env(PANE_ID_ENV, "3")
        .stdin(Stdio::null());
    // SAFETY: `setrlimit` is a direct syscall and async-signal-safe.
    unsafe {
        cmd.pre_exec(move || {
            if libc::setrlimit(libc::RLIMIT_NOFILE, &lowered) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let output = cmd.output().unwrap();

    assert!(output.status.success(), "{output:?}");
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.is_empty(),
        "CLI subcommand should not log RLIMIT_NOFILE: {stderr}"
    );
}

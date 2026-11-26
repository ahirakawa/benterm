//! A pane's credential is taken only from the processes of the pane's
//! program: the credential is in the program's environment,
//! where another process of the user can read it.
//!
//! The panes here are shells that lead sessions of their own, as the
//! programs the app starts on its PTYs do, and their clients are `nc`
//! processes the shells start.

mod common;

use std::io::{BufRead, BufReader, Write};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;

use benterm_ipc::{
    Client, ClientError, ErrorCode, ListPanes, PaneId, PermissionTable, Request, Server,
    ServerLimits,
};
use common::{FakeApp, private_dir};
use serde_json::{Value, json};

struct Fixture {
    app: Arc<FakeApp>,
    server: Server,
    path: std::path::PathBuf,
    _dir: tempfile::TempDir,
}

fn start_server() -> Fixture {
    let dir = private_dir();
    let path = dir.path().join("benterm.sock");
    let app = Arc::new(FakeApp::default());
    let server = Server::bind_with_permissions(
        &path,
        app.clone(),
        ServerLimits::default(),
        PermissionTable::new(true),
    )
    .unwrap();
    Fixture {
        app,
        server,
        path,
        _dir: dir,
    }
}

/// Stands in for the program of a pane: a shell that leads its session
/// and runs each line it reads as a command, printing `done` after it.
struct Pane {
    shell: Child,
    answers: BufReader<std::process::ChildStdout>,
}

impl Pane {
    fn start() -> Self {
        let mut command = Command::new("/bin/sh");
        command
            .args([
                "-c",
                r#"while read -r line; do eval "$line"; echo; echo done; done"#,
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped());
        // SAFETY: setsid is async-signal-safe.
        unsafe {
            command.pre_exec(|| match libc::setsid() {
                -1 => Err(std::io::Error::last_os_error()),
                _ => Ok(()),
            });
        }
        let mut shell = command.spawn().unwrap();
        let answers = BufReader::new(shell.stdout.take().unwrap());
        Self { shell, answers }
    }

    fn pid(&self) -> u32 {
        self.shell.id()
    }

    /// Runs `command` in the pane's shell and returns what it printed.
    fn run(&mut self, command: &str) -> String {
        let stdin = self.shell.stdin.as_mut().unwrap();
        writeln!(stdin, "{command}").unwrap();
        let mut printed = String::new();
        loop {
            let mut line = String::new();
            assert_ne!(self.answers.read_line(&mut line).unwrap(), 0, "{printed}");
            if line == "done\n" {
                return printed;
            }
            printed.push_str(&line);
        }
    }

    /// Sends `pane.list` with `token` from a process the pane's shell
    /// starts through `via`, and returns the server's answer.
    fn list_via(&mut self, via: &str, socket: &Path, token: &str) -> Value {
        let request = json!({"id": 1, "method": "pane.list", "params": {}, "token": token});
        let printed = self.run(&format!(
            "printf '%s\\n' '{request}' | {via} /usr/bin/nc -U -w 5 '{}'",
            socket.display()
        ));
        serde_json::from_str(printed.trim())
            .unwrap_or_else(|error| panic!("{error}: the client printed {printed:?}"))
    }

    fn list(&mut self, socket: &Path, token: &str) -> Value {
        self.list_via("", socket, token)
    }
}

impl Drop for Pane {
    fn drop(&mut self) {
        // SAFETY: the shell leads a group that holds only what it started.
        unsafe { libc::kill(-(self.shell.id() as libc::pid_t), libc::SIGKILL) };
        let _ = self.shell.wait();
    }
}

/// A pane with a program the table was told of, and its two credentials.
fn pane_with_credentials(server: &Fixture, id: u64) -> (Pane, String, String) {
    let permissions = server.server.permissions();
    let id = PaneId::new(id);
    permissions.expect_pane_process(id);
    let pane_token = permissions.issue_pane_token(id);
    let control_token = permissions.issue_control_token(Some(id));
    let pane = Pane::start();
    permissions.bind_pane_process(id, pane.pid());
    (pane, pane_token, control_token)
}

fn assert_refused(reply: &Value, token: &str) {
    assert_eq!(reply["error"]["code"], "unauthorized", "{reply}");
    let message = reply["error"]["message"].as_str().unwrap();
    assert!(message.contains("belongs to a pane"), "{message}");
    assert!(!message.contains(token), "{message}");
}

/// The input of the issue: the credential of a pane, used by a process
/// that is not one of the pane's.
#[test]
fn a_panes_credential_is_refused_from_a_process_of_another_pane() {
    let server = start_server();
    let (_first, pane_token, control_token) = pane_with_credentials(&server, 1);
    let (mut second, _, _) = pane_with_credentials(&server, 2);

    for token in [&pane_token, &control_token] {
        let reply = second.list(&server.path, token);
        assert_refused(&reply, token);
    }
    assert!(server.app.requests().is_empty());
}

#[test]
fn a_panes_credential_is_refused_from_a_process_outside_every_pane() {
    let server = start_server();
    let (_pane, _, control_token) = pane_with_credentials(&server, 1);

    // This process started the pane's program; it is not the program's.
    let mut client = Client::connect(&server.path)
        .unwrap()
        .with_token(Some(control_token.clone()));
    let error = client.call(&ListPanes {}).unwrap_err();
    assert!(
        matches!(
            error,
            ClientError::Api(ref api) if api.code == ErrorCode::Unauthorized
                && !api.message.contains(&control_token)
        ),
        "{error:?}"
    );
    assert!(server.app.requests().is_empty());
}

#[test]
fn a_panes_credential_is_taken_from_what_the_pane_started() {
    let server = start_server();
    let (mut pane, pane_token, control_token) = pane_with_credentials(&server, 1);

    let reply = pane.list(&server.path, &control_token);
    assert!(reply["result"]["panes"].is_array(), "{reply}");
    // The pane's own credential is known and the process is the pane's;
    // what it may ask is another matter.
    let reply = pane.list(&server.path, &pane_token);
    assert_eq!(reply["error"]["code"], "forbidden", "{reply}");
    // A deeper descendant, as a hook of an agent of a shell is.
    let reply = pane.list_via(
        "/bin/sh -c 'exec \"$@\"' sh /bin/sh -c 'exec \"$@\"' sh",
        &server.path,
        &control_token,
    );
    assert!(reply["result"]["panes"].is_array(), "{reply}");

    assert_eq!(
        server.app.requests(),
        vec![Request::ListPanes(ListPanes {}); 2]
    );
}

/// A command an agent runs in a sandbox is still a descendant of the
/// pane's program. Where a sandbox cannot be entered, as inside another,
/// the test has nothing to check.
#[cfg(target_os = "macos")]
#[test]
fn a_panes_credential_is_taken_from_a_sandboxed_process_of_the_pane() {
    let server = start_server();
    let (mut pane, _, control_token) = pane_with_credentials(&server, 1);
    let sandbox = "/usr/bin/sandbox-exec -p '(version 1)(allow default)'";
    if pane
        .run(&format!("{sandbox} /usr/bin/true; echo $?"))
        .trim()
        != "0"
    {
        eprintln!("skipped: sandbox-exec cannot start a sandbox here");
        return;
    }

    let reply = pane.list_via(sandbox, &server.path, &control_token);
    assert!(reply["result"]["panes"].is_array(), "{reply}");
}

/// A job that outlives the process that started it is adopted by the
/// first process of the system; it is still in the pane's session.
#[test]
fn a_panes_credential_is_taken_from_a_job_whose_parent_ended() {
    let server = start_server();
    let (mut pane, _, control_token) = pane_with_credentials(&server, 1);
    let dir = private_dir();
    let answer = dir.path().join("answer");
    let request = json!({"id": 1, "method": "pane.list", "params": {}, "token": control_token});
    // The inner shell ends at once; the job waits until it is an orphan.
    pane.run(&format!(
        "/bin/sh -c '(sleep 0.5; printf \"%s\\n\" \"$1\" | /usr/bin/nc -U -w 5 \"$2\" > \"$3.part\"; mv \"$3.part\" \"$3\") >/dev/null 2>&1 &' sh '{request}' '{}' '{}'",
        server.path.display(),
        answer.display()
    ));
    common::wait_until(|| answer.exists());
    let reply: Value =
        serde_json::from_str(std::fs::read_to_string(&answer).unwrap().trim()).unwrap();
    assert!(reply["result"]["panes"].is_array(), "{reply}");
}

/// A process that left the pane's session and its parents, as a daemon
/// does, is like any other process of the user.
#[test]
fn a_panes_credential_is_refused_from_a_daemon_the_pane_started() {
    let server = start_server();
    let (mut pane, _, control_token) = pane_with_credentials(&server, 1);
    let dir = private_dir();
    let answer = dir.path().join("answer");
    let request = json!({"id": 1, "method": "pane.list", "params": {}, "token": control_token});
    let script = dir.path().join("daemon.py");
    // Forks, leads a new session and waits to be an orphan.
    std::fs::write(
        &script,
        r#"import os, sys, time, socket
if os.fork():
    os._exit(0)
os.setsid()
while os.getppid() != 1:
    time.sleep(0.01)
client = socket.socket(socket.AF_UNIX)
client.connect(sys.argv[2])
client.sendall((sys.argv[1] + "\n").encode())
data = client.makefile().readline()
with open(sys.argv[3] + ".part", "w") as out:
    out.write(data)
os.rename(sys.argv[3] + ".part", sys.argv[3])
"#,
    )
    .unwrap();
    pane.run(&format!(
        "/usr/bin/python3 -I '{}' '{request}' '{}' '{}' >/dev/null 2>&1",
        script.display(),
        server.path.display(),
        answer.display()
    ));
    common::wait_until(|| answer.exists());
    let reply: Value =
        serde_json::from_str(std::fs::read_to_string(&answer).unwrap().trim()).unwrap();
    assert_refused(&reply, &control_token);
}

/// A request that comes before the app has told the pane's process waits
/// for it instead of being refused.
#[test]
fn a_request_sent_while_the_pane_starts_waits_for_its_process() {
    let server = start_server();
    let permissions = server.server.permissions().clone();
    let id = PaneId::new(1);
    permissions.expect_pane_process(id);
    let token = permissions.issue_control_token(Some(id));
    let mut pane = Pane::start();
    let pid = pane.pid();
    let teller = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(300));
        permissions.bind_pane_process(id, pid);
    });

    let reply = pane.list(&server.path, &token);
    teller.join().unwrap();
    assert!(reply["result"]["panes"].is_array(), "{reply}");
}

#[test]
fn a_pane_whose_process_is_never_told_is_refused() {
    let server = start_server();
    let permissions = server.server.permissions();
    let id = PaneId::new(1);
    permissions.expect_pane_process(id);
    let token = permissions.issue_control_token(Some(id));
    let mut pane = Pane::start();

    let reply = pane.list(&server.path, &token);
    assert_refused(&reply, &token);
}

/// A credential bound to no pane, and one of a pane no program was told
/// for, are not tied to a process.
#[test]
fn credentials_without_a_program_are_taken_from_any_process() {
    let server = start_server();
    let permissions = server.server.permissions();
    for token in [
        permissions.issue_control_token(None),
        permissions.issue_control_token(Some(PaneId::new(9))),
    ] {
        let mut client = Client::connect(&server.path)
            .unwrap()
            .with_token(Some(token));
        client.call(&ListPanes {}).unwrap();
    }
}

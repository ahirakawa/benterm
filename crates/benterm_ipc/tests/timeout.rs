//! A caller does not wait for an app that has stopped answering: the
//! client and the command line give up when their time runs out.

mod common;

use std::io::{BufRead, BufReader};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use benterm_ipc::cli::{self, CliEnv};
use benterm_ipc::{Client, ClientError, PaneId, PaneOutput, ReadOutput};
use common::{HELD_PANE, private_dir, start_server};

/// Plenty for a call that gives up after a fraction of a second, even on
/// a busy machine.
const SLACK: Duration = Duration::from_secs(5);

/// A server that takes connections and their requests and never answers.
/// The connections stay open for as long as the test runs. Each request
/// line is passed on to the test.
fn silent_server() -> (tempfile::TempDir, PathBuf, mpsc::Receiver<String>) {
    let dir = private_dir();
    let path = dir.path().join("silent.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let (sender, requests) = mpsc::channel();
    thread::spawn(move || {
        let mut open: Vec<UnixStream> = Vec::new();
        for stream in listener.incoming() {
            let stream = stream.unwrap();
            let mut line = String::new();
            let _ = BufReader::new(stream.try_clone().unwrap()).read_line(&mut line);
            let _ = sender.send(line);
            open.push(stream);
        }
    });
    (dir, path, requests)
}

struct Run {
    code: u8,
    stdout: String,
    stderr: String,
    took: Duration,
}

fn run_cli(socket: &Path, args: &[&str]) -> Run {
    let env = CliEnv {
        socket: Some(socket.as_os_str().to_owned()),
        pane: Some("3".into()),
        cwd: None,
        path: None,
        ..CliEnv::default()
    };
    let args: Vec<String> = args.iter().map(|arg| arg.to_string()).collect();
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let started = Instant::now();
    let code = cli::run(&args, &env, std::io::empty(), &mut stdout, &mut stderr);
    Run {
        code,
        stdout: String::from_utf8(stdout).unwrap(),
        stderr: String::from_utf8(stderr).unwrap(),
        took: started.elapsed(),
    }
}

fn read_pane(pane: u64) -> ReadOutput {
    ReadOutput {
        pane: PaneId::new(pane),
        lines: None,
    }
}

#[test]
fn a_call_to_a_server_that_never_answers_times_out() {
    let (_dir, path, _requests) = silent_server();
    let mut client = Client::connect(&path).unwrap();

    let started = Instant::now();
    let error = client
        .call_within(&read_pane(1), Duration::from_millis(200))
        .unwrap_err();

    let took = started.elapsed();
    assert!(matches!(error, ClientError::TimedOut(_)), "{error:?}");
    assert!(took >= Duration::from_millis(200), "{took:?}");
    assert!(took < SLACK, "{took:?}");
}

#[test]
fn the_timeout_error_says_the_request_may_have_been_carried_out() {
    let message = ClientError::TimedOut(Duration::from_millis(1500)).to_string();

    assert!(message.contains("1.5"), "{message}");
    assert!(message.contains("may"), "{message}");
}

#[test]
fn an_answer_that_comes_in_time_is_returned() {
    let server = start_server();
    let mut client = Client::connect(&server.path).unwrap();

    let output = client
        .call_within(&read_pane(4), Duration::from_secs(10))
        .unwrap();

    assert_eq!(
        output,
        PaneOutput {
            text: "screen of pane 4".into()
        }
    );
}

#[test]
fn every_command_gives_up_on_a_server_that_never_answers() {
    let (_dir, path, _requests) = silent_server();
    let commands: [&[&str]; 4] = [
        &["split"],
        &["new-workspace"],
        &["send", "--pane", "1", "hello"],
        &["read", "--pane", "1"],
    ];

    for command in commands {
        let mut args = vec!["--timeout", "0.2"];
        args.extend_from_slice(command);
        let run = run_cli(&path, &args);

        assert_eq!(run.code, 124, "{command:?}: {}", run.stderr);
        assert_eq!(run.stdout, "", "{command:?}");
        assert!(run.stderr.contains("0.2"), "{command:?}: {}", run.stderr);
        assert!(run.stderr.contains("may"), "{command:?}: {}", run.stderr);
        assert!(run.took < SLACK, "{command:?}: {:?}", run.took);
    }
}

#[test]
fn the_timeout_may_come_after_the_command() {
    let (_dir, path, _requests) = silent_server();

    let run = run_cli(&path, &["read", "--pane", "1", "--timeout", "0.2"]);

    assert_eq!(run.code, 124, "{}", run.stderr);
}

#[test]
fn a_command_waits_ten_seconds_by_default() {
    let (_dir, path, requests) = silent_server();
    let caller = thread::spawn(move || run_cli(&path, &["read", "--pane", "1"]));
    requests.recv_timeout(SLACK).unwrap();

    // Well past a short wait, the command is still waiting.
    thread::sleep(Duration::from_secs(1));
    assert!(!caller.is_finished());

    let run = caller.join().unwrap();
    assert_eq!(run.code, 124, "{}", run.stderr);
    assert!(run.took >= Duration::from_secs(10), "{:?}", run.took);
    assert!(run.took < Duration::from_secs(10) + SLACK, "{:?}", run.took);
}

#[test]
fn a_timeout_must_be_a_positive_number_of_seconds() {
    let (_dir, path, requests) = silent_server();

    for value in ["0", "-1", "abc", "inf", "NaN", ""] {
        let run = run_cli(&path, &["--timeout", value, "read", "--pane", "1"]);

        assert_eq!(run.code, 2, "{value:?}: {}", run.stderr);
        assert!(
            run.stderr.contains("--timeout"),
            "{value:?}: {}",
            run.stderr
        );
    }
    let run = run_cli(&path, &["read", "--pane", "1", "--timeout"]);
    assert_eq!(run.code, 2, "{}", run.stderr);
    assert!(requests.try_recv().is_err(), "nothing is sent");
}

#[test]
fn an_answer_that_comes_after_the_command_gave_up_harms_nothing() {
    let server = start_server();
    let pane = HELD_PANE.to_string();

    let run = run_cli(&server.path, &["--timeout", "0.2", "read", "--pane", &pane]);
    assert_eq!(run.code, 124, "{}", run.stderr);

    // The app answers after the command has gone.
    server.app.wait_for_held(1);
    for (request, reply) in server.app.take_held() {
        reply.ok(PaneOutput {
            text: common::output_of(&request),
        });
    }

    // The server still serves, and the connection of the command that
    // gave up is gone.
    let run = run_cli(&server.path, &["read", "--pane", "4"]);
    assert_eq!(
        (run.code, run.stdout.as_str(), run.stderr.as_str()),
        (0, "screen of pane 4\n", "")
    );
    common::wait_until(|| server.server.connection_count() == 0);
}

#[test]
fn a_client_that_timed_out_can_still_make_calls_that_are_answered() {
    let server = start_server();
    let mut client = Client::connect(&server.path).unwrap();

    let error = client
        .call_within(&read_pane(HELD_PANE), Duration::from_millis(100))
        .unwrap_err();
    assert!(matches!(error, ClientError::TimedOut(_)), "{error:?}");

    // The late answer to the first call is skipped, and the next call gets
    // its own.
    server.app.wait_for_held(1);
    for (request, reply) in server.app.take_held() {
        reply.ok(PaneOutput {
            text: common::output_of(&request),
        });
    }
    let output = client
        .call_within(&read_pane(4), Duration::from_secs(10))
        .unwrap();
    assert_eq!(output.text, "screen of pane 4");
}

#[test]
fn help_tells_the_timeout_and_what_a_timed_out_send_means() {
    let run = run_cli(Path::new("/nonexistent"), &["--help"]);

    assert_eq!(run.code, 0);
    assert!(run.stdout.contains("--timeout"), "{}", run.stdout);
    assert!(run.stdout.contains("124"), "{}", run.stdout);
    assert!(run.stdout.contains("twice"), "{}", run.stdout);
}

/// Sends a read of the held pane, with the given `timeout_ms` field if
/// any, and returns the reply the app holds for it.
fn held_read(server: &common::TestServer, timeout: Option<u64>) -> benterm_ipc::Reply<PaneOutput> {
    let mut client = common::RawClient::connect(&server.path);
    let timeout = timeout.map_or(String::new(), |ms| format!(r#","timeout_ms":{ms}"#));
    client.send(&format!(
        r#"{{"id":1,"method":"pane.read","params":{{"pane":{HELD_PANE}}}{timeout}}}"#
    ));
    server.app.wait_for_held(1);
    let (_, reply) = server.app.take_held().pop().unwrap();
    reply
}

#[test]
fn a_reply_tells_whether_the_time_of_its_request_has_run_out() {
    let server = start_server();

    let short = held_read(&server, Some(50));
    let long = held_read(&server, Some(60_000));
    let unlimited = held_read(&server, None);
    thread::sleep(Duration::from_millis(100));

    assert!(short.expired());
    assert!(!long.expired());
    assert!(!unlimited.expired());
}

#[test]
fn a_reply_is_not_expired_before_its_time_runs_out() {
    let server = start_server();

    let reply = held_read(&server, Some(500));

    assert!(!reply.expired());
    thread::sleep(Duration::from_millis(600));
    assert!(reply.expired());
}

#[test]
fn a_call_with_a_timeout_gives_the_app_less_time_than_it_waits() {
    let (_dir, path, requests) = silent_server();
    let mut client = Client::connect(&path).unwrap();

    let _ = client.call_within(&read_pane(1), Duration::from_secs(1));

    let request: serde_json::Value =
        serde_json::from_str(&requests.recv_timeout(SLACK).unwrap()).unwrap();
    let app_time = request["timeout_ms"].as_u64().expect("a timeout in ms");
    assert!((1..1000).contains(&app_time), "{app_time}");
}

#[test]
fn a_call_without_a_timeout_gives_the_app_no_deadline() {
    let (_dir, path, requests) = silent_server();
    let caller = thread::spawn(move || {
        let _ = Client::connect(&path).unwrap().call(&read_pane(1));
    });

    let request: serde_json::Value =
        serde_json::from_str(&requests.recv_timeout(SLACK).unwrap()).unwrap();
    assert!(request.get("timeout_ms").is_none(), "{request}");
    drop(caller);
}

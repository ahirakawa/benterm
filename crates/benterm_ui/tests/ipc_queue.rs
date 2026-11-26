//! The queue between the socket's reader threads and the UI thread, with
//! nobody taking calls from it: the readers must still answer.

use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use benterm_ipc::{Client, ClientError, ErrorCode, PaneId, ReadOutput, Server};
use benterm_ui::ipc;

const TIMEOUT: Duration = Duration::from_secs(10);

fn private_dir() -> tempfile::TempDir {
    tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap()
}

type Answer = Result<String, ClientError>;

/// Reads a pane on a connection of its own and sends the answer when it
/// comes.
fn read_in_background(path: &std::path::Path, pane: u64) -> mpsc::Receiver<Answer> {
    let (sender, answer) = mpsc::channel();
    let path = path.to_path_buf();
    thread::spawn(move || {
        let request = ReadOutput {
            pane: PaneId::new(pane),
            lines: None,
        };
        let result = Client::connect(path).unwrap().call(&request);
        let _ = sender.send(result.map(|output| output.text));
    });
    answer
}

fn code(answer: Answer) -> ErrorCode {
    match answer {
        Err(ClientError::Api(error)) => error.code,
        other => panic!("expected an error from the app, got {other:?}"),
    }
}

#[test]
fn a_full_queue_answers_at_once_and_a_dropped_queue_answers_the_rest() {
    let dir = private_dir();
    let path = dir.path().join("benterm.sock");
    let (sender, calls) = ipc::queue(2);
    let server = Server::bind(&path, Arc::new(sender)).unwrap();

    // Nobody takes the calls: two wait in the queue.
    let first = read_in_background(&path, 1);
    let second = read_in_background(&path, 2);
    let deadline = Instant::now() + TIMEOUT;
    while calls.waiting() < 2 {
        assert!(Instant::now() < deadline, "the calls did not arrive");
        thread::sleep(Duration::from_millis(2));
    }
    // The third finds the queue full and is told so without waiting.
    let third = read_in_background(&path, 3);
    assert_eq!(
        code(third.recv_timeout(TIMEOUT).unwrap()),
        ErrorCode::Failed
    );
    assert!(first.try_recv().is_err() && second.try_recv().is_err());

    // The UI side goes away: the waiting calls are answered, and so is
    // every later one.
    drop(calls);
    assert_eq!(
        code(first.recv_timeout(TIMEOUT).unwrap()),
        ErrorCode::Failed
    );
    assert_eq!(
        code(second.recv_timeout(TIMEOUT).unwrap()),
        ErrorCode::Failed
    );
    let late = read_in_background(&path, 4);
    assert_eq!(code(late.recv_timeout(TIMEOUT).unwrap()), ErrorCode::Failed);
    server.shutdown();
}

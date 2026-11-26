use std::io::{BufRead, BufReader};

use serde_json::Value;

use super::tests::CountingApp;
use super::*;

#[test]
fn invalid_first_request_releases_the_connection() {
    for (line, code) in [
        ("{\n", "invalid_request"),
        ("{\"id\":1,\"method\":\"pane.unknown\"}\n", "unknown_method"),
    ] {
        let (server_stream, mut client) = UnixStream::pair().unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        let mut read = BufReader::new(client.try_clone().unwrap());
        let reader = start_connection(
            1,
            server_stream,
            &Connections::default(),
            Arc::new(CountingApp::default()),
            ServerLimits {
                idle_timeout: Duration::from_secs(2),
                ..ServerLimits::default()
            },
            PermissionTable::new(true),
        )
        .unwrap();
        client.write_all(line.as_bytes()).unwrap();
        let mut response = String::new();
        read.read_line(&mut response).unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&response).unwrap()["error"]["code"],
            code
        );
        assert_eq!(read.read_line(&mut String::new()).unwrap(), 0);
        reader.join().unwrap();
    }
}

#[test]
fn an_unfinished_first_request_cannot_extend_the_authentication_deadline() {
    let (server_stream, mut client) = UnixStream::pair().unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    let reader = start_connection(
        1,
        server_stream,
        &Connections::default(),
        Arc::new(CountingApp::default()),
        ServerLimits {
            authentication_timeout: Duration::from_millis(100),
            idle_timeout: Duration::from_secs(2),
            partial_line_timeout: Duration::from_secs(2),
            ..ServerLimits::default()
        },
        PermissionTable::new(true),
    )
    .unwrap();
    client.write_all(b"{").unwrap();
    assert_eq!(client.read(&mut [0]).unwrap(), 0);
    reader.join().unwrap();
}

#[test]
fn unread_responses_are_limited_by_both_count_and_bytes() {
    let message = ServerMessage::error(None, ApiError::new(ErrorCode::InvalidRequest, "invalid"));
    let bytes = message.clone().to_line().len();
    for (max_pending_lines, max_pending_bytes) in [(2, bytes * 3), (3, bytes * 2)] {
        let (socket, _client) = UnixStream::pair().unwrap();
        let connection = Connection {
            state: Mutex::default(),
            changed: Condvar::new(),
            socket,
            id: ConnectionId(1),
            waited: AtomicBool::new(false),
            limits: ServerLimits {
                max_pending_lines,
                max_pending_bytes,
                ..ServerLimits::default()
            },
            permissions: PermissionTable::default(),
            trusted: true,
            peer: None,
        };
        assert!(connection.push(message.clone()));
        assert!(connection.push(message.clone()));
        assert!(!connection.push(message.clone()));
        assert!(connection.lock().closed);
        assert!(connection.next_line().is_none());
    }
}

#[test]
fn a_writer_gives_up_on_an_unread_line() {
    let (mut socket, _client) = UnixStream::pair().unwrap();
    let buffer: libc::c_int = 1024;
    // SAFETY: the option points to a live integer and the socket is open.
    assert_eq!(
        unsafe {
            libc::setsockopt(
                socket.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_SNDBUF,
                (&raw const buffer).cast(),
                std::mem::size_of_val(&buffer) as libc::socklen_t,
            )
        },
        0
    );
    let (done, result) = std::sync::mpsc::channel();
    let writer = thread::spawn(move || {
        let _ = done.send(write_line(
            &mut socket,
            &[b'x'; 8192],
            Duration::from_millis(50),
        ));
    });
    let error = result
        .recv_timeout(Duration::from_secs(1))
        .unwrap()
        .unwrap_err();
    assert!(matches!(
        error.kind(),
        ErrorKind::TimedOut | ErrorKind::WouldBlock
    ));
    writer.join().unwrap();
}

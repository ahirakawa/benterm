//! Where the socket lives and who may use it.

mod common;

use std::fs;
use std::io::ErrorKind;
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::Arc;

use benterm_ipc::{PANE_ID_ENV, SOCKET_ENV, Server, remove_dead_sockets, socket_path};
use common::{FakeApp, private_dir, start_server};

fn mode(path: &Path) -> u32 {
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

fn bind(path: &Path) -> std::io::Result<Server> {
    Server::bind(path, Arc::new(FakeApp::default()))
}

#[test]
fn only_the_owner_can_use_the_socket() {
    let server = start_server();

    assert!(fs::metadata(&server.path).unwrap().file_type().is_socket());
    assert_eq!(mode(&server.path), 0o600);
}

#[test]
fn a_missing_directory_is_created_for_the_owner_only() {
    let dir = private_dir();
    let path = dir.path().join("state/benterm/1.sock");

    let _server = bind(&path).unwrap();

    assert_eq!(mode(path.parent().unwrap()), 0o700);
    assert_eq!(mode(&path), 0o600);
}

#[test]
fn a_directory_open_to_others_is_refused() {
    let dir = private_dir();
    let shared = dir.path().join("shared");
    fs::create_dir(&shared).unwrap();
    fs::set_permissions(&shared, fs::Permissions::from_mode(0o755)).unwrap();
    let path = shared.join("1.sock");

    let error = bind(&path).err().expect("bind succeeded");

    assert_eq!(error.kind(), ErrorKind::PermissionDenied);
    assert!(!path.exists());
}

#[test]
fn a_socket_left_by_a_dead_server_is_replaced() {
    let dir = private_dir();
    let path = dir.path().join("1.sock");
    // A listener that is dropped leaves its file behind, like a crash does.
    drop(std::os::unix::net::UnixListener::bind(&path).unwrap());
    assert!(path.exists());

    let _server = bind(&path).unwrap();

    UnixStream::connect(&path).unwrap();
}

#[test]
fn a_socket_with_a_live_server_is_not_taken_over() {
    let server = start_server();

    let error = bind(&server.path).err().expect("bind succeeded");

    assert_eq!(error.kind(), ErrorKind::AddrInUse);
    UnixStream::connect(&server.path).unwrap();
}

#[test]
fn a_file_that_is_not_a_socket_is_not_removed() {
    let dir = private_dir();
    let path = dir.path().join("notes.txt");
    fs::write(&path, "keep me").unwrap();

    assert!(bind(&path).is_err());

    assert_eq!(fs::read_to_string(&path).unwrap(), "keep me");
}

#[test]
fn shutting_down_removes_the_socket() {
    let server = start_server();
    let path = server.path.clone();

    server.server.shutdown();

    assert!(!path.exists());
}

#[test]
fn dropping_the_server_removes_the_socket() {
    let server = start_server();
    let path = server.path.clone();

    drop(server.server);

    assert!(!path.exists());
}

#[test]
fn the_socket_is_named_after_the_process_under_the_state_directory() {
    assert_eq!(
        socket_path(Some("/xdg/state".as_ref()), Some("/home/me".as_ref()), 42).unwrap(),
        Path::new("/xdg/state/benterm/42.sock")
    );
    assert_eq!(
        socket_path(None, Some("/home/me".as_ref()), 42).unwrap(),
        Path::new("/home/me/.local/state/benterm/42.sock")
    );
    // An empty or relative XDG_STATE_HOME is ignored, as the XDG spec says.
    assert_eq!(
        socket_path(Some("".as_ref()), Some("/home/me".as_ref()), 7).unwrap(),
        Path::new("/home/me/.local/state/benterm/7.sock")
    );
    assert_eq!(
        socket_path(Some("state".as_ref()), Some("/home/me".as_ref()), 7).unwrap(),
        Path::new("/home/me/.local/state/benterm/7.sock")
    );
    assert_eq!(socket_path(None, None, 7), None);
}

#[test]
fn the_environment_variables_have_fixed_names() {
    assert_eq!(SOCKET_ENV, "BENTERM_SOCKET");
    assert_eq!(PANE_ID_ENV, "BENTERM_PANE_ID");
}

/// Process ids no process has: they are above the largest id macOS (99999)
/// and Linux (2^22) give out. Starting a process to get the id of one that
/// has exited would not do: the child would hold the other tests' sockets
/// open while it starts, so a listener they dropped would still answer.
const DEAD_PIDS: [u32; 2] = [i32::MAX as u32, i32::MAX as u32 - 1];

/// Leaves a socket file nobody listens on, like a crash does.
fn leave_dead_socket(path: &Path) {
    drop(std::os::unix::net::UnixListener::bind(path).unwrap());
}

#[test]
fn only_dead_sockets_of_benterm_are_cleaned_up() {
    let dir = private_dir();
    let at = |name: &str| dir.path().join(name);
    let dead = at(&format!("{}.sock", DEAD_PIDS[0]));
    leave_dead_socket(&dead);
    // Another Benterm, such as a development build, is listening.
    let live = start_server_at(&at(&format!("{}.sock", DEAD_PIDS[1])));
    fs::write(at("123.sock"), "not a socket").unwrap();
    let odd_names = [
        "benterm.sock",
        "12a.sock",
        "12.sock.old",
        ".sock",
        "+12.sock",
    ];
    for name in odd_names {
        leave_dead_socket(&at(name));
    }

    let removed = remove_dead_sockets(dir.path()).unwrap();

    assert_eq!(removed, vec![dead.clone()]);
    assert!(!dead.exists());
    UnixStream::connect(live.path()).unwrap();
    assert_eq!(fs::read_to_string(at("123.sock")).unwrap(), "not a socket");
    for name in odd_names {
        assert!(at(name).exists(), "{name} was removed");
    }
}

#[test]
fn a_socket_whose_process_is_running_is_kept_even_when_nobody_answers() {
    // A Benterm that is starting has its process before it listens, and a
    // busy one may refuse a connection for a moment.
    let dir = private_dir();
    let starting = dir.path().join(format!("{}.sock", std::process::id()));
    leave_dead_socket(&starting);

    let removed = remove_dead_sockets(dir.path()).unwrap();

    assert!(removed.is_empty());
    assert!(starting.exists());
}

#[test]
fn cleaning_up_a_missing_directory_removes_nothing() {
    let dir = private_dir();

    let removed = remove_dead_sockets(&dir.path().join("missing")).unwrap();

    assert!(removed.is_empty());
}

fn start_server_at(path: &Path) -> Server {
    bind(path).unwrap()
}

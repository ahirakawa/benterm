mod common;

use benterm_pty::{PtyConfig, PtySession};
use common::collect_to_exit;

#[test]
fn command_output_arrives_before_exit() {
    let (_session, events) =
        PtySession::spawn(PtyConfig::command("/bin/sh", ["-c", "echo hello"])).unwrap();
    let collected = collect_to_exit(&events);
    assert!(collected.text().contains("hello"), "{:?}", collected.text());
    assert!(collected.exit.unwrap().success());
}

#[test]
fn a_missing_program_fails_to_start() {
    for program in ["/nonexistent/benterm-test", "benterm-test-no-such-program"] {
        let result = PtySession::spawn(PtyConfig::command(program, [""; 0]));
        let error = result.err().expect("a missing program started");
        assert_eq!(error.kind(), std::io::ErrorKind::NotFound, "{program}");
    }
}

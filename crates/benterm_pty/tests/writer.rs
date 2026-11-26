//! Writing from a thread that does not own the session.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

use benterm_pty::{PtyConfig, PtySession};
use common::{Collected, collect_until, within};

#[test]
fn a_writer_on_another_thread_reaches_the_child() {
    let (session, events) = PtySession::spawn(PtyConfig::command("/bin/cat", [""; 0])).unwrap();
    let writer = session.writer();
    thread::spawn(move || writer.write(b"from the writer\n").unwrap())
        .join()
        .unwrap();

    let mut collected = Collected::default();
    // The terminal echoes the line and cat prints it again.
    collect_until(&events, &mut collected, |c| {
        c.text().matches("from the writer").count() == 2
    });
    assert_eq!(collected.text().matches("from the writer").count(), 2);
}

/// A writer must not keep the session from ending, and must fail instead
/// of hanging once the child is gone.
#[test]
fn a_writer_fails_after_the_session_shut_down() {
    let (session, _events) = PtySession::spawn(PtyConfig::command("/bin/cat", [""; 0])).unwrap();
    let writer = session.writer();
    within("shutdown", move || session.shutdown());
    let result = within("write", move || writer.write(b"too late\n"));
    assert!(result.is_err());
}

/// The same while other threads start sessions. A child
/// started on another thread must not keep this session's terminal open.
#[test]
fn a_writer_fails_after_the_session_shut_down_while_other_sessions_start() {
    let stop = Arc::new(AtomicBool::new(false));
    let starters: Vec<_> = (0..2)
        .map(|_| {
            let stop = Arc::clone(&stop);
            thread::spawn(move || {
                while !stop.load(Ordering::SeqCst) {
                    let (session, events) =
                        PtySession::spawn(PtyConfig::command("/usr/bin/true", [""; 0])).unwrap();
                    drop(session);
                    drop(events);
                }
            })
        })
        .collect();
    let late = (0..300)
        .filter(|_| {
            let (session, _events) =
                PtySession::spawn(PtyConfig::command("/bin/cat", [""; 0])).unwrap();
            let writer = session.writer();
            session.shutdown();
            writer.write(b"too late\n").is_ok()
        })
        .count();
    stop.store(true, Ordering::SeqCst);
    for starter in starters {
        starter.join().unwrap();
    }
    assert_eq!(
        late, 0,
        "writes after shutdown succeeded {late} times of 300"
    );
}

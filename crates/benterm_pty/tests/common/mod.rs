//! Helpers shared by the integration tests. Each test binary uses a
//! different subset.
#![allow(dead_code)]

use std::process::ExitStatus;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use benterm_pty::PtyEvent;

pub const TIMEOUT: Duration = Duration::from_secs(10);

/// Output and exit status collected from a session's events.
#[derive(Default)]
pub struct Collected {
    pub output: Vec<u8>,
    pub exit: Option<ExitStatus>,
}

impl Collected {
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.output).into_owned()
    }
}

/// Reads events until `done` holds or the exit event arrives.
/// Panics if neither happens within `TIMEOUT`, or if any event follows
/// the exit event.
pub fn collect_until(
    events: &Receiver<PtyEvent>,
    collected: &mut Collected,
    mut done: impl FnMut(&Collected) -> bool,
) {
    let deadline = Instant::now() + TIMEOUT;
    while !done(collected) && collected.exit.is_none() {
        let left = deadline.saturating_duration_since(Instant::now());
        match events.recv_timeout(left) {
            Ok(PtyEvent::Output(bytes)) => collected.output.extend_from_slice(&bytes),
            Ok(PtyEvent::Exited(status)) => collected.exit = Some(status),
            Err(e) => panic!(
                "no event before timeout ({e}); output so far: {:?}",
                collected.text()
            ),
        }
    }
}

/// Reads every event up to and including the exit event, then checks that
/// the channel is closed (the reader thread has ended).
pub fn collect_to_exit(events: &Receiver<PtyEvent>) -> Collected {
    let mut collected = Collected::default();
    collect_until(events, &mut collected, |_| false);
    assert!(
        events.recv_timeout(TIMEOUT).is_err(),
        "an event arrived after the exit event"
    );
    collected
}

/// Runs `f` on another thread and panics if it does not return within
/// `TIMEOUT`, so a hang fails the test instead of stalling it.
pub fn within<T: Send + 'static>(what: &str, f: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx.recv_timeout(TIMEOUT)
        .unwrap_or_else(|_| panic!("{what} did not return within {TIMEOUT:?}"))
}

/// Bytes around `at`, escaped so that control characters are visible.
fn context(bytes: &[u8], at: usize) -> String {
    let start = at.saturating_sub(32);
    let end = (at + 32).min(bytes.len());
    bytes[start..end]
        .iter()
        .flat_map(|&b| std::ascii::escape_default(b))
        .map(char::from)
        .collect()
}

/// Asserts that `got` equals `expected`. On failure, reports both lengths,
/// the first offset where they differ, and the bytes around it in each.
pub fn assert_bytes_eq(got: &[u8], expected: &[u8]) {
    if got == expected {
        return;
    }
    let at = got
        .iter()
        .zip(expected)
        .position(|(g, e)| g != e)
        .unwrap_or(got.len().min(expected.len()));
    panic!(
        "bytes differ at offset {at}: got {} bytes, expected {}\n  got:      \"{}\"\n  expected: \"{}\"",
        got.len(),
        expected.len(),
        context(got, at),
        context(expected, at),
    );
}

/// Runs `/bin/sh -c script` on a pseudo terminal opened here, without
/// `PtySession`, and reads its output a little at a time with pauses, so
/// that the kernel output queue fills up again and again.
pub fn read_slowly(script: &str) -> Vec<u8> {
    use std::fs::File;
    use std::io::Read;
    use std::os::fd::{FromRawFd, OwnedFd};
    use std::process::{Command, Stdio};

    let (mut master, slave) = unsafe {
        let mut master = -1;
        let mut slave = -1;
        let ret = libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        );
        assert_eq!(ret, 0, "openpty: {}", std::io::Error::last_os_error());
        (File::from_raw_fd(master), OwnedFd::from_raw_fd(slave))
    };
    let mut child = Command::new("/bin/sh")
        .args(["-c", script])
        .stdin(Stdio::from(slave.try_clone().unwrap()))
        .stdout(Stdio::from(slave.try_clone().unwrap()))
        .stderr(Stdio::from(slave))
        .spawn()
        .unwrap();
    // `Command` has dropped our copies of the slave, so the master reports
    // EOF or EIO once the child has exited and the queue is empty.
    let mut output = Vec::new();
    let mut buf = [0; 100];
    std::thread::sleep(Duration::from_millis(100));
    loop {
        match master.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => output.extend_from_slice(&buf[..n]),
            Err(e) if e.raw_os_error() == Some(libc::EIO) => break,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => panic!("read from PTY master: {e}"),
        }
        std::thread::sleep(Duration::from_micros(500));
    }
    assert!(child.wait().unwrap().success());
    output
}

/// A directory of this test's own, removed on drop. Its path has every
/// symbolic link resolved, as the kernel reports directories.
pub struct TestDir(pub std::path::PathBuf);

impl TestDir {
    pub fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!("benterm-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        Self(path.canonicalize().unwrap())
    }

    /// Makes the directory `name` in this one and returns its path.
    pub fn sub(&self, name: &str) -> std::path::PathBuf {
        let path = self.0.join(name);
        std::fs::create_dir_all(&path).unwrap();
        path
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Asks `read` again and again until it gives `expected`. Panics with
/// the last answer if it does not within `TIMEOUT`.
pub fn wait_for<T: PartialEq + std::fmt::Debug>(what: &str, expected: T, read: impl Fn() -> T) {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let got = read();
        if got == expected {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "{what}: expected {expected:?}, last got {got:?}"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

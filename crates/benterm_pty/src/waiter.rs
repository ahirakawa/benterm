//! The waiter thread.

use std::fs::File;
use std::io::Write;
use std::process::ExitStatus;
use std::sync::{Condvar, Mutex};
use std::time::Duration;

use crate::sys;

/// The exit status, shared by the waiter, the reader and the session.
#[derive(Default)]
pub struct ExitSlot {
    status: Mutex<Option<ExitStatus>>,
    changed: Condvar,
}

impl ExitSlot {
    pub fn get(&self) -> Option<ExitStatus> {
        *self.status.lock().unwrap()
    }

    /// Waits up to `timeout` for the status. Returns whether it is set.
    pub fn wait_timeout(&self, timeout: Duration) -> bool {
        let status = self.status.lock().unwrap();
        let (status, _) = self
            .changed
            .wait_timeout_while(status, timeout, |s| s.is_none())
            .unwrap();
        status.is_some()
    }

    /// Runs `f` while holding the lock, if the child is not yet reaped.
    /// The waiter reaps under the same lock, so `f` cannot signal a pid
    /// that has been reaped and reused.
    pub fn if_not_reaped(&self, f: impl FnOnce()) {
        let status = self.status.lock().unwrap();
        if status.is_none() {
            f();
        }
    }
}

/// Body of the waiter thread.
pub fn run(pid: libc::pid_t, slot: &ExitSlot, wake: File) {
    // The child has exited, not merely stopped; it is a zombie until
    // reaped below, so its pid cannot be reused while `if_not_reaped`
    // holds the lock.
    sys::wait_exit_no_reap(pid).expect("waiting on our own child");

    // Reap and store the status under one lock. The child is
    // a zombie, so `reap` returns at once and the lock is not held while
    // anything waits on the child.
    {
        let mut status = slot.status.lock().unwrap();
        *status = Some(sys::reap(pid).expect("waitpid on our own child"));
        slot.changed.notify_all();
    }

    // Wake the reader, then end.
    let _ = (&wake).write_all(&[1]);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use std::os::fd::FromRawFd;
    use std::os::unix::process::ExitStatusExt;
    use std::sync::Arc;
    use std::sync::mpsc;

    /// Long enough for a waiter that wrongly takes a stop for an exit to
    /// get there: it does so within microseconds of the stop.
    const STILL: Duration = Duration::from_millis(500);
    /// Long enough for anything that is coming to come.
    const TIMEOUT: Duration = Duration::from_secs(10);

    /// Starts `sleep 30` as our child and returns its pid. The child is
    /// reaped by the code under test, not by `std::process::Child`. It
    /// gets none of our output pipes, so that a stopped child left behind
    /// by a failed test does not keep them open.
    #[expect(
        clippy::zombie_processes,
        reason = "the code under test reaps the child"
    )]
    fn start_sleep() -> libc::pid_t {
        let child = std::process::Command::new("/bin/sleep")
            .arg("30")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        child.id() as libc::pid_t
    }

    /// Kills the child if the test fails, so that it does not stay
    /// stopped after the test binary has ended.
    struct KillOnPanic(libc::pid_t);

    impl Drop for KillOnPanic {
        fn drop(&mut self) {
            if std::thread::panicking() {
                unsafe { libc::kill(self.0, libc::SIGKILL) };
            }
        }
    }

    fn signal(pid: libc::pid_t, signal: libc::c_int) {
        assert_eq!(unsafe { libc::kill(pid, signal) }, 0);
    }

    /// Stops `pid` and returns once the kernel says it has stopped.
    fn stop(pid: libc::pid_t) {
        signal(pid, libc::SIGSTOP);
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let ret = unsafe {
            libc::waitid(
                libc::P_PID,
                pid as libc::id_t,
                &mut info,
                libc::WSTOPPED | libc::WNOWAIT,
            )
        };
        assert_eq!(ret, 0);
        assert_eq!(info.si_code, libc::CLD_STOPPED);
    }

    fn pipe() -> (File, File) {
        let mut fds = [0; 2];
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        unsafe { (File::from_raw_fd(fds[0]), File::from_raw_fd(fds[1])) }
    }

    /// Runs `f` on another thread and panics if it does not return
    /// within `TIMEOUT`, so a hang on the slot's lock fails the test.
    fn within<T: Send + 'static>(what: &str, f: impl FnOnce() -> T + Send + 'static) -> T {
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(f());
        });
        rx.recv_timeout(TIMEOUT)
            .unwrap_or_else(|_| panic!("{what} did not return within {TIMEOUT:?}"))
    }

    #[test]
    fn a_stopped_child_is_not_taken_for_an_exited_one() {
        let pid = start_sleep();
        let _guard = KillOnPanic(pid);
        stop(pid);
        let slot = Arc::new(ExitSlot::default());
        let (mut wake_read, wake_write) = pipe();
        let waiter = {
            let slot = Arc::clone(&slot);
            std::thread::spawn(move || run(pid, &slot, wake_write))
        };

        // The slot's lock stays free and the status stays unset.
        let s = Arc::clone(&slot);
        assert!(!within("wait_timeout", move || s.wait_timeout(STILL)));
        let s = Arc::clone(&slot);
        assert_eq!(within("get", move || s.get()), None);
        let s = Arc::clone(&slot);
        assert!(within("if_not_reaped", move || {
            let mut ran = false;
            s.if_not_reaped(|| ran = true);
            ran
        }));

        // Once the child goes on and is ended, it is reaped once.
        signal(pid, libc::SIGCONT);
        signal(pid, libc::SIGTERM);
        let s = Arc::clone(&slot);
        assert!(within("wait_timeout", move || s.wait_timeout(TIMEOUT)));
        assert_eq!(slot.get().unwrap().signal(), Some(libc::SIGTERM));
        waiter.join().unwrap();
        let mut byte = [0];
        wake_read.read_exact(&mut byte).unwrap();
        assert_eq!(
            unsafe { libc::kill(pid, 0) },
            -1,
            "the child was not reaped"
        );
    }

    #[test]
    fn a_child_killed_while_stopped_is_reaped() {
        let pid = start_sleep();
        let _guard = KillOnPanic(pid);
        stop(pid);
        let slot = Arc::new(ExitSlot::default());
        let (_wake_read, wake_write) = pipe();
        let waiter = {
            let slot = Arc::clone(&slot);
            std::thread::spawn(move || run(pid, &slot, wake_write))
        };
        signal(pid, libc::SIGKILL);
        let s = Arc::clone(&slot);
        assert!(within("wait_timeout", move || s.wait_timeout(TIMEOUT)));
        assert_eq!(slot.get().unwrap().signal(), Some(libc::SIGKILL));
        waiter.join().unwrap();
    }

    #[test]
    #[expect(
        clippy::zombie_processes,
        reason = "the code under test reaps the child"
    )]
    fn a_child_that_exited_before_the_waiter_started_is_reaped() {
        let child = std::process::Command::new("/bin/sh")
            .args(["-c", "exit 7"])
            .spawn()
            .unwrap();
        let pid = child.id() as libc::pid_t;
        // Wait, without reaping, until the child is a zombie.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let ret = unsafe {
            libc::waitid(
                libc::P_PID,
                pid as libc::id_t,
                &mut info,
                libc::WEXITED | libc::WNOWAIT,
            )
        };
        assert_eq!(ret, 0);
        let slot = ExitSlot::default();
        let (_wake_read, wake_write) = pipe();
        run(pid, &slot, wake_write);
        assert_eq!(slot.get().unwrap().code(), Some(7));
    }
}

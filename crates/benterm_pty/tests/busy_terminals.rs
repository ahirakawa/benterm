//! Starting sessions while other threads and processes open and close
//! pseudo terminals.
//!
//! Other apps, other test binaries and the other panes of the app open
//! and close terminals all the time. Starting a session must not fail
//! because of them.

mod common;

use std::env;
use std::process::{Child, Command, Stdio};
use std::thread;

use benterm_pty::{PtyConfig, PtySession};
use common::collect_to_exit;

/// Set in the copies of this binary that open and close terminals.
const CHURN: &str = "BENTERM_PTY_CHURN";
const CHURN_TEST: &str = "a_session_starts_while_other_processes_open_and_close_terminals";
const CHURNERS: usize = 4;
/// How many terminals each churner opens before closing them all.
const BATCH: usize = 50;

/// Opens `BATCH` terminals, looks up the name of each as other processes
/// using this crate do, and closes them, over and over, until killed.
/// Failures are ignored: only the other side of the race is tested.
fn churn() -> ! {
    loop {
        let mut open = Vec::new();
        for _ in 0..BATCH {
            let (mut master, mut slave) = (-1, -1);
            let ret = unsafe {
                libc::openpty(
                    &mut master,
                    &mut slave,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            };
            if ret == 0 {
                let mut name = [0 as libc::c_char; 128];
                unsafe { libc::ttyname_r(slave, name.as_mut_ptr(), name.len()) };
                open.push((master, slave));
            }
        }
        for (master, slave) in open {
            unsafe {
                libc::close(slave);
                libc::close(master);
            }
        }
    }
}

/// Copies of this binary running [`churn`], killed on drop.
struct Churners(Vec<Child>);

impl Churners {
    fn start() -> Self {
        let me = env::current_exe().unwrap();
        Self(
            (0..CHURNERS)
                .map(|_| {
                    Command::new(&me)
                        .args([CHURN_TEST, "--exact"])
                        .env(CHURN, "1")
                        .stdout(Stdio::null())
                        .spawn()
                        .unwrap()
                })
                .collect(),
        )
    }
}

impl Drop for Churners {
    fn drop(&mut self) {
        for child in &mut self.0 {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Starts a session running `/usr/bin/true` and checks that it ran.
fn start_one() {
    let (_session, events) = PtySession::spawn(PtyConfig::command("/usr/bin/true", [""; 0]))
        .unwrap_or_else(|e| panic!("the session did not start: {e}"));
    assert!(collect_to_exit(&events).exit.unwrap().success());
}

#[test]
fn a_session_starts_while_other_processes_open_and_close_terminals() {
    if env::var_os(CHURN).is_some() {
        churn();
    }
    let _churners = Churners::start();
    for _ in 0..300 {
        start_one();
    }
}

#[test]
fn sessions_start_on_many_threads_at_once() {
    let starters: Vec<_> = (0..8)
        .map(|_| thread::spawn(|| (0..400).for_each(|_| start_one())))
        .collect();
    for starter in starters {
        starter.join().unwrap();
    }
}

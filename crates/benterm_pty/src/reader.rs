//! The reader thread.

use std::fs::File;
use std::io::{self, Read};
use std::os::fd::AsRawFd;
use std::sync::mpsc::Sender;

use crate::PtyEvent;
use crate::sys;
use crate::waiter::ExitSlot;

const BUF_SIZE: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    /// Waiting on both the master and the wake pipe.
    Poll,
    /// The master returned EOF/EIO; waiting on the wake pipe only.
    Hup,
    /// The child is reaped; reading what is left without blocking.
    Drain,
}

enum ReadResult {
    Data(usize),
    /// EOF or EIO: no process holds the slave and the buffer is empty.
    Hup,
    /// Interrupted or nothing to read; try again.
    Retry,
}

fn read_master(master: &File, buf: &mut [u8]) -> ReadResult {
    match (&*master).read(buf) {
        Ok(0) => ReadResult::Hup,
        Ok(n) => ReadResult::Data(n),
        Err(e) if e.raw_os_error() == Some(libc::EIO) => ReadResult::Hup,
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
            ) =>
        {
            ReadResult::Retry
        }
        Err(e) => panic!("read from PTY master: {e}"),
    }
}

/// Body of the reader thread.
pub fn run(master: &File, wake: File, slot: &ExitSlot, events: Sender<PtyEvent>) {
    let mut buf = vec![0; BUF_SIZE];
    // If the app has dropped the receiver, keep reading and discard, so
    // the child never blocks on a full PTY buffer.
    let send = |bytes: &[u8]| {
        let _ = events.send(PtyEvent::Output(bytes.to_vec()));
    };
    let mut state = State::Poll;
    loop {
        state = match state {
            State::Poll => {
                let [m, w] = sys::poll_read([master.as_raw_fd(), wake.as_raw_fd()], -1)
                    .expect("poll PTY master");
                if w.readable {
                    State::Drain
                } else if m.readable {
                    match read_master(master, &mut buf) {
                        ReadResult::Data(n) => {
                            send(&buf[..n]);
                            State::Poll
                        }
                        ReadResult::Hup => State::Hup,
                        ReadResult::Retry => State::Poll,
                    }
                } else {
                    State::Poll
                }
            }
            State::Hup => {
                let [w] = sys::poll_read([wake.as_raw_fd()], -1).expect("poll wake pipe");
                if w.readable {
                    State::Drain // The waiter has signaled that the child exited.
                } else {
                    State::Hup
                }
            }
            State::Drain => {
                let [m] = sys::poll_read([master.as_raw_fd()], 0).expect("poll PTY master");
                let more = m.readable
                    && match read_master(master, &mut buf) {
                        ReadResult::Data(n) => {
                            send(&buf[..n]);
                            true
                        }
                        ReadResult::Hup => false,
                        ReadResult::Retry => true,
                    };
                if !more {
                    // The waiter set the status before waking us.
                    let status = slot.get().expect("woken before the status was set");
                    let _ = events.send(PtyEvent::Exited(status));
                    return;
                }
                State::Drain
            }
        };
    }
}

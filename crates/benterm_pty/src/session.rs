//! The session handle.

use std::env;
use std::ffi::{CString, OsStr, OsString};
use std::fs::File;
use std::io::{self, Write};
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::env::child_env;
use crate::shell_integration::{self, Shell};
use crate::waiter::ExitSlot;
use crate::{ForegroundProcess, PtyConfig, PtyEvent, WindowSize, reader, sys, waiter};

/// Held by [`PtySession::spawn`] from opening the terminal until the
/// parent's copy of the slave is closed.
///
/// The parent holds the slave while it starts the child: the size set
/// on the slave is lost when its last holder closes it. A child that
/// another thread starts in that time gets a copy of every descriptor
/// until its exec closes them, so a child of another session could hold
/// this session's slave, and keep the terminal open after this session's
/// child has gone. A child started by other code, outside
/// this lock, still can.
static SPAWN_LOCK: Mutex<()> = Mutex::new(());

/// How long `shutdown` waits after SIGHUP before sending SIGKILL.
const HANGUP_GRACE: Duration = Duration::from_secs(2);

const WAITER_THREAD: &str = "benterm-pty-waiter";
const READER_THREAD: &str = "benterm-pty-reader";

/// Starts a thread of a session. Tests can make it fail.
fn start_thread(
    name: &'static str,
    body: impl FnOnce() + Send + 'static,
) -> io::Result<JoinHandle<()>> {
    #[cfg(test)]
    if tests::fails(name) {
        return Err(io::Error::other(format!("{name} not started (test)")));
    }
    thread::Builder::new().name(name.into()).spawn(body)
}

/// A child process running on a pseudo terminal.
///
/// Dropping the session shuts it down (see [`PtySession::shutdown`]).
pub struct PtySession {
    pid: libc::pid_t,
    master: Arc<File>,
    slot: Arc<ExitSlot>,
    reader: Option<JoinHandle<()>>,
    waiter: Option<JoinHandle<()>>,
}

impl PtySession {
    /// Starts the child. The returned receiver gets output in order,
    /// followed by one `Exited` event before the channel closes.
    pub fn spawn(config: PtyConfig) -> io::Result<(Self, Receiver<PtyEvent>)> {
        let child = ChildArgs::new(&config)?;
        // Made before the child, so that a failure here leaves no child.
        let (wake_read, wake_write) = sys::pipe()?;
        let (events_tx, events_rx) = mpsc::channel();
        let (master, pid) = {
            let _alone = SPAWN_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
            let pty = sys::openpty(config.size)?;
            let pid = sys::spawn(&pty.slave_path, &child.as_spawn_args())?;
            #[cfg(test)]
            tests::spawned(pid);
            // Drop the parent's copy, so the master sees EOF once the
            // child side is closed.
            drop(pty.slave);
            (pty.master, pid)
        };

        let master = Arc::new(master);
        let slot = Arc::new(ExitSlot::default());

        // A thread that fails to start drops what was moved into it, so
        // only the child is left to end. Its group gets SIGKILL with no
        // grace: the caller has not seen the session, and the child has
        // had no input.
        let waiter = {
            let slot = Arc::clone(&slot);
            start_thread(WAITER_THREAD, move || waiter::run(pid, &slot, wake_write))
        };
        let waiter = match waiter {
            Ok(waiter) => waiter,
            Err(e) => {
                // No one else reaps the child, so its pid is ours here.
                // `reap` does not wait, so wait for the end of the child
                // first; no lock is held here.
                let _ = sys::killpg(pid, libc::SIGKILL);
                let _ = sys::wait_exit_no_reap(pid).and_then(|()| sys::reap(pid));
                return Err(e);
            }
        };
        let reader = {
            let master = Arc::clone(&master);
            let slot = Arc::clone(&slot);
            start_thread(READER_THREAD, move || {
                reader::run(&master, wake_read, &slot, events_tx)
            })
        };
        let reader = match reader {
            Ok(reader) => reader,
            Err(e) => {
                // The waiter reaps the child. Joining it closes the write
                // end of the wake pipe; the write it makes into the pipe
                // with no reader fails without a SIGPIPE.
                slot.if_not_reaped(|| drop(sys::killpg(pid, libc::SIGKILL)));
                let _ = waiter.join();
                return Err(e);
            }
        };

        let session = Self {
            pid,
            master,
            slot,
            reader: Some(reader),
            waiter: Some(waiter),
        };
        Ok((session, events_rx))
    }

    /// Writes `bytes` to the child's terminal input.
    ///
    /// This blocks while the kernel input buffer is full, that is, while
    /// the child does not read its input. After every process holding the
    /// terminal has exited, it fails with EIO.
    pub fn write(&self, bytes: &[u8]) -> io::Result<()> {
        (&*self.master).write_all(bytes)
    }

    /// A handle that writes to the child's terminal input from another
    /// thread.
    ///
    /// `write` can block for as long as the child does not read, so a
    /// caller that must stay responsive gives a writer to a thread of its
    /// own. The writer does not keep the session alive: after `shutdown`
    /// its writes fail, and a write blocked at that moment returns as soon
    /// as no process holds the terminal.
    pub fn writer(&self) -> PtyWriter {
        PtyWriter {
            master: Arc::clone(&self.master),
        }
    }

    /// Tells the child the new terminal size.
    ///
    /// The kernel sends SIGWINCH to the foreground process group. This
    /// works whether or not the child is still running.
    pub fn resize(&self, size: WindowSize) -> io::Result<()> {
        sys::set_window_size(self.master.as_raw_fd(), size)
    }

    /// Ends the session: hangs up the child, joins both threads and
    /// closes the PTY.
    ///
    /// If the child has not exited, its process group gets SIGHUP, and
    /// SIGKILL if it is still alive after a grace period. The exit event
    /// is still sent before the channel closes.
    pub fn shutdown(mut self) {
        self.shutdown_inner();
    }

    fn shutdown_inner(&mut self) {
        if self.reader.is_none() {
            return;
        }
        // The child started a new session, so its process group id is its
        // pid, and the group holds only the child and what it started.
        // `if_not_reaped` keeps the pid from being reused under us.
        self.slot
            .if_not_reaped(|| drop(sys::killpg(self.pid, libc::SIGHUP)));
        if !self.slot.wait_timeout(HANGUP_GRACE) {
            self.slot
                .if_not_reaped(|| drop(sys::killpg(self.pid, libc::SIGKILL)));
        }
        let _ = self.waiter.take().map(JoinHandle::join);
        let _ = self.reader.take().map(JoinHandle::join);
    }

    /// The directory the child is in, with every symbolic link resolved.
    ///
    /// This asks the kernel on each call and waits for the answer, so
    /// the answer is new as soon as the child has changed its directory:
    /// a shell's `cd` shows here before the shell writes anything after
    /// it. `None` once the child
    /// has exited. A directory that has been removed is still returned,
    /// so the caller checks it before starting a program in it.
    pub fn working_directory(&self) -> Option<PathBuf> {
        let mut cwd = None;
        // A reaped pid may belong to another process by now.
        self.slot
            .if_not_reaped(|| cwd = sys::working_directory(self.pid));
        cwd
    }

    /// The directory of the program in front: the first process of the
    /// process group that reads the keyboard.
    ///
    /// While the child is a shell waiting at its prompt, this is the
    /// shell's own directory. While the shell runs a job in the
    /// foreground, it is the directory of the job's first process: an
    /// editor, or a shell started from the shell. `None` if that process
    /// has ended while the rest of its job runs on, if it belongs to
    /// another user, or if another job came to the front while this was
    /// reading. The other processes of a job are not asked: which of
    /// them stands for the job would be a guess.
    pub fn foreground_working_directory(&self) -> Option<PathBuf> {
        let master = self.master.as_raw_fd();
        let group = sys::foreground_group(master)?;
        let cwd = if group == self.pid {
            self.working_directory()?
        } else {
            sys::working_directory(group)?
        };
        // A process id is not given out again while a process group
        // still has it as its id. So if the group is still in front, the
        // answer came from the group's own first process.
        (sys::foreground_group(master) == Some(group)).then_some(cwd)
    }

    /// The program in front, the one that reads the keyboard, with the
    /// arguments it was started with.
    ///
    /// Like [`foreground_working_directory`](Self::foreground_working_directory),
    /// this asks about the first process of the foreground process
    /// group, and is `None` when that one does not tell. It is `None`
    /// too once the child has exited and nothing holds the terminal.
    /// The kernel is asked on each call.
    ///
    /// Right after the start, before the child has made the terminal its
    /// controlling terminal, the terminal has no foreground group. The
    /// child is the only process that can come to the front then, so it
    /// is told, with the arguments of whatever it runs at that moment.
    pub fn foreground_process(&self) -> Option<ForegroundProcess> {
        let master = self.master.as_raw_fd();
        let group = sys::foreground_group(master).unwrap_or(self.pid);
        let args = if group == self.pid {
            let mut args = None;
            // A reaped pid may belong to another process by now.
            self.slot
                .if_not_reaped(|| args = sys::process_args(self.pid));
            args?
        } else {
            sys::process_args(group)?
        };
        // See `foreground_working_directory`.
        let still = sys::foreground_group(master).unwrap_or(self.pid) == group;
        still.then_some(ForegroundProcess {
            pid: group as u32,
            args,
        })
    }

    /// The child's process id.
    pub fn pid(&self) -> u32 {
        self.pid as u32
    }
}

#[cfg(test)]
mod tests;

impl Drop for PtySession {
    fn drop(&mut self) {
        self.shutdown_inner();
    }
}

/// Writes to a session's terminal input. See [`PtySession::writer`].
pub struct PtyWriter {
    master: Arc<File>,
}

impl PtyWriter {
    /// Like [`PtySession::write`].
    pub fn write(&self, bytes: &[u8]) -> io::Result<()> {
        (&*self.master).write_all(bytes)
    }
}

/// The program, arguments, environment and directory of the child, as
/// C strings for posix_spawn.
struct ChildArgs {
    program: CString,
    argv: Vec<CString>,
    env: Vec<CString>,
    cwd: Option<CString>,
}

impl ChildArgs {
    fn new(config: &PtyConfig) -> io::Result<Self> {
        let (program, arg0) = match &config.program {
            Some(program) => (program.clone(), program.clone().into_os_string()),
            None => {
                // A login shell: argv[0] is the shell's name with a leading "-".
                let shell = login_shell_path();
                let name = shell.file_name().unwrap_or(shell.as_os_str());
                let mut arg0 = OsString::from("-");
                arg0.push(name);
                (shell, arg0)
            }
        };
        let argv = std::iter::once(arg0.as_os_str())
            .chain(config.args.iter().map(OsString::as_os_str))
            .map(c_string)
            .collect::<io::Result<_>>()?;

        let mut env = match &config.base_env {
            Some(base) => child_env(base.iter().cloned(), &config.env),
            None => child_env(env::vars_os(), &config.env),
        };
        if let Some(shell) = Shell::of(&program) {
            // Without the files the shell starts as it would elsewhere.
            if let Ok(dir) = shell_integration::default_dir()
                && shell_integration::write_files(&dir).is_ok()
            {
                shell_integration::integrate(shell, &mut env, &dir);
            }
        }
        let env = env
            .into_iter()
            .map(|(name, value)| {
                let mut entry = name;
                entry.push("=");
                entry.push(value);
                c_string(&entry)
            })
            .collect::<io::Result<_>>()?;

        let cwd = config
            .cwd
            .clone()
            .or_else(|| env::var_os("HOME").map(PathBuf::from))
            .map(|cwd| c_string(cwd.as_os_str()))
            .transpose()?;
        Ok(Self {
            program: c_string(program.as_os_str())?,
            argv,
            env,
            cwd,
        })
    }

    fn as_spawn_args(&self) -> sys::SpawnArgs<'_> {
        sys::SpawnArgs {
            program: &self.program,
            argv: &self.argv,
            env: &self.env,
            cwd: self.cwd.as_deref(),
        }
    }
}

fn c_string(s: &OsStr) -> io::Result<CString> {
    CString::new(s.as_bytes()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("contains a NUL byte: {s:?}"),
        )
    })
}

/// The user's shell: `$SHELL`, else the shell in the user's passwd entry,
/// else `/bin/zsh`.
pub fn login_shell_path() -> PathBuf {
    env::var_os("SHELL")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .or_else(sys::passwd_shell)
        .unwrap_or_else(|| PathBuf::from("/bin/zsh"))
}

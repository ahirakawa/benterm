//! Bounded subprocess execution with timeout, stdout limit, and process-group cleanup.

use std::ffi::OsStr;
use std::io::{self, Read};
#[cfg(unix)]
use std::os::fd::{AsRawFd, RawFd};
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use crate::GithubError;

/// Maximum stderr bytes kept for error messages; further bytes are drained
/// and discarded so the child cannot block on a full stderr pipe.
pub(crate) const STDERR_LIMIT: usize = 64 << 10;

/// How many 8 KiB chunks to read from a single pipe before returning to
/// `poll` and re-checking the deadline.
const CHUNKS_PER_POLL: usize = 8;

/// Failure when running a subprocess.
pub(crate) enum RunError {
    NotFound,
    TimedOut(Duration),
    OutputTooLarge(usize),
    Io(String),
}

impl RunError {
    pub(crate) fn into_github_error(self, program: &str) -> GithubError {
        match self {
            Self::NotFound if program == "gh" => GithubError::GhNotFound,
            Self::NotFound => GithubError::CommandFailed(format!("{program} could not be found")),
            Self::TimedOut(timeout) => GithubError::TimedOut(timeout),
            Self::OutputTooLarge(limit) => GithubError::OutputTooLarge(limit),
            Self::Io(msg) => GithubError::CommandFailed(msg),
        }
    }
}

/// Formats a subprocess's stderr into a concise single-line message.
pub(crate) fn said(program: &str, output: &Output) -> String {
    let text = String::from_utf8_lossy(&output.stderr);
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    match lines.as_slice() {
        [] => format!("{program} ended with {}", output.status),
        lines => lines.join(" "),
    }
}

#[cfg(unix)]
fn set_nonblocking(fd: RawFd) -> io::Result<()> {
    // SAFETY: `fd` is an open pipe file descriptor owned by this function's caller.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags == -1 {
        return Err(io::Error::last_os_error());
    }
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Stops `child` and every process in its process group, then reaps `child`
/// so no zombie or descendant process is left behind.
fn kill_and_reap(child: &mut Child) {
    #[cfg(unix)]
    {
        let pid = child.id() as libc::pid_t;
        if pid > 0 {
            // SAFETY: `pid` is the process group leader created by `setpgid(0, 0)`
            // in `pre_exec` and has not been waited on yet, so it cannot have been reused.
            unsafe {
                libc::killpg(pid, libc::SIGKILL);
            }
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// Runs `command`, enforcing `timeout` and `stdout_limit` while draining stderr.
///
/// The child is placed in its own process group on Unix so any descendant
/// processes are terminated together on timeout or output overflow.
pub(crate) fn run_bounded(
    program: &str,
    command: &mut Command,
    timeout: Duration,
    stdout_limit: usize,
) -> Result<Output, RunError> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    #[cfg(unix)]
    // SAFETY: `setpgid(0, 0)` is async-signal-safe and puts the subprocess and
    // any child processes it spawns into a dedicated process group so
    // `kill_and_reap` can terminate the entire tree cleanly.
    unsafe {
        command.pre_exec(|| {
            if libc::setpgid(0, 0) != 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }

    let mut child = command.spawn().map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            RunError::NotFound
        } else {
            RunError::Io(format!("{program} could not be run ({})", error.kind()))
        }
    })?;

    let mut stdout = child.stdout.take();
    let mut stderr = child.stderr.take();

    #[cfg(unix)]
    {
        if let Some(pipe) = &stdout
            && let Err(error) = set_nonblocking(pipe.as_raw_fd())
        {
            drop(stdout);
            drop(stderr);
            kill_and_reap(&mut child);
            return Err(RunError::Io(format!(
                "{program} output could not be read ({})",
                error.kind()
            )));
        }
        if let Some(pipe) = &stderr
            && let Err(error) = set_nonblocking(pipe.as_raw_fd())
        {
            drop(stdout);
            drop(stderr);
            kill_and_reap(&mut child);
            return Err(RunError::Io(format!(
                "{program} output could not be read ({})",
                error.kind()
            )));
        }
    }

    let deadline = Instant::now() + timeout;
    let mut stdout_buf = Vec::new();
    let mut stderr_buf = Vec::new();
    let mut chunk = [0u8; 8192];

    #[cfg(unix)]
    while stdout.is_some() || stderr.is_some() {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            drop(stdout);
            drop(stderr);
            kill_and_reap(&mut child);
            return Err(RunError::TimedOut(timeout));
        }

        let mut pollfds = [
            libc::pollfd {
                fd: -1,
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: -1,
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        let mut count = 0;
        let mut stdout_slot = None;
        let mut stderr_slot = None;
        if let Some(pipe) = &stdout {
            stdout_slot = Some(count);
            pollfds[count].fd = pipe.as_raw_fd();
            count += 1;
        }
        if let Some(pipe) = &stderr {
            stderr_slot = Some(count);
            pollfds[count].fd = pipe.as_raw_fd();
            count += 1;
        }

        let timeout_ms = remaining.as_millis().clamp(1, i32::MAX as u128) as libc::c_int;
        // SAFETY: `pollfds[..count]` holds valid open pipe file descriptors.
        let ready = unsafe { libc::poll(pollfds.as_mut_ptr(), count as libc::nfds_t, timeout_ms) };
        if ready < 0 {
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            drop(stdout);
            drop(stderr);
            kill_and_reap(&mut child);
            return Err(RunError::Io(format!(
                "{program} output could not be read ({})",
                err.kind()
            )));
        }
        if ready == 0 {
            continue;
        }

        if let Some(slot) = stdout_slot
            && pollfds[slot].revents != 0
        {
            let mut closed = pollfds[slot].revents & libc::POLLNVAL != 0;
            if !closed && let Some(pipe) = stdout.as_mut() {
                for _ in 0..CHUNKS_PER_POLL {
                    match pipe.read(&mut chunk) {
                        Ok(0) => {
                            closed = true;
                            break;
                        }
                        Ok(n) => {
                            if stdout_buf.len().saturating_add(n) > stdout_limit {
                                drop(stdout);
                                drop(stderr);
                                kill_and_reap(&mut child);
                                return Err(RunError::OutputTooLarge(stdout_limit));
                            }
                            stdout_buf.extend_from_slice(&chunk[..n]);
                        }
                        Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                        Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                        Err(_) => {
                            closed = true;
                            break;
                        }
                    }
                }
            }
            if closed {
                stdout = None;
            }
        }

        if let Some(slot) = stderr_slot
            && pollfds[slot].revents != 0
        {
            let mut closed = pollfds[slot].revents & libc::POLLNVAL != 0;
            if !closed && let Some(pipe) = stderr.as_mut() {
                for _ in 0..CHUNKS_PER_POLL {
                    match pipe.read(&mut chunk) {
                        Ok(0) => {
                            closed = true;
                            break;
                        }
                        Ok(n) => {
                            let space = STDERR_LIMIT.saturating_sub(stderr_buf.len());
                            if space > 0 {
                                stderr_buf.extend_from_slice(&chunk[..n.min(space)]);
                            }
                        }
                        Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                        Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                        Err(_) => {
                            closed = true;
                            break;
                        }
                    }
                }
            }
            if closed {
                stderr = None;
            }
        }
    }

    #[cfg(not(unix))]
    {
        if let Some(mut pipe) = stdout {
            let mut limited = pipe.take((stdout_limit as u64).saturating_add(1));
            let _ = limited.read_to_end(&mut stdout_buf);
            if stdout_buf.len() > stdout_limit {
                kill_and_reap(&mut child);
                return Err(RunError::OutputTooLarge(stdout_limit));
            }
        }
        if let Some(mut pipe) = stderr {
            let mut limited = pipe.take(STDERR_LIMIT as u64);
            let _ = limited.read_to_end(&mut stderr_buf);
        }
    }

    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                return Ok(Output {
                    status,
                    stdout: stdout_buf,
                    stderr: stderr_buf,
                });
            }
            Ok(None) => {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    kill_and_reap(&mut child);
                    return Err(RunError::TimedOut(timeout));
                }
                std::thread::sleep(remaining.min(Duration::from_millis(5)));
            }
            Err(error) => {
                kill_and_reap(&mut child);
                return Err(RunError::Io(format!(
                    "{program} could not be waited on ({})",
                    error.kind()
                )));
            }
        }
    }
}

/// Applies optional `PATH` and extra environment variables to a command.
pub(crate) fn apply_env(
    command: &mut Command,
    path: Option<&OsStr>,
    env: &[(std::ffi::OsString, std::ffi::OsString)],
) {
    if let Some(path) = path {
        command.env("PATH", path);
    }
    for (key, value) in env {
        command.env(key, value);
    }
}

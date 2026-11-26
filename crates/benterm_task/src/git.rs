//! git, run as a program.
//!
//! Every argument is handed over on its own, never through a shell, and
//! what is not an option comes after `--` or as a full ref name, so a
//! name is never taken for an option. The variables that would point git
//! at another repository than the directory it is run in are taken out.

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::io::{self, Read};
#[cfg(unix)]
use std::os::fd::{AsRawFd, RawFd};
#[cfg(unix)]
use std::os::unix::process::CommandExt;
#[cfg(target_os = "macos")]
use std::os::unix::{ffi::OsStringExt, fs::PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use crate::TaskError;
use crate::diff::{DIFF_OUTPUT_LIMIT, NEW_FILES_COUNT_LIMIT, NEW_FILES_TOTAL_LIMIT};
use crate::store::Task;

/// How long a git command is allowed to run before it is stopped.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// Maximum stdout bytes read from general (non-diff) git commands.
const DEFAULT_OUTPUT_LIMIT: usize = 4 << 20;

/// Maximum stderr bytes kept for error messages; further bytes are drained
/// and discarded so the child cannot block on a full stderr pipe.
const STDERR_LIMIT: usize = 64 << 10;

/// How many 8 KiB chunks to read from a single pipe before returning to
/// `poll` and re-checking the deadline.
const CHUNKS_PER_POLL: usize = 8;

/// The variables that tell git which repository it is in. A hook that
/// started the app, or a shell inside `git rebase -x`, has them set.
const REPOSITORY_VARIABLES: [&str; 7] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_COMMON_DIR",
    "GIT_OBJECT_DIRECTORY",
    "GIT_NAMESPACE",
    "GIT_PREFIX",
];

// The Git executable can re-exec itself for built-in subcommands. Every
// other execution, including the shell used for compound filter commands,
// is denied by the OS even if the repository's config changes mid-command.
#[cfg(target_os = "macos")]
const GIT_EXECUTION_PROFILE: &str = r#"
(version 1)
(allow default)
(deny process-exec)
(allow process-exec (literal (param "GIT_TOOL")))
"#;

// A link to the Git executable under another name, as `git-clean`, is the
// Git executable to the rule above, and runs as the built-in command of
// that name. A repository's config can name such a link as a filter, so a
// command that only reads may write no file and reach no network either,
// whichever built-in command it is made to run. What git prints goes
// through pipes that are open already.
#[cfg(target_os = "macos")]
const READ_ONLY_PROFILE: &str = r#"
(version 1)
(allow default)
(deny process-exec)
(allow process-exec (literal (param "GIT_TOOL")))
(deny file-write*)
(allow file-write* (literal "/dev/null"))
(deny network*)
"#;

/// What a git command may do to the files of the machine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Access {
    /// Nothing is written: `status`, `diff`, `ls-files` and the like.
    Read,
    /// The repository is changed: a worktree or a branch made or removed.
    Write,
}

#[cfg(target_os = "macos")]
fn sandbox_command(git: &Path, access: Access) -> Command {
    let mut setting = OsString::from("GIT_TOOL=");
    setting.push(git);
    let profile = match access {
        Access::Read => READ_ONLY_PROFILE,
        Access::Write => GIT_EXECUTION_PROFILE,
    };
    let mut command = Command::new("/usr/bin/sandbox-exec");
    command.arg("-D").arg(setting).args(["-p", profile]);
    command
}

/// The most bytes read of a `.git` file, which holds one path.
const GIT_FILE_LIMIT: u64 = 16 << 10;

/// The folder a `.git` that is a file names, as git wrote it:
/// `gitdir: <path>`. `None` for a link, a folder, or other content.
fn git_file_target(git_file: &Path) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::OpenOptionsExt;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(git_file)
        .ok()?;
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let mut content = Vec::new();
    file.take(GIT_FILE_LIMIT + 1)
        .read_to_end(&mut content)
        .ok()?;
    if content.len() as u64 > GIT_FILE_LIMIT {
        return None;
    }
    let path = content.strip_prefix(b"gitdir: ")?;
    let path = path.strip_suffix(b"\n").unwrap_or(path);
    if path.is_empty() || path.contains(&b'\n') || path.contains(&0) {
        return None;
    }
    // A path that is not absolute is from the folder the file is in.
    Some(git_file.parent()?.join(OsStr::from_bytes(path)))
}

/// How git is run.
#[derive(Clone, Debug)]
pub struct Git {
    /// The `PATH` git is looked up on. Without one, the app's own.
    pub path: Option<OsString>,
    /// Apply the macOS executable allowlist. Tests using shell fixtures can
    /// turn this off to exercise the bounded runner independently.
    pub restrict_programs: bool,
    /// How long each git command may run before it is stopped.
    pub timeout: Duration,
    /// Maximum bytes read from `git diff` and `git ls-files` before stopping.
    pub diff_output_limit: usize,
    /// Maximum number of untracked files read in a diff.
    pub new_files_count_limit: usize,
    /// Maximum total bytes read across all untracked files in a diff.
    pub new_files_total_limit: u64,
}

impl Default for Git {
    fn default() -> Self {
        Self {
            path: None,
            restrict_programs: true,
            timeout: DEFAULT_TIMEOUT,
            diff_output_limit: DIFF_OUTPUT_LIMIT,
            new_files_count_limit: NEW_FILES_COUNT_LIMIT,
            new_files_total_limit: NEW_FILES_TOTAL_LIMIT,
        }
    }
}

/// The checkout a task branches off from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Source {
    /// The top directory of the repository's main checkout. The
    /// checkout the task branches off from may be a worktree of it.
    pub repository: PathBuf,
    /// The branch the checkout is on, by its short name.
    pub branch: String,
    /// The commit the branch is at, by its full name.
    pub commit: String,
}

/// What removing a task's worktree and branch would lose.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AtRisk {
    /// Files of the worktree with changes that are in no commit, new
    /// files among them. Files that git ignores are not counted.
    pub uncommitted: usize,
    /// Commits that are on the task's branch and on no other branch,
    /// local or remote.
    pub commits: usize,
}

impl AtRisk {
    /// Everything: whatever would be lost is accepted.
    pub const ANYTHING: Self = Self {
        uncommitted: usize::MAX,
        commits: usize::MAX,
    };

    /// Whether nothing would be lost.
    pub fn is_nothing(&self) -> bool {
        *self == Self::default()
    }

    /// Whether `self` is no more than `accepted`.
    pub fn within(&self, accepted: &Self) -> bool {
        self.uncommitted <= accepted.uncommitted && self.commits <= accepted.commits
    }
}

impl fmt::Display for AtRisk {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let files = match self.uncommitted {
            0 => None,
            1 => Some("1 uncommitted file".to_owned()),
            n => Some(format!("{n} uncommitted files")),
        };
        let commits = match self.commits {
            0 => None,
            1 => Some("1 commit that is on no other branch".to_owned()),
            n => Some(format!("{n} commits that are on no other branch")),
        };
        match (files, commits) {
            (Some(files), Some(commits)) => write!(f, "{files} and {commits}"),
            (Some(one), None) | (None, Some(one)) => f.write_str(&one),
            (None, None) => f.write_str("nothing"),
        }
    }
}

/// What git said when it failed, on one line.
pub(crate) fn said(output: &Output) -> String {
    let text = String::from_utf8_lossy(&output.stderr);
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    match lines.as_slice() {
        [] => format!("git ended with {}", output.status),
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
///
/// Must only be called before `child` has been reaped by `wait` / `try_wait`.
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

impl Git {
    /// Lists tracked files and non-ignored untracked files, NUL-separated.
    pub fn list_worktree_files(&self, directory: &Path) -> Result<Vec<u8>, TaskError> {
        let output = self.run(
            directory,
            &args([
                "--no-optional-locks",
                "ls-files",
                "-z",
                "--cached",
                "--others",
                "--exclude-standard",
            ]),
        )?;
        if output.status.success() {
            Ok(output.stdout)
        } else {
            Err(TaskError::new(said(&output)))
        }
    }

    #[cfg(target_os = "macos")]
    fn native_git(&self) -> Result<PathBuf, TaskError> {
        let path = self
            .path
            .clone()
            .or_else(|| std::env::var_os("PATH"))
            .unwrap_or_else(|| "/usr/bin:/bin".into());
        let git = std::env::split_paths(&path)
            .map(|folder| folder.join("git"))
            .find(|candidate| {
                candidate.metadata().is_ok_and(|metadata| {
                    metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
                })
            })
            .and_then(|program| program.canonicalize().ok())
            .ok_or_else(|| TaskError::new("git was not found on PATH"))?;
        if git != Path::new("/usr/bin/git") {
            return Ok(git);
        }

        // The system git is a developer-tool launcher. Use the selected
        // binary directly so the sandbox needs only one executable.
        let mut xcrun = Command::new("/usr/bin/xcrun");
        xcrun.args(["--find", "git"]);
        let output = self.run_bounded(xcrun, 4096)?;
        if !output.status.success() {
            return Err(TaskError::new("the system git tool could not be located"));
        }
        let path = output.stdout.strip_suffix(b"\n").unwrap_or(&output.stdout);
        let path = PathBuf::from(OsString::from_vec(path.to_vec()));
        if !path.is_absolute() {
            return Err(TaskError::new(
                "the system git tool did not return an absolute path",
            ));
        }
        path.canonicalize().map_err(|error| {
            TaskError::new(format!(
                "the system git tool could not be resolved ({})",
                error.kind()
            ))
        })
    }

    #[cfg(target_os = "macos")]
    fn restricted_command(&self, access: Access) -> Result<Command, TaskError> {
        let git = self.native_git()?;
        let mut command = sandbox_command(&git, access);
        command.arg(git);
        Ok(command)
    }

    /// Checks, before git is run in the worktree of a task, that the
    /// worktree's `.git` is still the file git made: a plain file that
    /// names a folder right under `worktrees` in the repository's own
    /// git folder. A `.git` put in its place, as a repository whose
    /// files are elsewhere, would have git work for that one.
    pub fn check_worktree(&self, repository: &Path, worktree: &Path) -> Result<(), TaskError> {
        let refused = || {
            TaskError::new(format!(
                "the .git of {} does not lead to the worktrees of {}, so git is not run in it",
                worktree.display(),
                repository.display()
            ))
        };
        let git_folder = repository.join(".git");
        // The repository's own `.git` is a file when it is a submodule.
        let git_folder = if git_folder.is_dir() {
            git_folder
        } else {
            git_file_target(&git_folder).ok_or_else(refused)?
        };
        let worktrees = git_folder
            .join("worktrees")
            .canonicalize()
            .map_err(|_| refused())?;
        let named = git_file_target(&worktree.join(".git"))
            .and_then(|folder| folder.canonicalize().ok())
            .ok_or_else(refused)?;
        if named.parent() == Some(worktrees.as_path()) && named.is_dir() {
            Ok(())
        } else {
            Err(refused())
        }
    }

    /// Runs git in `directory` with the default stdout byte limit. git
    /// may write no file.
    pub(crate) fn run(&self, directory: &Path, arguments: &[&OsStr]) -> Result<Output, TaskError> {
        self.run_with_limit(directory, arguments, DEFAULT_OUTPUT_LIMIT)
    }

    /// Runs git in `directory`, stopping and reaping its process group if it
    /// exceeds `self.timeout` or writes more than `stdout_limit` bytes to stdout.
    /// git may write no file.
    pub(crate) fn run_with_limit(
        &self,
        directory: &Path,
        arguments: &[&OsStr],
        stdout_limit: usize,
    ) -> Result<Output, TaskError> {
        self.run_as(Access::Read, directory, arguments, stdout_limit)
    }

    fn run_as(
        &self,
        access: Access,
        directory: &Path,
        arguments: &[&OsStr],
        stdout_limit: usize,
    ) -> Result<Output, TaskError> {
        #[cfg(target_os = "macos")]
        let mut command = if self.restrict_programs {
            self.restricted_command(access)?
        } else {
            Command::new("git")
        };
        // Elsewhere there is no restriction to choose.
        #[cfg(not(target_os = "macos"))]
        let mut command = {
            let _ = access;
            Command::new("git")
        };
        command.arg("-C").arg(directory);
        if self.restrict_programs {
            command.args([
                "--no-pager",
                "-c",
                "core.fsmonitor=false",
                "-c",
                "core.hooksPath=/dev/null",
            ]);
        }
        command
            .args(arguments)
            // git asks nothing: there is nobody at its terminal.
            .env("GIT_TERMINAL_PROMPT", "0");
        for variable in REPOSITORY_VARIABLES {
            command.env_remove(variable);
        }
        if let Some(path) = &self.path {
            command.env("PATH", path);
        }
        self.run_bounded(command, stdout_limit)
    }

    fn run_bounded(&self, mut command: Command, stdout_limit: usize) -> Result<Output, TaskError> {
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(unix)]
        // SAFETY: `setpgid(0, 0)` is async-signal-safe and puts git and any
        // helpers or hooks it spawns into a dedicated process group so
        // `kill_and_reap` can terminate the entire tree cleanly.
        unsafe {
            command.pre_exec(|| {
                if libc::setpgid(0, 0) != 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = command
            .spawn()
            .map_err(|error| TaskError::new(format!("git could not be run ({})", error.kind())))?;
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
                return Err(TaskError::new(format!(
                    "git output could not be read ({})",
                    error.kind()
                )));
            }
            if let Some(pipe) = &stderr
                && let Err(error) = set_nonblocking(pipe.as_raw_fd())
            {
                drop(stdout);
                drop(stderr);
                kill_and_reap(&mut child);
                return Err(TaskError::new(format!(
                    "git output could not be read ({})",
                    error.kind()
                )));
            }
        }

        let deadline = Instant::now() + self.timeout;
        let mut stdout_buf = Vec::new();
        let mut stderr_buf = Vec::new();
        let mut chunk = [0u8; 8192];

        while stdout.is_some() || stderr.is_some() {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                drop(stdout);
                drop(stderr);
                kill_and_reap(&mut child);
                return Err(TaskError::timed_out(self.timeout));
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
            let ready =
                unsafe { libc::poll(pollfds.as_mut_ptr(), count as libc::nfds_t, timeout_ms) };
            if ready < 0 {
                let err = io::Error::last_os_error();
                if err.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                drop(stdout);
                drop(stderr);
                kill_and_reap(&mut child);
                return Err(TaskError::new(format!(
                    "git output could not be read ({})",
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
                                    return Err(TaskError::too_large(stdout_limit));
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

        // Both pipes reached EOF; wait for the child to exit within the remaining timeout.
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
                        return Err(TaskError::timed_out(self.timeout));
                    }
                    std::thread::sleep(remaining.min(Duration::from_millis(5)));
                }
                Err(error) => {
                    kill_and_reap(&mut child);
                    return Err(TaskError::new(format!(
                        "git could not be waited on ({})",
                        error.kind()
                    )));
                }
            }
        }
    }

    /// What git prints, without the line end, or what it said when it
    /// failed.
    fn text(&self, directory: &Path, arguments: &[&OsStr]) -> Result<String, TaskError> {
        let output = self.run(directory, arguments)?;
        if !output.status.success() {
            return Err(TaskError::new(said(&output)));
        }
        String::from_utf8(output.stdout)
            .map(|text| text.trim_end_matches('\n').to_owned())
            .map_err(|_| TaskError::new("git printed what is not UTF-8"))
    }

    /// Runs a git command that changes the repository, and gives what
    /// it said when it failed.
    fn change(&self, directory: &Path, arguments: &[&OsStr]) -> Result<(), TaskError> {
        let output = self.run_as(Access::Write, directory, arguments, DEFAULT_OUTPUT_LIMIT)?;
        if output.status.success() {
            Ok(())
        } else {
            Err(TaskError::new(said(&output)))
        }
    }

    /// Whether git succeeds.
    fn holds(&self, directory: &Path, arguments: &[&OsStr]) -> Result<bool, TaskError> {
        Ok(self.run(directory, arguments)?.status.success())
    }

    /// The top directory of the checkout `directory` is in.
    ///
    /// The checkout must be `directory` or a folder above it. The
    /// repository's `core.worktree` can name any folder as its checkout,
    /// as in an archive someone unpacked, and git would then work there.
    fn checkout(&self, directory: &Path) -> Result<PathBuf, TaskError> {
        let checkout = self
            .text(directory, &args(["rev-parse", "--show-toplevel"]))
            .map(PathBuf::from)
            .map_err(|error| {
                if error.is_timed_out() || error.is_too_large() {
                    return error;
                }
                TaskError::new(format!(
                    "{} is not in the checkout of a git repository",
                    directory.display()
                ))
            })?;
        // git names the checkout by the path without links.
        let real = |path: &Path| path.canonicalize().unwrap_or_else(|_| path.to_owned());
        if !real(directory).starts_with(real(&checkout)) {
            return Err(TaskError::new(format!(
                "the checkout git names for {} is {}, which is not a folder it is in (the \
                 repository's core.worktree may name it), so no task is made from it",
                directory.display(),
                checkout.display()
            )));
        }
        Ok(checkout)
    }

    /// The main checkout of the repository that the checkout at
    /// `checkout` is of: the first worktree git lists.
    fn main_checkout_of(&self, checkout: &Path) -> Result<PathBuf, TaskError> {
        let list = self.text(checkout, &args(["worktree", "list", "--porcelain", "-z"]))?;
        Ok(list
            .split('\0')
            .next()
            .and_then(|first| first.strip_prefix("worktree "))
            .map_or_else(|| checkout.to_path_buf(), PathBuf::from))
    }

    /// The top directory of the main checkout of the repository
    /// `directory` is in, also from one of its worktrees. It is what a
    /// task records as its repository, and what tells one repository
    /// from another when there is no remote to go by.
    pub fn main_checkout(&self, directory: &Path) -> Result<PathBuf, TaskError> {
        let checkout = self.checkout(directory)?;
        self.main_checkout_of(&checkout)
    }

    /// The checkout `directory` is in, and the branch and commit it is
    /// on.
    pub fn source(&self, directory: &Path) -> Result<Source, TaskError> {
        let checkout = self.checkout(directory)?;
        let repository = self.main_checkout_of(&checkout)?;
        let on = args(["symbolic-ref", "--quiet", "--short", "HEAD"]);
        let branch = self.text(&checkout, &on).map_err(|error| {
            if error.is_timed_out() || error.is_too_large() {
                return error;
            }
            TaskError::new(
                "the checkout is on no branch (a detached HEAD), so there is no branch to \
                 branch off from",
            )
        })?;
        let at = args(["rev-parse", "--verify", "--quiet", "HEAD^{commit}"]);
        let commit = self.text(&checkout, &at).map_err(|error| {
            if error.is_timed_out() || error.is_too_large() {
                return error;
            }
            TaskError::new(format!("the branch {branch} has no commit yet"))
        })?;
        Ok(Source {
            repository,
            branch,
            commit,
        })
    }

    /// Makes the branch `branch` at the commit of `source`, and a
    /// worktree on it at `path`. A branch or a path that is there
    /// already is never used.
    pub fn add_worktree(
        &self,
        source: &Source,
        branch: &str,
        path: &Path,
    ) -> Result<(), TaskError> {
        let repository = &source.repository;
        let name = ref_of(branch);
        // `refs/heads/HEAD` is a ref git can make and nothing can use.
        let word = !branch.is_empty() && !branch.starts_with('-') && branch != "HEAD";
        if !word || !self.holds(repository, &args(["check-ref-format", &name]))? {
            return Err(TaskError::new(format!(
                "{branch:?} cannot be the name of a branch"
            )));
        }
        if self.holds(
            repository,
            &args(["show-ref", "--verify", "--quiet", &name]),
        )? {
            return Err(TaskError::new(format!(
                "the branch {branch} already exists"
            )));
        }
        let not_made = |path: &Path, error: std::io::Error| {
            TaskError::new(format!(
                "{} could not be made ({})",
                path.display(),
                error.kind()
            ))
        };
        if let Some(parent) = path.parent() {
            private_dirs(true)
                .create(parent)
                .map_err(|error| not_made(parent, error))?;
        }
        // Made here and empty, the folder is this task's: whoever else
        // wants the same place finds it taken. git fills an empty folder.
        private_dirs(false)
            .create(path)
            .map_err(|error| match error.kind() {
                std::io::ErrorKind::AlreadyExists => {
                    TaskError::new(format!("{} already exists", path.display()))
                }
                _ => not_made(path, error),
            })?;
        let add = [
            OsStr::new("worktree"),
            OsStr::new("add"),
            OsStr::new("--no-track"),
            OsStr::new("-b"),
            OsStr::new(branch),
            OsStr::new("--"),
            path.as_os_str(),
            OsStr::new(&source.commit),
        ];
        let added = self.change(repository, &add);
        if added.is_err() {
            // git makes the branch before the worktree, so a worktree it
            // has at the folder says that the branch is this task's too,
            // as when a hook failed the checkout. Without one, a branch
            // of the name is somebody else's, made in between.
            match self.has_worktree(repository, path) {
                Ok(true) => {
                    let _ = self.take_back(repository, branch, path, &source.commit);
                }
                _ => {
                    let _ = std::fs::remove_dir(path);
                }
            }
        }
        added
    }

    /// Whether `path` is a worktree of the repository, as git has it.
    fn has_worktree(&self, repository: &Path, path: &Path) -> Result<bool, TaskError> {
        let list = self.text(repository, &args(["worktree", "list", "--porcelain", "-z"]))?;
        // git names a worktree by the path without links.
        let real = |path: &Path| path.canonicalize().unwrap_or_else(|_| path.to_owned());
        let wanted = real(path);
        Ok(list
            .split('\0')
            .filter_map(|line| line.strip_prefix("worktree "))
            .any(|listed| real(Path::new(listed)) == wanted))
    }

    /// Removes a worktree nothing has worked in and its branch, if the
    /// branch is still where it was made.
    pub(crate) fn take_back(
        &self,
        repository: &Path,
        branch: &str,
        path: &Path,
        commit: &str,
    ) -> Result<(), TaskError> {
        if path.symlink_metadata().is_ok() {
            // Not forced: a file that has appeared in it stops this.
            self.remove_worktree(repository, path, false)?;
        }
        let name = ref_of(branch);
        let at = self.text(
            repository,
            &args(["rev-parse", "--verify", "--quiet", &name]),
        );
        match at {
            Ok(at) if at == commit => self.delete_branch(repository, branch),
            Ok(_) => Err(TaskError::new(format!(
                "the branch {branch} has moved since it was made, so it is left"
            ))),
            Err(error) if error.is_timed_out() || error.is_too_large() => Err(error),
            // No such branch.
            Err(_) => Ok(()),
        }
    }

    fn remove_worktree(
        &self,
        repository: &Path,
        path: &Path,
        force: bool,
    ) -> Result<(), TaskError> {
        let mut remove = vec![OsStr::new("worktree"), OsStr::new("remove")];
        if force {
            remove.push(OsStr::new("--force"));
        }
        remove.extend([OsStr::new("--"), path.as_os_str()]);
        self.change(repository, &remove)
    }

    fn delete_branch(&self, repository: &Path, branch: &str) -> Result<(), TaskError> {
        self.change(repository, &args(["branch", "-D", "--", branch]))
    }

    /// What removing the worktree and the branch of `task` would lose.
    /// Nothing is written, not even git's index.
    pub fn at_risk(&self, task: &Task) -> Result<AtRisk, TaskError> {
        let name = ref_of(&task.branch);
        let exclude = format!("--exclude={}", task.branch);
        let branch = self.holds(
            &task.repository,
            &args(["show-ref", "--verify", "--quiet", &name]),
        )?;
        // What is on the branch, and what the worktree is on if it has
        // left the branch, less what another branch has too.
        let commits = |directory: &Path, head: bool| {
            let mut count = vec!["rev-list", "--count"];
            count.extend(head.then_some("HEAD"));
            count.extend(branch.then_some(name.as_str()));
            if count.len() == 2 {
                return Ok(0);
            }
            count.extend(["--not", &exclude, "--branches", "--remotes"]);
            let count: Vec<&OsStr> = count.into_iter().map(OsStr::new).collect();
            let counted = self.text(directory, &count).map_err(|error| {
                TaskError::new(format!(
                    "the commits of the branch {} could not be counted: {error}",
                    task.branch
                ))
            })?;
            counted
                .parse()
                .map_err(|_| TaskError::new(format!("git counted the commits as {counted:?}")))
        };
        // Removed already, by hand or by a try that got no further.
        if task.worktree.symlink_metadata().is_err() {
            return Ok(AtRisk {
                uncommitted: 0,
                commits: commits(&task.repository, false)?,
            });
        }
        if !self.has_worktree(&task.repository, &task.worktree)? {
            return Err(TaskError::new(format!(
                "{} is no worktree of the repository any more",
                task.worktree.display()
            )));
        }
        self.check_worktree(&task.repository, &task.worktree)?;
        let status = self.run(
            &task.worktree,
            &args([
                "--no-optional-locks",
                "status",
                "--porcelain=v1",
                "-z",
                "--untracked-files=all",
            ]),
        )?;
        if !status.status.success() {
            return Err(TaskError::new(format!(
                "the worktree {} could not be looked at: {}",
                task.worktree.display(),
                said(&status)
            )));
        }
        Ok(AtRisk {
            uncommitted: changed_files(&status.stdout),
            commits: commits(&task.worktree, true)?,
        })
    }

    /// Removes the worktree of `task`. With `force`, also when files in
    /// it have changes that are in no commit.
    pub(crate) fn remove_worktree_of(&self, task: &Task, force: bool) -> Result<(), TaskError> {
        // Gone already when an earlier try got this far.
        if task.worktree.symlink_metadata().is_err() {
            return Ok(());
        }
        self.remove_worktree(&task.repository, &task.worktree, force)
            .map_err(|error| TaskError::new(format!("the worktree was not removed: {error}")))
    }

    /// Deletes the branch of `task`, whatever is on it.
    pub(crate) fn delete_branch_of(&self, task: &Task) -> Result<(), TaskError> {
        let name = ref_of(&task.branch);
        let exists = args(["show-ref", "--verify", "--quiet", &name]);
        if !self.holds(&task.repository, &exists)? {
            return Ok(());
        }
        self.delete_branch(&task.repository, &task.branch)
            .map_err(|error| {
                TaskError::new(format!(
                    "the branch {} was not deleted: {error}",
                    task.branch
                ))
            })
    }
}

/// Arguments that are all text.
/// Makes directories for their owner alone (0700): the ones that hold a
/// worktree and the worktree itself, which others on the machine have
/// no reason to read. With `parents`, the missing directories above are
/// made too, and a directory that is there is left as it is.
pub(crate) fn private_dirs(parents: bool) -> std::fs::DirBuilder {
    use std::os::unix::fs::DirBuilderExt;
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(parents).mode(0o700);
    builder
}

fn args<const N: usize>(arguments: [&str; N]) -> [&OsStr; N] {
    arguments.map(OsStr::new)
}

fn ref_of(branch: &str) -> String {
    format!("refs/heads/{branch}")
}

/// How many files `git status --porcelain=v1 -z` names. An entry is two
/// letters, a space and a path, ended by a NUL; a renamed or copied file
/// has the path it came from as one more piece.
fn changed_files(status: &[u8]) -> usize {
    let mut pieces = status.split(|byte| *byte == 0).filter(|p| !p.is_empty());
    let mut files = 0;
    while let Some(entry) = pieces.next() {
        files += 1;
        let letters = entry.iter().take(2);
        if letters.into_iter().any(|l| matches!(l, b'R' | b'C')) {
            pieces.next();
        }
    }
    files
}

/// The name of the folder for the worktree of `branch`: the branch with
/// every run of other characters than letters, digits, `.` and `_` made
/// a `-`. A branch that leaves nothing gets a name from a number made of
/// its own name, so that two such branches get two folders.
pub fn folder_name(branch: &str) -> String {
    let mut name = String::new();
    for ch in branch.chars() {
        if ch.is_alphanumeric() || matches!(ch, '.' | '_') {
            name.push(ch);
        } else if !name.ends_with('-') {
            name.push('-');
        }
    }
    // Not hidden, and not `.` or `..`.
    let name = name.trim_matches('-').trim_start_matches('.');
    if !name.is_empty() {
        return name.to_owned();
    }
    // FNV-1a.
    let hash = branch.bytes().fold(0x811c_9dc5_u32, |hash, byte| {
        (hash ^ u32::from(byte)).wrapping_mul(0x0100_0193)
    });
    format!("branch-{hash:08x}")
}

/// Where the worktree of `branch` goes: `root/<repository>/<branch>`,
/// with the name of the repository's top directory.
pub fn worktree_path(root: &Path, repository: &Path, branch: &str) -> PathBuf {
    let repository = repository
        .file_name()
        .map_or_else(|| "repository".into(), |name| name.to_owned());
    root.join(repository).join(folder_name(branch))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn application_git_overrides_repository_execution_preferences() {
        let directory = tempfile::tempdir().unwrap();
        let git = Git::default();
        git.change(directory.path(), &args(["init", "--quiet"]))
            .unwrap();
        let config = directory.path().join(".git/config");
        use std::io::Write;
        writeln!(
            std::fs::OpenOptions::new()
                .append(true)
                .open(config)
                .unwrap(),
            "[core]\nfsmonitor = true\nhooksPath = fixtures"
        )
        .unwrap();

        for (key, expected) in [
            ("core.fsmonitor", b"false\n".as_slice()),
            ("core.hooksPath", b"/dev/null\n".as_slice()),
        ] {
            let output = git
                .run(directory.path(), &args(["config", "--get", key]))
                .unwrap();
            assert!(output.status.success(), "{}", said(&output));
            assert_eq!(output.stdout, expected, "{key}");
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn git_sandbox_allows_git_and_denies_an_unlisted_executable() {
        let git = Git::default();
        let git_path = git.native_git().unwrap();
        let mut allowed = sandbox_command(&git_path, Access::Write);
        allowed.arg(&git_path).arg("--version");
        let output = git.run_bounded(allowed, 4096).unwrap();
        assert!(output.status.success(), "{}", said(&output));

        let mut denied = sandbox_command(&git_path, Access::Write);
        denied.arg("/usr/bin/true");
        let output = git.run_bounded(denied, 4096).unwrap();
        assert!(!output.status.success());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_command_that_reads_can_write_no_file() {
        let directory = tempfile::tempdir().unwrap();
        let git = Git::default();
        let init = args(["init", "--quiet"]);
        let output = git.run(directory.path(), &init).unwrap();
        assert!(!output.status.success());
        assert!(!directory.path().join(".git").exists());

        git.change(directory.path(), &init).unwrap();
        assert!(directory.path().join(".git").is_dir());
    }

    #[test]
    fn only_the_git_file_of_a_worktree_of_the_repository_is_accepted() {
        let directory = tempfile::tempdir().unwrap();
        let base = directory.path().canonicalize().unwrap();
        let repository = base.join("repository");
        let linked = repository.join(".git/worktrees/task");
        let worktree = base.join("worktree");
        std::fs::create_dir_all(&linked).unwrap();
        std::fs::create_dir(&worktree).unwrap();
        let git = Git::default();
        let dot_git = worktree.join(".git");
        let check = || git.check_worktree(&repository, &worktree);

        // No `.git` at all.
        assert!(check().is_err());

        std::fs::write(&dot_git, format!("gitdir: {}\n", linked.display())).unwrap();
        check().unwrap();

        // The repository's git folder itself, and a folder under another one.
        let other = base.join("other/.git/worktrees/task");
        std::fs::create_dir_all(&other).unwrap();
        for target in [repository.join(".git"), other, linked.join("../../..")] {
            std::fs::write(&dot_git, format!("gitdir: {}\n", target.display())).unwrap();
            assert!(check().is_err(), "{}", target.display());
        }

        // A link to the file git made.
        let real = base.join("real");
        std::fs::write(&real, format!("gitdir: {}\n", linked.display())).unwrap();
        std::fs::remove_file(&dot_git).unwrap();
        std::os::unix::fs::symlink(&real, &dot_git).unwrap();
        assert!(check().is_err());

        // A repository of its own.
        std::fs::remove_file(&dot_git).unwrap();
        std::fs::create_dir(&dot_git).unwrap();
        assert!(check().is_err());
    }

    #[test]
    fn a_renamed_file_is_one_file() {
        assert_eq!(changed_files(b""), 0);
        assert_eq!(changed_files(b"?? a\0 M b c\0"), 2);
        assert_eq!(changed_files(b"R  new\0old\0?? a\0"), 2);
        assert_eq!(changed_files(b"MM a\0"), 1);
    }

    #[test]
    fn what_would_be_lost_is_said_in_words() {
        let risk = |uncommitted, commits| {
            AtRisk {
                uncommitted,
                commits,
            }
            .to_string()
        };
        assert_eq!(risk(1, 0), "1 uncommitted file");
        assert_eq!(
            risk(3, 2),
            "3 uncommitted files and 2 commits that are on no other branch"
        );
        assert_eq!(risk(0, 1), "1 commit that is on no other branch");
    }
}

//! Tasks own a branch and worktree. The app tracks which workspace shows a task.
//! Git receives each argument separately. Forced operations require the caller
//! to accept the potential loss described by [`AtRisk`].

mod diff;
mod git;
mod issue;
mod local_issue_store;
mod local_issues;
mod store;

pub use crate::diff::{
    DIFF_OUTPUT_LIMIT, Diff, DiffLine, FileDiff, FileStatus, Hunk, LineKind, NEW_FILE_LIMIT,
    NEW_FILES_COUNT_LIMIT, NEW_FILES_TOTAL_LIMIT,
};
pub use crate::git::{AtRisk, DEFAULT_TIMEOUT, Git, Source, folder_name, worktree_path};
pub use crate::issue::{
    IssueTasks, RepoId, default_issue_branch, issue_branch_for, issue_number_in_branch,
    tasks_by_issue, tasks_for_issue,
};
pub use crate::local_issue_store::{
    LocalIssueChange, LocalIssuesChanged, NewLocalIssue, change_local_issues,
};
pub use crate::local_issues::{
    LOCAL_ISSUES_DIRECTORY, LOCAL_ISSUES_FILE, LOCAL_ISSUES_LIMIT, LOCAL_ISSUES_VERSION,
    LocalIssue, LocalIssueState, LocalIssues, LocalIssuesError, local_issues_path,
    parse_local_issues, read_local_issues,
};
pub use crate::store::{
    Closed, PullRequestLink, Recorded, Task, TaskStore, Worktree, tasks_path_beside,
};

use std::fmt;
use std::path::{Path, PathBuf};

/// What kind of failure a [`TaskError`] represents.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TaskErrorKind {
    #[default]
    Other,
    /// A git command did not finish within its timeout and was stopped.
    TimedOut,
    /// A git command's output exceeded its byte limit.
    TooLarge,
}

/// Why something was not done, as a sentence without a full stop.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskError {
    message: String,
    kind: TaskErrorKind,
}

impl TaskError {
    pub fn new(why: impl Into<String>) -> Self {
        Self {
            message: why.into(),
            kind: TaskErrorKind::Other,
        }
    }

    pub(crate) fn timed_out(timeout: std::time::Duration) -> Self {
        Self {
            message: format!("git timed out after {}", format_duration(timeout)),
            kind: TaskErrorKind::TimedOut,
        }
    }

    pub(crate) fn too_large(limit: usize) -> Self {
        Self {
            message: format!("git output is too large (over {})", format_bytes(limit)),
            kind: TaskErrorKind::TooLarge,
        }
    }

    pub fn kind(&self) -> TaskErrorKind {
        self.kind
    }

    pub fn is_timed_out(&self) -> bool {
        self.kind == TaskErrorKind::TimedOut
    }

    pub fn is_too_large(&self) -> bool {
        self.kind == TaskErrorKind::TooLarge
    }
}

fn format_duration(duration: std::time::Duration) -> String {
    if duration.subsec_nanos() == 0 {
        format!("{}s", duration.as_secs())
    } else {
        format!("{}ms", duration.as_millis())
    }
}

fn format_bytes(bytes: usize) -> String {
    const MIB: usize = 1 << 20;
    const KIB: usize = 1 << 10;
    if bytes >= MIB && bytes.is_multiple_of(MIB) {
        format!("{} MiB", bytes / MIB)
    } else if bytes >= KIB && bytes.is_multiple_of(KIB) {
        format!("{} KiB", bytes / KIB)
    } else {
        format!("{bytes} bytes")
    }
}

impl fmt::Display for TaskError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for TaskError {}

/// Why a task was not closed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CloseError {
    /// Removing the worktree and the branch would lose more than was
    /// accepted. Nothing was removed.
    AtRisk(AtRisk),
    Failed(TaskError),
}

impl fmt::Display for CloseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AtRisk(risk) => write!(f, "removing it would lose {risk}"),
            Self::Failed(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for CloseError {}

impl From<TaskError> for CloseError {
    fn from(error: TaskError) -> Self {
        Self::Failed(error)
    }
}

/// A task that was closed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Closing {
    pub task: Task,
    /// Why the branch is still there, when the worktree was removed and
    /// git refused to delete the branch.
    pub branch_left: Option<TaskError>,
}

/// What a task is made from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewTask {
    /// A directory of the checkout the task branches off from: the main
    /// checkout of a repository or one of its worktrees.
    pub directory: PathBuf,
    pub name: String,
    /// The branch to make. Without one it is named as the task is.
    pub branch: Option<String>,
    /// The role of the agent the task is given to.
    pub role: Option<String>,
    /// The directory the worktrees are put under.
    pub root: PathBuf,
}

/// Makes `root`, the directory the worktrees go under, and the folders
/// of the repositories in it the owner's alone (0700), as [`create`]
/// makes them now. The app does this when it starts, for the folders an
/// earlier version made readable by others (0755).
///
/// Only directories of the user's own are changed, and no link is
/// followed. The worktrees themselves are left as they are: what they
/// hold is the business of whoever works in them. Nothing is made, and
/// a folder that cannot be changed is passed over.
pub fn make_worktree_root_private(root: &Path) {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    // SAFETY: getuid has no preconditions and cannot fail.
    let user = unsafe { libc::getuid() };
    let tighten = |path: &Path| {
        let Ok(meta) = path.symlink_metadata() else {
            return false;
        };
        if !meta.is_dir() || meta.uid() != user {
            return false;
        }
        let mode = meta.mode() & 0o7777;
        if mode & 0o077 != 0 {
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode & !0o077));
        }
        true
    };
    if !tighten(root) {
        return;
    }
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        tighten(&entry.path());
    }
}

/// Makes the branch and the worktree of a task and writes the task down.
/// When any of it cannot be done, nothing is left behind.
pub fn create(git: &Git, store: &TaskStore, new: NewTask) -> Result<Task, TaskError> {
    let name = new.name.trim();
    if name.is_empty() {
        return Err(TaskError::new("a task needs a name"));
    }
    let branch = match new.branch.as_deref().map(str::trim) {
        Some(branch) if !branch.is_empty() => branch,
        _ => name,
    };
    if !new.root.is_absolute() {
        return Err(TaskError::new(format!(
            "the directory for worktrees must be an absolute path: {}",
            new.root.display()
        )));
    }
    let source = git.source(&new.directory)?;
    // A file that cannot be read stops the task before git makes anything.
    store.tasks()?;
    // Looked at without links, so that a link into the repository is
    // not taken for a place outside it.
    git::private_dirs(true).create(&new.root).map_err(|error| {
        TaskError::new(format!(
            "the directory for worktrees could not be made ({}): {}",
            error.kind(),
            new.root.display()
        ))
    })?;
    let real = |path: &Path| path.canonicalize().unwrap_or_else(|_| path.to_owned());
    let root = real(&new.root);
    let checkouts = [&source.repository, &new.directory];
    if checkouts
        .iter()
        .any(|inside| root.starts_with(real(inside)))
    {
        return Err(TaskError::new(format!(
            "the directory for worktrees is inside the repository: {}",
            new.root.display()
        )));
    }
    let worktree = worktree_path(&root, &source.repository, branch);
    // The tasks file and the session file hold paths as text.
    if worktree.to_str().is_none() || source.repository.to_str().is_none() {
        return Err(TaskError::new(
            "the path of the repository or of the worktree is not UTF-8",
        ));
    }
    git.add_worktree(&source, branch, &worktree)?;
    let recorded = Recorded {
        name: name.to_owned(),
        repository: source.repository.clone(),
        base_branch: source.branch.clone(),
        base_commit: source.commit.clone(),
        branch: branch.to_owned(),
        worktree: worktree.clone(),
        role: new.role,
    };
    store.add(recorded).inspect_err(|_| {
        let _ = git.take_back(&source.repository, branch, &worktree, &source.commit);
    })
}

/// Takes back a task that [`create`] has just made and nothing has
/// worked in: its worktree, its branch and its record. A worktree with a
/// file that git does not know, or a branch that has moved, is left.
pub fn undo(git: &Git, store: &TaskStore, task: &Task) -> Result<(), TaskError> {
    git.take_back(
        &task.repository,
        &task.branch,
        &task.worktree,
        &task.base_commit,
    )?;
    store.take_out(task.id)
}

/// What removing the worktree and the branch of the task `id` would
/// lose, if they can be removed and that is no more than `accepted`.
pub fn check_removal(
    git: &Git,
    store: &TaskStore,
    id: u64,
    accepted: AtRisk,
) -> Result<AtRisk, CloseError> {
    let tasks = store.tasks()?;
    let task = tasks
        .iter()
        .find(|task| task.id == id)
        .ok_or_else(|| store::no_task(id))?;
    let removed = |task: &Task| {
        task.closed
            .as_ref()
            .is_some_and(|closed| closed.worktree == Worktree::Removed)
    };
    if removed(task) {
        return Err(TaskError::new(format!(
            "the worktree and the branch of the task {id} are removed already"
        ))
        .into());
    }
    // A task made later at the place of one whose worktree was removed
    // by hand has what is there now.
    let same = |other: &&Task| {
        other.id != id
            && !removed(other)
            && other.repository == task.repository
            && (other.worktree == task.worktree || other.branch == task.branch)
    };
    if let Some(other) = tasks.iter().find(same) {
        return Err(TaskError::new(format!(
            "the task {} has the same worktree or branch now, so they are left",
            other.id
        ))
        .into());
    }
    let risk = git.at_risk(task)?;
    if !risk.within(&accepted) {
        return Err(CloseError::AtRisk(risk));
    }
    Ok(risk)
}

/// Closes the task `id`. With `remove`, its worktree and its branch are
/// removed too, unless more would be lost than `remove` says was
/// accepted. A task closed with its worktree kept can be closed again to
/// remove it.
pub fn close(
    git: &Git,
    store: &TaskStore,
    id: u64,
    remove: Option<AtRisk>,
) -> Result<Closing, CloseError> {
    let task = store.task(id)?.ok_or_else(|| store::no_task(id))?;
    let left = task.closed.as_ref().map(|closed| closed.worktree);
    let Some(accepted) = remove else {
        return match left {
            None => Ok(Closing {
                task: store.set_closed(id, store::closed_now(Worktree::Kept))?,
                branch_left: None,
            }),
            Some(_) => Err(TaskError::new(format!("the task {id} is closed already")).into()),
        };
    };
    let risk = check_removal(git, store, id, accepted)?;
    // Forced only for the files that were accepted: a file that has
    // appeared since still stops git.
    git.remove_worktree_of(&task, risk.uncommitted > 0)?;
    // The worktree is gone, so the task is closed whatever becomes of
    // the branch: a restart must not start a shell where nothing is.
    let task = store.set_closed(id, store::closed_now(Worktree::Removed))?;
    let branch_left = git.delete_branch_of(&task).err();
    Ok(Closing { task, branch_left })
}

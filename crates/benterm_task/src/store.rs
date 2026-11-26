//! The tasks file: `tasks.json` beside the session file.
//!
//! It is JSON with a version number. The tasks outlive the session:
//! closing the last workspace removes the session file, and the
//! worktrees are still there. So the tasks have a file of their own, and
//! the session file only says which workspace shows which task.
//!
//! Every change takes a lock beside the file, reads the file, changes
//! what it read and replaces the file as a whole. Two apps that run at
//! once so never lose each other's tasks, and a number is never given
//! twice. Fields this build does not know are written back as they were
//! read, so a file that a newer build added a field to is still good
//! for it after this build has changed it. A file with a newer version
//! is not changed at all.

use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::TaskError;

/// What became of the worktree and the branch of a closed task.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Worktree {
    Kept,
    Removed,
}

/// When and how a task was closed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Closed {
    /// Milliseconds since 1970.
    pub at_ms: u64,
    /// The branch is gone with the worktree, unless git refused to
    /// delete it.
    pub worktree: Worktree,
    /// What a newer build wrote, kept as it is.
    #[serde(flatten)]
    other: Map<String, Value>,
}

impl Closed {
    pub fn new(at_ms: u64, worktree: Worktree) -> Self {
        Self {
            at_ms,
            worktree,
            other: Map::new(),
        }
    }
}

/// A task, as the tasks file has it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Task {
    /// Never given to another task of the same file.
    pub id: u64,
    pub name: String,
    /// The top directory of the main checkout of the repository the
    /// task belongs to, also when the task was made in one of its
    /// worktrees.
    pub repository: PathBuf,
    /// The branch the task branched off from, by its short name.
    pub base_branch: String,
    /// The commit the task's branch started at, by its full name. The
    /// base branch may have moved on since.
    pub base_commit: String,
    /// The task's own branch, by its short name.
    pub branch: String,
    /// The directory of the task's worktree.
    pub worktree: PathBuf,
    /// The role of the agent the task was given to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// Milliseconds since 1970.
    pub created_at_ms: u64,
    /// Not there while the task is open.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closed: Option<Closed>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pull_request: Option<PullRequestLink>,
    /// What a newer build wrote, kept as it is.
    #[serde(flatten)]
    pub(crate) other: Map<String, Value>,
}

/// The GitHub pull request identified for a task.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequestLink {
    pub repository: String,
    pub number: u64,
}

/// A task before it has a number.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Recorded {
    pub name: String,
    pub repository: PathBuf,
    pub base_branch: String,
    pub base_commit: String,
    pub branch: String,
    pub worktree: PathBuf,
    pub role: Option<String>,
}

/// The version this build writes, and the only one it reads.
const VERSION: u64 = 1;

#[derive(Clone, Debug, Serialize, Deserialize)]
struct TasksFile {
    version: u64,
    /// The number of the next task.
    #[serde(default)]
    next_id: u64,
    #[serde(default)]
    tasks: Vec<Task>,
    #[serde(flatten)]
    other: Map<String, Value>,
}

impl Default for TasksFile {
    fn default() -> Self {
        Self {
            version: VERSION,
            next_id: 1,
            tasks: Vec::new(),
            other: Map::new(),
        }
    }
}

impl TasksFile {
    /// The number for a new task: never one a task of the file has or
    /// had.
    fn take_id(&mut self) -> Result<u64, TaskError> {
        let used = self.tasks.iter().map(|task| task.id).max().unwrap_or(0);
        let id = used.checked_add(1).map(|next| next.max(self.next_id));
        let after = id.and_then(|id| id.checked_add(1));
        let (Some(id), Some(after)) = (id, after) else {
            return Err(TaskError::new("there are no numbers left for tasks"));
        };
        self.next_id = after;
        Ok(id)
    }
}

/// The tasks file, or tasks that are kept only as long as the store.
#[derive(Debug)]
pub struct TaskStore {
    path: Option<PathBuf>,
    /// The tasks of a store without a file.
    memory: Mutex<TasksFile>,
}

/// `tasks.json` beside the session file at `session`.
pub fn tasks_path_beside(session: &Path) -> PathBuf {
    session.with_file_name("tasks.json")
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_millis() as u64)
}

/// The file's name, for a message: not the directories above it.
fn name_of(path: &Path) -> String {
    path.file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}

fn read(path: &Path) -> Result<TasksFile, TaskError> {
    let name = name_of(path);
    let text = match fs::read(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(TasksFile::default());
        }
        Err(error) => {
            return Err(TaskError::new(format!(
                "the tasks file {name} could not be read ({})",
                error.kind()
            )));
        }
    };
    let damaged = || {
        TaskError::new(format!(
            "the tasks file {name} is damaged, so it is left as it is"
        ))
    };
    let value: Value = serde_json::from_slice(&text).map_err(|_| damaged())?;
    match value.get("version").and_then(Value::as_u64) {
        Some(VERSION) => {}
        Some(version) if version > VERSION => {
            return Err(TaskError::new(format!(
                "the tasks file {name} is from a newer Benterm (version {version}), so it is left \
                 as it is"
            )));
        }
        _ => return Err(damaged()),
    }
    serde_json::from_value(value).map_err(|_| damaged())
}

fn write(path: &Path, file: &TasksFile) -> std::io::Result<()> {
    let mut text = serde_json::to_string_pretty(file).map_err(std::io::Error::other)?;
    text.push('\n');
    let part = path.with_extension("json.part");
    let mut out = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&part)?;
    out.write_all(text.as_bytes())?;
    out.sync_all()?;
    fs::rename(&part, path)?;
    // The new name of the file is in its directory.
    match path.parent() {
        Some(directory) => File::open(directory)?.sync_all(),
        None => Ok(()),
    }
}

impl TaskStore {
    /// The store of the tasks file at `path`. Nothing is read or
    /// written until the store is asked. Without a path the tasks are
    /// kept in the store alone.
    pub fn open(path: Option<&Path>) -> Self {
        Self {
            path: path.map(Path::to_owned),
            memory: Mutex::default(),
        }
    }

    /// All tasks, closed ones too, oldest first.
    pub fn tasks(&self) -> Result<Vec<Task>, TaskError> {
        match &self.path {
            Some(path) => Ok(read(path)?.tasks),
            None => Ok(self.memory.lock().expect("no holder panics").tasks.clone()),
        }
    }

    pub fn task(&self, id: u64) -> Result<Option<Task>, TaskError> {
        Ok(self.tasks()?.into_iter().find(|task| task.id == id))
    }

    /// Changes the file under its lock, or changes nothing when `change`
    /// refuses.
    fn update<T>(
        &self,
        change: impl FnOnce(&mut TasksFile) -> Result<T, TaskError>,
    ) -> Result<T, TaskError> {
        let Some(path) = &self.path else {
            return change(&mut self.memory.lock().expect("no holder panics"));
        };
        let name = name_of(path);
        let failed = |error: std::io::Error| {
            TaskError::new(format!(
                "the tasks file {name} could not be written ({})",
                error.kind()
            ))
        };
        let lock = (|| {
            let directory = path.parent().ok_or(std::io::ErrorKind::InvalidInput)?;
            DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(directory)?;
            let lock: File = OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(false)
                .mode(0o600)
                .open(path.with_extension("lock"))?;
            // Held for a read and a write of a small file.
            lock.lock()?;
            Ok(lock)
        })()
        .map_err(failed)?;
        let mut file = read(path)?;
        let result = change(&mut file)?;
        write(path, &file).map_err(failed)?;
        drop(lock);
        Ok(result)
    }

    /// Writes a task down and gives it its number.
    pub fn add(&self, recorded: Recorded) -> Result<Task, TaskError> {
        self.update(|file| {
            let task = Task {
                id: file.take_id()?,
                name: recorded.name,
                repository: recorded.repository,
                base_branch: recorded.base_branch,
                base_commit: recorded.base_commit,
                branch: recorded.branch,
                worktree: recorded.worktree,
                role: recorded.role,
                created_at_ms: now_ms(),
                closed: None,
                pull_request: None,
                other: Map::new(),
            };
            file.tasks.push(task.clone());
            Ok(task)
        })
    }

    /// Records the first PR found for this task and GitHub repository.
    pub fn link_pull_request(
        &self,
        id: u64,
        link: PullRequestLink,
    ) -> Result<PullRequestLink, TaskError> {
        self.update(|file| {
            let task = file
                .tasks
                .iter_mut()
                .find(|task| task.id == id)
                .ok_or_else(|| no_task(id))?;
            if let Some(existing) = &task.pull_request
                && existing.repository == link.repository
            {
                return Ok(existing.clone());
            }
            task.pull_request = Some(link.clone());
            Ok(link)
        })
    }

    pub fn set_closed(&self, id: u64, closed: Closed) -> Result<Task, TaskError> {
        self.update(|file| {
            let task = file
                .tasks
                .iter_mut()
                .find(|task| task.id == id)
                .ok_or_else(|| no_task(id))?;
            // What a newer build wrote about how it was closed stays.
            let other = task.closed.take().map(|before| before.other);
            task.closed = Some(Closed {
                other: other.unwrap_or_default(),
                ..closed
            });
            Ok(task.clone())
        })
    }

    /// Takes the record of the task `id` out, as if it had never been
    /// made. Its number is not given again.
    pub fn take_out(&self, id: u64) -> Result<(), TaskError> {
        self.update(|file| {
            file.tasks.retain(|task| task.id != id);
            Ok(())
        })
    }
}

pub(crate) fn no_task(id: u64) -> TaskError {
    TaskError::new(format!("there is no task {id}"))
}

pub(crate) fn closed_now(worktree: Worktree) -> Closed {
    Closed::new(now_ms(), worktree)
}

//! The rows of the workspace list in the sidebar, without a window.
//!
//! A workspace that shows a task is put under the workspace of the
//! repository the task was made from, moved in one step. A repository
//! with tasks and no workspace of its own gets a heading with its name,
//! and its tasks go under that. A group can be folded; the rows under a
//! folded group are not shown, except the selected one.
//!
//! A workspace is taken for a repository's when it does not show a task
//! and its directory is the repository or a directory in it. When it is
//! in two repositories, one inside the other, it is the inner one's. The
//! first such workspace from the top gets the tasks.
//!
//! The workspaces that show no task keep their order, and are drawn as
//! they were before tasks.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use benterm_layout::WorkspaceId;

/// A workspace as the rows are made from it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SidebarWorkspace {
    pub id: WorkspaceId,
    /// The task the workspace shows, if it shows an open one.
    pub task: Option<TaskPlace>,
    /// Where the workspace is, for one that shows no task: the directory
    /// of the shell of its focused pane. `None` when that is not known.
    pub directory: Option<PathBuf>,
}

/// Where a task was made from, and its branch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskPlace {
    /// The main checkout of the repository the task branches off from.
    pub repository: PathBuf,
    pub branch: String,
}

/// The tasks of a repository, under its workspace or its heading.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Group {
    pub repository: PathBuf,
    /// How many workspaces of tasks are in it.
    pub tasks: usize,
    pub folded: bool,
}

impl Group {
    /// The name of the repository: the last part of its path.
    pub fn name(&self) -> String {
        repository_name(&self.repository)
    }
}

/// The last part of the path of a repository, or the whole path if it
/// has none.
pub fn repository_name(repository: &Path) -> String {
    repository.file_name().map_or_else(
        || repository.to_string_lossy().into_owned(),
        |name| name.to_string_lossy().into_owned(),
    )
}

/// A row of the workspace list, from top to bottom.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SidebarRow {
    /// A workspace. One of a task is moved in one step and says its
    /// branch; one of a repository with tasks under it has the group.
    Workspace {
        id: WorkspaceId,
        /// How many steps the row is moved in: 0 or 1.
        depth: usize,
        /// The branch of the task the workspace shows.
        branch: Option<String>,
        /// The tasks under the workspace, for the workspace of their
        /// repository.
        group: Option<Group>,
    },
    /// The name of a repository whose tasks have no workspace of the
    /// repository to go under.
    Heading(Group),
}

impl SidebarRow {
    /// The workspace of the row, for a row of a workspace.
    pub fn workspace(&self) -> Option<WorkspaceId> {
        match self {
            Self::Workspace { id, .. } => Some(*id),
            Self::Heading(_) => None,
        }
    }
}

/// The rows of `workspaces`, which are in the order of the list.
/// `folded` are the repositories whose groups are folded; the
/// `selected` workspace has a row even in a folded group.
pub fn sidebar_rows(
    workspaces: &[SidebarWorkspace],
    folded: &BTreeSet<PathBuf>,
    selected: WorkspaceId,
) -> Vec<SidebarRow> {
    // The repositories with tasks.
    let repositories: BTreeSet<&Path> = workspaces
        .iter()
        .filter_map(|workspace| Some(workspace.task.as_ref()?.repository.as_path()))
        .collect();
    // The repository a workspace without a task is in: the innermost.
    let home = |workspace: &SidebarWorkspace| {
        let directory = workspace.directory.as_deref()?;
        repositories
            .iter()
            .filter(|repository| directory.starts_with(repository))
            .max_by_key(|repository| repository.components().count())
            .copied()
    };
    let mut owners: Vec<(&Path, WorkspaceId)> = Vec::new();
    for workspace in workspaces.iter().filter(|w| w.task.is_none()) {
        if let Some(repository) = home(workspace)
            && !owners.iter().any(|(owned, _)| *owned == repository)
        {
            owners.push((repository, workspace.id));
        }
    }
    let owner_of = |repository: &Path| {
        owners
            .iter()
            .find(|(owned, _)| *owned == repository)
            .map(|(_, id)| *id)
    };
    // The group of `repository` and the rows under it.
    let group = |repository: &Path| {
        let tasks: Vec<(WorkspaceId, &str)> = workspaces
            .iter()
            .filter_map(|workspace| {
                let task = workspace.task.as_ref()?;
                (task.repository == repository).then_some((workspace.id, task.branch.as_str()))
            })
            .collect();
        let is_folded = folded.contains(repository);
        let rows = tasks
            .iter()
            .filter(|(id, _)| !is_folded || *id == selected)
            .map(|(id, branch)| SidebarRow::Workspace {
                id: *id,
                depth: 1,
                branch: Some((*branch).to_owned()),
                group: None,
            })
            .collect::<Vec<_>>();
        let group = Group {
            repository: repository.to_owned(),
            tasks: tasks.len(),
            folded: is_folded,
        };
        (group, rows)
    };
    let mut rows = Vec::new();
    let mut headed: BTreeSet<&Path> = BTreeSet::new();
    for workspace in workspaces {
        match &workspace.task {
            None => {
                let owned = owners
                    .iter()
                    .find(|(_, id)| *id == workspace.id)
                    .map(|(repository, _)| group(repository));
                let (group, under) = owned.unzip();
                rows.push(SidebarRow::Workspace {
                    id: workspace.id,
                    depth: 0,
                    branch: None,
                    group,
                });
                rows.extend(under.into_iter().flatten());
            }
            Some(task) => {
                let repository = task.repository.as_path();
                if owner_of(repository).is_some() || !headed.insert(repository) {
                    continue;
                }
                let (group, under) = group(repository);
                rows.push(SidebarRow::Heading(group));
                rows.extend(under);
            }
        }
    }
    rows
}

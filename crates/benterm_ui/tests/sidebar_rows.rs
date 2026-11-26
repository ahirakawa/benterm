//! The rows of the workspace list, without a window: the workspaces of
//! tasks under the workspace of their repository or under its name, and
//! the other workspaces as they were.

use std::collections::BTreeSet;
use std::path::PathBuf;

use benterm_layout::{WorkspaceId, WorkspaceList};
use benterm_ui::sidebar_rows::{Group, SidebarRow, SidebarWorkspace, TaskPlace, sidebar_rows};
use proptest::prelude::*;

/// `count` ids no other workspace has, in rising order.
fn ids(count: usize) -> Vec<WorkspaceId> {
    let mut list = WorkspaceList::new(None, ());
    for _ in 1..count {
        list.add(None, ());
    }
    list.ids()
}

fn plain(id: WorkspaceId, directory: Option<&str>) -> SidebarWorkspace {
    SidebarWorkspace {
        id,
        task: None,
        directory: directory.map(PathBuf::from),
    }
}

fn task(id: WorkspaceId, repository: &str, branch: &str) -> SidebarWorkspace {
    SidebarWorkspace {
        id,
        task: Some(TaskPlace {
            repository: PathBuf::from(repository),
            branch: branch.to_owned(),
        }),
        directory: None,
    }
}

fn top(id: WorkspaceId) -> SidebarRow {
    SidebarRow::Workspace {
        id,
        depth: 0,
        branch: None,
        group: None,
    }
}

fn owner(id: WorkspaceId, repository: &str, tasks: usize, folded: bool) -> SidebarRow {
    SidebarRow::Workspace {
        id,
        depth: 0,
        branch: None,
        group: Some(Group {
            repository: PathBuf::from(repository),
            tasks,
            folded,
        }),
    }
}

fn under(id: WorkspaceId, branch: &str) -> SidebarRow {
    SidebarRow::Workspace {
        id,
        depth: 1,
        branch: Some(branch.to_owned()),
        group: None,
    }
}

fn heading(repository: &str, tasks: usize, folded: bool) -> SidebarRow {
    SidebarRow::Heading(Group {
        repository: PathBuf::from(repository),
        tasks,
        folded,
    })
}

fn none() -> BTreeSet<PathBuf> {
    BTreeSet::new()
}

#[test]
fn the_tasks_of_a_repository_go_under_its_workspace() {
    let w = ids(4);
    let workspaces = [
        plain(w[0], Some("/src/benterm")),
        plain(w[1], Some("/tmp")),
        task(w[2], "/src/benterm", "fix-a"),
        task(w[3], "/src/benterm", "fix-b"),
    ];
    assert_eq!(
        sidebar_rows(&workspaces, &none(), w[0]),
        [
            owner(w[0], "/src/benterm", 2, false),
            under(w[2], "fix-a"),
            under(w[3], "fix-b"),
            top(w[1]),
        ]
    );
}

#[test]
fn a_workspace_in_a_directory_of_the_repository_is_its_workspace() {
    let w = ids(2);
    let workspaces = [
        plain(w[0], Some("/src/benterm/crates")),
        task(w[1], "/src/benterm", "fix-a"),
    ];
    assert_eq!(
        sidebar_rows(&workspaces, &none(), w[0]),
        [owner(w[0], "/src/benterm", 1, false), under(w[1], "fix-a")]
    );
}

#[test]
fn a_directory_whose_name_only_starts_like_the_repository_is_not_in_it() {
    let w = ids(2);
    let workspaces = [
        plain(w[0], Some("/src/benterm-old")),
        task(w[1], "/src/benterm", "fix-a"),
    ];
    assert_eq!(
        sidebar_rows(&workspaces, &none(), w[0]),
        [
            top(w[0]),
            heading("/src/benterm", 1, false),
            under(w[1], "fix-a")
        ]
    );
}

#[test]
fn the_first_workspace_of_the_repository_gets_its_tasks() {
    let w = ids(3);
    let workspaces = [
        task(w[0], "/src/benterm", "fix-a"),
        plain(w[1], Some("/src/benterm")),
        plain(w[2], Some("/src/benterm")),
    ];
    assert_eq!(
        sidebar_rows(&workspaces, &none(), w[1]),
        [
            owner(w[1], "/src/benterm", 1, false),
            under(w[0], "fix-a"),
            top(w[2]),
        ]
    );
}

#[test]
fn a_workspace_in_a_repository_inside_another_is_the_inner_ones() {
    let w = ids(3);
    let workspaces = [
        plain(w[0], Some("/src/outer/inner/lib")),
        task(w[1], "/src/outer", "fix-outer"),
        task(w[2], "/src/outer/inner", "fix-inner"),
    ];
    assert_eq!(
        sidebar_rows(&workspaces, &none(), w[0]),
        [
            owner(w[0], "/src/outer/inner", 1, false),
            under(w[2], "fix-inner"),
            heading("/src/outer", 1, false),
            under(w[1], "fix-outer"),
        ]
    );
}

#[test]
fn tasks_without_a_workspace_of_their_repository_go_under_its_name() {
    let w = ids(4);
    let workspaces = [
        plain(w[0], Some("/tmp")),
        task(w[1], "/src/benterm", "fix-a"),
        plain(w[2], None),
        task(w[3], "/src/benterm", "fix-b"),
    ];
    assert_eq!(
        sidebar_rows(&workspaces, &none(), w[0]),
        [
            top(w[0]),
            heading("/src/benterm", 2, false),
            under(w[1], "fix-a"),
            under(w[3], "fix-b"),
            top(w[2]),
        ]
    );
}

#[test]
fn a_folded_group_shows_only_its_selected_row() {
    let w = ids(4);
    let workspaces = [
        plain(w[0], Some("/src/benterm")),
        task(w[1], "/src/benterm", "fix-a"),
        task(w[2], "/src/benterm", "fix-b"),
        task(w[3], "/src/web", "fix-c"),
    ];
    let folded = BTreeSet::from([PathBuf::from("/src/benterm"), PathBuf::from("/src/web")]);
    assert_eq!(
        sidebar_rows(&workspaces, &folded, w[0]),
        [
            owner(w[0], "/src/benterm", 2, true),
            heading("/src/web", 1, true)
        ]
    );
    assert_eq!(
        sidebar_rows(&workspaces, &folded, w[2]),
        [
            owner(w[0], "/src/benterm", 2, true),
            under(w[2], "fix-b"),
            heading("/src/web", 1, true),
        ]
    );
}

#[test]
fn a_folded_repository_without_tasks_changes_nothing() {
    let w = ids(2);
    let workspaces = [plain(w[0], Some("/src/benterm")), plain(w[1], None)];
    let folded = BTreeSet::from([PathBuf::from("/src/benterm")]);
    assert_eq!(
        sidebar_rows(&workspaces, &folded, w[0]),
        [top(w[0]), top(w[1])]
    );
}

#[test]
fn the_heading_is_named_after_the_last_part_of_the_repository() {
    let group = Group {
        repository: PathBuf::from("/src/benterm"),
        tasks: 1,
        folded: false,
    };
    assert_eq!(group.name(), "benterm");
}

/// Workspaces of up to three repositories, `None` for a workspace with
/// no task, and directories in or outside them.
fn workspaces() -> impl Strategy<Value = Vec<(Option<usize>, Option<usize>)>> {
    proptest::collection::vec(
        (
            proptest::option::of(0..3usize),
            proptest::option::of(0..4usize),
        ),
        1..8,
    )
}

const REPOSITORIES: [&str; 3] = ["/src/a", "/src/b", "/src/a/c"];
const DIRECTORIES: [&str; 4] = ["/src/a", "/src/b/x", "/src/a/c", "/elsewhere"];

proptest! {
    /// Every workspace has one row while nothing is folded, and the
    /// workspaces without a task keep their order and their look.
    #[test]
    fn every_workspace_has_one_row_and_the_others_keep_their_order(
        shape in workspaces(),
        selected in 0..8usize,
    ) {
        let w = ids(shape.len());
        let workspaces: Vec<SidebarWorkspace> = shape
            .iter()
            .zip(&w)
            .map(|((repository, directory), id)| match repository {
                Some(repository) => task(*id, REPOSITORIES[*repository], "b"),
                None => plain(*id, directory.map(|d| DIRECTORIES[d])),
            })
            .collect();
        let selected = w[selected % w.len()];
        let rows = sidebar_rows(&workspaces, &none(), selected);
        let mut shown: Vec<WorkspaceId> = rows.iter().filter_map(SidebarRow::workspace).collect();
        shown.sort();
        prop_assert_eq!(shown, w.clone());
        let plain_ids: Vec<WorkspaceId> = workspaces
            .iter()
            .filter(|workspace| workspace.task.is_none())
            .map(|workspace| workspace.id)
            .collect();
        let plain_rows: Vec<WorkspaceId> = rows
            .iter()
            .filter_map(|row| match row {
                SidebarRow::Workspace { id, depth: 0, branch: None, .. } => Some(*id),
                _ => None,
            })
            .collect();
        prop_assert_eq!(plain_rows, plain_ids);
    }

    /// Folding everything leaves the workspaces without a task and the
    /// selected one.
    #[test]
    fn folding_everything_leaves_the_others_and_the_selected_one(
        shape in workspaces(),
        selected in 0..8usize,
    ) {
        let w = ids(shape.len());
        let workspaces: Vec<SidebarWorkspace> = shape
            .iter()
            .zip(&w)
            .map(|((repository, directory), id)| match repository {
                Some(repository) => task(*id, REPOSITORIES[*repository], "b"),
                None => plain(*id, directory.map(|d| DIRECTORIES[d])),
            })
            .collect();
        let selected = w[selected % w.len()];
        let folded: BTreeSet<PathBuf> = REPOSITORIES.iter().map(PathBuf::from).collect();
        let rows = sidebar_rows(&workspaces, &folded, selected);
        let shown: BTreeSet<WorkspaceId> = rows.iter().filter_map(SidebarRow::workspace).collect();
        let expected: BTreeSet<WorkspaceId> = workspaces
            .iter()
            .filter(|workspace| workspace.task.is_none() || workspace.id == selected)
            .map(|workspace| workspace.id)
            .collect();
        prop_assert_eq!(shown, expected);
    }
}

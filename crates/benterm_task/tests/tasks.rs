//! Tasks against a real git repository: making one makes a branch and a
//! worktree, closing one keeps or removes them, and the record of it
//! stays in the tasks file.

mod common;

use benterm_task::{
    AtRisk, CloseError, Git, NewTask, TaskStore, Worktree, close, create,
    make_worktree_root_private, undo,
};
use common::{Repo, branches, commit, git, repo, worktrees};

fn new(repo: &Repo, name: &str) -> NewTask {
    NewTask {
        directory: repo.path.clone(),
        name: name.to_owned(),
        branch: None,
        role: None,
        root: repo.root.clone(),
    }
}

fn store(repo: &Repo) -> TaskStore {
    TaskStore::open(Some(&repo.dir.path().join("state/tasks.json")))
}

/// The folders made before they were made for the owner alone are
/// given those permissions when the app starts: the directory for
/// worktrees and the folders of the repositories in it.
#[test]
fn the_folders_an_earlier_version_made_for_worktrees_are_made_the_owners_alone() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("worktrees");
    let set = |path: &std::path::Path, mode| {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap()
    };
    let mode = |path: &std::path::Path| path.metadata().unwrap().permissions().mode() & 0o777;
    std::fs::create_dir_all(root.join("repo/branch")).unwrap();
    std::fs::write(root.join("note"), "").unwrap();
    let elsewhere = dir.path().join("elsewhere");
    std::fs::create_dir(&elsewhere).unwrap();
    std::os::unix::fs::symlink(&elsewhere, root.join("link")).unwrap();
    for path in [
        &root,
        &root.join("repo"),
        &root.join("repo/branch"),
        &elsewhere,
    ] {
        set(path, 0o755);
    }
    set(&root.join("note"), 0o644);

    make_worktree_root_private(&root);

    assert_eq!(mode(&root), 0o700);
    assert_eq!(mode(&root.join("repo")), 0o700);
    // A worktree, a file and what a link points to are left as they are.
    assert_eq!(mode(&root.join("repo/branch")), 0o755);
    assert_eq!(mode(&root.join("note")), 0o644);
    assert_eq!(mode(&elsewhere), 0o755);
    // A directory for worktrees that is not there is not made.
    make_worktree_root_private(&dir.path().join("missing"));
    assert!(!dir.path().join("missing").exists());
}

/// A repository's `core.worktree` can name any folder as its checkout,
/// as in an archive someone unpacked. A task is not made from a
/// checkout that is neither the pane's folder nor a folder above it.
#[test]
fn a_task_is_not_made_from_a_checkout_that_core_worktree_puts_elsewhere() {
    let repo = repo();
    let elsewhere = repo.dir.path().canonicalize().unwrap().join("elsewhere");
    // Another repository of the user's, which the task would be made in.
    std::fs::create_dir(&elsewhere).unwrap();
    git(&elsewhere, &["init", "-q", "-b", "main"]);
    commit(&elsewhere, "README.md", "other\n");
    git(
        &repo.path,
        &["config", "core.worktree", elsewhere.to_str().unwrap()],
    );
    let store = store(&repo);

    let made = create(&Git::default(), &store, new(&repo, "kanban"));

    let error = made.unwrap_err().to_string();
    assert!(error.contains("core.worktree"), "{error}");
    assert_eq!(branches(&elsewhere), ["main"]);
    assert!(!repo.root.exists());
}

#[test]
fn a_task_gets_a_branch_and_a_worktree_outside_the_repository() {
    let repo = repo();
    let head = git(&repo.path, &["rev-parse", "HEAD"]);
    let store = store(&repo);

    let task = create(&Git::default(), &store, new(&repo, "kanban")).unwrap();

    assert_eq!(task.name, "kanban");
    assert_eq!(task.branch, "kanban");
    assert_eq!(task.repository, repo.path);
    assert_eq!(task.base_branch, "main");
    assert_eq!(task.base_commit, head);
    assert_eq!(task.worktree, repo.root.join("benterm/kanban"));
    assert_eq!(task.closed, None);
    assert_eq!(worktrees(&repo.path), 2);
    assert_eq!(branches(&repo.path), ["kanban", "main"]);
    assert_eq!(git(&task.worktree, &["rev-parse", "HEAD"]), head);
    assert_eq!(
        git(&task.worktree, &["rev-parse", "--abbrev-ref", "HEAD"]),
        "kanban"
    );
    assert_eq!(store.tasks().unwrap(), [task]);
}

#[test]
fn the_folders_made_for_a_worktree_are_the_owners_alone() {
    use std::os::unix::fs::PermissionsExt;
    let repo = repo();
    let mode = |path: &std::path::Path| path.metadata().unwrap().permissions().mode() & 0o777;
    assert!(!repo.root.exists());

    let task = create(&Git::default(), &store(&repo), new(&repo, "kanban")).unwrap();

    // The directory for worktrees, the one of the repository in it, and
    // the worktree: others on the machine do not read the checkout.
    assert_eq!(mode(&repo.root), 0o700);
    assert_eq!(mode(&repo.root.join("benterm")), 0o700);
    assert_eq!(mode(&task.worktree), 0o700);

    // A directory that was there keeps the permissions it was given.
    std::fs::set_permissions(&repo.root, std::fs::Permissions::from_mode(0o755)).unwrap();
    let second = create(&Git::default(), &store(&repo), new(&repo, "second")).unwrap();
    assert_eq!(mode(&repo.root), 0o755);
    assert_eq!(mode(&second.worktree), 0o700);
}

#[test]
fn the_fork_point_stays_when_the_base_branch_moves_on() {
    let repo = repo();
    let head = git(&repo.path, &["rev-parse", "HEAD"]);
    let store = store(&repo);
    let task = create(&Git::default(), &store, new(&repo, "kanban")).unwrap();
    commit(&repo.path, "later.txt", "later\n");
    assert_eq!(store.tasks().unwrap()[0].base_commit, head);
    assert_eq!(task.base_commit, head);
}

#[test]
fn a_task_can_be_made_from_inside_a_subdirectory_and_with_a_branch_of_its_own() {
    let repo = repo();
    std::fs::create_dir(repo.path.join("src")).unwrap();
    let store = store(&repo);
    let mut new = new(&repo, "かんばん");
    new.directory = repo.path.join("src");
    new.branch = Some("issue-93/kanban".to_owned());
    new.role = Some("reviewer".to_owned());

    let task = create(&Git::default(), &store, new).unwrap();

    assert_eq!(task.name, "かんばん");
    assert_eq!(task.branch, "issue-93/kanban");
    assert_eq!(task.role.as_deref(), Some("reviewer"));
    assert_eq!(task.repository, repo.path);
    assert_eq!(task.worktree, repo.root.join("benterm/issue-93-kanban"));
}

#[test]
fn a_branch_that_exists_makes_nothing_and_says_why() {
    let repo = repo();
    git(&repo.path, &["branch", "kanban"]);
    let store = store(&repo);

    let error = create(&Git::default(), &store, new(&repo, "kanban")).unwrap_err();

    assert!(error.to_string().contains("kanban"), "{error}");
    assert!(error.to_string().contains("already"), "{error}");
    assert_eq!(worktrees(&repo.path), 1);
    assert!(!repo.root.join("benterm/kanban").exists());
    assert_eq!(store.tasks().unwrap(), []);
}

#[test]
fn what_cannot_be_a_task_makes_nothing_and_says_why() {
    let repo = repo();
    let store = store(&repo);
    let git_ = Git::default();
    for name in ["", "a b", "-x", "a..b", "HEAD", "@{-1}"] {
        let error = create(&git_, &store, new(&repo, name)).unwrap_err();
        assert!(!error.to_string().is_empty(), "{name:?}");
    }
    // Not a repository.
    let mut outside = new(&repo, "x");
    outside.directory = repo.dir.path().to_owned();
    let error = create(&git_, &store, outside).unwrap_err();
    assert!(error.to_string().contains("repository"), "{error}");
    // No branch to go back to.
    git(&repo.path, &["checkout", "-q", "--detach"]);
    let error = create(&git_, &store, new(&repo, "x")).unwrap_err();
    assert!(error.to_string().contains("branch"), "{error}");
    git(&repo.path, &["checkout", "-q", "main"]);
    // The place is taken.
    std::fs::create_dir_all(repo.root.join("benterm/taken")).unwrap();
    let error = create(&git_, &store, new(&repo, "taken")).unwrap_err();
    assert!(error.to_string().contains("already"), "{error}");

    assert_eq!(branches(&repo.path), ["main"]);
    assert_eq!(worktrees(&repo.path), 1);
    assert_eq!(store.tasks().unwrap(), []);
}

#[test]
fn a_task_that_was_just_made_can_be_taken_back() {
    let repo = repo();
    let store = store(&repo);
    let task = create(&Git::default(), &store, new(&repo, "kanban")).unwrap();

    undo(&Git::default(), &store, &task).unwrap();

    assert_eq!(branches(&repo.path), ["main"]);
    assert_eq!(worktrees(&repo.path), 1);
    assert!(!task.worktree.exists());
    assert_eq!(store.tasks().unwrap(), []);
}

#[test]
fn closing_and_keeping_leaves_the_worktree_and_the_branch() {
    let repo = repo();
    let store = store(&repo);
    let task = create(&Git::default(), &store, new(&repo, "kanban")).unwrap();

    let closed = close(&Git::default(), &store, task.id, None).unwrap().task;

    assert_eq!(
        closed.closed.as_ref().map(|c| c.worktree),
        Some(Worktree::Kept)
    );
    assert!(task.worktree.is_dir());
    assert_eq!(branches(&repo.path), ["kanban", "main"]);
    assert_eq!(store.tasks().unwrap(), [closed]);
}

#[test]
fn closing_and_removing_takes_the_worktree_and_the_branch_away() {
    let repo = repo();
    let store = store(&repo);
    let task = create(&Git::default(), &store, new(&repo, "kanban")).unwrap();

    let closed = close(&Git::default(), &store, task.id, Some(AtRisk::default()))
        .unwrap()
        .task;

    assert_eq!(
        closed.closed.as_ref().map(|c| c.worktree),
        Some(Worktree::Removed)
    );
    assert!(!task.worktree.exists());
    assert_eq!(branches(&repo.path), ["main"]);
    assert_eq!(worktrees(&repo.path), 1);
    // The record stays, for the board.
    assert_eq!(store.tasks().unwrap(), [closed]);
}

#[test]
fn what_would_be_lost_is_counted_and_removed_only_when_that_was_accepted() {
    let repo = repo();
    let store = store(&repo);
    let git_ = Git::default();
    let task = create(&git_, &store, new(&repo, "kanban")).unwrap();
    commit(&task.worktree, "done.txt", "done\n");
    std::fs::write(task.worktree.join("new.txt"), "new\n").unwrap();
    std::fs::write(task.worktree.join("README.md"), "changed\n").unwrap();
    std::fs::write(task.worktree.join("done.txt"), "again\n").unwrap();
    git(&task.worktree, &["add", "done.txt"]);
    std::fs::write(task.worktree.join("done.txt"), "and again\n").unwrap();

    let risk = git_.at_risk(&task).unwrap();
    assert_eq!(
        risk,
        AtRisk {
            uncommitted: 3,
            commits: 1
        }
    );

    // Nothing was accepted, so nothing is removed.
    let refused = close(&git_, &store, task.id, Some(AtRisk::default())).unwrap_err();
    assert_eq!(refused, CloseError::AtRisk(risk));
    assert!(task.worktree.join("new.txt").exists());
    assert_eq!(store.tasks().unwrap()[0].closed, None);

    // More is at risk than was accepted.
    let fewer = AtRisk {
        uncommitted: 2,
        commits: 1,
    };
    assert_eq!(
        close(&git_, &store, task.id, Some(fewer)).unwrap_err(),
        CloseError::AtRisk(risk)
    );

    let closed = close(&git_, &store, task.id, Some(risk)).unwrap().task;
    assert_eq!(
        closed.closed.as_ref().map(|c| c.worktree),
        Some(Worktree::Removed)
    );
    assert!(!task.worktree.exists());
    assert_eq!(branches(&repo.path), ["main"]);
}

#[test]
fn commits_that_another_branch_has_are_not_at_risk() {
    let repo = repo();
    let store = store(&repo);
    let git_ = Git::default();
    let task = create(&git_, &store, new(&repo, "kanban")).unwrap();
    commit(&task.worktree, "done.txt", "done\n");
    git(&repo.path, &["merge", "-q", "--ff-only", "kanban"]);
    assert_eq!(git_.at_risk(&task).unwrap(), AtRisk::default());
}

#[test]
fn a_kept_worktree_can_be_removed_later_and_a_removed_one_not_again() {
    let repo = repo();
    let store = store(&repo);
    let git_ = Git::default();
    let task = create(&git_, &store, new(&repo, "kanban")).unwrap();
    close(&git_, &store, task.id, None).unwrap();

    let closed = close(&git_, &store, task.id, Some(AtRisk::default()))
        .unwrap()
        .task;
    assert_eq!(closed.closed.map(|c| c.worktree), Some(Worktree::Removed));
    assert!(!task.worktree.exists());

    let again = close(&git_, &store, task.id, Some(AtRisk::default())).unwrap_err();
    assert!(matches!(again, CloseError::Failed(_)), "{again:?}");
    let unknown = close(&git_, &store, 99, None).unwrap_err();
    assert!(unknown.to_string().contains("99"), "{unknown}");
}

#[test]
fn the_name_of_a_closed_and_removed_task_is_free_again() {
    let repo = repo();
    let store = store(&repo);
    let git_ = Git::default();
    let first = create(&git_, &store, new(&repo, "kanban")).unwrap();
    close(&git_, &store, first.id, Some(AtRisk::default())).unwrap();

    let second = create(&git_, &store, new(&repo, "kanban")).unwrap();

    assert_ne!(first.id, second.id);
    assert_eq!(store.tasks().unwrap().len(), 2);
}

#[test]
fn a_task_made_in_a_worktree_belongs_to_the_main_checkout() {
    let repo = repo();
    let store = store(&repo);
    let git_ = Git::default();
    let first = create(&git_, &store, new(&repo, "first")).unwrap();
    let head = commit(&first.worktree, "done.txt", "done\n");

    let mut from_first = new(&repo, "second");
    from_first.directory = first.worktree.clone();
    let second = create(&git_, &store, from_first).unwrap();

    assert_eq!(second.repository, repo.path);
    assert_eq!(second.base_branch, "first");
    assert_eq!(second.base_commit, head);
    assert_eq!(second.worktree, repo.root.join("benterm/second"));
}

#[test]
fn application_worktree_creation_ignores_the_checkout_hook() {
    use std::os::unix::fs::PermissionsExt;
    let repo = repo();
    let hook = repo.path.join(".git/hooks/post-checkout");
    std::fs::write(&hook, "#!/bin/sh\nexit 42\n").unwrap();
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    let store = store(&repo);

    let task = create(&Git::default(), &store, new(&repo, "kanban")).unwrap();

    assert_eq!(branches(&repo.path), ["kanban", "main"]);
    assert_eq!(worktrees(&repo.path), 2);
    assert!(task.worktree.is_dir());
    assert_eq!(store.tasks().unwrap(), [task]);
}

#[test]
fn a_branch_that_cannot_be_deleted_is_said_and_the_task_is_closed_all_the_same() {
    let repo = repo();
    let store = store(&repo);
    let git_ = Git::default();
    let task = create(&git_, &store, new(&repo, "kanban")).unwrap();
    // Another checkout is on the branch too.
    let other = repo.dir.path().join("other");
    git(
        &repo.path,
        &[
            "worktree",
            "add",
            "-q",
            "--force",
            other.to_str().unwrap(),
            "kanban",
        ],
    );

    let closing = close(&git_, &store, task.id, Some(AtRisk::default())).unwrap();

    assert!(!task.worktree.exists());
    assert_eq!(branches(&repo.path), ["kanban", "main"]);
    let left = closing.branch_left.expect("the branch is left");
    assert!(left.to_string().contains("kanban"), "{left}");
    assert_eq!(
        closing.task.closed.map(|c| c.worktree),
        Some(Worktree::Removed)
    );
}

/// A `PATH` on which `git` is a script that runs `before` in the
/// repository ahead of every `git worktree add`, as another program
/// that gets in between would.
fn git_with(repo: &Repo, before: &str) -> Git {
    use std::os::unix::fs::PermissionsExt;
    let bin = repo.dir.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    let script = format!(
        "#!/bin/sh\nif [ \"$3\" = worktree ] && [ \"$4\" = add ]; then\n  \
         /usr/bin/git -C \"$2\" {before} >/dev/null 2>&1\nfi\nexec /usr/bin/git \"$@\"\n"
    );
    std::fs::write(bin.join("git"), script).unwrap();
    std::fs::set_permissions(bin.join("git"), std::fs::Permissions::from_mode(0o755)).unwrap();
    Git {
        path: Some(format!("{}:/usr/bin:/bin", bin.display()).into()),
        restrict_programs: false,
        ..Git::default()
    }
}

#[test]
fn a_branch_someone_else_makes_in_between_is_not_taken_away() {
    let repo = repo();
    let store = store(&repo);
    // The branch appears after it was looked for and before git makes it.
    let git_ = git_with(&repo, "branch kanban");

    let error = create(&git_, &store, new(&repo, "kanban"));

    assert!(error.is_err());
    assert_eq!(branches(&repo.path), ["kanban", "main"]);
    assert_eq!(worktrees(&repo.path), 1);
    assert!(!repo.root.join("benterm/kanban").exists());
    assert_eq!(store.tasks().unwrap(), []);
}

#[test]
fn commits_made_off_the_tasks_branch_in_its_worktree_are_at_risk_too() {
    let repo = repo();
    let store = store(&repo);
    let git_ = Git::default();
    let task = create(&git_, &store, new(&repo, "kanban")).unwrap();
    git(&task.worktree, &["checkout", "-q", "--detach"]);
    commit(&task.worktree, "done.txt", "done\n");

    let risk = git_.at_risk(&task).unwrap();

    assert_eq!(
        risk,
        AtRisk {
            uncommitted: 0,
            commits: 1
        }
    );
    let refused = close(&git_, &store, task.id, Some(AtRisk::default())).unwrap_err();
    assert_eq!(refused, CloseError::AtRisk(risk));
    assert!(task.worktree.join("done.txt").exists());
}

#[test]
fn an_older_task_does_not_remove_what_a_newer_one_has_at_the_same_place() {
    let repo = repo();
    let store = store(&repo);
    let git_ = Git::default();
    let old = create(&git_, &store, new(&repo, "kanban")).unwrap();
    close(&git_, &store, old.id, None).unwrap();
    // Removed by hand, and made again under the same name.
    git(
        &repo.path,
        &["worktree", "remove", old.worktree.to_str().unwrap()],
    );
    git(&repo.path, &["branch", "-D", "kanban"]);
    let new_task = create(&git_, &store, new(&repo, "kanban")).unwrap();
    assert_eq!(new_task.worktree, old.worktree);

    let refused = close(&git_, &store, old.id, Some(AtRisk::ANYTHING)).unwrap_err();

    assert!(matches!(refused, CloseError::Failed(_)), "{refused:?}");
    assert!(new_task.worktree.is_dir());
    assert_eq!(branches(&repo.path), ["kanban", "main"]);
}

#[test]
fn a_worktree_that_is_gone_already_leaves_the_branch_to_remove() {
    let repo = repo();
    let store = store(&repo);
    let git_ = Git::default();
    let task = create(&git_, &store, new(&repo, "kanban")).unwrap();
    git(
        &repo.path,
        &["worktree", "remove", task.worktree.to_str().unwrap()],
    );

    let closing = close(&git_, &store, task.id, Some(AtRisk::default())).unwrap();

    assert_eq!(closing.branch_left, None);
    assert_eq!(branches(&repo.path), ["main"]);
    assert_eq!(
        closing.task.closed.map(|c| c.worktree),
        Some(Worktree::Removed)
    );
}

#[test]
fn a_directory_for_worktrees_that_leads_into_the_repository_is_refused() {
    let repo = repo();
    let store = store(&repo);
    let alias = repo.dir.path().join("alias");
    std::os::unix::fs::symlink(&repo.path, &alias).unwrap();
    let mut inside = new(&repo, "kanban");
    inside.root = alias.join("trees");

    let error = create(&Git::default(), &store, inside).unwrap_err();

    assert!(error.to_string().contains("inside"), "{error}");
    assert_eq!(branches(&repo.path), ["main"]);
}

#[test]
fn a_worktree_whose_git_was_replaced_is_not_looked_at() {
    let repo = repo();
    let store = store(&repo);
    let git_ = Git::default();
    let task = create(&git_, &store, new(&repo, "kanban")).unwrap();
    git_.check_worktree(&task.repository, &task.worktree)
        .unwrap();

    std::fs::remove_file(task.worktree.join(".git")).unwrap();
    git(&task.worktree, &["init", "-q", "-b", "main"]);

    assert!(
        git_.check_worktree(&task.repository, &task.worktree)
            .is_err()
    );
    let error = git_.at_risk(&task).unwrap_err();
    assert!(error.to_string().contains("git is not run"), "{error}");
}

//! The issues a repository keeps in a file of its own: what the file may
//! say, what is refused, and how the tasks of a repository without a
//! remote are matched to them.

mod common;

use std::path::Path;

use benterm_task::{
    Git, LOCAL_ISSUES_LIMIT, LocalIssueState, LocalIssuesError, NewTask, TaskStore, create,
    local_issues_path, parse_local_issues, read_local_issues, tasks_by_issue,
};
use common::{git, repo};

fn write(root: &Path, text: &str) {
    let path = local_issues_path(root);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

const SAMPLE: &str = r#"{
  "version": 1,
  "issues": [
    { "number": 1, "title": "Epic", "description": "What the epic is for" },
    { "number": 2, "title": "First", "state": "closed", "parent": 1 },
    { "number": 3, "title": "Second", "state": "open", "parent": 1, "blocked_by": [2, 2] }
  ]
}"#;

#[test]
fn the_file_is_under_the_benterm_directory_of_the_repository() {
    assert_eq!(
        local_issues_path(Path::new("/r/repo")),
        Path::new("/r/repo/.benterm/issues.json")
    );
}

#[test]
fn a_file_gives_its_issues_with_their_parents_and_what_blocks_them() {
    let dir = tempfile::tempdir().unwrap();
    write(dir.path(), SAMPLE);

    let read = read_local_issues(dir.path()).unwrap();

    assert_eq!(read.issues.len(), 3);
    let epic = read.issue(1).unwrap();
    assert_eq!(epic.title, "Epic");
    assert_eq!(epic.state, LocalIssueState::Open);
    assert_eq!(epic.parent, None);
    assert_eq!(epic.description.as_deref(), Some("What the epic is for"));
    let first = read.issue(2).unwrap();
    assert_eq!(first.state, LocalIssueState::Closed);
    assert_eq!(first.parent, Some(1));
    let second = read.issue(3).unwrap();
    // The same blocker written twice is one blocker.
    assert_eq!(second.blocked_by, [2]);
    assert_eq!(second.description, None);
}

#[test]
fn a_repository_without_the_file_says_that_it_is_missing() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(
        read_local_issues(dir.path()),
        Err(LocalIssuesError::Missing)
    );
    std::fs::create_dir(dir.path().join(".benterm")).unwrap();
    assert_eq!(
        read_local_issues(dir.path()),
        Err(LocalIssuesError::Missing)
    );
}

#[test]
fn what_is_not_the_format_is_refused_with_what_is_wrong() {
    for (text, expected) in [
        ("{ not json", "JSON"),
        (r#"{"issues": []}"#, "version"),
        (r#"{"version": 2, "issues": []}"#, "version 2"),
        (r#"{"version": 1}"#, "issues"),
        (
            r#"{"version": 1, "issues": [{"number": 0, "title": "t"}]}"#,
            "number",
        ),
        (
            r#"{"version": 1, "issues": [{"number": 1, "title": "  "}]}"#,
            "#1 has no title",
        ),
        (
            r#"{"version": 1, "issues": [{"number": 1, "title": "t", "state": "done"}]}"#,
            "done",
        ),
        (
            r#"{"version": 1, "issues": [{"number": 1, "title": "a"}, {"number": 1, "title": "b"}]}"#,
            "#1 is there more than once",
        ),
        (
            r#"{"version": 1, "issues": [{"number": 1, "title": "t", "parent": 9}]}"#,
            "#9",
        ),
        (
            r#"{"version": 1, "issues": [{"number": 1, "title": "t", "blocked_by": [9]}]}"#,
            "#9",
        ),
        (
            r#"{"version": 1, "issues": [{"number": 1, "title": "t", "blocked_by": [1]}]}"#,
            "itself",
        ),
        (
            r#"{"version": 1, "issues": [{"number": 1, "title": "t", "parent": 1}]}"#,
            "itself",
        ),
        (
            r#"{"version": 1, "issues": [{"number": 1, "title": "t", "blockedBy": []}]}"#,
            "blockedBy",
        ),
    ] {
        match parse_local_issues(text.as_bytes()) {
            Err(LocalIssuesError::Invalid(why)) => {
                assert!(why.contains(expected), "{text}: {why:?} lacks {expected:?}")
            }
            other => panic!("{text}: {other:?}"),
        }
    }
}

#[test]
fn issues_that_block_each_other_in_a_circle_are_read_as_they_are() {
    let read = parse_local_issues(
        br#"{"version": 1, "issues": [
            {"number": 1, "title": "a", "blocked_by": [3]},
            {"number": 2, "title": "b", "blocked_by": [1]},
            {"number": 3, "title": "c", "blocked_by": [2]}
        ]}"#,
    )
    .unwrap();
    assert_eq!(read.issue(1).unwrap().blocked_by, [3]);
    assert_eq!(read.issue(2).unwrap().blocked_by, [1]);
    assert_eq!(read.issue(3).unwrap().blocked_by, [2]);
}

#[test]
fn a_file_over_the_limit_is_not_read() {
    let dir = tempfile::tempdir().unwrap();
    let padding = " ".repeat(LOCAL_ISSUES_LIMIT);
    write(
        dir.path(),
        &format!(r#"{{"version": 1, "issues": []}}{padding}"#),
    );
    assert_eq!(
        read_local_issues(dir.path()),
        Err(LocalIssuesError::TooLarge {
            limit: LOCAL_ISSUES_LIMIT
        })
    );
}

#[cfg(unix)]
#[test]
fn a_symbolic_link_is_not_followed_for_the_file_or_its_directory() {
    use std::os::unix::fs::symlink;

    let elsewhere = tempfile::tempdir().unwrap();
    write(elsewhere.path(), SAMPLE);

    // The file is a link.
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join(".benterm")).unwrap();
    symlink(
        local_issues_path(elsewhere.path()),
        local_issues_path(dir.path()),
    )
    .unwrap();
    assert!(matches!(
        read_local_issues(dir.path()),
        Err(LocalIssuesError::NotPlainFile(_))
    ));

    // The directory is a link.
    let dir = tempfile::tempdir().unwrap();
    symlink(
        elsewhere.path().join(".benterm"),
        dir.path().join(".benterm"),
    )
    .unwrap();
    assert!(matches!(
        read_local_issues(dir.path()),
        Err(LocalIssuesError::NotPlainFile(_))
    ));

    // The file is a directory.
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(local_issues_path(dir.path())).unwrap();
    assert!(matches!(
        read_local_issues(dir.path()),
        Err(LocalIssuesError::NotPlainFile(_))
    ));
}

#[test]
fn tasks_of_a_repository_without_a_remote_match_its_local_issues_by_branch() {
    let repo = repo();
    assert_eq!(git(&repo.path, &["remote"]), "");
    write(
        &repo.path,
        r#"{"version": 1, "issues": [
            {"number": 7, "title": "Login"},
            {"number": 8, "title": "Logout", "blocked_by": [7]}
        ]}"#,
    );
    let other = common::repo();
    let git_runner = Git::default();
    let store = TaskStore::open(Some(&repo.dir.path().join("state/tasks.json")));
    let task_in = |repo: &common::Repo, branch: &str| {
        create(
            &git_runner,
            &store,
            NewTask {
                directory: repo.path.clone(),
                name: branch.to_owned(),
                branch: Some(branch.to_owned()),
                role: None,
                root: repo.root.clone(),
            },
        )
        .unwrap()
    };
    let seven = task_in(&repo, "issue-7");
    let unrelated = task_in(&repo, "cleanup");
    let elsewhere = task_in(&other, "issue-8");

    // The repository is known by the path of its main checkout, from the
    // checkout itself and from the worktree of one of its tasks.
    let root = git_runner.main_checkout(&repo.path).unwrap();
    assert_eq!(root, repo.path);
    assert_eq!(git_runner.main_checkout(&seven.worktree).unwrap(), root);
    assert_ne!(git_runner.main_checkout(&elsewhere.worktree).unwrap(), root);

    let issues = read_local_issues(&root).unwrap();
    let tasks = [seven.clone(), unrelated, elsewhere];
    let matched = tasks_by_issue(&tasks, |repository| {
        git_runner
            .main_checkout(repository)
            .is_ok_and(|checkout| checkout == root)
    });

    assert_eq!(matched.keys().copied().collect::<Vec<_>>(), [7]);
    assert_eq!(matched[&7].open, [seven]);
    assert!(issues.issue(7).is_some());
    // The task for #8 is of another repository, so #8 here has none.
    assert!(issues.issue(8).is_some());
    assert!(!matched.contains_key(&8));
}

#[test]
fn outside_a_git_repository_there_is_no_main_checkout() {
    let dir = tempfile::tempdir().unwrap();
    assert!(Git::default().main_checkout(dir.path()).is_err());
}

#[test]
fn a_field_that_is_not_known_is_read_past_unless_it_is_spelt_like_a_known_one() {
    let read = parse_local_issues(
        br#"{"version": 1, "next_number": 9, "project": "p", "issues": [
            {"number": 1, "title": "t", "labels": ["a"], "estimate": 3}
        ]}"#,
    )
    .unwrap();
    assert_eq!(read.issues.len(), 1);
    assert_eq!(read.issue(1).unwrap().title, "t");

    for (text, field) in [
        (
            r#"{"version": 1, "issues": [{"number": 1, "title": "t", "blocked-by": []}]}"#,
            "blocked-by",
        ),
        (
            r#"{"version": 1, "issues": [{"number": 1, "title": "t", "Parent": 1}]}"#,
            "Parent",
        ),
        (
            r#"{"version": 1, "issues": [], "nextNumber": 3}"#,
            "nextNumber",
        ),
    ] {
        match parse_local_issues(text.as_bytes()) {
            Err(LocalIssuesError::Invalid(why)) => assert!(why.contains(field), "{why:?}"),
            other => panic!("{text}: {other:?}"),
        }
    }
}

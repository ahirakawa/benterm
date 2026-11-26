//! Matching tasks to issues by branch name and repository.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use benterm_task::{
    Closed, RepoId, Task, Worktree, default_issue_branch, issue_branch_for, issue_number_in_branch,
    tasks_by_issue, tasks_for_issue,
};

fn task(id: u64, repo: &str, branch: &str, created_at_ms: u64, closed: Option<Closed>) -> Task {
    serde_json::from_value(serde_json::json!({
        "id": id,
        "name": format!("task-{id}"),
        "repository": repo,
        "base_branch": "main",
        "base_commit": "0123456789abcdef0123456789abcdef01234567",
        "branch": branch,
        "worktree": format!("/w/repo/task-{id}"),
        "created_at_ms": created_at_ms,
        "closed": closed,
    }))
    .unwrap()
}

#[test]
fn branch_matching_requires_non_alphanumeric_boundaries() {
    assert_eq!(issue_number_in_branch("issue-12"), Some(12));
    assert_eq!(issue_number_in_branch("issue-123"), Some(123));
    assert_eq!(issue_number_in_branch("tissue-12"), None);
    assert_eq!(issue_number_in_branch("issue-12a"), None);
    assert_eq!(issue_number_in_branch("fix/issue-7-login"), Some(7));
    assert_eq!(issue_number_in_branch("tissue-1/issue-7"), Some(7));
    assert_eq!(issue_number_in_branch("feat_issue-42_wip"), Some(42));
    assert_eq!(issue_number_in_branch("issue-0"), None);
    assert_eq!(issue_number_in_branch("issue-"), None);
    assert_eq!(issue_number_in_branch("main"), None);
}

#[test]
fn only_issue_7_and_fix_issue_7_login_match_issue_7() {
    let repo = Path::new("/r/benterm");
    let tasks = vec![
        task(1, "/r/benterm", "issue-7", 100, None),
        task(2, "/r/benterm", "fix/issue-7-login", 200, None),
        task(3, "/r/benterm", "issue-70", 300, None),
        task(4, "/r/benterm", "tissue-7", 400, None),
    ];

    let matched = tasks_for_issue(&tasks, 7, |path| path == repo);
    let branches: Vec<&str> = matched.open.iter().map(|t| t.branch.as_str()).collect();
    assert_eq!(branches, vec!["fix/issue-7-login", "issue-7"]);
    assert!(matched.closed.is_empty());

    let grouped = tasks_by_issue(&tasks, |path| path == repo);
    assert_eq!(grouped.keys().copied().collect::<Vec<_>>(), vec![7, 70]);
}

#[test]
fn open_tasks_are_newest_first_and_closed_tasks_are_separated() {
    let repo = Path::new("/r/benterm");
    let tasks = vec![
        task(1, "/r/benterm", "issue-7", 1_000, None),
        task(
            2,
            "/r/benterm",
            "issue-7-old",
            2_000,
            Some(Closed::new(2_500, Worktree::Kept)),
        ),
        task(3, "/r/benterm", "issue-7-retry", 3_000, None),
    ];

    let matched = tasks_for_issue(&tasks, 7, |path| path == repo);
    let open_ids: Vec<u64> = matched.open.iter().map(|t| t.id).collect();
    let closed_ids: Vec<u64> = matched.closed.iter().map(|t| t.id).collect();
    assert_eq!(open_ids, vec![3, 1]);
    assert_eq!(closed_ids, vec![2]);
}

#[test]
fn tasks_from_another_repository_do_not_match() {
    let epic_repo = RepoId::new("Owner", "Benterm").unwrap();
    let resolved: HashMap<PathBuf, RepoId> = [
        (
            PathBuf::from("/r/benterm"),
            RepoId::parse_remote_url("git@github.com:owner/benterm.git").unwrap(),
        ),
        (
            PathBuf::from("/r/other"),
            RepoId::parse_remote_url("https://github.com/owner/other.git").unwrap(),
        ),
    ]
    .into_iter()
    .collect();

    let tasks = vec![
        task(1, "/r/benterm", "issue-7", 100, None),
        task(2, "/r/other", "issue-7", 200, None),
        task(3, "/r/unknown", "issue-7", 300, None),
    ];

    let matched = tasks_for_issue(&tasks, 7, |path| resolved.get(path) == Some(&epic_repo));
    let ids: Vec<u64> = matched.open.iter().map(|t| t.id).collect();
    assert_eq!(ids, vec![1]);
}

#[test]
fn default_issue_branch_and_collision_suffix() {
    assert_eq!(default_issue_branch(7), "issue-7");
    assert_eq!(issue_branch_for(7, []), "issue-7");
    assert_eq!(issue_branch_for(7, ["issue-7"]), "issue-7-2");
    assert_eq!(issue_branch_for(7, ["issue-7", "issue-7-2"]), "issue-7-3");
    assert_eq!(
        issue_number_in_branch(&issue_branch_for(7, ["issue-7"])),
        Some(7)
    );
}

#[test]
fn parse_remote_url_handles_https_ssh_and_scp_forms() {
    let expected = RepoId::new("octocat", "hello-world").unwrap();
    assert_eq!(
        RepoId::parse_remote_url("https://github.com/Octocat/Hello-World.git"),
        Some(expected.clone())
    );
    assert_eq!(
        RepoId::parse_remote_url("https://github.com/octocat/hello-world"),
        Some(expected.clone())
    );
    assert_eq!(
        RepoId::parse_remote_url("ssh://git@github.com/octocat/hello-world.git"),
        Some(expected.clone())
    );
    assert_eq!(
        RepoId::parse_remote_url("git@github.com:octocat/hello-world.git"),
        Some(expected)
    );
    assert_eq!(RepoId::parse_remote_url("not-a-url"), None);
}

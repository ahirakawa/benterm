//! Integration tests for `benterm_github` using fake `gh` scripts and local
//! temporary git repositories. Never contacts real GitHub.

use std::ffi::OsString;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use benterm_github::{
    ClosedReason, Github, GithubError, IssueRef, IssueState, PullRequest, PullRequestState,
    Repository,
};
use tempfile::TempDir;

/// Writes an executable fake `gh` script into a temporary directory and returns
/// a `Github` client whose `PATH` prepends that directory.
fn fake_gh(script: &str) -> (TempDir, Github) {
    let dir = TempDir::new().unwrap();
    let bin = dir.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let gh_path = bin.join("gh");
    std::fs::write(&gh_path, script).unwrap();
    std::fs::set_permissions(&gh_path, std::fs::Permissions::from_mode(0o755)).unwrap();

    let mut path = OsString::from(&bin);
    path.push(":/usr/bin:/bin");
    let github = Github {
        path: Some(path),
        ..Github::default()
    };
    (dir, github)
}

fn init_git_repo(remote_url: Option<&str>) -> TempDir {
    let dir = TempDir::new().unwrap();
    let status = Command::new("git")
        .args(["init", "-q"])
        .current_dir(dir.path())
        .status()
        .unwrap();
    assert!(status.success(), "git init failed");
    if let Some(url) = remote_url {
        let status = Command::new("git")
            .args(["remote", "add", "origin", url])
            .current_dir(dir.path())
            .status()
            .unwrap();
        assert!(status.success(), "git remote add failed");
    }
    dir
}

#[cfg(unix)]
fn assert_all_pids_reaped(pid_file: &Path) {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if let Ok(contents) = std::fs::read_to_string(pid_file) {
            let pids: Vec<i32> = contents
                .lines()
                .filter_map(|line| line.trim().parse::<i32>().ok())
                .collect();
            if !pids.is_empty() {
                let alive: Vec<i32> = pids
                    .into_iter()
                    .filter(|&pid| unsafe { libc::kill(pid, 0) == 0 })
                    .collect();
                if alive.is_empty() {
                    return;
                }
                if Instant::now() >= deadline {
                    panic!("child processes were not reaped: {alive:?}");
                }
            }
        } else if Instant::now() >= deadline {
            panic!("pid file {} was never written", pid_file.display());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn reads_epic_with_three_sub_issues_two_dependencies_and_one_pr() {
    let response = r#"{
  "data": {
    "repository": {
      "issue": {
        "number": 1,
        "title": "Epic: Graph View",
        "state": "OPEN",
        "stateReason": null,
        "url": "https://github.com/octo/demo/issues/1",
        "parent": null,
        "blockedBy": { "nodes": [] },
        "blocking": { "nodes": [] },
        "closedByPullRequestsReferences": { "nodes": [] },
        "subIssues": {
          "pageInfo": { "hasNextPage": false, "endCursor": null },
          "nodes": [
            {
              "number": 10,
              "title": "Read Epic data",
              "state": "CLOSED",
              "stateReason": "COMPLETED",
              "url": "https://github.com/octo/demo/issues/10",
              "blockedBy": { "nodes": [] },
              "blocking": {
                "nodes": [
                  {
                    "number": 11,
                    "title": "Render graph",
                    "state": "OPEN",
                    "stateReason": null,
                    "url": "https://github.com/octo/demo/issues/11"
                  }
                ]
              },
              "closedByPullRequestsReferences": {
                "nodes": [
                  {
                    "number": 42,
                    "state": "MERGED",
                    "isDraft": false,
                    "headRefName": "issue-10",
                    "url": "https://github.com/octo/demo/pull/42"
                  }
                ]
              }
            },
            {
              "number": 11,
              "title": "Render graph",
              "state": "OPEN",
              "stateReason": null,
              "url": "https://github.com/octo/demo/issues/11",
              "blockedBy": {
                "nodes": [
                  {
                    "number": 10,
                    "title": "Read Epic data",
                    "state": "CLOSED",
                    "stateReason": "COMPLETED",
                    "url": "https://github.com/octo/demo/issues/10"
                  },
                  {
                    "number": 99,
                    "title": "External dependency outside epic",
                    "state": "OPEN",
                    "stateReason": null,
                    "url": "https://github.com/octo/demo/issues/99"
                  }
                ]
              },
              "blocking": { "nodes": [] },
              "closedByPullRequestsReferences": { "nodes": [] }
            },
            {
              "number": 12,
              "title": "Dropped idea",
              "state": "CLOSED",
              "stateReason": "NOT_PLANNED",
              "url": "https://github.com/octo/demo/issues/12",
              "blockedBy": { "nodes": [] },
              "blocking": { "nodes": [] },
              "closedByPullRequestsReferences": { "nodes": [] }
            }
          ]
        }
      }
    }
  }
}"#;

    let script = format!(
        "#!/bin/sh\n\
         if [ \"$1\" = \"auth\" ] && [ \"$2\" = \"status\" ]; then\n\
           echo \"Logged in to github.com\"\n\
           exit 0\n\
         fi\n\
         if [ \"$1\" = \"api\" ] && [ \"$2\" = \"graphql\" ]; then\n\
           cat <<'EOF'\n\
{response}\n\
EOF\n\
           exit 0\n\
         fi\n\
         echo \"unexpected args: $*\" >&2\n\
         exit 1\n"
    );
    let (_dir, github) = fake_gh(&script);
    let repo = Repository::parse("octo/demo").unwrap();

    let epic = github.fetch_epic(&repo, 1).unwrap();

    assert_eq!(epic.repository, repo);
    assert_eq!(epic.issue.number, 1);
    assert_eq!(epic.issue.title, "Epic: Graph View");
    assert_eq!(epic.issue.state, IssueState::Open);
    assert_eq!(epic.issue.url, "https://github.com/octo/demo/issues/1");
    assert_eq!(epic.sub_issues.len(), 3);

    // Sub-issue #10: closed completed, blocking #11, closed by merged PR #42.
    let sub10 = &epic.sub_issues[0];
    assert_eq!(sub10.number, 10);
    assert_eq!(
        sub10.state,
        IssueState::Closed(Some(ClosedReason::Completed))
    );
    assert_eq!(sub10.blocking.len(), 1);
    assert_eq!(sub10.blocking[0].number, 11);
    assert_eq!(
        sub10.closing_prs,
        vec![PullRequest {
            number: 42,
            state: PullRequestState::Merged,
            head_ref_name: "issue-10".to_owned(),
            url: "https://github.com/octo/demo/pull/42".to_owned(),
        }]
    );

    // Sub-issue #11: open, blocked by #10 (in epic) and #99 (outside epic).
    let sub11 = &epic.sub_issues[1];
    assert_eq!(sub11.number, 11);
    assert_eq!(sub11.state, IssueState::Open);
    assert_eq!(
        sub11.blocked_by,
        vec![
            IssueRef {
                number: 10,
                title: "Read Epic data".to_owned(),
                state: IssueState::Closed(Some(ClosedReason::Completed)),
                url: "https://github.com/octo/demo/issues/10".to_owned(),
            },
            IssueRef {
                number: 99,
                title: "External dependency outside epic".to_owned(),
                state: IssueState::Open,
                url: "https://github.com/octo/demo/issues/99".to_owned(),
            },
        ]
    );

    // Sub-issue #12: closed not planned.
    let sub12 = &epic.sub_issues[2];
    assert_eq!(sub12.number, 12);
    assert_eq!(
        sub12.state,
        IssueState::Closed(Some(ClosedReason::NotPlanned))
    );
}

#[test]
fn distinguishes_all_pull_request_states() {
    let response = r#"{
  "data": {
    "repository": {
      "issue": {
        "number": 5,
        "title": "PR states",
        "state": "OPEN",
        "stateReason": null,
        "url": "https://github.com/octo/demo/issues/5",
        "parent": {
          "number": 1,
          "title": "Root",
          "state": "OPEN",
          "stateReason": null,
          "url": "https://github.com/octo/demo/issues/1"
        },
        "blockedBy": { "nodes": [] },
        "blocking": { "nodes": [] },
        "closedByPullRequestsReferences": {
          "nodes": [
            { "number": 101, "state": "OPEN", "isDraft": true, "headRefName": "feat-draft", "url": "https://github.com/octo/demo/pull/101" },
            { "number": 102, "state": "OPEN", "isDraft": false, "headRefName": "feat-open", "url": "https://github.com/octo/demo/pull/102" },
            { "number": 103, "state": "MERGED", "isDraft": false, "headRefName": "feat-merged", "url": "https://github.com/octo/demo/pull/103" },
            { "number": 104, "state": "CLOSED", "isDraft": false, "headRefName": "feat-closed", "url": "https://github.com/octo/demo/pull/104" }
          ]
        },
        "subIssues": {
          "pageInfo": { "hasNextPage": false, "endCursor": null },
          "nodes": []
        }
      }
    }
  }
}"#;

    let script = format!(
        "#!/bin/sh\n\
         if [ \"$1\" = \"auth\" ] && [ \"$2\" = \"status\" ]; then exit 0; fi\n\
         cat <<'EOF'\n\
{response}\n\
EOF\n"
    );
    let (_dir, github) = fake_gh(&script);
    let repo = Repository::parse("octo/demo").unwrap();

    let epic = github.fetch_epic(&repo, 5).unwrap();
    assert_eq!(epic.issue.parent.as_ref().unwrap().number, 1);
    let states: Vec<(u64, PullRequestState, &str)> = epic
        .issue
        .closing_prs
        .iter()
        .map(|pr| (pr.number, pr.state, pr.head_ref_name.as_str()))
        .collect();
    assert_eq!(
        states,
        vec![
            (101, PullRequestState::Draft, "feat-draft"),
            (102, PullRequestState::Open, "feat-open"),
            (103, PullRequestState::Merged, "feat-merged"),
            (104, PullRequestState::Closed, "feat-closed"),
        ]
    );
}

#[test]
fn paginates_when_epic_has_more_than_100_sub_issues() {
    let dir = TempDir::new().unwrap();
    let page1_path = dir.path().join("page1.json");
    let page2_path = dir.path().join("page2.json");

    let make_nodes = |start: u64, count: u64| -> String {
        (start..start + count)
            .map(|n| {
                format!(
                    r#"{{"number":{n},"title":"Task {n}","state":"OPEN","stateReason":null,"url":"https://github.com/octo/demo/issues/{n}","blockedBy":{{"nodes":[]}},"blocking":{{"nodes":[]}},"closedByPullRequestsReferences":{{"nodes":[]}}}}"#
                )
            })
            .collect::<Vec<_>>()
            .join(",")
    };

    let page1 = format!(
        r#"{{"data":{{"repository":{{"issue":{{"number":1,"title":"Big Epic","state":"OPEN","stateReason":null,"url":"https://github.com/octo/demo/issues/1","parent":null,"blockedBy":{{"nodes":[]}},"blocking":{{"nodes":[]}},"closedByPullRequestsReferences":{{"nodes":[]}},"subIssues":{{"pageInfo":{{"hasNextPage":true,"endCursor":"cursor-page-1"}},"nodes":[{}]}}}}}}}}}}"#,
        make_nodes(101, 100)
    );
    let page2 = format!(
        r#"{{"data":{{"repository":{{"issue":{{"number":1,"title":"Big Epic","state":"OPEN","stateReason":null,"url":"https://github.com/octo/demo/issues/1","parent":null,"blockedBy":{{"nodes":[]}},"blocking":{{"nodes":[]}},"closedByPullRequestsReferences":{{"nodes":[]}},"subIssues":{{"pageInfo":{{"hasNextPage":false,"endCursor":null}},"nodes":[{}]}}}}}}}}}}"#,
        make_nodes(201, 25)
    );

    std::fs::write(&page1_path, page1).unwrap();
    std::fs::write(&page2_path, page2).unwrap();

    let script = format!(
        "#!/bin/sh\n\
         if [ \"$1\" = \"auth\" ] && [ \"$2\" = \"status\" ]; then exit 0; fi\n\
         for arg in \"$@\"; do\n\
           if [ \"$arg\" = \"after=cursor-page-1\" ]; then\n\
             cat \"{p2}\"\n\
             exit 0\n\
           fi\n\
         done\n\
         cat \"{p1}\"\n",
        p1 = page1_path.display(),
        p2 = page2_path.display(),
    );
    let (_gh_dir, github) = fake_gh(&script);
    let repo = Repository::parse("octo/demo").unwrap();

    let epic = github.fetch_epic(&repo, 1).unwrap();
    assert_eq!(epic.sub_issues.len(), 125);
    assert_eq!(epic.sub_issues.first().unwrap().number, 101);
    assert_eq!(epic.sub_issues.last().unwrap().number, 225);
}

#[test]
fn returns_not_logged_in_when_gh_auth_status_fails() {
    let script = "#!/bin/sh\n\
                  if [ \"$1\" = \"auth\" ] && [ \"$2\" = \"status\" ]; then\n\
                    echo \"You are not logged into any GitHub hosts. Run gh auth login to authenticate.\" >&2\n\
                    exit 1\n\
                  fi\n\
                  echo \"should not reach api\" >&2\n\
                  exit 1\n";
    let (_dir, github) = fake_gh(script);
    let repo = Repository::parse("octo/demo").unwrap();

    let error = github.fetch_epic(&repo, 1).unwrap_err();
    assert!(
        matches!(error, GithubError::NotLoggedIn(_)),
        "expected NotLoggedIn, got: {error:?}"
    );
}

#[test]
fn returns_gh_not_found_when_gh_is_missing_from_path() {
    let empty_dir = TempDir::new().unwrap();
    let github = Github {
        path: Some(empty_dir.path().as_os_str().to_owned()),
        ..Github::default()
    };
    let repo = Repository {
        owner: "octo".to_owned(),
        name: "demo".to_owned(),
    };

    let error = github.fetch_epic(&repo, 1).unwrap_err();
    assert_eq!(error, GithubError::GhNotFound);
}

#[test]
fn returns_network_error_when_gh_cannot_connect() {
    let script = "#!/bin/sh\n\
                  if [ \"$1\" = \"auth\" ] && [ \"$2\" = \"status\" ]; then exit 0; fi\n\
                  echo \"error connecting to api.github.com: dial tcp: lookup api.github.com: no such host\" >&2\n\
                  exit 1\n";
    let (_dir, github) = fake_gh(script);
    let repo = Repository::parse("octo/demo").unwrap();

    let error = github.fetch_epic(&repo, 1).unwrap_err();
    assert!(
        matches!(error, GithubError::Network(_)),
        "expected Network, got: {error:?}"
    );
}

#[test]
fn returns_epic_not_found_when_issue_does_not_exist() {
    let script = r#"#!/bin/sh
if [ "$1" = "auth" ] && [ "$2" = "status" ]; then exit 0; fi
cat <<'EOF'
{"data":{"repository":{"issue":null}},"errors":[{"type":"NOT_FOUND","message":"Could not resolve to an Issue with the number of 9999."}]}
EOF
exit 1
"#;
    let (_dir, github) = fake_gh(script);
    let repo = Repository::parse("octo/demo").unwrap();

    let error = github.fetch_epic(&repo, 9999).unwrap_err();
    assert_eq!(
        error,
        GithubError::EpicNotFound {
            repository: repo,
            number: 9999,
        }
    );
}

#[test]
fn unresponsive_gh_times_out_and_reaps_children() {
    let dir = TempDir::new().unwrap();
    let pid_file = dir.path().join("gh.pids");
    let script = format!(
        "#!/bin/sh\n\
         if [ \"$1\" = \"auth\" ] && [ \"$2\" = \"status\" ]; then exit 0; fi\n\
         sleep 30 &\n\
         child=$!\n\
         printf '%s\\n%s\\n' \"$$\" \"$child\" > \"{tmp}\"\n\
         mv \"{tmp}\" \"{pid}\"\n\
         wait \"$child\"\n",
        tmp = format!("{}.tmp", pid_file.display()),
        pid = pid_file.display(),
    );
    let (_gh_dir, mut github) = fake_gh(&script);
    github.timeout = Duration::from_millis(300);
    let repo = Repository::parse("octo/demo").unwrap();

    let start = Instant::now();
    let error = github.fetch_epic(&repo, 1).unwrap_err();
    let elapsed = start.elapsed();

    assert!(
        matches!(error, GithubError::TimedOut(_)),
        "expected TimedOut, got: {error:?}"
    );
    assert!(
        elapsed < Duration::from_secs(3),
        "timeout took too long: {elapsed:?}"
    );
    #[cfg(unix)]
    assert_all_pids_reaped(&pid_file);
}

#[test]
fn oversized_stdout_aborts_and_reaps_children() {
    let dir = TempDir::new().unwrap();
    let pid_file = dir.path().join("huge.pids");
    let script = format!(
        "#!/bin/sh\n\
         if [ \"$1\" = \"auth\" ] && [ \"$2\" = \"status\" ]; then exit 0; fi\n\
         yes \"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\" &\n\
         child=$!\n\
         printf '%s\\n%s\\n' \"$$\" \"$child\" > \"{tmp}\"\n\
         mv \"{tmp}\" \"{pid}\"\n\
         wait \"$child\"\n",
        tmp = format!("{}.tmp", pid_file.display()),
        pid = pid_file.display(),
    );
    let (_gh_dir, mut github) = fake_gh(&script);
    github.output_limit = 4096;
    github.timeout = Duration::from_secs(5);
    let repo = Repository::parse("octo/demo").unwrap();

    let start = Instant::now();
    let error = github.fetch_epic(&repo, 1).unwrap_err();
    let elapsed = start.elapsed();

    assert!(
        matches!(error, GithubError::OutputTooLarge(4096)),
        "expected OutputTooLarge(4096), got: {error:?}"
    );
    assert!(
        elapsed < Duration::from_secs(3),
        "output limit abort took too long: {elapsed:?}"
    );
    #[cfg(unix)]
    if pid_file.exists() {
        assert_all_pids_reaped(&pid_file);
    }
}

#[test]
fn detects_github_repository_from_checkout_remotes() {
    let github = Github::default();

    let https_repo = init_git_repo(Some("https://github.com/octo/demo.git"));
    assert_eq!(
        github.repository_from_dir(https_repo.path()).unwrap(),
        Repository {
            owner: "octo".to_owned(),
            name: "demo".to_owned(),
        }
    );

    let ssh_repo = init_git_repo(Some("git@github.com:octo/demo.git"));
    assert_eq!(
        github.repository_from_dir(ssh_repo.path()).unwrap(),
        Repository {
            owner: "octo".to_owned(),
            name: "demo".to_owned(),
        }
    );

    let gitlab_repo = init_git_repo(Some("https://gitlab.com/octo/demo.git"));
    let err = github.repository_from_dir(gitlab_repo.path()).unwrap_err();
    assert!(
        matches!(err, GithubError::NotGithubRepository(_)),
        "expected NotGithubRepository, got: {err:?}"
    );

    let no_remote_repo = init_git_repo(None);
    let err = github
        .repository_from_dir(no_remote_repo.path())
        .unwrap_err();
    assert!(
        matches!(err, GithubError::NotGithubRepository(_)),
        "expected NotGithubRepository, got: {err:?}"
    );

    let not_a_repo = TempDir::new().unwrap();
    let err = github.repository_from_dir(not_a_repo.path()).unwrap_err();
    assert!(
        matches!(err, GithubError::NotGithubRepository(_)),
        "expected NotGithubRepository, got: {err:?}"
    );

    // Ensure PathBuf import stays used if needed or test a nonexistent path.
    let missing: PathBuf = not_a_repo.path().join("does-not-exist");
    let err = github.repository_from_dir(&missing).unwrap_err();
    assert!(
        matches!(err, GithubError::NotGithubRepository(_)),
        "expected NotGithubRepository, got: {err:?}"
    );
}

#[test]
fn finds_the_same_repository_pr_and_refreshes_it_by_number() {
    let (_dir, github) = fake_gh(
        r#"#!/bin/sh
case "$2" in
  list)
    printf '%s' '[{"number":4,"state":"OPEN","isDraft":true,"headRefName":"task","url":"https://github.com/octo/demo/pull/4","isCrossRepository":false},{"number":5,"state":"OPEN","headRefName":"task","isCrossRepository":true}]'
    ;;
  view)
    test "$3" = 4 || exit 1
    printf '%s' '{"number":4,"state":"MERGED","headRefName":"task","url":"https://github.com/octo/demo/pull/4","isCrossRepository":false}'
    ;;
  *) exit 1 ;;
esac
"#,
    );
    let repo = Repository::parse("octo/demo").unwrap();
    let pr = github
        .task_pull_request(&repo, "task", "main", None)
        .unwrap()
        .unwrap();
    assert_eq!((pr.number, pr.state), (4, PullRequestState::Draft));
    let updated = github
        .task_pull_request(&repo, "task", "main", Some(pr.number))
        .unwrap()
        .unwrap();
    assert_eq!(
        (updated.number, updated.state),
        (4, PullRequestState::Merged)
    );
}

#[test]
fn reads_repository_issues_with_parent_links_dependencies_and_standalone_issues() {
    let response = r#"{
  "data": {
    "repository": {
      "issues": {
        "pageInfo": { "hasNextPage": false, "endCursor": null },
        "nodes": [
          {
            "number": 1,
            "title": "Epic: Auth",
            "state": "OPEN",
            "stateReason": null,
            "url": "https://github.com/octo/demo/issues/1",
            "parent": null,
            "blockedBy": { "nodes": [] },
            "blocking": { "nodes": [] },
            "closedByPullRequestsReferences": { "nodes": [] }
          },
          {
            "number": 2,
            "title": "Add token refresh",
            "state": "OPEN",
            "stateReason": null,
            "url": "https://github.com/octo/demo/issues/2",
            "parent": {
              "number": 1,
              "title": "Epic: Auth",
              "state": "OPEN",
              "stateReason": null,
              "url": "https://github.com/octo/demo/issues/1"
            },
            "blockedBy": { "nodes": [] },
            "blocking": {
              "nodes": [
                {
                  "number": 3,
                  "title": "Retry on expired token",
                  "state": "OPEN",
                  "stateReason": null,
                  "url": "https://github.com/octo/demo/issues/3"
                }
              ]
            },
            "closedByPullRequestsReferences": {
              "nodes": [
                {
                  "number": 50,
                  "state": "OPEN",
                  "isDraft": true,
                  "headRefName": "issue-2",
                  "url": "https://github.com/octo/demo/pull/50"
                }
              ]
            }
          },
          {
            "number": 3,
            "title": "Retry on expired token",
            "state": "OPEN",
            "stateReason": null,
            "url": "https://github.com/octo/demo/issues/3",
            "parent": {
              "number": 1,
              "title": "Epic: Auth",
              "state": "OPEN",
              "stateReason": null,
              "url": "https://github.com/octo/demo/issues/1"
            },
            "blockedBy": {
              "nodes": [
                {
                  "number": 2,
                  "title": "Add token refresh",
                  "state": "OPEN",
                  "stateReason": null,
                  "url": "https://github.com/octo/demo/issues/2"
                }
              ]
            },
            "blocking": { "nodes": [] },
            "closedByPullRequestsReferences": { "nodes": [] }
          },
          {
            "number": 4,
            "title": "Standalone bug fix",
            "state": "CLOSED",
            "stateReason": "COMPLETED",
            "url": "https://github.com/octo/demo/issues/4",
            "parent": null,
            "blockedBy": { "nodes": [] },
            "blocking": { "nodes": [] },
            "closedByPullRequestsReferences": { "nodes": [] }
          }
        ]
      }
    }
  }
}"#;

    let script = format!(
        "#!/bin/sh\n\
         if [ \"$1\" = \"auth\" ] && [ \"$2\" = \"status\" ]; then exit 0; fi\n\
         if [ \"$1\" = \"api\" ] && [ \"$2\" = \"graphql\" ]; then\n\
           cat <<'EOF'\n\
{response}\n\
EOF\n\
           exit 0\n\
         fi\n\
         echo \"unexpected args: $*\" >&2\n\
         exit 1\n"
    );
    let (_dir, github) = fake_gh(&script);
    let repo = Repository::parse("octo/demo").unwrap();

    let result = github.fetch_repository_issues(&repo).unwrap();
    assert_eq!(result.repository, repo);
    assert_eq!(result.issues.len(), 4);
    assert_eq!(result.issues[0].number, 1);
    assert!(result.issues[0].parent.is_none());
    assert_eq!(result.issues[1].parent.as_ref().unwrap().number, 1);
    assert_eq!(result.issues[1].blocking[0].number, 3);
    assert_eq!(
        result.issues[1].closing_prs[0].state,
        PullRequestState::Draft
    );
    assert_eq!(result.issues[2].blocked_by[0].number, 2);
    assert_eq!(
        result.issues[3].state,
        IssueState::Closed(Some(ClosedReason::Completed))
    );
}

#[test]
fn paginates_when_repository_has_more_than_100_issues() {
    let dir = TempDir::new().unwrap();
    let page1_path = dir.path().join("repo_page1.json");
    let page2_path = dir.path().join("repo_page2.json");

    let make_nodes = |start: u64, count: u64| -> String {
        (start..start + count)
            .map(|n| {
                format!(
                    r#"{{"number":{n},"title":"Issue {n}","state":"OPEN","stateReason":null,"url":"https://github.com/octo/demo/issues/{n}","parent":null,"blockedBy":{{"nodes":[]}},"blocking":{{"nodes":[]}},"closedByPullRequestsReferences":{{"nodes":[]}}}}"#
                )
            })
            .collect::<Vec<_>>()
            .join(",")
    };

    let page1 = format!(
        r#"{{"data":{{"repository":{{"issues":{{"pageInfo":{{"hasNextPage":true,"endCursor":"repo-cursor-1"}},"nodes":[{}]}}}}}}}}"#,
        make_nodes(1, 100)
    );
    let page2 = format!(
        r#"{{"data":{{"repository":{{"issues":{{"pageInfo":{{"hasNextPage":false,"endCursor":null}},"nodes":[{}]}}}}}}}}"#,
        make_nodes(101, 15)
    );

    std::fs::write(&page1_path, page1).unwrap();
    std::fs::write(&page2_path, page2).unwrap();

    let script = format!(
        "#!/bin/sh\n\
         if [ \"$1\" = \"auth\" ] && [ \"$2\" = \"status\" ]; then exit 0; fi\n\
         for arg in \"$@\"; do\n\
           if [ \"$arg\" = \"after=repo-cursor-1\" ]; then\n\
             cat \"{p2}\"\n\
             exit 0\n\
           fi\n\
         done\n\
         cat \"{p1}\"\n",
        p1 = page1_path.display(),
        p2 = page2_path.display(),
    );
    let (_gh_dir, github) = fake_gh(&script);
    let repo = Repository::parse("octo/demo").unwrap();

    let result = github.fetch_repository_issues(&repo).unwrap();
    assert_eq!(result.issues.len(), 115);
    assert_eq!(result.issues.first().unwrap().number, 1);
    assert_eq!(result.issues.last().unwrap().number, 115);
}

#[test]
fn returns_repository_not_found_when_repository_does_not_exist() {
    let script = r#"#!/bin/sh
if [ "$1" = "auth" ] && [ "$2" = "status" ]; then exit 0; fi
cat <<'EOF'
{"data":{"repository":null},"errors":[{"type":"NOT_FOUND","message":"Could not resolve to a Repository with the name 'octo/missing'."}]}
EOF
exit 1
"#;
    let (_dir, github) = fake_gh(script);
    let repo = Repository::parse("octo/missing").unwrap();

    let error = github.fetch_repository_issues(&repo).unwrap_err();
    assert_eq!(error, GithubError::RepositoryNotFound { repository: repo });
}

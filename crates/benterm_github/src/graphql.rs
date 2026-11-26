//! Read-only GraphQL queries and response interpretation via `gh`.

use std::process::{Command, Output};

use serde::Deserialize;

use crate::process::{apply_env, run_bounded, said};
use crate::{
    ClosedReason, Epic, Github, GithubError, Issue, IssueRef, IssueState, PullRequest,
    PullRequestState, Repository, RepositoryIssues,
};

/// Read-only GraphQL query for an Epic issue, its direct sub-issues (paginated),
/// parent, dependencies (`blockedBy`, `blocking`), and closing pull requests.
const EPIC_QUERY: &str = r#"
query($owner: String!, $name: String!, $number: Int!, $after: String) {
  repository(owner: $owner, name: $name) {
    issue(number: $number) {
      number
      title
      state
      stateReason
      url
      parent {
        number
        title
        state
        stateReason
        url
      }
      blockedBy(first: 100) {
        nodes {
          number
          title
          state
          stateReason
          url
        }
      }
      blocking(first: 100) {
        nodes {
          number
          title
          state
          stateReason
          url
        }
      }
      closedByPullRequestsReferences(first: 100, includeClosedPrs: true) {
        nodes {
          number
          state
          isDraft
          headRefName
          url
        }
      }
      subIssues(first: 100, after: $after) {
        pageInfo {
          hasNextPage
          endCursor
        }
        nodes {
          number
          title
          state
          stateReason
          url
          parent {
            number
            title
            state
            stateReason
            url
          }
          blockedBy(first: 100) {
            nodes {
              number
              title
              state
              stateReason
              url
            }
          }
          blocking(first: 100) {
            nodes {
              number
              title
              state
              stateReason
              url
            }
          }
          closedByPullRequestsReferences(first: 100, includeClosedPrs: true) {
            nodes {
              number
              state
              isDraft
              headRefName
              url
            }
          }
        }
      }
    }
  }
}
"#;

/// Read-only GraphQL query for repository issues (paginated), including
/// parent links, dependencies (`blockedBy`, `blocking`), and closing pull requests.
const REPOSITORY_ISSUES_QUERY: &str = r#"
query($owner: String!, $name: String!, $after: String) {
  repository(owner: $owner, name: $name) {
    issues(first: 100, after: $after, orderBy: {field: CREATED_AT, direction: ASC}) {
      pageInfo {
        hasNextPage
        endCursor
      }
      nodes {
        number
        title
        state
        stateReason
        url
        parent {
          number
          title
          state
          stateReason
          url
        }
        blockedBy(first: 100) {
          nodes {
            number
            title
            state
            stateReason
            url
          }
        }
        blocking(first: 100) {
          nodes {
            number
            title
            state
            stateReason
            url
          }
        }
        closedByPullRequestsReferences(first: 100, includeClosedPrs: true) {
          nodes {
            number
            state
            isDraft
            headRefName
            url
          }
        }
      }
    }
  }
}
"#;

/// Checks `gh auth status` to verify that `gh` is installed and logged in.
pub(crate) fn check_auth(github: &Github) -> Result<(), GithubError> {
    let output = run_gh(github, &["auth", "status"])?;
    if output.status.success() {
        return Ok(());
    }
    let message = said("gh", &output);
    if looks_like_network_error(&message) {
        return Err(GithubError::Network(message));
    }
    Err(GithubError::NotLoggedIn(message))
}

/// Fetches an Epic and all of its direct sub-issues across GraphQL pages.
pub(crate) fn fetch_epic(
    github: &Github,
    repository: &Repository,
    epic_number: u64,
) -> Result<Epic, GithubError> {
    check_auth(github)?;

    let mut cursor: Option<String> = None;
    let mut epic_issue: Option<Issue> = None;
    let mut sub_issues: Vec<Issue> = Vec::new();

    for _page in 0..github.max_pages {
        let output = run_graphql_page(github, repository, epic_number, cursor.as_deref())?;
        let parsed = parse_graphql_output(&output, repository, Some(epic_number))?;

        let raw_issue = parsed
            .data
            .and_then(|d| d.repository)
            .and_then(|r| r.issue)
            .ok_or_else(|| GithubError::EpicNotFound {
                repository: repository.clone(),
                number: epic_number,
            })?;

        if epic_issue.is_none() {
            epic_issue = Some(convert_epic_issue(&raw_issue)?);
        }

        let sub_connection = raw_issue.sub_issues.unwrap_or_default();
        for node in sub_connection.nodes.into_iter().flatten() {
            sub_issues.push(convert_sub_issue(node)?);
        }

        let page_info = sub_connection.page_info.unwrap_or_default();
        if page_info.has_next_page {
            match page_info.end_cursor {
                Some(next) if !next.is_empty() => cursor = Some(next),
                _ => {
                    return Err(GithubError::InvalidResponse(
                        "subIssues hasNextPage is true without an endCursor".to_owned(),
                    ));
                }
            }
        } else {
            return Ok(Epic {
                repository: repository.clone(),
                issue: epic_issue.expect("epic_issue set on first page"),
                sub_issues,
            });
        }
    }

    Err(GithubError::InvalidResponse(format!(
        "subIssues pagination exceeded limit of {} pages",
        github.max_pages
    )))
}

/// Fetches all issues in a repository across GraphQL pages.
pub(crate) fn fetch_repository_issues(
    github: &Github,
    repository: &Repository,
) -> Result<RepositoryIssues, GithubError> {
    check_auth(github)?;

    let mut cursor: Option<String> = None;
    let mut issues: Vec<Issue> = Vec::new();

    for _page in 0..github.max_pages {
        let output = run_repository_issues_page(github, repository, cursor.as_deref())?;
        let parsed = parse_graphql_output(&output, repository, None)?;

        let raw_repo = parsed.data.and_then(|d| d.repository).ok_or_else(|| {
            GithubError::RepositoryNotFound {
                repository: repository.clone(),
            }
        })?;

        let connection = raw_repo.issues.unwrap_or_default();
        for node in connection.nodes.into_iter().flatten() {
            issues.push(convert_sub_issue(node)?);
        }

        let page_info = connection.page_info.unwrap_or_default();
        if page_info.has_next_page {
            match page_info.end_cursor {
                Some(next) if !next.is_empty() => cursor = Some(next),
                _ => {
                    return Err(GithubError::InvalidResponse(
                        "issues hasNextPage is true without an endCursor".to_owned(),
                    ));
                }
            }
        } else {
            return Ok(RepositoryIssues {
                repository: repository.clone(),
                issues,
            });
        }
    }

    Err(GithubError::InvalidResponse(format!(
        "issues pagination exceeded limit of {} pages",
        github.max_pages
    )))
}

fn run_repository_issues_page(
    github: &Github,
    repository: &Repository,
    after: Option<&str>,
) -> Result<Output, GithubError> {
    let owner_arg = format!("owner={}", repository.owner);
    let name_arg = format!("name={}", repository.name);
    let query_arg = format!("query={REPOSITORY_ISSUES_QUERY}");
    let after_arg = after.map(|cursor| format!("after={cursor}"));

    let mut args = vec![
        "api",
        "graphql",
        "-H",
        "GraphQL-Features: sub_issues",
        "-f",
        &owner_arg,
        "-f",
        &name_arg,
        "-f",
        &query_arg,
    ];
    if let Some(after_arg) = after_arg.as_deref() {
        args.push("-f");
        args.push(after_arg);
    }

    run_gh(github, &args)
}

fn run_graphql_page(
    github: &Github,
    repository: &Repository,
    epic_number: u64,
    after: Option<&str>,
) -> Result<Output, GithubError> {
    let owner_arg = format!("owner={}", repository.owner);
    let name_arg = format!("name={}", repository.name);
    let number_arg = format!("number={epic_number}");
    let query_arg = format!("query={EPIC_QUERY}");
    let after_arg = after.map(|cursor| format!("after={cursor}"));

    let mut args = vec![
        "api",
        "graphql",
        "-H",
        "GraphQL-Features: sub_issues",
        "-f",
        &owner_arg,
        "-f",
        &name_arg,
        "-F",
        &number_arg,
        "-f",
        &query_arg,
    ];
    if let Some(after_arg) = after_arg.as_deref() {
        args.push("-f");
        args.push(after_arg);
    }

    run_gh(github, &args)
}

pub(crate) fn task_pull_request(
    github: &Github,
    repository: &Repository,
    branch: &str,
    base: &str,
    number: Option<u64>,
) -> Result<Option<PullRequest>, GithubError> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Candidate {
        #[serde(flatten)]
        pull: RawPullRequest,
        is_cross_repository: bool,
    }

    let repo = repository.to_string();
    let fields = "number,state,isDraft,headRefName,url,isCrossRepository";
    let output = match number {
        Some(number) => run_gh(
            github,
            &[
                "pr",
                "view",
                &number.to_string(),
                "--repo",
                &repo,
                "--json",
                fields,
            ],
        ),
        None => run_gh(
            github,
            &[
                "pr", "list", "--repo", &repo, "--state", "all", "--head", branch, "--base", base,
                "--json", fields,
            ],
        ),
    }?;
    if !output.status.success() {
        return Err(GithubError::CommandFailed(said("gh", &output)));
    }
    let invalid = |error: serde_json::Error| GithubError::InvalidResponse(error.to_string());
    let candidates = if number.is_some() {
        vec![serde_json::from_slice(&output.stdout).map_err(invalid)?]
    } else {
        serde_json::from_slice::<Vec<Candidate>>(&output.stdout).map_err(invalid)?
    };
    let mut matching = candidates.into_iter().filter(|candidate| {
        !candidate.is_cross_repository && candidate.pull.head_ref_name == branch
    });
    match (matching.next(), matching.next()) {
        (Some(candidate), None) => convert_pr(candidate.pull).map(Some),
        _ => Ok(None),
    }
}

fn run_gh(github: &Github, arguments: &[&str]) -> Result<Output, GithubError> {
    let mut command = Command::new("gh");
    command.args(arguments);
    apply_env(&mut command, github.path.as_deref(), &github.env);

    run_bounded("gh", &mut command, github.timeout, github.output_limit)
        .map_err(|err| err.into_github_error("gh"))
}

fn not_found_error(repository: &Repository, epic_number: Option<u64>) -> GithubError {
    match epic_number {
        Some(number) => GithubError::EpicNotFound {
            repository: repository.clone(),
            number,
        },
        None => GithubError::RepositoryNotFound {
            repository: repository.clone(),
        },
    }
}

fn parse_graphql_output(
    output: &Output,
    repository: &Repository,
    epic_number: Option<u64>,
) -> Result<GraphQlResponse, GithubError> {
    let stderr_msg = said("gh", output);

    if !output.status.success() {
        // `gh api graphql` often writes the GraphQL JSON error body to stdout
        // while exiting non-zero (for example on NOT_FOUND).
        if let Ok(parsed) = serde_json::from_slice::<GraphQlResponse>(&output.stdout)
            && let Some(err) = classify_graphql_errors(&parsed.errors, repository, epic_number)
        {
            return Err(err);
        }
        if looks_like_auth_error(&stderr_msg) {
            return Err(GithubError::NotLoggedIn(stderr_msg));
        }
        if looks_like_network_error(&stderr_msg) {
            return Err(GithubError::Network(stderr_msg));
        }
        if looks_like_not_found(&stderr_msg) {
            return Err(not_found_error(repository, epic_number));
        }
        return Err(GithubError::CommandFailed(stderr_msg));
    }

    let parsed: GraphQlResponse = serde_json::from_slice(&output.stdout)
        .map_err(|error| GithubError::InvalidResponse(format!("malformed JSON: {error}")))?;

    if let Some(err) = classify_graphql_errors(&parsed.errors, repository, epic_number) {
        return Err(err);
    }

    Ok(parsed)
}

fn classify_graphql_errors(
    errors: &[GraphQlError],
    repository: &Repository,
    epic_number: Option<u64>,
) -> Option<GithubError> {
    if errors.is_empty() {
        return None;
    }
    for error in errors {
        let kind = error.kind.as_deref().unwrap_or("");
        let msg = error.message.as_str();
        if kind.eq_ignore_ascii_case("NOT_FOUND") || looks_like_not_found(msg) {
            return Some(not_found_error(repository, epic_number));
        }
        if looks_like_auth_error(msg) {
            return Some(GithubError::NotLoggedIn(msg.to_owned()));
        }
    }
    let joined = errors
        .iter()
        .map(|e| e.message.as_str())
        .collect::<Vec<_>>()
        .join("; ");
    Some(GithubError::CommandFailed(joined))
}

fn looks_like_network_error(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    [
        "error connecting to",
        "dial tcp",
        "no such host",
        "could not resolve host",
        "temporary failure in name resolution",
        "connection refused",
        "connection reset",
        "network is unreachable",
        "host is unreachable",
        "i/o timeout",
        "tls handshake",
        "failed to connect",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

fn looks_like_auth_error(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    [
        "not logged into",
        "gh auth login",
        "bad credentials",
        "http 401",
        "requires authentication",
        "not authenticated",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

fn looks_like_not_found(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("could not resolve to an issue")
        || lower.contains("could not resolve to a repository")
}

fn parse_issue_state(state: &str, reason: Option<&str>) -> Result<IssueState, GithubError> {
    match state {
        "OPEN" => Ok(IssueState::Open),
        "CLOSED" => {
            let closed_reason = match reason {
                Some("COMPLETED") => Some(ClosedReason::Completed),
                Some("NOT_PLANNED") => Some(ClosedReason::NotPlanned),
                Some("DUPLICATE") => Some(ClosedReason::Duplicate),
                _ => None,
            };
            Ok(IssueState::Closed(closed_reason))
        }
        other => Err(GithubError::InvalidResponse(format!(
            "unknown issue state {other:?}"
        ))),
    }
}

fn parse_pr_state(state: &str, is_draft: bool) -> Result<PullRequestState, GithubError> {
    match state {
        "OPEN" if is_draft => Ok(PullRequestState::Draft),
        "OPEN" => Ok(PullRequestState::Open),
        "MERGED" => Ok(PullRequestState::Merged),
        "CLOSED" => Ok(PullRequestState::Closed),
        other => Err(GithubError::InvalidResponse(format!(
            "unknown pull request state {other:?}"
        ))),
    }
}

fn convert_issue_ref(raw: RawIssueRef) -> Result<IssueRef, GithubError> {
    Ok(IssueRef {
        number: raw.number,
        title: raw.title,
        state: parse_issue_state(&raw.state, raw.state_reason.as_deref())?,
        url: raw.url,
    })
}

fn convert_pr(raw: RawPullRequest) -> Result<PullRequest, GithubError> {
    Ok(PullRequest {
        number: raw.number,
        state: parse_pr_state(&raw.state, raw.is_draft)?,
        head_ref_name: raw.head_ref_name,
        url: raw.url,
    })
}

fn convert_epic_issue(raw: &RawEpicIssue) -> Result<Issue, GithubError> {
    Ok(Issue {
        number: raw.number,
        title: raw.title.clone(),
        state: parse_issue_state(&raw.state, raw.state_reason.as_deref())?,
        url: raw.url.clone(),
        parent: raw.parent.clone().map(convert_issue_ref).transpose()?,
        blocked_by: raw
            .blocked_by
            .nodes
            .iter()
            .flatten()
            .cloned()
            .map(convert_issue_ref)
            .collect::<Result<Vec<_>, _>>()?,
        blocking: raw
            .blocking
            .nodes
            .iter()
            .flatten()
            .cloned()
            .map(convert_issue_ref)
            .collect::<Result<Vec<_>, _>>()?,
        closing_prs: raw
            .closed_by_prs
            .nodes
            .iter()
            .flatten()
            .cloned()
            .map(convert_pr)
            .collect::<Result<Vec<_>, _>>()?,
    })
}

fn convert_sub_issue(raw: RawSubIssue) -> Result<Issue, GithubError> {
    Ok(Issue {
        number: raw.number,
        title: raw.title,
        state: parse_issue_state(&raw.state, raw.state_reason.as_deref())?,
        url: raw.url,
        parent: raw.parent.map(convert_issue_ref).transpose()?,
        blocked_by: raw
            .blocked_by
            .nodes
            .into_iter()
            .flatten()
            .map(convert_issue_ref)
            .collect::<Result<Vec<_>, _>>()?,
        blocking: raw
            .blocking
            .nodes
            .into_iter()
            .flatten()
            .map(convert_issue_ref)
            .collect::<Result<Vec<_>, _>>()?,
        closing_prs: raw
            .closed_by_prs
            .nodes
            .into_iter()
            .flatten()
            .map(convert_pr)
            .collect::<Result<Vec<_>, _>>()?,
    })
}

#[derive(Deserialize)]
struct GraphQlResponse {
    data: Option<GraphQlData>,
    #[serde(default)]
    errors: Vec<GraphQlError>,
}

#[derive(Deserialize)]
struct GraphQlError {
    #[serde(rename = "type")]
    kind: Option<String>,
    message: String,
}

#[derive(Deserialize)]
struct GraphQlData {
    repository: Option<RawRepository>,
}

#[derive(Deserialize)]
struct RawRepository {
    issue: Option<RawEpicIssue>,
    issues: Option<SubIssuesConnection>,
}

#[derive(Deserialize)]
struct RawEpicIssue {
    number: u64,
    title: String,
    state: String,
    #[serde(rename = "stateReason")]
    state_reason: Option<String>,
    #[serde(default)]
    url: String,
    parent: Option<RawIssueRef>,
    #[serde(rename = "blockedBy", default)]
    blocked_by: NodesConnection<RawIssueRef>,
    #[serde(default)]
    blocking: NodesConnection<RawIssueRef>,
    #[serde(rename = "closedByPullRequestsReferences", default)]
    closed_by_prs: NodesConnection<RawPullRequest>,
    #[serde(rename = "subIssues")]
    sub_issues: Option<SubIssuesConnection>,
}

#[derive(Deserialize)]
struct RawSubIssue {
    number: u64,
    title: String,
    state: String,
    #[serde(rename = "stateReason")]
    state_reason: Option<String>,
    #[serde(default)]
    url: String,
    parent: Option<RawIssueRef>,
    #[serde(rename = "blockedBy", default)]
    blocked_by: NodesConnection<RawIssueRef>,
    #[serde(default)]
    blocking: NodesConnection<RawIssueRef>,
    #[serde(rename = "closedByPullRequestsReferences", default)]
    closed_by_prs: NodesConnection<RawPullRequest>,
}

#[derive(Clone, Deserialize)]
struct RawIssueRef {
    number: u64,
    title: String,
    state: String,
    #[serde(rename = "stateReason")]
    state_reason: Option<String>,
    #[serde(default)]
    url: String,
}

#[derive(Clone, Deserialize)]
struct RawPullRequest {
    number: u64,
    state: String,
    #[serde(rename = "isDraft", default)]
    is_draft: bool,
    #[serde(rename = "headRefName", default)]
    head_ref_name: String,
    #[serde(default)]
    url: String,
}

#[derive(Deserialize)]
#[serde(bound(deserialize = "T: Deserialize<'de>"))]
struct NodesConnection<T> {
    #[serde(default = "Vec::new")]
    nodes: Vec<Option<T>>,
}

impl<T> Default for NodesConnection<T> {
    fn default() -> Self {
        Self { nodes: Vec::new() }
    }
}

#[derive(Default, Deserialize)]
struct SubIssuesConnection {
    #[serde(rename = "pageInfo")]
    page_info: Option<PageInfo>,
    #[serde(default)]
    nodes: Vec<Option<RawSubIssue>>,
}

#[derive(Default, Deserialize)]
struct PageInfo {
    #[serde(rename = "hasNextPage", default)]
    has_next_page: bool,
    #[serde(rename = "endCursor")]
    end_cursor: Option<String>,
}

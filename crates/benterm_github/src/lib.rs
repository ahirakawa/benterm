//! Reads GitHub epics, dependencies, and pull requests through read-only GraphQL
//! queries using the existing `gh` login.

mod graphql;
mod process;
mod remote;

use std::ffi::OsString;
use std::fmt;
use std::path::Path;
use std::time::Duration;

/// Default timeout for each `gh` or `git` subprocess invocation.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// Default maximum stdout bytes read from a single `gh` or `git` invocation.
pub const DEFAULT_OUTPUT_LIMIT: usize = 4 << 20;

/// Default maximum number of GraphQL pages fetched for an Epic's sub-issues.
pub const DEFAULT_MAX_PAGES: usize = 50;

/// A GitHub repository identified by `owner/name`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Repository {
    pub owner: String,
    pub name: String,
}

impl Repository {
    /// Parses a repository slug in `owner/name` form.
    pub fn parse(slug: &str) -> Result<Self, GithubError> {
        remote::parse_slug(slug)
    }

    /// Parses a GitHub `owner/name` from a git remote URL.
    pub fn from_remote_url(url: &str) -> Result<Self, GithubError> {
        remote::parse_remote_url(url)
    }
}

impl fmt::Display for Repository {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.owner, self.name)
    }
}

/// Reason a GitHub issue was closed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClosedReason {
    Completed,
    NotPlanned,
    Duplicate,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IssueState {
    Open,
    Closed(Option<ClosedReason>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PullRequestState {
    Draft,
    Open,
    Merged,
    Closed,
}

/// Lightweight reference to an issue (used for `parent`, `blockedBy`, and `blocking`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IssueRef {
    pub number: u64,
    pub title: String,
    pub state: IssueState,
    pub url: String,
}

/// A pull request that closes an issue.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PullRequest {
    pub number: u64,
    pub state: PullRequestState,
    pub head_ref_name: String,
    pub url: String,
}

/// An issue with its dependencies and closing pull requests.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Issue {
    pub number: u64,
    pub title: String,
    pub state: IssueState,
    pub url: String,
    pub parent: Option<IssueRef>,
    pub blocked_by: Vec<IssueRef>,
    pub blocking: Vec<IssueRef>,
    pub closing_prs: Vec<PullRequest>,
}

/// An Epic issue together with all of its direct sub-issues.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Epic {
    pub repository: Repository,
    pub issue: Issue,
    pub sub_issues: Vec<Issue>,
}

/// Issues fetched from a GitHub repository (not limited to a single Epic).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepositoryIssues {
    pub repository: Repository,
    pub issues: Vec<Issue>,
}

/// Why a GitHub read failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GithubError {
    /// The `gh` executable could not be found in `PATH`.
    GhNotFound,
    /// `gh` is not logged into GitHub.
    NotLoggedIn(String),
    /// Could not reach GitHub over the network.
    Network(String),
    /// The requested Epic issue (or repository) was not found.
    EpicNotFound { repository: Repository, number: u64 },
    /// The requested repository was not found.
    RepositoryNotFound { repository: Repository },
    /// A subprocess exceeded its timeout and was stopped.
    TimedOut(Duration),
    /// A subprocess wrote more bytes to stdout than allowed.
    OutputTooLarge(usize),
    /// The directory is not a checkout of a GitHub repository.
    NotGithubRepository(String),
    /// `gh` returned data that could not be interpreted.
    InvalidResponse(String),
    /// Any other subprocess failure.
    CommandFailed(String),
}

impl fmt::Display for GithubError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::GhNotFound => f.write_str("gh could not be found in PATH"),
            Self::NotLoggedIn(msg) => write!(f, "not logged in to GitHub: {msg}"),
            Self::Network(msg) => write!(f, "could not connect to GitHub: {msg}"),
            Self::EpicNotFound { repository, number } => {
                write!(f, "epic #{number} was not found in {repository}")
            }
            Self::RepositoryNotFound { repository } => {
                write!(f, "repository {repository} was not found")
            }
            Self::TimedOut(timeout) => {
                write!(f, "gh timed out after {}", format_duration(*timeout))
            }
            Self::OutputTooLarge(limit) => {
                write!(f, "gh output exceeded limit of {}", format_bytes(*limit))
            }
            Self::NotGithubRepository(msg) => write!(f, "not a GitHub repository: {msg}"),
            Self::InvalidResponse(msg) => write!(f, "invalid response from gh: {msg}"),
            Self::CommandFailed(msg) => f.write_str(msg),
        }
    }
}

impl std::error::Error for GithubError {}

fn format_duration(duration: Duration) -> String {
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

/// Configuration for invoking `gh` and `git`.
#[derive(Clone, Debug)]
pub struct Github {
    /// `PATH` used to locate `gh` and `git`. When `None`, uses the process's `PATH`.
    pub path: Option<OsString>,
    /// Additional environment variables passed to `gh`.
    pub env: Vec<(OsString, OsString)>,
    /// Maximum duration each subprocess invocation may run.
    pub timeout: Duration,
    /// Maximum stdout bytes read per subprocess invocation.
    pub output_limit: usize,
    /// Maximum number of `subIssues` pages to fetch.
    pub max_pages: usize,
}

impl Default for Github {
    fn default() -> Self {
        Self {
            path: None,
            env: Vec::new(),
            timeout: DEFAULT_TIMEOUT,
            output_limit: DEFAULT_OUTPUT_LIMIT,
            max_pages: DEFAULT_MAX_PAGES,
        }
    }
}

impl Github {
    /// Determines the GitHub `owner/name` repository from a local git checkout directory.
    pub fn repository_from_dir(&self, directory: &Path) -> Result<Repository, GithubError> {
        remote::repository_from_dir(self, directory)
    }

    /// Finds an unambiguous same-repository PR, or refreshes a known PR by number.
    pub fn task_pull_request(
        &self,
        repository: &Repository,
        branch: &str,
        base: &str,
        number: Option<u64>,
    ) -> Result<Option<PullRequest>, GithubError> {
        graphql::task_pull_request(self, repository, branch, base, number)
    }

    /// Checks that `gh` is installed and authenticated.
    pub fn check_auth(&self) -> Result<(), GithubError> {
        graphql::check_auth(self)
    }

    /// Reads an Epic issue, all of its direct sub-issues (paginated),
    /// dependencies (`blockedBy` / `blocking`), and closing pull requests.
    pub fn fetch_epic(
        &self,
        repository: &Repository,
        epic_number: u64,
    ) -> Result<Epic, GithubError> {
        graphql::fetch_epic(self, repository, epic_number)
    }

    /// Reads issues in a repository (paginated), including parent links,
    /// dependencies (`blockedBy` / `blocking`), and closing pull requests.
    pub fn fetch_repository_issues(
        &self,
        repository: &Repository,
    ) -> Result<RepositoryIssues, GithubError> {
        graphql::fetch_repository_issues(self, repository)
    }
}

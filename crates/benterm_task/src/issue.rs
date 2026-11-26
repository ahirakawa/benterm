//! Matching tasks to GitHub issues by branch name.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use crate::store::Task;

/// A repository identified by its owner and name (such as on GitHub).
///
/// Stored in ASCII lowercase so identifiers for the same repository compare
/// equal regardless of casing.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RepoId {
    pub owner: String,
    pub name: String,
}

impl RepoId {
    /// Builds a normalised repository identifier, returning `None` if either
    /// part is empty after trimming.
    pub fn new(owner: &str, name: &str) -> Option<Self> {
        let owner = owner.trim();
        let name = name.trim();
        if owner.is_empty() || name.is_empty() || owner.contains('/') || name.contains('/') {
            return None;
        }
        Some(Self {
            owner: owner.to_ascii_lowercase(),
            name: name.to_ascii_lowercase(),
        })
    }

    /// Extracts `owner/name` from a git remote URL such as
    /// `https://github.com/owner/repo.git`, `ssh://git@github.com/owner/repo.git`,
    /// or `git@github.com:owner/repo.git`.
    pub fn parse_remote_url(url: &str) -> Option<Self> {
        let url = url.trim();
        let path = if let Some((_, rest)) = url.split_once("://") {
            let (_, path) = rest.split_once('/')?;
            path
        } else if let Some((host, path)) = url.split_once(':') {
            if host.is_empty() || host.contains('/') {
                return None;
            }
            path
        } else {
            return None;
        };
        let path = path.trim_matches('/');
        let path = path.strip_suffix(".git").unwrap_or(path);
        let (owner, name) = path.split_once('/')?;
        Self::new(owner, name)
    }
}

/// Tasks associated with one issue, separated into open and closed, each
/// ordered newest first.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IssueTasks {
    /// Open tasks (`Task::closed` is `None`), newest first.
    pub open: Vec<Task>,
    /// Closed tasks (`Task::closed` is `Some`), newest first.
    pub closed: Vec<Task>,
}

/// Extracts the issue number from a branch name when `issue-<number>` appears
/// delimited on both sides by non-alphanumeric characters (or string ends).
pub fn issue_number_in_branch(branch: &str) -> Option<u64> {
    const PREFIX: &str = "issue-";
    let mut search_from = 0;
    while let Some(rel) = branch[search_from..].find(PREFIX) {
        let start = search_from + rel;
        let digits_start = start + PREFIX.len();
        search_from = digits_start;

        let before_ok = branch[..start]
            .chars()
            .next_back()
            .is_none_or(|c| !c.is_alphanumeric());
        if !before_ok {
            continue;
        }

        let digits_len = branch[digits_start..]
            .bytes()
            .take_while(u8::is_ascii_digit)
            .count();
        if digits_len == 0 {
            continue;
        }

        let digits_end = digits_start + digits_len;
        let after_ok = branch[digits_end..]
            .chars()
            .next()
            .is_none_or(|c| !c.is_alphanumeric());
        if !after_ok {
            continue;
        }

        if let Ok(number) = branch[digits_start..digits_end].parse::<u64>()
            && number > 0
        {
            return Some(number);
        }
    }
    None
}

/// The default branch name for a task created from an issue in the graph:
/// `issue-<number>`.
pub fn default_issue_branch(issue: u64) -> String {
    format!("issue-{issue}")
}

/// Picks a branch name for `issue` starting with `issue-<number>`, appending
/// `-2`, `-3`, ... when that name is already present in `existing`.
pub fn issue_branch_for<'a>(issue: u64, existing: impl IntoIterator<Item = &'a str>) -> String {
    let taken: HashSet<&str> = existing.into_iter().collect();
    let base = default_issue_branch(issue);
    if !taken.contains(base.as_str()) {
        return base;
    }
    for suffix in 2u64.. {
        let candidate = format!("{base}-{suffix}");
        if !taken.contains(candidate.as_str()) {
            return candidate;
        }
    }
    unreachable!("u64 suffix space cannot be exhausted by in-memory branch names")
}

fn sort_newest_first(tasks: &mut [Task]) {
    tasks.sort_by(|a, b| {
        b.created_at_ms
            .cmp(&a.created_at_ms)
            .then_with(|| b.id.cmp(&a.id))
    });
}

/// Groups `tasks` by the issue number in their branch name, keeping only tasks
/// whose `repository` satisfies `same_repository`.
pub fn tasks_by_issue(
    tasks: &[Task],
    mut same_repository: impl FnMut(&Path) -> bool,
) -> BTreeMap<u64, IssueTasks> {
    let mut grouped: BTreeMap<u64, IssueTasks> = BTreeMap::new();
    for task in tasks {
        let Some(issue) = issue_number_in_branch(&task.branch) else {
            continue;
        };
        if !same_repository(&task.repository) {
            continue;
        }
        let entry = grouped.entry(issue).or_default();
        if task.closed.is_some() {
            entry.closed.push(task.clone());
        } else {
            entry.open.push(task.clone());
        }
    }
    for entry in grouped.values_mut() {
        sort_newest_first(&mut entry.open);
        sort_newest_first(&mut entry.closed);
    }
    grouped
}

/// Returns the open and closed tasks matching `issue` whose `repository`
/// satisfies `same_repository`, each ordered newest first.
pub fn tasks_for_issue(
    tasks: &[Task],
    issue: u64,
    mut same_repository: impl FnMut(&Path) -> bool,
) -> IssueTasks {
    let mut matched = IssueTasks::default();
    for task in tasks {
        if issue_number_in_branch(&task.branch) != Some(issue) {
            continue;
        }
        if !same_repository(&task.repository) {
            continue;
        }
        if task.closed.is_some() {
            matched.closed.push(task.clone());
        } else {
            matched.open.push(task.clone());
        }
    }
    sort_newest_first(&mut matched.open);
    sort_newest_first(&mut matched.closed);
    matched
}

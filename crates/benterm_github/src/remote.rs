//! GitHub repository (`owner/name`) parsing and detection from a local checkout.

use std::path::Path;
use std::process::Command;

use crate::process::{apply_env, run_bounded};
use crate::{Github, GithubError, Repository};

/// Git environment variables that redirect git to another repository.
const REPOSITORY_VARIABLES: [&str; 7] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_COMMON_DIR",
    "GIT_OBJECT_DIRECTORY",
    "GIT_NAMESPACE",
    "GIT_PREFIX",
];

fn valid_segment(segment: &str) -> bool {
    !segment.is_empty()
        && segment != "."
        && segment != ".."
        && !segment.chars().any(char::is_whitespace)
        && !segment.contains('/')
        && !segment.contains('\\')
}

/// Parses a repository slug in `owner/name` form.
pub(crate) fn parse_slug(slug: &str) -> Result<Repository, GithubError> {
    let trimmed = slug.trim();
    let Some((owner, name)) = trimmed.split_once('/') else {
        return Err(GithubError::NotGithubRepository(format!(
            "{slug:?} is not in owner/name format"
        )));
    };
    if !valid_segment(owner) || !valid_segment(name) {
        return Err(GithubError::NotGithubRepository(format!(
            "{slug:?} is not a valid GitHub repository"
        )));
    }
    Ok(Repository {
        owner: owner.to_owned(),
        name: name.to_owned(),
    })
}

/// Extracts a GitHub `owner/name` from a git remote URL.
///
/// Supports HTTPS, HTTP, `git://`, `ssh://`, and SCP-like `git@github.com:owner/repo` URLs.
pub(crate) fn parse_remote_url(url: &str) -> Result<Repository, GithubError> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return Err(GithubError::NotGithubRepository(
            "remote URL is empty".to_owned(),
        ));
    }

    let path = if let Some(rest) = strip_scheme(trimmed) {
        // Authority + path: `[user@]host[:port]/owner/repo[.git]`
        let Some((authority, path)) = rest.split_once('/') else {
            return Err(GithubError::NotGithubRepository(format!(
                "remote URL {trimmed:?} has no repository path"
            )));
        };
        let host = extract_host(authority);
        if !host.eq_ignore_ascii_case("github.com") {
            return Err(GithubError::NotGithubRepository(format!(
                "remote host {host:?} is not github.com"
            )));
        }
        path
    } else if let Some((authority, path)) = trimmed.split_once(':') {
        // SCP-like syntax: `[user@]github.com:owner/repo[.git]`
        // Reject Windows drive letters or local paths containing `/`.
        if authority.contains('/') || authority.len() <= 1 {
            return Err(GithubError::NotGithubRepository(format!(
                "remote URL {trimmed:?} is not a GitHub URL"
            )));
        }
        let host = authority
            .rsplit_once('@')
            .map_or(authority, |(_, host)| host);
        if !host.eq_ignore_ascii_case("github.com") {
            return Err(GithubError::NotGithubRepository(format!(
                "remote host {host:?} is not github.com"
            )));
        }
        path
    } else {
        return Err(GithubError::NotGithubRepository(format!(
            "remote URL {trimmed:?} is not a GitHub URL"
        )));
    };

    let cleaned = path.trim_matches('/');
    let cleaned = cleaned.strip_suffix(".git").unwrap_or(cleaned);
    let cleaned = cleaned.trim_end_matches('/');

    let mut parts = cleaned.split('/');
    let owner = parts.next().unwrap_or("");
    let name = parts.next().unwrap_or("");
    if parts.next().is_some() || !valid_segment(owner) || !valid_segment(name) {
        return Err(GithubError::NotGithubRepository(format!(
            "remote URL {trimmed:?} does not name a GitHub owner/repository"
        )));
    }

    Ok(Repository {
        owner: owner.to_owned(),
        name: name.to_owned(),
    })
}

fn strip_scheme(url: &str) -> Option<&str> {
    for scheme in ["https://", "http://", "ssh://", "git://"] {
        if url.len() >= scheme.len() && url[..scheme.len()].eq_ignore_ascii_case(scheme) {
            return Some(&url[scheme.len()..]);
        }
    }
    None
}

fn extract_host(authority: &str) -> &str {
    let without_user = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    without_user
        .split_once(':')
        .map_or(without_user, |(host, _)| host)
}

/// Determines the GitHub `owner/name` repository from a local git checkout directory.
pub(crate) fn repository_from_dir(
    github: &Github,
    directory: &Path,
) -> Result<Repository, GithubError> {
    if !directory.exists() {
        return Err(GithubError::NotGithubRepository(format!(
            "directory {} does not exist",
            directory.display()
        )));
    }

    let check = run_git(github, directory, &["rev-parse", "--is-inside-work-tree"])?;
    if !check.status.success() {
        return Err(GithubError::NotGithubRepository(format!(
            "{} is not inside a git repository",
            directory.display()
        )));
    }

    let remote = run_git(github, directory, &["remote", "get-url", "origin"])?;
    if !remote.status.success() {
        return Err(GithubError::NotGithubRepository(
            "git repository has no 'origin' remote".to_owned(),
        ));
    }

    let url = String::from_utf8(remote.stdout).map_err(|_| {
        GithubError::NotGithubRepository("origin remote URL is not UTF-8".to_owned())
    })?;
    parse_remote_url(url.trim())
}

fn run_git(
    github: &Github,
    directory: &Path,
    arguments: &[&str],
) -> Result<std::process::Output, GithubError> {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(directory)
        .args(arguments)
        .env("GIT_TERMINAL_PROMPT", "0");
    for variable in REPOSITORY_VARIABLES {
        command.env_remove(variable);
    }
    apply_env(&mut command, github.path.as_deref(), &[]);

    run_bounded("git", &mut command, github.timeout, github.output_limit)
        .map_err(|err| err.into_github_error("git"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_valid_github_remote_urls() {
        let expected = Repository {
            owner: "octo".to_owned(),
            name: "benterm".to_owned(),
        };
        for url in [
            "https://github.com/octo/benterm.git",
            "https://github.com/octo/benterm",
            "https://github.com/octo/benterm/",
            "https://user@github.com/octo/benterm.git",
            "git@github.com:octo/benterm.git",
            "git@github.com:octo/benterm",
            "ssh://git@github.com/octo/benterm.git",
            "ssh://git@github.com:22/octo/benterm.git",
            "git://github.com/octo/benterm.git",
            "HTTPS://GITHUB.COM/octo/benterm.git",
        ] {
            assert_eq!(parse_remote_url(url).unwrap(), expected, "url: {url}");
        }
    }

    #[test]
    fn rejects_non_github_remote_urls() {
        for url in [
            "",
            "https://gitlab.com/octo/benterm.git",
            "git@bitbucket.org:octo/benterm.git",
            "https://github.com/octo",
            "https://github.com/octo/benterm/extra",
            "/tmp/local/repo.git",
        ] {
            assert!(
                matches!(
                    parse_remote_url(url),
                    Err(GithubError::NotGithubRepository(_))
                ),
                "expected rejection for {url:?}"
            );
        }
    }
}

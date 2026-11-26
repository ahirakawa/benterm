//! A git repository in a temporary directory, for the tests.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;

pub struct Repo {
    pub dir: tempfile::TempDir,
    /// The checkout, with one commit on `main`.
    pub path: PathBuf,
    /// Where the worktrees of the tasks go.
    pub root: PathBuf,
}

/// Runs git in `dir` without the user's settings, and gives what it
/// printed.
pub fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

pub fn commit(dir: &Path, file: &str, text: &str) -> String {
    std::fs::write(dir.join(file), text).unwrap();
    git(dir, &["add", file]);
    git(dir, &["commit", "-q", "-m", file]);
    git(dir, &["rev-parse", "HEAD"])
}

pub fn repo() -> Repo {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().canonicalize().unwrap();
    let path = base.join("benterm");
    std::fs::create_dir(&path).unwrap();
    git(&path, &["init", "-q", "-b", "main"]);
    commit(&path, "README.md", "one\n");
    Repo {
        root: base.join("worktrees"),
        path,
        dir,
    }
}

pub fn branches(dir: &Path) -> Vec<String> {
    git(dir, &["branch", "--format=%(refname:short)"])
        .lines()
        .map(str::to_owned)
        .collect()
}

pub fn worktrees(dir: &Path) -> usize {
    git(dir, &["worktree", "list"]).lines().count()
}

//! The newest change of the files in an agent's directory, for telling
//! whether the agent has stalled.
//!
//! A look runs on a background thread. In a git checkout, git lists the
//! files, so what `.gitignore` leaves out, as build output, does not
//! count, nor does anything in `.git`. Elsewhere the directory is walked,
//! leaving out every `.git`. The directories the files are in count too:
//! a file removed or renamed shows only in them.
//!
//! The times of at most [`STAT_LIMIT`] paths are read in one look. A
//! directory with more paths is read a part at a time, the next look
//! going on where the last one stopped, so a change there is seen within
//! a few looks. A directory walked without git is listed up to
//! [`LIST_LIMIT`] paths.

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use benterm_task::Git;

/// The most paths whose times one look reads.
pub(crate) const STAT_LIMIT: usize = 20_000;
/// The most paths a walk without git lists.
pub(crate) const LIST_LIMIT: usize = 200_000;

/// What one look found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FileLook {
    /// The newest time a file or a directory was changed, of those read.
    pub(crate) newest: Option<SystemTime>,
    /// Where the next look starts in the list of paths.
    pub(crate) next: usize,
}

/// Looks at the paths of `directory` from the `from`th on, wrapping
/// around, with git found on `git_path`, or on the app's own `PATH`.
pub(crate) fn look(directory: &Path, git_path: Option<&OsStr>, from: usize) -> FileLook {
    look_limited(directory, git_path, from, STAT_LIMIT)
}

fn look_limited(directory: &Path, git_path: Option<&OsStr>, from: usize, limit: usize) -> FileLook {
    let paths = git_paths(directory, git_path).unwrap_or_else(|| walked_paths(directory));
    let count = paths.len().min(limit);
    if count == 0 {
        return FileLook {
            newest: None,
            next: 0,
        };
    }
    let start = from % paths.len();
    let newest = (0..count)
        .filter_map(|n| {
            let path = &paths[(start + n) % paths.len()];
            // A link counts as itself, not as what it points to.
            fs::symlink_metadata(path).ok()?.modified().ok()
        })
        .max();
    FileLook {
        newest,
        next: (start + count) % paths.len(),
    }
}

/// The directory, the files git lists in it that it does not ignore,
/// and the directories they are in. `None` when git cannot list them,
/// as outside a checkout.
fn git_paths(directory: &Path, git_path: Option<&OsStr>) -> Option<Vec<PathBuf>> {
    let git = Git {
        path: git_path.map(OsStr::to_owned),
        timeout: Duration::from_millis(750),
        ..Git::default()
    };
    let output = git.list_worktree_files(directory).ok()?;
    let files: Vec<&Path> = output
        .split(|byte| *byte == 0)
        .filter(|name| !name.is_empty())
        .map(|name| Path::new(OsStr::from_bytes(name)))
        .collect();
    let mut folders = BTreeSet::new();
    for file in &files {
        let mut folder = file.parent();
        while let Some(path) = folder.filter(|path| !path.as_os_str().is_empty()) {
            if !folders.insert(path) {
                break;
            }
            folder = path.parent();
        }
    }
    let mut paths = Vec::with_capacity(1 + folders.len() + files.len());
    paths.push(directory.to_path_buf());
    paths.extend(folders.into_iter().map(|folder| directory.join(folder)));
    paths.extend(files.into_iter().map(|file| directory.join(file)));
    Some(paths)
}

/// The directory and what is below it, leaving out every `.git`, up to
/// [`LIST_LIMIT`] paths, in the same order every time.
fn walked_paths(directory: &Path) -> Vec<PathBuf> {
    let mut paths = vec![directory.to_path_buf()];
    let mut folders = vec![directory.to_path_buf()];
    while let Some(folder) = folders.pop() {
        let Ok(entries) = fs::read_dir(&folder) else {
            continue;
        };
        let mut entries: Vec<_> = entries
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name() != ".git")
            .collect();
        entries.sort_by_key(fs::DirEntry::file_name);
        for entry in entries {
            if paths.len() >= LIST_LIMIT {
                return paths;
            }
            let path = entry.path();
            if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                folders.push(path.clone());
            }
            paths.push(path);
        }
    }
    paths
}

#[cfg(test)]
mod tests {
    use std::fs::File;
    use std::process::Command;
    use std::time::{Duration, Instant};

    use super::*;

    fn at(seconds: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000 + seconds)
    }

    fn set_time(path: &Path, time: SystemTime) {
        File::open(path).unwrap().set_modified(time).unwrap();
    }

    /// Sets every path below `root`, and `root`, to `time`.
    fn set_all(root: &Path, time: SystemTime) {
        for entry in fs::read_dir(root).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                set_all(&path, time);
            } else {
                set_time(&path, time);
            }
        }
        set_time(root, time);
    }

    fn git(directory: &Path, arguments: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(directory)
            .args(arguments)
            .status()
            .unwrap();
        assert!(status.success(), "git {arguments:?}");
    }

    #[test]
    fn the_newest_file_of_a_directory_without_git_is_found() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("a.txt"), "a").unwrap();
        fs::write(dir.path().join("src/b.rs"), "b").unwrap();
        set_all(dir.path(), at(0));
        set_time(&dir.path().join("src/b.rs"), at(30));

        let found = look(dir.path(), None, 0);
        assert_eq!(found.newest, Some(at(30)));
    }

    #[test]
    fn what_git_ignores_and_what_is_in_dot_git_do_not_count() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        git(root, &["init", "-q"]);
        fs::write(root.join(".gitignore"), "target/\n").unwrap();
        fs::create_dir(root.join("target")).unwrap();
        fs::write(root.join("target/out.o"), "o").unwrap();
        fs::write(root.join("tracked.rs"), "t").unwrap();
        git(root, &["add", ".gitignore", "tracked.rs"]);
        fs::write(root.join("untracked.rs"), "u").unwrap();
        set_all(root, at(0));
        set_time(&root.join("untracked.rs"), at(10));
        // Newer than anything that counts.
        set_time(&root.join("target/out.o"), at(50));
        set_time(&root.join("target"), at(50));
        set_time(&root.join(".git/index"), at(50));
        set_time(&root.join(".git"), at(50));

        assert_eq!(look(root, None, 0).newest, Some(at(10)));
        set_time(&root.join("tracked.rs"), at(20));
        assert_eq!(look(root, None, 0).newest, Some(at(20)));
    }

    #[test]
    fn a_removed_file_shows_in_its_directory() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("src/gone.rs"), "g").unwrap();
        fs::write(dir.path().join("src/kept.rs"), "k").unwrap();
        set_all(dir.path(), at(0));

        fs::remove_file(dir.path().join("src/gone.rs")).unwrap();
        let found = look(dir.path(), None, 0).newest.unwrap();
        assert!(found > at(0), "{found:?}");
    }

    #[test]
    fn a_directory_with_more_paths_than_the_limit_is_read_a_part_at_a_time() {
        let dir = tempfile::tempdir().unwrap();
        for n in 0..10 {
            fs::write(dir.path().join(format!("{n}.txt")), "x").unwrap();
        }
        set_all(dir.path(), at(0));
        set_time(&dir.path().join("7.txt"), at(40));

        // Eleven paths with the directory: four looks of three go round.
        let mut from = 0;
        let mut newest = Vec::new();
        for _ in 0..4 {
            let found = look_limited(dir.path(), None, from, 3);
            newest.push(found.newest);
            from = found.next;
        }
        assert_eq!(newest.iter().filter(|n| **n == Some(at(40))).count(), 1);
        assert!(newest.iter().all(|n| n.is_some()));
        // The fourth look went round to the start.
        assert_eq!(from, 1);
    }

    #[test]
    fn a_missing_directory_has_no_change() {
        let dir = tempfile::tempdir().unwrap();
        let found = look(&dir.path().join("gone"), None, 0);
        assert_eq!(found.newest, None);
    }

    /// One look at a large checkout ends in a bounded time.
    #[test]
    fn a_look_at_ten_thousand_files_ends_within_two_seconds() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        git(root, &["init", "-q"]);
        for d in 0..100 {
            let sub = root.join(format!("d{d}"));
            fs::create_dir(&sub).unwrap();
            for f in 0..100 {
                fs::write(sub.join(format!("f{f}.rs")), "x").unwrap();
            }
        }
        set_all(&root.join("d42"), at(0));
        let started = Instant::now();
        let found = look(root, None, 0);
        let took = started.elapsed();
        assert!(found.newest.is_some());
        assert!(took < Duration::from_secs(2), "took {took:?}");
    }
}

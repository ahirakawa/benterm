//! The diff of a task: what its worktree has now against the commit it
//! branched off from, read from a real git repository.

mod common;

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use benterm_task::{
    DIFF_OUTPUT_LIMIT, DiffLine, FileDiff, FileStatus, Git, LineKind, NEW_FILE_LIMIT,
    NEW_FILES_COUNT_LIMIT, NEW_FILES_TOTAL_LIMIT, NewTask, Task, TaskStore, create,
};
use common::{Repo, commit, git, repo};

fn task(repo: &Repo) -> Task {
    let store = TaskStore::open(Some(&repo.dir.path().join("state/tasks.json")));
    let new = NewTask {
        directory: repo.path.clone(),
        name: "kanban".to_owned(),
        branch: None,
        role: None,
        root: repo.root.clone(),
    };
    create(&Git::default(), &store, new).unwrap()
}

fn diff(task: &Task) -> Vec<FileDiff> {
    Git::default()
        .diff(&task.worktree, &task.base_commit)
        .unwrap()
        .files
}

fn file<'a>(files: &'a [FileDiff], path: &str) -> &'a FileDiff {
    files
        .iter()
        .find(|file| file.path == path)
        .unwrap_or_else(|| panic!("no {path} in {files:#?}"))
}

/// The lines of every hunk of `file`, as `+text`, `-text` and ` text`.
fn lines(file: &FileDiff) -> Vec<String> {
    let mark = |line: &DiffLine| match line.kind {
        LineKind::Added => '+',
        LineKind::Removed => '-',
        LineKind::Context => ' ',
    };
    file.hunks
        .iter()
        .flat_map(|hunk| &hunk.lines)
        .map(|line| format!("{}{}", mark(line), line.text))
        .collect()
}

#[test]
fn committed_and_uncommitted_changes_are_one_diff() {
    let repo = repo();
    let task = task(&repo);
    commit(&task.worktree, "README.md", "one\ntwo\n");
    std::fs::write(task.worktree.join("README.md"), "one\ntwo\nthree\n").unwrap();

    let files = diff(&task);

    assert_eq!(files.len(), 1, "{files:#?}");
    let readme = file(&files, "README.md");
    assert_eq!(readme.status, FileStatus::Modified);
    assert_eq!(lines(readme), [" one", "+two", "+three"]);
    let numbers: Vec<_> = readme.hunks[0]
        .lines
        .iter()
        .map(|line| (line.old, line.new))
        .collect();
    assert_eq!(
        numbers,
        [(Some(1), Some(1)), (None, Some(2)), (None, Some(3))]
    );
}

#[test]
fn a_file_git_does_not_know_is_all_added() {
    let repo = repo();
    let task = task(&repo);
    std::fs::create_dir(task.worktree.join("src")).unwrap();
    std::fs::write(task.worktree.join("src/new.txt"), "a\nb\n").unwrap();

    let files = diff(&task);

    let new = file(&files, "src/new.txt");
    assert_eq!(new.status, FileStatus::Added);
    assert_eq!(lines(new), ["+a", "+b"]);
    let numbers: Vec<_> = new.hunks[0].lines.iter().map(|line| line.new).collect();
    assert_eq!(numbers, [Some(1), Some(2)]);
}

#[test]
fn a_file_git_ignores_is_left_out() {
    let repo = repo();
    let task = task(&repo);
    std::fs::write(task.worktree.join(".gitignore"), "build/\n").unwrap();
    std::fs::create_dir(task.worktree.join("build")).unwrap();
    std::fs::write(task.worktree.join("build/out"), "x\n").unwrap();

    let files = diff(&task);

    let paths: Vec<_> = files.iter().map(|file| file.path.as_str()).collect();
    assert_eq!(paths, [".gitignore"]);
}

#[test]
fn a_removed_file_is_all_removed() {
    let repo = repo();
    let task = task(&repo);
    std::fs::remove_file(task.worktree.join("README.md")).unwrap();

    let files = diff(&task);

    let readme = file(&files, "README.md");
    assert_eq!(readme.status, FileStatus::Deleted);
    assert_eq!(lines(readme), ["-one"]);
}

#[test]
fn what_the_base_branch_gets_later_is_not_in_the_diff() {
    let repo = repo();
    let task = task(&repo);
    commit(&repo.path, "later.txt", "later\n");
    commit(&repo.path, "README.md", "one\nmain\n");
    std::fs::write(task.worktree.join("mine.txt"), "mine\n").unwrap();

    let files = diff(&task);

    let paths: Vec<_> = files.iter().map(|file| file.path.as_str()).collect();
    assert_eq!(paths, ["mine.txt"]);
}

#[test]
fn a_binary_file_says_so_without_lines() {
    let repo = repo();
    let task = task(&repo);
    std::fs::write(task.worktree.join("tracked.bin"), b"a\0b").unwrap();
    git(&task.worktree, &["add", "tracked.bin"]);
    std::fs::write(task.worktree.join("new.bin"), b"\0\x01\x02").unwrap();

    let files = diff(&task);

    for path in ["tracked.bin", "new.bin"] {
        let binary = file(&files, path);
        assert!(binary.binary, "{binary:#?}");
        assert!(binary.hunks.is_empty(), "{binary:#?}");
        assert_eq!(binary.status, FileStatus::Added);
    }
}

#[test]
fn a_name_with_spaces_and_other_letters_is_read_as_it_is() {
    let repo = repo();
    let task = task(&repo);
    for name in ["a b.txt", "かんばん.md", "quote\"d.txt"] {
        std::fs::write(task.worktree.join(name), "x\n").unwrap();
    }
    commit(&task.worktree, "tab\there.txt", "y\n");

    let files = diff(&task);

    let mut paths: Vec<_> = files.iter().map(|file| file.path.as_str()).collect();
    paths.sort();
    assert_eq!(
        paths,
        ["a b.txt", "quote\"d.txt", "tab\there.txt", "かんばん.md"]
    );
}

#[test]
fn files_come_in_the_order_of_their_paths() {
    let repo = repo();
    let task = task(&repo);
    std::fs::write(task.worktree.join("z.txt"), "z\n").unwrap();
    commit(&task.worktree, "m.txt", "m\n");
    std::fs::write(task.worktree.join("README.md"), "one\nmore\n").unwrap();
    std::fs::write(task.worktree.join("a.txt"), "a\n").unwrap();

    let files = diff(&task);

    let paths: Vec<_> = files.iter().map(|file| file.path.as_str()).collect();
    assert_eq!(paths, ["README.md", "a.txt", "m.txt", "z.txt"]);
}

#[test]
fn a_worktree_without_changes_has_an_empty_diff() {
    let repo = repo();
    let task = task(&repo);
    assert_eq!(diff(&task), []);
}

#[test]
fn a_worktree_that_is_gone_is_an_error() {
    let repo = repo();
    let task = task(&repo);
    std::fs::remove_dir_all(&task.worktree).unwrap();
    assert!(
        Git::default()
            .diff(&task.worktree, &task.base_commit)
            .is_err()
    );
}

/// Every file under `dir` with the time it was last changed.
fn times(dir: &Path) -> BTreeMap<PathBuf, SystemTime> {
    let mut found = BTreeMap::new();
    let mut folders = vec![dir.to_owned()];
    while let Some(folder) = folders.pop() {
        for entry in std::fs::read_dir(&folder).unwrap() {
            let entry = entry.unwrap();
            let meta = entry.metadata().unwrap();
            if meta.is_dir() {
                folders.push(entry.path());
            }
            found.insert(entry.path(), meta.modified().unwrap());
        }
    }
    found
}

#[test]
fn reading_the_diff_writes_nothing_under_git() {
    let repo = repo();
    let task = task(&repo);
    commit(&task.worktree, "a.txt", "a\n");
    std::fs::write(task.worktree.join("new.txt"), "new\n").unwrap();
    // Changed after the index was written, so git would refresh the
    // index if it were let.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    std::fs::write(task.worktree.join("a.txt"), "a\n").unwrap();
    std::fs::write(task.worktree.join("README.md"), "one\nchanged\n").unwrap();
    let before = times(&repo.path.join(".git"));

    diff(&task);

    assert_eq!(times(&repo.path.join(".git")), before);
}

/// A fake `git` script on `PATH` that spawns a sleeping child, records both
/// PIDs into `pid_file`, and waits without exiting for several seconds.
fn hanging_git(dir: &Path, pid_file: &Path, close_output: bool, timeout: Duration) -> Git {
    let bin = dir.join("fake-git-bin");
    std::fs::create_dir_all(&bin).unwrap();
    let redirect = if close_output {
        "exec >/dev/null 2>&1\n"
    } else {
        ""
    };
    let script = format!(
        "#!/bin/sh\n\
         {redirect}\
         sleep 5 &\n\
         child=$!\n\
         printf '%s\\n%s\\n' \"$$\" \"$child\" > \"{tmp}\"\n\
         mv \"{tmp}\" \"{pid}\"\n\
         wait \"$child\"\n",
        tmp = format!("{}.tmp", pid_file.display()),
        pid = pid_file.display(),
    );
    let git_bin = bin.join("git");
    std::fs::write(&git_bin, script).unwrap();
    std::fs::set_permissions(&git_bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    Git {
        path: Some(format!("{}:/usr/bin:/bin", bin.display()).into()),
        restrict_programs: false,
        timeout,
        ..Git::default()
    }
}

fn pid_exists(pid: i32) -> bool {
    let ret = unsafe { libc::kill(pid, 0) };
    if ret == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
}

fn assert_all_pids_reaped(pid_file: &Path) {
    let text = std::fs::read_to_string(pid_file).unwrap_or_else(|e| {
        panic!(
            "fake git did not record pids to {}: {e}",
            pid_file.display()
        )
    });
    let pids: Vec<i32> = text
        .lines()
        .map(|line| line.trim().parse::<i32>().expect("numeric pid"))
        .collect();
    assert_eq!(pids.len(), 2, "expected shell and sleep pids: {pids:?}");
    // Direct child must already be reaped; grandchild is reaped by PID 1
    // shortly after SIGKILL to the process group.
    let deadline = Instant::now() + Duration::from_secs(1);
    while pids.iter().any(|&pid| pid_exists(pid)) {
        assert!(
            Instant::now() < deadline,
            "child processes survived timeout: {pids:?}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn a_hung_git_command_times_out_and_leaves_no_child_processes() {
    let repo = repo();
    let task = task(&repo);
    let pid_file = repo.dir.path().join("hung.pids");
    let git = hanging_git(
        repo.dir.path(),
        &pid_file,
        false,
        Duration::from_millis(300),
    );

    let start = Instant::now();
    let error = git.diff(&task.worktree, &task.base_commit).unwrap_err();
    let elapsed = start.elapsed();

    assert!(error.is_timed_out(), "expected timeout error, got: {error}");
    assert!(
        error.to_string().contains("timed out"),
        "error should explain timeout: {error}"
    );
    assert!(
        elapsed < Duration::from_secs(2),
        "git took too long to time out: {elapsed:?}"
    );
    assert_all_pids_reaped(&pid_file);

    // Timeout must also surface clearly from `Git::source` rather than
    // being replaced with "not in a git repository".
    let source_pids = repo.dir.path().join("source.pids");
    let source_git = hanging_git(
        repo.dir.path(),
        &source_pids,
        false,
        Duration::from_millis(300),
    );
    let source_error = source_git.source(&repo.path).unwrap_err();
    assert!(
        source_error.is_timed_out(),
        "expected timeout from source(), got: {source_error}"
    );
    assert_all_pids_reaped(&source_pids);
}

#[test]
fn listing_worktree_files_uses_the_bounded_runner() {
    let repo = repo();
    let pid_file = repo.dir.path().join("list.pids");
    let git = hanging_git(
        repo.dir.path(),
        &pid_file,
        false,
        Duration::from_millis(300),
    );
    let error = git.list_worktree_files(&repo.path).unwrap_err();
    assert!(error.is_timed_out(), "{error}");
    assert_all_pids_reaped(&pid_file);
}

#[test]
fn a_hung_git_that_closes_output_still_times_out() {
    let repo = repo();
    let task = task(&repo);
    let pid_file = repo.dir.path().join("closed.pids");
    let git = hanging_git(repo.dir.path(), &pid_file, true, Duration::from_millis(300));

    let start = Instant::now();
    let error = git.diff(&task.worktree, &task.base_commit).unwrap_err();
    let elapsed = start.elapsed();

    assert!(error.is_timed_out(), "expected timeout error, got: {error}");
    assert!(
        elapsed < Duration::from_secs(2),
        "git took too long to time out: {elapsed:?}"
    );
    assert_all_pids_reaped(&pid_file);
}

#[test]
fn a_diff_exceeding_the_output_limit_fails_as_too_large() {
    let repo = repo();
    let task = task(&repo);
    let line = "x".repeat(100) + "\n";
    let lines = (DIFF_OUTPUT_LIMIT / line.len()) + 1000;
    let body = line.repeat(lines);
    commit(&task.worktree, "big.txt", &body);

    let error = Git::default()
        .diff(&task.worktree, &task.base_commit)
        .unwrap_err();

    assert!(
        error.is_too_large(),
        "expected too-large error, got: {error}"
    );
    assert!(
        error.to_string().contains("too large"),
        "error should explain size limit: {error}"
    );
}

#[test]
fn infinite_stdout_aborts_immediately_at_limit_and_reaps_children() {
    let repo = repo();
    let task = task(&repo);
    let pid_file = repo.dir.path().join("yes.pids");
    let bin = repo.dir.path().join("yes-git-bin");
    std::fs::create_dir_all(&bin).unwrap();
    let script = format!(
        "#!/bin/sh\n\
         yes \"xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\" &\n\
         child=$!\n\
         printf '%s\\n%s\\n' \"$$\" \"$child\" > \"{tmp}\"\n\
         mv \"{tmp}\" \"{pid}\"\n\
         wait \"$child\"\n",
        tmp = format!("{}.tmp", pid_file.display()),
        pid = pid_file.display(),
    );
    let git_bin = bin.join("git");
    std::fs::write(&git_bin, script).unwrap();
    std::fs::set_permissions(&git_bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    let git = Git {
        path: Some(format!("{}:/usr/bin:/bin", bin.display()).into()),
        restrict_programs: false,
        // Long timeout so this only passes if the byte limit stops git immediately.
        timeout: Duration::from_secs(10),
        ..Git::default()
    };

    let start = Instant::now();
    let error = git.diff(&task.worktree, &task.base_commit).unwrap_err();
    let elapsed = start.elapsed();

    assert!(
        error.is_too_large(),
        "expected too-large error, got: {error}"
    );
    assert!(
        elapsed < Duration::from_secs(3),
        "expected output limit to abort quickly, took {elapsed:?}"
    );
    if pid_file.exists() {
        assert_all_pids_reaped(&pid_file);
    }

    let error = git.list_worktree_files(&repo.path).unwrap_err();
    assert!(error.is_too_large(), "{error}");
}

#[test]
fn infinite_stderr_still_respects_timeout_and_reaps_children() {
    let repo = repo();
    let task = task(&repo);
    let pid_file = repo.dir.path().join("stderr.pids");
    let bin = repo.dir.path().join("stderr-git-bin");
    std::fs::create_dir_all(&bin).unwrap();
    let script = format!(
        "#!/bin/sh\n\
         yes \"stderr noise\" >&2 &\n\
         child=$!\n\
         printf '%s\\n%s\\n' \"$$\" \"$child\" > \"{tmp}\"\n\
         mv \"{tmp}\" \"{pid}\"\n\
         wait \"$child\"\n",
        tmp = format!("{}.tmp", pid_file.display()),
        pid = pid_file.display(),
    );
    let git_bin = bin.join("git");
    std::fs::write(&git_bin, script).unwrap();
    std::fs::set_permissions(&git_bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    let git = Git {
        path: Some(format!("{}:/usr/bin:/bin", bin.display()).into()),
        restrict_programs: false,
        timeout: Duration::from_millis(300),
        ..Git::default()
    };

    let start = Instant::now();
    let error = git.diff(&task.worktree, &task.base_commit).unwrap_err();
    let elapsed = start.elapsed();

    assert!(error.is_timed_out(), "expected timeout error, got: {error}");
    assert!(
        elapsed < Duration::from_secs(2),
        "stderr noise delayed timeout: {elapsed:?}"
    );
    assert_all_pids_reaped(&pid_file);
}

#[test]
fn untracked_special_files_do_not_read_link_contents_or_block() {
    let repo = repo();
    let task = task(&repo);
    let target = repo.dir.path().join("outside.txt");
    std::fs::write(&target, "outside contents\n").unwrap();
    std::os::unix::fs::symlink(&target, task.worktree.join("link")).unwrap();
    let status = std::process::Command::new("mkfifo")
        .arg(task.worktree.join("pipe"))
        .status()
        .unwrap();
    assert!(status.success());

    let files = diff(&task);
    assert_eq!(
        lines(file(&files, "link")),
        [format!("+{}", target.display())]
    );
    assert!(
        files
            .iter()
            .filter(|file| file.path == "pipe")
            .all(|file| file.hunks.is_empty())
    );
}

#[test]
fn untracked_files_stop_at_the_count_limit() {
    let repo = repo();
    let task = task(&repo);
    std::fs::write(task.worktree.join("README.md"), "one\ntwo\n").unwrap();
    let total_untracked = NEW_FILES_COUNT_LIMIT + 25;
    for i in 0..total_untracked {
        std::fs::write(task.worktree.join(format!("u{i:04}.txt")), "x\n").unwrap();
    }

    let diff = Git::default()
        .diff(&task.worktree, &task.base_commit)
        .unwrap();

    assert!(diff.untracked_truncated);
    // 1 tracked file + NEW_FILES_COUNT_LIMIT untracked files.
    assert_eq!(diff.files.len(), 1 + NEW_FILES_COUNT_LIMIT);
    assert!(diff.files.iter().any(|f| f.path == "README.md"));
}

#[test]
fn untracked_files_stop_at_the_total_size_limit() {
    let repo = repo();
    let task = task(&repo);
    // Each file is 512 KiB (under the 1 MiB per-file limit), so 4 files fit
    // exactly inside the 2 MiB total limit and the 5th stops reading.
    let chunk_size = (NEW_FILES_TOTAL_LIMIT / 4) as usize;
    assert!((chunk_size as u64) <= NEW_FILE_LIMIT);
    let body = "a".repeat(chunk_size);
    for i in 0..5 {
        std::fs::write(task.worktree.join(format!("part{i}.txt")), &body).unwrap();
    }

    let diff = Git::default()
        .diff(&task.worktree, &task.base_commit)
        .unwrap();

    assert!(diff.untracked_truncated);
    assert_eq!(diff.files.len(), 4);
    assert!(
        diff.files
            .iter()
            .all(|f| !f.too_large && !f.hunks.is_empty())
    );
}

#[test]
fn a_single_oversized_new_file_still_uses_the_per_file_limit() {
    let repo = repo();
    let task = task(&repo);
    let huge = vec![b'a'; (NEW_FILE_LIMIT + 1) as usize];
    std::fs::write(task.worktree.join("a_huge.txt"), &huge).unwrap();
    std::fs::write(task.worktree.join("b_small.txt"), "hello\n").unwrap();

    let diff = Git::default()
        .diff(&task.worktree, &task.base_commit)
        .unwrap();

    assert!(!diff.untracked_truncated);
    let huge_file = file(&diff.files, "a_huge.txt");
    assert!(huge_file.too_large);
    assert!(huge_file.hunks.is_empty());
    let small_file = file(&diff.files, "b_small.txt");
    assert!(!small_file.too_large);
    assert_eq!(lines(small_file), ["+hello"]);
}

/// The git executable the app runs under its restriction: the one on
/// `PATH`, or the one the system launcher stands for.
#[cfg(target_os = "macos")]
fn real_git() -> PathBuf {
    let path = std::env::var_os("PATH").unwrap();
    let found = std::env::split_paths(&path)
        .map(|folder| folder.join("git"))
        .find(|candidate| candidate.is_file())
        .unwrap()
        .canonicalize()
        .unwrap();
    if found != Path::new("/usr/bin/git") {
        return found;
    }
    let output = std::process::Command::new("/usr/bin/xcrun")
        .args(["--find", "git"])
        .output()
        .unwrap();
    assert!(output.status.success());
    PathBuf::from(String::from_utf8(output.stdout).unwrap().trim())
        .canonicalize()
        .unwrap()
}

/// A link named `name` to the real git, which runs as the built-in
/// command of that name, and passes for git where only git may run.
#[cfg(target_os = "macos")]
fn link_to_git(dir: &Path, name: &str) -> PathBuf {
    let links = dir.join("links");
    std::fs::create_dir_all(&links).unwrap();
    let link = links.join(name);
    std::os::unix::fs::symlink(real_git(), &link).unwrap();
    link
}

/// Puts a repository of its own in place of the `.git` of the task's
/// worktree: its files are in a folder outside the worktree, and its
/// filter for `README.md` is `filter`. Gives the folder and the commit.
#[cfg(target_os = "macos")]
fn own_repository_in_place_of_git(repo: &Repo, task: &Task, filter: &str) -> (PathBuf, String) {
    let outside = repo.dir.path().canonicalize().unwrap().join("outside");
    std::fs::create_dir(&outside).unwrap();
    git(&outside, &["init", "-q", "-b", "main"]);
    let base = commit(&outside, "README.md", "one\n");
    git(&outside, &["config", "user.name", "t"]);
    git(&outside, &["config", "user.email", "t@example.com"]);
    git(&outside, &["config", "filter.f.clean", filter]);
    git(&outside, &["config", "clean.requireForce", "false"]);
    std::fs::write(outside.join(".gitattributes"), "README.md filter=f\n").unwrap();
    std::fs::write(outside.join("README.md"), "changed\n").unwrap();
    std::fs::write(outside.join("victim.txt"), "kept\n").unwrap();
    std::fs::remove_file(task.worktree.join(".git")).unwrap();
    std::fs::rename(outside.join(".git"), task.worktree.join(".git")).unwrap();
    git(
        &task.worktree,
        &["config", "core.worktree", outside.to_str().unwrap()],
    );
    (outside, base)
}

#[cfg(target_os = "macos")]
#[test]
fn a_filter_that_is_a_link_to_git_clean_removes_nothing_in_the_worktree() {
    let repo = repo();
    let task = task(&repo);
    let link = link_to_git(repo.dir.path(), "git-clean");
    // Without arguments: git runs a command of one word itself, and one
    // of more words through a shell, which may not run.
    let filter = link.to_str().unwrap();
    git(&repo.path, &["config", "filter.f.clean", filter]);
    git(&repo.path, &["config", "clean.requireForce", "false"]);
    std::fs::write(task.worktree.join(".gitattributes"), "README.md filter=f\n").unwrap();
    std::fs::write(task.worktree.join("README.md"), "changed\n").unwrap();
    let untracked = task.worktree.join("untracked.txt");
    std::fs::write(&untracked, "kept\n").unwrap();

    let _ = Git::default().diff(&task.worktree, &task.base_commit);

    assert!(untracked.exists(), "git diff removed an untracked file");
}

#[cfg(target_os = "macos")]
#[test]
fn a_filter_that_is_a_link_to_git_clean_removes_nothing_outside_the_worktree() {
    let repo = repo();
    let task = task(&repo);
    let link = link_to_git(repo.dir.path(), "git-clean");
    let filter = link.to_str().unwrap();
    let (outside, base) = own_repository_in_place_of_git(&repo, &task, filter);

    let _ = Git::default().diff(&task.worktree, &base);

    assert!(
        outside.join("victim.txt").exists(),
        "git diff removed a file outside the worktree"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn a_filter_that_is_a_link_to_git_stash_overwrites_nothing_outside_the_worktree() {
    let repo = repo();
    let task = task(&repo);
    let link = link_to_git(repo.dir.path(), "git-stash");
    let filter = link.to_str().unwrap();
    let (outside, base) = own_repository_in_place_of_git(&repo, &task, filter);

    let _ = Git::default().diff(&task.worktree, &base);

    assert_eq!(
        std::fs::read_to_string(outside.join("README.md")).unwrap(),
        "changed\n",
        "git diff overwrote a file outside the worktree"
    );
}

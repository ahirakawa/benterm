//! The diff of a task: the files of its worktree as they are now, against
//! the commit the task branched off from. Committed and uncommitted
//! changes are one diff, and files git does not know yet are in it as
//! added, unless git ignores them.
//!
//! Git reads skip index refresh and filesystem monitoring to avoid writes under `.git`.

use std::ffi::OsStr;
use std::fs::OpenOptions;
use std::io::Read;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use crate::TaskError;
use crate::git::Git;

/// What git is told before every read, so that it writes nothing.
const READ_ONLY: [&str; 5] = [
    "--no-optional-locks",
    "-c",
    "diff.autoRefreshIndex=false",
    "-c",
    // Names other than ASCII as they are, so only a few are quoted.
    "core.quotePath=false",
];

/// A new file larger than this is not read: it is listed without lines.
pub const NEW_FILE_LIMIT: u64 = 1 << 20;

/// Maximum bytes read from `git diff` (and `git ls-files`) before stopping.
pub const DIFF_OUTPUT_LIMIT: usize = 2 << 20;

/// Maximum number of untracked files read in a diff.
pub const NEW_FILES_COUNT_LIMIT: usize = 200;

/// Maximum total bytes read across all untracked files in a diff.
pub const NEW_FILES_TOTAL_LIMIT: u64 = 2 << 20;

/// How many bytes at the start of a new file are looked at to tell a
/// binary file, as git does.
const BINARY_PROBE: usize = 8000;

/// The changed files of a worktree, by path.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Diff {
    pub files: Vec<FileDiff>,
    /// Whether some untracked files were skipped because the count or
    /// total size limit was reached.
    pub untracked_truncated: bool,
}

impl Diff {
    /// How many lines were added and removed, in every file.
    pub fn counts(&self) -> (usize, usize) {
        let lines = self
            .files
            .iter()
            .flat_map(|file| &file.hunks)
            .flat_map(|hunk| &hunk.lines);
        lines.fold((0, 0), |(added, removed), line| match line.kind {
            LineKind::Added => (added + 1, removed),
            LineKind::Removed => (added, removed + 1),
            LineKind::Context => (added, removed),
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileStatus {
    /// Not in the commit the task branched off from.
    Added,
    Modified,
    /// In that commit and not in the worktree.
    Deleted,
}

/// A changed file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileDiff {
    /// From the top of the worktree, with `/` between folders.
    pub path: String,
    pub status: FileStatus,
    /// A binary file has no lines.
    pub binary: bool,
    /// A new file over [`NEW_FILE_LIMIT`] has no lines.
    pub too_large: bool,
    pub hunks: Vec<Hunk>,
}

/// A run of changed lines with the lines around them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hunk {
    /// The `@@ -1,3 +1,4 @@` line, with what git puts after it.
    pub header: String,
    pub lines: Vec<DiffLine>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineKind {
    Context,
    Added,
    Removed,
}

/// A line of a hunk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffLine {
    pub kind: LineKind,
    /// The line's number in the commit the task branched off from, from
    /// 1. An added line has none.
    pub old: Option<u32>,
    /// The line's number in the worktree's file, from 1. A removed line
    /// has none.
    pub new: Option<u32>,
    /// Without the mark in front and the line end.
    pub text: String,
}

impl Git {
    /// The diff of the files in `worktree` against the commit `base`.
    pub fn diff(&self, worktree: &Path, base: &str) -> Result<Diff, TaskError> {
        // A commit by its name, which is never taken for an option.
        if base.is_empty() || !base.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(TaskError::new(format!(
                "{base:?} is not the name of a commit"
            )));
        }
        let tracked = self.read(
            worktree,
            &[
                "diff",
                "--no-color",
                "--no-ext-diff",
                "--no-textconv",
                "--no-renames",
                "--no-relative",
                "--src-prefix=a/",
                "--dst-prefix=b/",
                "--unified=3",
                base,
                "--",
            ],
            self.diff_output_limit,
        )?;
        let mut files = parse(&tracked);
        let others = self.read(
            worktree,
            &["ls-files", "--others", "--exclude-standard", "-z", "--"],
            self.diff_output_limit,
        )?;
        let mut untracked_count = 0usize;
        let mut untracked_bytes = 0u64;
        let mut untracked_truncated = false;
        for path in others.split(|byte| *byte == 0).filter(|p| !p.is_empty()) {
            if untracked_count >= self.new_files_count_limit {
                untracked_truncated = true;
                break;
            }
            let path = String::from_utf8_lossy(path).into_owned();
            let remaining = self.new_files_total_limit.saturating_sub(untracked_bytes);
            match new_file(&worktree.join(&path), path, remaining) {
                NewFile::Read(file, bytes) => {
                    untracked_count += 1;
                    untracked_bytes = untracked_bytes.saturating_add(bytes);
                    files.push(file);
                }
                NewFile::TotalLimitReached => {
                    untracked_truncated = true;
                    break;
                }
            }
        }
        files.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(Diff {
            files,
            untracked_truncated,
        })
    }

    /// What git prints, read only and bounded to `limit` bytes, or what it
    /// said when it failed.
    fn read(
        &self,
        worktree: &Path,
        arguments: &[&str],
        limit: usize,
    ) -> Result<Vec<u8>, TaskError> {
        let all: Vec<&OsStr> = READ_ONLY.iter().chain(arguments).map(OsStr::new).collect();
        let output = self.run_with_limit(worktree, &all, limit)?;
        if !output.status.success() {
            return Err(TaskError::new(crate::git::said(&output)));
        }
        Ok(output.stdout)
    }
}

enum NewFile {
    /// File was processed (possibly marked `too_large` by the per-file limit),
    /// consuming the given number of bytes from the untracked total budget.
    Read(FileDiff, u64),
    /// Reading this file would exceed the remaining untracked total byte budget.
    TotalLimitReached,
}

/// A file git does not know, as all added.
fn new_file(path: &Path, name: String, remaining_total: u64) -> NewFile {
    let mut file = FileDiff {
        path: name,
        status: FileStatus::Added,
        binary: false,
        too_large: false,
        hunks: Vec::new(),
    };
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        // Gone since git listed it.
        return NewFile::Read(file, 0);
    };
    let bytes = if meta.file_type().is_symlink() {
        // git keeps a link as the path it points to.
        match std::fs::read_link(path) {
            Ok(target) => {
                let raw = target.as_os_str().as_encoded_bytes().to_vec();
                if raw.len() as u64 > remaining_total {
                    return NewFile::TotalLimitReached;
                }
                raw
            }
            Err(_) => return NewFile::Read(file, 0),
        }
    } else {
        if !meta.is_file() {
            return NewFile::Read(file, 0);
        }
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        let Ok(handle) = options.open(path) else {
            return NewFile::Read(file, 0);
        };
        let Ok(opened_meta) = handle.metadata() else {
            return NewFile::Read(file, 0);
        };
        if !opened_meta.is_file() {
            return NewFile::Read(file, 0);
        }
        if opened_meta.len() > NEW_FILE_LIMIT {
            file.too_large = true;
            return NewFile::Read(file, 0);
        }
        if opened_meta.len() > remaining_total {
            return NewFile::TotalLimitReached;
        }
        let budget = NEW_FILE_LIMIT.min(remaining_total);
        let mut bytes = Vec::new();
        if handle
            .take(budget.saturating_add(1))
            .read_to_end(&mut bytes)
            .is_err()
        {
            return NewFile::Read(file, 0);
        }
        if bytes.len() as u64 > NEW_FILE_LIMIT {
            file.too_large = true;
            return NewFile::Read(file, 0);
        }
        if bytes.len() as u64 > remaining_total {
            return NewFile::TotalLimitReached;
        }
        bytes
    };
    let read_len = bytes.len() as u64;
    if bytes[..bytes.len().min(BINARY_PROBE)].contains(&0) {
        file.binary = true;
        return NewFile::Read(file, read_len);
    }
    let text = String::from_utf8_lossy(&bytes);
    let lines: Vec<DiffLine> = text
        .lines()
        .zip(1..)
        .map(|(text, number)| DiffLine {
            kind: LineKind::Added,
            old: None,
            new: Some(number),
            text: text.to_owned(),
        })
        .collect();
    if !lines.is_empty() {
        file.hunks.push(Hunk {
            header: format!("@@ -0,0 +1,{} @@", lines.len()),
            lines,
        });
    }
    NewFile::Read(file, read_len)
}

/// The files of what `git diff` printed.
fn parse(output: &[u8]) -> Vec<FileDiff> {
    let mut files: Vec<FileDiff> = Vec::new();
    // The numbers of the next old and new line of the hunk being read.
    let mut next: Option<(u32, u32)> = None;
    // The last element after the final line end is empty.
    for line in output.split(|byte| *byte == b'\n') {
        if let Some(rest) = line.strip_prefix(b"diff --git ") {
            next = None;
            files.push(FileDiff {
                path: header_path(rest),
                status: FileStatus::Modified,
                binary: false,
                too_large: false,
                hunks: Vec::new(),
            });
            continue;
        }
        let Some(file) = files.last_mut() else {
            continue;
        };
        if line.starts_with(b"@@ ") {
            let text = String::from_utf8_lossy(line).into_owned();
            next = hunk_start(&text);
            file.hunks.push(Hunk {
                header: text,
                lines: Vec::new(),
            });
            continue;
        }
        match (next.as_mut(), file.hunks.last_mut()) {
            (Some((old, new)), Some(hunk)) => {
                let (kind, numbers) = match line.first() {
                    Some(b'+') => (LineKind::Added, (None, Some(*new))),
                    Some(b'-') => (LineKind::Removed, (Some(*old), None)),
                    Some(b' ') => (LineKind::Context, (Some(*old), Some(*new))),
                    // `\ No newline at end of file`, or the end.
                    _ => continue,
                };
                if numbers.0.is_some() {
                    *old += 1;
                }
                if numbers.1.is_some() {
                    *new += 1;
                }
                hunk.lines.push(DiffLine {
                    kind,
                    old: numbers.0,
                    new: numbers.1,
                    text: String::from_utf8_lossy(&line[1..]).into_owned(),
                });
            }
            _ => {
                if line.starts_with(b"new file mode ") {
                    file.status = FileStatus::Added;
                } else if line.starts_with(b"deleted file mode ") {
                    file.status = FileStatus::Deleted;
                } else if line.starts_with(b"Binary files ") || line == b"GIT binary patch" {
                    file.binary = true;
                }
            }
        }
    }
    files
}

/// The first line numbers of a hunk, from `@@ -old,n +new,m @@`.
fn hunk_start(header: &str) -> Option<(u32, u32)> {
    let mut parts = header.split(' ').skip(1);
    let number = |part: Option<&str>, mark: char| -> Option<u32> {
        let part = part?.strip_prefix(mark)?;
        part.split(',').next()?.parse().ok()
    };
    let old = number(parts.next(), '-')?;
    let new = number(parts.next(), '+')?;
    // A side with no lines is numbered from the line before it.
    Some((old.max(1), new.max(1)))
}

/// The path in `a/P b/P`, the rest of a `diff --git` line. Without
/// renames both sides are the same path, quoted by git when it has a
/// quote, a backslash or a control character in it.
fn header_path(rest: &[u8]) -> String {
    if rest.first() == Some(&b'"') {
        let bytes = unquote(&rest[1..]);
        let path = bytes.strip_prefix(b"a/").unwrap_or(&bytes);
        return String::from_utf8_lossy(path).into_owned();
    }
    // `a/` + P + ` b/` + P.
    let length = rest.len().saturating_sub(5) / 2;
    let path = rest.get(2..2 + length).unwrap_or_default();
    String::from_utf8_lossy(path).into_owned()
}

/// The bytes of a C-style quoted string up to its closing quote, which
/// `quoted` starts after the opening one.
fn unquote(quoted: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(quoted.len());
    let mut rest = quoted.iter().copied();
    while let Some(byte) = rest.next() {
        match byte {
            b'"' => break,
            b'\\' => {
                let Some(escaped) = rest.next() else {
                    break;
                };
                let plain = match escaped {
                    b'a' => 0x07,
                    b'b' => 0x08,
                    b't' => b'\t',
                    b'n' => b'\n',
                    b'v' => 0x0b,
                    b'f' => 0x0c,
                    b'r' => b'\r',
                    b'0'..=b'7' => {
                        let mut value = u32::from(escaped - b'0');
                        for _ in 0..2 {
                            let Some(digit) = rest.next() else { break };
                            value = value * 8 + u32::from(digit.wrapping_sub(b'0') & 7);
                        }
                        value as u8
                    }
                    other => other,
                };
                bytes.push(plain);
            }
            other => bytes.push(other),
        }
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_header_gives_its_path_quoted_or_not() {
        assert_eq!(header_path(b"a/a b.txt b/a b.txt"), "a b.txt");
        assert_eq!(header_path(b"a/x b/x"), "x");
        assert_eq!(
            header_path(b"\"a/tab\\there\" \"b/tab\\there\""),
            "tab\there"
        );
        assert_eq!(
            header_path(b"\"a/\\343\\201\\202\" \"b/\\343\\201\\202\""),
            "あ"
        );
    }

    #[test]
    fn a_hunk_counts_its_lines_from_the_numbers_in_its_header() {
        let output = b"diff --git a/f b/f\n\
            index 1..2 100644\n\
            --- a/f\n\
            +++ b/f\n\
            @@ -10,3 +10,3 @@ fn main() {\n \
            same\n\
            --- removed\n\
            +++ added\n\
            \\ No newline at end of file\n";
        let files = parse(output);
        assert_eq!(files.len(), 1);
        let hunk = &files[0].hunks[0];
        assert_eq!(hunk.header, "@@ -10,3 +10,3 @@ fn main() {");
        let lines: Vec<_> = hunk
            .lines
            .iter()
            .map(|line| (line.kind, line.old, line.new, line.text.as_str()))
            .collect();
        assert_eq!(
            lines,
            [
                (LineKind::Context, Some(10), Some(10), "same"),
                (LineKind::Removed, Some(11), None, "-- removed"),
                (LineKind::Added, None, Some(11), "++ added"),
            ]
        );
    }
}

//! The issues a repository keeps in a file of its own, for a repository
//! that has no GitHub to ask: `.benterm/issues.json` under the top
//! directory of the checkout.
//!
//! ```json
//! {
//!   "version": 1,
//!   "issues": [
//!     { "number": 1, "title": "Epic", "description": "optional" },
//!     { "number": 2, "title": "Child", "state": "closed", "parent": 1 },
//!     { "number": 3, "title": "Next", "parent": 1, "blocked_by": [2] }
//!   ]
//! }
//! ```
//!
//! An epic is an issue that others name as their `parent`. The file is
//! written by whoever has the repository, so it is read as what a
//! stranger wrote: no more than [`LOCAL_ISSUES_LIMIT`] bytes of it, and
//! never through a symbolic link.
//!
//! The file may also say `next_number`, the number of the next issue
//! that [`crate::change_local_issues`] makes. A field this build does
//! not know is kept as it is when the file is written again, unless it
//! is a field of the format spelt another way, like `blockedBy`, which
//! is refused so that it is not silently no blocker.

use std::collections::BTreeSet;
use std::fmt;
use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub const LOCAL_ISSUES_DIRECTORY: &str = ".benterm";
pub const LOCAL_ISSUES_FILE: &str = "issues.json";
/// The most bytes of the file that are read. A larger file is refused.
pub const LOCAL_ISSUES_LIMIT: usize = 1 << 20;
pub const LOCAL_ISSUES_VERSION: u64 = 1;

pub fn local_issues_path(root: &Path) -> PathBuf {
    root.join(LOCAL_ISSUES_DIRECTORY).join(LOCAL_ISSUES_FILE)
}

/// Whether a local issue is still to be done.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LocalIssueState {
    #[default]
    Open,
    Closed,
}

impl LocalIssueState {
    fn is_open(&self) -> bool {
        *self == Self::Open
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalIssue {
    pub number: u64,
    pub title: String,
    pub state: LocalIssueState,
    /// The number of the issue this one is a part of.
    pub parent: Option<u64>,
    /// The numbers of the issues to be done before this one, each once.
    pub blocked_by: Vec<u64>,
    pub description: Option<String>,
}

/// The issues of a file, in the order the file has them. Every number is
/// there once, and every parent and blocker is an issue of the file.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LocalIssues {
    pub issues: Vec<LocalIssue>,
}

impl LocalIssues {
    pub fn issue(&self, number: u64) -> Option<&LocalIssue> {
        self.issues.iter().find(|issue| issue.number == number)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LocalIssuesError {
    /// The directory is not in the checkout of a git repository.
    NotGitRepository(String),
    /// There is no file.
    Missing,
    /// The file or its directory is a symbolic link, or is not a plain
    /// file or directory. Says which.
    NotPlainFile(String),
    /// The file has more bytes than are read.
    TooLarge { limit: usize },
    /// The file is there and could not be read.
    Io(String),
    /// The file is not in the format. Says what is wrong.
    Invalid(String),
    /// The file has no issue of this number.
    IssueNotFound(u64),
    /// The file could not be written.
    Write(String),
    /// A change was not made, and nothing was written. Says why.
    Refused(String),
}

impl fmt::Display for LocalIssuesError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let file = format!("{LOCAL_ISSUES_DIRECTORY}/{LOCAL_ISSUES_FILE}");
        match self {
            Self::NotGitRepository(why) => f.write_str(why),
            Self::Missing => write!(f, "there is no {file}"),
            Self::NotPlainFile(what) => write!(f, "{what} is not read"),
            Self::TooLarge { limit } => write!(f, "{file} has more than {limit} bytes"),
            Self::Io(why) => write!(f, "{file} could not be read ({why})"),
            Self::Invalid(why) => write!(f, "{file} is not in the format: {why}"),
            Self::IssueNotFound(number) => write!(f, "{file} has no issue #{number}"),
            Self::Write(why) => write!(f, "{file} could not be written ({why})"),
            Self::Refused(why) => f.write_str(why),
        }
    }
}

impl std::error::Error for LocalIssuesError {}

/// The version alone, read before the rest so that a file of another
/// version is said to be that and not a list of fields it lacks.
#[derive(Deserialize)]
struct Versioned {
    version: Option<u64>,
}

/// The file as a whole. What this build does not know is in `other`,
/// and is written back as it was read.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Written {
    pub(crate) version: u64,
    /// The number of the next issue: never one an issue of the file has
    /// or had.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) next_number: Option<u64>,
    pub(crate) issues: Vec<WrittenIssue>,
    #[serde(flatten)]
    pub(crate) other: Map<String, Value>,
}

impl Default for Written {
    fn default() -> Self {
        Self {
            version: LOCAL_ISSUES_VERSION,
            next_number: None,
            issues: Vec::new(),
            other: Map::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct WrittenIssue {
    pub(crate) number: u64,
    pub(crate) title: String,
    #[serde(default, skip_serializing_if = "LocalIssueState::is_open")]
    pub(crate) state: LocalIssueState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) parent: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) blocked_by: Vec<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) description: Option<String>,
    #[serde(flatten)]
    pub(crate) other: Map<String, Value>,
}

/// The fields of the format, as [`spelt_alike`] compares them.
const FIELDS: [&str; 9] = [
    "version",
    "nextnumber",
    "issues",
    "number",
    "title",
    "state",
    "parent",
    "blockedby",
    "description",
];

/// A field the format does not have is kept, but one that is a field of
/// the format in other capitals or with other dashes is a slip of the
/// hand: `blockedBy` would silently be no blocker.
fn spelt_alike(other: &Map<String, Value>) -> Result<(), LocalIssuesError> {
    for name in other.keys() {
        let plain: String = name
            .chars()
            .filter(|ch| !matches!(ch, '_' | '-'))
            .flat_map(char::to_lowercase)
            .collect();
        if FIELDS.contains(&plain.as_str()) {
            return Err(invalid(format!(
                "unknown field `{name}`, which is spelt like a field of the format"
            )));
        }
    }
    Ok(())
}

pub(crate) fn invalid(why: impl Into<String>) -> LocalIssuesError {
    LocalIssuesError::Invalid(why.into())
}

pub fn parse_local_issues(bytes: &[u8]) -> Result<LocalIssues, LocalIssuesError> {
    check(&parse_written(bytes)?)
}

pub(crate) fn parse_written(bytes: &[u8]) -> Result<Written, LocalIssuesError> {
    let versioned: Versioned = serde_json::from_slice(bytes)
        .map_err(|error| invalid(format!("it is not the JSON of the format ({error})")))?;
    match versioned.version {
        None => return Err(invalid("it has no version")),
        Some(LOCAL_ISSUES_VERSION) => {}
        Some(version) => {
            return Err(invalid(format!(
                "it is of version {version}, and this build reads version \
                 {LOCAL_ISSUES_VERSION}"
            )));
        }
    }
    serde_json::from_slice(bytes).map_err(|error| invalid(error.to_string()))
}

/// The issues `written` has, when every number is there once and every
/// parent and blocker is an issue of the file.
pub(crate) fn check(written: &Written) -> Result<LocalIssues, LocalIssuesError> {
    spelt_alike(&written.other)?;
    let mut numbers = BTreeSet::new();
    for issue in &written.issues {
        spelt_alike(&issue.other)?;
        if issue.number == 0 {
            return Err(invalid("an issue has the number 0"));
        }
        if !numbers.insert(issue.number) {
            return Err(invalid(format!(
                "#{} is there more than once",
                issue.number
            )));
        }
        if issue.title.trim().is_empty() {
            return Err(invalid(format!("#{} has no title", issue.number)));
        }
    }

    let mut issues = Vec::with_capacity(written.issues.len());
    for issue in &written.issues {
        let number = issue.number;
        let mut blocked_by = Vec::with_capacity(issue.blocked_by.len());
        for other in issue.parent.iter().chain(&issue.blocked_by) {
            if *other == number {
                return Err(invalid(format!(
                    "#{number} names itself as its parent or blocker"
                )));
            }
            if !numbers.contains(other) {
                return Err(invalid(format!(
                    "#{number} names #{other}, which is not in the file"
                )));
            }
        }
        for blocker in &issue.blocked_by {
            if !blocked_by.contains(blocker) {
                blocked_by.push(*blocker);
            }
        }
        issues.push(LocalIssue {
            number,
            title: issue.title.clone(),
            state: issue.state,
            parent: issue.parent,
            blocked_by,
            description: issue
                .description
                .clone()
                .filter(|text| !text.trim().is_empty()),
        });
    }
    Ok(LocalIssues { issues })
}

pub fn read_local_issues(root: &Path) -> Result<LocalIssues, LocalIssuesError> {
    parse_local_issues(&read_bounded(root)?)
}

pub(crate) fn io_error(error: &io::Error) -> LocalIssuesError {
    LocalIssuesError::Io(error.kind().to_string())
}

#[cfg(unix)]
pub(crate) fn open_at(
    parent: &File,
    name: &str,
    flags: libc::c_int,
    mode: libc::c_uint,
) -> io::Result<File> {
    use std::os::fd::{AsRawFd, FromRawFd};

    let name = std::ffi::CString::new(name).map_err(|_| io::ErrorKind::InvalidInput)?;
    // SAFETY: `parent` is an open descriptor and `name` a C string
    // that both outlive the call.
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_CLOEXEC | libc::O_NOFOLLOW | flags,
            mode,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `fd` was just opened and nothing else owns it.
    Ok(unsafe { File::from_raw_fd(fd) })
}

/// What opening `name` without following a link failed with.
#[cfg(unix)]
pub(crate) fn refused(error: &io::Error, name: &str, wanted: &str) -> LocalIssuesError {
    match error.raw_os_error() {
        Some(libc::ENOENT) => LocalIssuesError::Missing,
        // A link, with `O_NOFOLLOW`. FreeBSD says `EMLINK`.
        Some(libc::ELOOP | libc::EMLINK | libc::ENOTDIR) => LocalIssuesError::NotPlainFile(
            format!("{name}, which is a symbolic link or not {wanted},"),
        ),
        _ => io_error(error),
    }
}

/// The directory of the file, opened from the top directory `top` of
/// the checkout and without following a link.
#[cfg(unix)]
pub(crate) fn open_directory(top: &File) -> Result<File, LocalIssuesError> {
    open_at(
        top,
        LOCAL_ISSUES_DIRECTORY,
        libc::O_RDONLY | libc::O_DIRECTORY,
        0,
    )
    .map_err(|error| refused(&error, LOCAL_ISSUES_DIRECTORY, "a directory"))
}

/// The bytes of the file in its `directory`, opened without following a
/// link.
#[cfg(unix)]
pub(crate) fn read_in(directory: &File) -> Result<Vec<u8>, LocalIssuesError> {
    // Without blocking, so that a FIFO put there does not hold the read.
    let file = open_at(
        directory,
        LOCAL_ISSUES_FILE,
        libc::O_RDONLY | libc::O_NONBLOCK,
        0,
    )
    .map_err(|error| refused(&error, LOCAL_ISSUES_FILE, "a file"))?;
    let meta = file.metadata().map_err(|error| io_error(&error))?;
    if !meta.is_file() {
        return Err(LocalIssuesError::NotPlainFile(format!(
            "{LOCAL_ISSUES_FILE}, which is not a file,"
        )));
    }
    read_limited(file, meta.len())
}

/// Open each component relative to its parent descriptor with no symlink following to prevent swaps.
#[cfg(unix)]
fn read_bounded(root: &Path) -> Result<Vec<u8>, LocalIssuesError> {
    let top = File::open(root).map_err(|error| io_error(&error))?;
    read_in(&open_directory(&top)?)
}

#[cfg(not(unix))]
fn read_bounded(root: &Path) -> Result<Vec<u8>, LocalIssuesError> {
    let path = local_issues_path(root);
    for (path, name) in [
        (path.parent().unwrap_or(root), LOCAL_ISSUES_DIRECTORY),
        (path.as_path(), LOCAL_ISSUES_FILE),
    ] {
        match std::fs::symlink_metadata(path) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(LocalIssuesError::NotPlainFile(format!(
                    "{name}, which is a symbolic link,"
                )));
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Err(LocalIssuesError::Missing);
            }
            Err(error) => return Err(io_error(&error)),
        }
    }
    let file = File::open(&path).map_err(|error| io_error(&error))?;
    let meta = file.metadata().map_err(|error| io_error(&error))?;
    if !meta.is_file() {
        return Err(LocalIssuesError::NotPlainFile(format!(
            "{LOCAL_ISSUES_FILE}, which is not a file,"
        )));
    }
    read_limited(file, meta.len())
}

/// Reads `file`, which says it has `length` bytes, and no more than the
/// limit and one byte, which is how a file that grew is found out.
fn read_limited(file: File, length: u64) -> Result<Vec<u8>, LocalIssuesError> {
    let too_large = LocalIssuesError::TooLarge {
        limit: LOCAL_ISSUES_LIMIT,
    };
    if length > LOCAL_ISSUES_LIMIT as u64 {
        return Err(too_large);
    }
    let mut bytes = Vec::with_capacity(length as usize);
    file.take(LOCAL_ISSUES_LIMIT as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| io_error(&error))?;
    if bytes.len() > LOCAL_ISSUES_LIMIT {
        return Err(too_large);
    }
    Ok(bytes)
}

//! Changes to the local issues file: [`change_local_issues`] makes an
//! issue, edits one, opens or closes one, sets what an issue is a part
//! of and what blocks it, or takes an issue out.
//!
//! Every change takes a lock on the directory of the file, reads the
//! file, changes what it read and replaces the file as a whole, as the
//! tasks file is changed. Two changes made at once so never lose each
//! other, and a number is never given twice, also not the number of an
//! issue that was taken out. What this build does not know of the file
//! is written back as it was read.
//!
//! The directory and the file are opened without following a link, as
//! they are for reading, and the new file is put in place by a rename
//! in that very directory. A link put in the repository so never has
//! anything outside it written.

use std::collections::BTreeSet;
use std::path::Path;

use serde_json::Map;

use crate::local_issues::{
    LOCAL_ISSUES_FILE, LOCAL_ISSUES_LIMIT, LocalIssueState, LocalIssues, LocalIssuesError, Written,
    WrittenIssue, check,
};

/// An issue before it has a number.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NewLocalIssue {
    pub title: String,
    pub description: Option<String>,
    /// The number of the issue this one is a part of.
    pub parent: Option<u64>,
    /// The numbers of the issues to be done before this one.
    pub blocked_by: Vec<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LocalIssueChange {
    /// Makes an issue and gives it its number. Makes the file and its
    /// directory when they are not there.
    Create(NewLocalIssue),
    /// Says anew everything of the issue `number` but its state.
    Edit {
        number: u64,
        issue: NewLocalIssue,
    },
    SetState {
        number: u64,
        state: LocalIssueState,
    },
    /// Makes `number` a part of `parent`, or of nothing.
    SetParent {
        number: u64,
        parent: Option<u64>,
    },
    /// Says that `blocker` is to be done before `number`.
    AddBlocker {
        number: u64,
        blocker: u64,
    },
    RemoveBlocker {
        number: u64,
        blocker: u64,
    },
    /// Takes the issue out. No issue is a part of it or blocked by it
    /// afterwards, and its number is not given again.
    Delete {
        number: u64,
    },
}

/// The issues of a file after a change.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalIssuesChanged {
    pub issues: LocalIssues,
    /// The number of the issue that was made, changed or taken out.
    pub number: u64,
}

fn refuse(why: impl Into<String>) -> LocalIssuesError {
    LocalIssuesError::Refused(why.into())
}

fn position(file: &Written, number: u64) -> Result<usize, LocalIssuesError> {
    file.issues
        .iter()
        .position(|issue| issue.number == number)
        .ok_or(LocalIssuesError::IssueNotFound(number))
}

/// The number for a new issue: never one an issue of the file has or
/// had.
fn take_number(file: &mut Written) -> Result<u64, LocalIssuesError> {
    let used = file.issues.iter().map(|issue| issue.number).max();
    let number = used
        .unwrap_or(0)
        .checked_add(1)
        .map(|next| next.max(file.next_number.unwrap_or(1)));
    let after = number.and_then(|number| number.checked_add(1));
    let (Some(number), Some(after)) = (number, after) else {
        return Err(refuse("there are no numbers left for issues"));
    };
    file.next_number = Some(after);
    Ok(number)
}

fn title_of(title: &str) -> Result<String, LocalIssuesError> {
    let title = title.trim();
    if title.is_empty() {
        return Err(refuse("an issue needs a title"));
    }
    Ok(title.to_owned())
}

fn description_of(description: Option<String>) -> Option<String> {
    description
        .map(|text| text.trim().to_owned())
        .filter(|text| !text.is_empty())
}

/// `numbers` with each number once, in the order they were given.
fn each_once(numbers: &[u64]) -> Vec<u64> {
    let mut seen = BTreeSet::new();
    numbers
        .iter()
        .copied()
        .filter(|number| seen.insert(*number))
        .collect()
}

fn path_text(path: &[u64]) -> String {
    let names: Vec<String> = path.iter().map(|number| format!("#{number}")).collect();
    names.join(" → ")
}

/// Refuses when the issue `number` is, by the parents the file names, a
/// part of itself.
fn refuse_parent_circle(file: &Written, number: u64) -> Result<(), LocalIssuesError> {
    let parent_of = |number: u64| {
        file.issues
            .iter()
            .find(|issue| issue.number == number)
            .and_then(|issue| issue.parent)
    };
    let mut path = vec![number];
    let mut at = number;
    while let Some(parent) = parent_of(at) {
        path.push(parent);
        if parent == number {
            return Err(refuse(format!(
                "#{number} would be a part of itself ({})",
                path_text(&path)
            )));
        }
        // A circle that was written by hand and that `number` is not in.
        if path.len() > file.issues.len() {
            break;
        }
        at = parent;
    }
    Ok(())
}

/// Refuses when the issue `number` is, by the blockers the file names,
/// to be done before itself.
fn refuse_blocker_circle(file: &Written, number: u64) -> Result<(), LocalIssuesError> {
    let blockers_of = |number: u64| {
        file.issues
            .iter()
            .find(|issue| issue.number == number)
            .map(|issue| issue.blocked_by.as_slice())
            .unwrap_or_default()
    };
    // Each path that is still walked, from `number` to an issue that
    // is, in the end, to be done before it.
    let mut walked = vec![vec![number]];
    let mut seen = BTreeSet::new();
    while let Some(path) = walked.pop() {
        let at = *path.last().expect("a path has its start");
        for blocker in blockers_of(at) {
            let mut longer = path.clone();
            longer.push(*blocker);
            if *blocker == number {
                // Said from the first to be done to the last.
                longer.reverse();
                return Err(refuse(format!(
                    "#{number} would wait for itself ({})",
                    path_text(&longer)
                )));
            }
            if seen.insert(*blocker) {
                walked.push(longer);
            }
        }
    }
    Ok(())
}

/// Refuses a parent or a blocker that is the issue itself or no issue
/// of the file.
fn known_other(
    file: &Written,
    number: u64,
    other: u64,
    what: &str,
) -> Result<(), LocalIssuesError> {
    if other == number {
        return Err(refuse(format!("#{number} cannot be its own {what}")));
    }
    if !file.issues.iter().any(|issue| issue.number == other) {
        return Err(refuse(format!(
            "there is no issue #{other} to be the {what}"
        )));
    }
    Ok(())
}

/// Makes `change` to `file`, or refuses and says why. Gives the number
/// of the issue that was made, changed or taken out.
fn apply(file: &mut Written, change: LocalIssueChange) -> Result<u64, LocalIssuesError> {
    match change {
        LocalIssueChange::Create(new) => {
            let title = title_of(&new.title)?;
            let number = take_number(file)?;
            let blocked_by = each_once(&new.blocked_by);
            if let Some(other) = new.parent {
                known_other(file, number, other, "parent")?;
            }
            for other in &blocked_by {
                known_other(file, number, *other, "blocker")?;
            }
            file.issues.push(WrittenIssue {
                number,
                title,
                state: LocalIssueState::Open,
                parent: new.parent,
                blocked_by,
                description: description_of(new.description),
                other: Map::new(),
            });
            Ok(number)
        }
        LocalIssueChange::Edit { number, issue: new } => {
            let at = position(file, number)?;
            let title = title_of(&new.title)?;
            let blocked_by = each_once(&new.blocked_by);
            if let Some(other) = new.parent {
                known_other(file, number, other, "parent")?;
            }
            for other in &blocked_by {
                known_other(file, number, *other, "blocker")?;
            }
            let issue = &mut file.issues[at];
            issue.title = title;
            issue.description = description_of(new.description);
            issue.parent = new.parent;
            issue.blocked_by = blocked_by;
            refuse_parent_circle(file, number)?;
            refuse_blocker_circle(file, number)?;
            Ok(number)
        }
        LocalIssueChange::SetState { number, state } => {
            let at = position(file, number)?;
            file.issues[at].state = state;
            Ok(number)
        }
        LocalIssueChange::SetParent { number, parent } => {
            let at = position(file, number)?;
            if let Some(other) = parent {
                known_other(file, number, other, "parent")?;
            }
            file.issues[at].parent = parent;
            refuse_parent_circle(file, number)?;
            Ok(number)
        }
        LocalIssueChange::AddBlocker { number, blocker } => {
            let at = position(file, number)?;
            known_other(file, number, blocker, "blocker")?;
            if !file.issues[at].blocked_by.contains(&blocker) {
                file.issues[at].blocked_by.push(blocker);
            }
            refuse_blocker_circle(file, number)?;
            Ok(number)
        }
        LocalIssueChange::RemoveBlocker { number, blocker } => {
            let at = position(file, number)?;
            file.issues[at].blocked_by.retain(|other| *other != blocker);
            Ok(number)
        }
        LocalIssueChange::Delete { number } => {
            let at = position(file, number)?;
            // The number stays taken when it was the last one given.
            let used = file.issues.iter().map(|issue| issue.number).max();
            let after = used.and_then(|used| used.checked_add(1));
            file.next_number = file.next_number.max(after);
            file.issues.remove(at);
            for issue in &mut file.issues {
                if issue.parent == Some(number) {
                    issue.parent = None;
                }
                issue.blocked_by.retain(|other| *other != number);
            }
            Ok(number)
        }
    }
}

/// Makes `change` to the issues file of the checkout whose top
/// directory is `root`, and gives the issues as they are afterwards.
/// When the change is refused or the file cannot be written, the file
/// is as it was.
pub fn change_local_issues(
    root: &Path,
    change: LocalIssueChange,
) -> Result<LocalIssuesChanged, LocalIssuesError> {
    let makes = matches!(change, LocalIssueChange::Create(_));
    let place = place::Place::lock(root, makes)?;
    let mut file = match place.read() {
        Ok(bytes) => crate::local_issues::parse_written(&bytes)?,
        // The first issue of a repository makes the file.
        Err(LocalIssuesError::Missing) if makes => Written::default(),
        Err(error) => return Err(error),
    };
    // A file that is not in the format is left as it is.
    check(&file)?;
    let number = apply(&mut file, change)?;
    let issues = check(&file)?;
    let mut text = serde_json::to_string_pretty(&file)
        .map_err(|error| LocalIssuesError::Write(error.to_string()))?;
    text.push('\n');
    if text.len() > LOCAL_ISSUES_LIMIT {
        return Err(LocalIssuesError::TooLarge {
            limit: LOCAL_ISSUES_LIMIT,
        });
    }
    place.replace(text.as_bytes())?;
    Ok(LocalIssuesChanged { issues, number })
}

/// The name the new file has until it takes the place of the old one.
const PART: &str = ".issues.json.part";

#[cfg(unix)]
mod place {
    use std::ffi::CString;
    use std::fs::File;
    use std::io::{self, Write};
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    use super::{LOCAL_ISSUES_FILE, LocalIssuesError, PART};
    use crate::local_issues::{
        LOCAL_ISSUES_DIRECTORY, io_error, open_at, open_directory, read_in, refused,
    };

    fn failed(error: &io::Error) -> LocalIssuesError {
        LocalIssuesError::Write(error.kind().to_string())
    }

    /// The directory of the file, locked until this is dropped.
    pub(super) struct Place {
        directory: File,
    }

    impl Place {
        /// Opens the directory of the file under `root` without
        /// following a link, making it when `make` says so, and waits
        /// for its lock. The lock is on the directory itself, so that
        /// no file for it is left in the repository.
        pub(super) fn lock(root: &Path, make: bool) -> Result<Self, LocalIssuesError> {
            let top = File::open(root).map_err(|error| io_error(&error))?;
            let directory = match open_directory(&top) {
                Err(LocalIssuesError::Missing) if make => {
                    let name = CString::new(LOCAL_ISSUES_DIRECTORY).expect("no NUL in the name");
                    // SAFETY: `top` is an open descriptor and `name` a C
                    // string that both outlive the call.
                    let made = unsafe { libc::mkdirat(top.as_raw_fd(), name.as_ptr(), 0o755) };
                    if made != 0 {
                        let error = io::Error::last_os_error();
                        // Another change made it in between.
                        if error.kind() != io::ErrorKind::AlreadyExists {
                            return Err(failed(&error));
                        }
                    }
                    open_directory(&top)?
                }
                other => other?,
            };
            // Held for a read and a write of a small file.
            directory.lock().map_err(|error| failed(&error))?;
            Ok(Self { directory })
        }

        /// The bytes of the file as it is now.
        pub(super) fn read(&self) -> Result<Vec<u8>, LocalIssuesError> {
            read_in(&self.directory)
        }

        /// Puts `bytes` in the place of the file: written under another
        /// name of the same directory first, and renamed when all of it
        /// is on the disk.
        pub(super) fn replace(&self, bytes: &[u8]) -> Result<(), LocalIssuesError> {
            let at = self.directory.as_raw_fd();
            let part = CString::new(PART).expect("no NUL in the name");
            let name = CString::new(LOCAL_ISSUES_FILE).expect("no NUL in the name");
            // What the file may be read and written by stays.
            let mode = match open_at(&self.directory, LOCAL_ISSUES_FILE, libc::O_RDONLY, 0) {
                Ok(file) => file
                    .metadata()
                    .map_or(0o644, |meta| meta.permissions().mode() & 0o777),
                Err(_) => 0o644,
            };
            // One left by a change that did not finish. Under the lock
            // no other change is writing it.
            // SAFETY: `at` is an open descriptor and `part` a C string
            // that both outlive the call.
            unsafe { libc::unlinkat(at, part.as_ptr(), 0) };
            let written = (|| {
                // Made new, so that it is no link to another file.
                let mut out = open_at(
                    &self.directory,
                    PART,
                    libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
                    0o600,
                )?;
                out.write_all(bytes)?;
                out.set_permissions(std::fs::Permissions::from_mode(mode))?;
                out.sync_all()?;
                // SAFETY: as above, with two names in the one directory.
                // The rename replaces what has the name, and does not
                // follow it if it has become a link.
                if unsafe { libc::renameat(at, part.as_ptr(), at, name.as_ptr()) } != 0 {
                    return Err(io::Error::last_os_error());
                }
                // The new name of the file is in its directory.
                self.directory.sync_all()
            })();
            written.map_err(|error| {
                // SAFETY: as above.
                unsafe { libc::unlinkat(at, part.as_ptr(), 0) };
                match refused(&error, PART, "a file") {
                    LocalIssuesError::NotPlainFile(what) => LocalIssuesError::NotPlainFile(what),
                    _ => failed(&error),
                }
            })
        }
    }
}

#[cfg(not(unix))]
mod place {
    use std::path::Path;

    use super::LocalIssuesError;

    pub(super) struct Place;

    impl Place {
        pub(super) fn lock(_root: &Path, _make: bool) -> Result<Self, LocalIssuesError> {
            Err(LocalIssuesError::Write(
                "the local issues are not written on this system".to_owned(),
            ))
        }

        pub(super) fn read(&self) -> Result<Vec<u8>, LocalIssuesError> {
            Err(LocalIssuesError::Missing)
        }

        pub(super) fn replace(&self, _bytes: &[u8]) -> Result<(), LocalIssuesError> {
            Ok(())
        }
    }
}

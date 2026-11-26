//! The session file on disk: where it is, who may replace it, and how it
//! is replaced so that it is whole at every moment.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::fs::{self, DirBuilder, File, OpenOptions, TryLockError};
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::format::{MAX_FILE_BYTES, Session, Unusable, is_scrollback_key};

/// The session file for the current environment. See
/// [`session_path_from`].
pub fn session_path() -> Option<PathBuf> {
    session_path_from(std::env::var_os("HOME").as_deref())
}

/// `$HOME/Library/Application Support/benterm/session.json`, next to
/// the app's other files. `None` when `HOME` is not an absolute path.
pub fn session_path_from(home: Option<&OsStr>) -> Option<PathBuf> {
    let home = home.map(Path::new).filter(|home| home.is_absolute())?;
    Some(home.join("Library/Application Support/benterm/session.json"))
}

/// What a start found at the place of the session file.
#[derive(Debug)]
pub struct Opened {
    /// The session to make again, if there is one that can be read.
    pub session: Option<Session>,
    /// What to tell the user: why a session that was there is not made
    /// again, or why this one is not saved.
    pub notice: Option<String>,
    /// Replaces the file. `None` when this run must not write: another
    /// app is running, or the file is one a newer build can still read.
    pub store: Option<SessionStore>,
}

/// Marks the moment a snapshot of the session was taken. A snapshot with
/// an earlier ticket never replaces a later one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Ticket(u64);

/// The one writer of a session file. It holds a lock next to the file
/// for as long as it lives, so a second app does not write the same file.
#[derive(Debug)]
pub struct SessionStore {
    path: PathBuf,
    next: AtomicU64,
    /// The ticket of the last snapshot that was written or passed over.
    /// Held for the whole of a write, so that writes do not overlap.
    last: Mutex<u64>,
    /// Held for the whole of a change to a scrollback file.
    scrollback: Mutex<ScrollbackOrder>,
    /// Unlocked by the system when the file is closed, also when the app
    /// is killed.
    _lock: File,
}

/// Which snapshots of scrollback are still let through.
#[derive(Debug, Default)]
struct ScrollbackOrder {
    /// The ticket of the last change to the file of each key.
    last: HashMap<String, u64>,
    /// The ticket at which every file was removed. Nothing from before
    /// it is written.
    cleared: u64,
}

/// A scrollback file larger than this is neither written nor read. The
/// 10,000 rows a pane keeps at most, 250 columns wide and with a color
/// change every few cells, are well below it.
pub const MAX_SCROLLBACK_BYTES: usize = 16 * 1024 * 1024;

const NOT_SAVED: &str = "This session is not saved.";

/// How often the lock is tried, and how long is waited in between,
/// before another app is taken to be running.
const LOCK_TRIES: u32 = 6;
const LOCK_WAIT: std::time::Duration = std::time::Duration::from_millis(10);

/// Reads the session file at `path` and takes the right to replace it.
/// Nothing is written until the store is told to.
pub fn open(path: &Path) -> Opened {
    let not_saved = |notice: String| Opened {
        session: None,
        notice: Some(notice),
        store: None,
    };
    let lock = match lock_beside(path) {
        Ok(lock) => lock,
        Err(TryLockError::WouldBlock) => {
            return not_saved(format!(
                "Another Benterm is running and holds the saved session. {NOT_SAVED}"
            ));
        }
        Err(TryLockError::Error(error)) => {
            return not_saved(format!(
                "The place for the saved session could not be used ({}). {NOT_SAVED}",
                error.kind()
            ));
        }
    };
    let store = SessionStore {
        path: path.to_owned(),
        next: AtomicU64::new(1),
        last: Mutex::new(0),
        scrollback: Mutex::default(),
        _lock: lock,
    };
    let text = match read_limited(path) {
        Ok(Some(text)) => text,
        Ok(None) => {
            return Opened {
                session: None,
                notice: None,
                store: Some(store),
            };
        }
        Err(error) => {
            return not_saved(format!(
                "The saved session could not be read ({}), so it was not restored. {NOT_SAVED}",
                error.kind()
            ));
        }
    };
    let parsed = if text.len() > MAX_FILE_BYTES {
        // Too large to tell what it is: it may be from a newer build.
        Err(Unusable::Newer(format!(
            "it is larger than {MAX_FILE_BYTES} bytes"
        )))
    } else {
        Session::from_json(&text)
    };
    match parsed {
        Ok(session) => Opened {
            session: Some(session),
            notice: None,
            store: Some(store),
        },
        Err(Unusable::Newer(why)) => not_saved(format!(
            "The saved session was not restored, because {why}. \
             It is left as it is, and this session is not saved."
        )),
        Err(Unusable::Corrupt(why)) => {
            let damaged =
                format!("The saved session was not restored, because its file is damaged: {why}.");
            match store.keep_copy() {
                Ok(name) => Opened {
                    session: None,
                    notice: Some(format!("{damaged} A copy of the file is kept as {name}.")),
                    store: Some(store),
                },
                // Without a copy the file is the only one, so it stays.
                Err(error) => not_saved(format!(
                    "{damaged} No copy of it could be kept ({}), so it is left as it is. \
                     {NOT_SAVED}",
                    error.kind()
                )),
            }
        }
    }
}

/// Makes the directory of `path` if it is missing, and locks the lock
/// file in it without waiting.
fn lock_beside(path: &Path) -> Result<File, TryLockError> {
    let open = || {
        let directory = path.parent().ok_or(io::ErrorKind::InvalidInput)?;
        DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(directory)?;
        OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(beside(path, "lock"))
    };
    let lock = open().map_err(TryLockError::Error)?;
    // An app that is going away, or a child of it that has not yet let go
    // of the files it was born with, holds the lock for a moment longer.
    for _ in 1..LOCK_TRIES {
        match lock.try_lock() {
            Err(TryLockError::WouldBlock) => std::thread::sleep(LOCK_WAIT),
            result => return result.map(|()| lock),
        }
    }
    lock.try_lock()?;
    Ok(lock)
}

/// The file's bytes, at most one more than [`MAX_FILE_BYTES`], or `None`
/// if there is no file.
fn read_limited(path: &Path) -> io::Result<Option<Vec<u8>>> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let mut text = Vec::new();
    file.take(MAX_FILE_BYTES as u64 + 1)
        .read_to_end(&mut text)?;
    Ok(Some(text))
}

/// The file next to the session file whose name ends in `suffix`.
pub(crate) fn beside(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_owned();
    name.push(".");
    name.push(suffix);
    path.with_file_name(name)
}

impl SessionStore {
    /// The file the store replaces.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// A ticket for a snapshot taken now. Take it on the thread that
    /// takes the snapshot, before handing the snapshot to another.
    pub fn ticket(&self) -> Ticket {
        Ticket(self.next.fetch_add(1, Ordering::Relaxed))
    }

    /// Replaces the file with `session`, unless a snapshot with a later
    /// ticket has already been written; that is no error.
    ///
    /// The file is whole at every moment: the session goes to a
    /// temporary file that takes the place of the old one in one step.
    /// After an error the file is whole too: the old one, or, if only
    /// the wait for the disk failed, already the new one. A later write
    /// can succeed.
    pub fn write(&self, ticket: Ticket, session: &Session) -> io::Result<()> {
        let text = session
            .to_json()
            .map_err(|refused| io::Error::new(io::ErrorKind::InvalidInput, refused))?;
        let change = || publish(&self.path, text.as_bytes());
        self.in_order(ticket, change, || sync_directory(&self.path))
            .map(drop)
    }

    /// Removes the file, so that the next start has no session to make
    /// again, and then what its panes showed. A snapshot with an earlier
    /// ticket brings neither back.
    pub fn clear(&self, ticket: Ticket) -> io::Result<()> {
        if !self.clear_session(ticket)? {
            // The session that overtook this clearing has its panes.
            return Ok(());
        }
        let mut order = self.scrollback_order();
        order.cleared = order.cleared.max(ticket.0);
        self.remove_scrollback_files(|_| false)
    }

    /// Returns whether the file was removed, and not passed over.
    fn clear_session(&self, ticket: Ticket) -> io::Result<bool> {
        self.in_order(
            ticket,
            || match fs::remove_file(&self.path) {
                Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
                _ => Ok(()),
            },
            || sync_directory(&self.path),
        )
    }

    /// Runs `change`, which changes the file in one step, and then
    /// `sync`, which waits for the disk, unless a later ticket has been
    /// through already. Returns whether they ran.
    ///
    /// Once `change` has succeeded the file is that of this ticket, so
    /// earlier tickets are passed over from then on, also when `sync`
    /// fails. A `change` that failed left the file as it was, and an
    /// earlier ticket may still bring it nearer to now.
    fn in_order(
        &self,
        ticket: Ticket,
        change: impl FnOnce() -> io::Result<()>,
        sync: impl FnOnce() -> io::Result<()>,
    ) -> io::Result<bool> {
        // A write that panicked left the file whole, so go on.
        let mut last = self.last.lock().unwrap_or_else(|error| error.into_inner());
        if ticket.0 <= *last {
            return Ok(false);
        }
        change()?;
        *last = ticket.0;
        sync().map(|()| true)
    }

    /// What the pane with `key` showed, as it was last written. `None`
    /// when there is nothing to show: no such file, a key that is not
    /// one, a file past [`MAX_SCROLLBACK_BYTES`], one that cannot be
    /// read, or something that is not a file, such as a link.
    pub fn scrollback(&self, key: &str) -> Option<Vec<u8>> {
        let path = self.scrollback_file(key)?;
        let file = fs::symlink_metadata(&path).ok()?;
        let ours = self.has_scrollback_directory().ok()?;
        if !ours || !file.is_file() || file.len() > MAX_SCROLLBACK_BYTES as u64 {
            return None;
        }
        let mut bytes = Vec::new();
        let read = File::open(&path)
            .ok()?
            .take(MAX_SCROLLBACK_BYTES as u64 + 1);
        { read }.read_to_end(&mut bytes).ok()?;
        (bytes.len() <= MAX_SCROLLBACK_BYTES).then_some(bytes)
    }

    /// Replaces what the pane with `key` showed, in one step like the
    /// session file, with a file only the user can read. Passed over,
    /// which is no error, when a snapshot of the same pane with a later
    /// ticket has been written, or the pane's file or all files have
    /// been removed since the ticket was taken.
    pub fn write_scrollback(&self, ticket: Ticket, key: &str, bytes: &[u8]) -> io::Result<()> {
        let path = self
            .scrollback_file(key)
            .ok_or(io::ErrorKind::InvalidInput)?;
        if bytes.len() > MAX_SCROLLBACK_BYTES {
            return Err(io::ErrorKind::FileTooLarge.into());
        }
        let mut order = self.scrollback_order();
        if !order.lets_through(ticket, key) {
            return Ok(());
        }
        self.scrollback_directory()?;
        publish(&path, bytes)?;
        order.last.insert(key.to_owned(), ticket.0);
        sync_directory(&path)
    }

    /// Removes what the pane with `key` showed, as when the pane has
    /// been closed. A snapshot with an earlier ticket does not bring it
    /// back.
    pub fn remove_scrollback(&self, ticket: Ticket, key: &str) -> io::Result<()> {
        let path = self
            .scrollback_file(key)
            .ok_or(io::ErrorKind::InvalidInput)?;
        let mut order = self.scrollback_order();
        if !order.lets_through(ticket, key) {
            return Ok(());
        }
        order.last.insert(key.to_owned(), ticket.0);
        if !self.has_scrollback_directory()? {
            return Ok(());
        }
        remove_if_there(&path)?;
        remove_if_there(&beside(&path, "tmp"))
    }

    /// Removes the scrollback files whose keys no pane of `session` has,
    /// all of them without a session, and what interrupted writes left. For the start of the app, when `session` is
    /// what the session file holds: panes are told apart by their keys,
    /// so a file no leaf names will never be read again.
    ///
    /// Files with other names are left alone.
    pub fn keep_scrollback_of(&self, session: Option<&Session>) -> io::Result<()> {
        let keys = session.map(Session::scrollback_keys).unwrap_or_default();
        let _order = self.scrollback_order();
        self.remove_scrollback_files(|key| keys.contains(&key))
    }

    /// Removes the files of the keys that `keep` does not hold, and
    /// every temporary file. The caller holds the lock that every write
    /// holds, so no temporary file is in use.
    fn remove_scrollback_files(&self, keep: impl Fn(&str) -> bool) -> io::Result<()> {
        if !self.has_scrollback_directory()? {
            return Ok(());
        }
        let directory = self.path.with_file_name(SCROLLBACK_DIRECTORY);
        for entry in fs::read_dir(&directory)? {
            let name = entry?.file_name();
            let Some(name) = name.to_str() else { continue };
            let (key, temporary) = match name.strip_suffix(".tmp") {
                Some(key) => (key, true),
                None => (name, false),
            };
            if is_scrollback_key(key) && (temporary || !keep(key)) {
                remove_if_there(&directory.join(name))?;
            }
        }
        Ok(())
    }

    /// Whether the directory of the scrollback files is there and is a
    /// directory. Through a link in its place nothing is read or
    /// removed: it leads to files that are not ours.
    fn has_scrollback_directory(&self) -> io::Result<bool> {
        let directory = self.path.with_file_name(SCROLLBACK_DIRECTORY);
        match fs::symlink_metadata(&directory) {
            Ok(found) => Ok(found.is_dir()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error),
        }
    }

    /// The file of `key`, if it is a key.
    fn scrollback_file(&self, key: &str) -> Option<PathBuf> {
        let directory = self.path.with_file_name(SCROLLBACK_DIRECTORY);
        is_scrollback_key(key).then(|| directory.join(key))
    }

    /// Makes the directory of the scrollback files, which only the user
    /// can list, if it is missing. A link in its place is not followed.
    fn scrollback_directory(&self) -> io::Result<()> {
        let directory = self.path.with_file_name(SCROLLBACK_DIRECTORY);
        match DirBuilder::new().mode(0o700).create(&directory) {
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                if fs::symlink_metadata(&directory)?.is_dir() {
                    Ok(())
                } else {
                    Err(io::ErrorKind::NotADirectory.into())
                }
            }
            made => made,
        }
    }

    fn scrollback_order(&self) -> std::sync::MutexGuard<'_, ScrollbackOrder> {
        // A write that panicked left every file whole, so go on.
        self.scrollback
            .lock()
            .unwrap_or_else(|error| error.into_inner())
    }

    /// Copies the file to one named `.corrupt` beside it, in place of an
    /// older such copy. Returns the name of the copy.
    fn keep_copy(&self) -> io::Result<String> {
        let copy = beside(&self.path, "corrupt");
        let bytes = fs::read(&self.path)?;
        publish(&copy, &bytes)?;
        sync_directory(&copy)?;
        Ok(copy
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned())
    }
}

/// The directory of the scrollback files, beside the session file.
const SCROLLBACK_DIRECTORY: &str = "scrollback";

impl ScrollbackOrder {
    /// Whether a change with `ticket` to the file of `key` is not older
    /// than what the file is already.
    fn lets_through(&self, ticket: Ticket, key: &str) -> bool {
        ticket.0 > self.cleared && self.last.get(key).is_none_or(|last| ticket.0 > *last)
    }
}

fn remove_if_there(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

/// Puts a file with `bytes` in the place of `path` in one step. After
/// an error the file at `path` is as it was.
pub(crate) fn publish(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let temporary = beside(path, "tmp");
    let written = write_new(&temporary, bytes).and_then(|()| fs::rename(&temporary, path));
    if written.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    written
}

/// Writes a new file only the user can read, and waits until it is on
/// disk. Without the wait, a power cut after the rename could leave the
/// name with a file whose bytes never arrived.
///
/// What is at `path` already, as after an interrupted write, is removed
/// and never written into: it may be readable by others, or a link to
/// another file.
fn write_new(path: &Path, bytes: &[u8]) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
        _ => {}
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

/// Waits until the directory of `path` has the change to its names.
pub(crate) fn sync_directory(path: &Path) -> io::Result<()> {
    match path.parent() {
        Some(directory) => File::open(directory)?.sync_all(),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A store on a file in a directory of its own.
    fn store() -> (tempfile::TempDir, SessionStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = open(&dir.path().join("session.json")).store.unwrap();
        (dir, store)
    }

    #[test]
    fn a_change_that_took_effect_keeps_earlier_snapshots_out_though_its_sync_failed() {
        let (_dir, store) = store();
        let earlier = store.ticket();
        let later = store.ticket();
        let failed = store.in_order(later, || Ok(()), || Err(io::ErrorKind::Other.into()));
        assert!(failed.is_err());
        let mut ran = false;
        let skipped = store.in_order(
            earlier,
            || {
                ran = true;
                Ok(())
            },
            || Ok(()),
        );
        assert!(skipped.is_ok());
        assert!(!ran, "the earlier snapshot was passed over");
    }

    #[test]
    fn a_change_that_did_not_take_effect_lets_an_earlier_snapshot_through() {
        let (_dir, store) = store();
        let earlier = store.ticket();
        let later = store.ticket();
        let failed = store.in_order(later, || Err(io::ErrorKind::Other.into()), || Ok(()));
        assert!(failed.is_err());
        let mut ran = false;
        let through = store.in_order(
            earlier,
            || {
                ran = true;
                Ok(())
            },
            || Ok(()),
        );
        assert!(through.is_ok());
        assert!(ran, "what is newer than the file is still written");
    }
}

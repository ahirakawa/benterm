//! File-based mailbox for sandboxed agents whose environment cannot reach the
//! Unix domain socket.
//!
//! Each supported Codex launch gets a fresh private directory in app state:
//! - `peers.json`: atomically updated list of siblings under the same parent
//!   with `allow_peers == true`.
//! - `outbox/`: agent writes `<name>.json.tmp` and renames to `<name>.json`.
//! - `sent/`: accepted messages are moved here.
//! - `rejected/`: invalid or refused messages are moved here alongside
//!   `<name>.err` with the rejection reason.

use std::collections::{BTreeMap, HashSet};
use std::ffi::{CStr, CString};
use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::ops::Bound::{Excluded, Unbounded};
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use benterm_ipc::ReportStatus;
use futures::channel::mpsc::{Receiver, Sender, channel};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use serde::{Deserialize, Serialize};

/// Maximum byte size of one outbox JSON file, unified with `REPORT_LIMIT`.
pub const MAX_MESSAGE_BYTES: usize = crate::agents::REPORT_LIMIT;

/// Fallback poll interval in case `notify` misses an event or cannot watch a
/// directory.
const POLL_INTERVAL: Duration = Duration::from_millis(200);

#[derive(Clone, Copy)]
struct ScanLimits {
    entries_per_mailbox: usize,
    mailboxes_per_poll: usize,
    files_per_mailbox: usize,
    pending_per_mailbox: usize,
    failed_names: usize,
}

const SCAN_LIMITS: ScanLimits = ScanLimits {
    entries_per_mailbox: 256,
    mailboxes_per_poll: 8,
    files_per_mailbox: 8,
    pending_per_mailbox: 4,
    failed_names: 256,
};

/// At most 2 MiB of message data can wait for the UI, since each file is
/// limited to MAX_MESSAGE_BYTES. A mailbox can occupy at most four slots.
const DELIVERY_SLOTS: usize = 32;

/// One entry in `peers.json`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerEntry {
    pub name: String,
    pub pane: u64,
}

/// JSON schema of a message placed in `outbox/*.json`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutboxMessage {
    pub to: String,
    pub text: String,
    #[serde(default)]
    pub status: Option<ReportStatus>,
}

/// Why an outbox file was rejected before reaching the UI thread.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OutboxError {
    Symlink,
    NotRegularFile,
    TooLarge { size: u64, limit: usize },
    Io(String),
    InvalidJson(String),
    EmptyTarget,
    EmptyText,
}

impl fmt::Display for OutboxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Symlink => f.write_str("symbolic links are not allowed in mailbox outbox"),
            Self::NotRegularFile => f.write_str("only regular files are allowed in mailbox outbox"),
            Self::TooLarge { size, limit } => {
                write!(
                    f,
                    "mailbox message is {size} bytes, exceeding limit of {limit} bytes"
                )
            }
            Self::Io(message) => write!(f, "cannot read mailbox message: {message}"),
            Self::InvalidJson(message) => write!(f, "invalid mailbox JSON: {message}"),
            Self::EmptyTarget => f.write_str("mailbox message target `to` must not be empty"),
            Self::EmptyText => f.write_str("mailbox message `text` must not be empty"),
        }
    }
}

impl std::error::Error for OutboxError {}

/// Creates a fresh private mailbox under the app state directory.
pub fn prepare_mailbox(state: &Path) -> io::Result<PathBuf> {
    std::fs::create_dir_all(state)?;
    let dir = tempfile::Builder::new()
        .prefix("mailbox-")
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir_in(state.canonicalize()?)?;
    for name in ["outbox", "sent", "rejected"] {
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(dir.path().join(name))?;
    }
    write_peers(dir.path(), &[])?;
    Ok(dir.keep())
}

fn open_directory(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(path)
}

fn open_at(dir: &File, name: &str, flags: libc::c_int) -> io::Result<File> {
    let name = CString::new(name)?;
    // SAFETY: dir is open, name is NUL-terminated, and the result is owned here.
    let fd = unsafe {
        libc::openat(
            dir.as_raw_fd(),
            name.as_ptr(),
            flags | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            0o600,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}

fn rename_at(source: &File, name: &str, dest: &File, new_name: &str) -> io::Result<()> {
    let name = CString::new(name)?;
    let new_name = CString::new(new_name)?;
    // SAFETY: both descriptors and both names remain valid for the call.
    if unsafe {
        libc::renameat(
            source.as_raw_fd(),
            name.as_ptr(),
            dest.as_raw_fd(),
            new_name.as_ptr(),
        )
    } < 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn write_atomic(dir: &File, temp: &str, name: &str, bytes: &[u8]) -> io::Result<()> {
    let mut file = open_at(dir, temp, libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL)?;
    let result = file
        .write_all(bytes)
        .and_then(|()| rename_at(dir, temp, dir, name));
    if result.is_err() {
        let temp = CString::new(temp)?;
        // SAFETY: dir and temp are valid for the duration of unlinkat.
        unsafe { libc::unlinkat(dir.as_raw_fd(), temp.as_ptr(), 0) };
    }
    result
}

/// Atomically rewrites peers.json without following links.
pub fn write_peers(mailbox_dir: &Path, peers: &[PeerEntry]) -> io::Result<()> {
    let bytes = serde_json::to_vec_pretty(peers)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    write_atomic(
        &open_directory(mailbox_dir)?,
        "peers.json.tmp",
        "peers.json",
        &bytes,
    )
}

/// Removes an agent's mailbox directory when the agent exits.
pub fn cleanup_mailbox(mailbox_dir: &Path) {
    let _ = std::fs::remove_dir_all(mailbox_dir);
}

/// Safely validates and reads one outbox JSON file.
///
/// Security boundary:
/// - `symlink_metadata` rejects symlinks, FIFOs, directories, and oversized files
///   before opening.
/// - `O_NOFOLLOW | O_NONBLOCK` prevents TOCTOU symlink swaps and blocking on
///   FIFOs.
/// - `fstat` (`file.metadata()`) re-verifies `is_file()` and the size limit on
///   the opened descriptor.
/// - Bounded read caps memory at `MAX_MESSAGE_BYTES + 1`.
pub fn read_outbox_file(path: &Path) -> Result<OutboxMessage, OutboxError> {
    let meta =
        std::fs::symlink_metadata(path).map_err(|error| OutboxError::Io(error.to_string()))?;
    if meta.file_type().is_symlink() {
        return Err(OutboxError::Symlink);
    }
    if !meta.file_type().is_file() {
        return Err(OutboxError::NotRegularFile);
    }
    if meta.len() > MAX_MESSAGE_BYTES as u64 {
        return Err(OutboxError::TooLarge {
            size: meta.len(),
            limit: MAX_MESSAGE_BYTES,
        });
    }

    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|error| {
            if error.raw_os_error() == Some(libc::ELOOP) {
                OutboxError::Symlink
            } else {
                OutboxError::Io(error.to_string())
            }
        })?;

    read_message(file)
}

fn read_message(file: File) -> Result<OutboxMessage, OutboxError> {
    let fd_meta = file
        .metadata()
        .map_err(|error| OutboxError::Io(error.to_string()))?;
    if !fd_meta.file_type().is_file() {
        return Err(OutboxError::NotRegularFile);
    }
    if fd_meta.len() > MAX_MESSAGE_BYTES as u64 {
        return Err(OutboxError::TooLarge {
            size: fd_meta.len(),
            limit: MAX_MESSAGE_BYTES,
        });
    }

    let mut bytes = Vec::with_capacity((fd_meta.len() as usize).min(MAX_MESSAGE_BYTES));
    file.take(MAX_MESSAGE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| OutboxError::Io(error.to_string()))?;
    if bytes.len() > MAX_MESSAGE_BYTES {
        return Err(OutboxError::TooLarge {
            size: bytes.len() as u64,
            limit: MAX_MESSAGE_BYTES,
        });
    }

    let message: OutboxMessage = serde_json::from_slice(&bytes)
        .map_err(|error| OutboxError::InvalidJson(error.to_string()))?;
    if message.to.trim().is_empty() {
        return Err(OutboxError::EmptyTarget);
    }
    if message.text.trim().is_empty() {
        return Err(OutboxError::EmptyText);
    }
    Ok(message)
}

fn error_file_name(json_name: &str) -> String {
    let stem = json_name.strip_suffix(".json").unwrap_or(json_name);
    format!("{stem}.err")
}

fn archive_dirs(mailbox_dir: &Path, file_name: &str, dest: &str) -> io::Result<(File, File)> {
    if !file_name.ends_with(".json") || file_name.contains('/') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid outbox file name",
        ));
    }
    let dir = open_directory(mailbox_dir)?;
    let flags = libc::O_RDONLY | libc::O_DIRECTORY;
    Ok((open_at(&dir, "outbox", flags)?, open_at(&dir, dest, flags)?))
}

/// Moves an accepted outbox message to sent.
fn move_to_sent(mailbox_dir: &Path, file_name: &str) -> io::Result<PathBuf> {
    let (outbox, sent) = archive_dirs(mailbox_dir, file_name, "sent")?;
    rename_at(&outbox, file_name, &sent, file_name)?;
    Ok(mailbox_dir.join("sent").join(file_name))
}

/// Moves a rejected message to rejected and records the reason.
fn move_to_rejected(
    mailbox_dir: &Path,
    file_name: &str,
    reason: &str,
) -> io::Result<(PathBuf, PathBuf)> {
    let (outbox, rejected) = archive_dirs(mailbox_dir, file_name, "rejected")?;
    let error_file = error_file_name(file_name);
    write_atomic(
        &rejected,
        &format!("{error_file}.tmp"),
        &error_file,
        format!("{reason}\n").as_bytes(),
    )?;
    rename_at(&outbox, file_name, &rejected, file_name)?;
    let dest = mailbox_dir.join("rejected");
    Ok((dest.join(file_name), dest.join(error_file)))
}

#[derive(Default)]
struct DeliveryState {
    pending: HashSet<String>,
    failed: HashSet<String>,
    disabled: bool,
}

impl DeliveryState {
    fn record_failure(&mut self, name: &str, limit: usize) {
        if self.failed.len() < limit {
            self.failed.insert(name.to_owned());
        } else {
            // A mailbox with too many unarchivable files stays disabled
            // until it is unregistered, so no failed name becomes eligible again.
            self.disabled = true;
        }
    }
}

#[derive(Clone)]
struct WatchedMailbox {
    dir: PathBuf,
    state: Arc<Mutex<DeliveryState>>,
}

type WatchedMailboxes = Arc<Mutex<BTreeMap<u64, WatchedMailbox>>>;

/// A validated outbox message awaiting delivery on the UI thread.
pub struct PendingDelivery {
    pane: u64,
    mailbox: WatchedMailbox,
    file_name: String,
    message: OutboxMessage,
    failed_names: usize,
}

impl PendingDelivery {
    pub fn pane(&self) -> u64 {
        self.pane
    }

    pub fn message(&self) -> &OutboxMessage {
        &self.message
    }

    /// Marks the message as delivered by moving it to `sent/`.
    pub fn accept(self) -> io::Result<PathBuf> {
        let result = move_to_sent(&self.mailbox.dir, &self.file_name);
        self.remember_failure(&result);
        result
    }

    /// Marks the message as rejected by moving it to `rejected/` and writing
    /// `<name>.err`.
    pub fn reject(self, reason: &str) -> io::Result<(PathBuf, PathBuf)> {
        let result = move_to_rejected(&self.mailbox.dir, &self.file_name, reason);
        self.remember_failure(&result);
        result
    }

    fn remember_failure<T>(&self, result: &io::Result<T>) {
        if result.is_err() {
            self.mailbox
                .state
                .lock()
                .unwrap()
                .record_failure(&self.file_name, self.failed_names);
        }
    }
}

impl Drop for PendingDelivery {
    fn drop(&mut self) {
        self.mailbox
            .state
            .lock()
            .unwrap()
            .pending
            .remove(&self.file_name);
    }
}

/// Watches active agent `outbox/` directories with `notify` plus a fallback
/// polling timer, emitting validated [`PendingDelivery`] items to the UI thread.
pub struct MailboxWatcher {
    watcher: RecommendedWatcher,
    watched: WatchedMailboxes,
}

impl MailboxWatcher {
    pub fn start() -> io::Result<(Self, Receiver<PendingDelivery>)> {
        let (wake, raw) = mpsc::sync_channel(1);
        let watcher = notify::recommended_watcher(move |_: notify::Result<notify::Event>| {
            let _ = wake.try_send(());
        })
        .map_err(|error| io::Error::other(error.to_string()))?;
        let watched = WatchedMailboxes::default();
        let watched_thread = watched.clone();
        // futures gives the single sender an additional reserved slot.
        let (mut sender, deliveries) = channel(DELIVERY_SLOTS - 1);

        std::thread::Builder::new()
            .name("benterm-mailbox-watch".into())
            .spawn(move || {
                let mut last = None;
                let mut next_scan = Instant::now();
                loop {
                    if matches!(
                        raw.recv_timeout(POLL_INTERVAL),
                        Err(RecvTimeoutError::Disconnected)
                    ) {
                        return;
                    }
                    std::thread::sleep(next_scan.saturating_duration_since(Instant::now()));
                    next_scan = Instant::now() + POLL_INTERVAL;
                    if !scan_all(&watched_thread, &mut last, &mut sender, SCAN_LIMITS) {
                        return;
                    }
                }
            })?;

        Ok((Self { watcher, watched }, deliveries))
    }

    /// Registers an agent's mailbox directory for outbox watching.
    pub fn watch(&mut self, pane: u64, mailbox_dir: PathBuf) {
        let mut watched = self.watched.lock().unwrap();
        if let Some(mailbox) = watched.get(&pane) {
            if mailbox.dir == mailbox_dir {
                return;
            }
            let _ = self.watcher.unwatch(&mailbox.dir);
        }
        let _ = self
            .watcher
            .watch(&mailbox_dir, RecursiveMode::NonRecursive);
        watched.insert(
            pane,
            WatchedMailbox {
                dir: mailbox_dir,
                state: Arc::default(),
            },
        );
    }

    /// Stops watching an agent's mailbox directory.
    pub fn unwatch(&mut self, pane: u64) {
        if let Some(mailbox) = self.watched.lock().unwrap().remove(&pane) {
            let _ = self.watcher.unwatch(&mailbox.dir);
        }
    }

    pub(crate) fn is_current(&self, delivery: &PendingDelivery) -> bool {
        self.watched
            .lock()
            .unwrap()
            .get(&delivery.pane)
            .is_some_and(|mailbox| Arc::ptr_eq(&mailbox.state, &delivery.mailbox.state))
    }
}

fn scan_all(
    watched: &WatchedMailboxes,
    last: &mut Option<u64>,
    sender: &mut Sender<PendingDelivery>,
    limits: ScanLimits,
) -> bool {
    let snapshot: Vec<_> = {
        let watched = watched.lock().unwrap();
        let start = last.map_or(Unbounded, Excluded);
        watched
            .range((start, Unbounded))
            .chain(watched.iter())
            .take(limits.mailboxes_per_poll.min(watched.len()))
            .map(|(&pane, mailbox)| (pane, mailbox.clone()))
            .collect()
    };
    for (pane, mailbox) in &snapshot {
        match scan_outbox(*pane, mailbox, sender, limits) {
            ScanResult::Continue => *last = Some(*pane),
            ScanResult::Full { queued } => {
                if queued {
                    *last = Some(*pane);
                }
                break;
            }
            ScanResult::Closed => return false,
        }
    }
    !sender.is_closed()
}

fn message_names(outbox: &File, limit: usize) -> io::Result<Vec<String>> {
    let fd = outbox.try_clone()?.into_raw_fd();
    // SAFETY: fd is an owned directory descriptor. fdopendir takes ownership
    // on success, and otherwise leaves it to the caller.
    let stream = unsafe { libc::fdopendir(fd) };
    if stream.is_null() {
        let error = io::Error::last_os_error();
        unsafe { libc::close(fd) };
        return Err(error);
    }
    let mut names = Vec::new();
    let mut entries = 0;
    // SAFETY: the stream is local to this function. Each name is copied
    // before the next readdir call can invalidate the entry.
    unsafe {
        loop {
            let entry = libc::readdir(stream);
            if entry.is_null() {
                break;
            }
            let name = CStr::from_ptr((*entry).d_name.as_ptr());
            if matches!(name.to_bytes(), b"." | b"..") {
                continue;
            }
            if entries == limit {
                libc::closedir(stream);
                return Err(io::Error::other("too many mailbox outbox entries"));
            }
            entries += 1;
            if let Ok(name) = name.to_str()
                && name.ends_with(".json")
            {
                names.push(name.to_owned());
            }
        }
        libc::closedir(stream);
    }
    names.sort();
    Ok(names)
}

#[derive(Debug, PartialEq, Eq)]
enum ScanResult {
    Continue,
    Full { queued: bool },
    Closed,
}

fn scan_outbox(
    pane: u64,
    mailbox: &WatchedMailbox,
    sender: &mut Sender<PendingDelivery>,
    limits: ScanLimits,
) -> ScanResult {
    {
        let state = mailbox.state.lock().unwrap();
        if state.disabled || state.pending.len() >= limits.pending_per_mailbox {
            return ScanResult::Continue;
        }
    }
    let Ok(dir) = open_directory(&mailbox.dir) else {
        return ScanResult::Continue;
    };
    let Ok(outbox) = open_at(&dir, "outbox", libc::O_RDONLY | libc::O_DIRECTORY) else {
        return ScanResult::Continue;
    };
    let Ok(names) = message_names(&outbox, limits.entries_per_mailbox) else {
        return ScanResult::Continue;
    };
    // Charge each attempted file its maximum read size. This also bounds
    // bytes and file work across the mailboxes visited in one poll.
    let mut remaining = limits.files_per_mailbox;
    let mut queued = false;
    for file_name in names {
        {
            let mut state = mailbox.state.lock().unwrap();
            if remaining == 0 || state.disabled || state.pending.len() >= limits.pending_per_mailbox
            {
                break;
            }
            if state.failed.contains(&file_name) || !state.pending.insert(file_name.clone()) {
                continue;
            }
        }
        remaining -= 1;
        match open_at(&outbox, &file_name, libc::O_RDONLY | libc::O_NONBLOCK)
            .map_err(|error| OutboxError::Io(error.to_string()))
            .and_then(read_message)
        {
            Ok(message) => {
                let delivery = PendingDelivery {
                    pane,
                    mailbox: mailbox.clone(),
                    file_name,
                    message,
                    failed_names: limits.failed_names,
                };
                if let Err(error) = sender.try_send(delivery) {
                    return if error.is_full() {
                        ScanResult::Full { queued }
                    } else {
                        ScanResult::Closed
                    };
                }
                queued = true;
            }
            Err(error) => {
                let failed =
                    move_to_rejected(&mailbox.dir, &file_name, &error.to_string()).is_err();
                let mut state = mailbox.state.lock().unwrap();
                if failed {
                    state.record_failure(&file_name, limits.failed_names);
                }
                state.pending.remove(&file_name);
            }
        }
    }
    ScanResult::Continue
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_archive_does_not_deliver_the_same_name_again() {
        let state = tempfile::tempdir().unwrap();
        let dir = prepare_mailbox(state.path()).unwrap();
        std::fs::remove_dir(dir.join("sent")).unwrap();
        std::fs::write(
            dir.join("outbox/message.json"),
            r#"{"to":"parent","text":"done"}"#,
        )
        .unwrap();
        let mailbox = WatchedMailbox {
            dir,
            state: Arc::default(),
        };
        let (mut sender, mut deliveries) = channel(0);
        let limits = ScanLimits {
            entries_per_mailbox: 2,
            files_per_mailbox: 1,
            pending_per_mailbox: 1,
            failed_names: 1,
            ..SCAN_LIMITS
        };
        assert_eq!(
            scan_outbox(1, &mailbox, &mut sender, limits),
            ScanResult::Continue
        );
        assert!(deliveries.try_recv().unwrap().accept().is_err());
        assert_eq!(
            scan_outbox(1, &mailbox, &mut sender, limits),
            ScanResult::Continue
        );
        assert!(deliveries.try_recv().is_err());
    }

    #[test]
    fn a_full_mailbox_cannot_take_the_next_mailboxs_turn() {
        let state = tempfile::tempdir().unwrap();
        let mut mailboxes = BTreeMap::new();
        for pane in 1..=2 {
            let dir = prepare_mailbox(state.path()).unwrap();
            for file in ["first.json", "second.json"] {
                std::fs::write(
                    dir.join("outbox").join(file),
                    r#"{"to":"parent","text":"ok"}"#,
                )
                .unwrap();
            }
            mailboxes.insert(
                pane,
                WatchedMailbox {
                    dir,
                    state: Arc::default(),
                },
            );
        }
        let watched = Arc::new(Mutex::new(mailboxes));
        let mut last = None;
        let (mut sender, mut deliveries) = channel(0);
        let limits = ScanLimits {
            entries_per_mailbox: 2,
            mailboxes_per_poll: 2,
            files_per_mailbox: 2,
            pending_per_mailbox: 1,
            ..SCAN_LIMITS
        };

        for pane in 1..=2 {
            assert!(scan_all(&watched, &mut last, &mut sender, limits));
            let delivery = deliveries.try_recv().unwrap();
            assert_eq!(delivery.pane(), pane);
            assert!(deliveries.try_recv().is_err());
            delivery.accept().unwrap();
        }
    }

    #[test]
    fn directory_handles_refuse_symlinks() {
        let temp = tempfile::tempdir().unwrap();
        let real = temp.path().join("real");
        let link = temp.path().join("link");
        std::fs::create_dir(&real).unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let parent = open_directory(temp.path()).unwrap();

        assert!(open_directory(&real).is_ok());
        assert!(open_directory(&link).is_err());
        assert!(open_at(&parent, "link", libc::O_RDONLY | libc::O_DIRECTORY).is_err());
    }
}

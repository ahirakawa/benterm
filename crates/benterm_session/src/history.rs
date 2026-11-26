//! The history of the agents' states: which agent was in which state in
//! which pane, and from when.
//!
//! The file is beside the session file. Its first line names the format
//! and its version, and every other line is one change of a pane's
//! agent or of the agent's state, as a JSON object, in the order of the
//! changes. A change is added to the end of the file and never changed
//! afterwards, so a crash leaves at most a last line that was cut short,
//! which the next start drops.
//!
//! The version is the history's own and has nothing to do with the
//! session file's. The rules are the same: a build passes over a field it
//! does not know, the version goes up when an older build must not read
//! the file, and an older build leaves a newer file as it is. A line this
//! build cannot read, as with a state it does not know, is passed over
//! and stays in the file until it is too old.
//!
//! A pane is named by the key of its scrollback, which it keeps across
//! restarts, and by its id, which it has only until the app quits.
//!
//! Times are milliseconds since 1970 and go up from each record to the
//! next by at least one, whatever the clock does, so a time names one
//! record and a reader can ask for what came after it.
//!
//! Only a [`History`] made with the [`SessionStore`], and so with the
//! lock of the session file, writes. Records older than the retention
//! time are not read, and the file loses them at the start of the app
//! and once the oldest is a day past its time.

use std::collections::{HashMap, VecDeque};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::format::Agent;
use crate::store::{SessionStore, beside, publish, sync_directory};

/// The version of the history file this build writes, and the newest
/// one it reads.
pub const VERSION: u32 = 1;

/// How long a record is kept when nothing else is said: long enough to
/// look back over a week of work, and little enough to hold in memory. A
/// day of ten busy agents is a few thousand records of some 150 bytes.
pub const RETENTION: Duration = Duration::from_secs(7 * DAY_SECONDS);

const DAY_SECONDS: u64 = 24 * 60 * 60;

/// How far past its time the oldest line of the file is when the file
/// is rewritten without the lines that are too old, and how far after
/// now the time of a line may be at the start.
const DAY_MS: u64 = DAY_SECONDS * 1000;

/// The name of the header's field, which holds the version.
const HEADER: &str = "agent_history";

pub fn history_path(session: &Path) -> PathBuf {
    session.with_file_name("agent_history.jsonl")
}

/// What an agent is doing, as the history has it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum State {
    Working,
    /// Stopped until the user answers.
    Waiting(Waiting),
    /// At its prompt after a turn that finished, or before the first.
    Done,
    /// At its prompt after a turn that did not finish.
    Error,
    /// Nothing has shown the state yet, or, without an agent, there is
    /// no agent in the pane any more.
    Unknown,
}

/// What a waiting agent wants from the user.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Waiting {
    Permission,
    Question,
}

/// The agent of a pane, or its state, as it is from now on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    /// The key of the pane's scrollback.
    pub key: String,
    /// The number behind the pane's id.
    pub pane: u64,
    /// `None` when there is no agent in the pane; the state is then
    /// [`State::Unknown`].
    pub agent: Option<Agent>,
    pub state: State,
}

/// One change, with the time it was recorded at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    /// Milliseconds since 1970. No two records have the same.
    pub at_ms: u64,
    /// The key of the pane's scrollback, which the pane keeps across
    /// restarts.
    pub key: String,
    /// The number behind the id the pane had when this was recorded. A
    /// pane gets another id when the app starts again.
    pub pane: u64,
    /// `None` where the pane's agent went away.
    pub agent: Option<Agent>,
    pub state: State,
}

impl Record {
    /// The time of the record.
    pub fn at(&self) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_millis(self.at_ms)
    }
}

/// A record as a line of the file.
#[derive(Serialize, Deserialize)]
struct Line {
    at_ms: u64,
    key: String,
    pane: u64,
    agent: Option<Agent>,
    state: StateName,
    /// Set exactly when `state` is `waiting`.
    reason: Option<Waiting>,
}

#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum StateName {
    Working,
    Waiting,
    Done,
    Error,
    Unknown,
}

impl Line {
    fn of(record: &Record) -> Self {
        let (state, reason) = match record.state {
            State::Working => (StateName::Working, None),
            State::Waiting(reason) => (StateName::Waiting, Some(reason)),
            State::Done => (StateName::Done, None),
            State::Error => (StateName::Error, None),
            State::Unknown => (StateName::Unknown, None),
        };
        Self {
            at_ms: record.at_ms,
            key: record.key.clone(),
            pane: record.pane,
            agent: record.agent,
            state,
            reason,
        }
    }

    /// `None` for a line that says it waits and not for what.
    fn record(self) -> Option<Record> {
        let state = match self.state {
            StateName::Working => State::Working,
            StateName::Waiting => State::Waiting(self.reason?),
            StateName::Done => State::Done,
            StateName::Error => State::Error,
            StateName::Unknown => State::Unknown,
        };
        Some(Record {
            at_ms: self.at_ms,
            key: self.key,
            pane: self.pane,
            agent: self.agent,
            state,
        })
    }
}

/// The records of the retention time, and the file they are added to.
#[derive(Debug)]
pub struct History {
    retention_ms: u64,
    /// In the order of their times.
    records: VecDeque<Record>,
    /// The last record of each pane that has one.
    last: HashMap<String, Last>,
    /// The time of the last record, in this run or in the file.
    last_ms: u64,
    writer: Option<Writer>,
}

#[derive(Debug)]
struct Last {
    at_ms: u64,
    pane: u64,
    agent: Option<Agent>,
    state: State,
}

/// The right to add to the file, and the file.
#[derive(Debug)]
struct Writer {
    path: PathBuf,
    file: File,
    /// The time of the oldest line of the file.
    oldest_ms: Option<u64>,
    /// The file may end in half a line, which has to go before the next
    /// line is added.
    torn: bool,
    /// The lines that could not be added to the file yet, oldest first,
    /// with their times. They are added before the next line.
    unwritten: Vec<(u64, Vec<u8>)>,
    /// Holds the lock of the session file for as long as this may write.
    _store: Arc<SessionStore>,
}

/// A line of a history file as it was read.
struct Read {
    /// The time, of a line that is a JSON object with one.
    at_ms: Option<u64>,
    /// The record, of a line this build understands.
    record: Option<Record>,
    text: String,
}

/// What is at the place of a history file.
enum Found {
    Nothing,
    /// A history this build reads. `whole` is false when the file does
    /// not end with the end of a line.
    Lines {
        lines: Vec<Read>,
        whole: bool,
    },
    /// A history of a version this build does not know.
    Newer(u64),
    /// Not a history.
    Damaged(Vec<u8>),
}

fn millis(time: SystemTime) -> u64 {
    let since = time.duration_since(SystemTime::UNIX_EPOCH);
    since.map_or(0, |since| {
        u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
    })
}

/// How many lines wait at most for a file that cannot be written. The
/// oldest are given up for the newest.
const MAX_UNWRITTEN: usize = 4096;

/// Reads what is at `path`. A line with a time that is not after the
/// time of the line before it, or that is after `latest_ms`, is read as
/// a line without a time: the times of a history go up, and one from
/// the far future would keep every later record up there with it.
fn find(path: &Path, latest_ms: u64) -> io::Result<Found> {
    match fs::symlink_metadata(path) {
        Ok(found) if found.is_file() => {}
        // Through a link, files that are not ours would be written.
        Ok(_) => return Err(io::Error::other("it is not a file")),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Found::Nothing),
        Err(error) => return Err(error),
    }
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Found::Nothing),
        Err(error) => return Err(error),
    };
    if bytes.is_empty() {
        return Ok(Found::Nothing);
    }
    let whole = bytes.ends_with(b"\n");
    let mut texts: Vec<&[u8]> = bytes.split(|byte| *byte == b'\n').collect();
    // What follows the last end of a line: nothing, or half a line.
    texts.pop();
    let version = texts
        .first()
        .and_then(|header| serde_json::from_slice::<Value>(header).ok())
        .and_then(|header| header.as_object()?.get(HEADER)?.as_u64())
        .filter(|version| *version > 0);
    let Some(version) = version else {
        return Ok(Found::Damaged(bytes));
    };
    if version > u64::from(VERSION) {
        return Ok(Found::Newer(version));
    }
    let mut before = None;
    let lines = texts[1..]
        .iter()
        .map(|text| {
            let mut line = read(text);
            match line.at_ms {
                Some(at_ms) if before.is_none_or(|before| at_ms > before) && at_ms <= latest_ms => {
                    before = Some(at_ms);
                }
                _ => {
                    line.at_ms = None;
                    line.record = None;
                }
            }
            line
        })
        .collect();
    Ok(Found::Lines { lines, whole })
}

fn read(text: &[u8]) -> Read {
    let text = String::from_utf8_lossy(text).into_owned();
    // Nearly every line is a record, so that is tried first.
    let (at_ms, record) = match serde_json::from_str::<Line>(&text) {
        Ok(line) => (Some(line.at_ms), line.record()),
        Err(_) => {
            let value = serde_json::from_str::<Value>(&text).ok();
            let at_ms = value.and_then(|value| value.as_object()?.get("at_ms")?.as_u64());
            (at_ms, None)
        }
    };
    Read {
        at_ms,
        record,
        text,
    }
}

fn header() -> String {
    format!("{{\"{HEADER}\":{VERSION}}}\n")
}

/// Puts a history with the lines `texts` in the place of `path`, in one
/// step, and opens it to add to.
fn replace<'a>(path: &Path, texts: impl Iterator<Item = &'a str>) -> io::Result<File> {
    let mut bytes = header().into_bytes();
    for text in texts {
        bytes.extend_from_slice(text.as_bytes());
        bytes.push(b'\n');
    }
    publish(path, &bytes)?;
    sync_directory(path)?;
    append_to(path)
}

fn append_to(path: &Path) -> io::Result<File> {
    OpenOptions::new().append(true).open(path)
}

impl History {
    /// A history that is in no file: for an app without a session file.
    pub fn unsaved(retention: Duration) -> Self {
        Self {
            retention_ms: u64::try_from(retention.as_millis()).unwrap_or(u64::MAX),
            records: VecDeque::new(),
            last: HashMap::new(),
            last_ms: 0,
            writer: None,
        }
    }

    /// The history beside the session file at `session`, for an app that
    /// does not hold the session's lock: it reads what is there at `now`
    /// and never writes. What is recorded is kept in memory.
    pub fn read_only(session: &Path, retention: Duration, now: SystemTime) -> Self {
        let mut history = Self::unsaved(retention);
        let now_ms = millis(now);
        let found = find(&history_path(session), now_ms.saturating_add(DAY_MS));
        if let Ok(Found::Lines { lines, .. }) = found {
            history.take(&lines, now_ms);
            // The panes of the file are those of the app that writes
            // it, and are not this app's to say anything about.
            history.last.clear();
        }
        history
    }

    /// The history beside the session file of `store`, which is written
    /// from now on, for as long as this lives.
    ///
    /// The file loses the lines that are too old at `now`, and what a
    /// crash left of a last line. A pane whose last state says that an
    /// agent is there gets a record that the agent is gone: the app was
    /// closed or has crashed since, and no agent outlives it. That
    /// record has the time after the last one of the file, the last
    /// moment the app is known to have run.
    ///
    /// A file of a newer build is left as it is, and one that is not a
    /// history is kept as a copy named `.corrupt`. A line whose time
    /// does not come after the time of the line before it, or is more
    /// than a day after `now`, is dropped. Whenever the file cannot be
    /// used, the history is kept in memory alone, with what could be
    /// read of the file, and the reason is printed.
    pub fn open(store: Arc<SessionStore>, retention: Duration, now: SystemTime) -> Self {
        let mut history = Self::unsaved(retention);
        let path = history_path(store.path());
        match history.start(&path, millis(now)) {
            Ok(Some((file, oldest_ms))) => {
                history.writer = Some(Writer {
                    path,
                    file,
                    oldest_ms,
                    torn: false,
                    unwritten: Vec::new(),
                    _store: store,
                });
                history.end_all_at(0);
            }
            Ok(None) => {}
            Err(error) => {
                eprintln!("benterm: the agent history is not saved: {error}");
            }
        }
        history
    }

    /// Reads the file, makes it one that can be added to, and opens it.
    /// Returns the time of its oldest line too. `None` when the file is
    /// to be left alone.
    fn start(&mut self, path: &Path, now_ms: u64) -> io::Result<Option<(File, Option<u64>)>> {
        let (lines, whole) = match find(path, now_ms.saturating_add(DAY_MS))? {
            Found::Nothing => return Ok(Some((replace(path, std::iter::empty())?, None))),
            Found::Newer(version) => {
                eprintln!(
                    "benterm: the agent history is of a newer Benterm (version {version}): \
                     it is left as it is, and this run's history is not saved"
                );
                return Ok(None);
            }
            Found::Damaged(bytes) => {
                // Without a copy the file is the only one, so it stays.
                let copy = beside(path, "corrupt");
                publish(&copy, &bytes)?;
                eprintln!(
                    "benterm: the agent history was damaged: a copy is kept as {}",
                    copy.file_name().unwrap_or_default().to_string_lossy()
                );
                return Ok(Some((replace(path, std::iter::empty())?, None)));
            }
            Found::Lines { lines, whole } => (lines, whole),
        };
        // First, so that what was read is there even if the file cannot
        // be made fit to add to.
        self.take(&lines, now_ms);
        let cutoff = now_ms.saturating_sub(self.retention_ms);
        let stays = |line: &Read| line.at_ms.is_some_and(|at_ms| at_ms >= cutoff);
        let file = if whole && lines.iter().all(stays) {
            append_to(path)?
        } else {
            let kept = lines.iter().filter(|line| stays(line));
            replace(path, kept.map(|line| line.text.as_str()))?
        };
        let oldest_ms = lines
            .iter()
            .filter(|line| stays(line))
            .find_map(|l| l.at_ms);
        Ok(Some((file, oldest_ms)))
    }

    /// Takes the lines of a file as the records so far.
    fn take(&mut self, lines: &[Read], now_ms: u64) {
        let cutoff = now_ms.saturating_sub(self.retention_ms);
        for line in lines {
            // Also of the lines that are passed over: the next record
            // comes after every line of the file.
            self.last_ms = self.last_ms.max(line.at_ms.unwrap_or(0));
            let Some(record) = &line.record else { continue };
            if record.at_ms >= cutoff {
                self.keep(record.clone());
            }
        }
    }

    fn keep(&mut self, record: Record) {
        if (record.agent, record.state) == (None, State::Unknown) {
            // As a pane that never had a record.
            self.last.remove(&record.key);
        } else {
            self.last.insert(
                record.key.clone(),
                Last {
                    at_ms: record.at_ms,
                    pane: record.pane,
                    agent: record.agent,
                    state: record.state,
                },
            );
        }
        self.records.push_back(record);
    }

    /// Whether what is recorded goes to the file.
    pub fn is_saved(&self) -> bool {
        self.writer.is_some()
    }

    /// Records `change` as seen at `now`, unless the pane's last record
    /// says the same already. A pane without a record is taken to have
    /// no agent. Returns the record that was made.
    ///
    /// The record's time is `now`, or the millisecond after the last
    /// record's when the clock is not past that.
    pub fn record(&mut self, change: Change, now: SystemTime) -> Option<Record> {
        self.record_at(change, millis(now))
    }

    fn record_at(&mut self, change: Change, now_ms: u64) -> Option<Record> {
        let said = match self.last.get(&change.key) {
            Some(last) => (last.agent, last.state),
            None => (None, State::Unknown),
        };
        if said == (change.agent, change.state) {
            return None;
        }
        let record = Record {
            at_ms: now_ms.max(self.last_ms.saturating_add(1)),
            key: change.key,
            pane: change.pane,
            agent: change.agent,
            state: change.state,
        };
        self.last_ms = record.at_ms;
        let cutoff = now_ms.saturating_sub(self.retention_ms);
        while self.records.front().is_some_and(|old| old.at_ms < cutoff) {
            self.records.pop_front();
        }
        if let Some(writer) = &mut self.writer {
            writer.add(&record, cutoff);
        }
        self.keep(record.clone());
        Some(record)
    }

    /// Records that the agent of the pane with `key` is gone, as when
    /// the pane is closed.
    pub fn end(&mut self, key: &str, now: SystemTime) {
        if let Some(last) = self.last.get(key) {
            let gone = Change {
                key: key.to_owned(),
                pane: last.pane,
                agent: None,
                state: State::Unknown,
            };
            self.record_at(gone, millis(now));
        }
    }

    /// Records that the agent of every pane is gone, as when the app
    /// quits.
    pub fn end_all(&mut self, now: SystemTime) {
        self.end_all_at(millis(now));
    }

    fn end_all_at(&mut self, now_ms: u64) {
        let mut keys: Vec<(u64, String)> = self
            .last
            .iter()
            .map(|(key, last)| (last.at_ms, key.clone()))
            .collect();
        // In the order the panes were last heard of, so that the same
        // file always gives the same records.
        keys.sort();
        for (_, key) in keys {
            let pane = self.last[&key].pane;
            let gone = Change {
                key,
                pane,
                agent: None,
                state: State::Unknown,
            };
            self.record_at(gone, now_ms);
        }
    }

    /// The records that are not older than the retention time at `now`,
    /// oldest first.
    pub fn records(&self, now: SystemTime) -> Vec<Record> {
        self.records_since(0, now)
    }

    /// Those of [`records`](Self::records) with a time after `at_ms`.
    pub fn records_since(&self, at_ms: u64, now: SystemTime) -> Vec<Record> {
        let cutoff = millis(now).saturating_sub(self.retention_ms);
        // The times go up, so everything from the first that counts on.
        let from = self
            .records
            .partition_point(|record| record.at_ms < cutoff || record.at_ms <= at_ms);
        self.records.iter().skip(from).cloned().collect()
    }
}

impl Writer {
    /// Adds `record` to the end of the file. Before that, the file loses
    /// the lines from before `cutoff_ms`, once its oldest is a day
    /// older.
    ///
    /// A failure is printed. The record then waits, and is added before
    /// the next one, after the file has been put right: it may end in
    /// half a line.
    fn add(&mut self, record: &Record, cutoff_ms: u64) {
        match serde_json::to_vec(&Line::of(record)) {
            Ok(mut line) => {
                line.push(b'\n');
                self.unwritten.push((record.at_ms, line));
            }
            Err(error) => eprintln!("benterm: could not save the agent history: {error}"),
        }
        let due = self
            .oldest_ms
            .is_some_and(|oldest| oldest < cutoff_ms.saturating_sub(DAY_MS));
        let added = (|| {
            if due || self.torn {
                self.rewrite(cutoff_ms)?;
            }
            // One write for whole lines, so that no reader sees half.
            let lines: Vec<u8> = self
                .unwritten
                .iter()
                .flat_map(|(_, line)| line)
                .copied()
                .collect();
            self.file.write_all(&lines)
        })();
        match added {
            Ok(()) => {
                if let Some((first, _)) = self.unwritten.first() {
                    self.oldest_ms.get_or_insert(*first);
                }
                self.unwritten.clear();
            }
            Err(error) => {
                if !self.torn {
                    eprintln!("benterm: could not save the agent history: {error}");
                }
                self.torn = true;
                let over = self.unwritten.len().saturating_sub(MAX_UNWRITTEN);
                self.unwritten.drain(..over);
            }
        }
    }

    /// Replaces the file by one with its whole lines from `cutoff_ms`
    /// on. The lines are read from the file, which also has the ones
    /// this build passed over. The lines that wait and are in the file
    /// already, from a write that failed half way, wait no longer.
    fn rewrite(&mut self, cutoff_ms: u64) -> io::Result<()> {
        let Found::Lines { lines, .. } = find(&self.path, u64::MAX)? else {
            // Somebody else put another file there. It is not ours to
            // replace.
            return Err(io::ErrorKind::InvalidData.into());
        };
        let stays = |line: &&Read| line.at_ms.is_some_and(|at_ms| at_ms >= cutoff_ms);
        let kept = lines.iter().filter(stays);
        self.file = replace(&self.path, kept.map(|line| line.text.as_str()))?;
        self.oldest_ms = lines.iter().filter(stays).find_map(|line| line.at_ms);
        self.torn = false;
        if let Some(written) = lines.iter().filter_map(|line| line.at_ms).next_back() {
            self.unwritten.retain(|(at_ms, _)| *at_ms > written);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn working(key: &str) -> Change {
        Change {
            key: key.to_owned(),
            pane: 1,
            agent: Some(Agent::Claude),
            state: State::Working,
        }
    }

    /// A history whose file takes no writes, as on a full disk, and the
    /// file as it took them before.
    fn stuck(dir: &tempfile::TempDir, now: SystemTime) -> (History, File, PathBuf) {
        let session = dir.path().join("session.json");
        let store = Arc::new(crate::open(&session).store.unwrap());
        let mut history = History::open(store, RETENTION, now);
        history.record(working("a"), now);
        let path = history_path(&session);
        let writer = history.writer.as_mut().unwrap();
        let good = std::mem::replace(&mut writer.file, File::open(&path).unwrap());
        (history, good, path)
    }

    fn now() -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000)
    }

    fn assert_file_has(history: &History, dir: &tempfile::TempDir, count: usize) {
        let later = now() + Duration::from_secs(1);
        let session = dir.path().join("session.json");
        let read = History::read_only(&session, RETENTION, later);
        assert_eq!(read.records(later), history.records(later));
        assert_eq!(read.records(later).len(), count);
    }

    #[test]
    fn a_record_that_could_not_be_written_is_written_before_the_next() {
        let dir = tempfile::tempdir().unwrap();
        let (mut history, mut good, path) = stuck(&dir, now());

        history.record(working("b"), now());
        assert_eq!(fs::read_to_string(&path).unwrap().lines().count(), 2);
        let writer = history.writer.as_mut().unwrap();
        assert!(writer.torn);
        // Half of the line got there before the disk was full.
        good.write_all(&writer.unwritten[0].1[..10]).unwrap();

        // The file is put right, and takes both in their order.
        history.record(working("c"), now());
        assert!(history.writer.as_ref().unwrap().unwritten.is_empty());
        assert_file_has(&history, &dir, 3);
        assert_eq!(fs::read_to_string(&path).unwrap().lines().count(), 4);
    }

    #[test]
    fn a_line_that_got_to_the_file_though_the_write_failed_is_not_written_twice() {
        let dir = tempfile::tempdir().unwrap();
        let (mut history, mut good, path) = stuck(&dir, now());

        history.record(working("b"), now());
        // The whole line got there, and the failure came after.
        let writer = history.writer.as_mut().unwrap();
        good.write_all(&writer.unwritten[0].1).unwrap();

        history.record(working("c"), now());
        assert_file_has(&history, &dir, 3);
        assert_eq!(fs::read_to_string(&path).unwrap().lines().count(), 4);
    }
}

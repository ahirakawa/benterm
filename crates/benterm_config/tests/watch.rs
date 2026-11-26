//! These tests use the real file system and wait for its events.

use benterm_config::{Config, ConfigEvent, ConfigWatcher, Loaded, parse};
use std::fs;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

const TIMEOUT: Duration = Duration::from_secs(10);
const FULL: &str = "[palette]\naccent = \"#ff0000\"\n\n[terminal]\nfont_size = 14\npadding = 4\n";

fn start(path: &std::path::Path) -> (ConfigWatcher, Loaded, Receiver<ConfigEvent>) {
    let (tx, rx) = mpsc::channel();
    let (watcher, loaded) = ConfigWatcher::start(path, move |e| {
        let _ = tx.send(e);
    })
    .unwrap();
    (watcher, loaded, rx)
}

/// Waits for an event that satisfies `pred` and returns the events seen
/// before it, then that event.
fn wait_for(
    rx: &Receiver<ConfigEvent>,
    pred: impl Fn(&ConfigEvent) -> bool,
) -> (Vec<ConfigEvent>, ConfigEvent) {
    let deadline = Instant::now() + TIMEOUT;
    let mut before = Vec::new();
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(left) {
            Ok(e) if pred(&e) => return (before, e),
            Ok(e) => before.push(e),
            Err(e) => panic!("no matching event ({e:?}); saw {before:?}"),
        }
    }
}

fn applied(expected: &Loaded) -> impl Fn(&ConfigEvent) -> bool + '_ {
    move |e| matches!(e, ConfigEvent::Applied(l) if l == expected)
}

#[test]
fn start_reads_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    fs::write(&path, FULL).unwrap();
    let (_w, loaded, _rx) = start(&path);
    assert_eq!(loaded, parse(FULL).unwrap());
}

#[test]
fn start_creates_a_missing_directory() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("benterm").join("config.toml");
    let (_w, loaded, _rx) = start(&path);
    assert!(path.parent().unwrap().is_dir());
    assert_eq!(loaded.config, Config::default());
}

#[test]
fn saving_the_file_applies_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let (_w, _, rx) = start(&path);
    fs::write(&path, FULL).unwrap();
    wait_for(&rx, applied(&parse(FULL).unwrap()));
}

#[test]
fn saving_by_renaming_a_temporary_file_applies_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    fs::write(&path, "").unwrap();
    let (_w, _, rx) = start(&path);
    let tmp = dir.path().join(".config.toml.tmp");
    fs::write(&tmp, FULL).unwrap();
    fs::rename(&tmp, &path).unwrap();
    wait_for(&rx, applied(&parse(FULL).unwrap()));
}

#[test]
fn a_broken_save_is_rejected_and_a_later_good_one_applied() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let (_w, _, rx) = start(&path);
    fs::write(&path, "[palette\n").unwrap();
    wait_for(&rx, |e| matches!(e, ConfigEvent::Rejected(_)));
    fs::write(&path, FULL).unwrap();
    wait_for(&rx, applied(&parse(FULL).unwrap()));
}

#[test]
fn a_save_seen_halfway_never_applies_a_partial_config() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let (_w, _, rx) = start(&path);
    let expected = parse(FULL).unwrap();
    // FULL[..40] ends after "[terminal]", so it is valid TOML on its own.
    // The pauses are long enough for each write to raise its own event, and
    // shorter than the time the watcher waits for the file to settle.
    for cut in [40, 25, 40] {
        fs::write(&path, &FULL[..cut]).unwrap();
        std::thread::sleep(Duration::from_millis(20));
    }
    fs::write(&path, FULL).unwrap();
    let (before, _) = wait_for(&rx, applied(&expected));
    for e in before {
        assert!(
            !matches!(e, ConfigEvent::Applied(_)),
            "a partial file was applied: {e:?}"
        );
    }
}

#[test]
fn changes_to_other_files_in_the_directory_are_ignored() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let (_w, _, rx) = start(&path);
    fs::write(dir.path().join("other.toml"), "[palette\n").unwrap();
    assert_eq!(
        rx.recv_timeout(Duration::from_millis(500)).err(),
        Some(RecvTimeoutError::Timeout)
    );
}

#[test]
fn dropping_the_watcher_stops_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    let (w, _, rx) = start(&path);
    drop(w);
    // The callback, and with it the sender, is gone once the thread ends.
    assert_eq!(
        rx.recv_timeout(TIMEOUT).err(),
        Some(RecvTimeoutError::Disconnected)
    );
}

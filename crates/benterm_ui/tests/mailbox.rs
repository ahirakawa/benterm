//! File-based mailbox tests: directory layout, atomic `peers.json` updates,
//! security boundaries (symlinks, FIFOs, directories, size limits, malformed
//! JSON, empty text), and watcher delivery (`outbox/` -> `sent/` or `rejected/`).

use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::symlink;
use std::time::{Duration, Instant};

use benterm_ipc::ReportStatus;
use benterm_ui::mailbox::{
    MAX_MESSAGE_BYTES, MailboxWatcher, OutboxError, OutboxMessage, PeerEntry, cleanup_mailbox,
    prepare_mailbox, read_outbox_file, write_peers,
};

#[test]
fn preparation_allocates_fresh_empty_outboxes() {
    let state = tempfile::tempdir().unwrap();
    let first = prepare_mailbox(state.path()).unwrap();
    std::fs::write(first.join("outbox").join("old.json"), b"old output").unwrap();
    let second = prepare_mailbox(state.path()).unwrap();
    assert_ne!(first, second);
    assert_eq!(first.parent(), second.parent());
    assert!(first.join("outbox").join("old.json").exists());
    assert!(second.join("outbox").read_dir().unwrap().next().is_none());
}

#[test]
fn everything_in_a_mailbox_is_the_owners_alone() {
    use std::os::unix::fs::PermissionsExt;
    let state = tempfile::tempdir().unwrap();
    let dir = prepare_mailbox(state.path()).unwrap();
    write_peers(&dir, &[]).unwrap();

    let mode = |path: &std::path::Path| path.metadata().unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&dir), 0o700);
    for name in ["outbox", "sent", "rejected"] {
        assert_eq!(mode(&dir.join(name)), 0o700, "{name}");
    }
    assert_eq!(mode(&dir.join("peers.json")), 0o600);
}

#[test]
fn peer_index_preserves_an_existing_staging_file() {
    let state = tempfile::tempdir().unwrap();
    let dir = prepare_mailbox(state.path()).unwrap();
    let temp = dir.join("peers.json.tmp");
    std::fs::write(&temp, b"reserved").unwrap();

    assert!(write_peers(&dir, &[]).is_err());
    assert_eq!(std::fs::read(&temp).unwrap(), b"reserved");
    assert_eq!(std::fs::read(dir.join("peers.json")).unwrap(), b"[]");
}

#[test]
fn prepare_and_cleanup_mailbox_manages_layout_and_peers_json() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = prepare_mailbox(tmp.path()).unwrap();

    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        dir.parent(),
        Some(tmp.path().canonicalize().unwrap().as_path())
    );
    assert_eq!(dir.metadata().unwrap().permissions().mode() & 0o777, 0o700);
    assert!(dir.join("outbox").is_dir());
    assert!(dir.join("sent").is_dir());
    assert!(dir.join("rejected").is_dir());

    let initial: Vec<PeerEntry> =
        serde_json::from_str(&std::fs::read_to_string(dir.join("peers.json")).unwrap()).unwrap();
    assert!(initial.is_empty());

    let peers = vec![
        PeerEntry {
            name: "alice".into(),
            pane: 10,
        },
        PeerEntry {
            name: "bob".into(),
            pane: 11,
        },
    ];
    write_peers(&dir, &peers).unwrap();
    assert!(!dir.join("peers.json.tmp").exists());
    let updated: Vec<PeerEntry> =
        serde_json::from_str(&std::fs::read_to_string(dir.join("peers.json")).unwrap()).unwrap();
    assert_eq!(updated, peers);

    cleanup_mailbox(&dir);
    assert!(!dir.exists());
}

#[test]
fn read_outbox_file_parses_valid_peer_and_parent_messages() {
    let tmp = tempfile::tempdir().unwrap();
    let peer_path = tmp.path().join("1.json");
    std::fs::write(&peer_path, r#"{"to":"bob","text":"hello peer"}"#).unwrap();
    assert_eq!(
        read_outbox_file(&peer_path).unwrap(),
        OutboxMessage {
            to: "bob".into(),
            text: "hello peer".into(),
            status: None,
        }
    );

    let parent_path = tmp.path().join("2.json");
    std::fs::write(
        &parent_path,
        r#"{"to":"parent","text":"finished","status":"pass"}"#,
    )
    .unwrap();
    assert_eq!(
        read_outbox_file(&parent_path).unwrap(),
        OutboxMessage {
            to: "parent".into(),
            text: "finished".into(),
            status: Some(ReportStatus::Pass),
        }
    );
}

#[test]
fn read_outbox_file_rejects_symlink_fifo_directory_and_oversized_file() {
    let tmp = tempfile::tempdir().unwrap();

    // Symlink
    let secret = tmp.path().join("secret.txt");
    std::fs::write(&secret, r#"{"to":"parent","text":"secret"}"#).unwrap();
    let link = tmp.path().join("link.json");
    symlink(&secret, &link).unwrap();
    assert!(matches!(read_outbox_file(&link), Err(OutboxError::Symlink)));

    // FIFO (must not block)
    let fifo = tmp.path().join("pipe.json");
    let c_fifo = CString::new(fifo.as_os_str().as_bytes()).unwrap();
    // SAFETY: valid nul-terminated path and standard permissions.
    assert_eq!(unsafe { libc::mkfifo(c_fifo.as_ptr(), 0o600) }, 0);
    assert!(matches!(
        read_outbox_file(&fifo),
        Err(OutboxError::NotRegularFile)
    ));

    // Directory
    let dir_entry = tmp.path().join("dir.json");
    std::fs::create_dir(&dir_entry).unwrap();
    assert!(matches!(
        read_outbox_file(&dir_entry),
        Err(OutboxError::NotRegularFile)
    ));

    // Oversized (> REPORT_LIMIT / 64 KiB)
    let big = tmp.path().join("big.json");
    let payload = "x".repeat(MAX_MESSAGE_BYTES + 1);
    std::fs::write(&big, payload).unwrap();
    assert!(matches!(
        read_outbox_file(&big),
        Err(OutboxError::TooLarge { .. })
    ));
}

#[test]
fn read_outbox_file_rejects_malformed_json_and_empty_fields() {
    let tmp = tempfile::tempdir().unwrap();

    let bad_json = tmp.path().join("bad.json");
    std::fs::write(&bad_json, "{not valid json").unwrap();
    assert!(matches!(
        read_outbox_file(&bad_json),
        Err(OutboxError::InvalidJson(_))
    ));

    let empty_to = tmp.path().join("empty_to.json");
    std::fs::write(&empty_to, r#"{"to":"   ","text":"hello"}"#).unwrap();
    assert!(matches!(
        read_outbox_file(&empty_to),
        Err(OutboxError::EmptyTarget)
    ));

    let empty_text = tmp.path().join("empty_text.json");
    std::fs::write(&empty_text, r#"{"to":"bob","text":"  \n\t "}"#).unwrap();
    assert!(matches!(
        read_outbox_file(&empty_text),
        Err(OutboxError::EmptyText)
    ));
}

#[test]
fn watcher_delivers_valid_messages_ignores_tmp_and_rejects_invalid_entries() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = prepare_mailbox(tmp.path()).unwrap();
    let outbox = dir.join("outbox");

    let (mut watcher, mut rx) = MailboxWatcher::start().unwrap();
    watcher.watch(7, dir.clone());

    // `.tmp` file must be ignored and stay in outbox.
    let tmp_file = outbox.join("draft.json.tmp");
    std::fs::write(&tmp_file, r#"{"to":"bob","text":"not ready"}"#).unwrap();

    // Malformed JSON must be moved to rejected/ with .err.
    std::fs::write(outbox.join("bad.json"), "not-json").unwrap();

    // Symlink must be moved to rejected/ with .err.
    let outside = tmp.path().join("outside.json");
    std::fs::write(&outside, r#"{"to":"parent","text":"nope"}"#).unwrap();
    symlink(&outside, outbox.join("link.json")).unwrap();

    // Oversized file must be moved to rejected/ with .err.
    std::fs::write(outbox.join("huge.json"), "a".repeat(MAX_MESSAGE_BYTES + 10)).unwrap();

    // Valid message written atomically via tmp -> rename.
    let stage = outbox.join("0001.json.tmp");
    std::fs::write(&stage, r#"{"to":"bob","text":"ping"}"#).unwrap();
    std::fs::rename(&stage, outbox.join("0001.json")).unwrap();

    // Second valid message that we will reject at delivery time.
    let stage2 = outbox.join("0002.json.tmp");
    std::fs::write(&stage2, r#"{"to":"carol","text":"nope"}"#).unwrap();
    std::fs::rename(&stage2, outbox.join("0002.json")).unwrap();

    let wait_next = |rx: &mut futures::channel::mpsc::Receiver<_>| {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Ok(item) = rx.try_recv() {
                return item;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for mailbox delivery"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    };

    let first = wait_next(&mut rx);
    assert_eq!(first.pane(), 7);
    assert_eq!(
        first.message(),
        &OutboxMessage {
            to: "bob".into(),
            text: "ping".into(),
            status: None,
        }
    );
    first.accept().unwrap();
    assert!(dir.join("sent").join("0001.json").is_file());
    assert!(!outbox.join("0001.json").exists());

    let second = wait_next(&mut rx);
    assert_eq!(second.pane(), 7);
    assert_eq!(second.message().to, "carol");
    second.reject("peer not allowed").unwrap();
    assert!(dir.join("rejected").join("0002.json").is_file());
    assert!(
        std::fs::read_to_string(dir.join("rejected").join("0002.err"))
            .unwrap()
            .contains("peer not allowed")
    );

    // Wait briefly to confirm invalid files were rejected and draft.json.tmp remains untouched.
    let deadline = Instant::now() + Duration::from_secs(5);
    while !(dir.join("rejected").join("bad.err").exists()
        && dir.join("rejected").join("link.err").exists()
        && dir.join("rejected").join("huge.err").exists())
    {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for rejections"
        );
        std::thread::sleep(Duration::from_millis(20));
    }

    assert!(dir.join("rejected").join("bad.json").exists());
    assert!(
        dir.join("rejected")
            .join("link.json")
            .symlink_metadata()
            .is_ok()
    );
    assert!(dir.join("rejected").join("huge.json").exists());
    assert!(tmp_file.is_file(), ".tmp file must be ignored");
}

/// Links that an agent puts in its mailbox lead nowhere. What
/// the app writes stays in the mailbox, and nothing the links point at is
/// made or changed.
mod links_in_the_mailbox {
    use super::*;
    use std::path::{Path, PathBuf};

    fn fresh(state: &Path) -> PathBuf {
        prepare_mailbox(state).unwrap()
    }

    fn next(
        rx: &mut futures::channel::mpsc::Receiver<benterm_ui::mailbox::PendingDelivery>,
    ) -> benterm_ui::mailbox::PendingDelivery {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Ok(item) = rx.try_recv() {
                return item;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for mailbox delivery"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// The watcher has had time for several scans.
    fn scans() {
        std::thread::sleep(Duration::from_millis(1500));
    }

    fn entries(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn an_accepted_message_is_not_moved_through_a_linked_sent_folder() {
        let state = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let dir = fresh(state.path());
        std::fs::remove_dir(dir.join("sent")).unwrap();
        symlink(outside.path(), dir.join("sent")).unwrap();

        let (mut watcher, mut rx) = MailboxWatcher::start().unwrap();
        watcher.watch(7, dir.clone());
        std::fs::write(
            dir.join("outbox").join("settings.json"),
            r#"{"to":"bob","text":"ping","hooks":{"x":"y"}}"#,
        )
        .unwrap();
        let _ = next(&mut rx).accept();

        assert_eq!(entries(outside.path()), Vec::<String>::new());
    }

    #[test]
    fn a_rejected_message_is_not_moved_through_a_linked_rejected_folder() {
        let state = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let dir = fresh(state.path());
        std::fs::remove_dir(dir.join("rejected")).unwrap();
        symlink(outside.path(), dir.join("rejected")).unwrap();

        let (mut watcher, _rx) = MailboxWatcher::start().unwrap();
        watcher.watch(7, dir.clone());
        std::fs::write(dir.join("outbox").join("package.json"), "any bytes").unwrap();
        scans();

        assert_eq!(entries(outside.path()), Vec::<String>::new());
    }

    #[test]
    fn the_reason_of_a_rejection_is_not_written_through_a_link() {
        let state = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let target = outside.path().join("victim");
        std::fs::write(&target, "untouched").unwrap();
        let dir = fresh(state.path());
        symlink(&target, dir.join("rejected").join("v.err")).unwrap();

        let (mut watcher, _rx) = MailboxWatcher::start().unwrap();
        watcher.watch(7, dir.clone());
        std::fs::write(dir.join("outbox").join("v.json"), "not-json").unwrap();
        scans();

        assert_eq!(std::fs::read_to_string(&target).unwrap(), "untouched");
    }

    #[test]
    fn the_peer_list_is_not_written_through_a_linked_staging_file() {
        let state = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let target = outside.path().join("victim");
        std::fs::write(&target, "untouched").unwrap();
        let dir = fresh(state.path());
        symlink(&target, dir.join("peers.json.tmp")).unwrap();

        let _ = write_peers(
            &dir,
            &[PeerEntry {
                name: "carol".into(),
                pane: 9,
            }],
        );

        assert_eq!(std::fs::read_to_string(&target).unwrap(), "untouched");
    }
}

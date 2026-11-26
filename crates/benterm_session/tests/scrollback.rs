//! The files that hold what the panes showed: one beside the session
//! file for each key, which only the user can read.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use benterm_session::{
    MAX_SCROLLBACK_BYTES, Node, Session, SessionStore, Tab, Terminal, Workspace, open,
};

const KEY: &str = "0123456789abcdef0123456789abcdef";
const OTHER_KEY: &str = "ffffffffffffffffffffffffffffffff";

/// A session with one pane, whose scrollback is kept under `key`.
fn session(key: &str) -> Session {
    Session {
        selected_workspace: 0,
        workspaces_made: 1,
        folded_repositories: Vec::new(),
        workspaces: vec![Workspace {
            task: None,
            name: "Workspace 1".into(),
            color: Some(0),
            selected_tab: 0,
            tabs: vec![Tab {
                name: None,
                focused_pane: 0,
                root: Node::Terminal(Terminal {
                    cwd: None,
                    scrollback: Some(key.into()),
                    conversation: None,
                    ..Terminal::default()
                }),
            }],
        }],
    }
}

/// A store in a directory of its own, and the directory of the
/// scrollback files.
fn store() -> (tempfile::TempDir, SessionStore, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("benterm").join("session.json");
    let store = open(&path).store.unwrap();
    let scrollback = path.with_file_name("scrollback");
    (dir, store, scrollback)
}

fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

fn mode(path: &Path) -> u32 {
    fs::symlink_metadata(path).unwrap().permissions().mode() & 0o777
}

#[test]
fn what_is_written_under_a_key_is_read_under_it() {
    let (_dir, store, _) = store();
    assert_eq!(store.scrollback(KEY), None);
    store
        .write_scrollback(store.ticket(), KEY, b"old\r\n")
        .unwrap();
    store
        .write_scrollback(store.ticket(), OTHER_KEY, b"other\r\n")
        .unwrap();
    assert_eq!(store.scrollback(KEY).as_deref(), Some(&b"old\r\n"[..]));
    assert_eq!(
        store.scrollback(OTHER_KEY).as_deref(),
        Some(&b"other\r\n"[..])
    );
}

#[test]
fn only_the_user_can_read_the_files_and_list_their_directory() {
    let (_dir, store, scrollback) = store();
    store
        .write_scrollback(store.ticket(), KEY, b"secret\r\n")
        .unwrap();
    assert_eq!(mode(&scrollback), 0o700);
    assert_eq!(mode(&scrollback.join(KEY)), 0o600);
    // Replacing a file does not widen it either.
    store
        .write_scrollback(store.ticket(), KEY, b"more\r\n")
        .unwrap();
    assert_eq!(mode(&scrollback.join(KEY)), 0o600);
    assert_eq!(names(&scrollback), [KEY]);
}

#[test]
fn a_file_someone_made_readable_by_others_is_replaced_by_one_that_is_not() {
    let (_dir, store, scrollback) = store();
    store
        .write_scrollback(store.ticket(), KEY, b"one\r\n")
        .unwrap();
    fs::set_permissions(scrollback.join(KEY), fs::Permissions::from_mode(0o644)).unwrap();
    store
        .write_scrollback(store.ticket(), KEY, b"two\r\n")
        .unwrap();
    assert_eq!(mode(&scrollback.join(KEY)), 0o600);
}

#[test]
fn a_key_that_is_not_one_names_no_file() {
    let (_dir, store, scrollback) = store();
    store
        .write_scrollback(store.ticket(), KEY, b"old\r\n")
        .unwrap();
    for key in [
        "",
        "../session.json",
        "0123",
        "0123456789ABCDEF0123456789ABCDEF",
    ] {
        assert!(
            store
                .write_scrollback(store.ticket(), key, b"x\r\n")
                .is_err()
        );
        assert_eq!(store.scrollback(key), None, "{key:?}");
    }
    assert_eq!(names(&scrollback), [KEY]);
    assert_eq!(
        names(scrollback.parent().unwrap()),
        ["scrollback", "session.json.lock"]
    );
}

#[test]
fn a_snapshot_taken_earlier_does_not_replace_a_later_one_of_the_same_pane() {
    let (_dir, store, _) = store();
    let earlier = store.ticket();
    let between = store.ticket();
    let later = store.ticket();
    store.write_scrollback(later, KEY, b"later\r\n").unwrap();
    // As when a write in the background is overtaken by the one at quit.
    store
        .write_scrollback(earlier, KEY, b"earlier\r\n")
        .unwrap();
    assert_eq!(store.scrollback(KEY).as_deref(), Some(&b"later\r\n"[..]));
    // Another pane is not held up by it.
    store
        .write_scrollback(between, OTHER_KEY, b"other\r\n")
        .unwrap();
    assert_eq!(
        store.scrollback(OTHER_KEY).as_deref(),
        Some(&b"other\r\n"[..])
    );
}

#[test]
fn a_removed_scrollback_is_not_brought_back_by_an_earlier_snapshot() {
    let (_dir, store, scrollback) = store();
    let earlier = store.ticket();
    store
        .write_scrollback(store.ticket(), KEY, b"old\r\n")
        .unwrap();
    store.remove_scrollback(store.ticket(), KEY).unwrap();
    store
        .write_scrollback(earlier, KEY, b"earlier\r\n")
        .unwrap();
    assert_eq!(store.scrollback(KEY), None);
    assert_eq!(names(&scrollback), [""; 0]);
    // Removing what is not there is fine.
    store.remove_scrollback(store.ticket(), OTHER_KEY).unwrap();
}

#[test]
fn clearing_the_session_removes_what_its_panes_showed() {
    let (_dir, store, scrollback) = store();
    let earlier = store.ticket();
    store.write(store.ticket(), &session(KEY)).unwrap();
    store
        .write_scrollback(store.ticket(), KEY, b"old\r\n")
        .unwrap();
    store
        .write_scrollback(store.ticket(), OTHER_KEY, b"other\r\n")
        .unwrap();
    store.clear(store.ticket()).unwrap();
    assert_eq!(names(&scrollback), [""; 0]);
    // A write that was on its way does not leave a file behind.
    store
        .write_scrollback(earlier, KEY, b"earlier\r\n")
        .unwrap();
    assert_eq!(names(&scrollback), [""; 0]);
}

/// Found by Codex: a clearing that a later save has overtaken is passed
/// over, and so must be the removal of what the panes showed.
#[test]
fn a_clearing_taken_earlier_removes_nothing_of_a_later_session() {
    let (_dir, store, scrollback) = store();
    let earlier = store.ticket();
    store
        .write_scrollback(store.ticket(), KEY, b"new\r\n")
        .unwrap();
    store.write(store.ticket(), &session(KEY)).unwrap();
    store.clear(earlier).unwrap();
    assert_eq!(names(&scrollback), [KEY]);
    assert!(scrollback.with_file_name("session.json").exists());
}

#[test]
fn files_of_panes_that_are_in_no_session_are_removed_on_request() {
    let (_dir, store, scrollback) = store();
    store
        .write_scrollback(store.ticket(), KEY, b"kept\r\n")
        .unwrap();
    store
        .write_scrollback(store.ticket(), OTHER_KEY, b"gone\r\n")
        .unwrap();
    // What an interrupted write left, and a file that is not ours.
    fs::write(scrollback.join(format!("{OTHER_KEY}.tmp")), "half").unwrap();
    fs::write(scrollback.join(format!("{KEY}.tmp")), "half").unwrap();
    fs::write(scrollback.join("notes.txt"), "mine").unwrap();
    store.keep_scrollback_of(Some(&session(KEY))).unwrap();
    assert_eq!(names(&scrollback), [KEY, "notes.txt"]);

    store.keep_scrollback_of(None).unwrap();
    assert_eq!(names(&scrollback), ["notes.txt"]);
}

#[test]
fn keeping_the_files_of_a_session_needs_no_directory() {
    let (_dir, store, scrollback) = store();
    store.keep_scrollback_of(None).unwrap();
    assert!(!scrollback.exists());
}

#[test]
fn more_than_the_limit_is_neither_written_nor_read() {
    let (_dir, store, scrollback) = store();
    store
        .write_scrollback(store.ticket(), KEY, b"old\r\n")
        .unwrap();
    let large = vec![b'a'; MAX_SCROLLBACK_BYTES + 1];
    assert!(store.write_scrollback(store.ticket(), KEY, &large).is_err());
    assert_eq!(store.scrollback(KEY).as_deref(), Some(&b"old\r\n"[..]));

    fs::write(scrollback.join(OTHER_KEY), &large).unwrap();
    assert_eq!(store.scrollback(OTHER_KEY), None);
    let most = vec![b'a'; MAX_SCROLLBACK_BYTES];
    store.write_scrollback(store.ticket(), KEY, &most).unwrap();
    assert_eq!(
        store.scrollback(KEY).map(|bytes| bytes.len()),
        Some(MAX_SCROLLBACK_BYTES)
    );
}

#[test]
fn a_link_in_the_place_of_a_file_is_neither_read_nor_written_through() {
    let (dir, store, scrollback) = store();
    store
        .write_scrollback(store.ticket(), OTHER_KEY, b"x\r\n")
        .unwrap();
    let target = dir.path().join("target");
    fs::write(&target, "elsewhere").unwrap();
    std::os::unix::fs::symlink(&target, scrollback.join(KEY)).unwrap();
    assert_eq!(store.scrollback(KEY), None);

    store
        .write_scrollback(store.ticket(), KEY, b"new\r\n")
        .unwrap();
    assert_eq!(fs::read(&target).unwrap(), b"elsewhere");
    assert_eq!(store.scrollback(KEY).as_deref(), Some(&b"new\r\n"[..]));
}

/// Found by Codex.
#[test]
fn nothing_is_removed_through_a_link_in_the_place_of_the_directory() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("benterm").join("session.json");
    let store = open(&path).store.unwrap();
    let elsewhere = dir.path().join("elsewhere");
    fs::create_dir(&elsewhere).unwrap();
    fs::write(elsewhere.join(KEY), "not ours").unwrap();
    std::os::unix::fs::symlink(&elsewhere, path.with_file_name("scrollback")).unwrap();
    store.remove_scrollback(store.ticket(), KEY).unwrap();
    store.keep_scrollback_of(None).unwrap();
    store.clear(store.ticket()).unwrap();
    assert_eq!(names(&elsewhere), [KEY]);
}

#[test]
fn a_link_in_the_place_of_the_directory_is_not_used() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("benterm").join("session.json");
    let store = open(&path).store.unwrap();
    let elsewhere = dir.path().join("elsewhere");
    fs::create_dir(&elsewhere).unwrap();
    std::os::unix::fs::symlink(&elsewhere, path.with_file_name("scrollback")).unwrap();
    assert!(
        store
            .write_scrollback(store.ticket(), KEY, b"x\r\n")
            .is_err()
    );
    assert_eq!(names(&elsewhere), [""; 0]);
    assert_eq!(store.scrollback(KEY), None);
}

//! Opening the session file, replacing it, and what happens to a file
//! that cannot be used. Every test works in a directory of its own.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use benterm_session::{Node, Session, Tab, Terminal, Workspace, open, session_path_from};

fn session(cwd: &str) -> Session {
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
                    cwd: Some(cwd.into()),
                    ..Terminal::default()
                }),
            }],
        }],
    }
}

/// A directory that does not exist yet, inside a temporary one, and the
/// session file in it.
fn place() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("benterm").join("session.json");
    (dir, path)
}

fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

#[test]
fn the_file_is_with_the_other_files_of_the_app() {
    use std::ffi::OsStr;
    assert_eq!(
        session_path_from(Some(OsStr::new("/Users/me"))),
        Some(PathBuf::from(
            "/Users/me/Library/Application Support/benterm/session.json"
        ))
    );
    assert_eq!(session_path_from(Some(OsStr::new("relative"))), None);
    assert_eq!(session_path_from(Some(OsStr::new(""))), None);
    assert_eq!(session_path_from(None), None);
}

#[test]
fn without_a_file_there_is_nothing_to_restore_and_nothing_to_say() {
    let (_dir, path) = place();
    let opened = open(&path);
    assert_eq!(opened.session, None);
    assert_eq!(opened.notice, None);
    assert!(opened.store.is_some());
    // Opening does not write.
    assert!(!path.exists());
}

#[test]
fn a_written_session_is_there_at_the_next_start() {
    let (_dir, path) = place();
    let opened = open(&path);
    let store = opened.store.unwrap();
    store.write(store.ticket(), &session("/tmp")).unwrap();
    drop(store);

    let opened = open(&path);
    assert_eq!(opened.session, Some(session("/tmp")));
    assert_eq!(opened.notice, None);
    assert!(opened.store.is_some());
}

#[test]
fn only_the_user_can_read_the_file() {
    let (_dir, path) = place();
    let store = open(&path).store.unwrap();
    store.write(store.ticket(), &session("/tmp")).unwrap();
    let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&path), 0o600);
    assert_eq!(mode(path.parent().unwrap()), 0o700);
}

#[test]
fn a_write_leaves_no_other_file_behind() {
    let (_dir, path) = place();
    let store = open(&path).store.unwrap();
    store.write(store.ticket(), &session("/a")).unwrap();
    store.write(store.ticket(), &session("/b")).unwrap();
    assert_eq!(
        names(path.parent().unwrap()),
        ["session.json", "session.json.lock"]
    );
}

#[test]
fn a_snapshot_taken_earlier_does_not_replace_a_later_one() {
    let (_dir, path) = place();
    let store = open(&path).store.unwrap();
    let earlier = store.ticket();
    let later = store.ticket();
    store.write(later, &session("/later")).unwrap();
    // As when a background write is overtaken by the write at quit.
    store.write(earlier, &session("/earlier")).unwrap();
    drop(store);
    assert_eq!(open(&path).session, Some(session("/later")));
}

#[test]
fn a_cleared_session_is_not_there_at_the_next_start() {
    let (_dir, path) = place();
    let store = open(&path).store.unwrap();
    // Clearing what was never written is fine.
    store.clear(store.ticket()).unwrap();
    let before = store.ticket();
    store.write(store.ticket(), &session("/tmp")).unwrap();
    store.clear(store.ticket()).unwrap();
    // A snapshot from before the clearing does not bring the session back.
    store.write(before, &session("/before")).unwrap();
    drop(store);
    let opened = open(&path);
    assert_eq!(opened.session, None);
    assert_eq!(opened.notice, None);
}

#[test]
fn a_session_the_reader_would_refuse_leaves_the_file_as_it_was() {
    let (_dir, path) = place();
    let store = open(&path).store.unwrap();
    store.write(store.ticket(), &session("/tmp")).unwrap();
    let mut empty = session("/tmp");
    empty.workspaces.clear();
    assert!(store.write(store.ticket(), &empty).is_err());
    drop(store);
    assert_eq!(open(&path).session, Some(session("/tmp")));
}

#[test]
fn a_corrupt_file_is_not_restored_and_a_copy_of_it_is_kept() {
    let (_dir, path) = place();
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, "{\"version\":1,\"worksp").unwrap();

    let opened = open(&path);
    assert_eq!(opened.session, None);
    let notice = opened.notice.unwrap();
    assert!(
        notice.starts_with("The saved session was not restored, because its file is damaged: "),
        "{notice}"
    );
    assert!(
        notice.ends_with(" A copy of the file is kept as session.json.corrupt."),
        "{notice}"
    );
    let copy = path.with_file_name("session.json.corrupt");
    assert_eq!(fs::read(&copy).unwrap(), b"{\"version\":1,\"worksp");
    // The file itself stays until a session replaces it.
    assert_eq!(fs::read(&path).unwrap(), b"{\"version\":1,\"worksp");

    let store = opened.store.expect("the new session is saved");
    store.write(store.ticket(), &session("/tmp")).unwrap();
    drop(store);
    assert_eq!(open(&path).session, Some(session("/tmp")));
    assert_eq!(fs::read(&copy).unwrap(), b"{\"version\":1,\"worksp");
}

#[test]
fn the_notice_does_not_show_what_the_file_holds() {
    let (_dir, path) = place();
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(
        &path,
        r#"{"version":1,"workspaces":[{"name":"/secret/place","tabs":7}]}"#,
    )
    .unwrap();
    let notice = open(&path).notice.unwrap();
    assert!(!notice.contains("secret"), "{notice}");
}

#[test]
fn a_file_of_a_newer_benterm_is_left_alone_and_nothing_is_saved() {
    let (_dir, path) = place();
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let newer = r#"{"version":6,"workspaces":[],"more":true}"#;
    fs::write(&path, newer).unwrap();

    let opened = open(&path);
    assert_eq!(opened.session, None);
    assert_eq!(
        opened.notice.as_deref(),
        Some(
            "The saved session was not restored, because it has version 6 and this Benterm \
             reads up to version 5. It is left as it is, and this session is not saved."
        )
    );
    assert!(opened.store.is_none());
    assert_eq!(fs::read_to_string(&path).unwrap(), newer);
    assert_eq!(
        names(path.parent().unwrap()),
        ["session.json", "session.json.lock"]
    );
}

#[test]
fn a_file_that_is_too_large_to_tell_what_it_is_is_left_alone() {
    let (_dir, path) = place();
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    // It may be the file of a newer Benterm, which this one must not replace.
    fs::write(&path, vec![b' '; benterm_session::MAX_FILE_BYTES + 1]).unwrap();
    let opened = open(&path);
    assert_eq!(opened.session, None);
    assert!(opened.store.is_none());
    let notice = opened.notice.unwrap();
    assert!(notice.contains("because it is larger than"), "{notice}");
    assert!(
        notice.ends_with("It is left as it is, and this session is not saved."),
        "{notice}"
    );
    assert_eq!(
        names(path.parent().unwrap()),
        ["session.json", "session.json.lock"]
    );
}

#[test]
fn a_file_that_cannot_be_read_is_left_alone_and_nothing_is_saved() {
    let (_dir, path) = place();
    // A directory in the place of the file cannot be read as a file.
    fs::create_dir_all(&path).unwrap();
    let opened = open(&path);
    assert_eq!(opened.session, None);
    let notice = opened.notice.unwrap();
    assert!(
        notice.starts_with("The saved session could not be read"),
        "{notice}"
    );
    assert!(notice.ends_with("This session is not saved."), "{notice}");
    assert!(opened.store.is_none());
    assert!(path.is_dir());
}

#[test]
fn a_directory_that_cannot_be_made_means_nothing_is_saved() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("file");
    fs::write(&file, "").unwrap();
    let opened = open(&file.join("benterm").join("session.json"));
    assert_eq!(opened.session, None);
    assert!(opened.store.is_none());
    let notice = opened.notice.unwrap();
    assert!(notice.ends_with("This session is not saved."), "{notice}");
}

#[test]
fn a_second_benterm_neither_restores_nor_saves_the_session_of_the_first() {
    let (_dir, path) = place();
    let first = open(&path);
    let store = first.store.unwrap();
    store.write(store.ticket(), &session("/first")).unwrap();

    let second = open(&path);
    assert_eq!(second.session, None);
    assert!(second.store.is_none());
    assert_eq!(
        second.notice.as_deref(),
        Some(
            "Another Benterm is running and holds the saved session. \
             This session is not saved."
        )
    );

    // Once the first is gone, the next start has its session.
    drop(store);
    let third = open(&path);
    assert_eq!(third.session, Some(session("/first")));
    assert!(third.store.is_some());
}

#[test]
fn what_an_interrupted_write_left_behind_is_not_read() {
    let (_dir, path) = place();
    let store = open(&path).store.unwrap();
    store.write(store.ticket(), &session("/tmp")).unwrap();
    drop(store);
    // A write that ended before its rename leaves half a temporary file.
    fs::write(
        path.with_file_name("session.json.tmp"),
        "{\"version\":1,\"work",
    )
    .unwrap();

    let opened = open(&path);
    assert_eq!(opened.session, Some(session("/tmp")));
    assert_eq!(opened.notice, None);
    let store = opened.store.unwrap();
    store.write(store.ticket(), &session("/next")).unwrap();
    assert_eq!(
        names(path.parent().unwrap()),
        ["session.json", "session.json.lock"]
    );
}

#[test]
fn a_write_that_fails_leaves_the_file_as_it_was() {
    let (_dir, path) = place();
    let store = open(&path).store.unwrap();
    store.write(store.ticket(), &session("/tmp")).unwrap();
    // The temporary file cannot be made where a directory has its name.
    fs::create_dir(path.with_file_name("session.json.tmp")).unwrap();
    assert!(store.write(store.ticket(), &session("/next")).is_err());
    assert_eq!(
        Session::from_json(&fs::read(&path).unwrap()).unwrap(),
        session("/tmp")
    );
    // The next write is tried again, and succeeds once it can.
    fs::remove_dir(path.with_file_name("session.json.tmp")).unwrap();
    store.write(store.ticket(), &session("/next")).unwrap();
    assert_eq!(
        Session::from_json(&fs::read(&path).unwrap()).unwrap(),
        session("/next")
    );
}

#[test]
fn a_lock_held_only_for_a_moment_is_waited_for() {
    // As when the app is started again while the one before it is still
    // going away, or one of its last children has not yet let go of the
    // files it was born with.
    let (_dir, path) = place();
    let first = open(&path);
    let store = first.store.unwrap();
    store.write(store.ticket(), &session("/first")).unwrap();
    let holder = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(20));
        drop(store);
    });

    let second = open(&path);
    holder.join().unwrap();
    assert_eq!(second.notice, None);
    assert_eq!(second.session, Some(session("/first")));
    assert!(second.store.is_some());
}

#[test]
fn a_session_too_large_to_read_back_leaves_the_file_as_it_was() {
    let (_dir, path) = place();
    let store = open(&path).store.unwrap();
    store.write(store.ticket(), &session("/tmp")).unwrap();
    let mut large = session("/tmp");
    large.workspaces[0].name = "n".repeat(benterm_session::MAX_FILE_BYTES);
    assert!(store.write(store.ticket(), &large).is_err());
    drop(store);
    let opened = open(&path);
    assert_eq!(opened.session, Some(session("/tmp")));
    assert!(opened.store.is_some());
}

#[test]
fn a_file_with_splits_past_the_limit_is_left_alone() {
    let (_dir, path) = place();
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let leaf = r#"{"kind":"terminal"}"#;
    let root = (0..101).fold(leaf.to_owned(), |rest, _| {
        format!(
            r#"{{"kind":"split","axis":"vertical","ratio":0.5,"first":{leaf},"second":{rest}}}"#
        )
    });
    let text =
        format!(r#"{{"version":1,"workspaces":[{{"name":"a","tabs":[{{"root":{root}}}]}}]}}"#);
    fs::write(&path, &text).unwrap();
    let opened = open(&path);
    assert_eq!(opened.session, None);
    assert!(opened.store.is_none());
    assert_eq!(fs::read_to_string(&path).unwrap(), text);
    assert_eq!(
        names(path.parent().unwrap()),
        ["session.json", "session.json.lock"]
    );
}

#[test]
fn a_temporary_file_left_by_someone_else_is_not_written_into() {
    let (dir, path) = place();
    let store = open(&path).store.unwrap();
    let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
    let temporary = path.with_file_name("session.json.tmp");

    // A file others can read does not become the session file.
    fs::write(&temporary, "left behind").unwrap();
    fs::set_permissions(&temporary, fs::Permissions::from_mode(0o644)).unwrap();
    store.write(store.ticket(), &session("/a")).unwrap();
    assert_eq!(mode(&path), 0o600);

    // A link is not followed to the file it names.
    let elsewhere = dir.path().join("elsewhere");
    fs::write(&elsewhere, "untouched").unwrap();
    std::os::unix::fs::symlink(&elsewhere, &temporary).unwrap();
    store.write(store.ticket(), &session("/b")).unwrap();
    assert_eq!(fs::read_to_string(&elsewhere).unwrap(), "untouched");
    assert!(
        !fs::symlink_metadata(&path)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(
        Session::from_json(&fs::read(&path).unwrap()).unwrap(),
        session("/b")
    );
}

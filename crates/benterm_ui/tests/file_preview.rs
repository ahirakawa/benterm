//! What the file browser shows of a file: the start of a large one, and
//! nothing of one that is not text.

use std::fs;
use std::path::Path;

use benterm_ui::file_preview::{Content, PREVIEW_LIMIT, read_preview};

#[test]
fn a_small_text_file_is_shown_whole() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.txt");
    fs::write(&path, "hello\nworld\n").unwrap();
    let preview = read_preview(&path, PREVIEW_LIMIT);
    assert_eq!(preview.content, Content::Text("hello\nworld\n".into()));
    assert_eq!(preview.size, 12);
    assert!(!preview.truncated);
}

#[test]
fn an_empty_file_is_empty_text() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("empty");
    fs::write(&path, "").unwrap();
    let preview = read_preview(&path, PREVIEW_LIMIT);
    assert_eq!(preview.content, Content::Text(String::new()));
    assert!(!preview.truncated);
}

#[test]
fn a_large_file_shows_its_start_and_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("big.log");
    fs::write(&path, "line\n".repeat(1000)).unwrap();
    let preview = read_preview(&path, 100);
    assert_eq!(preview.content, Content::Text("line\n".repeat(20)));
    assert_eq!(preview.size, 5000);
    assert!(preview.truncated);
}

#[test]
fn a_file_of_exactly_the_limit_is_whole() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("exact");
    fs::write(&path, "x".repeat(100)).unwrap();
    let preview = read_preview(&path, 100);
    assert!(!preview.truncated);
}

#[test]
fn a_character_cut_by_the_limit_is_left_out() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ja.txt");
    // Three bytes each, so the limit of 7 cuts the third character.
    fs::write(&path, "日本語です").unwrap();
    let preview = read_preview(&path, 7);
    assert_eq!(preview.content, Content::Text("日本".into()));
    assert!(preview.truncated);
}

#[test]
fn a_file_with_a_nul_byte_is_not_text() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("a.bin");
    fs::write(&path, b"abc\0def").unwrap();
    let preview = read_preview(&path, PREVIEW_LIMIT);
    assert_eq!(preview.content, Content::NotText);
    assert_eq!(preview.size, 7);
}

#[test]
fn a_file_that_is_not_utf8_is_not_text() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("image.png");
    fs::write(&path, [0x89, b'P', b'N', b'G', 0xff, 0xfe, b'\n']).unwrap();
    let preview = read_preview(&path, PREVIEW_LIMIT);
    assert_eq!(preview.content, Content::NotText);
}

/// A device is not opened: opening one can wait or have effects.
#[test]
fn a_device_is_not_a_regular_file() {
    let preview = read_preview(Path::new("/dev/zero"), 1000);
    assert_eq!(preview.content, Content::NotRegularFile);
}

/// Makes a named pipe at `path`.
fn mkfifo(path: &Path) {
    let status = std::process::Command::new("mkfifo")
        .arg(path)
        .status()
        .unwrap();
    assert!(status.success());
}

/// Reads the preview of `path` on another thread, and fails if it takes
/// more than two seconds, so that a pipe that is opened fails the test
/// instead of hanging it.
fn read_in_time(path: &Path) -> benterm_ui::file_preview::Preview {
    let (sender, receiver) = std::sync::mpsc::channel();
    let read = path.to_owned();
    std::thread::spawn(move || {
        let _ = sender.send(read_preview(&read, PREVIEW_LIMIT));
    });
    receiver
        .recv_timeout(std::time::Duration::from_secs(2))
        .expect("reading returns in time")
}

/// Opening a named pipe for reading waits until something opens it for
/// writing, which would stop the app. It is not opened.
#[test]
fn a_named_pipe_is_not_opened_and_says_it_is_not_a_regular_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("pipe");
    mkfifo(&path);
    assert_eq!(read_in_time(&path).content, Content::NotRegularFile);
}

/// A symbolic link counts as what it points to.
#[test]
fn a_link_to_a_named_pipe_is_not_a_regular_file_and_a_link_to_a_file_is() {
    let dir = tempfile::tempdir().unwrap();
    mkfifo(&dir.path().join("pipe"));
    fs::write(dir.path().join("file"), "text").unwrap();
    std::os::unix::fs::symlink(dir.path().join("pipe"), dir.path().join("to-pipe")).unwrap();
    std::os::unix::fs::symlink(dir.path().join("file"), dir.path().join("to-file")).unwrap();
    let to_pipe = read_in_time(&dir.path().join("to-pipe"));
    assert_eq!(to_pipe.content, Content::NotRegularFile);
    let to_file = read_preview(&dir.path().join("to-file"), PREVIEW_LIMIT);
    assert_eq!(to_file.content, Content::Text("text".into()));
}

#[test]
fn a_missing_file_says_why() {
    let dir = tempfile::tempdir().unwrap();
    let preview = read_preview(&dir.path().join("gone"), PREVIEW_LIMIT);
    assert!(matches!(preview.content, Content::Unreadable(_)));
}

#[test]
fn a_folder_cannot_be_shown_as_a_file() {
    let dir = tempfile::tempdir().unwrap();
    let preview = read_preview(dir.path(), PREVIEW_LIMIT);
    assert_eq!(preview.content, Content::NotRegularFile);
}

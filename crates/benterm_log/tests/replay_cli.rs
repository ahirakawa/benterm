//! The `benterm-replay` binary.

use std::path::PathBuf;
use std::process::{Command, Output};

use benterm_core::GridSize;
use benterm_log::{LogWriter, Record};

fn temp_file(name: &str, contents: &[u8]) -> PathBuf {
    let path = std::env::temp_dir().join(format!("benterm-replay-{}-{name}", std::process::id()));
    std::fs::write(&path, contents).unwrap();
    path
}

fn run(args: &[&std::ffi::OsStr]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_benterm-replay"))
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn prints_the_final_screen_of_a_log() {
    let mut writer = LogWriter::new(Vec::new(), GridSize::new(3, 20)).unwrap();
    writer
        .write_record(&Record::Output(b"$ echo hi\r\nhi\r\n".to_vec()))
        .unwrap();
    writer
        .write_record(&Record::Resize(GridSize::new(2, 20)))
        .unwrap();
    writer
        .write_record(&Record::Output(b"$ ".to_vec()))
        .unwrap();
    let path = temp_file("ok", &writer.into_inner());

    let output = run(&[path.as_os_str()]);
    std::fs::remove_file(&path).unwrap();

    assert!(output.status.success(), "{output:?}");
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "hi\n$\n");
}

#[test]
fn reports_a_file_that_is_not_a_log() {
    let path = temp_file("bad", b"hello");
    let output = run(&[path.as_os_str()]);
    std::fs::remove_file(&path).unwrap();

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(!output.stderr.is_empty());
}

#[test]
fn requires_exactly_one_path() {
    let output = run(&[]);
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("usage"));
}

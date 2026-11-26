//! The files every terminal runs or sources, the helper and zsh's
//! integration, are kept where only the user's own unconfined programs
//! write: not in the temporary directory, which an agent in a sandbox
//! may write in. A file that was changed all the same is put back
//! before it is used.
//!
//! This file holds a single test because it changes the environment of
//! the test process, which other tests running at the same time would see.

mod common;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use benterm_pty::{PtyConfig, PtySession};
use common::collect_to_exit;

fn files(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for entry in fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            found.extend(files(&path));
        } else {
            found.push(path);
        }
    }
    found
}

fn named(dir: &Path, start: &str) -> Vec<PathBuf> {
    files(dir)
        .into_iter()
        .filter(|path| {
            path.file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with(start))
        })
        .collect()
}

/// zsh reading its startup files, with a home that has none.
fn zsh(home: &Path) -> PtyConfig {
    let mut config = PtyConfig::command("/bin/zsh", ["-c", "echo zsh-ran"]);
    config.env.push(("HOME".into(), home.into()));
    config
}

fn output_of(config: PtyConfig) -> String {
    let (_session, events) = PtySession::spawn(config).unwrap();
    collect_to_exit(&events).text()
}

#[test]
fn what_the_terminals_run_is_kept_out_of_the_temporary_directory_and_put_back_when_changed() {
    let state = tempfile::tempdir().unwrap();
    let temporary = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    // SAFETY: this is the only test in this binary, and no other thread
    // reads or writes the environment while it runs.
    unsafe {
        std::env::set_var("XDG_STATE_HOME", state.path());
        std::env::set_var("TMPDIR", temporary.path());
    }

    assert!(output_of(PtyConfig::command("/bin/echo", ["first"])).contains("first"));
    assert!(output_of(zsh(home.path())).contains("zsh-ran"));

    // Nothing of Benterm's is where a sandbox lets its programs write.
    assert_eq!(files(temporary.path()), Vec::<PathBuf>::new());

    // The helper is in a directory of the user's alone.
    let helpers = named(state.path(), "benterm-pty-helper-");
    assert_eq!(helpers.len(), 1, "{helpers:?}");
    let helper = &helpers[0];
    let directory = helper.parent().unwrap();
    assert_eq!(
        fs::metadata(directory).unwrap().permissions().mode() & 0o777,
        0o700
    );

    // A helper someone put in its place is not run.
    fs::remove_file(helper).unwrap();
    fs::write(helper, "#!/bin/sh\necho REPLACED-HELPER-RAN\n").unwrap();
    fs::set_permissions(helper, fs::Permissions::from_mode(0o755)).unwrap();
    let text = output_of(PtyConfig::command("/bin/echo", ["second"]));
    assert!(text.contains("second"), "{text:?}");
    assert!(!text.contains("REPLACED-HELPER-RAN"), "{text:?}");

    // Nor is a line added to the file zsh reads first.
    let zshenvs = named(state.path(), ".zshenv");
    assert_eq!(zshenvs.len(), 1, "{zshenvs:?}");
    let mut changed = fs::read_to_string(&zshenvs[0]).unwrap();
    changed.push_str("\necho ADDED-LINE-RAN\n");
    fs::write(&zshenvs[0], changed).unwrap();
    let text = output_of(zsh(home.path()));
    assert!(text.contains("zsh-ran"), "{text:?}");
    assert!(!text.contains("ADDED-LINE-RAN"), "{text:?}");
}

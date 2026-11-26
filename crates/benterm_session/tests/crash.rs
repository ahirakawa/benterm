//! The session file is whole even when the app is killed while writing it.
//!
//! The test starts this test program again as a child that writes
//! sessions as fast as it can, and kills the child at an arbitrary
//! moment, many times over. Whatever the moment, the file read afterwards
//! is one of the sessions the child wrote, in full.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use benterm_session::{Axis, Node, Session, Split, Tab, Terminal, Workspace, open};

/// Set for the child to the path it writes to.
const WRITER_PATH: &str = "BENTERM_SESSION_CRASH_TEST_PATH";

/// A session with `panes` panes, so that files differ in length and a
/// shorter one replaces a longer one.
fn session(panes: usize) -> Session {
    let leaf = |n: usize| {
        Node::Terminal(Terminal {
            cwd: Some(format!("/some/long/directory/name/number/{n}")),
            ..Terminal::default()
        })
    };
    let root = (1..panes).fold(leaf(0), |rest, n| {
        Node::Split(Split {
            axis: Axis::Horizontal,
            ratio: 0.5,
            first: Box::new(leaf(n)),
            second: Box::new(rest),
        })
    });
    Session {
        selected_workspace: 0,
        workspaces_made: panes as u64,
        folded_repositories: Vec::new(),
        workspaces: vec![Workspace {
            task: None,
            name: "Workspace 1".into(),
            color: Some(0),
            selected_tab: 0,
            tabs: vec![Tab {
                name: None,
                focused_pane: 0,
                root,
            }],
        }],
    }
}

const SIZES: [usize; 4] = [1, 60, 7, 90];

/// The child: writes until it is killed. As a test of its own it does
/// nothing, since the path is not set.
#[test]
fn writer() {
    let Some(path) = std::env::var_os(WRITER_PATH) else {
        return;
    };
    let store = open(Path::new(&path))
        .store
        .expect("the child can write the session");
    for (round, panes) in SIZES.iter().cycle().enumerate() {
        store.write(store.ticket(), &session(*panes)).unwrap();
        if round == 0 {
            println!("writing");
        }
    }
}

#[test]
fn a_writer_killed_at_any_moment_leaves_a_whole_file() {
    use std::io::{BufRead, BufReader};

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session.json");
    let written: Vec<Session> = SIZES.iter().map(|panes| session(*panes)).collect();
    let mut seen_sizes = std::collections::HashSet::new();
    for round in 0..40u64 {
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "writer", "--nocapture", "--test-threads=1"])
            .env(WRITER_PATH, &path)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        // Wait until the child has written once and goes on writing.
        let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
        assert!(
            lines.any(|line| line.unwrap().contains("writing")),
            "the child did not start writing"
        );
        std::thread::sleep(Duration::from_micros(500 + round * 1731 % 20_000));
        // SIGKILL: the child runs no code of its own after this.
        child.kill().unwrap();
        child.wait().unwrap();

        let opened = open(&path);
        assert_eq!(opened.notice, None, "round {round}");
        let session = opened.session.expect("the child wrote at least once");
        let which = written.iter().position(|w| *w == session);
        assert!(
            which.is_some(),
            "round {round}: the file is no session the child wrote"
        );
        seen_sizes.insert(which);
    }
    // The kills did not all land in the same place.
    assert!(seen_sizes.len() > 1, "{seen_sizes:?}");
}

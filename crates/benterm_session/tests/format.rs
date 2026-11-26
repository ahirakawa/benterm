//! What the session file looks like, and which files are read.

use std::path::Path;

use benterm_session::{
    Agent, Axis, Browser, Conversation, MAX_FILE_BYTES, MAX_PANES, Node, Session, Split, Tab,
    Terminal, Unusable, Workspace, new_scrollback_key,
};
use proptest::collection::vec;
use proptest::prelude::*;

fn terminal(cwd: Option<&str>) -> Node {
    Node::Terminal(Terminal {
        cwd: cwd.map(str::to_owned),
        scrollback: None,
        conversation: None,
        ..Terminal::default()
    })
}

fn split(axis: Axis, ratio: f64, first: Node, second: Node) -> Node {
    Node::Split(Split {
        axis,
        ratio,
        first: Box::new(first),
        second: Box::new(second),
    })
}

fn one_tab(root: Node) -> Session {
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
                root,
            }],
        }],
    }
}

/// A session with two workspaces, the second with a split tab.
fn example() -> Session {
    Session {
        selected_workspace: 1,
        workspaces_made: 3,
        folded_repositories: Vec::new(),
        workspaces: vec![
            Workspace {
                task: None,
                name: "api".into(),
                color: Some(0),
                selected_tab: 0,
                tabs: vec![Tab {
                    name: None,
                    focused_pane: 0,
                    root: terminal(Some("/srv/api")),
                }],
            },
            Workspace {
                task: None,
                name: "Workspace 3".into(),
                color: Some(2),
                selected_tab: 1,
                tabs: vec![
                    Tab {
                        name: None,
                        focused_pane: 0,
                        root: terminal(None),
                    },
                    Tab {
                        name: None,
                        focused_pane: 2,
                        root: split(
                            Axis::Horizontal,
                            0.25,
                            terminal(Some("/tmp")),
                            split(Axis::Vertical, 0.5, terminal(Some("/")), terminal(None)),
                        ),
                    },
                ],
            },
        ],
    }
}

/// The file the first version of the format wrote for [`example`]. It is
/// kept as text, so that a later change to the types cannot change what
/// an old file means without this test noticing.
const FIRST_VERSION: &str = r#"{
  "version": 1,
  "selected_workspace": 1,
  "workspaces_made": 3,
  "workspaces": [
    {
      "name": "api",
      "color": 0,
      "selected_tab": 0,
      "tabs": [
        {
          "focused_pane": 0,
          "root": {
            "kind": "terminal",
            "cwd": "/srv/api"
          }
        }
      ]
    },
    {
      "name": "Workspace 3",
      "color": 2,
      "selected_tab": 1,
      "tabs": [
        {
          "focused_pane": 0,
          "root": {
            "kind": "terminal"
          }
        },
        {
          "focused_pane": 2,
          "root": {
            "kind": "split",
            "axis": "horizontal",
            "ratio": 0.25,
            "first": {
              "kind": "terminal",
              "cwd": "/tmp"
            },
            "second": {
              "kind": "split",
              "axis": "vertical",
              "ratio": 0.5,
              "first": {
                "kind": "terminal",
                "cwd": "/"
              },
              "second": {
                "kind": "terminal"
              }
            }
          }
        }
      ]
    }
  ]
}
"#;

fn corrupt(text: &str) -> String {
    match Session::from_json(text.as_bytes()) {
        Err(Unusable::Corrupt(why)) => why,
        other => panic!("expected a corrupt file, got {other:?}"),
    }
}

fn newer(text: &str) -> String {
    match Session::from_json(text.as_bytes()) {
        Err(Unusable::Newer(why)) => why,
        other => panic!("expected a newer file, got {other:?}"),
    }
}

const KEY: &str = "0123456789abcdef0123456789abcdef";
const OTHER_KEY: &str = "ffffffffffffffffffffffffffffffff";

/// A tab with one pane whose scrollback is kept under `key`.
fn with_scrollback(key: &str) -> Session {
    one_tab(Node::Terminal(Terminal {
        cwd: Some("/tmp".into()),
        scrollback: Some(key.into()),
        conversation: None,
        ..Terminal::default()
    }))
}

/// The file the second version of the format wrote for
/// [`with_scrollback`], kept as text like [`FIRST_VERSION`].
const SECOND_VERSION: &str = r#"{
  "version": 2,
  "selected_workspace": 0,
  "workspaces_made": 1,
  "workspaces": [
    {
      "name": "Workspace 1",
      "color": 0,
      "selected_tab": 0,
      "tabs": [
        {
          "focused_pane": 0,
          "root": {
            "kind": "terminal",
            "cwd": "/tmp",
            "scrollback": "0123456789abcdef0123456789abcdef"
          }
        }
      ]
    }
  ]
}
"#;

#[test]
fn a_session_is_written_as_the_first_version_wrote_it_but_for_the_version() {
    let current = FIRST_VERSION.replacen("\"version\": 1", "\"version\": 3", 1);
    assert_eq!(example().to_json().unwrap(), current);
}

#[test]
fn the_key_of_a_pane_s_scrollback_is_written_in_its_leaf() {
    let current = SECOND_VERSION.replacen("\"version\": 2", "\"version\": 3", 1);
    assert_eq!(with_scrollback(KEY).to_json().unwrap(), current);
    assert_eq!(
        Session::from_json(SECOND_VERSION.as_bytes()).unwrap(),
        with_scrollback(KEY)
    );
}

#[test]
fn a_key_that_could_name_another_file_is_not_kept() {
    for key in [
        "",
        "../session.json",
        "/etc/passwd",
        "0123456789ABCDEF0123456789ABCDEF",
        "0123456789abcdef0123456789abcde",
        "0123456789abcdef0123456789abcdef0",
        "0123456789abcdef0123456789abcde/",
    ] {
        let text = SECOND_VERSION.replace(KEY, key);
        let session = Session::from_json(text.as_bytes()).unwrap();
        assert_eq!(session, one_tab(terminal(Some("/tmp"))), "{key:?}");
    }
}

#[test]
fn a_key_is_kept_for_the_first_pane_that_has_it_only() {
    let leaf = |key: &str| {
        Node::Terminal(Terminal {
            cwd: None,
            scrollback: Some(key.into()),
            conversation: None,
            ..Terminal::default()
        })
    };
    let twice = one_tab(split(
        Axis::Horizontal,
        0.5,
        leaf(KEY),
        split(Axis::Vertical, 0.5, leaf(KEY), leaf(OTHER_KEY)),
    ));
    let read = Session::from_json(twice.to_json().unwrap().as_bytes()).unwrap();
    assert_eq!(read.scrollback_keys(), [KEY, OTHER_KEY]);
    let once = one_tab(split(
        Axis::Horizontal,
        0.5,
        leaf(KEY),
        split(Axis::Vertical, 0.5, terminal(None), leaf(OTHER_KEY)),
    ));
    assert_eq!(read, once);
}

#[test]
fn new_keys_are_names_of_files_and_differ() {
    let (one, other) = (new_scrollback_key().unwrap(), new_scrollback_key().unwrap());
    assert_ne!(one, other);
    for key in [one, other] {
        assert_eq!(key.len(), 32);
        assert!(key.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')));
        let text = SECOND_VERSION.replace(KEY, &key);
        let session = Session::from_json(text.as_bytes()).unwrap();
        assert_eq!(session.scrollback_keys(), [key.as_str()]);
    }
}

#[test]
fn a_file_of_the_first_version_is_read() {
    assert_eq!(
        Session::from_json(FIRST_VERSION.as_bytes()).unwrap(),
        example()
    );
}

#[test]
fn a_file_without_the_optional_fields_is_read() {
    let text = r#"{"version":1,"workspaces":[{"name":"a","tabs":[{"root":{"kind":"terminal"}}]}]}"#;
    let session = Session::from_json(text.as_bytes()).unwrap();
    assert_eq!(
        session,
        Session {
            selected_workspace: 0,
            workspaces_made: 0,
            folded_repositories: Vec::new(),
            workspaces: vec![Workspace {
                task: None,
                name: "a".into(),
                color: None,
                selected_tab: 0,
                tabs: vec![Tab {
                    name: None,
                    focused_pane: 0,
                    root: terminal(None),
                }],
            }],
        }
    );
}

#[test]
fn fields_a_later_build_added_are_passed_over() {
    let text = r#"{"version":1,"window":{"width":900},"workspaces":[{"name":"a","task":7,
        "tabs":[{"zoomed":true,"root":{"kind":"terminal","cwd":"/tmp",
        "agent":{"kind":"claude","conversation":"abc"},"scrollback":"p1.log"}}]}]}"#;
    let session = Session::from_json(text.as_bytes()).unwrap();
    assert_eq!(session.workspaces[0].tabs[0].root, terminal(Some("/tmp")));
}

#[test]
fn a_file_of_a_later_version_is_not_read() {
    let why = newer(r#"{"version":6,"workspaces":[]}"#);
    assert_eq!(
        why,
        "it has version 6 and this Benterm reads up to version 5"
    );
}

/// [`example`] with its second workspace's second tab named `review`.
fn with_tab_name() -> Session {
    let mut session = example();
    session.workspaces[1].tabs[1].name = Some("review".into());
    session
}

#[test]
fn the_name_of_a_tab_is_written_in_the_tab_and_read_back() {
    let text = with_tab_name().to_json().unwrap();
    let file: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(file["workspaces"][1]["tabs"][1]["name"], "review");
    assert!(file["workspaces"][1]["tabs"][0].get("name").is_none());
    assert_eq!(
        Session::from_json(text.as_bytes()).unwrap(),
        with_tab_name()
    );
}

#[test]
fn a_session_with_a_tab_name_is_still_one_an_older_build_reads() {
    // An older build passes the name over, and the tab shows the title
    // of its program as it did before it had a name.
    let text = with_tab_name().to_json().unwrap();
    let file: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(file["version"], 3);
}

#[test]
fn a_tab_name_that_is_only_spaces_is_read_as_none() {
    let text = r#"{"version":3,"workspaces":[{"name":"a","tabs":[
        {"name":"  ","root":{"kind":"terminal"}},
        {"name":"a\nb","root":{"kind":"terminal"}},
        {"name":" review ","root":{"kind":"terminal"}}]}]}"#;
    let session = Session::from_json(text.as_bytes()).unwrap();
    let names: Vec<_> = session.workspaces[0]
        .tabs
        .iter()
        .map(|tab| tab.name.as_deref())
        .collect();
    assert_eq!(names, [None, None, Some("review")]);
}

/// [`example`] with its first workspace showing the task 7.
fn with_task() -> Session {
    let mut session = example();
    session.workspaces[0].task = Some(7);
    session
}

#[test]
fn a_session_with_a_task_is_one_an_older_build_must_not_read() {
    // An older build would pass the task over and write the session
    // again without it, and the workspace would be no task's any more.
    let text = with_task().to_json().unwrap();
    let file: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(file["version"], 4);
    assert_eq!(file["workspaces"][0]["task"], 7);
    assert!(file["workspaces"][1].get("task").is_none());
    assert_eq!(Session::from_json(text.as_bytes()).unwrap(), with_task());
}

#[test]
fn a_session_without_a_task_is_still_one_an_older_build_reads() {
    let text = example().to_json().unwrap();
    let file: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(file["version"], 3);
}

#[test]
fn a_pane_of_a_kind_a_later_build_added_makes_the_file_a_newer_one() {
    let text = r#"{"version":1,"workspaces":[{"name":"a","tabs":[{"root":
        {"kind":"split","axis":"vertical","ratio":0.5,
         "first":{"kind":"terminal"},"second":{"kind":"canvas","drawing":"abc"}}}]}]}"#;
    assert_eq!(
        newer(text),
        "it has a pane of a kind this Benterm does not know"
    );
}

#[test]
fn the_reason_does_not_show_what_the_file_holds() {
    let text =
        r#"{"version":1,"workspaces":[{"name":"a","tabs":[{"root":{"kind":"/secret/place"}}]}]}"#;
    assert!(!newer(text).contains("secret"));
}

#[test]
fn a_file_that_is_not_a_session_is_corrupt() {
    for text in [
        "",
        "{",
        "[]",
        "null",
        r#"{"workspaces":[]}"#,
        r#"{"version":"1","workspaces":[]}"#,
        r#"{"version":-1,"workspaces":[]}"#,
        r#"{"version":1}"#,
        r#"{"version":1,"workspaces":{}}"#,
        r#"{"version":1,"workspaces":[{"name":"a","tabs":[{"root":{"cwd":"/"}}]}]}"#,
        r#"{"version":1,"workspaces":[{"name":"a","tabs":[{"root":{"kind":7}}]}]}"#,
        r#"{"version":1,"workspaces":[{"name":"a","tabs":[{"root":{"kind":"split","axis":"diagonal","ratio":0.5,"first":{"kind":"terminal"},"second":{"kind":"terminal"}}}]}]}"#,
        r#"{"version":1,"workspaces":[{"name":"a","tabs":[{"root":{"kind":"split","axis":"vertical","ratio":"wide","first":{"kind":"terminal"},"second":{"kind":"terminal"}}}]}]}"#,
        r#"{"version":1,"workspaces":[{"name":"a","tabs":[{"root":{"kind":"split","axis":"vertical","ratio":0.5,"first":{"kind":"terminal"}}}]}]}"#,
        r#"{"version":1,"workspaces":[{"name":"a","tabs":[{"focused_pane":-1,"root":{"kind":"terminal"}}]}]}"#,
    ] {
        corrupt(text);
    }
    assert!(Session::from_json(&[0xff, 0xfe, 0x00]).is_err());
}

#[test]
fn version_zero_was_never_written_so_it_is_corrupt() {
    let why = corrupt(r#"{"version":0,"workspaces":[]}"#);
    assert_eq!(why, "it has version 0, which no Benterm wrote");
}

#[test]
fn a_session_without_a_workspace_or_a_tab_is_corrupt() {
    let why = corrupt(r#"{"version":1,"workspaces":[]}"#);
    assert_eq!(why, "it has no workspace");
    let why = corrupt(r#"{"version":1,"workspaces":[{"name":"a","tabs":[]}]}"#);
    assert_eq!(why, "it has a workspace without a tab");
}

/// A tab with `panes` panes side by side, nested to the right.
fn row(panes: usize) -> Node {
    (1..panes).fold(terminal(None), |rest, _| {
        split(Axis::Horizontal, 0.5, terminal(None), rest)
    })
}

/// A session of tabs with 64 panes each, `panes` in all.
fn many(panes: usize) -> Session {
    let mut session = one_tab(terminal(None));
    session.workspaces[0].tabs = (0..panes / 64)
        .map(|_| Tab {
            name: None,
            focused_pane: 0,
            root: row(64),
        })
        .collect();
    session
}

#[test]
fn more_panes_than_the_limit_are_neither_written_nor_read() {
    let most = many(MAX_PANES);
    let text = most.to_json().unwrap();
    assert_eq!(Session::from_json(text.as_bytes()).unwrap(), most);

    let mut too_many = many(MAX_PANES);
    too_many.workspaces[0].tabs.push(Tab {
        name: None,
        focused_pane: 0,
        root: terminal(None),
    });
    assert!(too_many.to_json().is_err());
    let tab = r#"{"root":{"kind":"terminal"}}"#;
    let tabs = vec![tab; MAX_PANES + 1].join(",");
    let text = format!(r#"{{"version":1,"workspaces":[{{"name":"a","tabs":[{tabs}]}}]}}"#);
    // A later build may allow more, so the file is not one to replace.
    assert_eq!(newer(&text), format!("it has more than {MAX_PANES} panes"));
}

/// The text of a file with `splits` splits inside one another, which
/// the writer does not write.
fn nested(splits: usize) -> String {
    let leaf = r#"{"kind":"terminal"}"#;
    let root = (0..splits).fold(leaf.to_owned(), |rest, _| {
        format!(
            r#"{{"kind":"split","axis":"vertical","ratio":0.5,"first":{leaf},"second":{rest}}}"#
        )
    });
    format!(r#"{{"version":1,"workspaces":[{{"name":"a","tabs":[{{"root":{root}}}]}}]}}"#)
}

#[test]
fn splits_nested_deeper_than_the_limit_are_taken_for_a_newer_file() {
    assert!(Session::from_json(nested(100).as_bytes()).is_ok());
    // Not so deep that the reader gives up on the JSON itself.
    assert_eq!(newer(&nested(101)), "it has splits nested deeper than 100");
}

#[test]
fn a_session_too_large_for_the_reader_is_not_written() {
    let mut large = one_tab(terminal(None));
    large.workspaces[0].name = "n".repeat(MAX_FILE_BYTES);
    assert!(large.to_json().is_err());
    // The largest name that fits is written and read back.
    let rest = one_tab(terminal(None)).to_json().unwrap().len() - "Workspace 1".len();
    large.workspaces[0].name = "n".repeat(MAX_FILE_BYTES - rest);
    let text = large.to_json().unwrap();
    assert_eq!(text.len(), MAX_FILE_BYTES);
    assert_eq!(Session::from_json(text.as_bytes()).unwrap(), large);
    large.workspaces[0].name.push('n');
    assert!(large.to_json().is_err());
}

#[test]
fn the_limit_leaves_room_for_the_terminals_of_other_apps() {
    // macOS has 511 terminals for all apps together (kern.tty.ptmx_max).
    assert_eq!(MAX_PANES, 256);
}

#[test]
fn the_deepest_splits_that_are_written_are_read_back() {
    // 100 splits inside one another.
    let deepest = one_tab(row(101));
    let text = deepest.to_json().unwrap();
    assert_eq!(Session::from_json(text.as_bytes()).unwrap(), deepest);
    assert!(one_tab(row(102)).to_json().is_err());
}

#[test]
fn a_file_nested_deeper_than_the_reader_goes_is_taken_for_a_newer_one() {
    // The reader gives up before it can tell what the file is, so the
    // file is not one to replace.
    let text = format!("{}1{}", "[".repeat(200), "]".repeat(200));
    assert_eq!(newer(&text), "it is nested deeper than this Benterm reads");
    let text = format!(
        r#"{{"version":1,"workspaces":{}1{}}}"#,
        "[".repeat(200),
        "]".repeat(200)
    );
    assert_eq!(newer(&text), "it is nested deeper than this Benterm reads");
}

#[test]
fn a_session_the_reader_would_refuse_is_not_written() {
    let mut empty = example();
    empty.workspaces.clear();
    assert!(empty.to_json().is_err());
    let mut no_tab = example();
    no_tab.workspaces[0].tabs.clear();
    assert!(no_tab.to_json().is_err());
    // JSON has no way to write this number.
    let not_a_number = one_tab(split(
        Axis::Vertical,
        f64::NAN,
        terminal(None),
        terminal(None),
    ));
    assert!(not_a_number.to_json().is_err());
}

#[test]
fn what_is_out_of_range_is_brought_into_range_when_read() {
    let text = r#"{"version":1,"selected_workspace":9,"workspaces":[{"name":"a","selected_tab":5,
        "tabs":[{"focused_pane":4,"root":{"kind":"split","axis":"vertical","ratio":1.5,
        "first":{"kind":"terminal","cwd":"relative/path"},
        "second":{"kind":"split","axis":"vertical","ratio":-2,
          "first":{"kind":"terminal","cwd":""},"second":{"kind":"terminal","cwd":"/a\u0000b"}}}}]}]}"#;
    let session = Session::from_json(text.as_bytes()).unwrap();
    assert_eq!(session.selected_workspace, 0);
    assert_eq!(session.workspaces[0].selected_tab, 0);
    let tab = &session.workspaces[0].tabs[0];
    assert_eq!(tab.focused_pane, 2);
    // A directory that is not an absolute path is no directory.
    let inner = split(Axis::Vertical, 0.0, terminal(None), terminal(None));
    assert_eq!(tab.root, split(Axis::Vertical, 1.0, terminal(None), inner));
}

#[test]
fn a_terminal_keeps_a_directory_only_as_an_absolute_path_in_utf8() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    let kept = Terminal::in_directory(Some(Path::new("/tmp/日本語")));
    assert_eq!(kept.cwd.as_deref(), Some("/tmp/日本語"));
    assert_eq!(kept.directory(), Some(Path::new("/tmp/日本語")));
    assert_eq!(Terminal::in_directory(None).cwd, None);
    assert_eq!(Terminal::in_directory(Some(Path::new("tmp"))).cwd, None);
    // Written with a replacement character, the path would name another
    // directory.
    let not_utf8 = Path::new(OsStr::from_bytes(b"/tmp/\xff"));
    assert_eq!(Terminal::in_directory(Some(not_utf8)).cwd, None);
    assert_eq!(Terminal::in_directory(None).directory(), None);
}

fn node() -> impl Strategy<Value = Node> {
    let cwd = prop_oneof![
        Just(None),
        "(/[a-zA-Z0-9 ._\\-\"\\\\あ]{1,8}){1,4}".prop_map(Some),
    ];
    // Keys differ from pane to pane, as the ones the app makes do.
    let scrollback = prop_oneof![Just(false), Just(true)];
    let agent = prop_oneof![Just(Agent::Claude), Just(Agent::Codex)];
    let conversation = prop_oneof![
        Just(None),
        (agent, "[a-zA-Z0-9][a-zA-Z0-9_\\-]{0,40}")
            .prop_map(|(agent, id)| Conversation::new(agent, &id)),
    ];
    let leaf = (cwd, scrollback, conversation).prop_map(|(cwd, scrollback, conversation)| {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let number = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let scrollback = scrollback.then(|| format!("{number:032x}"));
        Node::Terminal(Terminal {
            cwd,
            scrollback,
            conversation,
            ..Terminal::default()
        })
    });
    let page = prop_oneof![
        Just(Browser::default()),
        "https?://[a-z0-9.]{1,12}(:[0-9]{2,4})?(/[a-zA-Z0-9._~%?#=&\\-]{0,12}){0,3}"
            .prop_map(|url| Browser::at(&url)),
    ];
    let leaf = prop_oneof![3 => leaf, 1 => page.prop_map(Node::Browser)];
    leaf.prop_recursive(6, 24, 2, |inner| {
        let axis = prop_oneof![Just(Axis::Horizontal), Just(Axis::Vertical)];
        (axis, 0.0f64..=1.0, inner.clone(), inner)
            .prop_map(|(axis, ratio, first, second)| split(axis, ratio, first, second))
    })
}

fn panes(node: &Node) -> usize {
    match node {
        Node::Terminal(_) | Node::Browser(_) => 1,
        Node::Split(split) => panes(&split.first) + panes(&split.second),
    }
}

fn tab() -> impl Strategy<Value = Tab> {
    let name = prop_oneof![Just(None), "[a-zA-Z0-9\"\\\\あ]{1,12}".prop_map(Some)];
    (node(), any::<prop::sample::Index>(), name).prop_map(|(root, focused, name)| Tab {
        name,
        focused_pane: focused.index(panes(&root)),
        root,
    })
}

fn workspace() -> impl Strategy<Value = Workspace> {
    let color = prop_oneof![Just(None), (0usize..6).prop_map(Some)];
    (
        "[a-zA-Z0-9 \"\\\\あ]{1,12}",
        color,
        vec(tab(), 1..5),
        any::<prop::sample::Index>(),
    )
        .prop_map(|(name, color, tabs, selected)| Workspace {
            task: None,
            name,
            color,
            selected_tab: selected.index(tabs.len()),
            tabs,
        })
}

fn session() -> impl Strategy<Value = Session> {
    (
        vec(workspace(), 1..5),
        any::<prop::sample::Index>(),
        0u64..1000,
    )
        .prop_map(|(workspaces, selected, made)| Session {
            selected_workspace: selected.index(workspaces.len()),
            workspaces_made: made,
            folded_repositories: Vec::new(),
            workspaces,
        })
}

proptest! {
    /// A session that is written and read back is the same session: the
    /// same workspaces, tabs, splits, ratios and directories.
    #[test]
    fn a_written_session_is_read_back_the_same(session in session()) {
        let text = session.to_json().unwrap();
        prop_assert_eq!(Session::from_json(text.as_bytes()).unwrap(), session);
    }

    /// No bytes make the reader panic.
    #[test]
    fn any_bytes_are_read_or_refused(bytes in vec(any::<u8>(), 0..200)) {
        let _ = Session::from_json(&bytes);
    }

    /// A file cut short, as an interrupted write that was not atomic
    /// would leave it, is refused and never read as another session.
    #[test]
    fn a_file_cut_short_is_refused(session in session(), cut in any::<prop::sample::Index>()) {
        let text = session.to_json().unwrap();
        let end = text.trim_end().len();
        let mut at = cut.index(end);
        while !text.is_char_boundary(at) {
            at -= 1;
        }
        prop_assert!(Session::from_json(&text.as_bytes()[..at]).is_err());
    }
}

#[test]
fn the_folded_repositories_are_written_and_read_back() {
    let mut session = example();
    session.workspaces[0].task = Some(1);
    session.folded_repositories = vec!["/src/benterm".into()];

    let text = session.to_json().unwrap();

    assert!(text.contains("\"folded_repositories\""));
    assert_eq!(Session::from_json(text.as_bytes()).unwrap(), session);
}

#[test]
fn a_file_without_folded_repositories_has_none() {
    let session = Session::from_json(FIRST_VERSION.as_bytes()).unwrap();
    assert!(session.folded_repositories.is_empty());
}

fn browser(url: &str) -> Node {
    Node::Browser(Browser::at(url))
}

#[test]
fn a_browser_pane_comes_back_with_its_url_in_its_place() {
    let root = split(
        Axis::Horizontal,
        0.25,
        terminal(Some("/tmp")),
        split(
            Axis::Vertical,
            0.5,
            browser("http://127.0.0.1:8080/docs?page=2#top"),
            terminal(None),
        ),
    );
    let mut session = one_tab(root);
    session.workspaces[0].tabs[0].focused_pane = 1;
    let text = session.to_json().unwrap();
    assert_eq!(Session::from_json(text.as_bytes()), Ok(session));
}

#[test]
fn a_browser_pane_is_written_as_its_kind_and_its_url() {
    let text = one_tab(browser("https://example.com/")).to_json().unwrap();
    let file: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        file["workspaces"][0]["tabs"][0]["root"],
        serde_json::json!({"kind": "browser", "url": "https://example.com/"})
    );
}

/// A build from before browser panes does not know the kind, and leaves
/// such a file as it is whatever its version says.
#[test]
fn a_session_with_a_browser_pane_keeps_the_version_of_one_without() {
    let text = one_tab(browser("https://example.com/")).to_json().unwrap();
    let file: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(file["version"], 3);
}

#[test]
fn a_browser_pane_only_keeps_the_address_of_a_web_page() {
    for url in [
        "file:///etc/passwd",
        "javascript:alert(1)",
        "example.com",
        "http://",
        "https://exa mple.com/",
        "http://:80/",
        "http://[::1/",
        "http://example.com:abc/",
        "http://example.com:99999/",
        "http://user@/",
        "https://example.com/\n",
        "",
    ] {
        assert_eq!(Browser::at(url).url(), None, "{url:?}");
        let text = format!(
            r#"{{"version":3,"workspaces":[{{"name":"a","tabs":[{{"root":{{"kind":"browser","url":{}}}}}]}}]}}"#,
            serde_json::Value::from(url)
        );
        let session = Session::from_json(text.as_bytes()).unwrap();
        assert_eq!(
            session.workspaces[0].tabs[0].root,
            Node::Browser(Browser::default())
        );
    }
    for url in [
        "HTTPS://Example.com/a",
        "http://localhost",
        "http://127.0.0.1:8080/docs?page=2#top",
        "http://[::1]:3000/",
        "https://例え.jp/パス",
        "https://example.com/?mail=user@example.com",
    ] {
        assert_eq!(Browser::at(url).url(), Some(url), "{url:?}");
    }
}

/// An address may name a user and a password before the host. The
/// session file is kept on disk, so neither is kept with the address,
/// and neither is taken from a file an older build wrote.
#[test]
fn a_browser_pane_does_not_keep_the_user_and_password_of_an_address() {
    for (url, kept) in [
        (
            "https://user:secret@example.com:8443/",
            "https://example.com:8443/",
        ),
        (
            "https://user@example.com/a?q=1#top",
            "https://example.com/a?q=1#top",
        ),
        ("http://:secret@localhost:3000/x", "http://localhost:3000/x"),
    ] {
        assert_eq!(Browser::at(url).url(), Some(kept), "{url:?}");
        let written = one_tab(browser(url)).to_json().unwrap();
        assert!(!written.contains("secret"), "{written}");
        assert!(!written.contains("user"), "{written}");

        let text = format!(
            r#"{{"version":3,"workspaces":[{{"name":"a","tabs":[{{"root":{{"kind":"browser","url":{}}}}}]}}]}}"#,
            serde_json::Value::from(url)
        );
        let session = Session::from_json(text.as_bytes()).unwrap();
        assert_eq!(session.workspaces[0].tabs[0].root, browser(kept));
    }
}

#[test]
fn a_browser_pane_without_a_url_is_one_that_shows_no_page() {
    let text = r#"{"version":3,"workspaces":[{"name":"a","tabs":[{"root":{"kind":"browser"}}]}]}"#;
    let session = Session::from_json(text.as_bytes()).unwrap();
    assert_eq!(
        session.workspaces[0].tabs[0].root,
        Node::Browser(Browser::default())
    );
    assert_eq!(session.scrollback_keys(), Vec::<&str>::new());
}

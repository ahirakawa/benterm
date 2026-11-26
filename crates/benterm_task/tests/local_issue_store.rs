//! Changes to the issues a repository keeps in a file of its own: the
//! numbers they get, what is refused, and what of the file is kept.

use std::path::Path;

use benterm_task::{
    LocalIssueChange, LocalIssueState, LocalIssues, LocalIssuesError, NewLocalIssue,
    change_local_issues, local_issues_path, read_local_issues,
};

fn write(root: &Path, text: &str) {
    let path = local_issues_path(root);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn text(root: &Path) -> String {
    std::fs::read_to_string(local_issues_path(root)).unwrap()
}

fn titled(title: &str) -> NewLocalIssue {
    NewLocalIssue {
        title: title.to_owned(),
        ..NewLocalIssue::default()
    }
}

fn create(root: &Path, new: NewLocalIssue) -> u64 {
    change_local_issues(root, LocalIssueChange::Create(new))
        .unwrap()
        .number
}

fn change(root: &Path, change: LocalIssueChange) -> Result<LocalIssues, LocalIssuesError> {
    change_local_issues(root, change).map(|changed| changed.issues)
}

fn refused(root: &Path, what: LocalIssueChange, expected: &str) {
    let before = text(root);
    match change_local_issues(root, what.clone()) {
        Err(LocalIssuesError::Refused(why)) => {
            assert!(
                why.contains(expected),
                "{what:?}: {why:?} lacks {expected:?}"
            )
        }
        other => panic!("{what:?}: {other:?}"),
    }
    assert_eq!(text(root), before, "a refused change wrote the file");
}

/// An epic #1 with the children #2 and #3, where #2 is done before #3.
fn sample(root: &Path) {
    assert_eq!(create(root, titled("Epic")), 1);
    let child = |title: &str, blocked_by: &[u64]| NewLocalIssue {
        title: title.to_owned(),
        parent: Some(1),
        blocked_by: blocked_by.to_vec(),
        ..NewLocalIssue::default()
    };
    assert_eq!(create(root, child("First", &[])), 2);
    assert_eq!(create(root, child("Second", &[2, 2])), 3);
}

#[test]
fn the_first_issue_makes_the_file_and_each_issue_gets_the_next_number() {
    let dir = tempfile::tempdir().unwrap();
    let made = change_local_issues(
        dir.path(),
        LocalIssueChange::Create(NewLocalIssue {
            title: "  Epic  ".to_owned(),
            description: Some("  What it is for\n".to_owned()),
            ..NewLocalIssue::default()
        }),
    )
    .unwrap();
    assert_eq!(made.number, 1);
    assert_eq!(create(dir.path(), titled("Two")), 2);
    assert_eq!(create(dir.path(), titled("Three")), 3);

    // What was given back is what the file says.
    let read = read_local_issues(dir.path()).unwrap();
    assert_eq!(read.issues.len(), 3);
    let epic = read.issue(1).unwrap();
    assert_eq!(epic.title, "Epic");
    assert_eq!(epic.description.as_deref(), Some("What it is for"));
    assert_eq!(epic.state, LocalIssueState::Open);
    assert_eq!(made.issues.issue(1), Some(epic));
    // Nothing is left beside the file.
    let names: Vec<_> = std::fs::read_dir(dir.path().join(".benterm"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(names, ["issues.json"]);
}

#[test]
fn the_number_of_an_issue_that_was_taken_out_is_not_given_again() {
    let dir = tempfile::tempdir().unwrap();
    sample(dir.path());
    change(dir.path(), LocalIssueChange::Delete { number: 3 }).unwrap();
    assert_eq!(create(dir.path(), titled("After")), 4);

    // A file written by hand has no next number: the highest one counts.
    let by_hand = tempfile::tempdir().unwrap();
    write(
        by_hand.path(),
        r#"{"version": 1, "issues": [{"number": 7, "title": "a"}, {"number": 2, "title": "b"}]}"#,
    );
    assert_eq!(create(by_hand.path(), titled("c")), 8);
    change(by_hand.path(), LocalIssueChange::Delete { number: 8 }).unwrap();
    assert_eq!(create(by_hand.path(), titled("d")), 9);
}

#[test]
fn changes_made_at_once_lose_nothing_and_give_no_number_twice() {
    let dir = tempfile::tempdir().unwrap();
    sample(dir.path());
    let root = dir.path();
    const THREADS: usize = 8;
    const EACH: usize = 6;
    let numbers: Vec<u64> = std::thread::scope(|scope| {
        let makers: Vec<_> = (0..THREADS)
            .map(|thread| {
                scope.spawn(move || {
                    (0..EACH)
                        .map(|nth| {
                            // Another kind of change in between.
                            let state = if nth % 2 == 0 {
                                LocalIssueState::Closed
                            } else {
                                LocalIssueState::Open
                            };
                            change(root, LocalIssueChange::SetState { number: 2, state }).unwrap();
                            create(root, titled(&format!("{thread}-{nth}")))
                        })
                        .collect::<Vec<u64>>()
                })
            })
            .collect();
        makers
            .into_iter()
            .flat_map(|maker| maker.join().unwrap())
            .collect()
    });

    let mut sorted = numbers.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), THREADS * EACH, "a number was given twice");
    let read = read_local_issues(root).unwrap();
    assert_eq!(read.issues.len(), 3 + THREADS * EACH);
    for number in numbers {
        assert!(read.issue(number).is_some(), "#{number} was lost");
    }
}

#[test]
fn an_issue_is_edited_opened_and_closed() {
    let dir = tempfile::tempdir().unwrap();
    sample(dir.path());

    let issues = change(
        dir.path(),
        LocalIssueChange::Edit {
            number: 3,
            issue: NewLocalIssue {
                title: "Second, renamed".to_owned(),
                description: Some("Why".to_owned()),
                parent: None,
                blocked_by: vec![1, 2],
            },
        },
    )
    .unwrap();
    let edited = issues.issue(3).unwrap();
    assert_eq!(edited.title, "Second, renamed");
    assert_eq!(edited.description.as_deref(), Some("Why"));
    assert_eq!(edited.parent, None);
    assert_eq!(edited.blocked_by, [1, 2]);

    let closed = |state| LocalIssueChange::SetState { number: 3, state };
    let issues = change(dir.path(), closed(LocalIssueState::Closed)).unwrap();
    assert_eq!(issues.issue(3).unwrap().state, LocalIssueState::Closed);
    assert_eq!(read_local_issues(dir.path()).unwrap(), issues);
    let issues = change(dir.path(), closed(LocalIssueState::Open)).unwrap();
    assert_eq!(issues.issue(3).unwrap().state, LocalIssueState::Open);

    refused(
        dir.path(),
        LocalIssueChange::Edit {
            number: 3,
            issue: titled("   "),
        },
        "title",
    );
    assert_eq!(
        change(dir.path(), closed(LocalIssueState::Closed)).map(|_| ()),
        Ok(())
    );
    assert_eq!(
        change_local_issues(
            dir.path(),
            LocalIssueChange::SetState {
                number: 9,
                state: LocalIssueState::Closed
            }
        ),
        Err(LocalIssuesError::IssueNotFound(9))
    );
}

#[test]
fn parents_and_blockers_are_set_and_taken_away() {
    let dir = tempfile::tempdir().unwrap();
    sample(dir.path());
    assert_eq!(create(dir.path(), titled("Alone")), 4);

    let issues = change(
        dir.path(),
        LocalIssueChange::SetParent {
            number: 4,
            parent: Some(1),
        },
    )
    .unwrap();
    assert_eq!(issues.issue(4).unwrap().parent, Some(1));
    let add = LocalIssueChange::AddBlocker {
        number: 4,
        blocker: 3,
    };
    change(dir.path(), add.clone()).unwrap();
    // Said twice, it is one blocker.
    let issues = change(dir.path(), add).unwrap();
    assert_eq!(issues.issue(4).unwrap().blocked_by, [3]);

    let issues = change(
        dir.path(),
        LocalIssueChange::RemoveBlocker {
            number: 4,
            blocker: 3,
        },
    )
    .unwrap();
    assert_eq!(issues.issue(4).unwrap().blocked_by, [0u64; 0]);
    let issues = change(
        dir.path(),
        LocalIssueChange::SetParent {
            number: 4,
            parent: None,
        },
    )
    .unwrap();
    assert_eq!(issues.issue(4).unwrap().parent, None);

    // Taking an issue out takes it out of the others too.
    let issues = change(dir.path(), LocalIssueChange::Delete { number: 2 }).unwrap();
    assert_eq!(issues.issue(2), None);
    assert_eq!(issues.issue(3).unwrap().blocked_by, [0u64; 0]);
    let issues = change(dir.path(), LocalIssueChange::Delete { number: 1 }).unwrap();
    assert_eq!(issues.issue(3).unwrap().parent, None);
    assert_eq!(read_local_issues(dir.path()).unwrap(), issues);
}

#[test]
fn a_parent_or_a_blocker_that_makes_a_circle_is_refused_with_the_circle() {
    let dir = tempfile::tempdir().unwrap();
    sample(dir.path());
    assert_eq!(create(dir.path(), titled("Third")), 4);
    change(
        dir.path(),
        LocalIssueChange::AddBlocker {
            number: 4,
            blocker: 3,
        },
    )
    .unwrap();
    let root = dir.path();
    let blocker = |number, blocker| LocalIssueChange::AddBlocker { number, blocker };
    let parent = |number, parent| LocalIssueChange::SetParent {
        number,
        parent: Some(parent),
    };

    // Itself.
    refused(root, blocker(2, 2), "#2 cannot be its own blocker");
    refused(root, parent(2, 2), "#2 cannot be its own parent");
    // #2 is before #3, which is before #4.
    refused(root, blocker(3, 4), "#3 → #4 → #3");
    refused(root, blocker(2, 4), "#2 → #3 → #4 → #2");
    // #2 is a part of #1.
    refused(root, parent(1, 2), "#1 → #2 → #1");
    change(root, parent(4, 3)).unwrap();
    refused(root, parent(1, 4), "#1 → #4 → #3 → #1");
    // The same through the form of an issue.
    refused(
        root,
        LocalIssueChange::Edit {
            number: 2,
            issue: NewLocalIssue {
                title: "First".to_owned(),
                blocked_by: vec![4],
                ..NewLocalIssue::default()
            },
        },
        "wait for itself",
    );
    // What is not in the file.
    refused(root, blocker(2, 9), "no issue #9");
    refused(root, parent(2, 9), "no issue #9");
    refused(
        root,
        LocalIssueChange::Create(NewLocalIssue {
            title: "t".to_owned(),
            parent: Some(9),
            ..NewLocalIssue::default()
        }),
        "no issue #9",
    );

    // What makes no circle is still made.
    let issues = change(root, blocker(4, 2)).unwrap();
    assert_eq!(issues.issue(4).unwrap().blocked_by, [3, 2]);
}

#[test]
fn what_this_build_does_not_know_of_the_file_is_written_back() {
    let dir = tempfile::tempdir().unwrap();
    write(
        dir.path(),
        r#"{
  "version": 1,
  "project": { "name": "kept" },
  "issues": [
    { "number": 1, "title": "Epic", "labels": ["a", "b"], "estimate": 3 },
    { "number": 2, "title": "Child", "parent": 1, "assignee": "someone" }
  ]
}"#,
    );
    // Read, with what is not known left aside.
    assert_eq!(read_local_issues(dir.path()).unwrap().issues.len(), 2);

    assert_eq!(create(dir.path(), titled("New")), 3);
    change(
        dir.path(),
        LocalIssueChange::SetState {
            number: 2,
            state: LocalIssueState::Closed,
        },
    )
    .unwrap();
    change(
        dir.path(),
        LocalIssueChange::Edit {
            number: 1,
            issue: titled("Epic, renamed"),
        },
    )
    .unwrap();

    let value: serde_json::Value = serde_json::from_str(&text(dir.path())).unwrap();
    assert_eq!(value["version"], 1);
    assert_eq!(value["project"]["name"], "kept");
    assert_eq!(value["issues"][0]["title"], "Epic, renamed");
    assert_eq!(value["issues"][0]["labels"], serde_json::json!(["a", "b"]));
    assert_eq!(value["issues"][0]["estimate"], 3);
    assert_eq!(value["issues"][1]["assignee"], "someone");
    assert_eq!(value["issues"][1]["state"], "closed");
    assert_eq!(value["issues"][2]["title"], "New");
}

#[test]
fn a_file_that_is_not_in_the_format_is_left_as_it_is() {
    let dir = tempfile::tempdir().unwrap();
    for broken in [
        "{ not json",
        r#"{"version": 2, "issues": []}"#,
        r#"{"version": 1, "issues": [{"number": 1, "title": "t", "blockedBy": []}]}"#,
    ] {
        write(dir.path(), broken);
        assert!(matches!(
            change_local_issues(dir.path(), LocalIssueChange::Create(titled("t"))),
            Err(LocalIssuesError::Invalid(_))
        ));
        assert_eq!(text(dir.path()), broken);
    }
    // Without a file there is nothing to change, and none is made for it.
    let empty = tempfile::tempdir().unwrap();
    assert_eq!(
        change_local_issues(empty.path(), LocalIssueChange::Delete { number: 1 }),
        Err(LocalIssuesError::Missing)
    );
    assert!(!empty.path().join(".benterm").exists());
}

#[cfg(unix)]
#[test]
fn nothing_is_written_through_a_symbolic_link() {
    use std::os::unix::fs::symlink;

    let outside = tempfile::tempdir().unwrap();
    let elsewhere = outside.path().join("issues.json");
    let original = r#"{"version": 1, "issues": [{"number": 1, "title": "elsewhere"}]}"#;
    std::fs::write(&elsewhere, original).unwrap();
    let make = || LocalIssueChange::Create(titled("t"));

    // The file is a link.
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join(".benterm")).unwrap();
    symlink(&elsewhere, local_issues_path(dir.path())).unwrap();
    assert!(matches!(
        change_local_issues(dir.path(), make()),
        Err(LocalIssuesError::NotPlainFile(_))
    ));

    // The directory is a link.
    let dir = tempfile::tempdir().unwrap();
    symlink(outside.path(), dir.path().join(".benterm")).unwrap();
    assert!(matches!(
        change_local_issues(dir.path(), make()),
        Err(LocalIssuesError::NotPlainFile(_))
    ));

    // The name the new file is written under is a link.
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join(".benterm")).unwrap();
    let target = outside.path().join("target");
    std::fs::write(&target, "untouched").unwrap();
    symlink(&target, dir.path().join(".benterm/.issues.json.part")).unwrap();
    assert_eq!(create(dir.path(), titled("t")), 1);
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "untouched");

    assert_eq!(std::fs::read_to_string(&elsewhere).unwrap(), original);
    let names: Vec<_> = std::fs::read_dir(outside.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(names.len(), 2, "{names:?}");
}

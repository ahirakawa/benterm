//! The tasks file: it is still there after a restart, keeps what a newer
//! build wrote into it, and is left alone when this build cannot read it.

use std::path::PathBuf;

use benterm_task::{Closed, Recorded, TaskStore, Worktree, folder_name};

fn recorded(name: &str) -> Recorded {
    Recorded {
        name: name.to_owned(),
        repository: PathBuf::from("/r/benterm"),
        base_branch: "main".to_owned(),
        base_commit: "0123456789abcdef0123456789abcdef01234567".to_owned(),
        branch: name.to_owned(),
        worktree: PathBuf::from("/w/benterm").join(name),
        role: Some("reviewer".to_owned()),
    }
}

#[test]
fn tasks_are_numbered_and_still_there_after_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("deep/tasks.json");
    let store = TaskStore::open(Some(&path));
    assert_eq!(store.tasks().unwrap(), []);
    let a = store.add(recorded("a")).unwrap();
    let b = store.add(recorded("b")).unwrap();
    assert_eq!((a.id, b.id), (1, 2));
    assert!(a.created_at_ms > 0);
    drop(store);

    let store = TaskStore::open(Some(&path));
    assert_eq!(store.tasks().unwrap(), [a.clone(), b]);
    assert_eq!(store.task(1).unwrap(), Some(a));
    assert_eq!(store.task(3).unwrap(), None);
    // A number is not given twice, also after its task is taken out.
    store.take_out(2).unwrap();
    assert_eq!(store.add(recorded("c")).unwrap().id, 3);
}

#[test]
fn the_file_has_the_names_the_other_views_read() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tasks.json");
    let store = TaskStore::open(Some(&path));
    let task = store.add(recorded("kanban")).unwrap();
    store
        .set_closed(task.id, Closed::new(5, Worktree::Kept))
        .unwrap();
    let file: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(file["version"], 1);
    let task = &file["tasks"][0];
    assert_eq!(task["id"], 1);
    assert_eq!(task["name"], "kanban");
    assert_eq!(task["repository"], "/r/benterm");
    assert_eq!(task["base_branch"], "main");
    assert_eq!(
        task["base_commit"],
        "0123456789abcdef0123456789abcdef01234567"
    );
    assert_eq!(task["branch"], "kanban");
    assert_eq!(task["worktree"], "/w/benterm/kanban");
    assert_eq!(task["role"], "reviewer");
    assert_eq!(task["closed"]["worktree"], "kept");
    assert_eq!(task["closed"]["at_ms"], 5);
}

#[test]
fn what_a_newer_build_wrote_beside_the_known_fields_is_kept() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tasks.json");
    std::fs::write(
        &path,
        r#"{"version":1,"next_id":8,"board":{"x":1},"tasks":[{"id":7,"name":"n",
        "repository":"/r","base_branch":"main","base_commit":"c","branch":"n",
        "worktree":"/w/n","created_at_ms":1,"review":"sent",
        "closed":{"at_ms":2,"worktree":"kept","why":"later"}}]}"#,
    )
    .unwrap();
    let store = TaskStore::open(Some(&path));
    assert_eq!(store.add(recorded("a")).unwrap().id, 8);
    let file: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(file["board"]["x"], 1);
    assert_eq!(file["tasks"][0]["review"], "sent");
    assert_eq!(file["tasks"][0]["closed"]["why"], "later");
    // Also when this build closes the task another way.
    store
        .set_closed(7, Closed::new(9, Worktree::Removed))
        .unwrap();
    let file: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(file["tasks"][0]["closed"]["worktree"], "removed");
    assert_eq!(file["tasks"][0]["closed"]["why"], "later");
    assert_eq!(file["tasks"][1]["id"], 8);
}

#[test]
fn a_file_this_build_cannot_read_is_left_as_it_is() {
    let dir = tempfile::tempdir().unwrap();
    for text in [r#"{"version":2,"tasks":[]}"#, "not json", r#"{"tasks":[]}"#] {
        let path = dir.path().join("tasks.json");
        std::fs::write(&path, text).unwrap();
        let store = TaskStore::open(Some(&path));
        let error = store.tasks().unwrap_err();
        assert!(error.to_string().contains("tasks.json"), "{error}");
        assert!(store.add(recorded("a")).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
    }
}

#[test]
fn without_a_file_the_tasks_live_as_long_as_the_store() {
    let store = TaskStore::open(None);
    let task = store.add(recorded("a")).unwrap();
    assert_eq!(store.tasks().unwrap(), [task]);
}

#[test]
fn a_branch_becomes_one_folder_name() {
    assert_eq!(folder_name("kanban"), "kanban");
    assert_eq!(folder_name("issue-93/kanban"), "issue-93-kanban");
    assert_eq!(folder_name("feature/a.b_c"), "feature-a.b_c");
    assert_eq!(folder_name("かんばん"), "かんばん");
    assert_eq!(folder_name(".hidden"), "hidden");
    // Nothing a folder can be named after: told apart by a number.
    let (plus, tilde) = (folder_name("+"), folder_name("+++"));
    assert!(plus.starts_with("branch-"), "{plus}");
    assert_ne!(plus, tilde);
}

#[test]
fn the_numbers_do_not_start_again_when_they_run_out() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tasks.json");
    std::fs::write(
        &path,
        format!(r#"{{"version":1,"next_id":{},"tasks":[]}}"#, u64::MAX),
    )
    .unwrap();
    let store = TaskStore::open(Some(&path));
    assert!(store.add(recorded("a")).is_err());
}

#[test]
fn a_linked_pull_request_survives_reopening_the_store() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("tasks.json");
    let store = TaskStore::open(Some(&path));
    let task = store.add(recorded("a")).unwrap();
    let link = benterm_task::PullRequestLink {
        repository: "octo/demo".into(),
        number: 4,
    };
    store.link_pull_request(task.id, link.clone()).unwrap();
    let reopened = TaskStore::open(Some(&path));
    assert_eq!(
        reopened.task(task.id).unwrap().unwrap().pull_request,
        Some(link.clone())
    );
    let other = benterm_task::PullRequestLink {
        number: 5,
        ..link.clone()
    };
    assert_eq!(reopened.link_pull_request(task.id, other).unwrap(), link);
}

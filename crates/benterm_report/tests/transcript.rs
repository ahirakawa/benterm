//! Reading what a turn did from a transcript of Claude Code or Codex.

mod common;

use benterm_report::{Rules, collect_facts, needs_report, parse_transcript, turn_start};
use common::{Claude, Codex, touch};

#[test]
fn the_edits_and_commands_of_the_turn_are_collected_and_no_others() {
    let dir = common::folder();
    let files = touch(dir.path(), &["old.rs", "a.rs", "b.md"]);
    let text = Claude::default()
        .prompt("p1", "first")
        .edit(&files[0])
        .bash("cargo build")
        .prompt("p2", "second")
        .edit(&files[1])
        .write(&files[2])
        .edit(&files[1])
        .bash("cargo test")
        .read("a.rs")
        .text();
    let entries = parse_transcript(&text);
    let start = turn_start(&entries, Some("p2")).unwrap();
    let facts = collect_facts(
        &entries,
        start,
        dir.path().to_str().unwrap(),
        &Rules::default(),
    );
    let edited: Vec<&str> = facts.edited_files.iter().map(String::as_str).collect();
    assert_eq!(
        edited,
        [files[1].to_str().unwrap(), files[2].to_str().unwrap()]
    );
    assert_eq!(facts.commands, ["cargo test"]);
    assert_eq!(facts.tool_calls, 5);
    assert_eq!(facts.report_file, None);
}

#[test]
fn stop_feedback_does_not_start_a_turn_of_claude_code() {
    let text = Claude::default()
        .prompt("p1", "do it")
        .bash("ls")
        .stop_feedback("p1", "write a report")
        .bash("pwd")
        .text();
    let entries = parse_transcript(&text);
    // With the id of the hook's input and without it.
    assert_eq!(turn_start(&entries, Some("p1")), Some(0));
    assert_eq!(turn_start(&entries, None), Some(0));
    assert_eq!(
        entries
            .iter()
            .filter(|entry| entry.is_stop_feedback())
            .count(),
        1
    );
}

#[test]
fn a_file_that_is_gone_an_ignored_one_and_a_subagents_are_left_out() {
    let dir = common::folder();
    let files = touch(dir.path(), &["kept.rs", ".claude/notes.md", "side.rs"]);
    let text = Claude::default()
        .prompt("p1", "do it")
        .edit(&files[0])
        .edit(&files[1])
        .edit(&dir.path().join("gone.rs"))
        .sidechain_edit(&files[2])
        .text();
    let entries = parse_transcript(&text);
    let facts = collect_facts(&entries, 0, dir.path().to_str().unwrap(), &Rules::default());
    assert_eq!(facts.edited_files, [files[0].to_str().unwrap()]);
}

/// A scratch file is not work to report. A folder of the project that
/// happens to be named `tmp` is.
#[test]
fn a_file_in_the_systems_temporary_folders_is_left_out() {
    let scratch = tempfile::tempdir().unwrap();
    let scratch_file = touch(scratch.path(), &["scratch.rs"]);
    let dir = common::folder();
    let files = touch(dir.path(), &["tmp/kept.rs"]);
    let text = Claude::default()
        .prompt("p1", "do it")
        .edit(&scratch_file[0])
        .edit(&scratch_file[0].canonicalize().unwrap())
        .edit(&files[0])
        .text();
    let entries = parse_transcript(&text);
    let facts = collect_facts(&entries, 0, dir.path().to_str().unwrap(), &Rules::default());
    assert_eq!(facts.edited_files, [files[0].to_str().unwrap()]);
}

#[test]
fn the_report_file_is_told_from_the_edited_files() {
    let dir = common::folder();
    let files = touch(dir.path(), &["a.rs", ".benterm/work.report.json"]);
    let text = Claude::default()
        .prompt("p1", "do it")
        .edit(&files[0])
        .write(&files[1])
        .text();
    let entries = parse_transcript(&text);
    let facts = collect_facts(&entries, 0, dir.path().to_str().unwrap(), &Rules::default());
    assert_eq!(facts.edited_files, [files[0].to_str().unwrap()]);
    assert_eq!(facts.report_file.as_deref(), files[1].to_str());
}

#[test]
fn a_turn_of_codex_is_read_by_its_turn_id() {
    let dir = common::folder();
    let files = touch(dir.path(), &["old.rs", "src/a.rs", "b.md"]);
    let text = Codex::default()
        .session(dir.path())
        .turn("t1", "first")
        .patch(&["old.rs"])
        .turn("t2", "second")
        .patch(&["src/a.rs", "b.md"])
        .exec("cargo test -p made_up")
        .says("done")
        .text();
    let entries = parse_transcript(&text);
    let start = turn_start(&entries, Some("t2")).unwrap();
    let facts = collect_facts(
        &entries,
        start,
        dir.path().to_str().unwrap(),
        &Rules::default(),
    );
    assert_eq!(
        facts.edited_files,
        [files[1].to_str().unwrap(), files[2].to_str().unwrap()]
    );
    assert_eq!(facts.commands, ["cargo test -p made_up"]);
    assert_eq!(facts.tool_calls, 2);
}

/// Codex records the reason of a block as a message of the user inside
/// a `hook_prompt` tag. It is feedback, whatever its words are, and the
/// turn goes on through it.
#[test]
fn stop_feedback_does_not_start_a_turn_of_codex() {
    let dir = common::folder();
    touch(dir.path(), &["a.rs", "b.rs"]);
    let text = Codex::default()
        .session(dir.path())
        .turn("t1", "do it")
        .patch(&["a.rs"])
        .stop_feedback("Any words at all, in any language")
        .patch(&["b.rs"])
        .text();
    let entries = parse_transcript(&text);
    for turn in [Some("t1"), None] {
        let start = turn_start(&entries, turn).unwrap();
        let facts = collect_facts(
            &entries,
            start,
            dir.path().to_str().unwrap(),
            &Rules::default(),
        );
        assert_eq!(facts.edited_files.len(), 2, "with turn id {turn:?}");
    }
    assert_eq!(
        entries
            .iter()
            .filter(|entry| entry.is_stop_feedback())
            .count(),
        1
    );
}

/// Only the tag itself is what a hook said: a prompt of the user that
/// starts with a longer word is a prompt.
#[test]
fn a_prompt_that_only_looks_like_the_tag_of_a_hook_is_a_prompt() {
    let prompt = |text: &str| {
        let line = serde_json::json!({
            "type": "response_item",
            "payload": {"type": "message", "role": "user", "content": text},
        });
        parse_transcript(&line.to_string()).remove(0)
    };
    let looks_like = prompt("<hook_prompt_example hook_run_id=\"stop:1:x\">hello");
    assert!(!looks_like.is_meta && !looks_like.is_stop_feedback());
    let other_hook =
        prompt("<hook_prompt hook_run_id=\"user_prompt_submit:1:x\">context</hook_prompt>");
    assert!(other_hook.is_meta && !other_hook.is_stop_feedback());
    let stop = prompt("<hook_prompt hook_run_id=\"stop:1:x\">fix it</hook_prompt>");
    assert!(stop.is_meta && stop.is_stop_feedback());
}

#[test]
fn a_report_is_needed_from_either_threshold_on() {
    let dir = common::folder();
    let files = touch(dir.path(), &["a.rs", "b.rs", "c.rs"]);
    let rules = Rules {
        edited_files_threshold: 3,
        tool_call_threshold: 4,
        ..Rules::default()
    };
    let facts = |text: String| {
        let entries = parse_transcript(&text);
        collect_facts(&entries, 0, dir.path().to_str().unwrap(), &rules)
    };
    let two_files = Claude::default()
        .prompt("p", "x")
        .edit(&files[0])
        .edit(&files[1]);
    assert!(!needs_report(&facts(two_files.text()), &rules));
    let three_files = Claude::default()
        .prompt("p", "x")
        .edit(&files[0])
        .edit(&files[1])
        .edit(&files[2]);
    assert!(needs_report(&facts(three_files.text()), &rules));
    let three_calls = Claude::default()
        .prompt("p", "x")
        .bash("a")
        .bash("b")
        .bash("c");
    assert!(!needs_report(&facts(three_calls.text()), &rules));
    let four_calls = Claude::default()
        .prompt("p", "x")
        .bash("a")
        .bash("b")
        .bash("c")
        .bash("d");
    assert!(needs_report(&facts(four_calls.text()), &rules));
}

#[test]
fn lines_that_are_not_json_are_skipped() {
    let text = format!(
        "not json\n{}{{\"type\":\n",
        Claude::default().prompt("p", "x").text()
    );
    assert_eq!(parse_transcript(&text).len(), 1);
}

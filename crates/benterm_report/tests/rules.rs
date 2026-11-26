//! The rules of the check, each with a report that passes and one that
//! does not.

mod common;

use benterm_report::{Report, ReportStatus, Rules, TurnFacts, check_report};
use common::{good_report, touch};
use serde_json::{Value, json};

struct Turn {
    dir: tempfile::TempDir,
    facts: TurnFacts,
}

/// A turn that edited `files` under a folder of its own and ran
/// `commands`.
fn turn(files: &[&str], commands: &[&str]) -> Turn {
    let dir = common::folder();
    let edited = touch(dir.path(), files);
    let facts = TurnFacts {
        edited_files: edited
            .iter()
            .map(|path| path.to_str().unwrap().to_owned())
            .collect(),
        commands: commands.iter().map(|command| command.to_string()).collect(),
        tool_calls: files.len() + commands.len(),
        report_file: None,
    };
    Turn { dir, facts }
}

fn problems_with(report: Value, reply: &str, turn: &Turn, rules: &Rules) -> Vec<String> {
    let report: Report = serde_json::from_value(report).unwrap();
    check_report(
        &report,
        reply,
        &turn.facts,
        turn.dir.path().to_str().unwrap(),
        rules,
    )
}

fn problems(report: Value, turn: &Turn) -> Vec<String> {
    problems_with(report, "Done.", turn, &Rules::default())
}

fn usual_turn() -> Turn {
    turn(&["src/a.rs", "docs/plan.md"], &[])
}

#[track_caller]
fn assert_one(problems: &[String], needle: &str) {
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(
        problems[0].contains(needle),
        "{problems:?} does not say {needle:?}"
    );
}

#[test]
fn a_report_that_follows_the_rules_passes() {
    assert_eq!(problems(good_report(), &usual_turn()), Vec::<String>::new());
}

#[test]
fn a_required_field_that_is_empty_fails() {
    for field in ["title", "goal", "summary"] {
        let mut report = good_report();
        report[field] = json!("  ");
        assert_one(&problems(report, &usual_turn()), field);
    }
}

#[test]
fn a_report_without_a_goal_a_conclusion_or_a_summary_is_read_and_fails() {
    for field in ["goal", "conclusion", "summary"] {
        let mut report = good_report();
        report.as_object_mut().unwrap().remove(field);
        assert_one(&problems(report, &usual_turn()), field);
    }
}

/// The status is one of the words of an agent's report to its parent.
#[test]
fn the_conclusion_is_one_of_the_report_statuses() {
    for status in ReportStatus::ALL {
        let mut report = good_report();
        report["conclusion"] = json!(status.as_str());
        assert_eq!(
            problems(report.clone(), &usual_turn()),
            Vec::<String>::new()
        );
        let report: Report = serde_json::from_value(report).unwrap();
        assert_eq!(report.status(), Some(status));
    }
    let mut report = good_report();
    report["conclusion"] = json!("The thing is done.");
    let found = problems(report, &usual_turn());
    assert_one(&found, "conclusion");
    assert!(found[0].contains("pass, issues or blocked"), "{found:?}");
}

#[test]
fn the_summary_the_question_and_the_reply_have_a_longest_length() {
    let rules = Rules {
        summary_max_chars: 10,
        question_max_chars: 8,
        reply_max_chars: 6,
        ..Rules::default()
    };
    let turn = usual_turn();
    let fits = |report: &mut Value| {
        report["summary"] = json!("十文字ちょうどの結論");
        report["decisions"][0]["question"] = json!("八文字の問いか？");
    };

    let mut report = good_report();
    fits(&mut report);
    assert_eq!(
        problems_with(report, "六文字の返事", &turn, &rules),
        Vec::<String>::new()
    );

    let mut report = good_report();
    fits(&mut report);
    report["summary"] = json!("十一文字になった結論だ");
    assert_one(
        &problems_with(report, "ok", &turn, &rules),
        "summary has 11",
    );

    let mut report = good_report();
    fits(&mut report);
    report["decisions"][0]["question"] = json!("九文字ある問いか？");
    assert_one(
        &problems_with(report, "ok", &turn, &rules),
        "question has 9",
    );

    let mut report = good_report();
    fits(&mut report);
    assert_one(
        &problems_with(report, "七文字ある返事", &turn, &rules),
        "reply",
    );
}

#[test]
fn the_recommended_choice_is_one_of_the_choices() {
    let mut report = good_report();
    report["decisions"][0]["recommend"] = json!("Replace");
    let found = problems(report, &usual_turn());
    assert_one(&found, "recommend");
    assert!(found[0].contains("Keep the old one?"), "{found:?}");
}

#[test]
fn a_decision_has_two_choices_or_more_each_with_its_effect_and_a_reason() {
    let mut report = good_report();
    report["decisions"][0]["options"]
        .as_array_mut()
        .unwrap()
        .truncate(1);
    report["decisions"][0]["recommend"] = json!("Keep");
    assert_one(&problems(report, &usual_turn()), "fewer than 2");

    let mut report = good_report();
    report["decisions"][0]["options"][0]["effect"] = json!("");
    assert_one(&problems(report, &usual_turn()), "effect");

    let mut report = good_report();
    report["decisions"][0]["reason"] = json!("");
    assert_one(&problems(report, &usual_turn()), "reason");

    let mut report = good_report();
    report["decisions"][0]["question"] = json!("");
    assert_one(&problems(report, &usual_turn()), "question");
}

#[test]
fn a_report_with_nothing_to_decide_passes() {
    let mut report = good_report();
    report["decisions"] = json!([]);
    assert_eq!(problems(report, &usual_turn()), Vec::<String>::new());
}

/// The example of the issue: four files edited, three of them listed.
#[test]
fn an_edited_file_that_is_not_listed_is_named() {
    let turn = turn(&["one.md", "two.md", "three.md", "four.md"], &[]);
    let mut report = good_report();
    report["deliverables"] = json!([
        {"path": "one.md", "summary": "One."},
        {"path": "two.md", "summary": "Two."},
        {"path": "three.md", "summary": "Three."},
    ]);
    let found = problems(report.clone(), &turn);
    assert_one(&found, "four.md");
    assert!(
        !found[0].contains(turn.dir.path().to_str().unwrap()),
        "{found:?}"
    );

    report["deliverables"]
        .as_array_mut()
        .unwrap()
        .push(json!({"path": "four.md", "summary": "Four."}));
    assert_eq!(problems(report, &turn), Vec::<String>::new());
}

#[test]
fn a_folder_covers_the_files_in_it_that_are_not_code() {
    let turn = turn(&["notes/a.md", "notes/deep/b.md", "notes/c.rs"], &[]);
    let mut report = good_report();
    report["deliverables"] = json!([{"path": "notes/", "summary": "Three notes."}]);
    assert_one(&problems(report, &turn), "notes/c.rs");
}

#[test]
fn a_code_file_has_its_changes_with_a_line_and_what_was_before_and_after() {
    let turn = turn(&["src/a.rs"], &[]);
    let with_changes = |changes: Value| {
        let mut report = good_report();
        report["deliverables"] =
            json!([{"path": "src/a.rs", "summary": "It.", "changes": changes}]);
        report
    };
    let change = json!({"line": 3, "before": "Did nothing.", "after": "Does it."});
    assert_eq!(
        problems(with_changes(json!([change])), &turn),
        Vec::<String>::new()
    );
    assert_one(&problems(with_changes(json!([])), &turn), "changes");
    for (field, value) in [
        ("line", json!(0)),
        ("before", json!("")),
        ("after", json!(" ")),
    ] {
        let mut bad = change.clone();
        bad[field] = value;
        assert_one(&problems(with_changes(json!([bad])), &turn), "src/a.rs");
    }
    let mut unlisted = good_report();
    unlisted["deliverables"] = json!([{"path": "src/", "summary": "The folder."}]);
    assert_one(&problems(unlisted, &turn), "src/a.rs");
}

#[test]
fn a_deliverable_has_a_path_and_a_summary() {
    let mut report = good_report();
    report["deliverables"][1]["summary"] = json!("");
    assert_one(&problems(report, &usual_turn()), "deliverables[1]");
}

/// The example of the issue: "ran the tests" with no test command.
#[test]
fn what_is_said_to_be_verified_needs_its_command_in_the_turn() {
    for (claim, word) in [
        ("テストを流した", "テスト"),
        ("Ran the tests", "test"),
        ("ビルドが通る", "ビルド"),
    ] {
        let mut report = good_report();
        report["verified"] = json!([claim]);
        let found = problems(report, &usual_turn());
        assert_one(&found, word);
    }

    let mut report = good_report();
    report["verified"] = json!(["テストを流した", "The build passes"]);
    let ran = turn(
        &["src/a.rs", "docs/plan.md"],
        &["cargo test --workspace", "cargo build"],
    );
    assert_eq!(problems(report.clone(), &ran), Vec::<String>::new());
    let only_tests = turn(&["src/a.rs", "docs/plan.md"], &["cargo test --workspace"]);
    assert_one(&problems(report, &only_tests), "build");
}

/// A word of the rules is looked for at the start of a word, so that
/// "latest" does not say "test".
#[test]
fn a_word_inside_another_word_claims_nothing() {
    let mut report = good_report();
    report["verified"] = json!(["Read the latest output", "Looked at the input"]);
    assert_eq!(problems(report, &usual_turn()), Vec::<String>::new());
}

#[test]
fn every_problem_of_a_report_is_told_at_once() {
    let mut report = good_report();
    report["title"] = json!("");
    report["decisions"][0]["recommend"] = json!("Neither");
    report["verified"] = json!(["pushed"]);
    assert_eq!(problems(report, &usual_turn()).len(), 3);
}

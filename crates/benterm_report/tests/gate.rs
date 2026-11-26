//! The decision at a Stop: let the agent stop, or send it back, and how
//! many times.

mod common;

use std::path::PathBuf;

use benterm_report::{BLOCK_PREFIX, HookInput, Rules, Verdict, check_stop, hint};
use common::{Claude, Codex, good_report, touch};
use serde_json::{Value, json};

/// A working directory with a transcript, and a folder for the counts.
struct Session {
    dir: tempfile::TempDir,
    rules: Rules,
}

impl Session {
    fn new() -> Self {
        let dir = common::folder();
        // The real path, as an agent names its files by it.
        std::fs::create_dir(dir.path().join("work")).unwrap();
        Self {
            dir,
            rules: Rules::default(),
        }
    }

    fn work(&self) -> PathBuf {
        self.dir.path().canonicalize().unwrap().join("work")
    }

    fn state(&self) -> PathBuf {
        self.dir.path().join("state")
    }

    fn files(&self, names: &[&str]) -> Vec<PathBuf> {
        touch(&self.work(), names)
    }

    fn report_path(&self) -> PathBuf {
        self.work().join(".benterm/work.report.json")
    }

    fn write_report(&self, report: &Value) {
        let path = self.report_path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, report.to_string()).unwrap();
    }

    fn input(&self, transcript: &str, turn: Value) -> HookInput {
        let path = self.dir.path().join("transcript.jsonl");
        std::fs::write(&path, transcript).unwrap();
        let mut input = json!({
            "session_id": "made-up-session",
            "transcript_path": path,
            "cwd": self.work(),
            "hook_event_name": "Stop",
            "last_assistant_message": "Done.",
        });
        input
            .as_object_mut()
            .unwrap()
            .extend(turn.as_object().unwrap().clone());
        HookInput::from_json(&input.to_string()).unwrap()
    }

    fn stop(&self, transcript: &str, turn: Value) -> Verdict {
        check_stop(&self.input(transcript, turn), &self.rules, &self.state()).unwrap()
    }

    fn hint(&self, transcript: &str, turn: Value) -> Option<String> {
        hint(&self.input(transcript, turn), &self.rules, &self.state()).unwrap()
    }

    /// A turn of Claude Code that edited the two files of the made-up
    /// report and one more, which is enough to need a report.
    fn large_turn(&self, prompt_id: &str) -> Claude {
        let files = self.files(&["src/a.rs", "docs/plan.md", "docs/more.md"]);
        Claude::default()
            .prompt(prompt_id, "do it")
            .edit(&files[0])
            .write(&files[1])
            .write(&files[2])
    }
}

fn report_for_three() -> Value {
    let mut report = good_report();
    report["deliverables"]
        .as_array_mut()
        .unwrap()
        .push(json!({"path": "docs/more.md", "summary": "More."}));
    report
}

#[track_caller]
fn reason(verdict: &Verdict) -> &str {
    match verdict {
        Verdict::Block { reason } => reason,
        other => panic!("not a block: {other:?}"),
    }
}

fn p(prompt_id: &str) -> Value {
    json!({"prompt_id": prompt_id})
}

#[test]
fn a_small_turn_stops_without_a_word() {
    let session = Session::new();
    let files = session.files(&["a.rs"]);
    let transcript = Claude::default()
        .prompt("p1", "fix it")
        .edit(&files[0])
        .bash("cargo test");
    assert_eq!(session.stop(&transcript.text(), p("p1")), Verdict::Quiet);
    assert!(
        !session.state().exists(),
        "a small turn leaves nothing behind"
    );
}

#[test]
fn a_large_turn_without_a_report_is_sent_back_with_how_to_write_one() {
    let session = Session::new();
    let verdict = session.stop(&session.large_turn("p1").text(), p("p1"));
    let reason = reason(&verdict);
    assert!(reason.starts_with(BLOCK_PREFIX), "{reason}");
    assert!(reason.contains(".benterm/"), "{reason}");
    assert!(reason.contains("\"deliverables\""), "{reason}");
    assert!(reason.contains("pass"), "{reason}");
    // A report written by a command of the shell is not seen.
    assert!(reason.contains("file-writing tool"), "{reason}");
}

/// The first stop without a report only teaches the form. After it the
/// agent is sent back `max_rounds` times, and then it stops with what
/// is still wrong.
#[test]
fn the_first_block_teaches_and_is_not_counted_and_the_count_has_a_limit() {
    let mut session = Session::new();
    session.rules.max_rounds = 2;
    let turn = session.large_turn("p1").text();
    for round in 0..3 {
        let verdict = session.stop(&turn, p("p1"));
        assert!(
            matches!(verdict, Verdict::Block { .. }),
            "stop {round}: {verdict:?}"
        );
    }
    match session.stop(&turn, p("p1")) {
        Verdict::Stop {
            report: None,
            problems,
        } => {
            assert_eq!(problems.len(), 1, "{problems:?}");
            assert!(problems[0].contains("No report"), "{problems:?}");
        }
        other => panic!("not a stop: {other:?}"),
    }
}

#[test]
fn a_report_with_problems_is_sent_back_with_them_and_passes_once_fixed() {
    let session = Session::new();
    let turn = session
        .large_turn("p1")
        .write(&session.report_path())
        .text();
    let mut report = report_for_three();
    report["decisions"][0]["recommend"] = json!("Neither");
    session.write_report(&report);
    let verdict = session.stop(&turn, p("p1"));
    assert!(reason(&verdict).contains("recommend"), "{verdict:?}");

    session.write_report(&report_for_three());
    match session.stop(&turn, p("p1")) {
        Verdict::Stop {
            report: Some(report),
            problems,
        } => {
            assert_eq!(problems, Vec::<String>::new());
            assert_eq!(report.title, "Made-up work");
            assert_eq!(report.decisions[0].question, "Keep the old one?");
        }
        other => panic!("not a pass: {other:?}"),
    }
}

/// A report that is there from the first stop on is not taught: its
/// first block counts.
#[test]
fn a_report_that_stays_wrong_is_shown_with_its_problems_at_the_limit() {
    let mut session = Session::new();
    session.rules.max_rounds = 1;
    let turn = session
        .large_turn("p1")
        .write(&session.report_path())
        .text();
    let mut report = report_for_three();
    report["title"] = json!("");
    session.write_report(&report);
    assert!(matches!(
        session.stop(&turn, p("p1")),
        Verdict::Block { .. }
    ));
    match session.stop(&turn, p("p1")) {
        Verdict::Stop {
            report: Some(_),
            problems,
        } => assert_eq!(problems.len(), 1),
        other => panic!("not a stop with the report: {other:?}"),
    }
}

#[test]
fn a_new_turn_starts_the_count_again() {
    let mut session = Session::new();
    session.rules.max_rounds = 1;
    let first = session.large_turn("p1").text();
    assert!(matches!(
        session.stop(&first, p("p1")),
        Verdict::Block { .. }
    ));
    assert!(matches!(
        session.stop(&first, p("p1")),
        Verdict::Block { .. }
    ));
    assert!(matches!(
        session.stop(&first, p("p1")),
        Verdict::Stop { .. }
    ));
    let second = format!("{first}{}", session.large_turn("p2").text());
    assert!(matches!(
        session.stop(&second, p("p2")),
        Verdict::Block { .. }
    ));
}

/// The count on disk is not the only limit: the blocks the transcript
/// records are counted too, so a count that was lost does not send the
/// agent back for ever.
#[test]
fn the_blocks_the_transcript_records_limit_the_count_by_themselves() {
    let mut session = Session::new();
    session.rules.max_rounds = 2;
    let mut turn = session.large_turn("p1");
    for _ in 0..3 {
        turn = turn.stop_feedback("p1", "write the report");
    }
    assert!(matches!(
        session.stop(&turn.text(), p("p1")),
        Verdict::Stop { .. }
    ));
}

#[test]
fn a_report_outside_its_folder_is_a_problem_of_the_agent() {
    let session = Session::new();
    let stray = session.files(&["elsewhere/.benterm/deep/x.report.json"]);
    let turn = session.large_turn("p1").write(&stray[0]).text();
    let verdict = session.stop(&turn, p("p1"));
    assert!(
        reason(&verdict).contains("Cannot read the report"),
        "{verdict:?}"
    );
}

#[test]
fn a_report_that_is_not_the_json_of_a_report_is_a_problem_of_the_agent() {
    let session = Session::new();
    let turn = session
        .large_turn("p1")
        .write(&session.report_path())
        .text();
    session.write_report(&json!({"title": "Only a title"}));
    let verdict = session.stop(&turn, p("p1"));
    assert!(
        reason(&verdict).contains("Cannot read the report"),
        "{verdict:?}"
    );
}

/// Codex names the turn `turn_id`, records its edits as patches with
/// paths from the working directory, and keeps the turn through a block.
#[test]
fn a_turn_of_codex_is_sent_back_and_counted_as_one_of_claude_code_is() {
    let mut session = Session::new();
    session.rules.max_rounds = 1;
    session.files(&["src/a.rs", "docs/plan.md", "docs/more.md"]);
    let turn = Codex::default()
        .session(&session.work())
        .turn("t1", "do it")
        .patch(&["src/a.rs", "docs/plan.md", "docs/more.md"]);
    let t = json!({"turn_id": "t1"});
    let first = session.stop(&turn.text(), t.clone());
    assert!(reason(&first).starts_with(BLOCK_PREFIX));

    let turn = turn.stop_feedback(reason(&first)).says("I will write it.");
    assert!(matches!(
        session.stop(&turn.text(), t.clone()),
        Verdict::Block { .. }
    ));
    let turn = turn.stop_feedback("again");
    assert!(matches!(
        session.stop(&turn.text(), t.clone()),
        Verdict::Stop { .. }
    ));

    session.write_report(&report_for_three());
    let turn = turn.patch(&[".benterm/work.report.json"]);
    match session.stop(&turn.text(), t) {
        Verdict::Stop {
            report: Some(_),
            problems,
        } => assert_eq!(problems, Vec::<String>::new()),
        other => panic!("not a pass: {other:?}"),
    }
}

#[test]
fn the_reply_is_the_last_thing_the_agent_said_when_the_hook_gives_none() {
    let mut session = Session::new();
    session.rules.reply_max_chars = 10;
    session.write_report(&report_for_three());
    let turn = session
        .large_turn("p1")
        .write(&session.report_path())
        .says("A reply that is longer than ten characters.");
    let verdict = session.stop(
        &turn.text(),
        json!({"prompt_id": "p1", "last_assistant_message": ""}),
    );
    assert!(reason(&verdict).contains("reply"), "{verdict:?}");
}

#[test]
fn a_transcript_that_cannot_be_read_is_an_error_and_not_a_block() {
    let session = Session::new();
    let mut input = session.input("", p("p1"));
    input.transcript_path = Some(
        session
            .dir
            .path()
            .join("none.jsonl")
            .to_string_lossy()
            .into_owned(),
    );
    assert!(check_stop(&input, &session.rules, &session.state()).is_err());
    input.transcript_path = None;
    assert!(check_stop(&input, &session.rules, &session.state()).is_err());
}

#[test]
fn the_form_is_handed_over_once_when_the_turn_comes_to_need_a_report() {
    let session = Session::new();
    let files = session.files(&["src/a.rs", "docs/plan.md", "docs/more.md"]);
    let small = Claude::default()
        .prompt("p1", "do it")
        .edit(&files[0])
        .edit(&files[1]);
    assert_eq!(session.hint(&small.text(), p("p1")), None);

    let large = small.edit(&files[2]);
    let handed = session
        .hint(&large.text(), p("p1"))
        .expect("the turn needs a report now");
    assert!(handed.contains(".benterm/"), "{handed}");
    assert!(handed.contains("\"deliverables\""), "{handed}");

    let larger = large.bash("cargo test");
    assert_eq!(session.hint(&larger.text(), p("p1")), None, "once a turn");

    let next = format!("{}{}", larger.text(), session.large_turn("p2").text());
    assert!(
        session.hint(&next, p("p2")).is_some(),
        "again in the next turn"
    );
}

#[test]
fn the_form_is_not_handed_over_once_the_report_is_written() {
    let session = Session::new();
    session.write_report(&report_for_three());
    let turn = session.large_turn("p1").write(&session.report_path());
    assert_eq!(session.hint(&turn.text(), p("p1")), None);
}

/// The hint leaves the count of the Stop alone: the first stop after it
/// still teaches.
#[test]
fn a_hint_does_not_use_up_a_round() {
    let mut session = Session::new();
    session.rules.max_rounds = 1;
    let turn = session.large_turn("p1").text();
    assert!(session.hint(&turn, p("p1")).is_some());
    assert!(matches!(
        session.stop(&turn, p("p1")),
        Verdict::Block { .. }
    ));
    assert!(matches!(
        session.stop(&turn, p("p1")),
        Verdict::Block { .. }
    ));
    assert!(matches!(session.stop(&turn, p("p1")), Verdict::Stop { .. }));
}

#[test]
fn the_lengths_of_the_rules_are_the_ones_the_form_names() {
    let rules = Rules {
        summary_max_chars: 123,
        question_max_chars: 45,
        reply_max_chars: 678,
        ..Rules::default()
    };
    let form = benterm_report::report_format(&rules);
    for length in ["123", "45", "678"] {
        assert!(form.contains(length), "{form}");
    }
}

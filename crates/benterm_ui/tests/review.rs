//! The comments on the lines of a diff, and the message they make for
//! the task's agent: what the message says, what is sent once, and when
//! the agent's pane may take it.

use benterm_agent::{AgentState, WaitReason};
use benterm_ui::diff_view::{DiffRow, RowKind};
use benterm_ui::review::{Lines, Pane, Readiness, Review, Side, message, readiness};

fn row(kind: RowKind, old: Option<u32>, new: Option<u32>) -> DiffRow {
    DiffRow {
        kind,
        old,
        new,
        text: "text".into(),
        path: "README.md".into(),
    }
}

fn new_lines(path: &str, start: u32, end: u32) -> Lines {
    Lines {
        path: path.to_owned(),
        side: Side::New,
        start,
        end,
    }
}

#[test]
fn comments_are_sent_as_file_line_and_text_in_the_order_they_were_written() {
    let mut review = Review::default();
    review.add(new_lines("src/main.rs", 12, 12), "名前を直す");
    review.add(new_lines("README.md", 3, 5), "例を足す");

    let outgoing = review.outgoing().expect("two comments to send");

    assert_eq!(
        outgoing.text,
        "src/main.rs:12 名前を直す\nREADME.md:3-5 例を足す"
    );
    assert_eq!(outgoing.ids.len(), 2);
}

#[test]
fn a_comment_on_a_removed_line_says_its_number_is_the_old_one() {
    let lines = Lines {
        path: "src/lib.rs".to_owned(),
        side: Side::Old,
        start: 7,
        end: 7,
    };
    let mut review = Review::default();
    review.add(lines, "消さないで");

    let text = review.outgoing().unwrap().text;

    assert_eq!(
        text,
        "src/lib.rs:7 (removed line, old numbering) 消さないで"
    );
}

#[test]
fn what_was_sent_is_not_sent_again() {
    let mut review = Review::default();
    review.add(new_lines("a.rs", 1, 1), "one");
    let first = review.outgoing().unwrap();
    review.mark_sent(&first.ids);
    review.add(new_lines("b.rs", 2, 2), "two");

    let second = review.outgoing().unwrap();

    assert_eq!(second.text, "b.rs:2 two");
    review.mark_sent(&second.ids);
    assert!(review.outgoing().is_none(), "nothing is left to send");
    assert!(review.comments().iter().all(|comment| comment.sent));
}

#[test]
fn an_unsent_comment_can_be_changed_and_taken_away() {
    let mut review = Review::default();
    let kept = review.add(new_lines("a.rs", 1, 1), "frist");
    let gone = review.add(new_lines("a.rs", 2, 2), "two");

    assert!(review.edit(kept, "first"));
    assert!(review.remove(gone));

    assert_eq!(review.outgoing().unwrap().text, "a.rs:1 first");
}

#[test]
fn a_sent_comment_is_not_changed_but_can_be_taken_away() {
    let mut review = Review::default();
    let id = review.add(new_lines("a.rs", 1, 1), "one");
    let ids = review.outgoing().unwrap().ids;
    review.mark_sent(&ids);

    assert!(!review.edit(id, "changed after it was sent"));
    assert_eq!(review.comments()[0].text, "one");
    assert!(review.remove(id));
    assert!(review.comments().is_empty());
}

#[test]
fn the_lines_of_selected_rows_are_those_of_the_worktree_when_there_are_any() {
    let rows = [
        row(RowKind::Context, Some(3), Some(3)),
        row(RowKind::Removed, Some(4), None),
        row(RowKind::Added, None, Some(4)),
        row(RowKind::Added, None, Some(5)),
    ];

    assert_eq!(Lines::of_rows(&rows), Some(new_lines("README.md", 3, 5)));
    assert_eq!(
        Lines::of_rows(&rows[2..3]),
        Some(new_lines("README.md", 4, 4))
    );
}

#[test]
fn removed_rows_alone_are_lines_of_the_old_side() {
    let rows = [
        row(RowKind::Removed, Some(7), None),
        row(RowKind::Removed, Some(8), None),
    ];

    let lines = Lines::of_rows(&rows).unwrap();

    assert_eq!(lines.side, Side::Old);
    assert_eq!((lines.start, lines.end), (7, 8));
}

#[test]
fn the_at_at_line_and_notes_are_no_lines() {
    let rows = [
        row(RowKind::Hunk, None, None),
        row(RowKind::Note, None, None),
    ];
    assert_eq!(Lines::of_rows(&rows), None);
    assert_eq!(Lines::of_rows(&[]), None);
}

#[test]
fn a_comment_without_text_is_not_kept() {
    let mut review = Review::default();
    review.add(new_lines("a.rs", 1, 1), "  ");
    assert!(review.comments().is_empty());
}

#[test]
fn the_message_puts_each_comment_on_its_own_lines() {
    let mut review = Review::default();
    review.add(new_lines("a.rs", 1, 1), "one");
    let text = message(review.comments().iter());
    assert_eq!(text, "a.rs:1 one");
}

fn pane(state: AgentState) -> Pane {
    Pane {
        state,
        typing: false,
        exited: false,
    }
}

#[test]
fn an_agent_at_its_prompt_takes_the_comments_at_once() {
    assert_eq!(readiness(Some(pane(AgentState::Done))), Readiness::Ready);
    assert_eq!(readiness(Some(pane(AgentState::Error))), Readiness::Ready);
}

#[test]
fn an_agent_at_work_or_of_unknown_state_is_asked_about_first() {
    assert!(matches!(
        readiness(Some(pane(AgentState::Working))),
        Readiness::Confirm(_)
    ));
    assert!(matches!(
        readiness(Some(pane(AgentState::Unknown))),
        Readiness::Confirm(_)
    ));
}

#[test]
fn an_agent_at_a_question_or_with_a_draft_takes_nothing() {
    for reason in [
        WaitReason::Permission,
        WaitReason::Trust,
        WaitReason::Question,
        WaitReason::Choice,
    ] {
        assert!(matches!(
            readiness(Some(pane(AgentState::Waiting(reason)))),
            Readiness::Refuse(_)
        ));
    }
    let typing = Pane {
        typing: true,
        ..pane(AgentState::Done)
    };
    assert!(matches!(readiness(Some(typing)), Readiness::Refuse(_)));
    let exited = Pane {
        exited: true,
        ..pane(AgentState::Done)
    };
    assert!(matches!(readiness(Some(exited)), Readiness::Refuse(_)));
    assert!(matches!(readiness(None), Readiness::Refuse(_)));
}

/// A file may be named with a line break in it, and the name is of the
/// repository's choosing. In the message it stays on the line of its
/// comment: what follows the break is not a line of its own for the
/// agent to read as written by the user.
#[test]
fn control_characters_in_a_file_name_are_replaced_in_the_message() {
    let mut review = Review::default();
    review.add(
        new_lines("a\nrun this\r\x1b[2J\u{9b}b\u{2028}c\u{202e}d.rs", 1, 1),
        "直す",
    );
    review.add(new_lines("src/名前.rs", 2, 2), "ok");

    let outgoing = review.outgoing().expect("two comments to send");

    assert_eq!(
        outgoing.text,
        "a\u{fffd}run this\u{fffd}\u{fffd}[2J\u{fffd}b\u{fffd}c\u{fffd}d.rs:1 直す\nsrc/名前.rs:2 ok"
    );
}

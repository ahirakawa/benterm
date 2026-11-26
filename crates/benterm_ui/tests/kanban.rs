//! Which column of the kanban a task is in, from what the window
//! recorded of its agents and its review.

use benterm_session::Agent;
use benterm_session::history::{Record, State, Waiting};
use benterm_task::Worktree;
use benterm_ui::kanban::{BoardTask, Column, Dot, Event, Happening, Sent, card, column, events};
use proptest::prelude::*;

fn record(at_ms: u64, key: &str, agent: Option<Agent>, state: State) -> Record {
    Record {
        at_ms,
        key: key.into(),
        pane: 1,
        agent,
        state,
    }
}

fn event(at_ms: u64, pane: &str, happening: Happening) -> Event {
    Event {
        at_ms,
        pane: pane.into(),
        happening,
    }
}

#[test]
fn a_record_tells_what_the_agent_did() {
    let claude = Some(Agent::Claude);
    let cases = [
        (claude, State::Working, Some(Happening::Working)),
        (
            claude,
            State::Waiting(Waiting::Permission),
            Some(Happening::Waiting),
        ),
        (
            claude,
            State::Waiting(Waiting::Question),
            Some(Happening::Waiting),
        ),
        (claude, State::Error, Some(Happening::Error)),
        (claude, State::Done, Some(Happening::TurnEnded)),
        // An agent whose state nothing showed yet did nothing.
        (claude, State::Unknown, None),
        (None, State::Unknown, Some(Happening::AgentLeft)),
    ];
    for (agent, state, happening) in cases {
        let record = record(5, "a", agent, state);
        assert_eq!(Happening::of_record(&record), happening, "{state:?}");
    }
}

#[test]
fn the_events_are_those_of_the_task_panes_with_the_comments_oldest_first() {
    let claude = Some(Agent::Claude);
    let records = [
        record(10, "a", claude, State::Working),
        record(20, "other", claude, State::Working),
        record(30, "a", claude, State::Done),
        record(40, "b", claude, State::Unknown),
        record(50, "b", None, State::Unknown),
    ];
    let panes = ["a".to_string(), "b".to_string()];
    let sends = [("a".to_string(), 35), ("other".to_string(), 36)];
    assert_eq!(
        events(&records, &panes, &sends),
        vec![
            event(10, "a", Happening::Working),
            event(30, "a", Happening::TurnEnded),
            event(35, "a", Happening::ReviewSent),
            event(50, "b", Happening::AgentLeft),
        ]
    );
}

#[test]
fn a_task_nothing_happened_in_is_not_started() {
    assert_eq!(column(false, &[]), Column::NotStarted);
}

#[test]
fn a_working_or_waiting_agent_puts_its_task_in_agent_working() {
    for happening in [Happening::Working, Happening::Waiting, Happening::Error] {
        let events = [event(1, "a", happening)];
        assert_eq!(
            column(false, &events),
            Column::AgentWorking,
            "{happening:?}"
        );
    }
}

#[test]
fn an_agent_that_finished_its_turn_puts_its_task_in_review() {
    let events = [
        event(1, "a", Happening::Working),
        event(2, "a", Happening::TurnEnded),
    ];
    assert_eq!(column(false, &events), Column::AwaitingReview);
}

#[test]
fn comments_sent_move_the_task_back_before_the_agent_starts() {
    let events = [
        event(1, "a", Happening::Working),
        event(2, "a", Happening::TurnEnded),
        event(3, "a", Happening::ReviewSent),
    ];
    assert_eq!(column(false, &events), Column::AgentWorking);
}

#[test]
fn one_agent_still_at_work_keeps_the_task_in_agent_working() {
    let events = [
        event(1, "a", Happening::Working),
        event(2, "b", Happening::Working),
        event(3, "a", Happening::TurnEnded),
    ];
    assert_eq!(column(false, &events), Column::AgentWorking);
}

#[test]
fn an_agent_that_went_away_leaves_the_changes_to_review() {
    let events = [
        event(1, "a", Happening::Working),
        event(2, "a", Happening::AgentLeft),
    ];
    assert_eq!(column(false, &events), Column::AwaitingReview);
}

#[test]
fn a_closed_task_is_done() {
    let events = [event(1, "a", Happening::Working)];
    assert_eq!(column(true, &events), Column::Done);
    assert_eq!(column(true, &[]), Column::Done);
}

fn happening() -> impl Strategy<Value = Happening> {
    prop_oneof![
        Just(Happening::Working),
        Just(Happening::Waiting),
        Just(Happening::Error),
        Just(Happening::TurnEnded),
        Just(Happening::AgentLeft),
        Just(Happening::ReviewSent),
    ]
}

/// Events in up to three panes, oldest first, no two at the same time.
fn history() -> impl Strategy<Value = Vec<Event>> {
    prop::collection::vec((1u64..50, 0usize..3, happening()), 0..20).prop_map(|steps| {
        let mut at_ms = 0;
        steps
            .into_iter()
            .map(|(gap, pane, happening)| {
                at_ms += gap;
                event(at_ms, ["a", "b", "c"][pane], happening)
            })
            .collect()
    })
}

proptest! {
    /// A closed task is done whatever its agents did.
    #[test]
    fn closed_is_always_done(events in history()) {
        prop_assert_eq!(column(true, &events), Column::Done);
    }

    /// An open task is never done, and is not started only while
    /// nothing happened in it.
    #[test]
    fn an_open_task_is_not_started_only_without_events(events in history()) {
        let column = column(false, &events);
        prop_assert_ne!(column, Column::Done);
        prop_assert_eq!(column == Column::NotStarted, events.is_empty());
    }

    /// Whatever happened before, the agent starting again, waiting,
    /// failing, or taking comments puts the task in agent working.
    #[test]
    fn the_agent_at_work_again_moves_the_task_to_agent_working(
        mut events in history(),
        pane in 0usize..3,
        last in prop_oneof![
            Just(Happening::Working),
            Just(Happening::Waiting),
            Just(Happening::Error),
            Just(Happening::ReviewSent),
        ],
    ) {
        let at_ms = events.last().map_or(1, |event| event.at_ms + 1);
        events.push(event(at_ms, ["a", "b", "c"][pane], last));
        prop_assert_eq!(column(false, &events), Column::AgentWorking);
    }

    /// When the last agent of a task ends its turn or goes away, the
    /// task waits for review.
    #[test]
    fn every_agent_at_rest_moves_the_task_to_review(
        mut events in history(),
        last in prop_oneof![Just(Happening::TurnEnded), Just(Happening::AgentLeft)],
    ) {
        let mut at_ms = events.last().map_or(0, |event| event.at_ms);
        for pane in ["a", "b", "c"] {
            at_ms += 1;
            events.push(event(at_ms, pane, last));
        }
        prop_assert_eq!(column(false, &events), Column::AwaitingReview);
    }

    /// The column depends on the last thing that happened in each pane,
    /// not on what came before it there.
    #[test]
    fn only_the_last_event_of_each_pane_counts(events in history()) {
        let mut last: Vec<Event> = Vec::new();
        for event in &events {
            last.retain(|kept| kept.pane != event.pane);
            last.push(event.clone());
        }
        prop_assert_eq!(column(false, &events), column(false, &last));
    }
}

const MINUTE: u64 = 60_000;

fn task(panes: &[&str]) -> BoardTask {
    BoardTask {
        id: 7,
        name: "add-export-csv".into(),
        repository: "api".into(),
        closed: None,
        panes: panes.iter().map(|pane| pane.to_string()).collect(),
        sends: Vec::new(),
    }
}

fn shown(task: &BoardTask, records: &[Record], now_ms: u64) -> (Column, Dot, String) {
    let card = card(task, records, now_ms);
    assert_eq!((card.task, card.name.as_str()), (7, "add-export-csv"));
    (card.column, card.dot, card.meta)
}

#[test]
fn a_task_not_started_shows_its_repository() {
    assert_eq!(
        shown(&task(&["a"]), &[], 0),
        (Column::NotStarted, Dot::Idle, "api".into())
    );
}

#[test]
fn a_working_agent_shows_how_long_it_has_worked() {
    let records = [
        record(10 * MINUTE, "a", Some(Agent::Claude), State::Working),
        record(
            11 * MINUTE,
            "other",
            Some(Agent::Codex),
            State::Waiting(Waiting::Question),
        ),
    ];
    assert_eq!(
        shown(&task(&["a"]), &records, 42 * MINUTE),
        (
            Column::AgentWorking,
            Dot::Working,
            "claude · working 32 min".into()
        )
    );
}

#[test]
fn a_waiting_agent_of_two_is_what_the_card_shows() {
    let records = [
        record(MINUTE, "a", Some(Agent::Claude), State::Working),
        record(2 * MINUTE, "b", Some(Agent::Codex), State::Working),
        record(
            3 * MINUTE,
            "a",
            Some(Agent::Claude),
            State::Waiting(Waiting::Permission),
        ),
    ];
    assert_eq!(
        shown(&task(&["a", "b"]), &records, 5 * MINUTE),
        (
            Column::AgentWorking,
            Dot::Waiting,
            "claude + codex · waiting for input".into()
        )
    );
}

#[test]
fn an_agent_stopped_on_an_error_shows_it() {
    let records = [record(MINUTE, "a", Some(Agent::Codex), State::Error)];
    assert_eq!(
        shown(&task(&["a"]), &records, 5 * MINUTE),
        (
            Column::AgentWorking,
            Dot::Error,
            "codex · stopped on an error".into()
        )
    );
}

#[test]
fn comments_just_sent_show_how_many_went() {
    let records = [record(MINUTE, "a", Some(Agent::Claude), State::Done)];
    let mut task = task(&["a"]);
    task.sends.push(Sent {
        pane: "a".into(),
        at_ms: 2 * MINUTE,
        comments: 3,
    });
    assert_eq!(
        shown(&task, &records, 3 * MINUTE),
        (
            Column::AgentWorking,
            Dot::Working,
            "claude · 3 comments sent".into()
        )
    );
}

#[test]
fn a_task_in_review_shows_the_comments_sent_so_far() {
    let mut records = vec![record(MINUTE, "a", Some(Agent::Claude), State::Done)];
    assert_eq!(
        shown(&task(&["a"]), &records, 3 * MINUTE),
        (Column::AwaitingReview, Dot::Done, "no comments sent".into())
    );
    let mut task = task(&["a"]);
    for (at_ms, comments) in [(2 * MINUTE, 2), (4 * MINUTE, 1)] {
        task.sends.push(Sent {
            pane: "a".into(),
            at_ms,
            comments,
        });
    }
    records.push(record(5 * MINUTE, "a", Some(Agent::Claude), State::Working));
    records.push(record(6 * MINUTE, "a", Some(Agent::Claude), State::Done));
    assert_eq!(
        shown(&task, &records, 7 * MINUTE),
        (Column::AwaitingReview, Dot::Done, "3 comments sent".into())
    );
}

#[test]
fn a_closed_task_shows_what_became_of_its_worktree() {
    for (worktree, meta) in [
        (Worktree::Kept, "closed · worktree kept"),
        (Worktree::Removed, "closed · worktree removed"),
    ] {
        let mut task = task(&[]);
        task.closed = Some(worktree);
        assert_eq!(shown(&task, &[], 0), (Column::Done, Dot::Done, meta.into()));
    }
}

//! The rows of the session list, without a window: which terminals get a
//! row, what a row says, and the order of the rows, children under the
//! agent that started them, and what is under each row to answer its
//! agent with.

use std::collections::HashMap;
use std::path::PathBuf;

use benterm_agent::{AgentState, Plan, PlanItem, PlanStatus, WaitReason};
use benterm_ipc::ReportStatus;
use benterm_layout::{PaneId, PaneTree, Size};
use benterm_report::{Choice, Decision, Report};
use benterm_ui::checked_reports::{CheckedReport, shown_line, shown_text};
use benterm_ui::session_rows::{
    AnswerKey, PLAN_LINES, ReportLine, ReportMark, SessionButton, SessionEntry, SessionPlace,
    SessionRow, SessionState, session_rows,
};
use proptest::prelude::*;

/// `count` ids no other pane has, in rising order.
fn panes(count: usize) -> Vec<PaneId> {
    (0..count)
        .map(|_| PaneTree::new(Size::new(1, 1)).focused())
        .collect()
}

/// A terminal behind the window in which `agent` was recognised.
fn entry(pane: PaneId, agent: Option<&'static str>, state: AgentState) -> SessionEntry {
    SessionEntry {
        pane,
        parent: None,
        agent,
        state,
        name: None,
        directory: None,
        place: SessionPlace::Background,
        unseen: false,
        marked: false,
        plan: None,
        report_status: None,
        checked: None,
        stalled: false,
        msg: None,
    }
}

/// An agent started under `name` by `parent`.
fn named(pane: PaneId, name: &str, parent: Option<PaneId>) -> SessionEntry {
    SessionEntry {
        parent,
        name: Some(name.into()),
        ..entry(pane, Some("claude"), AgentState::Working)
    }
}

fn names(entries: Vec<SessionEntry>) -> Vec<(String, usize)> {
    session_rows(entries)
        .into_iter()
        .map(|row| (row.label(), row.depth))
        .collect()
}

#[test]
fn a_terminal_without_an_agent_has_no_row() {
    let [claude, codex, shell] = panes(3).try_into().unwrap();
    let rows = session_rows(vec![
        entry(claude, Some("claude"), AgentState::Working),
        entry(
            codex,
            Some("codex"),
            AgentState::Waiting(WaitReason::Question),
        ),
        entry(shell, None, AgentState::Unknown),
    ]);
    let shown: Vec<_> = rows.iter().map(|row| (row.pane, row.state)).collect();
    assert_eq!(
        shown,
        vec![
            (claude, SessionState::Working),
            (codex, SessionState::Waiting(WaitReason::Question)),
        ]
    );
    assert_eq!(rows[0].kind(), "Claude Code");
    assert_eq!(rows[1].kind(), "Codex");
}

/// A program that is no agent tells its state with OSC 7501.
#[test]
fn a_program_that_tells_its_state_has_a_row_without_being_an_agent() {
    let question = AgentState::Waiting(WaitReason::Question);
    for (state, shown) in [
        (AgentState::Working, SessionState::Working),
        (question, SessionState::Waiting(WaitReason::Question)),
        (AgentState::Error, SessionState::Error),
    ] {
        let [pane] = panes(1).try_into().unwrap();
        let rows = session_rows(vec![entry(pane, None, state)]);
        assert_eq!(rows.len(), 1, "{state:?}");
        assert_eq!(rows[0].state, shown);
        assert_eq!(rows[0].kind(), "Program");
    }
}

/// A program at rest has no row, whether it finished or waits for the
/// next instruction: nobody is asked for anything, and its pane is not
/// marked, so nothing would ever take the row away.
#[test]
fn a_program_at_rest_has_no_row() {
    let [pane] = panes(1).try_into().unwrap();
    assert!(session_rows(vec![entry(pane, None, AgentState::Done)]).is_empty());
    let unseen = SessionEntry {
        unseen: true,
        ..entry(pane, None, AgentState::Done)
    };
    assert!(session_rows(vec![unseen]).is_empty());
}

/// The keys that allow and deny are those of the agents' dialogs.
#[test]
fn a_program_asking_for_permission_only_goes_to_its_pane() {
    let [pane] = panes(1).try_into().unwrap();
    let permission = AgentState::Waiting(WaitReason::Permission);
    let row = session_rows(vec![entry(pane, None, permission)]).remove(0);
    let controls = row.controls();
    assert!(!controls.field);
    assert_eq!(controls.buttons.to_vec(), vec![SessionButton::Go]);
}

#[test]
fn an_agent_whose_state_nothing_has_shown_yet_has_no_row() {
    let [pane] = panes(1).try_into().unwrap();
    let rows = session_rows(vec![entry(pane, Some("claude"), AgentState::Unknown)]);
    assert!(rows.is_empty());
}

#[test]
fn an_agent_started_under_a_name_has_a_row_before_it_is_recognised() {
    let [pane] = panes(1).try_into().unwrap();
    let rows = session_rows(vec![SessionEntry {
        agent: None,
        state: AgentState::Unknown,
        ..named(pane, "w1", None)
    }]);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].state, SessionState::Starting);
    assert_eq!(rows[0].kind(), "Agent");
    assert_eq!(rows[0].label(), "w1");
}

#[test]
fn a_row_is_called_by_its_name_or_else_by_its_directory() {
    let [with_name, with_directory, with_neither] = panes(3).try_into().unwrap();
    let directory = Some(PathBuf::from("/work/api"));
    let rows = session_rows(vec![
        SessionEntry {
            directory: directory.clone(),
            ..named(with_name, "reviewer", None)
        },
        SessionEntry {
            directory,
            ..entry(with_directory, Some("codex"), AgentState::Done)
        },
        entry(with_neither, Some("codex"), AgentState::Done),
    ]);
    let labels: Vec<String> = rows.iter().map(|row| row.label()).collect();
    assert_eq!(labels, vec!["reviewer", "api", ""]);
}

#[test]
fn a_finished_turn_is_done_until_it_was_seen_and_idle_after() {
    let [unseen, seen] = panes(2).try_into().unwrap();
    let rows = session_rows(vec![
        SessionEntry {
            unseen: true,
            ..entry(unseen, Some("claude"), AgentState::Done)
        },
        entry(seen, Some("claude"), AgentState::Done),
    ]);
    assert_eq!(rows[0].state, SessionState::Done);
    assert_eq!(rows[1].state, SessionState::Idle);
}

#[test]
fn a_row_at_work_has_the_stall_mark_of_its_terminal() {
    let [stalled, working, done] = panes(3).try_into().unwrap();
    let rows = session_rows(vec![
        SessionEntry {
            stalled: true,
            ..entry(stalled, Some("claude"), AgentState::Working)
        },
        entry(working, Some("codex"), AgentState::Working),
        // Only work stalls; a mark left from before is not shown.
        SessionEntry {
            stalled: true,
            ..entry(done, Some("claude"), AgentState::Done)
        },
    ]);
    let marks: Vec<(PaneId, bool)> = rows.iter().map(|row| (row.pane, row.stalled)).collect();
    assert_eq!(
        marks,
        vec![(stalled, true), (working, false), (done, false)]
    );
    assert_eq!(rows[0].state, SessionState::Working);
}

#[test]
fn a_row_has_the_mark_of_its_terminal() {
    let [marked, plain] = panes(2).try_into().unwrap();
    let rows = session_rows(vec![
        SessionEntry {
            marked: true,
            ..entry(marked, Some("claude"), AgentState::Done)
        },
        entry(plain, Some("codex"), AgentState::Working),
    ]);
    let marks: Vec<(PaneId, bool)> = rows.iter().map(|row| (row.pane, row.marked)).collect();
    assert_eq!(marks, vec![(marked, true), (plain, false)]);
}

#[test]
fn waiting_and_failed_agents_stay_so_after_they_were_seen() {
    let [waiting, failed] = panes(2).try_into().unwrap();
    let rows = session_rows(vec![
        entry(
            waiting,
            Some("claude"),
            AgentState::Waiting(WaitReason::Permission),
        ),
        entry(failed, Some("codex"), AgentState::Error),
    ]);
    assert_eq!(rows[0].state, SessionState::Waiting(WaitReason::Permission));
    assert_eq!(rows[1].state, SessionState::Error);
}

#[test]
fn only_a_waiting_agent_takes_a_reply() {
    let [waiting, working] = panes(2).try_into().unwrap();
    let rows = session_rows(vec![
        entry(
            waiting,
            Some("claude"),
            AgentState::Waiting(WaitReason::Question),
        ),
        entry(working, Some("claude"), AgentState::Working),
    ]);
    assert!(rows[0].takes_reply());
    assert!(!rows[1].takes_reply());
}

#[test]
fn children_come_right_after_their_parent_one_step_in() {
    let [p, other, w1, w2] = panes(4).try_into().unwrap();
    let shown = names(vec![
        named(p, "p", None),
        named(other, "other", None),
        named(w1, "w1", Some(p)),
        named(w2, "w2", Some(p)),
    ]);
    assert_eq!(
        shown,
        vec![
            ("p".to_owned(), 0),
            ("w1".to_owned(), 1),
            ("w2".to_owned(), 1),
            ("other".to_owned(), 0),
        ]
    );
}

#[test]
fn a_grandchild_is_two_steps_in() {
    let [p, child, grandchild] = panes(3).try_into().unwrap();
    let shown = names(vec![
        named(grandchild, "grandchild", Some(child)),
        named(child, "child", Some(p)),
        named(p, "p", None),
    ]);
    assert_eq!(
        shown,
        vec![
            ("p".to_owned(), 0),
            ("child".to_owned(), 1),
            ("grandchild".to_owned(), 2),
        ]
    );
}

#[test]
fn a_child_whose_parent_has_no_row_is_at_the_top() {
    let [shell, w1] = panes(2).try_into().unwrap();
    let shown = names(vec![
        entry(shell, None, AgentState::Unknown),
        named(w1, "w1", Some(shell)),
    ]);
    assert_eq!(shown, vec![("w1".to_owned(), 0)]);
}

#[test]
fn agents_that_name_each_other_as_parent_all_get_a_row() {
    let [a, b, c] = panes(3).try_into().unwrap();
    let shown = names(vec![
        named(a, "a", Some(b)),
        named(b, "b", Some(a)),
        named(c, "c", Some(c)),
    ]);
    assert_eq!(
        shown,
        vec![
            ("a".to_owned(), 0),
            ("b".to_owned(), 1),
            ("c".to_owned(), 0),
        ]
    );
}

#[test]
fn a_child_of_agents_that_name_each_other_stays_under_its_parent() {
    let [leaf, a, b] = panes(3).try_into().unwrap();
    let shown = names(vec![
        named(leaf, "leaf", Some(a)),
        named(a, "a", Some(b)),
        named(b, "b", Some(a)),
    ]);
    assert_eq!(
        shown,
        vec![
            ("a".to_owned(), 0),
            ("leaf".to_owned(), 1),
            ("b".to_owned(), 1),
        ]
    );
}

/// For each of `count` agents, the index of the agent it names as its
/// parent, if it names one: any agent, itself included.
fn any_parents() -> impl Strategy<Value = Vec<Option<usize>>> {
    (1usize..12).prop_flat_map(|count| prop::collection::vec(prop::option::of(0..count), count))
}

fn entries_of(ids: &[PaneId], parents: &[Option<usize>]) -> Vec<SessionEntry> {
    parents
        .iter()
        .enumerate()
        .map(|(index, parent)| named(ids[index], &index.to_string(), parent.map(|p| ids[p])))
        .collect()
}

proptest! {
    #[test]
    fn every_agent_has_one_row_whatever_the_parents(parents in any_parents()) {
        let ids = panes(parents.len());
        let rows = session_rows(entries_of(&ids, &parents));
        let mut shown: Vec<PaneId> = rows.iter().map(|row| row.pane).collect();
        shown.sort();
        prop_assert_eq!(shown, ids);
    }

    #[test]
    fn a_row_that_is_stepped_in_follows_its_parents_rows(parents in any_parents()) {
        let ids = panes(parents.len());
        let rows = session_rows(entries_of(&ids, &parents));
        let position: HashMap<PaneId, usize> =
            rows.iter().enumerate().map(|(at, row)| (row.pane, at)).collect();
        for (at, row) in rows.iter().enumerate() {
            if row.depth == 0 {
                continue;
            }
            // The parent is above, one step less in, and nothing between
            // the two is further out than the row: the row is in the
            // block of its parent.
            let parent = position[&row.parent.expect("a row stepped in has a parent")];
            prop_assert!(parent < at);
            prop_assert_eq!(rows[parent].depth + 1, row.depth);
            prop_assert!(rows[parent + 1..at].iter().all(|between| between.depth >= row.depth));
        }
    }

    #[test]
    fn no_child_comes_before_its_parent(
        // An agent is started by one that runs already, so in a real
        // window the parent is an older agent.
        parents in (1usize..12).prop_flat_map(|count| {
            (0..count)
                .map(|index| if index == 0 {
                    Just(None).boxed()
                } else {
                    prop::option::of(0..index).boxed()
                })
                .collect::<Vec<_>>()
        }),
        seed in any::<u64>(),
    ) {
        let ids = panes(parents.len());
        // The entries come in any order.
        let mut entries = entries_of(&ids, &parents);
        let len = entries.len();
        for index in 0..len {
            entries.swap(index, (seed as usize).wrapping_mul(index + 7) % len);
        }
        let rows = session_rows(entries);
        let position: HashMap<PaneId, usize> =
            rows.iter().enumerate().map(|(at, row)| (row.pane, at)).collect();
        for (index, parent) in parents.iter().enumerate() {
            let Some(parent) = parent else { continue };
            let row = &rows[position[&ids[index]]];
            prop_assert!(position[&ids[*parent]] < position[&ids[index]]);
            prop_assert_eq!(row.depth, rows[position[&ids[*parent]]].depth + 1);
        }
    }
}

fn row_of(agent: &'static str, state: AgentState) -> SessionRow {
    let [pane] = panes(1).try_into().unwrap();
    session_rows(vec![entry(pane, Some(agent), state)]).remove(0)
}

/// Whether the row has a field, and its buttons.
fn controls(agent: &'static str, state: AgentState) -> (bool, Vec<SessionButton>) {
    let controls = row_of(agent, state).controls();
    (controls.field, controls.buttons.to_vec())
}

#[test]
fn a_row_asking_for_permission_has_buttons_and_no_field() {
    use SessionButton::{Allow, Deny, Go};
    let permission = AgentState::Waiting(WaitReason::Permission);
    assert_eq!(
        controls("claude", permission),
        (false, vec![Allow, Deny, Go])
    );
    assert_eq!(
        controls("codex", permission),
        (false, vec![Allow, Deny, Go])
    );
}

#[test]
fn a_row_asking_to_trust_a_folder_only_goes_to_its_pane() {
    let trust = AgentState::Waiting(WaitReason::Trust);
    assert_eq!(controls("claude", trust), (false, vec![SessionButton::Go]));
    assert_eq!(controls("codex", trust), (false, vec![SessionButton::Go]));
}

#[test]
fn a_row_with_a_question_of_choices_only_goes_to_its_pane() {
    let choice = AgentState::Waiting(WaitReason::Choice);
    assert_eq!(controls("claude", choice), (false, vec![SessionButton::Go]));
}

#[test]
fn a_row_with_a_question_that_may_take_text_has_both_a_field_and_a_way_to_its_pane() {
    let question = AgentState::Waiting(WaitReason::Question);
    assert_eq!(controls("codex", question), (true, vec![SessionButton::Go]));
}

#[test]
fn a_row_at_its_prompt_has_a_field() {
    let [seen, unseen, failed] = panes(3).try_into().unwrap();
    let rows = session_rows(vec![
        entry(seen, Some("claude"), AgentState::Done),
        SessionEntry {
            unseen: true,
            ..entry(unseen, Some("codex"), AgentState::Done)
        },
        entry(failed, Some("claude"), AgentState::Error),
    ]);
    let states: Vec<_> = rows.iter().map(|row| row.state).collect();
    assert_eq!(
        states,
        vec![SessionState::Idle, SessionState::Done, SessionState::Error]
    );
    for row in &rows {
        let controls = row.controls();
        assert!(controls.field, "{:?}", row.state);
        assert!(controls.buttons.is_empty(), "{:?}", row.state);
    }
}

#[test]
fn a_row_at_work_or_starting_has_nothing_under_it() {
    let [pane] = panes(1).try_into().unwrap();
    let starting = session_rows(vec![named(pane, "w1", None)]).remove(0);
    let working = row_of("claude", AgentState::Working);
    for row in [starting, working] {
        let controls = row.controls();
        assert!(!controls.field);
        assert!(controls.buttons.is_empty());
    }
}

#[test]
fn the_answers_to_a_permission_are_single_keys_of_each_agent() {
    let permission = AgentState::Waiting(WaitReason::Permission);
    let claude = row_of("claude", permission);
    let codex = row_of("codex", permission);
    // "1. Yes" and "3. No, and tell Claude what to do differently (esc)".
    assert_eq!(
        claude.answer_key(SessionButton::Allow),
        Some(AnswerKey::Text("1"))
    );
    assert_eq!(
        claude.answer_key(SessionButton::Deny),
        Some(AnswerKey::Escape)
    );
    // "1. Yes, proceed (y)" and "No, and tell Codex what to do
    // differently (esc)".
    assert_eq!(
        codex.answer_key(SessionButton::Allow),
        Some(AnswerKey::Text("y"))
    );
    assert_eq!(
        codex.answer_key(SessionButton::Deny),
        Some(AnswerKey::Escape)
    );
    assert_eq!(claude.answer_key(SessionButton::Go), None);
}

#[test]
fn a_row_asking_to_trust_a_folder_sends_no_key() {
    let trust = AgentState::Waiting(WaitReason::Trust);
    for agent in ["claude", "codex"] {
        let row = row_of(agent, trust);
        for button in [SessionButton::Allow, SessionButton::Deny, SessionButton::Go] {
            assert_eq!(row.answer_key(button), None, "{agent} {button:?}");
        }
    }
}

#[test]
fn only_a_permission_is_answered_with_a_key() {
    for state in [
        AgentState::Waiting(WaitReason::Choice),
        AgentState::Waiting(WaitReason::Question),
        AgentState::Done,
        AgentState::Working,
    ] {
        let row = row_of("claude", state);
        assert_eq!(row.answer_key(SessionButton::Allow), None, "{state:?}");
        assert_eq!(row.answer_key(SessionButton::Deny), None, "{state:?}");
    }
}

/// A plan whose items are named by their place and have these statuses.
fn plan(statuses: &[PlanStatus]) -> Plan {
    let items = statuses.iter().enumerate().map(|(at, status)| PlanItem {
        text: format!("step {at}"),
        status: *status,
    });
    Plan {
        items: items.collect(),
    }
}

/// The row of a working agent with `plan`.
fn row_with(plan: Option<Plan>) -> SessionRow {
    let [pane] = panes(1).try_into().unwrap();
    let entry = SessionEntry {
        plan,
        ..entry(pane, Some("claude"), AgentState::Working)
    };
    session_rows(vec![entry]).remove(0)
}

/// The texts of the items a row shows of `plan`, and how many it leaves
/// out before and after them.
fn shown(statuses: &[PlanStatus]) -> (Vec<String>, usize, usize) {
    let view = row_with(Some(plan(statuses))).plan_view().unwrap();
    let texts = view.items.into_iter().map(|item| item.text).collect();
    (texts, view.before, view.after)
}

fn steps(range: std::ops::Range<usize>) -> Vec<String> {
    range.map(|at| format!("step {at}")).collect()
}

#[test]
fn a_row_says_how_much_of_the_plan_is_done_in_progress_and_left() {
    use PlanStatus::{Completed, InProgress, Pending};
    let row = row_with(Some(plan(&[Completed, InProgress, Pending, Pending])));

    let view = row.plan_view().unwrap();

    assert_eq!(view.summary, "1 done \u{b7} 1 in progress \u{b7} 2 left");
    assert_eq!(
        view.items,
        plan(&[Completed, InProgress, Pending, Pending]).items
    );
    assert_eq!((view.before, view.after), (0, 0));
}

#[test]
fn a_row_without_a_plan_or_with_an_empty_one_shows_none() {
    assert_eq!(row_with(None).plan_view(), None);
    assert_eq!(row_with(Some(Plan::default())).plan_view(), None);
}

#[test]
fn a_long_plan_is_shown_around_the_item_in_progress() {
    use PlanStatus::{Completed, InProgress, Pending};
    let mut statuses = [Pending; 9];
    statuses[..3].fill(Completed);
    statuses[3] = InProgress;

    // The item before the one in progress comes first.
    assert_eq!(shown(&statuses), (steps(2..7), 2, 2));

    // Near the end, the row is still full.
    statuses[..7].fill(Completed);
    statuses[7] = InProgress;
    assert_eq!(shown(&statuses), (steps(4..9), 4, 0));

    // With nothing in progress, the first item left to do is the one.
    statuses[7] = Pending;
    assert_eq!(shown(&statuses), (steps(4..9), 4, 0));
    assert_eq!(shown(&[Pending; 9]), (steps(0..5), 0, 4));

    // A plan all done shows its end.
    assert_eq!(shown(&[Completed; 9]), (steps(4..9), 4, 0));
}

proptest! {
    /// Whatever the plan, the row shows a run of its items no longer
    /// than it has room for, with the item in progress among them, and
    /// counts every other item as before or after.
    #[test]
    fn the_items_shown_are_a_run_of_the_plan_with_the_one_in_progress(
        statuses in prop::collection::vec(
            prop_oneof![
                Just(PlanStatus::Pending),
                Just(PlanStatus::InProgress),
                Just(PlanStatus::Completed),
            ],
            1..20,
        ),
    ) {
        let plan = plan(&statuses);
        let view = row_with(Some(plan.clone())).plan_view().unwrap();

        prop_assert_eq!(view.before + view.items.len() + view.after, plan.items.len());
        prop_assert_eq!(view.items.len(), plan.items.len().min(PLAN_LINES));
        prop_assert_eq!(&view.items[..], &plan.items[view.before..][..view.items.len()]);
        if let Some(first) = statuses.iter().position(|s| *s == PlanStatus::InProgress) {
            prop_assert!((view.before..view.before + view.items.len()).contains(&first));
        }
    }
}

#[test]
fn a_row_has_the_status_of_the_last_report_and_problems_stand_out() {
    let [pass, issues, blocked, none] = panes(4).try_into().unwrap();
    let reported = |pane, status| SessionEntry {
        report_status: status,
        ..named(pane, &format!("w{}", pane.get()), None)
    };
    let rows = session_rows(vec![
        reported(pass, Some(ReportStatus::Pass)),
        reported(issues, Some(ReportStatus::Issues)),
        reported(blocked, Some(ReportStatus::Blocked)),
        reported(none, None),
    ]);

    let marks: Vec<_> = rows.iter().map(SessionRow::report_mark).collect();
    assert_eq!(
        marks,
        vec![
            Some(ReportMark {
                label: "Pass",
                stands_out: false
            }),
            Some(ReportMark {
                label: "Issues",
                stands_out: true
            }),
            Some(ReportMark {
                label: "Blocked",
                stands_out: true
            }),
            None,
        ]
    );
    assert_eq!(rows[2].report_status, Some(ReportStatus::Blocked));
}

/// Whether the row of an agent recognised as `agent` in `state` shows
/// its field, unchosen and chosen.
fn field_shown(agent: &'static str, state: AgentState, unseen: bool) -> (bool, bool) {
    let [pane] = panes(1).try_into().unwrap();
    let row = session_rows(vec![SessionEntry {
        unseen,
        ..entry(pane, Some(agent), state)
    }])
    .remove(0);
    (row.shows_field(false), row.shows_field(true))
}

#[test]
fn a_row_that_needs_an_answer_always_shows_its_field() {
    let question = AgentState::Waiting(WaitReason::Question);
    assert_eq!(field_shown("codex", question, false), (true, true));
    // A turn that ended or failed out of view.
    assert_eq!(field_shown("claude", AgentState::Done, true), (true, true));
    assert_eq!(field_shown("claude", AgentState::Error, true), (true, true));
}

#[test]
fn a_row_at_its_prompt_that_was_seen_shows_its_field_only_when_chosen() {
    // Idle.
    assert_eq!(
        field_shown("claude", AgentState::Done, false),
        (false, true)
    );
    // A failed turn that was looked at.
    assert_eq!(
        field_shown("codex", AgentState::Error, false),
        (false, true)
    );
}

#[test]
fn a_row_that_takes_no_reply_shows_no_field_even_when_chosen() {
    for state in [
        AgentState::Working,
        AgentState::Waiting(WaitReason::Permission),
        AgentState::Waiting(WaitReason::Trust),
        AgentState::Waiting(WaitReason::Choice),
    ] {
        assert_eq!(
            field_shown("claude", state, true),
            (false, false),
            "{state:?}"
        );
    }
    // The buttons of a permission stay, chosen or not.
    let row = row_of("claude", AgentState::Waiting(WaitReason::Permission));
    assert_eq!(row.controls().buttons.len(), 3);
}

/// A made-up checked report under `title` that asks `questions`, with
/// `problems` left if it ran out of rounds.
fn checked(title: &str, conclusion: &str, questions: &[&str], problems: &[&str]) -> CheckedReport {
    let decisions = questions
        .iter()
        .map(|question| Decision {
            question: (*question).into(),
            options: vec![Choice {
                label: "Yes".into(),
                effect: "It is done.".into(),
            }],
            recommend: "Yes".into(),
            reason: "It is cheap.".into(),
        })
        .collect();
    CheckedReport {
        agent: "claude".into(),
        session_id: None,
        report: Report {
            title: title.into(),
            goal: "To have it".into(),
            conclusion: conclusion.into(),
            summary: "It is there.".into(),
            decisions,
            verified: vec![],
            unverified: vec![],
            deliverables: vec![],
        },
        problems: problems.iter().map(|problem| (*problem).into()).collect(),
    }
}

fn with_checked(pane: PaneId, checked: Option<CheckedReport>) -> SessionEntry {
    SessionEntry {
        checked,
        ..named(pane, &format!("w{}", pane.get()), None)
    }
}

#[test]
fn the_row_of_a_checked_report_asks_its_first_question() {
    let [asks, tells, none] = panes(3).try_into().unwrap();
    let rows = session_rows(vec![
        with_checked(
            asks,
            Some(checked("Work", "pass", &["Keep it?", "Ship it?"], &[])),
        ),
        with_checked(tells, Some(checked("Other work", "pass", &[], &[]))),
        with_checked(none, None),
    ]);

    assert_eq!(
        rows[0].report_line(),
        Some(ReportLine {
            text: "Keep it?".into(),
            asks: true,
            more: 1,
            stopped: false,
        })
    );
    // A report that asks nothing is opened by its title.
    assert_eq!(
        rows[1].report_line(),
        Some(ReportLine {
            text: "Other work".into(),
            asks: false,
            more: 0,
            stopped: false,
        })
    );
    assert_eq!(rows[2].report_line(), None, "no report, no line");
}

#[test]
fn the_line_of_a_report_that_ran_out_of_rounds_says_it_stopped() {
    let [pane] = panes(1).try_into().unwrap();
    let report = checked("Work", "", &["Keep it?"], &["title is too plain."]);
    let row = session_rows(vec![with_checked(pane, Some(report))]).remove(0);

    let line = row.report_line().unwrap();
    assert!(line.stopped);
    assert_eq!(line.text, "Keep it?");
}

#[test]
fn the_line_of_a_report_is_one_line_of_its_text() {
    let [pane] = panes(1).try_into().unwrap();
    let report = checked("Work", "pass", &["Keep\n the \u{1b}old\tone?  "], &[]);
    let row = session_rows(vec![with_checked(pane, Some(report))]).remove(0);

    assert_eq!(row.report_line().unwrap().text, "Keep the old one?");
}

/// The badge of a row and the panel it opens say the same: the
/// conclusion of the checked report, when it has one, and otherwise the
/// status of the last report to the parent.
#[test]
fn the_badge_of_a_row_follows_the_checked_report_before_the_report_to_the_parent() {
    let [both, stopped, parent_only] = panes(3).try_into().unwrap();
    let entry = |pane, report, status| SessionEntry {
        report_status: status,
        ..with_checked(pane, report)
    };
    let rows = session_rows(vec![
        entry(
            both,
            Some(checked("Work", "blocked", &[], &[])),
            Some(ReportStatus::Pass),
        ),
        entry(
            stopped,
            Some(checked("Work", "not a word", &[], &["no conclusion."])),
            Some(ReportStatus::Issues),
        ),
        entry(parent_only, None, Some(ReportStatus::Pass)),
    ]);

    let labels: Vec<_> = rows
        .iter()
        .map(|row| row.report_mark().map(|mark| mark.label))
        .collect();
    assert_eq!(labels, vec![Some("Blocked"), Some("Issues"), Some("Pass")]);
}

#[test]
fn a_line_of_the_agents_text_has_no_breaks_and_no_control_characters() {
    assert_eq!(shown_line("  a\r\nb\u{7}\u{1b}[31mc  "), "a b[31mc");
    assert_eq!(shown_line("\n\t"), "");
}

#[test]
fn a_paragraph_of_the_agents_text_keeps_its_breaks_and_drops_control_characters() {
    assert_eq!(shown_text("a\r\nb\u{1b}c\t d \n\n"), "a\nbc d");
}

/// The kind of an agent is said by an icon, so the first line of its row
/// is free for what tells it from the others of its kind.
#[test]
fn a_row_is_headed_by_what_tells_it_apart_and_its_kind_is_an_icon() {
    use benterm_ui::session_rows::AgentIcon;
    let [claude, codex, other, plain] = panes(4).try_into().unwrap();
    let named = |pane, agent, name: &str| SessionEntry {
        name: Some(name.to_owned()),
        ..entry(pane, agent, AgentState::Working)
    };
    let rows = session_rows(vec![
        named(claude, Some("claude"), "reviewer"),
        SessionEntry {
            directory: Some("/src/benterm".into()),
            ..entry(codex, Some("codex"), AgentState::Working)
        },
        named(other, None, "w1"),
        entry(plain, Some("claude"), AgentState::Working),
    ]);
    let shown: Vec<_> = rows.iter().map(|row| (row.title(), row.icon())).collect();
    assert_eq!(
        shown,
        vec![
            ("reviewer".to_owned(), AgentIcon::ClaudeCode),
            ("benterm".to_owned(), AgentIcon::Codex),
            ("w1".to_owned(), AgentIcon::Agent),
            // With nothing to tell it apart, the kind is its title.
            ("Claude Code".to_owned(), AgentIcon::ClaudeCode),
        ]
    );
}

#[test]
fn a_row_shows_sanitized_msg_only_when_waiting_or_error() {
    let [waiting, err, working, done, idle, starting] = panes(6).try_into().unwrap();
    let with_msg = |pane, agent, state, unseen, msg: &str| SessionEntry {
        unseen,
        msg: Some(msg.to_owned()),
        ..entry(pane, agent, state)
    };
    let rows = session_rows(vec![
        with_msg(
            waiting,
            Some("claude"),
            AgentState::Waiting(WaitReason::Permission),
            false,
            "Allow Bash: cargo test",
        ),
        with_msg(
            err,
            Some("claude"),
            AgentState::Error,
            false,
            "  Failed\u{1b}[31m:\n\tbad\u{07} token  ",
        ),
        with_msg(
            working,
            Some("claude"),
            AgentState::Working,
            false,
            "Running tests",
        ),
        with_msg(done, Some("claude"), AgentState::Done, true, "All done"),
        with_msg(idle, Some("claude"), AgentState::Done, false, "Idle msg"),
        SessionEntry {
            msg: Some("Booting".into()),
            ..named(starting, "w1", None)
        },
    ]);

    assert_eq!(rows[0].msg.as_deref(), Some("Allow Bash: cargo test"));
    assert_eq!(rows[1].msg.as_deref(), Some("Failed[31m: bad token"));
    assert_eq!(rows[2].msg, None);
    assert_eq!(rows[3].msg, None);
    assert_eq!(rows[4].msg, None);
    assert_eq!(rows[5].msg, None);

    // Control-only or blank msg is omitted.
    let [blank] = panes(1).try_into().unwrap();
    let rows = session_rows(vec![with_msg(
        blank,
        Some("claude"),
        AgentState::Waiting(WaitReason::Question),
        false,
        "\u{07}\u{1b}\n\t  ",
    )]);
    assert_eq!(rows[0].msg, None);
}

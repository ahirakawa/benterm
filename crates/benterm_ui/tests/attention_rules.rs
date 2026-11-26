//! The rules of the attention marks, without a window: which events mark
//! a pane, when a notification is posted, and when a mark goes.

use std::time::{Duration, SystemTime};

use benterm_agent::WaitReason;
use benterm_layout::{PaneId, PaneTree, Size};
use benterm_ui::attention::{Attention, AttentionEvent, AttentionKind, REPEAT_WINDOW};
use proptest::prelude::*;

/// `count` ids no other pane has.
fn panes(count: usize) -> Vec<PaneId> {
    (0..count)
        .map(|_| PaneTree::new(Size::new(1, 1)).focused())
        .collect()
}

fn done(pane: PaneId, at: SystemTime) -> AttentionEvent {
    AttentionEvent {
        pane,
        kind: AttentionKind::Done {
            agent: Some("claude"),
        },
        at,
    }
}

fn osc(pane: PaneId, at: SystemTime) -> AttentionEvent {
    AttentionEvent {
        pane,
        kind: AttentionKind::Osc {
            title: None,
            body: "build done".into(),
        },
        at,
    }
}

#[test]
fn an_event_of_a_pane_not_in_view_marks_it_and_asks_for_a_notification() {
    let [shown, behind] = panes(2).try_into().unwrap();
    let mut attention = Attention::default();
    attention.set_viewing(Some(shown));
    assert!(attention.take(&done(behind, SystemTime::now())));
    assert!(attention.is_marked(behind));
    assert!(!attention.is_marked(shown));
}

#[test]
fn an_event_of_the_pane_in_view_neither_marks_nor_notifies() {
    let [shown] = panes(1).try_into().unwrap();
    let mut attention = Attention::default();
    attention.set_viewing(Some(shown));
    assert!(!attention.take(&done(shown, SystemTime::now())));
    assert!(!attention.is_marked(shown));
}

#[test]
fn with_the_window_in_the_back_every_pane_is_out_of_view() {
    let [pane] = panes(1).try_into().unwrap();
    let mut attention = Attention::default();
    attention.set_viewing(None);
    let waiting = AttentionEvent {
        pane,
        kind: AttentionKind::Waiting {
            agent: Some("codex"),
            reason: WaitReason::Permission,
        },
        at: SystemTime::now(),
    };
    assert!(attention.take(&waiting));
    assert!(attention.is_marked(pane));
}

#[test]
fn viewing_a_pane_takes_its_mark_away_and_only_its_own() {
    let [first, second] = panes(2).try_into().unwrap();
    let mut attention = Attention::default();
    let now = SystemTime::now();
    attention.take(&done(first, now));
    attention.take(&done(second, now));
    assert!(attention.set_viewing(Some(first)));
    assert!(!attention.is_marked(first));
    assert!(attention.is_marked(second));
    // Nothing more goes when the same pane stays in view.
    assert!(!attention.set_viewing(Some(first)));
}

#[test]
fn a_second_event_of_a_pane_soon_after_the_first_marks_without_a_second_notification() {
    // Claude Code can send its own OSC 9 right after the hook said done.
    let [pane] = panes(1).try_into().unwrap();
    let mut attention = Attention::default();
    let now = SystemTime::now();
    assert!(attention.take(&done(pane, now)));
    assert!(!attention.take(&osc(pane, now + Duration::from_millis(300))));
    assert!(attention.is_marked(pane));
    // Later on the pane notifies again.
    assert!(attention.take(&osc(pane, now + REPEAT_WINDOW * 2)));
}

#[test]
fn events_of_other_panes_notify_each() {
    let [first, second] = panes(2).try_into().unwrap();
    let mut attention = Attention::default();
    let now = SystemTime::now();
    assert!(attention.take(&done(first, now)));
    assert!(attention.take(&done(second, now)));
}

#[test]
fn a_closed_pane_is_forgotten() {
    let [pane] = panes(1).try_into().unwrap();
    let mut attention = Attention::default();
    attention.take(&done(pane, SystemTime::now()));
    assert!(attention.forget(pane));
    assert!(!attention.is_marked(pane));
}

#[derive(Debug, Clone)]
enum Step {
    /// An event of the pane at this index, this many milliseconds after
    /// the step before.
    Event(usize, u64),
    /// The user looks at the pane at this index, or at none, as when the
    /// window goes to the back.
    View(Option<usize>),
}

const PANES: usize = 4;

fn step() -> impl Strategy<Value = Step> {
    prop_oneof![
        (0..PANES, 0..5_000u64).prop_map(|(pane, after)| Step::Event(pane, after)),
        proptest::option::of(0..PANES).prop_map(Step::View),
    ]
}

proptest! {
    #[test]
    fn the_pane_in_view_has_no_mark_and_the_others_keep_theirs_until_viewed(
        steps in proptest::collection::vec(step(), 0..60),
    ) {
        let ids = panes(PANES);
        let mut attention = Attention::default();
        let mut now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        let mut viewing: Option<PaneId> = None;
        for step in steps {
            let before: Vec<bool> = ids.iter().map(|id| attention.is_marked(*id)).collect();
            match step {
                Step::Event(pane, after) => {
                    now += Duration::from_millis(after);
                    attention.take(&done(ids[pane], now));
                    // A pane out of view is marked by its event.
                    if viewing != Some(ids[pane]) {
                        prop_assert!(attention.is_marked(ids[pane]));
                    }
                }
                Step::View(pane) => {
                    viewing = pane.map(|pane| ids[pane]);
                    attention.set_viewing(viewing);
                }
            }
            prop_assert_eq!(attention.viewing(), viewing);
            for (index, id) in ids.iter().enumerate() {
                if Some(*id) == viewing {
                    prop_assert!(!attention.is_marked(*id), "the pane in view is marked");
                } else if before[index] {
                    prop_assert!(attention.is_marked(*id), "a mark went without a look");
                }
            }
        }
    }
}

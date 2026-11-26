//! Which evidence decides when the screen and the hooks disagree, and
//! whose hooks count.
//!
//! The rule: a hook of the session that owns the pane sets its state at
//! once; a screen sets its state after showing it for the settle time
//! with no contrary screen and no accepted hook in between. So a hook is
//! never undone by the frame it overtook, and a hook that never came, as
//! when the user interrupts a turn, is made up for by the screen.
//! `stability.rs` checks the screen's half of that; this file checks the
//! hooks' half and that the screen has the last word.

mod common;

use std::time::{Duration, Instant};

use benterm_agent::{AgentState, Detector, HookEvent, WaitReason};
use benterm_core::Snapshot;
use common::{definitions, screen_of};
use proptest::prelude::*;

const SETTLE: Duration = Detector::DEFAULT_SETTLE;
const WAITING: AgentState = AgentState::Waiting(WaitReason::Question);

/// What a hook says, for the made-up agent of `common`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Says {
    State(AgentState),
    Gone,
    /// An event no rule matches.
    Nothing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Input {
    /// A screen showing this state, or no signal at all for `None`.
    Screen(Option<AgentState>),
    Hook {
        says: Says,
        /// 0 is a hook that names no session.
        session: u8,
        subagent: bool,
    },
}

fn screen(state: Option<AgentState>) -> Snapshot {
    match state {
        Some(AgentState::Working) => screen_of(&["thinking"]),
        Some(AgentState::Waiting(_)) => screen_of(&["allow? [y/n]"]),
        // A failed turn leaves the prompt on the screen too.
        Some(AgentState::Done | AgentState::Error) => screen_of(&["toy>"]),
        Some(AgentState::Unknown) | None => screen_of(&["some output"]),
    }
}

fn session_name(session: u8) -> Option<String> {
    (session != 0).then(|| format!("session-{session}"))
}

fn event(says: Says, session: u8, subagent: bool) -> HookEvent {
    let name = match says {
        Says::State(AgentState::Working) => "start",
        Says::State(AgentState::Waiting(_)) => "ask",
        Says::State(AgentState::Error) => "fail",
        Says::State(_) => "end",
        Says::Gone => "bye",
        Says::Nothing => "something-else",
    };
    let mut event = HookEvent::new("toy", name);
    if let Some(session) = session_name(session) {
        event = event.with_session(session);
    }
    if subagent {
        event = event.from_subagent();
    }
    event
}

fn shown_state() -> impl Strategy<Value = AgentState> {
    prop::sample::select(vec![
        AgentState::Working,
        AgentState::Waiting(WaitReason::Permission),
        AgentState::Done,
    ])
}

fn says() -> impl Strategy<Value = Says> {
    prop_oneof![
        3 => Just(Says::State(AgentState::Working)),
        2 => Just(Says::State(WAITING)),
        3 => Just(Says::State(AgentState::Done)),
        1 => Just(Says::State(AgentState::Error)),
        1 => Just(Says::Gone),
        1 => Just(Says::Nothing),
    ]
}

fn input() -> impl Strategy<Value = Input> {
    prop_oneof![
        3 => shown_state().prop_map(|state| Input::Screen(Some(state))),
        1 => Just(Input::Screen(None)),
        4 => (says(), 0u8..3, prop::bool::weighted(0.15)).prop_map(
            |(says, session, subagent)| Input::Hook { says, session, subagent }
        ),
    ]
}

/// Steps of (milliseconds since the previous step, input).
fn steps() -> impl Strategy<Value = Vec<(u64, Input)>> {
    prop::collection::vec((0u64..400, input()), 1..120)
}

fn in_a_turn(state: AgentState) -> bool {
    matches!(state, AgentState::Working | AgentState::Waiting(_))
}

proptest! {
    /// A hook is judged by who sent it and by nothing the screen shows:
    /// the owner's hook sets its state at once, and a hook that is not
    /// the owner's changes nothing in the middle of a turn and never ends
    /// the session.
    #[test]
    fn a_hook_counts_only_from_the_session_that_owns_the_pane(steps in steps()) {
        let mut detector = Detector::new(definitions());
        let mut now = Instant::now();
        // The session of the hook that was accepted last; `None` when
        // that hook named no session, and before any hook.
        let mut owner: Option<String> = None;
        for (ms, input) in steps {
            now += Duration::from_millis(ms);
            let before = detector.state();
            let known = detector.agent().is_some();
            match input {
                Input::Screen(state) => {
                    detector.observe_screen(&screen(state), now);
                    // Only hooks name the made-up agent here, so a screen
                    // never changes who is known.
                    prop_assert_eq!(detector.agent().is_some(), known);
                }
                Input::Hook { says, session, subagent } => {
                    detector.observe_hook(&event(says, session, subagent), now);
                    let session = session_name(session);
                    let strange = owner.is_some() && owner != session;
                    let ignored = subagent
                        || (strange && in_a_turn(before))
                        || (says == Says::Gone && owner != session);
                    if ignored {
                        prop_assert_eq!(detector.state(), before);
                        prop_assert_eq!(detector.agent().is_some(), known);
                        continue;
                    }
                    match says {
                        Says::State(state) => {
                            prop_assert_eq!(detector.state(), state);
                            prop_assert!(detector.agent().is_some());
                            owner = session;
                        }
                        Says::Nothing => {
                            // It names the agent when none is known and
                            // does nothing else: it takes the pane from
                            // nobody.
                            prop_assert_eq!(detector.state(), before);
                            prop_assert!(detector.agent().is_some());
                            if !known {
                                owner = session;
                            }
                        }
                        Says::Gone => {
                            prop_assert_eq!(detector.state(), AgentState::Unknown);
                            prop_assert!(detector.agent().is_none());
                            owner = None;
                        }
                    }
                }
            }
        }
    }

    /// After a hook changed the state, no screen changes it again before
    /// the settle time has passed since that hook.
    #[test]
    fn a_screen_does_not_undo_a_hook_within_the_settle_time(steps in steps()) {
        let mut detector = Detector::new(definitions());
        let mut now = Instant::now();
        let mut hooked: Option<Instant> = None;
        for (ms, input) in steps {
            now += Duration::from_millis(ms);
            let before = detector.state();
            match input {
                Input::Screen(state) => {
                    detector.observe_screen(&screen(state), now);
                    if detector.state() != before && let Some(hooked) = hooked {
                        prop_assert!(
                            now - hooked >= SETTLE,
                            "a screen changed {before:?} {:?} after a hook",
                            now - hooked
                        );
                    }
                }
                Input::Hook { says, session, subagent } => {
                    detector.observe_hook(&event(says, session, subagent), now);
                    if detector.state() != before {
                        hooked = Some(now);
                    }
                }
            }
        }
    }

    /// Whatever the hooks said and whoever sent them, a screen that shows
    /// one state for the settle time has the last word, as long as the
    /// agent is still known. The one exception is the prompt after a
    /// turn that failed, which is the prompt of a turn that went well:
    /// it leaves the error.
    #[test]
    fn a_steady_screen_wins_over_any_hooks_in_the_end(
        steps in steps(),
        last in shown_state(),
    ) {
        let mut detector = Detector::new(definitions());
        let mut now = Instant::now();
        // The last screen showed a state, so a pending state is the one
        // it showed and is still on the screen.
        let mut showed_a_state = false;
        for (ms, input) in steps {
            now += Duration::from_millis(ms);
            match input {
                Input::Screen(state) => {
                    detector.observe_screen(&screen(state), now);
                    showed_a_state = state.is_some();
                }
                Input::Hook { says, session, subagent } => {
                    detector.observe_hook(&event(says, session, subagent), now);
                }
            }
        }
        prop_assume!(detector.agent().is_some());
        now += Duration::from_millis(1);
        // The state changes only when the detector looks, so a state the
        // screen kept showing past its time has taken effect by now,
        // though the detector does not say so yet. A turn the screen
        // showed after an error ends it, and the prompt after that turn
        // is a prompt like any other.
        let overdue = showed_a_state && detector.pending_deadline().is_some_and(|due| due < now);
        let failed = detector.state() == AgentState::Error && !overdue;
        let last = if failed && last == AgentState::Done { AgentState::Error } else { last };
        detector.observe_screen(&screen(Some(last)), now);
        // When the state is still to change, the detector says by when
        // to look again.
        prop_assert!(
            detector.state() == last
                || detector.pending_deadline().is_some_and(|due| due <= now + SETTLE)
        );
        detector.observe_screen(&screen(Some(last)), now + SETTLE);
        prop_assert_eq!(detector.state(), last);
    }
}

/// A screen that showed a new turn past the settle time took effect
/// then, though nobody looked until the next screen came: the prompt that
/// came next ends that turn, and with it the error of the turn before.
/// The hooks in between change nothing, for one tells no state and the
/// other is not the owner's.
#[test]
fn a_turn_the_screen_showed_after_an_error_ends_at_the_prompt() {
    let mut detector = Detector::new(definitions());
    let start = Instant::now();
    let at = |ms| start + Duration::from_millis(ms);
    detector.observe_screen(&screen(Some(AgentState::Working)), at(0));
    detector.observe_hook(&event(Says::State(AgentState::Error), 0, false), at(0));
    detector.observe_screen(&screen(Some(AgentState::Working)), at(0));
    detector.observe_hook(&event(Says::Nothing, 0, false), at(166));
    detector.observe_hook(&event(Says::Gone, 1, false), at(500));
    assert_eq!(detector.state(), AgentState::Error);

    detector.observe_screen(&screen(Some(AgentState::Done)), at(501));
    assert_eq!(detector.state(), AgentState::Working);
    assert_eq!(detector.pending_deadline(), Some(at(501) + SETTLE));
    detector.observe_screen(&screen(Some(AgentState::Done)), at(501) + SETTLE);
    assert_eq!(detector.state(), AgentState::Done);
}

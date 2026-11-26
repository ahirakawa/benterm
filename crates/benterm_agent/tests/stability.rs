//! The state does not flip back and forth when its evidence is noisy.

mod common;

use std::time::{Duration, Instant};

use benterm_agent::{AgentState, Detector, HookEvent, WaitReason};
use benterm_core::Snapshot;
use common::{definitions, screen_of};
use proptest::prelude::*;

const SETTLE: Duration = Detector::DEFAULT_SETTLE;

/// What one observation shows, for the made-up agent of `common`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Input {
    /// A screen showing this state, or no signal at all for `None`.
    Screen(Option<AgentState>),
    Hook(AgentState),
}

const STATES: [AgentState; 3] = [
    AgentState::Working,
    AgentState::Waiting(WaitReason::Permission),
    AgentState::Done,
];

struct Screens {
    working: Snapshot,
    waiting: Snapshot,
    done: Snapshot,
    blank: Snapshot,
}

impl Screens {
    fn new() -> Self {
        Self {
            working: screen_of(&["thinking"]),
            waiting: screen_of(&["allow? [y/n]"]),
            done: screen_of(&["toy>"]),
            blank: screen_of(&["some output"]),
        }
    }

    fn get(&self, state: Option<AgentState>) -> &Snapshot {
        match state {
            Some(AgentState::Working) => &self.working,
            Some(AgentState::Waiting(_)) => &self.waiting,
            // A failed turn leaves the prompt on the screen too.
            Some(AgentState::Done | AgentState::Error) => &self.done,
            Some(AgentState::Unknown) | None => &self.blank,
        }
    }
}

fn hook_for(state: AgentState) -> HookEvent {
    let name = match state {
        AgentState::Working => "start",
        AgentState::Waiting(_) => "ask",
        _ => "end",
    };
    HookEvent::new("toy", name)
}

fn hook_state(state: AgentState) -> AgentState {
    match state {
        AgentState::Waiting(_) => AgentState::Waiting(WaitReason::Question),
        other => other,
    }
}

fn detector() -> Detector {
    let mut detector = Detector::new(definitions());
    detector.identify_command("toy");
    detector
}

fn state() -> impl Strategy<Value = AgentState> {
    prop::sample::select(STATES.to_vec())
}

fn screen_input() -> impl Strategy<Value = Input> {
    prop_oneof![
        3 => state().prop_map(|state| Input::Screen(Some(state))),
        1 => Just(Input::Screen(None)),
    ]
}

fn any_input() -> impl Strategy<Value = Input> {
    prop_oneof![
        6 => screen_input(),
        1 => state().prop_map(Input::Hook),
    ]
}

/// Steps of (milliseconds since the previous step, input).
fn steps(input: impl Strategy<Value = Input>) -> impl Strategy<Value = Vec<(u64, Input)>> {
    prop::collection::vec((0u64..400, input), 1..120)
}

/// Runs the steps and returns, for each step, its time and the state after
/// it.
fn run(steps: &[(u64, Input)]) -> Vec<(Duration, AgentState)> {
    let screens = Screens::new();
    let mut detector = detector();
    let start = Instant::now();
    let mut elapsed = Duration::ZERO;
    let mut out = Vec::new();
    for (ms, input) in steps {
        elapsed += Duration::from_millis(*ms);
        match input {
            Input::Screen(state) => detector.observe_screen(screens.get(*state), start + elapsed),
            Input::Hook(state) => detector.observe_hook(&hook_for(*state), start + elapsed),
        }
        out.push((elapsed, detector.state()));
    }
    out
}

proptest! {
    /// A change made by a screen needs the new state to have been shown,
    /// with no contrary screen and no hook event in between, for at least
    /// the settle time: by that screen, or by the screens before it until
    /// it came, when the look at the time the state was due came late.
    #[test]
    fn a_screen_changes_the_state_only_after_showing_it_for_the_settle_time(
        steps in steps(any_input()),
    ) {
        let states = run(&steps);
        let mut previous = AgentState::Unknown;
        for (k, (time, now_state)) in states.iter().enumerate() {
            if *now_state != previous && matches!(steps[k].1, Input::Screen(_)) {
                let shown = Input::Screen(Some(*now_state));
                // The screens up to the one that shows the state.
                let last = if steps[k].1 == shown { k } else { k - 1 };
                prop_assert_eq!(steps[last].1, shown);
                // Walk back over the run of observations that agree.
                let mut earliest = states[last].0;
                for j in (0..last).rev() {
                    match steps[j].1 {
                        Input::Screen(None) => continue,
                        Input::Screen(Some(state)) if state == *now_state => {
                            earliest = states[j].0;
                        }
                        _ => break,
                    }
                }
                prop_assert!(
                    *time - earliest >= SETTLE,
                    "changed to {:?} at {:?} after showing it only since {:?}",
                    now_state, time, earliest
                );
            }
            previous = *now_state;
        }
    }

    #[test]
    fn a_hook_event_sets_its_state_at_once(steps in steps(any_input())) {
        let states = run(&steps);
        for (k, (_, state)) in states.iter().enumerate() {
            if let Input::Hook(hooked) = steps[k].1 {
                prop_assert_eq!(*state, hook_state(hooked));
            }
        }
    }

    /// With screens only, the state changes at most once per settle time.
    #[test]
    fn screens_change_the_state_at_most_once_per_settle_time(
        steps in steps(screen_input()),
    ) {
        let states = run(&steps);
        let changes = states
            .windows(2)
            .filter(|pair| pair[0].1 != pair[1].1)
            .count()
            + usize::from(states[0].1 != AgentState::Unknown);
        let elapsed = states.last().unwrap().0;
        let allowed = 1 + (elapsed.as_millis() / SETTLE.as_millis()) as usize;
        prop_assert!(changes <= allowed, "{} changes in {:?}", changes, elapsed);
    }

    /// Short bursts of other states between long stretches of one state do
    /// not move the state away from it.
    #[test]
    fn short_glitches_do_not_move_a_settled_state(
        base in state(),
        bursts in prop::collection::vec(
            (
                prop::collection::vec((0u64..100, state(), any::<bool>()), 1..6),
                0u64..2000,
            ),
            1..20,
        ),
    ) {
        let screens = Screens::new();
        let mut detector = detector();
        let mut now = Instant::now();
        detector.observe_screen(screens.get(Some(base)), now);
        now += SETTLE;
        detector.observe_screen(screens.get(Some(base)), now);
        prop_assert_eq!(detector.state(), base);

        for (burst, calm) in bursts {
            let burst_start = now;
            for (ms, glitch, blank) in burst {
                // Keep every burst shorter than the settle time.
                let at = (now + Duration::from_millis(ms)).min(burst_start + SETTLE / 2);
                now = at;
                let shown = if blank { None } else { Some(glitch) };
                detector.observe_screen(screens.get(shown), now);
                prop_assert_eq!(detector.state(), base);
            }
            now += Duration::from_millis(1);
            detector.observe_screen(screens.get(Some(base)), now);
            now += Duration::from_millis(calm);
            detector.observe_screen(screens.get(Some(base)), now);
            prop_assert_eq!(detector.state(), base);
        }
    }

    /// Whatever came before, a screen that shows one state for the settle
    /// time brings the detector to that state.
    #[test]
    fn a_steady_screen_always_wins_in_the_end(
        steps in steps(any_input()),
        last in state(),
    ) {
        let screens = Screens::new();
        let mut detector = detector();
        let start = Instant::now();
        let mut now = start;
        for (ms, input) in &steps {
            now += Duration::from_millis(*ms);
            match input {
                Input::Screen(state) => detector.observe_screen(screens.get(*state), now),
                Input::Hook(state) => detector.observe_hook(&hook_for(*state), now),
            }
        }
        now += Duration::from_millis(1);
        detector.observe_screen(screens.get(Some(last)), now);
        detector.observe_screen(screens.get(Some(last)), now + SETTLE);
        prop_assert_eq!(detector.state(), last);
    }
}

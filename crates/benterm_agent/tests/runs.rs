//! Telling one run of an agent in a pane from the next: what a run left
//! behind, its state and its session, does not pass to the run after it,
//! and a late hook of an agent that left the front does not bring it back.
//!
//! A run is told from the one before it by the process in front of the
//! terminal, when an agent is in front, and otherwise by the hook that
//! tells the start of a session. `precedence.rs` and `front.rs` check that nothing
//! else changed for an agent that stays.

mod common;

use std::time::{Duration, Instant};

use benterm_agent::{AgentState, Detector, HookEvent};
use common::{definitions, screen_of};
use proptest::prelude::*;

const SETTLE: Duration = Detector::DEFAULT_SETTLE;

fn args(line: &[&str]) -> Vec<String> {
    line.iter().map(|arg| (*arg).to_owned()).collect()
}

fn agent(detector: &Detector) -> Option<&'static str> {
    detector.agent().map(|agent| agent.id)
}

fn claude(session: &str, name: &str) -> HookEvent {
    HookEvent::new("claude", name).with_session(session)
}

fn codex(session: &str, name: &str) -> HookEvent {
    HookEvent::new("codex", name).with_session(session)
}

/// A detector whose pane ran Claude Code's session `a` until a turn of
/// it failed, under `front` when one is given.
fn after_a_failed_turn(front: Option<&[String]>) -> (Detector, Instant) {
    let mut detector = Detector::new(definitions());
    let t = Instant::now();
    if front.is_some() {
        detector.observe_front(front);
    }
    for name in ["SessionStart", "UserPromptSubmit", "StopFailure"] {
        detector.observe_hook(&claude("a", name), t);
    }
    assert_eq!(detector.state(), AgentState::Error);
    assert_eq!(detector.owner(), Some("a"));
    (detector, t)
}

#[test]
fn the_start_of_another_session_drops_what_the_run_before_it_left() {
    let (mut detector, t) = after_a_failed_turn(None);
    detector.observe_hook(&claude("b", "SessionStart"), t);
    assert_eq!(agent(&detector), Some("claude"));
    assert_eq!(detector.state(), AgentState::Unknown);
    assert_eq!(detector.owner(), Some("b"));
}

#[test]
fn under_a_wrapper_the_first_hook_of_the_next_run_drops_the_run_before_it() {
    let wrapper = args(&["/bin/sh", "./run-the-agent"]);
    // The next run tells its start.
    let (mut detector, t) = after_a_failed_turn(Some(&wrapper));
    detector.observe_front(Some(&wrapper));
    detector.observe_hook(&claude("b", "SessionStart"), t);
    assert_eq!(detector.state(), AgentState::Unknown);
    assert_eq!(detector.owner(), Some("b"));
    // Or its first hook tells a state.
    let (mut detector, t) = after_a_failed_turn(Some(&wrapper));
    detector.observe_front(Some(&wrapper));
    detector.observe_hook(&claude("b", "UserPromptSubmit"), t);
    assert_eq!(detector.state(), AgentState::Working);
    assert_eq!(detector.owner(), Some("b"));
}

#[test]
fn the_start_of_a_session_drops_a_state_the_screen_was_about_to_tell() {
    let mut detector = Detector::new(definitions());
    let t = Instant::now();
    detector.observe_hook(&claude("a", "UserPromptSubmit"), t);
    detector.observe_hook(&claude("a", "Stop"), t);
    detector.observe_screen(&screen_of(&["Claude Code", "esc to interrupt"]), t);
    assert!(detector.pending_deadline().is_some());
    detector.observe_hook(&claude("b", "SessionStart"), t);
    assert_eq!(detector.pending_deadline(), None);
    assert_eq!(detector.state(), AgentState::Unknown);
}

#[test]
fn the_start_of_the_owner_s_own_session_changes_nothing() {
    // A session starts again when its context is compacted, in a turn or
    // between two, and when it is taken up again.
    let (mut detector, t) = after_a_failed_turn(None);
    detector.observe_hook(&claude("a", "SessionStart"), t);
    assert_eq!(detector.state(), AgentState::Error);
    assert_eq!(detector.owner(), Some("a"));

    detector.observe_hook(&claude("a", "UserPromptSubmit"), t);
    detector.observe_hook(&claude("a", "SessionStart"), t);
    assert_eq!(detector.state(), AgentState::Working);
    assert_eq!(detector.owner(), Some("a"));
}

#[test]
fn the_start_of_a_nested_session_in_a_turn_changes_nothing() {
    // The agent in front runs `claude -p` as a tool.
    let mut detector = Detector::new(definitions());
    let t = Instant::now();
    detector.observe_hook(&claude("outer", "UserPromptSubmit"), t);
    for name in ["SessionStart", "UserPromptSubmit", "Stop", "SessionEnd"] {
        detector.observe_hook(&claude("nested", name), t);
        assert_eq!(detector.state(), AgentState::Working, "{name}");
        assert_eq!(detector.owner(), Some("outer"), "{name}");
    }
}

#[test]
fn a_new_process_of_the_same_agent_in_front_starts_without_the_old_state() {
    // The agent ended and was started again before the shell between the
    // two was read in front.
    let mut detector = Detector::new(definitions());
    let t = Instant::now();
    let line = args(&["claude"]);
    detector.observe_front_process(100, &line);
    for name in ["SessionStart", "UserPromptSubmit", "StopFailure"] {
        detector.observe_hook(&claude("a", name), t);
    }
    detector.observe_screen(&screen_of(&["Claude Code", "esc to interrupt"]), t);
    assert!(detector.pending_deadline().is_some());

    detector.observe_front_process(200, &line);
    assert_eq!(agent(&detector), Some("claude"));
    assert_eq!(detector.state(), AgentState::Unknown);
    assert_eq!(detector.owner(), None);
    assert_eq!(detector.pending_deadline(), None);
    // A session taken up again by the new process, as by
    // `claude --resume a`, is heard without telling its start.
    detector.observe_hook(&claude("a", "UserPromptSubmit"), t);
    assert_eq!(detector.owner(), Some("a"));
    assert_eq!(detector.state(), AgentState::Working);
}

#[test]
fn the_same_process_in_front_again_keeps_its_state() {
    let mut detector = Detector::new(definitions());
    let t = Instant::now();
    let line = args(&["claude"]);
    detector.observe_front_process(100, &line);
    detector.observe_hook(&claude("a", "UserPromptSubmit"), t);
    detector.observe_hook(&claude("a", "StopFailure"), t);
    // Read again, with a reading that fails in between.
    detector.observe_front_process(100, &line);
    detector.observe_front(None);
    detector.observe_front_process(100, &line);
    assert_eq!(detector.state(), AgentState::Error);
    assert_eq!(detector.owner(), Some("a"));
}

#[test]
fn a_late_hook_of_the_agent_that_left_does_not_bring_its_state_back() {
    let mut detector = Detector::new(definitions());
    let t = Instant::now();
    detector.observe_front_process(100, &args(&["claude"]));
    detector.observe_hook(&claude("a", "UserPromptSubmit"), t);
    // Codex comes to the front; the shell between the two was not read.
    detector.observe_front_process(200, &args(&["codex"]));
    assert_eq!(agent(&detector), Some("codex"));
    assert_eq!(detector.state(), AgentState::Unknown);

    // Before Codex has told anything.
    for name in ["Stop", "SessionEnd", "SessionStart", "UserPromptSubmit"] {
        detector.observe_hook(&claude("a", name), t);
        assert_eq!(agent(&detector), Some("codex"), "{name}");
        assert_eq!(detector.state(), AgentState::Unknown, "{name}");
        assert_eq!(detector.owner(), None, "{name}");
    }
    // Between two turns of Codex.
    detector.observe_hook(&codex("c", "UserPromptSubmit"), t);
    detector.observe_hook(&codex("c", "Stop"), t);
    for name in ["StopFailure", "SessionEnd", "SessionStart"] {
        detector.observe_hook(&claude("a", name), t);
        assert_eq!(agent(&detector), Some("codex"), "{name}");
        assert_eq!(detector.state(), AgentState::Done, "{name}");
        assert_eq!(detector.owner(), Some("c"), "{name}");
    }
}

#[test]
fn until_another_program_is_read_the_agent_last_read_in_front_decides() {
    // A reading that fails changes nothing, so it does not let the hooks
    // of another agent in either: whether one counts does not depend on
    // a reading failing under load. The price is the first hook of an
    // agent that came to the front while no reading went well.
    let mut detector = Detector::new(definitions());
    let t = Instant::now();
    detector.observe_front_process(100, &args(&["claude"]));
    detector.observe_hook(&claude("a", "StopFailure"), t);
    detector.observe_front(None);
    detector.observe_hook(&codex("b", "Interrupt"), t);
    assert_eq!(agent(&detector), Some("claude"));
    assert_eq!(detector.owner(), Some("a"));
    // Once Codex is read in front, its hooks count.
    detector.observe_front_process(200, &args(&["codex"]));
    assert_eq!(detector.state(), AgentState::Unknown);
    detector.observe_hook(&codex("b", "UserPromptSubmit"), t);
    assert_eq!(agent(&detector), Some("codex"));
    assert_eq!(detector.state(), AgentState::Working);
    assert_eq!(detector.owner(), Some("b"));
}

#[test]
fn with_an_agent_in_front_the_start_of_another_session_changes_nothing() {
    // The process in front tells its runs apart. A session that starts
    // under it between two turns is one of a nested agent, left running
    // by a tool.
    let mut detector = Detector::new(definitions());
    let t = Instant::now();
    detector.observe_front_process(100, &args(&["claude"]));
    detector.observe_hook(&claude("outer", "UserPromptSubmit"), t);
    detector.observe_hook(&claude("outer", "Stop"), t);
    for name in ["SessionStart", "SessionEnd"] {
        detector.observe_hook(&claude("nested", name), t);
        assert_eq!(agent(&detector), Some("claude"), "{name}");
        assert_eq!(detector.state(), AgentState::Done, "{name}");
        assert_eq!(detector.owner(), Some("outer"), "{name}");
    }
}

#[test]
fn a_start_that_names_no_session_changes_nothing() {
    let (mut detector, t) = after_a_failed_turn(None);
    detector.observe_hook(&HookEvent::new("claude", "SessionStart"), t);
    assert_eq!(detector.state(), AgentState::Error);
    assert_eq!(detector.owner(), Some("a"));
}

#[test]
fn under_a_wrapper_the_start_of_another_agent_drops_the_run_before_it() {
    let wrapper = args(&["/bin/sh", "./run-the-agent"]);
    let (mut detector, t) = after_a_failed_turn(Some(&wrapper));
    detector.observe_hook(&codex("b", "SessionStart"), t);
    assert_eq!(agent(&detector), Some("codex"));
    assert_eq!(detector.state(), AgentState::Unknown);
    assert_eq!(detector.owner(), Some("b"));
}

#[test]
fn the_screen_tells_the_state_of_the_new_process() {
    let mut detector = Detector::new(definitions());
    let t = Instant::now();
    let line = args(&["claude"]);
    detector.observe_front_process(100, &line);
    detector.observe_hook(&claude("a", "UserPromptSubmit"), t);
    detector.observe_hook(&claude("a", "StopFailure"), t);
    detector.observe_front_process(200, &line);
    // The prompt, which would leave the error of the run before.
    let prompt = screen_of(&["Claude Code", "❯ ", "  ? for shortcuts"]);
    detector.observe_screen(&prompt, t);
    detector.observe_screen(&prompt, t + SETTLE);
    assert_eq!(detector.state(), AgentState::Done);
}

#[derive(Debug, Clone)]
enum Input {
    Hook {
        agent: &'static str,
        name: &'static str,
        session: &'static str,
    },
    /// The agent in front is read again, or the reading fails.
    Front(bool),
}

fn input() -> impl Strategy<Value = Input> {
    let names = vec![
        "SessionStart",
        "UserPromptSubmit",
        "PermissionRequest",
        "Stop",
        "SessionEnd",
        "Notification",
    ];
    prop_oneof![
        6 => (
            prop::sample::select(vec!["claude", "codex"]),
            prop::sample::select(names),
            prop::sample::select(vec!["a", "b"]),
        )
            .prop_map(|(agent, name, session)| Input::Hook { agent, name, session }),
        1 => any::<bool>().prop_map(Input::Front),
    ]
}

proptest! {
    /// While one process of an agent stays in front, the hooks of
    /// another agent change nothing, whatever they say and whenever
    /// they come: the detector decides as one that never got them.
    #[test]
    fn hooks_of_an_agent_that_is_not_the_one_in_front_change_nothing(
        inputs in prop::collection::vec(input(), 0..60),
    ) {
        let line = args(&["codex"]);
        let mut all = Detector::new(definitions());
        let mut own = Detector::new(definitions());
        all.observe_front_process(7, &line);
        own.observe_front_process(7, &line);
        let t = Instant::now();
        for input in inputs {
            match input {
                Input::Hook { agent, name, session } => {
                    let event = HookEvent::new(agent, name).with_session(session);
                    all.observe_hook(&event, t);
                    if agent == "codex" {
                        own.observe_hook(&event, t);
                    }
                }
                Input::Front(read) => {
                    for detector in [&mut all, &mut own] {
                        if read {
                            detector.observe_front_process(7, &line);
                        } else {
                            detector.observe_front(None);
                        }
                    }
                }
            }
            prop_assert_eq!(agent(&all), agent(&own));
            prop_assert_eq!(all.state(), own.state());
            prop_assert_eq!(all.owner(), own.owner());
        }
    }
}

/// What only an agent may be given goes to a process that was read in
/// front, not to whatever a banner on the screen names.
#[test]
fn the_agent_in_front_is_a_process_that_was_read_there() {
    let in_front = |detector: &Detector| {
        let front = detector.agent_in_front();
        front.map(|(agent, pid)| (agent.id, pid))
    };
    let mut detector = Detector::new(definitions());
    let t = Instant::now();
    // The front was never read, and the screen names the agent.
    detector.observe_screen(&screen_of(&["Claude Code"]), t);
    assert_eq!(agent(&detector), Some("claude"));
    assert_eq!(in_front(&detector), None);

    detector.observe_front_process(100, &args(&["claude"]));
    assert_eq!(in_front(&detector), Some(("claude", 100)));
    detector.observe_front_process(200, &args(&["claude"]));
    assert_eq!(in_front(&detector), Some(("claude", 200)));

    // A reading without a process id tells no process.
    detector.observe_front(Some(&args(&["zsh"])));
    detector.observe_front(Some(&args(&["claude"])));
    assert_eq!(agent(&detector), Some("claude"));
    assert_eq!(in_front(&detector), None);

    // The agent gave way to a shell, and its banner is still shown.
    detector.observe_front_process(300, &args(&["claude"]));
    detector.observe_front_process(400, &args(&["zsh"]));
    detector.observe_screen(&screen_of(&["Claude Code"]), t);
    assert_eq!(in_front(&detector), None);
}

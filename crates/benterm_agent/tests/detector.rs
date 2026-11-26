//! How a detector combines screens and hook events over time.

mod common;

use std::time::{Duration, Instant};

use benterm_agent::{AgentState, Detector, HookEvent, WaitReason};
use common::{definitions, screen_of};

const SETTLE: Duration = Detector::DEFAULT_SETTLE;

fn hook(name: &str) -> HookEvent {
    HookEvent::new("toy", name)
}

#[test]
fn the_state_is_unknown_until_an_agent_is_recognised() {
    let mut detector = Detector::new(definitions());
    let t = Instant::now();
    detector.observe_screen(&screen_of(&["$ ls", "thinking"]), t);
    detector.observe_screen(&screen_of(&["$ ls", "thinking"]), t + SETTLE * 2);
    assert!(detector.agent().is_none());
    assert_eq!(detector.state(), AgentState::Unknown);
}

#[test]
fn a_new_definition_is_recognised_and_classified_without_other_changes() {
    let mut detector = Detector::new(definitions());
    let t = Instant::now();
    let screen = screen_of(&["TOY AGENT v1", "thinking"]);
    detector.observe_screen(&screen, t);
    detector.observe_screen(&screen, t + SETTLE);
    assert_eq!(detector.agent().map(|agent| agent.id), Some("toy"));
    assert_eq!(detector.state(), AgentState::Working);
}

#[test]
fn an_agent_can_be_named_by_the_command_that_started_it() {
    let mut detector = Detector::new(definitions());
    assert!(detector.identify_command("/usr/local/bin/toy"));
    assert_eq!(detector.agent().map(|agent| agent.id), Some("toy"));
    assert!(!detector.identify_command("vim"));
    assert_eq!(detector.agent().map(|agent| agent.id), Some("toy"));
}

#[test]
fn the_agent_stays_recognised_after_its_banner_scrolls_away() {
    let mut detector = Detector::new(definitions());
    let t = Instant::now();
    detector.observe_screen(&screen_of(&["TOY AGENT v1", "toy>"]), t);
    let later = screen_of(&["output", "allow? [y/n]"]);
    detector.observe_screen(&later, t + SETTLE);
    detector.observe_screen(&later, t + SETTLE * 2);
    assert_eq!(detector.agent().map(|agent| agent.id), Some("toy"));
    assert_eq!(
        detector.state(),
        AgentState::Waiting(WaitReason::Permission)
    );
}

#[test]
fn a_screen_state_takes_effect_only_after_it_has_lasted_the_settle_time() {
    let mut detector = Detector::new(definitions());
    detector.identify_command("toy");
    let t = Instant::now();
    let working = screen_of(&["thinking"]);
    detector.observe_screen(&working, t);
    assert_eq!(detector.state(), AgentState::Unknown);
    detector.observe_screen(&working, t + SETTLE - Duration::from_millis(1));
    assert_eq!(detector.state(), AgentState::Unknown);
    detector.observe_screen(&working, t + SETTLE);
    assert_eq!(detector.state(), AgentState::Working);
}

#[test]
fn a_contrary_screen_restarts_the_settle_time() {
    let mut detector = Detector::new(definitions());
    detector.identify_command("toy");
    let t = Instant::now();
    let working = screen_of(&["thinking"]);
    detector.observe_screen(&working, t);
    detector.observe_screen(&screen_of(&["toy>"]), t + SETTLE / 2);
    detector.observe_screen(&working, t + SETTLE / 2 + Duration::from_millis(1));
    detector.observe_screen(&working, t + SETTLE);
    assert_eq!(detector.state(), AgentState::Unknown);
}

#[test]
fn a_screen_without_any_signal_keeps_the_state() {
    let mut detector = Detector::new(definitions());
    detector.identify_command("toy");
    let t = Instant::now();
    detector.observe_hook(&hook("start"), t);
    let blank = screen_of(&["some output"]);
    detector.observe_screen(&blank, t + SETTLE);
    detector.observe_screen(&blank, t + SETTLE * 3);
    assert_eq!(detector.state(), AgentState::Working);
}

#[test]
fn a_hook_event_takes_effect_at_once_and_names_the_agent() {
    let mut detector = Detector::new(definitions());
    detector.observe_hook(&hook("ask"), Instant::now());
    assert_eq!(detector.agent().map(|agent| agent.id), Some("toy"));
    assert_eq!(detector.state(), AgentState::Waiting(WaitReason::Question));
}

#[test]
fn a_hook_event_of_an_unknown_agent_or_kind_is_ignored() {
    let mut detector = Detector::new(definitions());
    let t = Instant::now();
    detector.observe_hook(&HookEvent::new("nobody", "start"), t);
    assert!(detector.agent().is_none());
    detector.observe_hook(&hook("start"), t);
    detector.observe_hook(&hook("something-else"), t);
    assert_eq!(detector.state(), AgentState::Working);
}

#[test]
fn a_screen_that_lags_behind_a_hook_does_not_undo_it() {
    let mut detector = Detector::new(definitions());
    detector.identify_command("toy");
    let t = Instant::now();
    let working = screen_of(&["thinking"]);
    detector.observe_screen(&working, t);
    detector.observe_screen(&working, t + SETTLE);
    detector.observe_hook(&hook("end"), t + SETTLE * 2);
    // The screen still shows the old frame for a moment.
    detector.observe_screen(&working, t + SETTLE * 2);
    detector.observe_screen(&working, t + SETTLE * 3 - Duration::from_millis(1));
    assert_eq!(detector.state(), AgentState::Done);
}

#[test]
fn the_screen_corrects_a_hook_when_it_disagrees_for_the_settle_time() {
    let mut detector = Detector::new(definitions());
    detector.identify_command("toy");
    let t = Instant::now();
    detector.observe_hook(&hook("start"), t);
    // Interrupted by the user: no hook arrives, the prompt comes back.
    let prompt = screen_of(&["toy>"]);
    detector.observe_screen(&prompt, t + SETTLE);
    detector.observe_screen(&prompt, t + SETTLE * 2);
    assert_eq!(detector.state(), AgentState::Done);
}

#[test]
fn the_prompt_on_the_screen_does_not_end_an_error() {
    let mut detector = Detector::new(definitions());
    detector.identify_command("toy");
    let t = Instant::now();
    detector.observe_hook(&hook("start"), t);
    detector.observe_hook(&hook("fail"), t);
    assert_eq!(detector.state(), AgentState::Error);
    // The agent is back at its prompt, as after a turn that went well:
    // the screen does not tell the two apart.
    let prompt = screen_of(&["toy>"]);
    detector.observe_screen(&prompt, t + SETTLE);
    detector.observe_screen(&prompt, t + SETTLE * 2);
    assert_eq!(detector.state(), AgentState::Error);
    assert_eq!(detector.pending_deadline(), None);
}

#[test]
fn the_next_turn_ends_an_error_by_a_hook_or_on_the_screen() {
    let mut detector = Detector::new(definitions());
    detector.identify_command("toy");
    let t = Instant::now();
    detector.observe_hook(&hook("fail"), t);
    detector.observe_hook(&hook("start"), t);
    assert_eq!(detector.state(), AgentState::Working);

    detector.observe_hook(&hook("fail"), t);
    let working = screen_of(&["thinking"]);
    detector.observe_screen(&working, t + SETTLE);
    detector.observe_screen(&working, t + SETTLE * 2);
    assert_eq!(detector.state(), AgentState::Working);
}

#[test]
fn another_session_takes_over_after_an_error() {
    let mut detector = Detector::new(definitions());
    let t = Instant::now();
    detector.observe_hook(&hook_of("first", "start"), t);
    detector.observe_hook(&hook_of("first", "fail"), t);
    detector.observe_hook(&hook_of("second", "start"), t);
    assert_eq!(detector.state(), AgentState::Working);
    assert_eq!(detector.owner(), Some("second"));
}

fn hook_of(session: &str, name: &str) -> HookEvent {
    hook(name).with_session(session)
}

#[test]
fn a_hook_of_another_session_is_ignored_in_the_middle_of_a_turn() {
    let mut detector = Detector::new(definitions());
    let t = Instant::now();
    detector.observe_hook(&hook_of("outer", "start"), t);
    // An agent the outer one runs as a tool reports with the same pane id.
    detector.observe_hook(&hook_of("nested", "start"), t);
    detector.observe_hook(&hook_of("nested", "end"), t);
    assert_eq!(detector.state(), AgentState::Working);
    detector.observe_hook(&hook_of("nested", "bye"), t);
    assert_eq!(detector.agent().map(|agent| agent.id), Some("toy"));
    assert_eq!(detector.state(), AgentState::Working);

    detector.observe_hook(&hook_of("outer", "ask"), t);
    detector.observe_hook(&hook_of("nested", "end"), t);
    assert_eq!(detector.state(), AgentState::Waiting(WaitReason::Question));
    // A hook that names no session is not the owner's either.
    detector.observe_hook(&hook("end"), t);
    assert_eq!(detector.state(), AgentState::Waiting(WaitReason::Question));
}

#[test]
fn another_session_takes_over_between_turns() {
    let mut detector = Detector::new(definitions());
    let t = Instant::now();
    detector.observe_hook(&hook_of("first", "start"), t);
    detector.observe_hook(&hook_of("first", "end"), t);
    detector.observe_hook(&hook_of("second", "start"), t);
    assert_eq!(detector.state(), AgentState::Working);
    // Now the first one is the stranger.
    detector.observe_hook(&hook_of("first", "end"), t);
    assert_eq!(detector.state(), AgentState::Working);
}

#[test]
fn a_session_that_died_in_a_turn_is_replaced_once_the_screen_shows_the_prompt() {
    let mut detector = Detector::new(definitions());
    let t = Instant::now();
    detector.observe_hook(&hook_of("dead", "start"), t);
    let prompt = screen_of(&["toy>"]);
    detector.observe_screen(&prompt, t + SETTLE);
    detector.observe_screen(&prompt, t + SETTLE * 2);
    assert_eq!(detector.state(), AgentState::Done);
    detector.observe_hook(&hook_of("new", "start"), t + SETTLE * 3);
    assert_eq!(detector.state(), AgentState::Working);
}

#[test]
fn the_end_of_another_session_does_not_remove_the_agent() {
    let mut detector = Detector::new(definitions());
    let t = Instant::now();
    detector.observe_hook(&hook_of("outer", "start"), t);
    detector.observe_hook(&hook_of("outer", "end"), t);
    // Between turns, too: an agent left running in the background ends.
    detector.observe_hook(&hook_of("background", "bye"), t);
    assert_eq!(detector.agent().map(|agent| agent.id), Some("toy"));
    assert_eq!(detector.state(), AgentState::Done);
    detector.observe_hook(&hook("bye"), t);
    assert_eq!(detector.state(), AgentState::Done);
}

#[test]
fn a_hook_of_a_subagent_changes_nothing() {
    let mut detector = Detector::new(definitions());
    let t = Instant::now();
    detector.observe_hook(&hook("start").from_subagent(), t);
    assert!(detector.agent().is_none());
    detector.observe_hook(&hook_of("main", "end"), t);
    detector.observe_hook(&hook_of("main", "start").from_subagent(), t);
    detector.observe_hook(&hook_of("main", "bye").from_subagent(), t);
    assert_eq!(detector.state(), AgentState::Done);
}

#[test]
fn a_hook_of_another_agent_is_ignored_in_the_middle_of_a_turn() {
    let mut detector = Detector::new(definitions());
    let t = Instant::now();
    detector.observe_hook(&hook("start"), t);
    detector.observe_hook(&HookEvent::new("claude", "SessionStart"), t);
    detector.observe_hook(&HookEvent::new("claude", "Stop"), t);
    assert_eq!(detector.agent().map(|agent| agent.id), Some("toy"));
    assert_eq!(detector.state(), AgentState::Working);
}

#[test]
fn the_end_of_the_session_removes_the_agent_and_its_state() {
    let mut detector = Detector::new(definitions());
    let t = Instant::now();
    let banner_and_prompt = screen_of(&["TOY AGENT", "toy>"]);
    detector.observe_screen(&banner_and_prompt, t);
    detector.observe_hook(&hook_of("s", "start"), t);
    detector.observe_hook(&hook_of("s", "end"), t);
    detector.observe_hook(&hook_of("s", "bye"), t);
    assert!(detector.agent().is_none());
    assert_eq!(detector.state(), AgentState::Unknown);

    // What the agent left on the screen does not bring it back.
    detector.observe_screen(&banner_and_prompt, t + SETTLE);
    detector.observe_screen(&banner_and_prompt, t + SETTLE * 2);
    assert!(detector.agent().is_none());
    assert_eq!(detector.state(), AgentState::Unknown);

    // Once the old banner has left the screen, a banner is a new agent.
    detector.observe_screen(&screen_of(&["$ ls"]), t + SETTLE * 3);
    detector.observe_screen(&banner_and_prompt, t + SETTLE * 4);
    detector.observe_screen(&banner_and_prompt, t + SETTLE * 5);
    assert_eq!(detector.agent().map(|agent| agent.id), Some("toy"));
    assert_eq!(detector.state(), AgentState::Done);
}

#[test]
fn a_hook_names_the_agent_again_right_after_the_end_of_a_session() {
    let mut detector = Detector::new(definitions());
    let t = Instant::now();
    detector.observe_hook(&hook_of("old", "end"), t);
    detector.observe_hook(&hook_of("old", "bye"), t);
    detector.observe_hook(&hook_of("new", "start"), t);
    assert_eq!(detector.agent().map(|agent| agent.id), Some("toy"));
    assert_eq!(detector.state(), AgentState::Working);
}

#[test]
fn the_detector_tells_when_a_screen_state_is_due() {
    let mut detector = Detector::new(definitions());
    detector.identify_command("toy");
    let t = Instant::now();
    assert_eq!(detector.pending_deadline(), None);
    let prompt = screen_of(&["toy>"]);
    detector.observe_screen(&prompt, t);
    assert_eq!(detector.pending_deadline(), Some(t + SETTLE));
    // Looking again when it is due is enough; nothing has to be printed.
    detector.observe_rows(&prompt.rows, t + SETTLE);
    assert_eq!(detector.state(), AgentState::Done);
    assert_eq!(detector.pending_deadline(), None);
}

/// The look at the time a screen state is due can come late, as when
/// the app is busy. The state was shown all that time, so it takes
/// effect before the screen that comes next is weighed: an agent whose
/// turn was on the screen for two seconds is not taken to have gone from
/// nothing straight to its prompt.
#[test]
fn a_screen_state_shown_past_its_time_takes_effect_when_the_next_screen_comes() {
    let mut detector = Detector::new(definitions());
    detector.identify_command("toy");
    let t = Instant::now();
    detector.observe_screen(&screen_of(&["thinking"]), t);
    // Nobody looked when it was due; the prompt comes long after.
    let later = t + SETTLE * 4;
    detector.observe_screen(&screen_of(&["toy>"]), later);
    assert_eq!(detector.state(), AgentState::Working);
    assert_eq!(detector.pending_deadline(), Some(later + SETTLE));
}

#[test]
fn a_screen_state_shown_past_its_time_takes_effect_when_a_screen_without_a_signal_comes() {
    let mut detector = Detector::new(definitions());
    detector.identify_command("toy");
    let t = Instant::now();
    detector.observe_screen(&screen_of(&["thinking"]), t);
    detector.observe_screen(&screen_of(&["some output"]), t + SETTLE * 4);
    assert_eq!(detector.state(), AgentState::Working);
    assert_eq!(detector.pending_deadline(), None);
}

/// Only a state that was on the screen when it was due takes effect late:
/// one a screen without a signal replaced before then does not.
#[test]
fn a_screen_state_replaced_before_its_time_does_not_take_effect_late() {
    let mut detector = Detector::new(definitions());
    detector.identify_command("toy");
    let t = Instant::now();
    detector.observe_screen(&screen_of(&["thinking"]), t);
    detector.observe_screen(&screen_of(&["some output"]), t + SETTLE / 2);
    detector.observe_screen(&screen_of(&["toy>"]), t + SETTLE * 4);
    assert_eq!(detector.state(), AgentState::Unknown);
    assert_eq!(detector.pending_deadline(), Some(t + SETTLE * 5));
}

#[test]
fn a_screen_state_that_is_not_shown_when_it_is_due_is_dropped() {
    let mut detector = Detector::new(definitions());
    detector.identify_command("toy");
    let t = Instant::now();
    detector.observe_hook(&hook("start"), t);
    // One half-drawn frame shows the prompt, and then nothing tells the
    // state for a while.
    let prompt = screen_of(&["toy>"]);
    let blank = screen_of(&["some output"]);
    detector.observe_screen(&prompt, t);
    detector.observe_screen(&blank, t + SETTLE);
    assert_eq!(detector.pending_deadline(), None);
    // A second such frame starts from nothing.
    detector.observe_screen(&prompt, t + SETTLE * 2);
    assert_eq!(detector.state(), AgentState::Working);
    assert_eq!(detector.pending_deadline(), Some(t + SETTLE * 3));
}

#[test]
fn an_event_no_rule_matches_names_an_unknown_agent_and_changes_nothing_else() {
    let mut detector = Detector::new(definitions());
    let t = Instant::now();
    // The start of a session is such an event for the built-in agents.
    detector.observe_hook(&hook_of("first", "something-else"), t);
    assert_eq!(detector.agent().map(|agent| agent.id), Some("toy"));
    detector.observe_hook(&hook_of("first", "end"), t);

    // Between turns another session takes the pane over only by saying
    // what it does.
    detector.observe_hook(&hook_of("second", "something-else"), t);
    detector.observe_hook(&hook_of("first", "bye"), t);
    assert!(detector.agent().is_none());
}

#[test]
fn the_owner_is_the_session_in_front_until_it_ends() {
    let mut detector = Detector::new(definitions());
    let t = Instant::now();
    assert_eq!(detector.owner(), None);
    detector.observe_hook(&hook_of("outer", "start"), t);
    assert_eq!(detector.owner(), Some("outer"));
    // An agent the outer one runs as a tool does not take the pane.
    detector.observe_hook(&hook_of("nested", "start"), t);
    detector.observe_hook(&hook_of("nested", "bye"), t);
    assert_eq!(detector.owner(), Some("outer"));
    detector.observe_hook(&hook_of("outer", "bye"), t);
    assert_eq!(detector.owner(), None);
}

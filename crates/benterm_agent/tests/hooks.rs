//! The built-in hook rules against event sequences that real Claude Code
//! 2.1.330 and Codex 0.169.0 sent while the agents were recorded.

use std::time::Instant;

use benterm_agent::{AgentState, Detector, HookEvent, HookRule, WaitReason, builtin};

const WORKING: AgentState = AgentState::Working;
const DONE: AgentState = AgentState::Done;
const ERROR: AgentState = AgentState::Error;
const PERMISSION: AgentState = AgentState::Waiting(WaitReason::Permission);
const QUESTION: AgentState = AgentState::Waiting(WaitReason::Question);
const CHOICE: AgentState = AgentState::Waiting(WaitReason::Choice);

fn follow(agent: &str, events: &[(HookEvent, AgentState)]) {
    let mut detector = Detector::new(builtin());
    let now = Instant::now();
    for (event, expected) in events {
        detector.observe_hook(event, now);
        assert_eq!(detector.state(), *expected, "after {event:?}");
        assert_eq!(detector.agent().map(|agent| agent.id), Some(agent));
    }
}

#[test]
fn claude_code_hook_events_give_their_states() {
    let event = |name| HookEvent::new("claude", name);
    follow(
        "claude",
        &[
            // The start of a session names the agent and no state: a
            // session also starts in the middle of a turn, after its
            // context is compacted.
            (event("SessionStart"), AgentState::Unknown),
            (event("UserPromptSubmit"), WORKING),
            (event("Stop"), DONE),
            (event("UserPromptSubmit"), WORKING),
            (event("PreToolUse").with_tool("Bash"), WORKING),
            (event("PermissionRequest").with_tool("Bash"), PERMISSION),
            (event("PostToolUse").with_tool("Bash"), WORKING),
            (event("Stop"), DONE),
            (event("UserPromptSubmit"), WORKING),
            (event("PreToolUse").with_tool("AskUserQuestion"), CHOICE),
            (
                event("PermissionRequest").with_tool("AskUserQuestion"),
                CHOICE,
            ),
            (
                event("Notification").with_notification_type("permission_prompt"),
                CHOICE,
            ),
            (event("PostToolUse").with_tool("AskUserQuestion"), WORKING),
            (event("Stop"), DONE),
            (
                event("Notification").with_notification_type("idle_prompt"),
                DONE,
            ),
            (event("UserPromptSubmit"), WORKING),
            // An API error ended the turn: this comes in place of Stop.
            (event("StopFailure"), ERROR),
            (event("UserPromptSubmit"), WORKING),
        ],
    );
}

#[test]
fn codex_hook_events_give_their_states() {
    let event = |name| HookEvent::new("codex", name);
    follow(
        "codex",
        &[
            (event("SessionStart"), AgentState::Unknown),
            (event("UserPromptSubmit"), WORKING),
            (event("Stop"), DONE),
            (event("UserPromptSubmit"), WORKING),
            (event("PreToolUse").with_tool("Bash"), WORKING),
            (event("PermissionRequest").with_tool("Bash"), PERMISSION),
            // A refused approval and an interrupted turn.
            (event("Interrupt"), ERROR),
            (event("UserPromptSubmit"), WORKING),
            (
                event("PreToolUse").with_tool("request_user_input"),
                QUESTION,
            ),
            (
                event("PostToolUse").with_tool("request_user_input"),
                WORKING,
            ),
            (event("Stop"), DONE),
        ],
    );
}

#[test]
fn hook_rules_can_match_the_tool_and_the_notification_type() {
    let rule = HookRule::new("PreToolUse", AgentState::Working);
    let question = HookRule::new("PreToolUse", AgentState::Waiting(WaitReason::Question))
        .tool("AskUserQuestion");
    let idle = HookRule::new("Notification", AgentState::Done).notification_type("idle_prompt");

    let ask = HookEvent::new("claude", "PreToolUse").with_tool("AskUserQuestion");
    let bash = HookEvent::new("claude", "PreToolUse").with_tool("Bash");
    assert!(question.matches(&ask));
    assert!(!question.matches(&bash));
    assert!(rule.matches(&bash));

    let idle_event = HookEvent::new("claude", "Notification").with_notification_type("idle_prompt");
    let other =
        HookEvent::new("claude", "Notification").with_notification_type("permission_prompt");
    assert!(idle.matches(&idle_event));
    assert!(!idle.matches(&other));
}

#[test]
fn the_end_of_a_session_means_the_agent_is_gone() {
    for agent in ["claude", "codex"] {
        let mut detector = Detector::new(builtin());
        let now = Instant::now();
        detector.observe_hook(&HookEvent::new(agent, "UserPromptSubmit"), now);
        detector.observe_hook(&HookEvent::new(agent, "Stop"), now);
        detector.observe_hook(&HookEvent::new(agent, "SessionEnd"), now);
        assert_eq!(detector.state(), AgentState::Unknown, "{agent}");
        assert!(detector.agent().is_none(), "{agent}");
    }
}

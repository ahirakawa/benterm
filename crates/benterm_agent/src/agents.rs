//! The definitions of the agents Benterm knows.
//!
//! The screen rules were written against Claude Code 2.1.330 and Codex
//! 0.169.0; `tests/fixtures` holds made-up screens with the same footers
//! and dialogs, which they are checked against. The rules key on the footer
//! each agent draws under its prompt or dialog, so they look only at the
//! last few lines.
//!
//! The hook rules follow the events the same versions sent while the
//! agents were recorded. The hook command passes `hook_event_name`,
//! `tool_name` and `notification_type` from its JSON input.

use crate::definition::{AgentDefinition, HookRule, Matcher, Region, ScreenRule};
use crate::state::{AgentState, WaitReason};

use Matcher::{All, Any, Contains, LastLine, LineStartsWith, NumberedLine};

const WORKING: AgentState = AgentState::Working;
const DONE: AgentState = AgentState::Done;
const ERROR: AgentState = AgentState::Error;
const PERMISSION: AgentState = AgentState::Waiting(WaitReason::Permission);
const QUESTION: AgentState = AgentState::Waiting(WaitReason::Question);
const CHOICE: AgentState = AgentState::Waiting(WaitReason::Choice);
const TRUST: AgentState = AgentState::Waiting(WaitReason::Trust);
/// For the hook event that ends a session.
const GONE: AgentState = AgentState::Unknown;

/// Every built-in definition.
pub fn builtin() -> Vec<AgentDefinition> {
    vec![claude_code(), codex()]
}

fn rule(state: AgentState, last_lines: usize, matcher: Matcher) -> ScreenRule {
    ScreenRule {
        state,
        region: Region::LastLines(last_lines),
        matcher,
    }
}

pub fn claude_code() -> AgentDefinition {
    AgentDefinition {
        id: "claude",
        name: "Claude Code",
        commands: &["claude"],
        banner: Contains("Claude Code"),
        screen_rules: vec![
            // AskUserQuestion: "Enter to select · ↑/↓ to navigate · Esc to cancel".
            rule(
                CHOICE,
                2,
                All(vec![Contains("Enter to select"), Contains("Esc to cancel")]),
            ),
            // Tool approval: "Do you want to proceed?" above the options and
            // "Esc to cancel · Tab to amend" at the bottom.
            rule(
                PERMISSION,
                12,
                All(vec![Contains("Do you want to"), Contains("Esc to cancel")]),
            ),
            // Trusting the working directory at start-up.
            rule(TRUST, 2, Contains("Enter to confirm · Esc to cancel")),
            // The footer under the prompt box while a turn runs.
            rule(WORKING, 3, Contains("esc to interrupt")),
            // The prompt box and its footer.
            rule(
                DONE,
                4,
                Any(vec![LineStartsWith("❯"), Contains("? for shortcuts")]),
            ),
        ],
        hook_rules: vec![
            // AskUserQuestion goes through the permission machinery too.
            HookRule::new("PreToolUse", CHOICE).tool("AskUserQuestion"),
            HookRule::new("PermissionRequest", CHOICE).tool("AskUserQuestion"),
            HookRule::new("PermissionRequest", PERMISSION),
            // An MCP server's form, whose fields may take text.
            HookRule::new("Notification", QUESTION).notification_type("elicitation_dialog"),
            HookRule::new("Notification", DONE).notification_type("idle_prompt"),
            // A "permission_prompt" notification follows PermissionRequest
            // for both approvals and questions, so it adds nothing and is
            // left unmatched.
            HookRule::new("UserPromptSubmit", WORKING),
            HookRule::new("PreToolUse", WORKING),
            HookRule::new("PostToolUse", WORKING),
            HookRule::new("PostToolUseFailure", WORKING),
            HookRule::new("Stop", DONE),
            // Comes in place of Stop when an API error ended the turn.
            HookRule::new("StopFailure", ERROR),
            // SessionStart names the agent and gives no state: a session
            // also starts in the middle of a turn, when its context is
            // compacted, and the prompt shows on the screen soon enough.
            HookRule::new("SessionEnd", GONE),
        ],
        session_start: Some("SessionStart"),
    }
}

pub fn codex() -> AgentDefinition {
    AgentDefinition {
        id: "codex",
        name: "Codex",
        commands: &["codex"],
        banner: Contains(">_ OpenAI Codex"),
        screen_rules: vec![
            // request_user_input, whose options end with one for text of
            // the user's own, so it is no question of choices. Its footer also says "esc to interrupt", so
            // this rule comes before the working one.
            rule(
                QUESTION,
                2,
                Any(vec![
                    Contains("enter to submit answer"),
                    Contains("enter to submit all"),
                ]),
            ),
            // Command approval.
            rule(
                PERMISSION,
                2,
                Contains("Press enter to confirm or esc to cancel"),
            ),
            // Trusting the working directory at start-up; 0.171.0 says
            // "enter continue · esc back" at the bottom.
            rule(
                TRUST,
                12,
                All(vec![
                    Contains("Trust this folder?"),
                    Contains("enter continue"),
                ]),
            ),
            // A numbered choice, such as the one at start-up when the
            // background server runs with other feature settings in
            // 0.171.0. The selected option starts with "›", as the prompt
            // line does, so this rule comes before the prompt's. A numbered
            // list typed at the prompt looks the same, but the prompt has a
            // footer under it, and the choice has none: its options are the
            // last lines drawn.
            rule(
                CHOICE,
                10,
                All(vec![
                    NumberedLine("›"),
                    LastLine(Box::new(Any(vec![NumberedLine("›"), NumberedLine("")]))),
                ]),
            ),
            // The status line above the prompt: "• Working (3s • esc to interrupt)".
            rule(WORKING, 6, Contains("esc to interrupt)")),
            // The prompt and its footer.
            rule(
                DONE,
                4,
                Any(vec![LineStartsWith("›"), Contains("? for shortcuts")]),
            ),
        ],
        hook_rules: vec![
            // request_user_input asks without a PermissionRequest.
            HookRule::new("PreToolUse", QUESTION).tool("request_user_input"),
            HookRule::new("PermissionRequest", PERMISSION),
            HookRule::new("UserPromptSubmit", WORKING),
            HookRule::new("PreToolUse", WORKING),
            HookRule::new("PostToolUse", WORKING),
            HookRule::new("Stop", DONE),
            // A refused approval and a turn the user interrupted.
            HookRule::new("Interrupt", ERROR),
            HookRule::new("SessionEnd", GONE),
        ],
        session_start: Some("SessionStart"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(lines: &[&str]) -> Vec<String> {
        lines.iter().map(|line| (*line).to_owned()).collect()
    }

    /// A numbered choice Codex shows at start-up, before its prompt: the
    /// options are the last lines drawn, with no footer under them.
    #[test]
    fn codex_reads_a_numbered_choice_whichever_option_is_selected() {
        for selected in 0..3 {
            let mut screen = lines(&["  Made-up question at start-up", ""]);
            for (i, option) in ["Go on this time", "Restart", "Cancel"].iter().enumerate() {
                let mark = if i == selected { "›" } else { " " };
                screen.push(format!("{mark} {}. {option}", i + 1));
            }
            assert_eq!(codex().state_in(&screen), Some(CHOICE), "{screen:#?}");
        }
    }

    /// The prompt with a numbered list typed into it looks like a choice,
    /// but the footer comes under it.
    #[test]
    fn codex_reads_a_numbered_draft_at_its_prompt_as_its_prompt() {
        for footer in ["  made-up model · ~/made-up-folder", "  ? for shortcuts"] {
            let screen = lines(&["› 1. first item", "  2. second item", "", footer]);
            assert_eq!(codex().state_in(&screen), Some(DONE), "{footer}");
        }
    }
}

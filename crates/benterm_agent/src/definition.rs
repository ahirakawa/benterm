//! The data that describes one agent.

use crate::plan::PlanUpdate;
use benterm_core::{Row, Snapshot};

use crate::state::AgentState;

/// Everything Benterm knows about one agent.
#[derive(Debug, Clone)]
pub struct AgentDefinition {
    /// A short stable name, also used by hook events to say which agent
    /// sent them.
    pub id: &'static str,
    /// The name shown to the user.
    pub name: &'static str,
    /// Names of the executables that start the agent.
    pub commands: &'static [&'static str],
    /// Text that only this agent puts on the screen, such as its start-up
    /// banner. It is looked for on the whole screen.
    pub banner: Matcher,
    /// Screen rules in order of priority; the first that matches decides.
    pub screen_rules: Vec<ScreenRule>,
    /// Hook rules in order of priority; the first that matches decides.
    /// Events no rule matches are ignored.
    pub hook_rules: Vec<HookRule>,
    /// The name of the hook event that tells a session has started, if
    /// the agent sends one. It tells one run of the agent from the next
    /// when the program in front does not.
    pub session_start: Option<&'static str>,
}

impl AgentDefinition {
    pub fn shows_banner(&self, screen: &Snapshot) -> bool {
        self.banner_in(&screen_lines(&screen.rows))
    }

    /// The state the screen shows, or `None` when no rule matches.
    pub fn screen_state(&self, screen: &Snapshot) -> Option<AgentState> {
        self.state_in(&screen_lines(&screen.rows))
    }

    pub(crate) fn banner_in(&self, lines: &[String]) -> bool {
        self.banner.matches(lines)
    }

    pub(crate) fn state_in(&self, lines: &[String]) -> Option<AgentState> {
        self.screen_rules
            .iter()
            .find(|rule| rule.matcher.matches(rule.region.select(lines)))
            .map(|rule| rule.state)
    }

    /// Whether a hook event tells that a session of this agent started.
    pub fn starts_session(&self, event: &HookEvent) -> bool {
        event.agent == self.id && self.session_start == Some(event.name.as_str())
    }

    /// The state a hook event reports, or `None` when no rule matches.
    pub fn hook_state(&self, event: &HookEvent) -> Option<AgentState> {
        if event.agent != self.id {
            return None;
        }
        self.hook_rules
            .iter()
            .find(|rule| rule.matches(event))
            .map(|rule| rule.state)
    }
}

/// One screen rule: when `matcher` matches the lines of `region`, the agent
/// is in `state`.
#[derive(Debug, Clone)]
pub struct ScreenRule {
    pub state: AgentState,
    pub region: Region,
    pub matcher: Matcher,
}

/// The part of the screen a rule looks at. Lines are compared with their
/// trailing blanks removed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Region {
    /// Every line of the screen.
    Screen,
    /// The last `n` lines that are not blank. Agents draw their prompt and
    /// dialogs at the bottom, so this keeps old output above from matching.
    LastLines(usize),
}

impl Region {
    fn select(self, lines: &[String]) -> &[String] {
        match self {
            Self::Screen => lines,
            Self::LastLines(n) => {
                // Blank lines are dropped before the last `n` are taken.
                let last = lines.iter().rposition(|line| !line.is_empty());
                let Some(last) = last else { return &[] };
                let mut start = last + 1;
                let mut kept = 0;
                while start > 0 && kept < n {
                    start -= 1;
                    if !lines[start].is_empty() {
                        kept += 1;
                    }
                }
                &lines[start..=last]
            }
        }
    }
}

/// The rows of a screen as lines with trailing blanks removed.
pub(crate) fn screen_lines(rows: &[Row]) -> Vec<String> {
    rows.iter()
        .map(|row| row.text().trim_end().to_owned())
        .collect()
}

/// A test on the lines of a region.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Matcher {
    /// Some line contains the text.
    Contains(&'static str),
    /// Some line starts with the text, after its leading blanks.
    LineStartsWith(&'static str),
    /// Some line is an option of a numbered list marked with the text:
    /// after its leading blanks, the text, more blanks, a number and ". ".
    /// With "" it is an option without a mark.
    NumberedLine(&'static str),
    LastLine(Box<Matcher>),
    /// Every matcher matches.
    All(Vec<Matcher>),
    /// At least one matcher matches.
    Any(Vec<Matcher>),
    Not(Box<Matcher>),
}

impl Matcher {
    pub fn matches(&self, lines: &[String]) -> bool {
        match self {
            Self::Contains(text) => lines.iter().any(|line| line.contains(text)),
            Self::LineStartsWith(text) => {
                lines.iter().any(|line| line.trim_start().starts_with(text))
            }
            Self::NumberedLine(mark) => lines.iter().any(|line| is_numbered(line, mark)),
            Self::LastLine(matcher) => lines
                .last()
                .is_some_and(|line| matcher.matches(std::slice::from_ref(line))),
            Self::All(matchers) => matchers.iter().all(|matcher| matcher.matches(lines)),
            Self::Any(matchers) => matchers.iter().any(|matcher| matcher.matches(lines)),
            Self::Not(matcher) => !matcher.matches(lines),
        }
    }
}

/// Whether `line` is an option of a numbered list marked with `mark`.
fn is_numbered(line: &str, mark: &str) -> bool {
    let Some(rest) = line.trim_start().strip_prefix(mark) else {
        return false;
    };
    let rest = rest.trim_start();
    let number = rest.len() - rest.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    number > 0 && rest[number..].starts_with(". ")
}

/// One hook rule: an event named `event`, and with the given tool and
/// notification type when those are set, means the agent is in `state`.
///
/// A rule whose state is [`AgentState::Unknown`] marks the event that ends
/// the agent's session: after it the agent is gone from the pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookRule {
    pub event: &'static str,
    pub tool: Option<&'static str>,
    pub notification_type: Option<&'static str>,
    pub state: AgentState,
}

impl HookRule {
    /// A rule matching every event named `event`.
    pub fn new(event: &'static str, state: AgentState) -> Self {
        Self {
            event,
            tool: None,
            notification_type: None,
            state,
        }
    }

    /// Narrows the rule to events about `tool`.
    pub fn tool(self, tool: &'static str) -> Self {
        Self {
            tool: Some(tool),
            ..self
        }
    }

    pub fn notification_type(self, kind: &'static str) -> Self {
        Self {
            notification_type: Some(kind),
            ..self
        }
    }

    /// Whether the event has this rule's name and, where the rule sets
    /// them, its tool and notification type. The agent is not compared.
    pub fn matches(&self, event: &HookEvent) -> bool {
        let narrows = |want: Option<&str>, got: &Option<String>| {
            want.is_none_or(|want| got.as_deref() == Some(want))
        };
        event.name == self.event
            && narrows(self.tool, &event.tool)
            && narrows(self.notification_type, &event.notification_type)
    }
}

/// An event reported by an agent's hook.
///
/// The fields are those of the hook's JSON input that decide the state,
/// `hook_event_name`, `tool_name` and, for Claude Code's `Notification`,
/// `notification_type`, and those that tell who sent the event,
/// `session_id` and `agent_id`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookEvent {
    /// The [`AgentDefinition::id`] of the agent that sent the event.
    pub agent: String,
    pub name: String,
    pub tool: Option<String>,
    pub notification_type: Option<String>,
    /// The session the event belongs to, when the hook told. An agent
    /// that another agent runs as a tool reports for the same pane with a
    /// session of its own.
    pub session: Option<String>,
    /// The event comes from a subagent of the session, not from its main
    /// thread.
    pub subagent: bool,
    /// What the event does to the agent's plan, for the call of a tool
    /// the agent keeps its plan with.
    pub plan: Option<PlanUpdate>,
}

impl HookEvent {
    pub fn new(agent: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            agent: agent.into(),
            name: name.into(),
            tool: None,
            notification_type: None,
            session: None,
            subagent: false,
            plan: None,
        }
    }

    pub fn with_plan(self, plan: PlanUpdate) -> Self {
        Self {
            plan: Some(plan),
            ..self
        }
    }

    pub fn with_session(self, session: impl Into<String>) -> Self {
        Self {
            session: Some(session.into()),
            ..self
        }
    }

    pub fn from_subagent(self) -> Self {
        Self {
            subagent: true,
            ..self
        }
    }

    pub fn with_tool(self, tool: impl Into<String>) -> Self {
        Self {
            tool: Some(tool.into()),
            ..self
        }
    }

    pub fn with_notification_type(self, kind: impl Into<String>) -> Self {
        Self {
            notification_type: Some(kind.into()),
            ..self
        }
    }
}

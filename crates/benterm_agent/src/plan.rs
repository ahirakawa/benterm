//! The plan of an agent: the list of things it means to do, as it keeps
//! it with its own tool. Claude Code keeps it with TodoWrite, or with
//! TaskCreate and TaskUpdate; Codex keeps it with update_plan.
//!
//! The plan is read from what the hooks of those tools are given: the
//! input of the tool and what it answered.

use serde_json::Value;

/// How far one item of a plan is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanStatus {
    Pending,
    InProgress,
    Completed,
}

/// One thing the agent means to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanItem {
    pub text: String,
    pub status: PlanStatus,
}

/// How many items of a plan are how far.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PlanCounts {
    pub done: usize,
    pub in_progress: usize,
    pub remaining: usize,
}

/// The items of a plan, in the agent's order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Plan {
    pub items: Vec<PlanItem>,
}

impl Plan {
    pub fn counts(&self) -> PlanCounts {
        let mut counts = PlanCounts::default();
        for item in &self.items {
            match item.status {
                PlanStatus::Completed => counts.done += 1,
                PlanStatus::InProgress => counts.in_progress += 1,
                PlanStatus::Pending => counts.remaining += 1,
            }
        }
        counts
    }
}

/// What one call of a plan tool does to the plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanUpdate {
    /// The whole plan anew, of a tool that is given all of it.
    Replace(Vec<PlanItem>),
    /// A new task at the end, with the number the tool gave it.
    Create { id: String, text: String },
    /// Another text or status for the task with the number `id`.
    Change {
        id: String,
        text: Option<String>,
        status: Option<PlanStatus>,
    },
    /// The task with the number `id` is no more.
    Delete { id: String },
}

impl PlanUpdate {
    /// Reads what a hook event does to the plan, from the fields of the
    /// hook's input. A plan is read from a tool that has run: the event
    /// before a tool tells what the agent asked for, which may fail.
    pub fn from_hook(
        agent: &str,
        event: &str,
        tool: Option<&str>,
        input: Option<&Value>,
        response: Option<&Value>,
    ) -> Option<Self> {
        if event != "PostToolUse" {
            return None;
        }
        Self::from_tool(agent, tool?, input.unwrap_or(&Value::Null), response)
    }

    /// Reads the call of `tool` by `agent` from its input and its answer,
    /// as a PostToolUse hook is given them. Claude Code's TodoWrite gives
    /// the whole list every time; its Task tools make and change one
    /// numbered task at a time; TaskList is not read, as it may run
    /// beside other tools and its hook may come late. Which of the two a session has is Claude
    /// Code's choice. Codex's update_plan gives the whole plan every
    /// time. `None` for a tool that keeps
    /// no plan, for a call that failed, and for one that cannot be read.
    pub fn from_tool(
        agent: &str,
        tool: &str,
        input: &Value,
        response: Option<&Value>,
    ) -> Option<Self> {
        match (agent, tool) {
            ("claude", "TodoWrite") => {
                let todos = input.get("todos")?.as_array()?;
                Some(Self::Replace(items(todos, "content")))
            }
            ("claude", "TaskCreate") => {
                let (id, subject) = created_task(response?)?;
                let text = subject.or_else(|| text_of(input, "subject"))?;
                Some(Self::Create { id, text })
            }
            ("claude", "TaskUpdate") => {
                if response.is_some_and(|response| !task_updated(response)) {
                    return None;
                }
                let id = text_of(input, "taskId")?;
                if input.get("status").and_then(Value::as_str) == Some("deleted") {
                    return Some(Self::Delete { id });
                }
                Some(Self::Change {
                    id,
                    text: text_of(input, "subject"),
                    status: input.get("status").and_then(status),
                })
            }
            ("codex", "update_plan") => {
                // The arguments of a function call are JSON in a text,
                // which a hook may be given as it is.
                let parsed;
                let input = match input.as_str() {
                    Some(text) => {
                        parsed = serde_json::from_str::<Value>(text).ok()?;
                        &parsed
                    }
                    None => input,
                };
                let steps = input.get("plan")?.as_array()?;
                Some(Self::Replace(items(steps, "step")))
            }
            _ => None,
        }
    }
}

/// The field `name` of `value`, if it is text that says something.
fn text_of(value: &Value, name: &str) -> Option<String> {
    let text = value.get(name)?.as_str()?.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

/// A status as the tools spell it. `None` for what is no status.
fn status(value: &Value) -> Option<PlanStatus> {
    match value.as_str()? {
        "pending" => Some(PlanStatus::Pending),
        // Codex's tool spells it with an underscore, its app-server
        // without.
        "in_progress" | "inProgress" => Some(PlanStatus::InProgress),
        "completed" => Some(PlanStatus::Completed),
        _ => None,
    }
}

/// One item, whose text is the field `text`. An item in a state unheard
/// of is one left to do.
fn item(value: &Value, text: &str) -> Option<PlanItem> {
    Some(PlanItem {
        text: text_of(value, text)?,
        status: value
            .get("status")
            .and_then(status)
            .unwrap_or(PlanStatus::Pending),
    })
}

fn items(values: &[Value], text: &str) -> Vec<PlanItem> {
    values
        .iter()
        .filter_map(|value| item(value, text))
        .collect()
}

/// Whether the answer of TaskUpdate says that the task was changed. The
/// answer is `{"success": true, ..}`, or the text the model reads,
/// "Updated task #3 status".
fn task_updated(response: &Value) -> bool {
    match response.as_str() {
        Some(text) => text.starts_with("Updated task #"),
        None => response.get("success") != Some(&Value::Bool(false)),
    }
}

/// The number of the task TaskCreate made, and its subject if the answer
/// tells it. The answer is `{"task": {"id", "subject"}}`; that of a
/// subagent is the text the model reads, "Task #3 created successfully:
/// <subject>".
fn created_task(response: &Value) -> Option<(String, Option<String>)> {
    if let Some(task) = response.get("task") {
        return Some((text_of(task, "id")?, text_of(task, "subject")));
    }
    let rest = response.as_str()?.strip_prefix("Task #")?;
    let (id, rest) = rest.split_once(' ')?;
    let subject = rest.strip_prefix("created successfully")?;
    let subject = subject.strip_prefix(':').map(str::trim);
    let subject = subject.filter(|subject| !subject.is_empty());
    Some((id.to_owned(), subject.map(str::to_owned)))
}

/// The plan of one conversation, kept up with the calls of its tools.
#[derive(Debug, Clone, Default)]
pub struct PlanTracker {
    /// The conversation the plan belongs to.
    session: Option<String>,
    plan: Plan,
    /// The number of each item of the plan, for a plan of tasks.
    ids: Vec<String>,
}

impl PlanTracker {
    /// Takes an update of the plan of the conversation `session`. The
    /// plan of another conversation is dropped first. Returns whether
    /// what [`plan_of`](Self::plan_of) gives changed.
    pub fn apply(&mut self, session: &str, update: PlanUpdate) -> bool {
        let mut changed = false;
        if self.session.as_deref() != Some(session) {
            changed = !self.plan.items.is_empty();
            *self = Self {
                session: Some(session.to_owned()),
                ..Self::default()
            };
        }
        let before = self.plan.clone();
        match update {
            PlanUpdate::Replace(items) => {
                self.ids.clear();
                self.plan.items = items;
            }
            PlanUpdate::Create { id, text } => {
                // A task told a second time keeps how far it is.
                match self.position(&id) {
                    Some(at) => self.plan.items[at].text = text,
                    None => {
                        let status = PlanStatus::Pending;
                        self.push(id, PlanItem { text, status });
                    }
                }
            }
            PlanUpdate::Change { id, text, status } => match (self.position(&id), text) {
                (Some(at), text) => {
                    let item = &mut self.plan.items[at];
                    if let Some(text) = text {
                        item.text = text;
                    }
                    if let Some(status) = status {
                        item.status = status;
                    }
                }
                // A task made before the plan was followed is known
                // once the agent names it.
                (None, Some(text)) => {
                    let status = status.unwrap_or(PlanStatus::Pending);
                    self.push(id, PlanItem { text, status });
                }
                (None, None) => {}
            },
            PlanUpdate::Delete { id } => {
                if let Some(at) = self.position(&id) {
                    self.ids.remove(at);
                    self.plan.items.remove(at);
                }
            }
        }
        changed || before != self.plan
    }

    /// The plan of the conversation `session`: `None` when none was read
    /// for it, and when nothing is left of it.
    pub fn plan_of(&self, session: &str) -> Option<&Plan> {
        let known = self.session.as_deref() == Some(session) && !self.plan.items.is_empty();
        known.then_some(&self.plan)
    }

    fn position(&self, id: &str) -> Option<usize> {
        self.ids.iter().position(|known| known == id)
    }

    fn push(&mut self, id: String, item: PlanItem) {
        // A plan of tasks has a number for every item.
        if self.ids.len() != self.plan.items.len() {
            self.plan.items.clear();
            self.ids.clear();
        }
        self.ids.push(id);
        self.plan.items.push(item);
    }
}

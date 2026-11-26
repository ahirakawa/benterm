//! Reading the transcript of an agent: the JSON Lines file Claude Code
//! names in a hook's `transcript_path`, and the rollout file of Codex.
//! Both are read into the same entries.

use std::io::{self, BufRead, BufReader, Read};
use std::path::Path;

use serde_json::Value;

use crate::files::open_regular;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    User,
    Assistant,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolCall {
    pub name: String,
    /// The files the call writes: one for an edit of Claude Code, every
    /// file of the patch for Codex's `apply_patch`.
    pub file_paths: Vec<String>,
    /// The command of a call of the shell.
    pub command: Option<String>,
}

/// One line of a transcript that is of the user or of the agent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub kind: Kind,
    /// Names the entry in its transcript.
    pub id: String,
    /// The turn the entry belongs to: the prompt id of Claude Code, the
    /// turn id of Codex.
    pub turn: Option<String>,
    /// Written by the agent's program and not by the user.
    pub is_meta: bool,
    pub has_tool_result: bool,
    pub text: String,
    pub tools: Vec<ToolCall>,
}

/// How Claude Code starts the entry it records when a Stop hook sends
/// the agent back.
const CLAUDE_STOP_FEEDBACK: &str = "Stop hook feedback";

/// How Codex starts the message of the user it records for what a hook
/// said: `<hook_prompt hook_run_id="stop:...">`. Seen with Codex 0.171.
const CODEX_HOOK_PROMPT: &str = "<hook_prompt";
const CODEX_STOP_RUN: &str = "hook_run_id=\"stop:";

impl Entry {
    /// Whether the entry records that a Stop hook sent the agent back:
    /// any Stop hook, with any words.
    pub fn is_stop_feedback(&self) -> bool {
        if self.kind != Kind::User || !self.is_meta {
            return false;
        }
        if let Some(attributes) = hook_prompt_attributes(&self.text) {
            return attributes
                .split_whitespace()
                .any(|attribute| attribute.starts_with(CODEX_STOP_RUN));
        }
        self.text.starts_with(CLAUDE_STOP_FEEDBACK)
    }
}

/// The attributes of the `hook_prompt` tag that `text` starts with, if
/// it starts with one: the tag's name ends there, and the tag is closed.
fn hook_prompt_attributes(text: &str) -> Option<&str> {
    let rest = text.strip_prefix(CODEX_HOOK_PROMPT)?;
    let (attributes, _) = rest.split_once('>')?;
    (attributes.is_empty() || attributes.starts_with(char::is_whitespace)).then_some(attributes)
}

/// How the entries of the user start that Claude Code writes itself:
/// slash commands, skill bodies and notices.
const WRAPPER_PREFIXES: [&str; 6] = [
    "<command-",
    "<local-command",
    "Base directory for this skill",
    "<task-notification",
    "[Request interrupted",
    "<system-reminder",
];

fn str_field(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

/// A line of Claude Code. `None` for the lines of a subagent and for
/// lines that are neither of the user nor of the agent.
fn claude_line(line: &Value) -> Option<Entry> {
    let kind = match line.get("type").and_then(Value::as_str) {
        Some("user") => Kind::User,
        Some("assistant") => Kind::Assistant,
        _ => return None,
    };
    if line.get("isSidechain").and_then(Value::as_bool) == Some(true) {
        return None;
    }
    let mut texts: Vec<String> = Vec::new();
    let mut tools = Vec::new();
    let mut has_tool_result = false;
    match line
        .get("message")
        .and_then(|message| message.get("content"))
    {
        Some(Value::String(text)) => texts.push(text.clone()),
        Some(Value::Array(parts)) => {
            for part in parts {
                match part.get("type").and_then(Value::as_str) {
                    Some("text") => texts.push(str_field(part, "text")),
                    Some("tool_result") => has_tool_result = true,
                    Some("tool_use") => {
                        let input = part.get("input");
                        let field = |key: &str| {
                            input
                                .and_then(|input| input.get(key))
                                .and_then(Value::as_str)
                                .map(str::to_owned)
                        };
                        tools.push(ToolCall {
                            name: str_field(part, "name"),
                            file_paths: field("file_path")
                                .or_else(|| field("notebook_path"))
                                .into_iter()
                                .collect(),
                            command: field("command"),
                        });
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }
    Some(Entry {
        kind,
        id: str_field(line, "uuid"),
        turn: line
            .get("promptId")
            .and_then(Value::as_str)
            .map(str::to_owned),
        is_meta: line.get("isMeta").and_then(Value::as_bool) == Some(true),
        has_tool_result,
        text: texts.join("\n").trim().to_owned(),
        tools,
    })
}

/// The largest transcript the check reads. A larger one is an error, so
/// that a very long session does not slow every stop.
const MAX_TRANSCRIPT_BYTES: u64 = 256 * 1024 * 1024;

pub fn load_transcript(path: &Path) -> io::Result<Vec<Entry>> {
    let file = open_regular(path)?;
    if file.metadata()?.len() > MAX_TRANSCRIPT_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "the transcript is too large",
        ));
    }
    // The limit also holds if the file grows while it is read.
    let mut reader = BufReader::new(file.take(MAX_TRANSCRIPT_BYTES));
    let mut parser = Parser::default();
    let mut line = Vec::new();
    // Read as bytes: a line that is not text is skipped like one that
    // is not JSON, and an error ends the reading.
    while reader
        .read_until(b'\n', &mut line)
        .is_ok_and(|read| read > 0)
    {
        if let Ok(text) = std::str::from_utf8(&line) {
            parser.line(text);
        }
        line.clear();
    }
    Ok(parser.entries)
}

/// The entries of a transcript given as text. Lines that are not JSON
/// are skipped: the last line may be half written.
pub fn parse_transcript(text: &str) -> Vec<Entry> {
    let mut parser = Parser::default();
    for line in text.lines() {
        parser.line(line);
    }
    parser.entries
}

#[derive(Default)]
struct Parser {
    entries: Vec<Entry>,
    codex: codex::State,
}

impl Parser {
    fn line(&mut self, line: &str) {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            return;
        };
        let entry = if codex::is_codex_line(&value) {
            self.codex.line(&value)
        } else {
            claude_line(&value)
        };
        self.entries.extend(entry);
    }
}

/// The transcripts of Codex (`~/.codex/sessions/.../rollout-*.jsonl`).
/// Every line is `{timestamp, type, payload}`. The shapes are the ones
/// Codex 0.169 to 0.171 wrote.
///
/// - `turn_context` and `event_msg` of type `task_started` give the id
///   of the turn that the entries after them belong to.
/// - `response_item` whose payload is a `message` of the user or the
///   assistant becomes an entry; one of the developer is left out.
/// - `function_call` (`arguments`, JSON in a string) and
///   `custom_tool_call` (`input`, a string) become tool calls. A call of
///   the shell is named Bash here, as the hooks name it, and
///   `apply_patch` writes every file its patch names.
mod codex {
    use serde_json::Value;

    use super::{Entry, Kind, ToolCall, hook_prompt_attributes, str_field};

    const TYPES: [&str; 8] = [
        "session_meta",
        "turn_context",
        "response_item",
        "event_msg",
        "token_usage_record",
        "world_state",
        "inter_agent_communication_metadata",
        "compacted",
    ];

    pub(super) fn is_codex_line(line: &Value) -> bool {
        line.get("payload").is_some_and(Value::is_object)
            && line
                .get("type")
                .and_then(Value::as_str)
                .is_some_and(|kind| TYPES.contains(&kind))
    }

    #[derive(Default)]
    pub(super) struct State {
        turn: Option<String>,
        lines: usize,
    }

    fn texts(content: Option<&Value>) -> String {
        match content {
            Some(Value::String(text)) => text.trim().to_owned(),
            Some(Value::Array(parts)) => parts
                .iter()
                .filter_map(|part| part.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n")
                .trim()
                .to_owned(),
            _ => String::new(),
        }
    }

    /// The command of a call of the shell: a string, or an argv whose
    /// script is the last item when it starts with a shell
    /// (`["bash", "-lc", "..."]`).
    fn command_of(value: &Value) -> Option<String> {
        match value {
            Value::String(text) => Some(text.clone()),
            Value::Array(items) => {
                let argv: Vec<&str> = items.iter().filter_map(Value::as_str).collect();
                let shell = argv.first().is_some_and(|arg| {
                    matches!(arg.rsplit('/').next(), Some("bash" | "sh" | "zsh"))
                });
                if shell && argv.len() >= 3 && argv[argv.len() - 2].starts_with('-') {
                    argv.last().map(|script| script.to_string())
                } else {
                    Some(argv.join(" "))
                }
            }
            _ => None,
        }
    }

    /// The files a patch writes: the ones it adds, updates and moves
    /// to. The ones it deletes are no longer there.
    fn patch_files(patch: &str) -> Vec<String> {
        let mut files: Vec<String> = Vec::new();
        for line in patch.lines() {
            let path = ["*** Add File: ", "*** Update File: ", "*** Move to: "]
                .iter()
                .find_map(|prefix| line.trim_end().strip_prefix(prefix))
                .map(str::trim)
                .filter(|path| !path.is_empty());
            if let Some(path) = path
                && !files.iter().any(|file| file == path)
            {
                files.push(path.to_owned());
            }
        }
        files
    }

    fn tool_call(payload: &Value) -> ToolCall {
        let name = str_field(payload, "name");
        let raw = payload
            .get("arguments")
            .or_else(|| payload.get("input"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        let json = serde_json::from_str::<Value>(raw)
            .ok()
            .filter(Value::is_object);
        let field = |key: &str| json.as_ref().and_then(|json| json.get(key));
        if name == "apply_patch" {
            let patch = field("input")
                .or_else(|| field("patch"))
                .and_then(Value::as_str)
                .unwrap_or(raw);
            return ToolCall {
                name,
                file_paths: patch_files(patch),
                command: None,
            };
        }
        let command = field("cmd")
            .or_else(|| field("command"))
            .and_then(command_of);
        if matches!(
            name.as_str(),
            "exec" | "shell" | "exec_command" | "local_shell"
        ) {
            let command =
                command.or_else(|| (!raw.is_empty() && json.is_none()).then(|| raw.to_owned()));
            return ToolCall {
                name: "Bash".into(),
                file_paths: Vec::new(),
                command,
            };
        }
        ToolCall {
            name,
            file_paths: Vec::new(),
            command,
        }
    }

    impl State {
        pub(super) fn line(&mut self, line: &Value) -> Option<Entry> {
            self.lines += 1;
            let payload = line.get("payload")?;
            let payload_type = payload.get("type").and_then(Value::as_str);
            match line.get("type").and_then(Value::as_str)? {
                "turn_context" => self.set_turn(payload),
                "event_msg" if payload_type == Some("task_started") => self.set_turn(payload),
                "response_item" => return self.item(payload, payload_type?),
                _ => {}
            }
            None
        }

        fn set_turn(&mut self, payload: &Value) {
            let turn = payload.get("turn_id").and_then(Value::as_str);
            if let Some(turn) = turn.filter(|turn| !turn.is_empty()) {
                self.turn = Some(turn.to_owned());
            }
        }

        fn item(&self, payload: &Value, payload_type: &str) -> Option<Entry> {
            let mut entry = Entry {
                kind: Kind::Assistant,
                id: format!("line-{}", self.lines),
                turn: self.turn.clone(),
                is_meta: false,
                has_tool_result: false,
                text: String::new(),
                tools: Vec::new(),
            };
            match payload_type {
                "message" => {
                    entry.text = texts(payload.get("content"));
                    match payload.get("role").and_then(Value::as_str) {
                        Some("assistant") => {}
                        Some("user") => {
                            entry.kind = Kind::User;
                            // What a hook said, not what the user typed.
                            entry.is_meta = hook_prompt_attributes(&entry.text).is_some();
                        }
                        _ => return None,
                    }
                }
                "function_call" | "custom_tool_call" | "local_shell_call" => {
                    entry.tools.push(tool_call(payload));
                }
                "function_call_output" | "custom_tool_call_output" => {
                    entry.kind = Kind::User;
                    entry.has_tool_result = true;
                }
                _ => return None,
            }
            Some(entry)
        }
    }
}

/// A prompt typed or sent into the session, as opposed to the results
/// of tools and the entries the agent's program writes.
fn is_real_prompt(entry: &Entry) -> bool {
    entry.kind == Kind::User
        && !entry.is_meta
        && !entry.has_tool_result
        && !entry.text.is_empty()
        && !WRAPPER_PREFIXES
            .iter()
            .any(|prefix| entry.text.starts_with(prefix))
}

/// The index of the entry that starts the turn `turn`, or the last turn
/// when the hook named none or the transcript does not know the one it
/// named. What a hook said never starts a turn.
pub fn turn_start(entries: &[Entry], turn: Option<&str>) -> Option<usize> {
    if let Some(turn) = turn {
        let found = entries.iter().position(|entry| {
            entry.kind == Kind::User
                && !entry.is_meta
                && !entry.has_tool_result
                && entry.turn.as_deref() == Some(turn)
        });
        if found.is_some() {
            return found;
        }
    }
    entries.iter().rposition(is_real_prompt)
}

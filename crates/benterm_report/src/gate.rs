//! The decision at a Stop hook, and the hint after a tool call.

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::check::check_report;
use crate::facts::{collect_facts, needs_report};
use crate::files::{clip, read_regular, write_private};
use crate::report::Report;
use crate::rules::Rules;
use crate::transcript::{Entry, Kind, load_transcript, turn_start};

/// The most a report file or a count may hold. Real ones are a few
/// kilobytes.
const MAX_FILE_BYTES: u64 = 1024 * 1024;

/// How the reason of a block starts.
pub const BLOCK_PREFIX: &str = "The report does not follow the rules";

/// What the check reads of the input of a hook.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HookInput {
    /// The name of the hook's event, such as `Stop`.
    pub event: String,
    /// The subagent of the session the event comes from, if it comes
    /// from one.
    pub agent_id: Option<String>,
    pub session_id: String,
    pub transcript_path: Option<String>,
    /// The working directory of the agent.
    pub cwd: String,
    /// The turn: `prompt_id` of Claude Code, `turn_id` of Codex.
    pub turn: Option<String>,
    pub last_assistant_message: String,
}

impl HookInput {
    /// The input as the agent wrote it: one JSON object.
    pub fn from_json(text: &str) -> Result<Self, String> {
        let value: Value =
            serde_json::from_str(text).map_err(|error| format!("hook input: {error}"))?;
        if !value.is_object() {
            return Err("hook input: not a JSON object".into());
        }
        let field = |key: &str| {
            value
                .get(key)
                .and_then(Value::as_str)
                .filter(|text| !text.is_empty())
                .map(str::to_owned)
        };
        Ok(Self {
            event: field("hook_event_name").unwrap_or_default(),
            agent_id: field("agent_id"),
            session_id: field("session_id").unwrap_or_default(),
            transcript_path: field("transcript_path"),
            cwd: field("cwd").unwrap_or_default(),
            turn: field("prompt_id").or_else(|| field("turn_id")),
            last_assistant_message: field("last_assistant_message").unwrap_or_default(),
        })
    }
}

/// What the check decided for a stop.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// The turn needs no report: the agent stops and nothing is said.
    Quiet,
    /// The agent is sent back with what to fix.
    Block { reason: String },
    /// The agent stops. Without problems the report passed. With
    /// problems the agent was sent back as often as the rules allow,
    /// and the problems are what is still wrong; the report is there
    /// if one could be read.
    Stop {
        report: Option<Report>,
        problems: Vec<String>,
    },
}

/// The count of the blocks of one turn. Another turn starts it again.
#[derive(Serialize, Deserialize, Default)]
struct State {
    turn_key: String,
    rounds: u32,
    /// Whether the agent was told once how to write the report.
    #[serde(default)]
    taught: bool,
}

/// The file of a session in the folder of the counts. Only letters,
/// digits, `-` and `_` of the session's id are kept, so that an id
/// cannot name a file elsewhere.
fn session_file(state_dir: &Path, session_id: &str, extension: &str) -> PathBuf {
    let safe: String = session_id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    let name = if safe.is_empty() { "no-session" } else { &safe };
    state_dir.join(format!("{name}.{extension}"))
}

/// The text of `path`, or `None` when there is no such file. Any other
/// failure is an error: it must not read as "nothing counted yet".
fn read_count(path: &Path) -> Result<Option<String>, String> {
    match read_regular(path, MAX_FILE_BYTES) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("cannot read the count: {error}")),
    }
}

/// Which turns need a report, told with every block and hint, so that
/// the agent does not write one after every small piece of work.
fn when_to_report(rules: &Rules) -> String {
    format!(
        "Write a report only for a turn that meets one of these. In other turns, do not write \
         the report JSON unless the user asks for it.\n\
         - It edited {} or more files.\n\
         - It made {} or more tool calls.",
        rules.edited_files_threshold, rules.tool_call_threshold
    )
}

/// How to write a report, with the names and the lengths of `rules`.
pub fn report_format(rules: &Rules) -> String {
    include_str!("report-format.md")
        .replace("{dir}", rules.report_dir.trim_matches('/'))
        .replace("{suffix}", &rules.report_suffix)
        .replace("{summary_max}", &rules.summary_max_chars.to_string())
        .replace("{question_max}", &rules.question_max_chars.to_string())
        .replace("{reply_max}", &rules.reply_max_chars.to_string())
}

/// The turn of the input in its transcript.
struct Turn {
    entries: Vec<Entry>,
    start: usize,
    /// Names the turn among the turns of the session: the id the hook
    /// gave, or else the id of the entry the turn starts with.
    key: String,
}

fn read_turn(input: &HookInput) -> Result<Turn, String> {
    let path = input
        .transcript_path
        .as_deref()
        .ok_or("no transcript_path")?;
    let entries = load_transcript(Path::new(path))
        .map_err(|error| format!("cannot read the transcript: {error}"))?;
    let start =
        turn_start(&entries, input.turn.as_deref()).ok_or("no prompt found in the transcript")?;
    let key = input
        .turn
        .clone()
        .unwrap_or_else(|| entries[start].id.clone());
    Ok(Turn {
        entries,
        start,
        key,
    })
}

/// Resolves links and `..` first, so that only a regular file with the
/// suffix of a report, directly in a folder named as the reports' folder
/// is, is read as a report.
fn read_report(raw: &str, rules: &Rules) -> Result<Report, String> {
    let shown = clip(raw, 200);
    let path = std::fs::canonicalize(raw).map_err(|error| format!("{shown}: {error}"))?;
    let folder = path.parent().and_then(Path::file_name);
    let dir = rules.report_dir.trim_matches('/');
    if folder.and_then(|name| name.to_str()) != Some(dir) {
        return Err(format!("{shown} is not directly in {dir}/"));
    }
    let named = path
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_suffix(&rules.report_suffix))
        .is_some_and(|stem| !stem.is_empty());
    if !named {
        return Err(format!(
            "the name of {shown} does not end with {}",
            rules.report_suffix
        ));
    }
    let text = read_regular(&path, MAX_FILE_BYTES).map_err(|error| error.to_string())?;
    serde_json::from_str(&text).map_err(|error| error.to_string())
}

/// Decides a stop: whether the turn needs a report, whether its report
/// passes, and whether the agent is sent back once more.
///
/// The first stop of a turn without a report only teaches the form and
/// is not counted. After it the agent is sent back at most
/// `rules.max_rounds` times; the count is kept in `state_dir`, a file
/// for each session.
///
/// An error is a failure of the check itself, such as a transcript it
/// cannot read. The caller lets the agent stop then: a check that does
/// not work must not hold an agent.
pub fn check_stop(input: &HookInput, rules: &Rules, state_dir: &Path) -> Result<Verdict, String> {
    let turn = read_turn(input)?;
    let entries = &turn.entries[turn.start..];
    let facts = collect_facts(&turn.entries, turn.start, &input.cwd, rules);
    if !needs_report(&facts, rules) {
        return Ok(Verdict::Quiet);
    }

    let reply = match input.last_assistant_message.trim() {
        "" => entries
            .iter()
            .rev()
            .find(|entry| entry.kind == Kind::Assistant && !entry.text.is_empty())
            .map(|entry| entry.text.clone())
            .unwrap_or_default(),
        message => message.to_owned(),
    };

    // A report that is missing, misplaced or unreadable is the agent's
    // to fix, not a failure of the check.
    let mut report = None;
    let problems = match &facts.report_file {
        None => vec![format!(
            "No report JSON was written in this turn. Write it to {}/<task-name>{}.",
            rules.report_dir.trim_matches('/'),
            rules.report_suffix
        )],
        Some(raw) => match read_report(raw, rules) {
            Err(error) => vec![format!("Cannot read the report JSON: {error}")],
            Ok(read) => {
                let found = check_report(&read, &reply, &facts, &input.cwd, rules);
                report = Some(read);
                found
            }
        },
    };
    if problems.is_empty() {
        return Ok(Verdict::Stop { report, problems });
    }
    // A problem can quote the report, so nothing in it may forge a line
    // where it is shown.
    let problems: Vec<String> = problems.iter().map(|problem| clip(problem, 300)).collect();

    let path = session_file(state_dir, &input.session_id, "json");
    let mut state: State = match read_count(&path)? {
        Some(text) => serde_json::from_str(&text)
            .map_err(|error| format!("cannot read the count: {error}"))?,
        None => State::default(),
    };
    if state.turn_key != turn.key {
        state = State {
            turn_key: turn.key,
            ..State::default()
        };
    }

    let teaching = facts.report_file.is_none() && !state.taught;
    // A second limit that does not need the count: the agent's program
    // records every block of a Stop hook in the transcript.
    let recorded = entries
        .iter()
        .filter(|entry| entry.is_stop_feedback())
        .count();
    let within_limit = recorded <= rules.max_rounds as usize;
    if !within_limit || !(teaching || state.rounds < rules.max_rounds) {
        return Ok(Verdict::Stop { report, problems });
    }
    if teaching {
        state.taught = true;
    } else {
        state.rounds += 1;
    }
    // Without a saved count the limit cannot be kept, so a count that
    // cannot be saved is an error and the agent stops.
    let text = serde_json::to_string(&state).map_err(|error| error.to_string())?;
    write_private(&path, text.as_bytes())
        .map_err(|error| format!("cannot save the count: {error}"))?;

    let list: Vec<String> = problems
        .iter()
        .map(|problem| format!("- {problem}"))
        .collect();
    let reason = format!(
        "{BLOCK_PREFIX}. Fix these points.\n\n{}\n\n{}\n\n{}",
        list.join("\n"),
        when_to_report(rules),
        report_format(rules)
    );
    Ok(Verdict::Block { reason })
}

/// What to tell the agent after a tool call: how to write the report,
/// once a turn, as soon as the turn comes to need one. `None` when
/// there is nothing to tell.
///
/// The count of the Stop is left alone: a stop without a report is
/// still sent back.
pub fn hint(input: &HookInput, rules: &Rules, state_dir: &Path) -> Result<Option<String>, String> {
    let path = session_file(state_dir, &input.session_id, "hint");
    let handed = read_count(&path)?;
    let handed_for = |key: &str| handed.as_deref().is_some_and(|last| last.trim() == key);
    // Told before the transcript is read: after the hint, the tool
    // calls of the turn cost no reading.
    if input.turn.as_deref().is_some_and(handed_for) {
        return Ok(None);
    }
    let turn = read_turn(input)?;
    let facts = collect_facts(&turn.entries, turn.start, &input.cwd, rules);
    // A report that is written needs no hint.
    if facts.report_file.is_some() || !needs_report(&facts, rules) {
        return Ok(None);
    }
    if handed_for(&turn.key) {
        return Ok(None);
    }
    // Saved first: a hint that cannot be remembered would come again
    // after every tool call.
    write_private(&path, turn.key.as_bytes())
        .map_err(|error| format!("cannot save the hint: {error}"))?;
    Ok(Some(format!(
        "This turn has met the report conditions, so write the report JSON before you stop. If \
         you stop without it, the Stop hook sends you back.\n\n{}\n\n{}",
        when_to_report(rules),
        report_format(rules)
    )))
}

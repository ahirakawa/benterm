//! What a turn did, as its transcript tells it, whatever the report
//! says.

use std::path::{Path, PathBuf};

use crate::rules::Rules;
use crate::transcript::{Entry, Kind};

/// What the transcript says happened in a turn.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TurnFacts {
    /// The files edited in the turn that are still there and are not
    /// ignored, as absolute paths, in the order of their first edit.
    pub edited_files: Vec<String>,
    /// The commands the turn ran in the shell.
    pub commands: Vec<String>,
    pub tool_calls: usize,
    /// The report the turn wrote last, if it wrote one.
    pub report_file: Option<String>,
}

/// `path` as an absolute path: from the home directory after `~/`, and
/// from `cwd` when it is relative.
pub(crate) fn absolute(path: &str, cwd: &str) -> String {
    let joined = if let Some(rest) = path.strip_prefix("~/") {
        PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(rest)
    } else if Path::new(path).is_absolute() {
        return path.to_owned();
    } else {
        Path::new(cwd).join(path)
    };
    joined.to_string_lossy().into_owned()
}

/// Whether `pattern` leaves `path` out. A pattern that starts with `/`
/// names a folder from the root, also behind macOS's `/private`; any
/// other is looked for anywhere in the path.
fn is_ignored(path: &str, pattern: &str) -> bool {
    if pattern.starts_with('/') {
        path.strip_prefix("/private")
            .is_some_and(|rest| rest.starts_with(pattern))
            || path.starts_with(pattern)
    } else {
        path.contains(pattern)
    }
}

/// What the entries from `start` on did. `cwd` is the working directory
/// of the agent, which relative paths are taken from.
pub fn collect_facts(entries: &[Entry], start: usize, cwd: &str, rules: &Rules) -> TurnFacts {
    let mut facts = TurnFacts::default();
    let report_dir = format!("/{}/", rules.report_dir.trim_matches('/'));
    let turn = entries.get(start..).unwrap_or_default();
    for entry in turn.iter().filter(|entry| entry.kind == Kind::Assistant) {
        for tool in &entry.tools {
            facts.tool_calls += 1;
            facts.commands.extend(tool.command.clone());
            if !rules.edit_tools.contains(&tool.name) {
                continue;
            }
            for path in &tool.file_paths {
                let path = absolute(path, cwd);
                if path.contains(&report_dir) && path.ends_with(&rules.report_suffix) {
                    facts.report_file = Some(path);
                    continue;
                }
                let ignored = rules
                    .ignore_path_patterns
                    .iter()
                    .any(|pattern| is_ignored(&path, pattern));
                if !ignored && Path::new(&path).exists() && !facts.edited_files.contains(&path) {
                    facts.edited_files.push(path);
                }
            }
        }
    }
    facts
}

/// Whether the turn needs a report. A small turn is left alone: a
/// report for a change of one line costs the user more than it tells.
/// A turn that wrote a report has it checked whatever its size.
pub fn needs_report(facts: &TurnFacts, rules: &Rules) -> bool {
    facts.edited_files.len() >= rules.edited_files_threshold
        || facts.tool_calls >= rules.tool_call_threshold
        || facts.report_file.is_some()
}

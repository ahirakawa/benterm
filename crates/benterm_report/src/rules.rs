//! What the check holds a turn and its report against.

/// The most rounds a setting can ask for: nothing a file says may send
/// an agent back without end.
pub const MAX_ROUNDS_LIMIT: u32 = 5;

/// Words that claim a check, and the commands one of which the turn has
/// to have run for the claim to stand.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerificationRule {
    pub words: Vec<String>,
    pub commands: Vec<String>,
}

/// The thresholds, the limits and the lists of the check. The numbers
/// come from the settings file; the lists are the same for everyone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rules {
    /// A turn needs a report when it edits at least this many files,
    pub edited_files_threshold: usize,
    /// or calls its tools at least this many times.
    pub tool_call_threshold: usize,
    /// How many times a turn is sent back after the form was taught.
    pub max_rounds: u32,
    pub summary_max_chars: usize,
    /// A question is shown in a row of the session list, so it is short.
    pub question_max_chars: usize,
    /// The reply in the terminal, which the report stands in for.
    pub reply_max_chars: usize,
    /// The folder of a working directory the reports are written to.
    pub report_dir: String,
    /// What the name of a report file ends with.
    pub report_suffix: String,
    /// The extensions of files whose changes a report tells place by
    /// place.
    pub code_extensions: Vec<String>,
    /// A file whose path holds one of these does not count as edited.
    /// One that starts with `/` names a folder from the root.
    pub ignore_path_patterns: Vec<String>,
    /// The tools that write files.
    pub edit_tools: Vec<String>,
    pub verification_rules: Vec<VerificationRule>,
}

fn strings(words: &[&str]) -> Vec<String> {
    words.iter().map(|word| word.to_string()).collect()
}

impl Default for Rules {
    fn default() -> Self {
        let rule = |words: &[&str], commands: &[&str]| VerificationRule {
            words: strings(words),
            commands: strings(commands),
        };
        Self {
            edited_files_threshold: 3,
            tool_call_threshold: 20,
            max_rounds: 2,
            summary_max_chars: 200,
            question_max_chars: 40,
            reply_max_chars: 400,
            report_dir: ".benterm".into(),
            report_suffix: ".report.json".into(),
            code_extensions: strings(&[
                "rs", "py", "ts", "tsx", "js", "jsx", "go", "java", "kt", "swift", "c", "h", "cpp",
                "rb", "php", "sh", "sql",
            ]),
            ignore_path_patterns: strings(&["/tmp/", "/var/folders/", ".claude/", ".benterm/"]),
            edit_tools: strings(&["Edit", "Write", "NotebookEdit", "apply_patch"]),
            verification_rules: vec![
                rule(&["テスト", "test"], &["test"]),
                rule(
                    &["ビルド", "build"],
                    &["build", "cargo check", "tsc", "make"],
                ),
                rule(&["push"], &["git push"]),
            ],
        }
    }
}

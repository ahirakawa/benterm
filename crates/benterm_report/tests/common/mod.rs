//! Made-up transcripts and reports for the tests. Nothing here is taken
//! from a real conversation.
#![allow(dead_code)]

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

/// A made-up transcript of Claude Code, line by line.
#[derive(Default)]
pub struct Claude {
    lines: Vec<Value>,
}

impl Claude {
    pub fn prompt(mut self, prompt_id: &str, text: &str) -> Self {
        self.lines.push(json!({
            "type": "user", "uuid": format!("u{}", self.lines.len()), "promptId": prompt_id,
            "message": {"role": "user", "content": text},
        }));
        self
    }

    fn tool(mut self, name: &str, input: Value) -> Self {
        self.lines.push(json!({
            "type": "assistant", "uuid": format!("a{}", self.lines.len()),
            "message": {"role": "assistant", "content": [{"type": "tool_use", "name": name, "input": input}]},
        }));
        self.lines.push(json!({
            "type": "user", "uuid": format!("r{}", self.lines.len()),
            "message": {"role": "user", "content": [{"type": "tool_result", "content": "ok"}]},
        }));
        self
    }

    pub fn edit(self, path: &Path) -> Self {
        self.tool("Edit", json!({"file_path": path}))
    }

    pub fn write(self, path: &Path) -> Self {
        self.tool("Write", json!({"file_path": path}))
    }

    pub fn bash(self, command: &str) -> Self {
        self.tool("Bash", json!({"command": command}))
    }

    pub fn read(self, path: &str) -> Self {
        self.tool("Read", json!({"file_path": path}))
    }

    pub fn says(mut self, text: &str) -> Self {
        self.lines.push(json!({
            "type": "assistant", "uuid": format!("a{}", self.lines.len()),
            "message": {"role": "assistant", "content": [{"type": "text", "text": text}]},
        }));
        self
    }

    /// What Claude Code records when a Stop hook sends the agent back.
    pub fn stop_feedback(mut self, prompt_id: &str, reason: &str) -> Self {
        self.lines.push(json!({
            "type": "user", "uuid": format!("f{}", self.lines.len()), "promptId": prompt_id,
            "isMeta": true,
            "message": {"role": "user", "content": format!("Stop hook feedback:\n{reason}")},
        }));
        self
    }

    pub fn sidechain_edit(mut self, path: &Path) -> Self {
        self.lines.push(json!({
            "type": "assistant", "uuid": "s", "isSidechain": true,
            "message": {"content": [{"type": "tool_use", "name": "Edit", "input": {"file_path": path}}]},
        }));
        self
    }

    pub fn text(&self) -> String {
        lines(&self.lines)
    }
}

/// A made-up transcript of Codex, line by line.
#[derive(Default)]
pub struct Codex {
    lines: Vec<Value>,
}

impl Codex {
    fn item(mut self, payload: Value) -> Self {
        self.lines.push(json!({
            "timestamp": "2026-01-01T00:00:00Z", "ordinal": self.lines.len(),
            "type": "response_item", "payload": payload,
        }));
        self
    }

    pub fn session(mut self, cwd: &Path) -> Self {
        self.lines.push(json!({
            "type": "session_meta", "payload": {"id": "made-up-session", "cwd": cwd},
        }));
        self
    }

    pub fn turn(mut self, turn_id: &str, prompt: &str) -> Self {
        self.lines.push(json!({
            "type": "event_msg", "payload": {"type": "task_started", "turn_id": turn_id},
        }));
        self.item(json!({
            "type": "message", "role": "user",
            "content": [{"type": "input_text", "text": prompt}],
        }))
    }

    pub fn patch(self, files: &[&str]) -> Self {
        let mut patch = String::from("*** Begin Patch\n");
        for file in files {
            patch.push_str(&format!("*** Update File: {file}\n@@\n-a\n+b\n"));
        }
        patch.push_str("*** End Patch\n");
        self.item(json!({"type": "custom_tool_call", "name": "apply_patch", "input": patch}))
    }

    pub fn exec(self, command: &str) -> Self {
        self.item(json!({
            "type": "function_call", "name": "exec_command",
            "arguments": json!({"cmd": command}).to_string(),
        }))
    }

    pub fn says(self, text: &str) -> Self {
        self.item(json!({
            "type": "message", "role": "assistant",
            "content": [{"type": "output_text", "text": text}],
        }))
    }

    /// What Codex records when a Stop hook sends the agent back: a
    /// message of the user, wrapped in a tag that names the hook.
    pub fn stop_feedback(self, reason: &str) -> Self {
        self.item(json!({
            "type": "message", "role": "user",
            "content": [{
                "type": "input_text",
                "text": format!("<hook_prompt hook_run_id=\"stop:1:config.toml\">{reason}</hook_prompt>"),
            }],
        }))
    }

    pub fn text(&self) -> String {
        lines(&self.lines)
    }
}

fn lines(values: &[Value]) -> String {
    values.iter().map(|line| format!("{line}\n")).collect()
}

/// A folder of its own for a test. Not the system's folder for
/// temporary files: a file there does not count as edited.
pub fn folder() -> tempfile::TempDir {
    let root = Path::new(env!("CARGO_TARGET_TMPDIR"));
    let real = root.canonicalize().unwrap();
    let real = real.to_string_lossy();
    let scratch = [
        "/tmp/",
        "/private/tmp/",
        "/var/folders/",
        "/private/var/folders/",
    ];
    assert!(
        !scratch.iter().any(|prefix| real.starts_with(prefix)),
        "these tests edit made-up files below the build directory, and files in the \
         system's temporary folders do not count as edited: build somewhere else than {real}"
    );
    tempfile::tempdir_in(root).unwrap()
}

/// Makes the files, empty, with the folders above them.
pub fn touch(root: &Path, names: &[&str]) -> Vec<PathBuf> {
    names
        .iter()
        .map(|name| {
            let path = root.join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, "").unwrap();
            path
        })
        .collect()
}

/// A report that passes for a turn that edited `src/a.rs` and
/// `docs/plan.md`.
pub fn good_report() -> Value {
    json!({
        "title": "Made-up work",
        "goal": "To have a made-up thing",
        "conclusion": "pass",
        "summary": "The made-up thing is there.",
        "decisions": [{
            "question": "Keep the old one?",
            "options": [
                {"label": "Keep", "effect": "Both stay."},
                {"label": "Drop", "effect": "Only the new one stays."},
            ],
            "recommend": "Drop",
            "reason": "Nothing uses the old one.",
        }],
        "verified": ["Read the output by eye"],
        "unverified": ["Other platforms"],
        "deliverables": [
            {"path": "src/a.rs", "summary": "The thing itself.",
             "changes": [{"line": 3, "before": "none (new file)", "after": "Makes the thing."}]},
            {"path": "docs/plan.md", "summary": "The plan."},
        ],
    })
}

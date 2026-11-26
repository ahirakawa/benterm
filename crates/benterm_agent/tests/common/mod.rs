#![allow(dead_code)]

use std::fs::File;
use std::io::BufReader;
use std::path::PathBuf;

use benterm_agent::{
    AgentDefinition, AgentState, HookRule, Matcher, Region, ScreenRule, WaitReason, builtin,
};
use benterm_core::{AlacrittyCore, GridSize, Snapshot, TerminalCore};
use benterm_log::{LogReader, Record, replay};

pub fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(format!("{name}.bentermlog"))
}

/// The screen at the end of a recorded log.
pub fn final_screen(name: &str) -> Snapshot {
    let log = LogReader::new(BufReader::new(File::open(fixture_path(name)).unwrap())).unwrap();
    let mut core = AlacrittyCore::new(log.initial_size(), 10_000);
    replay(log, &mut core).unwrap();
    core.snapshot()
}

/// The screen after each record of a recorded log, in order.
pub fn screens(name: &str) -> Vec<Snapshot> {
    let log = LogReader::new(BufReader::new(File::open(fixture_path(name)).unwrap())).unwrap();
    let mut core = AlacrittyCore::new(log.initial_size(), 10_000);
    let mut screens = Vec::new();
    for record in log {
        match record.unwrap() {
            Record::Output(bytes) => core.feed(&bytes),
            Record::Resize(size) => core.resize(size),
        }
        screens.push(core.snapshot());
    }
    screens
}

/// A 10 x 40 screen showing `lines` from the top.
pub fn screen_of(lines: &[&str]) -> Snapshot {
    let mut core = AlacrittyCore::new(GridSize::new(10, 40), 0);
    core.feed(lines.join("\r\n").as_bytes());
    core.snapshot()
}

/// A made-up agent, defined the same way as the built-in ones.
pub fn toy() -> AgentDefinition {
    AgentDefinition {
        id: "toy",
        name: "Toy Agent",
        commands: &["toy"],
        banner: Matcher::Contains("TOY AGENT"),
        screen_rules: vec![
            ScreenRule {
                state: AgentState::Waiting(WaitReason::Permission),
                region: Region::LastLines(2),
                matcher: Matcher::Contains("allow? [y/n]"),
            },
            ScreenRule {
                state: AgentState::Working,
                region: Region::LastLines(2),
                matcher: Matcher::Contains("thinking"),
            },
            ScreenRule {
                state: AgentState::Done,
                region: Region::LastLines(1),
                matcher: Matcher::LineStartsWith("toy>"),
            },
        ],
        hook_rules: vec![
            HookRule::new("start", AgentState::Working),
            HookRule::new("ask", AgentState::Waiting(WaitReason::Question)),
            HookRule::new("end", AgentState::Done),
            HookRule::new("fail", AgentState::Error),
            HookRule::new("bye", AgentState::Unknown),
        ],
        session_start: None,
    }
}

/// The built-in definitions and the made-up one.
pub fn definitions() -> Vec<AgentDefinition> {
    let mut definitions = builtin();
    definitions.push(toy());
    definitions
}

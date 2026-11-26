//! The order in which Claude Code and Codex send their OSC 7501 reports
//! and their hooks, as recorded from the real agents.
//!
//! Each `.order` file in `tests/fixtures` is one session, recorded on a
//! PTY that answered the query `OSC 7501 ; ?` as Benterm does. It holds
//! what arrived, in the order it arrived, and nothing of the screen:
//!
//! ```text
//! osc <report>           the text of an OSC 7501 report, without `msg`
//! hook <event> [tool=<name>] [type=<notification type>] [session=other]
//! is <state>             not recorded: the state the pane has by then
//! work <count>           not recorded: how often work has begun by then
//! ```
//!
//! The `msg` of a report can quote the user's prompt, so it is left out,
//! and the id Claude Code made up for its background command is written
//! `bg1`. A hook of the session that sent the first one names no
//! session; `session=other` is any other. Times are left out: only the
//! order counts here. The lines `is` and `work`, the comments and the
//! blank lines were written by hand.

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use benterm_agent::{AgentState, Detector, HookEvent, WaitReason, builtin};
use benterm_core::{
    AlacrittyCore, GridSize, ProgramRecord, ProgramState, TerminalCore, TerminalEvent,
};

const SESSIONS: &[(&str, &str)] = &[("claude", "session"), ("codex", "session")];

fn path(agent: &str, name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(agent)
        .join(format!("{name}.order"))
}

/// What the terminal holds of the program itself, numbered as
/// `benterm_terminal` numbers it.
#[derive(Default)]
struct Records {
    root: Option<ProgramRecord>,
    serial: u64,
    work: u64,
}

impl Records {
    fn take(&mut self, event: TerminalEvent) {
        match event {
            TerminalEvent::ProgramStatus(status) => {
                self.serial += 1;
                if status.id.is_some() {
                    return;
                }
                let working = |record: &ProgramRecord| record.status.state == ProgramState::Working;
                let began = status.state == ProgramState::Working
                    && !self.root.as_ref().is_some_and(working);
                self.work += u64::from(began);
                self.root = Some(ProgramRecord {
                    status,
                    serial: self.serial,
                    work: self.work,
                });
            }
            TerminalEvent::ProgramStatusCleared { id: None } => self.root = None,
            _ => {}
        }
    }
}

fn state_named(name: &str) -> AgentState {
    match name {
        "working" => AgentState::Working,
        "done" => AgentState::Done,
        "permission" => AgentState::Waiting(WaitReason::Permission),
        "question" => AgentState::Waiting(WaitReason::Question),
        other => panic!("unknown state {other}"),
    }
}

fn hook(agent: &str, words: &str) -> HookEvent {
    let mut words = words.split(' ');
    let mut event = HookEvent::new(agent, words.next().unwrap());
    let mut session = "own";
    for word in words {
        match word.split_once('=') {
            Some(("tool", tool)) => event = event.with_tool(tool),
            Some(("type", kind)) => event = event.with_notification_type(kind),
            Some(("session", name)) => session = name,
            _ => panic!("unknown word {word}"),
        }
    }
    event.with_session(session)
}

/// Plays a session to a detector with the agent in front and checks
/// every `is` and `work` line. With `look_at_once`, the detector is shown each report as
/// it comes; without, only when a hook comes or the state is asked, as
/// when the view was not woken in between.
fn play(agent: &str, name: &str, look_at_once: bool) {
    let text = fs::read_to_string(path(agent, name)).unwrap();
    let mut core = AlacrittyCore::new(GridSize::new(4, 20), 0);
    let mut records = Records::default();
    let mut detector = Detector::new(builtin());
    detector.observe_front_process(100, &[agent.to_owned()]);
    let now = Instant::now();
    let mut last = "";
    for (number, line) in text.lines().enumerate() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let at = format!("{agent}/{name}:{}: {line}", number + 1);
        let (kind, rest) = line.split_once(' ').expect(&at);
        match kind {
            "osc" => {
                core.feed(format!("\x1b]7501;{rest}\x1b\\").as_bytes());
                let events = core.take_events();
                assert_eq!(events.len(), 1, "{at}");
                events.into_iter().for_each(|event| records.take(event));
                if look_at_once {
                    detector.observe_program(records.root.as_ref());
                }
            }
            "hook" => {
                detector.observe_program(records.root.as_ref());
                detector.observe_hook(&hook(agent, rest), now);
            }
            "is" => {
                detector.observe_program(records.root.as_ref());
                assert_eq!(detector.state(), state_named(rest), "{at}");
            }
            "work" => {
                detector.observe_program(records.root.as_ref());
                assert_eq!(detector.work_begun(), rest.parse::<u64>().unwrap(), "{at}");
            }
            _ => panic!("{at}"),
        }
        last = kind;
    }
    assert!(
        matches!(last, "is" | "work"),
        "{agent}/{name} ends with the state it reaches"
    );
}

#[test]
fn each_recorded_order_gives_its_states() {
    for (agent, name) in SESSIONS {
        play(agent, name, true);
    }
}

#[test]
fn each_recorded_order_gives_the_same_states_when_reports_are_seen_late() {
    for (agent, name) in SESSIONS {
        play(agent, name, false);
    }
}

#[test]
fn every_order_in_the_fixtures_is_played() {
    let mut found = Vec::new();
    for agent in ["claude", "codex"] {
        for entry in fs::read_dir(path(agent, "x").parent().unwrap()).unwrap() {
            let file = entry.unwrap().path();
            if file.extension().is_some_and(|ext| ext == "order") {
                let name = file.file_stem().unwrap().to_str().unwrap().to_owned();
                found.push((agent.to_owned(), name));
            }
        }
    }
    found.sort();
    let mut played: Vec<(String, String)> = SESSIONS
        .iter()
        .map(|(agent, name)| ((*agent).to_owned(), (*name).to_owned()))
        .collect();
    played.sort();
    assert_eq!(found, played);
}

//! Turns shorter than the settle time.
//!
//! A program that tells its state with OSC 7501 is working from the
//! moment it says so. The terminal keeps only its last report, so a turn
//! may begin and end between two looks: the detector then counts the
//! work that began, though the state it shows never was working. A turn
//! that only the screen tells has to be on the screen for the settle
//! time, and a shorter one is not seen at all.

mod common;

use std::time::{Duration, Instant};

use benterm_agent::{AgentState, Detector, HookEvent, builtin};
use benterm_core::{BlockKind, ProgramRecord, ProgramState, ProgramStatus};
use common::{screen_of, toy};

fn status(state: ProgramState) -> ProgramStatus {
    ProgramStatus {
        state,
        id: None,
        kind: None,
        progress: None,
        app: None,
        title: None,
        msg: None,
    }
}

/// A detector with `command` in front, and what the terminal holds of
/// the program itself, numbered as the terminal numbers it.
struct Pane {
    detector: Detector,
    record: Option<ProgramRecord>,
    serial: u64,
    work: u64,
}

impl Pane {
    fn with(command: &str) -> Self {
        let mut definitions = builtin();
        definitions.push(toy());
        let mut detector = Detector::new(definitions);
        detector.observe_front_process(100, &[command.to_owned()]);
        Self {
            detector,
            record: None,
            serial: 0,
            work: 0,
        }
    }

    /// The program tells `state`, and the terminal keeps it. Work begins
    /// when the program was not working before.
    fn tell_unseen(&mut self, state: ProgramState) {
        let working = |record: &ProgramRecord| record.status.state == ProgramState::Working;
        let began = state == ProgramState::Working && !self.record.as_ref().is_some_and(working);
        self.serial += 1;
        self.work += u64::from(began);
        self.record = Some(ProgramRecord {
            status: status(state),
            serial: self.serial,
            work: self.work,
        });
    }

    /// The detector is shown what the terminal holds.
    fn look(&mut self) {
        self.detector.observe_program(self.record.as_ref());
    }
}

/// No look at the screen and no time passes: the report alone is the
/// change.
#[test]
fn working_told_with_osc_7501_is_working_at_once() {
    for command in ["claude", "codex", "terraform"] {
        let mut pane = Pane::with(command);
        pane.tell_unseen(ProgramState::Working);
        pane.look();
        assert_eq!(pane.detector.state(), AgentState::Working, "{command}");
        assert_eq!(pane.detector.pending_deadline(), None, "{command}");
        assert_eq!(pane.detector.work_begun(), 1, "{command}");
    }
}

/// Working and done came in one write, or the view was woken once for
/// both: the state shown goes from done to done.
#[test]
fn work_that_began_and_ended_between_two_looks_is_counted() {
    for command in ["claude", "codex", "terraform"] {
        let mut pane = Pane::with(command);
        pane.tell_unseen(ProgramState::Idle);
        pane.look();
        assert_eq!(pane.detector.work_begun(), 0, "{command}");

        pane.tell_unseen(ProgramState::Working);
        pane.tell_unseen(ProgramState::Done);
        pane.look();

        assert_eq!(pane.detector.state(), AgentState::Done, "{command}");
        assert_eq!(pane.detector.work_begun(), 1, "{command}");
    }
}

#[test]
fn only_reports_of_work_count() {
    let mut pane = Pane::with("claude");
    pane.tell_unseen(ProgramState::Working);
    pane.look();
    // The same record seen again.
    pane.look();
    assert_eq!(pane.detector.work_begun(), 1);
    // Done, and done again, with no work told in between.
    pane.tell_unseen(ProgramState::Done);
    pane.look();
    pane.tell_unseen(ProgramState::Done);
    pane.look();
    // The record goes, as when a prompt starts.
    pane.record = None;
    pane.look();
    assert_eq!(pane.detector.work_begun(), 1);
}

/// The limit with neither hooks nor OSC 7501: a screen at work for less
/// than the settle time is no turn.
#[test]
fn a_turn_only_the_screen_tells_is_not_seen_when_it_is_shorter_than_the_settle_time() {
    let mut pane = Pane::with("toy");
    let t = Instant::now();
    let done = screen_of(&["toy>"]);
    pane.detector.observe_screen(&done, t);
    pane.detector
        .observe_screen(&done, t + Detector::DEFAULT_SETTLE);
    assert_eq!(pane.detector.state(), AgentState::Done);

    let start = t + Detector::DEFAULT_SETTLE * 2;
    pane.detector
        .observe_screen(&screen_of(&["thinking"]), start);
    let end = start + Duration::from_millis(100);
    pane.detector.observe_screen(&done, end);
    pane.detector
        .observe_screen(&done, end + Detector::DEFAULT_SETTLE);

    assert_eq!(pane.detector.state(), AgentState::Done);
    assert_eq!(pane.detector.work_begun(), 0);
    assert_eq!(pane.detector.turns_begun(), 0);
}

#[test]
fn turns_begun_counts_new_work_accepted_hooks_and_active_transitions_only() {
    let mut pane = Pane::with("claude");
    let now = Instant::now();

    // Pre-send working record observed once, then shown again: counts once.
    pane.tell_unseen(ProgramState::Working);
    pane.look();
    let base = pane.detector.turns_begun();
    assert_eq!(base, 1);
    pane.look();
    assert_eq!(pane.detector.turns_begun(), base);

    // Working repeated while still working is the same work.
    pane.tell_unseen(ProgramState::Working);
    pane.look();
    assert_eq!(pane.detector.turns_begun(), base);

    // An accepted active hook counts even when already Working.
    pane.detector.observe_hook(
        &HookEvent::new("claude", "UserPromptSubmit").with_session("a"),
        now,
    );
    assert_eq!(pane.detector.turns_begun(), base + 1);

    // Ignored hooks (subagent, other session during turn) and Stop do not count.
    let after_prompt = pane.detector.turns_begun();
    pane.detector.observe_hook(
        &HookEvent::new("claude", "UserPromptSubmit")
            .with_session("a")
            .from_subagent(),
        now,
    );
    pane.detector.observe_hook(
        &HookEvent::new("claude", "UserPromptSubmit").with_session("other"),
        now,
    );
    pane.detector
        .observe_hook(&HookEvent::new("claude", "Stop").with_session("a"), now);
    assert_eq!(pane.detector.turns_begun(), after_prompt);

    // Working repeated after Stop (held back) and Done do not count.
    pane.tell_unseen(ProgramState::Working);
    pane.look();
    pane.tell_unseen(ProgramState::Done);
    pane.look();
    assert_eq!(pane.detector.turns_begun(), after_prompt);

    // New work after Done counts.
    pane.tell_unseen(ProgramState::Working);
    pane.look();
    assert_eq!(pane.detector.turns_begun(), after_prompt + 1);

    // Transition to Blocked (Waiting) counts even without new work.
    let before_block = pane.detector.turns_begun();
    pane.tell_unseen(ProgramState::Blocked);
    pane.record.as_mut().unwrap().status.kind = Some(BlockKind::Permission);
    pane.look();
    assert_eq!(pane.detector.turns_begun(), before_block + 1);

    // A hook without any OSC 7501 record counts on its own.
    let mut no_osc = Pane::with("claude");
    assert_eq!(no_osc.detector.turns_begun(), 0);
    no_osc.detector.observe_hook(
        &HookEvent::new("claude", "UserPromptSubmit").with_session("a"),
        now,
    );
    assert_eq!(no_osc.detector.turns_begun(), 1);
    no_osc.detector.observe_hook(
        &HookEvent::new("claude", "PermissionRequest").with_session("a"),
        now,
    );
    assert_eq!(no_osc.detector.turns_begun(), 2);
    no_osc.detector.observe_hook(
        &HookEvent::new("claude", "StopFailure").with_session("a"),
        now,
    );
    assert_eq!(no_osc.detector.turns_begun(), 3);
}

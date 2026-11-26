//! Work the program told with OSC 7501 before a hook told that the turn
//! rests.
//!
//! The rule: a hook that tells the end of a turn, a failure or a wait
//! for the user is newer than the work the program had begun by then.
//! While the terminal's record still tells that work, the hook decides,
//! and the screen behind it as ever. Claude Code 2.1.330 tells `working`
//! again right after its `Stop` hook, and keeps telling it, while a
//! command it left running behind is not over. The record decides again
//! when the program begins to work anew, which the terminal counts only
//! when the program was not working before, or when a hook tells that
//! the agent works. A program that sends no hooks is never held so.

mod common;

use std::time::{Duration, Instant};

use benterm_agent::{AgentState, Detector, HookEvent, WaitReason, builtin};
use benterm_core::{BlockKind, ProgramRecord, ProgramState, ProgramStatus};
use common::{screen_of, toy};

const SETTLE: Duration = Detector::DEFAULT_SETTLE;
const PERMISSION: AgentState = AgentState::Waiting(WaitReason::Permission);
const CHOICE: AgentState = AgentState::Waiting(WaitReason::Choice);

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

/// A detector with a program in front, and the reports it is given
/// numbered as the terminal numbers them.
struct Pane {
    detector: Detector,
    /// What the terminal holds of the program itself.
    record: Option<ProgramRecord>,
    serial: u64,
    work: u64,
    now: Instant,
}

impl Pane {
    fn with(command: &str) -> Self {
        let mut definitions = builtin();
        definitions.push(toy());
        let mut pane = Self {
            detector: Detector::new(definitions),
            record: None,
            serial: 0,
            work: 0,
            now: Instant::now(),
        };
        pane.front(100, command);
        pane
    }

    fn front(&mut self, pid: u32, command: &str) {
        self.detector
            .observe_front_process(pid, &[command.to_owned()]);
    }

    /// The program tells `state`, and the detector is shown it.
    fn tell(&mut self, state: ProgramState) {
        self.tell_unseen(state);
        self.look();
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

    fn hook(&mut self, name: &str) {
        self.hook_of(HookEvent::new("claude", name).with_session("a"));
    }

    fn hook_of(&mut self, event: HookEvent) {
        self.detector.observe_hook(&event, self.now);
    }

    fn state(&self) -> AgentState {
        self.detector.state()
    }

    fn work_begun(&self) -> u64 {
        self.detector.work_begun()
    }
}

#[test]
fn the_end_a_hook_tells_is_not_hidden_by_work_told_before_it() {
    let mut pane = Pane::with("claude");
    pane.tell(ProgramState::Working);
    pane.hook("UserPromptSubmit");
    pane.hook("Stop");
    assert_eq!(pane.state(), AgentState::Done);
    // The terminal holds the record as before.
    pane.look();
    assert_eq!(pane.state(), AgentState::Done);
}

#[test]
fn working_told_again_after_the_end_is_no_new_work() {
    let mut pane = Pane::with("claude");
    pane.tell(ProgramState::Working);
    pane.hook("Stop");
    let work = pane.work_begun();
    for _ in 0..3 {
        pane.tell(ProgramState::Working);
        assert_eq!(pane.state(), AgentState::Done);
        assert_eq!(pane.work_begun(), work);
    }
}

#[test]
fn a_new_prompt_a_hook_tells_lets_the_record_decide_again() {
    let mut pane = Pane::with("claude");
    pane.tell(ProgramState::Working);
    pane.hook("Stop");
    pane.hook("UserPromptSubmit");
    assert_eq!(pane.state(), AgentState::Working);
    pane.tell(ProgramState::Working);
    assert_eq!(pane.state(), AgentState::Working);
    // The record of this turn is not held back by the end of the last.
    pane.tell(ProgramState::Blocked);
    assert_eq!(pane.state(), AgentState::Waiting(WaitReason::Question));
}

/// The program told that it was done, seen or not, and works again: the
/// next turn began, though no hook has told of it yet. Both agents tell
/// `working` a moment before their `UserPromptSubmit` hook comes.
#[test]
fn work_that_began_after_the_end_is_working() {
    let mut pane = Pane::with("claude");
    pane.tell(ProgramState::Working);
    pane.hook("Stop");
    pane.tell(ProgramState::Done);
    let work = pane.work_begun();
    pane.tell(ProgramState::Working);
    assert_eq!(pane.state(), AgentState::Working);
    assert_eq!(pane.work_begun(), work + 1);

    let mut pane = Pane::with("claude");
    pane.tell(ProgramState::Working);
    pane.hook("Stop");
    let work = pane.work_begun();
    pane.tell_unseen(ProgramState::Done);
    pane.tell_unseen(ProgramState::Working);
    pane.look();
    assert_eq!(pane.state(), AgentState::Working);
    assert_eq!(pane.work_begun(), work + 1);
}

#[test]
fn a_wait_a_hook_tells_is_not_hidden_by_work_told_before_it() {
    let mut pane = Pane::with("claude");
    pane.tell(ProgramState::Working);
    pane.hook("PermissionRequest");
    assert_eq!(pane.state(), PERMISSION);
    let work = pane.work_begun();
    pane.tell(ProgramState::Working);
    assert_eq!(pane.state(), PERMISSION);
    assert_eq!(pane.work_begun(), work);
    // The user allowed it, and the tool ran.
    pane.hook("PostToolUse");
    assert_eq!(pane.state(), AgentState::Working);

    let mut pane = Pane::with("claude");
    pane.tell(ProgramState::Working);
    pane.hook_of(
        HookEvent::new("claude", "PreToolUse")
            .with_session("a")
            .with_tool("AskUserQuestion"),
    );
    pane.tell(ProgramState::Working);
    assert_eq!(pane.state(), CHOICE);
}

#[test]
fn a_failure_a_hook_tells_is_not_hidden_by_work_told_before_it() {
    let mut pane = Pane::with("claude");
    pane.tell(ProgramState::Working);
    pane.hook("StopFailure");
    assert_eq!(pane.state(), AgentState::Error);
    pane.tell(ProgramState::Working);
    assert_eq!(pane.state(), AgentState::Error);
}

/// What the program tells apart from its work is what it does now.
#[test]
fn what_the_program_tells_after_the_hook_that_is_no_work_decides() {
    let mut pane = Pane::with("claude");
    pane.tell(ProgramState::Working);
    pane.hook("Stop");
    pane.tell_unseen(ProgramState::Blocked);
    pane.record.as_mut().unwrap().status.kind = Some(BlockKind::Permission);
    pane.look();
    assert_eq!(pane.state(), PERMISSION);
}

#[test]
fn a_hook_that_is_not_taken_holds_nothing_back() {
    let mut pane = Pane::with("claude");
    pane.tell(ProgramState::Working);
    pane.hook("UserPromptSubmit");
    pane.hook_of(
        HookEvent::new("claude", "Stop")
            .with_session("a")
            .from_subagent(),
    );
    pane.hook_of(HookEvent::new("claude", "Stop").with_session("b"));
    pane.hook_of(HookEvent::new("codex", "Stop").with_session("c"));
    assert_eq!(pane.state(), AgentState::Working);
}

/// The screen is weighed behind the hook as ever, so a turn that went on
/// with no hook telling so is seen there. No time alone brings the
/// record back.
#[test]
fn a_steady_screen_puts_the_hook_right_while_the_record_is_held_back() {
    let mut pane = Pane::with("claude");
    pane.tell(ProgramState::Working);
    pane.hook("Stop");
    let at_work = screen_of(&["✻ Thinking…", "  esc to interrupt"]);
    pane.detector.observe_screen(&at_work, pane.now);
    assert_eq!(pane.state(), AgentState::Done);
    pane.now += SETTLE;
    pane.detector.observe_screen(&at_work, pane.now);
    assert_eq!(pane.state(), AgentState::Working);

    let at_rest = screen_of(&["❯ ", "  ? for shortcuts"]);
    pane.now += SETTLE * 600;
    pane.detector.observe_screen(&at_rest, pane.now);
    pane.now += SETTLE;
    pane.detector.observe_screen(&at_rest, pane.now);
    pane.tell(ProgramState::Working);
    assert_eq!(pane.state(), AgentState::Done);
}

/// The record is the terminal's, and the next run's work is not older
/// than the hook of the run before.
#[test]
fn the_end_of_one_run_does_not_hold_back_the_work_of_the_next() {
    let mut pane = Pane::with("claude");
    pane.tell(ProgramState::Working);
    pane.hook("Stop");
    pane.front(100, "claude");
    pane.front(200, "claude");
    pane.tell(ProgramState::Working);
    assert_eq!(pane.state(), AgentState::Working);
}

/// Work told again that was held back is no word of the program that
/// comes next, though it came since the front was last read.
#[test]
fn work_held_back_does_not_pass_to_the_program_after_the_agent() {
    for next in ["zsh", "claude", "codex"] {
        let mut pane = Pane::with("claude");
        pane.tell(ProgramState::Working);
        pane.hook("Stop");
        pane.front(100, "claude");
        pane.tell(ProgramState::Working);
        assert_eq!(pane.state(), AgentState::Done);
        pane.front(200, next);
        assert_eq!(pane.state(), AgentState::Unknown, "{next}");
        // The terminal holds the record as before.
        pane.look();
        assert_eq!(pane.state(), AgentState::Unknown, "{next}");
    }
}

#[test]
fn a_program_that_sends_no_hooks_has_the_state_it_tells() {
    for command in ["terraform", "toy", "claude", "codex"] {
        let mut pane = Pane::with(command);
        pane.tell(ProgramState::Working);
        assert_eq!(pane.state(), AgentState::Working, "{command}");
        // Told again.
        pane.tell(ProgramState::Working);
        assert_eq!(pane.state(), AgentState::Working, "{command}");
        assert_eq!(pane.work_begun(), 1, "{command}");
        pane.tell(ProgramState::Done);
        assert_eq!(pane.state(), AgentState::Done, "{command}");
        pane.tell(ProgramState::Working);
        assert_eq!(pane.state(), AgentState::Working, "{command}");
        assert_eq!(pane.work_begun(), 2, "{command}");
    }
}

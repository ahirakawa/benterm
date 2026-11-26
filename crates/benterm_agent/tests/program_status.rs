//! What the program tells of itself with OSC 7501, weighed against the
//! hooks and the screen.
//!
//! The rule: while the terminal holds a record of the program itself,
//! the record decides the state, before the hooks and the screen. The
//! record says what the program is doing now, in the program's own
//! words; a hook tells one moment and may be late, and the screen is
//! read by rules. Three things only a hook or the screen can tell stay:
//! that a question is one of choices, that a permission is the trust of
//! a folder, and that the turn that ended had failed. The hooks and the screen are weighed as before behind the
//! record, and decide again once the record is gone.
//!
//! One thing a hook tells comes before the record: that the work the
//! record still tells is over or waits for the user. `told_before_hook.rs`
//! checks that.

mod common;

use std::time::{Duration, Instant};

use benterm_agent::{AgentState, Detector, HookEvent, WaitReason, builtin};
use benterm_core::{BlockKind, ProgramRecord, ProgramState, ProgramStatus};
use common::screen_of;

const SETTLE: Duration = Detector::DEFAULT_SETTLE;
const PERMISSION: AgentState = AgentState::Waiting(WaitReason::Permission);
const TRUST: AgentState = AgentState::Waiting(WaitReason::Trust);
const QUESTION: AgentState = AgentState::Waiting(WaitReason::Question);
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

fn blocked(kind: Option<BlockKind>) -> ProgramStatus {
    ProgramStatus {
        kind,
        ..status(ProgramState::Blocked)
    }
}

/// A detector with Claude Code in front, and the reports it is given
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
    fn empty() -> Self {
        Self {
            detector: Detector::new(builtin()),
            record: None,
            serial: 0,
            work: 0,
            now: Instant::now(),
        }
    }

    fn with_claude() -> Self {
        let mut pane = Self::empty();
        pane.front(100, "claude");
        pane
    }

    fn front(&mut self, pid: u32, command: &str) {
        self.detector
            .observe_front_process(pid, &[command.to_owned()]);
    }

    /// The program tells `status`, and the detector is shown it.
    fn tell(&mut self, status: ProgramStatus) {
        self.tell_unseen(status);
        self.look();
    }

    /// The program tells `status`, and the terminal keeps it. Work
    /// begins when the program was not working before.
    fn tell_unseen(&mut self, status: ProgramStatus) {
        let working = |record: &ProgramRecord| record.status.state == ProgramState::Working;
        let began =
            status.state == ProgramState::Working && !self.record.as_ref().is_some_and(working);
        self.serial += 1;
        self.work += u64::from(began);
        self.record = Some(ProgramRecord {
            status,
            serial: self.serial,
            work: self.work,
        });
    }

    /// The terminal drops what it holds.
    fn drop_record(&mut self) {
        self.record = None;
        self.look();
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

    /// The screen shows `lines` for longer than the settle time.
    fn show(&mut self, lines: &[&str]) {
        let screen = screen_of(lines);
        self.detector.observe_screen(&screen, self.now);
        self.now += SETTLE;
        self.detector.observe_screen(&screen, self.now);
    }

    fn state(&self) -> AgentState {
        self.detector.state()
    }
}

fn ask_choices() -> HookEvent {
    HookEvent::new("claude", "PreToolUse")
        .with_session("a")
        .with_tool("AskUserQuestion")
}

#[test]
fn each_state_of_a_program_is_a_state_of_an_agent() {
    use BlockKind::{Auth, Permission, Question};
    for (told, state) in [
        (status(ProgramState::Working), AgentState::Working),
        (status(ProgramState::Done), AgentState::Done),
        (status(ProgramState::Idle), AgentState::Done),
        (status(ProgramState::Error), AgentState::Error),
        (blocked(Some(Permission)), PERMISSION),
        (blocked(Some(Question)), QUESTION),
        // A login is something the user types, and nothing to allow or
        // deny with a key.
        (blocked(Some(Auth)), QUESTION),
        // Blocked by something the program did not name.
        (blocked(None), QUESTION),
    ] {
        assert_eq!(AgentState::of_program(&told), state, "{told:?}");
        let mut pane = Pane::with_claude();
        pane.tell(told);
        assert_eq!(pane.state(), state);
    }
}

#[test]
fn working_told_by_the_program_after_a_hook_said_done_is_working() {
    let mut pane = Pane::with_claude();
    pane.hook("Stop");
    assert_eq!(pane.state(), AgentState::Done);
    pane.tell(status(ProgramState::Working));
    assert_eq!(pane.state(), AgentState::Working);
}

#[test]
fn a_hook_that_says_otherwise_does_not_undo_what_the_program_tells_now() {
    let mut pane = Pane::with_claude();
    pane.tell(status(ProgramState::Working));
    pane.tell(status(ProgramState::Done));
    // A hook that comes late.
    pane.hook("PostToolUse");
    assert_eq!(pane.state(), AgentState::Done);
    pane.tell(blocked(Some(BlockKind::Permission)));
    pane.hook("Stop");
    assert_eq!(pane.state(), PERMISSION);
}

#[test]
fn a_screen_that_says_otherwise_does_not_undo_what_the_program_tells_now() {
    let mut pane = Pane::with_claude();
    pane.tell(status(ProgramState::Working));
    pane.show(&["❯ ", "  ? for shortcuts"]);
    assert_eq!(pane.state(), AgentState::Working);
}

#[test]
fn once_the_record_is_gone_the_hooks_and_the_screen_decide_again() {
    let mut pane = Pane::with_claude();
    pane.tell(status(ProgramState::Working));
    pane.show(&["❯ ", "  ? for shortcuts"]);
    pane.drop_record();
    assert_eq!(pane.state(), AgentState::Done);
    pane.hook("UserPromptSubmit");
    assert_eq!(pane.state(), AgentState::Working);
}

/// The program took back what it told, or a prompt started, and neither
/// a hook nor the screen has told anything.
#[test]
fn a_state_only_the_record_told_goes_with_the_record() {
    for told in [
        status(ProgramState::Working),
        blocked(Some(BlockKind::Permission)),
        status(ProgramState::Done),
    ] {
        let mut pane = Pane::with_claude();
        pane.tell(told);
        pane.drop_record();
        assert_eq!(pane.state(), AgentState::Unknown);
    }
    // What a hook told after the report stays.
    let mut pane = Pane::with_claude();
    pane.tell(status(ProgramState::Working));
    pane.hook("PermissionRequest");
    pane.drop_record();
    assert_eq!(pane.state(), PERMISSION);
}

#[test]
fn a_question_is_one_of_choices_when_a_hook_says_so_whichever_comes_first() {
    let mut pane = Pane::with_claude();
    pane.tell(status(ProgramState::Working));
    pane.hook_of(ask_choices());
    pane.tell(blocked(Some(BlockKind::Question)));
    assert_eq!(pane.state(), CHOICE);

    let mut pane = Pane::with_claude();
    pane.tell(status(ProgramState::Working));
    pane.tell(blocked(Some(BlockKind::Question)));
    assert_eq!(pane.state(), QUESTION);
    pane.hook_of(ask_choices());
    assert_eq!(pane.state(), CHOICE);
}

#[test]
fn the_choices_of_an_earlier_question_do_not_stay_for_the_next() {
    let mut pane = Pane::with_claude();
    pane.hook_of(ask_choices());
    pane.tell(blocked(Some(BlockKind::Question)));
    pane.tell(status(ProgramState::Working));
    pane.tell(blocked(Some(BlockKind::Question)));
    assert_eq!(pane.state(), QUESTION);
}

#[test]
fn only_a_question_becomes_one_of_choices() {
    for kind in [Some(BlockKind::Permission), Some(BlockKind::Auth), None] {
        let mut pane = Pane::with_claude();
        pane.hook_of(ask_choices());
        pane.tell(blocked(kind));
        assert_ne!(pane.state(), CHOICE, "{kind:?}");
    }
}

/// Claude Code tells a permission while it asks whether to trust the
/// folder. Only the screen tells the two apart, and their dialogs take
/// other keys.
#[test]
fn a_permission_is_the_trust_of_a_folder_when_the_screen_says_so_whichever_comes_first() {
    let trust_dialog = [
        "   Yes, I trust this folder",
        " Enter to confirm · Esc to cancel",
    ];
    let mut pane = Pane::with_claude();
    pane.tell(blocked(Some(BlockKind::Permission)));
    assert_eq!(pane.state(), PERMISSION);
    pane.show(&trust_dialog);
    assert_eq!(pane.state(), TRUST);

    let mut pane = Pane::with_claude();
    pane.show(&trust_dialog);
    pane.tell(blocked(Some(BlockKind::Permission)));
    assert_eq!(pane.state(), TRUST);
    // The folder is trusted, and a tool asks next.
    pane.tell(status(ProgramState::Working));
    pane.tell(blocked(Some(BlockKind::Permission)));
    assert_eq!(pane.state(), PERMISSION);
}

#[test]
fn a_turn_a_hook_says_failed_is_an_error_whichever_comes_first() {
    for at_rest in [ProgramState::Done, ProgramState::Idle] {
        let mut pane = Pane::with_claude();
        pane.tell(status(ProgramState::Working));
        pane.hook("StopFailure");
        pane.tell(status(at_rest));
        assert_eq!(pane.state(), AgentState::Error);

        let mut pane = Pane::with_claude();
        pane.tell(status(ProgramState::Working));
        pane.tell(status(at_rest));
        pane.hook("StopFailure");
        assert_eq!(pane.state(), AgentState::Error);
        // Codex tells done and then idle.
        pane.tell(status(ProgramState::Idle));
        assert_eq!(pane.state(), AgentState::Error);
    }
}

#[test]
fn the_error_of_an_earlier_turn_does_not_stay_for_the_next() {
    let mut pane = Pane::with_claude();
    pane.hook("StopFailure");
    pane.tell(status(ProgramState::Working));
    assert_eq!(pane.state(), AgentState::Working);
    // No hook tells the end of this turn.
    pane.tell(status(ProgramState::Done));
    assert_eq!(pane.state(), AgentState::Done);
}

/// The terminal keeps only the last report, and the next turn began and
/// ended before the detector was shown any of it.
#[test]
fn a_turn_that_began_unseen_ends_the_error_and_the_choices_of_the_one_before() {
    let mut pane = Pane::with_claude();
    pane.hook("StopFailure");
    pane.tell_unseen(status(ProgramState::Working));
    pane.tell_unseen(status(ProgramState::Done));
    pane.look();
    assert_eq!(pane.state(), AgentState::Done);

    let mut pane = Pane::with_claude();
    pane.hook_of(ask_choices());
    pane.tell(blocked(Some(BlockKind::Question)));
    assert_eq!(pane.state(), CHOICE);
    pane.tell_unseen(status(ProgramState::Working));
    pane.tell_unseen(blocked(Some(BlockKind::Question)));
    pane.look();
    assert_eq!(pane.state(), QUESTION);
}

#[test]
fn a_program_that_is_no_agent_has_the_state_it_tells() {
    let mut pane = Pane::empty();
    pane.front(100, "terraform");
    assert_eq!(pane.state(), AgentState::Unknown);
    pane.tell(blocked(Some(BlockKind::Permission)));
    assert!(pane.detector.agent().is_none());
    assert_eq!(pane.state(), PERMISSION);
    pane.drop_record();
    assert_eq!(pane.state(), AgentState::Unknown);
}

/// The record is the terminal's and names no process. One told before
/// the front was last read is that of the program read then.
#[test]
fn what_an_agent_told_is_over_when_it_leaves_the_front() {
    for told in [
        status(ProgramState::Working),
        blocked(Some(BlockKind::Permission)),
        status(ProgramState::Done),
        status(ProgramState::Error),
    ] {
        let mut pane = Pane::with_claude();
        pane.tell(told.clone());
        pane.front(100, "claude");
        // The agent died without a word, and no shell told a prompt.
        pane.front(200, "zsh");
        assert_eq!(pane.state(), AgentState::Unknown, "{told:?}");
        // The terminal still holds the record.
        pane.look();
        assert_eq!(pane.state(), AgentState::Unknown);
        // The next program tells the same.
        pane.tell(told.clone());
        assert_eq!(pane.state(), AgentState::of_program(&told));
    }
}

#[test]
fn what_an_agent_told_is_over_when_another_run_of_it_is_in_front() {
    let mut pane = Pane::with_claude();
    pane.tell(blocked(Some(BlockKind::Permission)));
    pane.front(100, "claude");
    pane.front(200, "claude");
    assert_eq!(pane.state(), AgentState::Unknown);
}

/// Claude Code told an error and died, and Codex, which here tells
/// nothing of itself, is started in the same pane.
#[test]
fn what_one_agent_reached_does_not_cover_what_the_next_one_does() {
    let mut pane = Pane::with_claude();
    pane.tell(status(ProgramState::Error));
    pane.front(100, "claude");
    pane.front(200, "zsh");
    pane.front(300, "codex");
    pane.look();
    pane.hook_of(HookEvent::new("codex", "UserPromptSubmit").with_session("b"));
    assert_eq!(pane.state(), AgentState::Working);
}

/// The front is read now and then, so the first report of a program can
/// come before the program is read in front.
#[test]
fn a_report_that_came_since_the_front_was_last_read_is_that_of_the_program_read_next() {
    // An agent after the shell.
    let mut pane = Pane::empty();
    pane.front(100, "zsh");
    pane.tell(status(ProgramState::Idle));
    pane.front(200, "claude");
    assert_eq!(pane.detector.agent().map(|agent| agent.id), Some("claude"));
    assert_eq!(pane.state(), AgentState::Done);

    // Another run of the agent, with no shell read in between.
    let mut pane = Pane::with_claude();
    pane.tell(status(ProgramState::Working));
    pane.front(100, "claude");
    pane.tell(blocked(Some(BlockKind::Permission)));
    pane.front(200, "claude");
    assert_eq!(pane.state(), PERMISSION);
}

#[test]
fn the_same_record_seen_again_changes_nothing() {
    let mut pane = Pane::with_claude();
    pane.tell(status(ProgramState::Working));
    pane.hook("PermissionRequest");
    pane.show(&["Do you want to proceed?", "Esc to cancel · Tab to amend"]);
    // The terminal holds the record as before.
    pane.look();
    pane.drop_record();
    assert_eq!(pane.state(), PERMISSION);
}

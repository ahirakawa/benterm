//! The agent of one pane: a [`Detector`] fed with the pane's screen and
//! its hook events, and somebody to tell when what it decides changes.

use std::time::Instant;

use benterm_agent::{AgentState, Detector, HookEvent, Plan, PlanTracker, builtin};
use benterm_core::Row;
use benterm_session::{Agent, Conversation};
use benterm_terminal::{CommandEnd, ForegroundProcess, ProgramRecord};

use crate::checked_reports::shown_line;

/// Which agent runs in a pane and what it is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AgentStatus {
    /// The id of the agent's definition, `claude` or `codex`; `None` when
    /// no agent is recognised. The state is then unknown, unless the
    /// program in the pane tells its state itself (OSC 7501).
    pub agent: Option<&'static str>,
    pub state: AgentState,
}

/// What a pane tells whoever listens to its agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentNews {
    /// The agent or its state is no longer what it was.
    Changed(AgentStatus),
    Closed,
}

/// Called on the UI thread, inside an update of the pane's view. It must
/// not wait for anything.
pub type AgentListener = Box<dyn FnMut(AgentNews)>;

pub(crate) struct AgentWatch {
    detector: Detector,
    /// What the listener was told last.
    told: AgentStatus,
    /// The turns begun as observers were last told of them.
    told_turns: u64,
    /// Work starts reported by OSC 7501 as of the last notification check.
    told_work: u64,
    /// The agent named since the shell last told the end of a command:
    /// the one the command running now started, if any.
    ran: Option<&'static str>,
    /// The plan the hooks told, with the conversation it belongs to.
    plan: PlanTracker,
    /// The `msg` from the terminal's latest root OSC 7501 record, on one
    /// line without control characters.
    program_msg: Option<String>,
    listener: Option<AgentListener>,
}

impl AgentWatch {
    pub(crate) fn new(listener: Option<AgentListener>) -> Self {
        let mut detector = Detector::new(builtin());
        // The view reads the program in front before it shows the screen.
        detector.expect_front();
        Self {
            told: status_of(&detector),
            told_turns: detector.turns_begun(),
            told_work: detector.work_begun(),
            ran: None,
            plan: PlanTracker::default(),
            program_msg: None,
            detector,
            listener,
        }
    }

    pub(crate) fn status(&self) -> AgentStatus {
        status_of(&self.detector)
    }

    /// The id of the agent whose process the last reading of the program
    /// in front found there, with the id of that process.
    pub(crate) fn agent_in_front(&self) -> Option<(&'static str, u32)> {
        let (agent, pid) = self.detector.agent_in_front()?;
        Some((agent.id, pid))
    }

    /// What the program told of itself with OSC 7501 (`msg`), on one
    /// line without control characters, while the pane waits for the
    /// user or has stopped with an error.
    pub(crate) fn msg(&self) -> Option<&str> {
        let show = matches!(
            self.detector.state(),
            AgentState::Waiting(_) | AgentState::Error
        );
        show.then_some(self.program_msg.as_deref()).flatten()
    }

    /// Looks at the screen. Returns whether the status changed.
    pub(crate) fn observe_rows(&mut self, rows: &[Row], now: Instant) -> bool {
        self.detector.observe_rows(rows, now);
        self.tell()
    }

    /// Takes what the terminal holds of what its program told of itself
    /// with OSC 7501. Returns whether the status or shown `msg` changed.
    pub(crate) fn observe_program(&mut self, record: Option<ProgramRecord>) -> bool {
        let msg_before = self.msg().map(str::to_owned);
        self.program_msg = record
            .as_ref()
            .and_then(|record| record.status.msg.as_deref())
            .map(shown_line)
            .filter(|msg| !msg.is_empty());
        self.detector.observe_program(record.as_ref());
        let msg_changed = self.msg() != msg_before.as_deref();
        self.tell() || msg_changed
    }

    /// Takes the program in front of the pane's terminal, `None` when it
    /// could not be read. Returns whether the status changed.
    pub(crate) fn observe_front(&mut self, front: Option<ForegroundProcess>) -> bool {
        self.tell_front(front);
        self.tell()
    }

    /// Takes the end of a command, as the shell told it, and the program
    /// in front now. An agent the command started that exited with a
    /// code other than 0 is told as an error, and then as gone. Returns
    /// whether the status changed.
    ///
    /// With an agent still in front, the end is not the agent's: the
    /// agent printed the mark itself, or the front could not be read.
    pub(crate) fn observe_command_end(
        &mut self,
        end: CommandEnd,
        front: Option<ForegroundProcess>,
    ) -> bool {
        self.tell_front(front);
        if self.detector.agent().is_some() {
            return self.tell();
        }
        let failed = end.exit_code.is_some_and(|code| code != 0);
        let error = self.ran.take().filter(|_| failed).map(|agent| AgentStatus {
            agent: Some(agent),
            state: AgentState::Error,
        });
        if let Some(error) = error {
            self.told = error;
            if let Some(listener) = &mut self.listener {
                listener(AgentNews::Changed(error));
            }
        }
        self.tell() || error.is_some()
    }

    /// Tells the detector the program in front with its process id, by
    /// which it tells one run of an agent from the next.
    fn tell_front(&mut self, front: Option<ForegroundProcess>) {
        let Some(front) = front else {
            self.detector.observe_front(None);
            return;
        };
        let args: Vec<String> = front
            .args
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        self.detector.observe_front_process(front.pid, &args);
    }

    /// Takes a hook event. Returns whether the status or the plan
    /// changed.
    pub(crate) fn observe_hook(&mut self, event: &HookEvent, now: Instant) -> bool {
        let plan_before = self.plan().cloned();
        self.detector.observe_hook(event, now);
        self.observe_plan(event);
        let plan_changed = self.plan() != plan_before.as_ref();
        self.tell() || plan_changed
    }

    /// Takes what a hook event does to the plan, if it is an event of
    /// the conversation in front: the detector has weighed the event
    /// already, and names another conversation for one of an agent that
    /// runs as a tool or has left. A subagent keeps a list of its own.
    fn observe_plan(&mut self, event: &HookEvent) {
        let (Some(update), Some(session)) = (&event.plan, &event.session) else {
            return;
        };
        let in_front = self
            .detector
            .agent()
            .is_some_and(|agent| agent.id == event.agent)
            && self.detector.owner() == Some(session);
        if in_front && !event.subagent {
            self.plan.apply(session, update.clone());
        }
    }

    /// The conversation of the agent in front, as its hooks named it:
    /// `None` when no hook has named one, or once it ended.
    pub(crate) fn conversation(&self) -> Option<Conversation> {
        let agent = Agent::from_id(self.detector.agent()?.id)?;
        Conversation::new(agent, self.detector.owner()?)
    }

    /// The plan of the conversation of the agent in front, as the hooks
    /// of its tools told it: `None` when they told none.
    pub(crate) fn plan(&self) -> Option<&Plan> {
        self.detector.agent()?;
        self.plan.plan_of(self.detector.owner()?)
    }

    /// How many times a turn began, asked the user, or stopped by an
    /// error. Work that began and ended between two looks counts too,
    /// though the status never showed it.
    pub(crate) fn turns_begun(&self) -> u64 {
        self.detector.turns_begun()
    }

    /// When to look at the screen again although nothing was printed.
    pub(crate) fn pending_deadline(&self) -> Option<Instant> {
        self.detector.pending_deadline()
    }

    /// Tells the listener a new status or a short turn that ended in the
    /// same state. Returns whether the view's observers should be notified.
    fn tell(&mut self) -> bool {
        let status = self.status();
        if status.agent.is_some() {
            self.ran = status.agent;
        }
        let turns = self.detector.turns_begun();
        let turn_began = turns != self.told_turns;
        self.told_turns = turns;
        let work = self.detector.work_begun();
        let work_began = work != self.told_work;
        self.told_work = work;
        let same_status = status == self.told;
        // Repeated waiting hooks count for turn tracking. A fresh OSC
        // 7501 work start is evidence of work between observations.
        let completed_short_turn = same_status
            && work_began
            && status.agent.is_some()
            && matches!(
                status.state,
                AgentState::Done | AgentState::Error | AgentState::Waiting(_)
            );
        if same_status && !completed_short_turn {
            return turn_began;
        }
        self.told = status;
        if let Some(listener) = &mut self.listener {
            listener(AgentNews::Changed(status));
        }
        true
    }
}

impl Drop for AgentWatch {
    fn drop(&mut self) {
        if let Some(listener) = &mut self.listener {
            listener(AgentNews::Closed);
        }
    }
}

fn status_of(detector: &Detector) -> AgentStatus {
    AgentStatus {
        agent: detector.agent().map(|agent| agent.id),
        state: detector.state(),
    }
}

#[cfg(test)]
mod tests {
    use benterm_core::{AlacrittyCore, GridSize, TerminalCore};

    use super::*;

    /// A pane restored with Claude Code's start screen on it.
    fn restored_rows() -> Vec<Row> {
        let mut core = AlacrittyCore::new(GridSize::new(24, 80), 100);
        core.feed(b"* Welcome to Claude Code\r\n\r\n\xe2\x9d\xaf \r\n  ? for shortcuts\r\n");
        core.snapshot().rows
    }

    fn front(pid: u32, command: &str) -> Option<ForegroundProcess> {
        Some(ForegroundProcess {
            pid,
            args: vec![command.into()],
        })
    }

    fn hook(agent: &str, name: &str, session: &str) -> HookEvent {
        HookEvent::new(agent, name).with_session(session)
    }

    /// The agent ended and was started again before the shell between
    /// the two was read in front.
    #[test]
    fn a_new_process_of_the_agent_in_front_has_no_state_and_no_conversation() {
        let mut watch = AgentWatch::new(None);
        let now = Instant::now();
        watch.observe_front(front(100, "claude"));
        watch.observe_hook(&hook("claude", "UserPromptSubmit", "a"), now);
        watch.observe_hook(&hook("claude", "StopFailure", "a"), now);
        assert_eq!(watch.status().state, AgentState::Error);
        assert!(watch.conversation().is_some());
        // The same process read again is the same run.
        assert!(!watch.observe_front(front(100, "claude")));

        assert!(watch.observe_front(front(200, "claude")));
        assert_eq!(watch.status().agent, Some("claude"));
        assert_eq!(watch.status().state, AgentState::Unknown);
        assert_eq!(watch.conversation(), None);
    }

    /// The shell tells the end of a command while a new process of the
    /// agent is already in front.
    #[test]
    fn the_end_of_a_command_tells_a_new_process_in_front_too() {
        let mut watch = AgentWatch::new(None);
        let now = Instant::now();
        watch.observe_front(front(100, "claude"));
        watch.observe_hook(&hook("claude", "StopFailure", "a"), now);
        let end = CommandEnd { exit_code: Some(0) };
        assert!(watch.observe_command_end(end, front(200, "claude")));
        assert_eq!(watch.status().state, AgentState::Unknown);
        assert_eq!(watch.conversation(), None);
    }

    #[test]
    fn a_late_hook_of_the_agent_that_left_the_front_changes_nothing() {
        let mut watch = AgentWatch::new(None);
        let now = Instant::now();
        watch.observe_front(front(100, "claude"));
        watch.observe_hook(&hook("claude", "UserPromptSubmit", "a"), now);
        watch.observe_front(front(200, "codex"));
        assert!(!watch.observe_hook(&hook("claude", "Stop", "a"), now));
        assert_eq!(watch.status().agent, Some("codex"));
        assert_eq!(watch.status().state, AgentState::Unknown);
        assert_eq!(watch.conversation(), None);
    }

    /// The first reading of the program in front can fail under load, as
    /// the screen restored in the pane is first looked at.
    #[test]
    fn a_restored_banner_names_no_agent_before_the_front_is_read() {
        let mut watch = AgentWatch::new(None);
        watch.observe_front(None);
        let now = Instant::now();
        watch.observe_rows(&restored_rows(), now);
        watch.observe_rows(&restored_rows(), now + Detector::DEFAULT_SETTLE);
        assert_eq!(watch.status().agent, None);
    }

    fn record(
        serial: u64,
        work: u64,
        state: benterm_core::ProgramState,
        kind: Option<benterm_core::BlockKind>,
        msg: Option<&str>,
    ) -> ProgramRecord {
        ProgramRecord {
            status: benterm_core::ProgramStatus {
                state,
                id: None,
                kind,
                progress: None,
                app: None,
                title: None,
                msg: msg.map(str::to_owned),
            },
            serial,
            work,
        }
    }

    #[test]
    fn program_msg_shows_only_when_waiting_or_error_and_strips_control_chars() {
        use benterm_core::{BlockKind, ProgramState};

        let mut watch = AgentWatch::new(None);
        // Waiting with msg.
        assert!(watch.observe_program(Some(record(
            1,
            0,
            ProgramState::Blocked,
            Some(BlockKind::Permission),
            Some("Allow Bash: cargo test"),
        ))));
        assert_eq!(watch.msg(), Some("Allow Bash: cargo test"));

        // Working clears msg even if the record carries one.
        assert!(watch.observe_program(Some(record(
            2,
            1,
            ProgramState::Working,
            None,
            Some("Running cargo test"),
        ))));
        assert_eq!(watch.status().state, AgentState::Working);
        assert_eq!(watch.msg(), None);

        // Done and Idle do not show msg.
        assert!(watch.observe_program(Some(record(
            3,
            1,
            ProgramState::Done,
            None,
            Some("Finished"),
        ))));
        assert_eq!(watch.msg(), None);

        assert!(!watch.observe_program(Some(record(
            4,
            1,
            ProgramState::Idle,
            None,
            Some("Ready"),
        ))));
        assert_eq!(watch.msg(), None);

        // Error shows msg, stripping control characters and collapsing whitespace.
        assert!(watch.observe_program(Some(record(
            5,
            1,
            ProgramState::Error,
            None,
            Some("  API\u{1b}[31m error:\n\tlimit\u{07} reached  "),
        ))));
        assert_eq!(watch.status().state, AgentState::Error);
        assert_eq!(watch.msg(), Some("API[31m error: limit reached"));

        // Clearing the record clears msg.
        assert!(watch.observe_program(None));
        assert_eq!(watch.msg(), None);
    }

    #[test]
    fn short_turn_ending_in_done_notifies_listener_even_when_status_unchanged() {
        use benterm_core::ProgramState;
        use std::cell::RefCell;
        use std::rc::Rc;

        let news = Rc::new(RefCell::new(Vec::new()));
        let recorded = Rc::clone(&news);
        let mut watch = AgentWatch::new(Some(Box::new(move |event| {
            recorded.borrow_mut().push(event);
        })));
        watch.observe_front(front(100, "claude"));
        news.borrow_mut().clear();

        // First turn completes and lands in Done.
        assert!(watch.observe_program(Some(record(1, 1, ProgramState::Done, None, None))));
        assert_eq!(news.borrow().len(), 1);
        news.borrow_mut().clear();

        // Another turn starts and finishes in Done between two observations (work increments).
        assert!(watch.observe_program(Some(record(2, 2, ProgramState::Done, None, None))));
        assert_eq!(
            news.borrow().as_slice(),
            &[AgentNews::Changed(watch.status())],
            "listener must be notified so unseen attention mark is re-applied"
        );
    }
}

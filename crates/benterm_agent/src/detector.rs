//! Following one pane's agent over time.

use std::path::Path;
use std::time::{Duration, Instant};

use benterm_core::{BlockKind, ProgramRecord, Row, Snapshot};

use crate::definition::{AgentDefinition, HookEvent, screen_lines};
use crate::state::{AgentState, WaitReason};

/// Follows the agent of one pane and decides its state.
///
/// Hook events are the agent's own report, so they change the state at
/// once. Screens are read while the agent redraws them, so a screen changes
/// the state only after it has shown the new state, with no contrary screen
/// and no accepted hook event in between, for the settle time. A screen
/// that shows no state at all is no evidence and leaves the state as it
/// is; a pending screen state survives such screens until it is due, and
/// is dropped if it is not shown then. A state that was still on the
/// screen when it was due takes effect even if the look at that time came
/// late: the next screen is weighed after it.
///
/// Waiting for the settle time keeps a single half-drawn frame, or a frame
/// that lags behind a hook event, from flipping the state. Because a screen
/// can still win after the settle time, a missing hook event, such as when
/// the user interrupts Claude Code, does not leave the state stuck, and a
/// wrong one is put right.
///
/// Only a hook event tells that a turn failed. The prompt the agent shows
/// afterwards is the same as after a turn that went well, so it leaves the
/// error as it is; the next turn ends it, by a hook event or on the screen.
///
/// Every program started in the pane inherits the pane's id, so hook
/// events also arrive from agents that are not the one in front: a
/// subagent, or an agent the one in front runs as a tool. Events of a
/// subagent are ignored. The session of the last event that told a state
/// owns the pane, or the one that first named the agent; an event of any other session is ignored in the middle of a turn
/// (working or waiting), and it never ends the session. Between turns any
/// session may take over, so an agent started after the previous one died
/// is not locked out; if that one died in the middle of a turn, the screen
/// ends the turn first.
///
/// The program in front of the pane's terminal, when the caller tells
/// it with [`observe_front`](Self::observe_front), decides whether the
/// screen may name an agent. While a program that is no agent is in
/// front, such as the shell, the screen names none: a banner left on the
/// screen by an agent that has ended, or restored from an earlier
/// session, is not an agent, and neither is a prompt that looks like an
/// agent's. When the agent in front gives way to another program, the
/// agent and its state are dropped at once, with or without a hook
/// telling that its session ended. Hooks still name an agent under any
/// program, because an agent started by a wrapper script sends them
/// while the script stays in front, so the first reading of a program
/// that is no agent drops only an agent the screen named. A front that
/// cannot be read is no evidence and changes nothing: the last reading
/// stands, and before any reading the detector works as if it had never
/// been told, unless the caller said it would read the front with
/// [`expect_front`](Self::expect_front).
///
/// One run of an agent is told from the next, so that the state and the
/// session of a run that has ended do not pass to the one started after
/// it. While an agent is in front, its process tells: another process of
/// the same agent, read with
/// [`observe_front_process`](Self::observe_front_process), is a new run
/// and starts with no state and no session, also when the shell between
/// the two was never read. Hooks of any other agent are ignored then,
/// whether a nested agent sent them or one that has left the front sent
/// them late; the agent last read in front decides this until another
/// program is read, so a reading that fails lets no such hook in. While
/// no agent is known to be in front, as under a wrapper
/// script, the hook that tells the start of a session tells: between
/// turns, the start of a session that is not the owner's drops the state
/// and makes that session the owner. In the middle of a turn it is
/// ignored like any hook of another session, because a nested agent
/// starts its session then. With an agent in front it is ignored too:
/// the process has not changed, so the session is a nested one. Hooks
/// do not tell which process sent them, so under a wrapper script a
/// nested agent that starts its session between turns is taken for the
/// next run, a run that died in the middle of a turn keeps the pane
/// until the screen ends the turn, and a late hook of a run that has
/// ended counts as before.
///
/// What the program tells of itself with OSC 7501, given with
/// [`observe_program`](Self::observe_program), comes before the hooks
/// and the screen: while the terminal holds a record of the program
/// itself, the record is the state. It says what the program is doing
/// now in the program's own words, where a hook tells one moment and may
/// come late, and the screen is read by rules. Three things the record
/// cannot tell are kept from the hooks and the screen: a question the
/// program tells is one of choices when they say so, a permission is
/// the trust of a folder when the screen says so, and a program at
/// rest has failed its turn when a hook said so. They hold whichever
/// came first, the report or the hook or screen. The hooks and the screen are
/// weighed behind the record as before, and decide again once the
/// record is gone. A report also counts as a hook does for them, so
/// that the choices of one question, the trust of the folder and the
/// failure of one turn do not stay for the next.
///
/// One thing a hook tells comes before the record. A hook that tells
/// the end of a turn, a failure or a wait for the user is newer than the
/// work the program had begun by then, so while the record still tells
/// that work, the hook decides, and the screen behind it as ever. Claude
/// Code tells that it works right after the hook of the end of its turn,
/// and goes on telling it, while a command it left running behind is not
/// over. The record decides again when the program begins to work anew,
/// which the terminal counts only when the program was not working
/// before, or when a hook tells that the agent works. Nothing else ends
/// it, and no time does: a turn at rest sends no hooks however long it
/// rests. The work is older than the hook only as far as the detector
/// was shown it before the hook, so a caller that takes a hook shows the
/// record first. A program that sends no hooks is never held so.
///
/// A state that only the record told goes with the record, as when the
/// program takes it back or the shell starts a prompt. The terminal
/// keeps only the last report, so each record tells how many times the
/// program has begun to work: work that began unseen ends the choices
/// and the failure of the turn before as well.
///
/// A program that is no agent has the state it tells, and no agent.
/// The record is the terminal's and names no process, so the run of
/// the agent decides whose it is: when the agent leaves the front,
/// another run of it comes, or another agent is named, what was told
/// before is no longer taken from the record, whatever the terminal
/// still holds. A record that came since the front was last read is
/// taken for the next program's, because the front is read now and
/// then and a program tells of itself as soon as it starts; an agent
/// that dies right after a report can so leave its last words to the
/// program after it, until that one tells of itself or the shell
/// starts a prompt. Programs that are no agents are not told apart:
/// their records end as the terminal ends them.
///
/// The state changes only inside the `observe_*` calls, so a caller that
/// wants a pending screen state to take effect looks at the screen again
/// at [`pending_deadline`](Self::pending_deadline), even when nothing was
/// printed in the meantime.
pub struct Detector {
    definitions: Vec<AgentDefinition>,
    settle: Duration,
    /// Index into `definitions` of the recognised agent.
    agent: Option<usize>,
    state: AgentState,
    /// A state the screen shows that differs from `state`, and the time it
    /// was first shown without interruption.
    pending: Option<(AgentState, Instant)>,
    /// The last screen looked at showed the pending state.
    pending_on_screen: bool,
    /// The session of the last accepted hook event, when it named one.
    owner: Option<String>,
    /// The agent whose session ended while its banner may still be on the
    /// screen. That banner does not name the agent again.
    left_banner: Option<usize>,
    /// The screen named the agent, and neither a hook nor the front has
    /// told of it since.
    named_by_screen: bool,
    /// What was last read to be in front of the terminal.
    front: Front,
    /// The process id of the program last read in front, when the caller
    /// told it.
    front_pid: Option<u32>,
    /// What the program tells of itself now, unless its run is over.
    program: Option<Told>,
    /// The serial of the last record taken.
    program_serial: Option<u64>,
    /// How many times the program had begun to work by the last record
    /// taken.
    program_work: u64,
    /// How many records told that the program began to work.
    work_begun: u64,
    /// How many times a turn began, waited for the user, or stopped by
    /// an error: by OSC 7501 work begun, an accepted hook telling one
    /// of those states, or the state changing into one of them.
    turns_begun: u64,
    /// A record was taken since the front was last read.
    told_since_front: bool,
    /// `state` is as a report of the program left it: neither a hook
    /// nor the screen has set it since.
    state_by_program: bool,
    /// How many times the program had begun to work when a hook last
    /// told that the turn is over or waits for the user. `None` once a
    /// hook told that the agent works, and for another run.
    rested_at_work: Option<u64>,
}

/// What a program told of itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Told {
    state: AgentState,
    /// It told that it has a question, which may be one of choices.
    question: bool,
}

impl Told {
    /// The state, with what only the hooks and the screen can tell, by
    /// which the state is `behind`.
    fn refined_by(self, behind: AgentState) -> AgentState {
        let choice = AgentState::Waiting(WaitReason::Choice);
        let permission = AgentState::Waiting(WaitReason::Permission);
        let trust = AgentState::Waiting(WaitReason::Trust);
        if self.question && behind == choice {
            choice
        } else if self.state == permission && behind == trust {
            trust
        } else if self.state == AgentState::Done && behind == AgentState::Error {
            AgentState::Error
        } else {
            self.state
        }
    }
}

/// The program in front of the terminal, as far as the detector knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Front {
    /// Never read.
    Unread,
    /// Never read, and the caller will read it: see
    /// [`Detector::expect_front`].
    Expected,
    /// The agent of `definitions[index]`.
    Agent(usize),
    /// A program that is no agent.
    Other,
}

/// Programs that run a script named by their first argument that is not
/// an option. The agents installed with npm run as such scripts.
const INTERPRETERS: &[&str] = &["node", "bun", "deno", "sh", "bash", "zsh", "dash"];

/// Options of [`INTERPRETERS`] that take code instead of a script.
const CODE_OPTIONS: &[&str] = &["-c", "-e", "--eval", "-p", "--print"];

/// The command a command line runs: the name `argv[0]` gives, or the
/// script an interpreter runs. `None` for code given on the command line.
fn command_of(args: &[String]) -> Option<&str> {
    let first = file_name(args.first()?);
    if !INTERPRETERS.contains(&first) {
        return Some(first);
    }
    for arg in &args[1..] {
        if CODE_OPTIONS.contains(&arg.as_str()) {
            return None;
        }
        if !arg.starts_with('-') {
            return Some(file_name(arg));
        }
    }
    None
}

/// The last part of a path, or `path` itself.
fn file_name(path: &str) -> &str {
    Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(path)
}

impl Detector {
    /// How long a screen has to keep showing a new state before the state
    /// changes.
    pub const DEFAULT_SETTLE: Duration = Duration::from_millis(500);

    pub fn new(definitions: Vec<AgentDefinition>) -> Self {
        Self::with_settle(definitions, Self::DEFAULT_SETTLE)
    }

    pub fn with_settle(definitions: Vec<AgentDefinition>, settle: Duration) -> Self {
        Self {
            definitions,
            settle,
            agent: None,
            state: AgentState::Unknown,
            pending: None,
            pending_on_screen: false,
            owner: None,
            left_banner: None,
            named_by_screen: false,
            front: Front::Unread,
            front_pid: None,
            program: None,
            program_serial: None,
            program_work: 0,
            work_begun: 0,
            turns_begun: 0,
            told_since_front: false,
            state_by_program: false,
            rested_at_work: None,
        }
    }

    /// Names the agent by the executable that started it, given as a name or
    /// a path. Returns whether a definition lists the command; when none
    /// does, nothing changes.
    pub fn identify_command(&mut self, command: &str) -> bool {
        let found = self.definition_of(file_name(command));
        if let Some(index) = found {
            self.set_agent(index);
        }
        found.is_some()
    }

    /// Tells that the program in front will be read: until a reading
    /// succeeds, the screen names no agent.
    ///
    /// A caller that reads the front should call this first. Otherwise a
    /// first reading that fails leaves a banner already on the screen,
    /// such as one restored from an earlier session, free to name an
    /// agent until the next reading.
    pub fn expect_front(&mut self) {
        if self.front == Front::Unread {
            self.front = Front::Expected;
        }
    }

    /// Takes the program in front of the pane's terminal: the arguments
    /// its first process was started with, `argv[0]` first, or `None`
    /// when they could not be read.
    ///
    /// An agent coming to the front is named at once. The agent in front
    /// giving way to a program that is no agent ends it, as the end of
    /// its session does. The same program in front again, or a reading
    /// that failed, changes nothing.
    ///
    /// A caller that knows the process id of the program tells it with
    /// [`observe_front_process`](Self::observe_front_process) instead.
    pub fn observe_front(&mut self, args: Option<&[String]>) {
        if let Some(args) = args {
            self.take_front(None, args);
            self.told_since_front = false;
        }
    }

    /// Like [`observe_front`](Self::observe_front) for a reading that
    /// went well, with the process id of the program in front. Another
    /// process of the agent that was in front before is a new run of it:
    /// it starts with no state and no session.
    pub fn observe_front_process(&mut self, pid: u32, args: &[String]) {
        self.take_front(Some(pid), args);
        self.told_since_front = false;
    }

    fn take_front(&mut self, pid: Option<u32>, args: &[String]) {
        let front = match command_of(args).and_then(|command| self.definition_of(command)) {
            Some(index) => Front::Agent(index),
            None => Front::Other,
        };
        if front == self.front {
            let Some(pid) = pid else {
                return;
            };
            let new_process = self.front_pid.is_some_and(|before| before != pid);
            self.front_pid = Some(pid);
            if let Front::Agent(index) = front
                && new_process
            {
                self.start_run(index);
            }
            return;
        }
        match front {
            Front::Agent(index) => {
                self.set_agent(index);
                self.named_by_screen = false;
            }
            Front::Other => {
                let gave_way = matches!(self.front, Front::Agent(_));
                if let Some(index) = self.agent
                    && (gave_way || self.named_by_screen)
                {
                    self.forget_agent(index);
                }
            }
            Front::Unread | Front::Expected => unreachable!("not made from arguments"),
        }
        self.front = front;
        self.front_pid = pid;
    }

    /// The definition that lists `command`.
    fn definition_of(&self, command: &str) -> Option<usize> {
        self.definitions
            .iter()
            .position(|definition| definition.commands.contains(&command))
    }

    pub fn observe_screen(&mut self, screen: &Snapshot, now: Instant) {
        self.observe_rows(&screen.rows, now);
    }

    /// Like [`observe_screen`](Self::observe_screen), for a caller that
    /// holds the rows of the screen and no snapshot.
    pub fn observe_rows(&mut self, rows: &[Row], now: Instant) {
        let mut state_before = self.state();
        // The look when the pending state was due came late, and the
        // screen showed the state until now: it took effect then, before
        // this screen came.
        if self.pending_on_screen
            && let Some((pending, since)) = self.pending
            && now.saturating_duration_since(since) > self.settle
        {
            self.state = pending;
            self.state_by_program = false;
            self.pending = None;
            let state_after = self.state();
            if state_after != state_before && is_turn_state(state_after) {
                self.turns_begun += 1;
            }
            state_before = state_after;
        }
        let shown = self.weigh_rows(rows, now);
        self.pending_on_screen = shown.is_some() && self.pending.map(|(state, _)| state) == shown;
        let state_after = self.state();
        if state_after != state_before && is_turn_state(state_after) {
            self.turns_begun += 1;
        }
    }

    /// Weighs a screen against the state. Returns the state the screen
    /// shows, if it shows one.
    fn weigh_rows(&mut self, rows: &[Row], now: Instant) -> Option<AgentState> {
        let lines = screen_lines(rows);
        if self.agent.is_none() {
            if matches!(self.front, Front::Other | Front::Expected) {
                return None;
            }
            if let Some(left) = self.left_banner
                && !self.definitions[left].banner_in(&lines)
            {
                self.left_banner = None;
            }
            let left = self.left_banner;
            let index = self
                .definitions
                .iter()
                .enumerate()
                .position(|(index, definition)| {
                    Some(index) != left && definition.banner_in(&lines)
                })?;
            self.set_agent(index);
            self.named_by_screen = true;
        }
        let Some(shown) = self.agent().and_then(|agent| agent.state_in(&lines)) else {
            // No evidence, so nothing changes. But a pending state has to
            // be on the screen when it is due: two half-drawn frames with
            // nothing telling in between do not add up to a state.
            if self
                .pending_deadline()
                .is_some_and(|deadline| now >= deadline)
            {
                self.pending = None;
            }
            return None;
        };
        // After a turn that failed the agent shows its prompt, as after
        // one that went well, so that screen has nothing to add.
        let failed_and_at_prompt = self.state == AgentState::Error && shown == AgentState::Done;
        if shown == self.state || failed_and_at_prompt {
            self.pending = None;
            return Some(shown);
        }
        match self.pending {
            Some((pending, since)) if pending == shown => {
                if now.saturating_duration_since(since) >= self.settle {
                    self.state = shown;
                    self.state_by_program = false;
                    self.pending = None;
                }
            }
            _ => self.pending = Some((shown, now)),
        }
        Some(shown)
    }

    /// When the state the screen shows now takes effect, if the screen
    /// keeps showing it: the time to look at the screen again. `None` when
    /// the screen and the state agree.
    pub fn pending_deadline(&self) -> Option<Instant> {
        self.pending.map(|(_, since)| since + self.settle)
    }

    pub fn observe_hook(&mut self, event: &HookEvent, _now: Instant) {
        let Some(index) = self
            .definitions
            .iter()
            .position(|definition| definition.id == event.agent)
        else {
            return;
        };
        if event.subagent {
            return;
        }
        // The agent in front is the pane's agent. A hook of another one
        // comes from an agent it runs as a tool, or late from one that
        // has left the front.
        if let Front::Agent(front) = self.front
            && front != index
        {
            return;
        }
        let reported = self.definitions[index].hook_state(event);
        let ends_session = reported == Some(AgentState::Unknown);
        if ends_session {
            // Only the session that owns the pane takes the agent away.
            if self.agent == Some(index) && event.session == self.owner {
                self.forget_agent(index);
            }
            return;
        }
        let other_agent = self.agent.is_some_and(|agent| agent != index);
        let other_session = self.owner.is_some() && event.session != self.owner;
        let in_a_turn = matches!(self.state, AgentState::Working | AgentState::Waiting(_));
        if in_a_turn && (other_agent || other_session) {
            return;
        }
        let Some(state) = reported else {
            // An event that tells no state, such as the start of a
            // session, names the agent when none is known. It takes the
            // pane from nobody.
            if self.agent.is_none() {
                self.set_agent(index);
                self.owner.clone_from(&event.session);
                self.named_by_screen = false;
                return;
            }
            // The start of a session that is not the owner's is the
            // start of the next run, where no agent in front tells the
            // runs apart by its process. A start that names no session
            // tells nothing about whose it is.
            let front_tells = matches!(self.front, Front::Agent(_));
            if self.definitions[index].starts_session(event)
                && event.session.is_some()
                && !front_tells
                && (other_agent || other_session)
            {
                self.start_run(index);
                self.owner.clone_from(&event.session);
            }
            return;
        };
        self.set_agent(index);
        self.owner.clone_from(&event.session);
        self.named_by_screen = false;
        self.state = state;
        self.state_by_program = false;
        self.pending = None;
        // The work the program began before this hook is over, or waits
        // for the user.
        self.rested_at_work = (state != AgentState::Working).then_some(self.program_work);
        if is_turn_state(state) && is_turn_state(self.state()) {
            self.turns_begun += 1;
        }
    }

    /// Takes what the terminal holds of the program itself: its last
    /// report with OSC 7501, or `None` when it holds none.
    ///
    /// The same record given again changes nothing.
    pub fn observe_program(&mut self, record: Option<&ProgramRecord>) {
        let Some(record) = record else {
            self.program = None;
            // What only the record told goes with it.
            if self.state_by_program {
                self.state = AgentState::Unknown;
                self.pending = None;
                self.state_by_program = false;
            }
            return;
        };
        if self.program_serial == Some(record.serial) {
            return;
        }
        let state_before = self.state();
        self.program_serial = Some(record.serial);
        self.told_since_front = true;
        let work_began = record.work != self.program_work;
        self.program_work = record.work;
        self.work_begun += u64::from(work_began);
        let told = Told {
            state: AgentState::of_program(&record.status),
            question: record.status.kind == Some(BlockKind::Question),
        };
        self.program = Some(told);
        if self.agent.is_some() && !self.told_before_hook(told) {
            // Behind the record, a report counts as a hook of the agent
            // does, except where it would take away what only they tell.
            // Work that began since the last record seen has ended what
            // they told of the turn before, whatever this report tells.
            if work_began {
                self.state = AgentState::Working;
            }
            if work_began || told.refined_by(self.state) == told.state {
                self.state = told.state;
                self.pending = None;
                self.state_by_program = true;
            }
        }
        let state_after = self.state();
        if work_began || (state_after != state_before && is_turn_state(state_after)) {
            self.turns_begun += 1;
        }
    }

    /// How many records the detector was shown told that the program
    /// began to work, whoever's run it was and whatever the state is now.
    ///
    /// The terminal keeps only the last report, so work that began and
    /// ended between two looks leaves the state as it was: the count
    /// tells that a turn went by all the same.
    pub fn work_begun(&self) -> u64 {
        self.work_begun
    }

    /// How many times a turn began, asked the user, or stopped by an
    /// error: each time OSC 7501 told that work began, each accepted
    /// hook that told working, waiting or an error while the state is
    /// one of those, and each transition of [`state`](Self::state) into
    /// one of those.
    pub fn turns_begun(&self) -> u64 {
        self.turns_begun
    }

    /// The run of the program in front is over, and with it what it told
    /// of itself, whatever the terminal still holds. A report that came
    /// since the front was last read is taken for the next program's:
    /// the front is read now and then, and a program tells of itself as
    /// soon as it starts. Not so work that a hook of this run holds
    /// back: the run told it before.
    fn end_program_run(&mut self) {
        // Work a hook of this run held back is this run's, whenever it
        // was told again.
        let held_back = self.program.is_some_and(|told| self.told_before_hook(told));
        if !self.told_since_front || held_back {
            self.program = None;
        }
        self.rested_at_work = None;
    }

    /// The recognised agent, if any.
    pub fn agent(&self) -> Option<&AgentDefinition> {
        self.agent.map(|index| &self.definitions[index])
    }

    /// The recognised agent, if the last reading of the program in front
    /// that went well found a process of it there, with the id of that
    /// process. `None` for an agent only the screen or a hook names, and
    /// for a reading that told no process id.
    pub fn agent_in_front(&self) -> Option<(&AgentDefinition, u32)> {
        let Front::Agent(index) = self.front else {
            return None;
        };
        let agent = self.agent().filter(|_| self.agent == Some(index))?;
        Some((agent, self.front_pid?))
    }

    pub fn state(&self) -> AgentState {
        match self.program {
            Some(told) if !self.told_before_hook(told) => told.refined_by(self.state),
            _ => self.state,
        }
    }

    /// `told`, which the record tells now, is work the program began
    /// before a hook told that the turn is over or waits for the user.
    fn told_before_hook(&self, told: Told) -> bool {
        told.state == AgentState::Working && self.rested_at_work == Some(self.program_work)
    }

    /// The session of the agent in front: the one of the last hook event
    /// that told a state, or that first named the agent. `None` once that
    /// session has ended, and while no hook event has named one.
    pub fn owner(&self) -> Option<&str> {
        self.owner.as_deref()
    }

    /// Drops the agent `definitions[index]`, whose banner may stay on the
    /// screen.
    fn forget_agent(&mut self, index: usize) {
        self.agent = None;
        self.state = AgentState::Unknown;
        self.state_by_program = false;
        self.pending = None;
        self.owner = None;
        self.left_banner = Some(index);
        self.named_by_screen = false;
        self.end_program_run();
    }

    /// Starts a new run of the agent `definitions[index]`: nothing the
    /// run before it told is kept.
    fn start_run(&mut self, index: usize) {
        self.agent = Some(index);
        self.state = AgentState::Unknown;
        self.state_by_program = false;
        self.pending = None;
        self.owner = None;
        self.left_banner = None;
        self.named_by_screen = false;
        self.end_program_run();
    }

    /// Switches to another agent and forgets what the previous one showed.
    fn set_agent(&mut self, index: usize) {
        if self.agent != Some(index) {
            self.end_program_run();
            self.agent = Some(index);
            self.state = AgentState::Unknown;
            self.state_by_program = false;
            self.pending = None;
            self.owner = None;
            self.left_banner = None;
        }
    }
}

/// Whether `state` shows that a turn began, asked the user, or stopped
/// by an error.
fn is_turn_state(state: AgentState) -> bool {
    matches!(
        state,
        AgentState::Working | AgentState::Waiting(_) | AgentState::Error
    )
}

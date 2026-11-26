use benterm_core::{BlockKind, ProgramState, ProgramStatus};

/// What an agent is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AgentState {
    Working,
    /// Stopped in the middle of a turn until the user answers.
    Waiting(WaitReason),
    /// Not working and not asking anything: a turn has finished, or no turn
    /// has started yet, and the agent waits at its prompt.
    Done,
    /// The last turn did not finish: the agent's hooks said that it
    /// failed, or that it was interrupted or refused. The agent waits at
    /// its prompt, as when it is done.
    Error,
    /// No agent is recognised, or nothing has shown its state yet.
    Unknown,
}

/// What a waiting agent wants from the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WaitReason {
    /// Approval to run a tool.
    Permission,
    /// Approval to work in a folder the agent does not trust yet, asked
    /// at start-up. Its dialog takes other keys than a tool's approval.
    Trust,
    /// An answer to a question the agent asked, which may be text of
    /// the user's own: one whose kind no rule can tell, such as Codex's
    /// request_user_input, which takes text besides its choices.
    Question,
    /// One of the choices of a question that takes nothing else, such as
    /// Claude Code's AskUserQuestion.
    Choice,
}

impl AgentState {
    /// The state a program is in by what it told of itself with OSC
    /// 7501.
    ///
    /// A program at rest is done, whether it has just finished or waits
    /// for the next instruction. A program blocked by a login needs
    /// something typed, like one with a question, and so does one that
    /// did not say what blocks it: only a permission is answered by
    /// allowing or denying.
    pub fn of_program(status: &ProgramStatus) -> Self {
        match (status.state, status.kind) {
            (ProgramState::Working, _) => Self::Working,
            (ProgramState::Done | ProgramState::Idle, _) => Self::Done,
            (ProgramState::Error, _) => Self::Error,
            (ProgramState::Blocked, Some(BlockKind::Permission)) => {
                Self::Waiting(WaitReason::Permission)
            }
            (ProgramState::Blocked, Some(BlockKind::Question | BlockKind::Auth) | None) => {
                Self::Waiting(WaitReason::Question)
            }
        }
    }
}

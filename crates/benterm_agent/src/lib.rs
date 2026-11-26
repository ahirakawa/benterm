//! Agent definitions map terminal snapshots and hook events to agent identity and state.
//! To support another agent, add an [`AgentDefinition`]. The [`Detector`] uses
//! caller-supplied observation times and has no GPUI dependency.

mod agents;
mod definition;
mod detector;
mod plan;
mod state;

pub use crate::agents::{builtin, claude_code, codex};
pub use crate::definition::{AgentDefinition, HookEvent, HookRule, Matcher, Region, ScreenRule};
pub use crate::detector::Detector;
pub use crate::plan::{Plan, PlanCounts, PlanItem, PlanStatus, PlanTracker, PlanUpdate};
pub use crate::state::{AgentState, WaitReason};

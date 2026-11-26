//! TOML theme and settings shared by the app and renderer, independent of GPUI
//! and the terminal core.

mod color;
mod file;
mod ipc;
mod pty_log;
mod read;
mod reload;
mod report;
mod role;
mod schema;
mod session;
mod stall;
mod watch;
mod worktrees;

pub use color::{ColorError, Rgba};
pub use file::{config_path, config_path_from, load_file};
pub use ipc::IpcSettings;
pub use pty_log::PtyLogSettings;
pub use read::{Loaded, Problem, Warning, load, parse};
pub use reload::{ConfigEvent, Reloader};
pub use report::ReportSettings;
pub use role::{
    Agent, Role, Roles, RolesWatcher, codex_takes_no_daemon, fit_codex_launch, load_roles,
    roles_dir, roles_dir_from, worker_rules_file, worker_rules_line,
};
pub use schema::{
    BoardColors, ButtonColors, Colors, Config, DiffColors, FONT_SIZE_RANGE, Font, InputColors,
    LINE_HEIGHT_RANGE, PADDING_RANGE, Palette, PaneColors, SidebarColors, StatusColors,
    StatusbarColors, TabColors, TaskAgent, TaskSettings, TerminalColors, TerminalSettings,
    TitlebarColors, UiSettings, setting_keys,
};
pub use session::{SCROLLBACK_LINES_RANGE, SessionSettings};
pub use stall::{STALL_SECONDS_RANGE, StallSettings};
pub use watch::ConfigWatcher;
pub use worktrees::WorktreeSettings;

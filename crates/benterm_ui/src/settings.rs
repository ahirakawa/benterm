//! The config in use, as the theme the views draw with and the messages
//! the user should see about the config file.

use std::io;
use std::path::Path;

use benterm_config::{
    ConfigEvent, ConfigWatcher, Loaded, PtyLogSettings, Role, Roles, RolesWatcher, StallSettings,
    TaskAgent, Warning, WorktreeSettings, config_path, load_file, load_roles,
};
use futures::channel::mpsc::{UnboundedReceiver, unbounded};

use crate::theme::Theme;

/// Follows the events of the config file.
#[derive(Debug, Clone)]
pub struct Settings {
    theme: Theme,
    /// Read when a terminal starts; a change applies to later terminals.
    pty_log: PtyLogSettings,
    /// Read at every save of the session.
    scrollback_lines: usize,
    /// Read when a task is made; a task keeps the place it was made at.
    worktrees: WorktreeSettings,
    default_task_agent: Option<TaskAgent>,
    /// Read at every look for stalled agents.
    stall: StallSettings,
    /// Whether the app checks the reports of agents. Read when a
    /// terminal starts.
    report_check: bool,
    /// Whether the socket API asks every request for a credential.
    /// Read when the app starts and at every save of the file.
    ipc_restricted: bool,
    /// Problems with the config in use.
    warnings: Vec<Warning>,
    /// Why the last saved file was not used, until a file is used again.
    rejected: Option<Warning>,
    /// Why the file is not watched, if it is not.
    not_watched: Option<String>,
    /// The roles of the roles directory, in order of name.
    roles: Vec<Role>,
    /// The files of the roles directory that are not roles.
    role_warnings: Vec<Warning>,
}

impl Settings {
    pub fn new(loaded: Loaded) -> Self {
        Self {
            theme: Theme::from_config(&loaded.config),
            pty_log: loaded.config.pty_log,
            scrollback_lines: loaded.config.session.scrollback_lines,
            worktrees: loaded.config.worktrees,
            default_task_agent: loaded.config.tasks.default_agent,
            stall: loaded.config.stall,
            report_check: loaded.config.report.enabled,
            ipc_restricted: loaded.config.ipc.restricted,
            warnings: loaded.warnings,
            rejected: None,
            not_watched: None,
            roles: Vec::new(),
            role_warnings: Vec::new(),
        }
    }

    /// The roles the window can start, in order of name.
    pub fn roles(&self) -> &[Role] {
        &self.roles
    }

    pub(crate) fn default_task_agent(&self) -> Option<&TaskAgent> {
        self.default_task_agent.as_ref()
    }

    /// Takes the roles of the roles directory as it is now.
    pub fn set_roles(&mut self, roles: Roles) {
        self.roles = roles.roles;
        self.role_warnings = roles.warnings;
    }

    pub fn theme(&self) -> &Theme {
        &self.theme
    }

    /// Whether and where to record the PTY log of a terminal.
    pub fn pty_log(&self) -> &PtyLogSettings {
        &self.pty_log
    }

    /// How many rows of each pane are saved with the session and shown
    /// again at the next start; 0 for none.
    pub fn scrollback_lines(&self) -> usize {
        self.scrollback_lines
    }

    /// The directory the worktrees of tasks go under, for the home
    /// directory `home`.
    pub fn worktree_root(&self, home: Option<&Path>) -> Option<std::path::PathBuf> {
        self.worktrees.root(home)
    }

    /// How long an agent at work may go with neither its screen nor its
    /// files changing before it is marked as stalled.
    pub fn stall_threshold(&self) -> std::time::Duration {
        self.stall.threshold()
    }

    /// Whether the app checks the reports of agents (`[report]
    /// enabled`).
    pub fn checks_reports(&self) -> bool {
        self.report_check
    }

    /// Whether the socket API asks every request for a credential
    /// (`[ipc] restricted`).
    pub fn restricts_ipc(&self) -> bool {
        self.ipc_restricted
    }

    /// Takes an event of the config file. Returns whether the theme
    /// changed.
    pub fn apply(&mut self, event: ConfigEvent) -> bool {
        match event {
            ConfigEvent::Applied(loaded) => {
                self.rejected = None;
                self.warnings = loaded.warnings;
                let theme = Theme::from_config(&loaded.config);
                self.pty_log = loaded.config.pty_log;
                self.scrollback_lines = loaded.config.session.scrollback_lines;
                self.worktrees = loaded.config.worktrees;
                self.default_task_agent = loaded.config.tasks.default_agent;
                self.stall = loaded.config.stall;
                self.report_check = loaded.config.report.enabled;
                self.ipc_restricted = loaded.config.ipc.restricted;
                let changed = theme != self.theme;
                self.theme = theme;
                changed
            }
            ConfigEvent::Rejected(warning) => {
                self.rejected = Some(warning);
                false
            }
        }
    }

    /// Records that saving the file will not change anything.
    pub fn not_watched(&mut self, error: &io::Error) {
        self.not_watched = Some(format!(
            "changes to the config file are not applied until restart: {error}"
        ));
    }

    /// What to tell the user, most important first.
    pub fn messages(&self) -> Vec<String> {
        let rejected = self
            .rejected
            .iter()
            .map(|w| format!("{w}. The previous settings stay in use."));
        rejected
            .chain(self.not_watched.iter().cloned())
            .chain(self.warnings.iter().map(Warning::to_string))
            .chain(self.role_warnings.iter().map(Warning::to_string))
            .collect()
    }
}

/// Where a window gets its settings: the config at startup and the events
/// of each later save.
pub struct ConfigFeed {
    pub(crate) settings: Settings,
    pub(crate) events: Option<UnboundedReceiver<ConfigEvent>>,
    /// Kept alive as long as the window that watches.
    pub(crate) watcher: Option<ConfigWatcher>,
    /// The roles after each change of the roles directory.
    pub(crate) role_events: Option<UnboundedReceiver<Roles>>,
    pub(crate) roles_watcher: Option<RolesWatcher>,
}

impl ConfigFeed {
    /// The default settings, never changing.
    pub fn defaults() -> Self {
        Self::new(Loaded::default())
    }

    /// Fixed settings, never changing.
    pub fn new(loaded: Loaded) -> Self {
        Self {
            settings: Settings::new(loaded),
            events: None,
            watcher: None,
            role_events: None,
            roles_watcher: None,
        }
    }

    /// The settings of the file at `path`, followed as the file is saved,
    /// and the roles of the `roles` directory beside it, followed as the
    /// directory changes. If the file cannot be watched, it is read once
    /// and the reason is one of the messages.
    pub fn watch(path: &Path) -> Self {
        let (sender, events) = unbounded();
        let on_event = move |event| {
            // The receiver is gone only after the window has closed.
            let _ = sender.unbounded_send(event);
        };
        let mut feed = match ConfigWatcher::start(path, on_event) {
            Ok((watcher, loaded)) => Self {
                events: Some(events),
                watcher: Some(watcher),
                ..Self::new(loaded)
            },
            Err(error) => {
                let mut feed = Self::new(load_file(path));
                feed.settings.not_watched(&error);
                feed
            }
        };
        let roles = path.parent().unwrap_or(Path::new(".")).join("roles");
        let (sender, role_events) = unbounded();
        let on_change = move |roles| {
            let _ = sender.unbounded_send(roles);
        };
        match RolesWatcher::start(&roles, on_change) {
            Ok((watcher, loaded)) => {
                feed.settings.set_roles(loaded);
                feed.role_events = Some(role_events);
                feed.roles_watcher = Some(watcher);
            }
            // The config file's watcher has made the same directory, so
            // what keeps this one from being watched is rare: the roles
            // there now are still listed.
            Err(_) => feed.settings.set_roles(load_roles(&roles)),
        }
        feed
    }

    /// The file of [`config_path`], or the defaults when there is no home
    /// directory to find it in.
    pub fn user_config() -> Self {
        config_path().map_or_else(Self::defaults, |path| Self::watch(&path))
    }

    pub fn settings(&self) -> &Settings {
        &self.settings
    }
}

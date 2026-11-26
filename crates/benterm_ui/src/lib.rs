//! GPUI views for Benterm.

mod agent_feed;
mod agent_graph;
mod agent_history;
mod agent_watch;
mod agents;
pub mod attention;
pub mod browser;
pub mod checked_reports;
pub mod command_match;
mod command_palette;
mod context_menu;
pub mod design;
pub mod diff_view;
mod file_browser;
pub mod file_preview;
pub mod file_tree;
pub mod graph;
mod icons;
pub mod ime;
pub mod input;
pub mod ipc;
pub mod issue_graph;
pub mod issue_graph_view;
pub mod kanban;
pub mod layout;
pub mod links;
pub mod local_issue_form;
pub mod local_issues;
pub mod login_env;
pub mod mailbox;
mod main_view;
pub mod markdown;
pub mod menu;
pub mod name_editor;
mod pane_area;
mod pane_grid;
mod peek;
mod progress_board;
pub mod recording;
pub mod redraw_stats;
mod report_panel;
pub mod review;
mod roles;
pub mod selection;
pub mod session;
mod session_list;
pub mod session_rows;
pub mod settings;
mod sidebar;
pub mod sidebar_rows;
mod stall;
mod tab_group;
mod task_form;
mod tasks;
pub mod terminal_search;
mod terminal_view;
pub mod theme;
pub mod timeline;
pub mod toast;
pub mod tooltip;
pub mod viewport;

use std::path::{Path, PathBuf};
use std::rc::Rc;

use benterm_config::ConfigEvent;
use benterm_layout::{
    PaneId, SplitDirection, WorkspaceColor, WorkspaceError, WorkspaceId, WorkspaceList,
};
use benterm_terminal::PtyConfig;
use futures::StreamExt;
use gpui::{
    App, AppContext, Bounds, Context, Entity, EntityId, FontWeight, InteractiveElement,
    IntoElement, MouseButton, ParentElement, Render, Styled, Subscription, TitlebarOptions, Window,
    WindowBounds, WindowHandle, WindowOptions, div, px, size,
};

use crate::browser::BrowserError;
use crate::name_editor::{NameEditor, NameEditorEvent};

pub use crate::agent_feed::{AgentFeed, FeedListener};
pub use crate::agent_watch::{AgentListener, AgentNews, AgentStatus};
pub use crate::agents::AgentRow;
pub use crate::command_palette::{
    CommandPalette, SelectNextCommand, SelectPreviousCommand, ShownCommand, ToggleCommandPalette,
};

pub use crate::context_menu::{ContextMenu, MenuItemShown};
pub use crate::file_browser::{FileBrowser, FileReader, FsReader, ToggleFileBrowser};
pub use crate::issue_graph_view::{
    EpicReader, EpicTarget, IssueGraphView, NewLocalChildIssue, NewLocalEpic, OpenEpic,
    OpenLocalEpic, OpenLocalIssues, OpenRepositoryIssues,
};
pub use crate::main_view::{
    MainView, ShowTerminals, ToggleDiff, ToggleIssues, ToggleProgressBoard,
};
pub use crate::pane_grid::{
    FirstPane, FocusDown, FocusLeft, FocusRight, FocusUp, PaneGrid, PaneGridEvent, ResizeDown,
    ResizeLeft, ResizeRight, ResizeUp, RestoredPanes, SavedPane, SpawnTerminal, SplitDown,
    SplitRight, ToggleZoom,
};
pub use crate::peek::{ClosePeek, OpenPeekAsTab, PeekNextAgent};
pub use crate::progress_board::{BoardMode, ProgressBoard};
pub use crate::report_panel::{CloseReport, OpenReport};
pub use crate::roles::LaunchRole;
pub use crate::session_list::{CloseSession, FocusReply};
use crate::settings::{ConfigFeed, Settings};
pub use crate::sidebar::directory_label;
pub use crate::sidebar::{
    CloseWorkspace, FocusSidebar, NewWorkspace, RenameWorkspace, SelectNextWorkspace,
    SelectPreviousWorkspace, ToggleSidebarCollapse,
};
pub use crate::stall::StallMark;
pub use crate::tab_group::{
    Close, CloseOtherTabs, GotoTab, NewTab, RenameTab, SelectLastTab, SelectNextTab,
    SelectPreviousTab, TabGroup, TabGroupEvent,
};
pub use crate::task_form::{TaskForm, TaskFormValues};
pub use crate::tasks::{IssueAgentState, NewTask, TaskRow};
pub use crate::terminal_search::{FindNext, FindPrevious, OpenSearch};
use crate::terminal_view::hsla;
pub use crate::terminal_view::{
    Copy, Paste, SYNC_UPDATE_TIMEOUT, StartedTerminal, TerminalEvent, TerminalView,
};

/// The application name shown to the user.
pub const APP_NAME: &str = "Benterm";

/// Binds the app's keys. Every one of them has command held, which is
/// what keeps them apart from the keys that go to the program.
pub fn bind_keys(cx: &mut App) {
    browser::bind_keys(cx);
    command_palette::bind_keys(cx);
    context_menu::bind_keys(cx);
    file_browser::bind_keys(cx);
    main_view::bind_keys(cx);
    menu::bind_keys(cx);
    name_editor::bind_keys(cx);
    local_issue_form::bind_keys(cx);
    pane_grid::bind_keys(cx);
    peek::bind_keys(cx);
    report_panel::bind_keys(cx);
    progress_board::bind_keys(cx);
    diff_view::bind_keys(cx);
    issue_graph_view::bind_keys(cx);
    session_list::bind_keys(cx);
    sidebar::bind_keys(cx);
    tab_group::bind_keys(cx);
    task_form::bind_keys(cx);
    terminal_search::bind_keys(cx);
    terminal_view::bind_keys(cx);
}

/// What a workspace holds in a window: its tabs.
struct WorkspaceTabs {
    tabs: Entity<TabGroup>,
    /// The number of the task the workspace shows, if it shows one. A
    /// workspace that stays when its task is closed shows none.
    task: std::cell::Cell<Option<u64>>,
    /// The window follows the events of the tabs and redraws the sidebar
    /// when they change.
    _subscriptions: [Subscription; 2],
}

/// A workspace whose name is being typed.
struct Renaming {
    id: WorkspaceId,
    editor: Entity<NameEditor>,
    _subscriptions: [Subscription; 2],
}

/// The root view of a Benterm window: the sidebar with its workspaces, the
/// tabs of the selected workspace, and a bar above them for problems with
/// the config file.
pub struct BentermWindow {
    workspaces: WorkspaceList<WorkspaceTabs>,
    /// Starts the first terminal of a workspace made from the window, and
    /// the terminals opened in it later.
    spawn: SpawnTerminal,
    renaming: Option<Renaming>,
    sidebar: sidebar::SidebarState,
    palette: Option<command_palette::OpenPalette>,
    /// The right-click menu, while it is open.
    context_menu: Option<context_menu::OpenContextMenu>,
    files: Option<file_browser::OpenFileBrowser>,
    /// What the pane area shows, chosen with the tabs above it.
    main_view: MainView,
    /// The progress board, once it has been shown.
    board: Option<main_view::OpenBoard>,
    /// The diff of a task, once it has been shown.
    diff: Option<main_view::OpenDiff>,
    /// The Epic and child-issue graph, once it has been shown.
    issues: Option<main_view::OpenIssues>,
    /// The floating Open Epic prompt, while it is open.
    epic_prompt: Option<issue_graph_view::OpenEpicPrompt>,
    /// The GitHub reader used by the Issues tab.
    epic_reader: std::sync::Arc<dyn issue_graph_view::EpicReader>,
    /// The peek at an agent, while it is open.
    peek: Option<peek::OpenPeek>,
    /// The panel of a checked report, while it is open.
    report_panel: Option<report_panel::OpenPanel>,
    settings: Settings,
    /// The user closed the bar; it opens again on the next change.
    dismissed: bool,
    /// Saves the session, if this window's session is saved.
    session: Option<session::SessionSaver>,
    /// Why a saved session was not made again, or why this one is not
    /// saved, until the bar is closed.
    session_notice: Option<String>,
    /// Why the app has no socket API, until the bar is closed.
    socket_notice: Option<String>,
    /// The agents started over the socket API; see [`agents`].
    agents: agents::Agents,
    /// The messages and the closed agents of the board's graph.
    graph: agent_graph::AgentGraph,
    /// The panes that need the user, and how the user is told.
    attention: attention::WindowAttention,
    agent_history: agent_history::AgentRecorder,
    sessions: session_list::Sessions,
    /// What starts the terminals of the agents, if the window has its
    /// panes started by [`ipc::open_window`].
    shells: Option<Rc<ipc::Shells>>,
    /// The last report of each pane that its Stop hook checked; see
    /// [`checked_reports`].
    checked_reports: std::collections::HashMap<PaneId, checked_reports::CheckedReport>,
    /// Why a role did not start, until the bar is closed.
    role_notice: Option<String>,
    /// Why the programs start with the app's own environment and not the
    /// one of the user's shell, until the bar is closed.
    login_env_notice: Option<String>,
    /// The tasks, and the form for a new one while it is open.
    tasks: tasks::Tasks,
    /// The agents at work, watched for whether they have stalled.
    stalls: stall::WindowStalls,
    /// Watches the config file for as long as the window is open.
    _watcher: Option<benterm_config::ConfigWatcher>,
    /// Watches the roles directory for as long as the window is open.
    _roles_watcher: Option<benterm_config::RolesWatcher>,
}

impl BentermWindow {
    /// A window whose first pane is `first`, drawn with the settings of
    /// `feed` and following its later events. `spawn` starts the
    /// terminals of the panes, tabs and workspaces opened later.
    pub fn new(
        first: FirstPane,
        spawn: SpawnTerminal,
        feed: ConfigFeed,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let theme = feed.settings().theme().clone();
        let first = Self::workspace_tabs_with(first, spawn.clone(), theme, window, cx);
        Self::with_workspaces(WorkspaceList::new(None, first), spawn, feed, window, cx)
    }

    /// A window showing `workspaces`, drawn with the settings of `feed`
    /// and following its later events.
    fn with_workspaces(
        workspaces: WorkspaceList<WorkspaceTabs>,
        spawn: SpawnTerminal,
        feed: ConfigFeed,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let ConfigFeed {
            settings,
            events,
            watcher,
            role_events,
            roles_watcher,
        } = feed;
        if let Some(mut role_events) = role_events {
            cx.spawn(async move |this, cx| {
                while let Some(roles) = role_events.next().await {
                    if this
                        .update(cx, |this, cx| this.apply_roles(roles, cx))
                        .is_err()
                    {
                        break;
                    }
                }
            })
            .detach();
        }
        // The menu bar lists the roles of the window that came last.
        cx.set_menus(menu::menus_with_roles(settings.roles()));
        if let Some(mut events) = events {
            cx.spawn(async move |this, cx| {
                while let Some(event) = events.next().await {
                    if this
                        .update(cx, |this, cx| this.apply_config(event, cx))
                        .is_err()
                    {
                        break;
                    }
                }
            })
            .detach();
        }
        // The pane in view changes when the window comes to the front or
        // goes to the back.
        cx.observe_window_activation(window, Self::look).detach();
        let mut this = Self {
            workspaces,
            spawn,
            renaming: None,
            sidebar: sidebar::SidebarState::new(cx),
            palette: None,
            context_menu: None,
            files: None,
            main_view: MainView::Terminals,
            board: None,
            diff: None,
            issues: None,
            epic_prompt: None,
            epic_reader: Self::default_epic_reader(),
            peek: None,
            report_panel: None,
            settings,
            dismissed: false,
            session: None,
            session_notice: None,
            socket_notice: None,
            checked_reports: std::collections::HashMap::new(),
            agents: agents::Agents::default(),
            graph: agent_graph::AgentGraph::default(),
            attention: attention::WindowAttention::default(),
            agent_history: agent_history::AgentRecorder::default(),
            sessions: session_list::Sessions::default(),
            shells: None,
            role_notice: None,
            login_env_notice: None,
            tasks: tasks::Tasks::default(),
            stalls: Self::watch_stalls(cx),
            _watcher: watcher,
            _roles_watcher: roles_watcher,
        };
        this.init_mailbox(cx);
        this
    }

    fn workspace_tabs_with(
        first: FirstPane,
        spawn: SpawnTerminal,
        theme: theme::Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> WorkspaceTabs {
        let tabs = cx.new(|cx| TabGroup::new(first, spawn, theme, window, cx));
        Self::workspace_tabs_of(tabs, window, cx)
    }

    /// What a workspace holds for `tabs`, whose events the window follows.
    fn workspace_tabs_of(
        tabs: Entity<TabGroup>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> WorkspaceTabs {
        let events = cx.subscribe_in(&tabs, window, |this, tabs, event, window, cx| {
            this.on_tabs_event(tabs.entity_id(), event.clone(), window, cx);
        });
        // A row of the sidebar says how many tabs its workspace has.
        let changes = cx.observe(&tabs, |_, _, cx| cx.notify());
        WorkspaceTabs {
            tabs,
            task: std::cell::Cell::new(None),
            _subscriptions: [events, changes],
        }
    }

    /// The workspaces from top to bottom.
    pub fn workspaces(&self) -> Vec<WorkspaceId> {
        self.workspaces.ids()
    }

    /// The selected workspace, the one whose tabs are shown.
    pub fn selected_workspace(&self) -> WorkspaceId {
        self.workspaces.selected()
    }

    pub fn workspace_name(&self, id: WorkspaceId) -> Option<String> {
        self.workspaces.get(id).map(|w| w.name().to_owned())
    }

    /// What a workspace is shown by: the name it was given, or, for one
    /// nobody named, the directory of the first shell of its first tab,
    /// wherever that shell is now.
    pub fn workspace_label(&self, id: WorkspaceId, cx: &App) -> Option<String> {
        let workspace = self.workspaces.get(id)?;
        if workspace.is_named() {
            return Some(workspace.name().to_owned());
        }
        let tabs = workspace.content().tabs.read(cx);
        let directory = tabs
            .tabs()
            .first()
            .and_then(|tab| tabs.grid(*tab))
            .and_then(|grid| {
                // The first pane that is a terminal: a browser has no
                // directory.
                let grid = grid.read(cx);
                let first = grid
                    .panes()
                    .into_iter()
                    .find_map(|pane| grid.terminal(pane))?;
                first.read(cx).working_directory()
            });
        let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
        Some(directory.map_or_else(
            || workspace.name().to_owned(),
            |directory| sidebar::directory_label(&directory, home.as_deref()),
        ))
    }

    /// Which of the theme's workspace colors the icon of a workspace has.
    pub fn workspace_color(&self, id: WorkspaceId) -> Option<WorkspaceColor> {
        self.workspaces.get(id).map(|w| w.color())
    }

    pub fn workspace_tabs(&self, id: WorkspaceId) -> Option<Entity<TabGroup>> {
        self.workspaces.get(id).map(|w| w.content().tabs.clone())
    }

    /// The tabs of the selected workspace.
    pub fn tabs(&self) -> &Entity<TabGroup> {
        &self.workspaces.selected_workspace().content().tabs
    }

    /// The panes of the selected tab.
    pub fn grid(&self, cx: &App) -> Entity<PaneGrid> {
        self.tabs().read(cx).selected_grid().clone()
    }

    /// The terminal of the focused pane of the selected tab. Only for a
    /// pane that is known to have one; see
    /// [`focused_terminal`](Self::focused_terminal).
    pub fn terminal(&self, cx: &App) -> Entity<TerminalView> {
        self.focused_terminal(cx)
            .expect("the focused pane has a terminal")
    }

    /// The terminal of the focused pane of the selected tab, if the pane
    /// has one: a browser pane does not.
    pub fn focused_terminal(&self, cx: &App) -> Option<Entity<TerminalView>> {
        let grid = self.grid(cx);
        let grid = grid.read(cx);
        grid.terminal(grid.focused()).cloned()
    }

    /// Opens a browser pane that shows the page at `url`, or no page
    /// without one, and returns the pane.
    ///
    /// The pane is split off pane `beside`, in whichever workspace and
    /// tab that is, towards `direction`; without `beside`, off the
    /// focused pane of the selected tab. With `focus`, the tab of the new
    /// pane is shown and the pane gets the keyboard, as when the user
    /// opens it; without, the selection and the keyboard stay where they
    /// are, as when a program does.
    pub fn open_browser(
        &mut self,
        url: Option<&str>,
        beside: Option<PaneId>,
        direction: SplitDirection,
        focus: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<PaneId, BrowserError> {
        if url.is_some_and(|url| !browser::is_page_url(url)) {
            return Err(BrowserError::NotAPage);
        }
        let place = match beside {
            Some(pane) => Some(
                self.locate(pane.get(), cx)
                    .ok_or(BrowserError::UnknownPane)?,
            ),
            None => None,
        };
        let grid = match &place {
            Some(place) => place.grid.clone(),
            None => self.grid(cx),
        };
        let shown = pane_area::pane_area(window, cx).unwrap_or_default();
        let new = grid.update(cx, |grid, cx| {
            let beside = beside.unwrap_or_else(|| grid.focused());
            grid.open_browser(beside, direction, url, shown, window, cx)
        })?;
        if focus {
            if let Some(place) = place {
                self.select_workspace(place.workspace, window, cx);
                let tab = place.tab;
                place
                    .tabs
                    .update(cx, |tabs, cx| tabs.select(tab, window, cx));
            }
            self.show_terminals(cx);
            grid.update(cx, |grid, cx| grid.focus(new, window, cx));
        }
        Ok(new)
    }

    /// Adds a workspace at the bottom whose first pane is `first`.
    /// `spawn` starts the terminals opened in it later, so a workspace can
    /// have a working directory of its own. Without a `name` the workspace
    /// is numbered. The selection and the keyboard stay where they are.
    pub fn add_workspace(
        &mut self,
        name: Option<&str>,
        first: FirstPane,
        spawn: SpawnTerminal,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> WorkspaceId {
        let theme = self.settings.theme().clone();
        let tabs = Self::workspace_tabs_with(first, spawn, theme, window, cx);
        cx.notify();
        self.workspaces.add(name, tabs)
    }

    /// Makes a workspace with a new shell at the bottom, and selects it.
    pub fn new_workspace(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let theme = &self.settings.theme().terminal;
        let size = pane_area::terminal_size(window, theme, cx);
        let first = match FirstPane::start(&self.spawn, theme, None, size) {
            Ok(first) => first,
            Err(error) => {
                eprintln!("benterm: could not start a terminal for the new workspace: {error}");
                return;
            }
        };
        let id = self.add_workspace(None, first, self.spawn.clone(), window, cx);
        self.select_workspace(id, window, cx);
    }

    /// Selects a workspace and gives the keyboard to the focused pane of
    /// its selected tab.
    pub fn select_workspace(
        &mut self,
        id: WorkspaceId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.workspaces.select(id).is_ok() {
            if self.main_view() == MainView::ProgressBoard {
                self.show_view(MainView::ProgressBoard, window, cx);
                self.show_title(window, cx);
                cx.notify();
            } else {
                self.focus_selected(window, cx);
            }
        }
    }

    /// Gives the keyboard to the selected workspace and redraws. The tabs
    /// answer with their title, which the window takes. A name being typed
    /// is given up, as the keyboard leaves its field. The pane area shows
    /// the panes again, if it showed another view.
    fn focus_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.renaming = None;
        self.show_terminals(cx);
        self.tabs()
            .clone()
            .update(cx, |tabs, cx| tabs.focus_selected(window, cx));
        cx.notify();
    }

    /// Closes a workspace with all its tabs. With the last workspace the
    /// window closes.
    pub fn close_workspace(
        &mut self,
        id: WorkspaceId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(workspace) = self.workspaces.get(id) else {
            return;
        };
        // The keyboard moves on only if it was in what closes: in one of
        // the workspace's panes, or in the field for its name.
        let renamed = self.renaming() == Some(id);
        let had_focus = renamed || workspace.content().tabs.read(cx).has_focus(window, cx);
        let task = workspace.content().task.get();
        match self.workspaces.close(id) {
            Ok(_) => {
                self.workspace_of_task_closed(task);
                self.sync_board(cx);
                if had_focus {
                    self.focus_selected(window, cx);
                } else {
                    self.show_title(window, cx);
                    cx.notify();
                }
            }
            Err(WorkspaceError::LastWorkspace) => {
                // A child started to outlive its parent outlives the last
                // pane too: the window stays, with the child in the
                // workspace's only tab. Its session is kept and saved as
                // that of any window.
                if let Some(lone) = self.agents.take_outliving() {
                    // The workspace stays for the child, and is no
                    // task's any more.
                    self.workspaces
                        .selected_workspace()
                        .content()
                        .task
                        .set(None);
                    self.workspace_of_task_closed(task);
                    let tabs = self.tabs().clone();
                    tabs.update(cx, |tabs, cx| tabs.replace_with_adopted(lone, window, cx));
                    self.renaming = None;
                    cx.notify();
                    return;
                }
                self.workspace_of_task_closed(task);
                // What the user closed does not come back at the next start.
                self.clear_session();
                window.remove_window()
            }
            Err(_) => {}
        }
    }

    /// Gives a workspace another name.
    pub fn rename_workspace(
        &mut self,
        id: WorkspaceId,
        name: &str,
        cx: &mut Context<Self>,
    ) -> Result<(), WorkspaceError> {
        self.workspaces.rename(id, name)?;
        // A pane's node has the name of its workspace.
        self.sync_graph(cx);
        cx.notify();
        Ok(())
    }

    /// The workspace whose name is being typed, if one is.
    pub fn renaming(&self) -> Option<WorkspaceId> {
        self.renaming.as_ref().map(|renaming| renaming.id)
    }

    /// What is in the field for the name now.
    pub fn renaming_text(&self, cx: &App) -> Option<String> {
        self.renaming
            .as_ref()
            .map(|renaming| renaming.editor.read(cx).text().to_owned())
    }

    /// Puts a field in the place of a workspace's name and gives it the
    /// keyboard. Enter takes the name, Escape or leaving the field keeps
    /// the old one. A workspace whose name is already being typed keeps
    /// its field and what is in it.
    pub fn start_renaming(&mut self, id: WorkspaceId, window: &mut Window, cx: &mut Context<Self>) {
        if self.renaming() == Some(id) {
            return;
        }
        let Some(workspace) = self.workspaces.get(id) else {
            return;
        };
        let theme = self.settings.theme().sidebar.clone();
        let editor = cx.new(|cx| NameEditor::new(workspace.name(), theme, cx));
        let events = cx.subscribe_in(&editor, window, |this, _, event, window, cx| {
            let name = match event {
                NameEditorEvent::Confirmed(name) => Some(name.as_str()),
                NameEditorEvent::Cancelled => None,
            };
            this.finish_renaming(name, window, cx);
        });
        let focus = editor.read(cx).focus_handle().clone();
        let field = editor.entity_id();
        // The keyboard went somewhere else, which whoever moved it has
        // taken care of. An older field going away is not this one.
        let blur = cx.on_blur(&focus, window, move |this, _, cx| {
            if this
                .renaming
                .as_ref()
                .is_some_and(|renaming| renaming.editor.entity_id() == field)
            {
                this.renaming = None;
                cx.notify();
            }
        });
        self.renaming = Some(Renaming {
            id,
            editor,
            _subscriptions: [events, blur],
        });
        window.focus(&focus, cx);
        cx.notify();
    }

    /// Ends the typing of a name, with the name to take if there is one,
    /// and gives the keyboard back to the terminal. A name that cannot be
    /// one, such as an empty one, leaves the old name.
    fn finish_renaming(&mut self, name: Option<&str>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(renaming) = self.renaming.take() else {
            return;
        };
        if let Some(name) = name {
            let _ = self.workspaces.rename(renaming.id, name);
        }
        self.focus_selected(window, cx);
    }

    fn workspace_of(&self, tabs: EntityId) -> Option<WorkspaceId> {
        self.workspaces
            .iter()
            .find(|w| w.content().tabs.entity_id() == tabs)
            .map(|w| w.id())
    }

    /// The window's title is that of the selected workspace.
    fn show_title(&self, window: &mut Window, cx: &App) {
        window.set_window_title(&self.tabs().read(cx).window_title(cx));
    }

    fn on_tabs_event(
        &mut self,
        tabs: EntityId,
        event: TabGroupEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(id) = self.workspace_of(tabs) else {
            return;
        };
        match event {
            TabGroupEvent::TitleChanged => {
                if id == self.workspaces.selected() {
                    self.show_title(window, cx);
                }
            }
            TabGroupEvent::Notified(pane, notification) => {
                self.take_notification(pane, notification, window, cx);
            }
            TabGroupEvent::Emptied => self.close_workspace(id, window, cx),
            TabGroupEvent::MenuRequested(pane, position) => {
                let target = context_menu::MenuTarget::Pane(pane);
                self.open_context_menu(target, position, window, cx);
            }
            TabGroupEvent::TabMenuRequested(tab, position) => {
                let target = context_menu::MenuTarget::Tab(id, tab);
                self.open_context_menu(target, position, window, cx);
            }
            TabGroupEvent::OpenUrl(pane, url) => self.open_link(pane, &url, window, cx),
            TabGroupEvent::OpenPath(pane, path) => {
                self.open_path_in_file_browser(pane, path, window, cx);
            }
            TabGroupEvent::StartupFailed(pane, failure) => {
                self.show_startup_failure(pane, &failure, cx);
            }
        }
        // The selected tab or its focused pane may have changed.
        self.look(window, cx);
    }

    pub fn apply_config(&mut self, event: ConfigEvent, cx: &mut Context<Self>) {
        let theme_changed = self.settings.apply(event);
        if let Some(shells) = &self.shells {
            // For the panes opened from now on.
            shells.report_check.set(self.settings.checks_reports());
            // Every pane has its credential already, so the check can
            // come on or go off for the requests from now on.
            if let Some(permissions) = shells.permissions() {
                permissions.set_restricted(self.settings.restricts_ipc());
            }
        }
        if theme_changed {
            let theme = self.settings.theme().clone();
            for workspace in self.workspaces.iter() {
                let theme = theme.clone();
                let tabs = &workspace.content().tabs;
                tabs.update(cx, |tabs, cx| tabs.set_theme(theme, cx));
            }
            if let Some(renaming) = &self.renaming {
                let theme = theme.sidebar.clone();
                renaming
                    .editor
                    .update(cx, |editor, cx| editor.set_theme(theme, cx));
            }
            self.set_session_theme(&theme.sidebar, cx);
        }
        self.dismissed = false;
        cx.notify();
    }

    /// The messages about the config file the window shows now.
    pub fn shown_messages(&self) -> Vec<String> {
        if self.dismissed {
            Vec::new()
        } else {
            self.settings.messages()
        }
    }

    /// Closes the bar until the file is saved again.
    pub fn dismiss_messages(&mut self, cx: &mut Context<Self>) {
        // What the bar said about the session is not said again.
        self.session_notice = None;
        self.socket_notice = None;
        self.role_notice = None;
        self.login_env_notice = None;
        self.tasks.notice = None;
        self.dismissed = true;
        cx.notify();
    }

    /// A temporary bar listing `messages`. It gives way to in-app
    /// notifications once they exist.
    fn notice_bar(
        &self,
        groups: Vec<(&'static str, Vec<String>)>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let notice = &self.settings.theme().notice;
        let close = div()
            .id("dismiss-config-messages")
            .flex_none()
            .self_start()
            .px_1()
            .cursor_pointer()
            .child("×")
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| this.dismiss_messages(cx)),
            );
        let accent = hsla(notice.accent);
        // Each group of messages under its heading.
        let lines = groups.into_iter().flat_map(|(heading, messages)| {
            let heading = div().text_color(accent).child(heading);
            let lines = messages.into_iter().map(move |message| {
                div()
                    .flex()
                    .gap_2()
                    .child(
                        div()
                            .text_color(accent)
                            .font_weight(FontWeight::BOLD)
                            .child("!"),
                    )
                    .child(div().min_w_0().flex_1().overflow_hidden().child(message))
            });
            std::iter::once(heading).chain(lines)
        });
        div()
            .flex()
            .flex_none()
            .px_3()
            .py_2()
            .gap_2()
            .bg(hsla(notice.background))
            .border_b_1()
            .border_color(hsla(notice.border))
            .text_color(hsla(notice.text))
            .font_family(notice.font_family.clone())
            .text_size(px(notice.font_size))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .gap_1()
                    .children(lines),
            )
            .child(close)
    }
}

impl Render for BentermWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Drawn first in every frame: the views it holds are drawn after.
        redraw_stats::frame_drawn(cx);
        let groups: Vec<_> = [
            ("Session", self.shown_session_notice().into_iter().collect()),
            (
                "Socket API",
                self.shown_socket_notice().into_iter().collect(),
            ),
            ("Config file", self.shown_messages()),
            ("Roles", self.role_notice.iter().cloned().collect()),
            ("Tasks", self.shown_task_notice().into_iter().collect()),
            (
                "Shell environment",
                self.shown_login_env_notice().into_iter().collect(),
            ),
        ]
        .into_iter()
        .filter(|(_, messages): &(_, Vec<String>)| !messages.is_empty())
        .collect();
        let bar = (!groups.is_empty()).then(|| self.notice_bar(groups, cx).into_any_element());
        self.check_peek(window, cx);
        browser::begin_frame(window, cx);
        let main = self.main_area(cx);
        let sidebar = self.sidebar(window, cx).into_any_element();
        let root = div()
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .key_context(sidebar::KEY_CONTEXT)
            .on_action(cx.listener(|this, _: &ToggleCommandPalette, window, cx| {
                this.toggle_command_palette(window, cx)
            }))
            .on_action(cx.listener(|this, action: &LaunchRole, window, cx| {
                this.launch_role(&action.0, window, cx)
            }))
            .on_action(cx.listener(|this, _: &ToggleFileBrowser, window, cx| {
                this.toggle_file_browser(window, cx)
            }))
            .on_action(
                cx.listener(|this, _: &NewWorkspace, window, cx| this.new_workspace(window, cx)),
            )
            .on_action(cx.listener(|this, _: &NewTask, window, cx| this.open_task_form(window, cx)))
            .on_action(cx.listener(|this, _: &CloseWorkspace, window, cx| {
                this.ask_to_close_workspace(this.selected_workspace(), window, cx)
            }))
            .on_action(cx.listener(|this, _: &SelectNextWorkspace, window, cx| {
                this.select_shown(1, window, cx)
            }))
            .on_action(
                cx.listener(|this, _: &SelectPreviousWorkspace, window, cx| {
                    this.select_shown(-1, window, cx)
                }),
            )
            .on_action(
                cx.listener(|this, _: &FocusSidebar, window, cx| this.focus_sidebar(window, cx)),
            )
            .on_action(cx.listener(|this, _: &ToggleSidebarCollapse, _, cx| {
                this.toggle_sidebar_collapse(cx)
            }))
            .on_action(cx.listener(|this, _: &RenameWorkspace, window, cx| {
                this.start_renaming(this.selected_workspace(), window, cx)
            }));
        let root = self.session_list_actions(root, cx);
        let root = self.peek_actions(root, cx);
        let root = self.report_panel_actions(root, cx);
        let sidebar_bg = hsla(self.settings.theme().sidebar.background);
        self.main_view_actions(root, cx)
            .bg(sidebar_bg)
            .children(bar)
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .child(sidebar)
                    .child(main)
                    .children(self.file_browser_panel(cx)),
            )
            .children(self.report_panel_overlay(window, cx))
            .children(self.command_palette_overlay())
            .children(self.epic_prompt_overlay())
            .children(self.task_form_overlay())
            .children(self.toast_overlay(cx))
            .children(self.context_menu_overlay(cx))
    }
}

pub fn window_options(cx: &App) -> WindowOptions {
    let bounds = Bounds::centered(None, size(px(960.), px(640.)), cx);
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        titlebar: Some(TitlebarOptions {
            title: Some(APP_NAME.into()),
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// Opens the main window with the user's config file and the login shell
/// in every pane, and serves the socket API for it. The window has the
/// workspaces, tabs and panes of the saved session, if there is one, and
/// saves its own. The terminals started after `login_env` has come get
/// the environment it brings; see [`login_env::start`].
pub fn open_main_window(
    login_env: Option<futures::channel::oneshot::Receiver<login_env::Taken>>,
    cx: &mut App,
) -> anyhow::Result<WindowHandle<BentermWindow>> {
    redraw_stats::start(
        redraw_stats::path_from(std::env::var_os(redraw_stats::ENV)),
        cx,
    );
    let feed = ConfigFeed::user_config();
    let home = std::env::var_os("HOME").map(PathBuf::from);
    // The folders an earlier version made for worktrees are made the
    // user's alone, as the ones made now are.
    if let Some(root) = feed.settings().worktree_root(home.as_deref()) {
        benterm_task::make_worktree_root_private(&root);
    }
    // The PTY log follows the settings the app started with.
    let pty_log = feed.settings().pty_log().clone();
    let start: ipc::StartTerminal = Rc::new(move |pty, theme| {
        StartedTerminal::spawn_recorded(pty, theme, &pty_log, home.as_deref())
    });
    let socket = benterm_ipc::default_socket_path();
    let session = benterm_session::session_path();
    let window =
        ipc::open_session_window(PtyConfig::login_shell(), start, socket, session, feed, cx)?;
    if let Some(taken) = login_env {
        window.update(cx, |root, window, cx| {
            root.take_login_env(taken, window, cx)
        })?;
    }
    Ok(window)
}

/// `pty`, started in `directory` if there is one, else where `pty` says.
fn in_directory(pty: &PtyConfig, directory: Option<&Path>) -> PtyConfig {
    PtyConfig {
        cwd: directory.map(Path::to_owned).or_else(|| pty.cwd.clone()),
        ..pty.clone()
    }
}

/// Opens a window whose panes each run `pty`, with the settings of `feed`.
/// A new tab and a split pane start in the directory of the focused pane.
/// A pane closes when its program exits, a tab closes with its last pane,
/// a workspace with its last tab, and the window with its last workspace.
pub fn open_terminal_window(
    pty: PtyConfig,
    feed: ConfigFeed,
    cx: &mut App,
) -> anyhow::Result<WindowHandle<BentermWindow>> {
    let spawn: SpawnTerminal = Rc::new(move |_, theme, directory, size| {
        let pty = PtyConfig {
            size,
            ..in_directory(&pty, directory)
        };
        StartedTerminal::spawn(pty, theme)
    });
    let first = FirstPane::start(&spawn, &feed.settings().theme().terminal, None, None)?;
    open_started_window(first, spawn, feed, cx)
}

/// Opens a window whose first pane has already started, with the settings
/// of `feed`. `spawn` starts the terminals of the panes and tabs opened
/// later.
pub fn open_started_window(
    first: FirstPane,
    spawn: SpawnTerminal,
    feed: ConfigFeed,
    cx: &mut App,
) -> anyhow::Result<WindowHandle<BentermWindow>> {
    let options = window_options(cx);
    cx.open_window(options, |window, cx| {
        let root = cx.new(|cx| BentermWindow::new(first, spawn, feed, window, cx));
        let grid = root.read(cx).grid(cx);
        grid.update(cx, |grid, cx| grid.focus_terminal(window, cx));
        root
    })
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use benterm_config::{ConfigEvent, parse};
    use benterm_terminal::PtyConfig;
    use gpui::{TestAppContext, VisualTestContext};

    use super::*;
    use crate::settings::ConfigFeed;

    /// Opens a window that runs `script`. The terminal's worker thread
    /// wakes the app from outside the test scheduler, which the scheduler
    /// has to be told to accept.
    fn open(script: &str, cx: &mut TestAppContext) -> WindowHandle<BentermWindow> {
        cx.executor().allow_parking();
        open_with(script, ConfigFeed::defaults(), cx)
    }

    fn open_with(
        script: &str,
        feed: ConfigFeed,
        cx: &mut TestAppContext,
    ) -> WindowHandle<BentermWindow> {
        cx.executor().allow_parking();
        cx.update(|cx| open_terminal_window(sh(script), feed, cx))
            .unwrap()
    }

    /// Lets the app run until `done` holds for the window.
    fn wait_for_window(
        window: WindowHandle<BentermWindow>,
        cx: &mut TestAppContext,
        done: impl Fn(&BentermWindow, &App) -> bool,
    ) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            cx.run_until_parked();
            let _ = window.update(cx, |_, window, _| window.refresh());
            if window.read_with(cx, |root, cx| done(root, cx)).unwrap() {
                return;
            }
            assert!(Instant::now() < deadline, "not before the timeout");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn apply(window: WindowHandle<BentermWindow>, cx: &mut TestAppContext, text: &str) {
        let event = ConfigEvent::Applied(parse(text).unwrap());
        window
            .update(cx, |root, _, cx| root.apply_config(event, cx))
            .unwrap();
    }

    fn sh(script: &str) -> PtyConfig {
        PtyConfig::command("/bin/sh", ["-c", script])
    }

    /// Lets the app run until `done` holds for the terminal view. The
    /// program runs in real time on other threads, hence the polling.
    fn wait_until(
        window: WindowHandle<BentermWindow>,
        cx: &mut TestAppContext,
        done: impl Fn(&TerminalView) -> bool,
    ) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            cx.run_until_parked();
            let terminal = window.update(cx, |root, _, cx| root.terminal(cx));
            let Ok(terminal) = terminal else {
                panic!("the window closed while waiting");
            };
            if terminal.read_with(cx, |view, _| done(view)) {
                return;
            }
            assert!(Instant::now() < deadline, "not before the timeout");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[gpui::test]
    fn window_title_is_app_name(cx: &mut TestAppContext) {
        let options = cx.update(|cx| window_options(cx));
        let title = options.titlebar.and_then(|titlebar| titlebar.title);
        assert_eq!(title.as_deref(), Some(APP_NAME));
    }

    #[gpui::test]
    fn open_terminal_window_opens_one_window(cx: &mut TestAppContext) {
        let handle = open("sleep 100", cx);
        assert_eq!(cx.windows().len(), 1);
        assert!(handle.update(cx, |_, _, _| ()).is_ok());
    }

    #[gpui::test]
    fn output_of_the_program_reaches_the_view(cx: &mut TestAppContext) {
        let window = open("printf hello; sleep 100", cx);
        wait_until(window, cx, |view| {
            view.screen().rows[0].text().starts_with("hello")
        });
    }

    #[gpui::test]
    fn typed_text_reaches_the_program(cx: &mut TestAppContext) {
        // The program turns the echo off late, so letters typed at once
        // would be echoed by the terminal before the program reads them.
        // It says ready once the echo is off, and only then is it typed to.
        let window = open("sleep 0.2; stty -echo; printf ready; cat", cx);
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        wait_until(window, &mut cx, |view| {
            view.screen().rows[0].text().starts_with("ready")
        });
        cx.simulate_input("hi");
        cx.simulate_keystrokes("enter");
        // With the echo off, only the program can put the letters there.
        wait_until(window, &mut cx, |view| {
            view.screen().rows[0].text().trim_end() == "readyhi"
        });
    }

    #[gpui::test]
    fn the_terminal_starts_with_the_theme_of_the_config(cx: &mut TestAppContext) {
        let feed = ConfigFeed::new(benterm_config::load("[terminal]\npadding = 7"));
        let window = open_with("sleep 100", feed, cx);
        wait_for_window(window, cx, |root, cx| {
            root.terminal(cx).read(cx).theme().terminal.padding == 7.0
        });
    }

    #[gpui::test]
    fn an_applied_config_reaches_the_terminal_view(cx: &mut TestAppContext) {
        let window = open("sleep 100", cx);
        apply(window, cx, "[colors.terminal]\nbackground = \"#010203\"");
        wait_for_window(window, cx, |root, cx| {
            root.terminal(cx).read(cx).theme().terminal.background == benterm_core::Rgb(1, 2, 3)
        });
    }

    #[gpui::test]
    fn a_larger_font_and_padding_give_a_smaller_grid(cx: &mut TestAppContext) {
        let window = open("sleep 100", cx);
        let rows = |root: &BentermWindow, cx: &App| root.terminal(cx).read(cx).screen().size.rows;
        wait_for_window(window, cx, |root, cx| rows(root, cx) > 2);
        let before = window.read_with(cx, |root, cx| rows(root, cx)).unwrap();
        apply(window, cx, "[terminal]\nfont_size = 30\npadding = 60");
        wait_for_window(window, cx, |root, cx| rows(root, cx) < before);
    }

    #[gpui::test]
    fn warnings_of_the_config_are_shown(cx: &mut TestAppContext) {
        let feed = ConfigFeed::new(benterm_config::load("[terminal]\nfoo = 1"));
        let window = open_with("sleep 100", feed, cx);
        let shown = window
            .read_with(cx, |root, _| root.shown_messages())
            .unwrap();
        assert_eq!(shown, vec!["terminal.foo: unknown setting"]);
    }

    #[gpui::test]
    fn dismissed_warnings_come_back_with_the_next_save(cx: &mut TestAppContext) {
        let feed = ConfigFeed::new(benterm_config::load("[terminal]\nfoo = 1"));
        let window = open_with("sleep 100", feed, cx);
        window
            .update(cx, |root, _, cx| root.dismiss_messages(cx))
            .unwrap();
        let shown = window
            .read_with(cx, |root, _| root.shown_messages())
            .unwrap();
        assert!(shown.is_empty());
        apply(window, cx, "[ui]\nbar = 1");
        let shown = window
            .read_with(cx, |root, _| root.shown_messages())
            .unwrap();
        assert_eq!(shown, vec!["ui.bar: unknown setting"]);
    }

    #[gpui::test]
    fn saving_the_file_changes_the_theme_without_a_restart(cx: &mut TestAppContext) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[terminal]\npadding = 3\n").unwrap();
        let window = open_with("sleep 100", ConfigFeed::watch(&path), cx);
        let padding =
            |root: &BentermWindow, cx: &App| root.terminal(cx).read(cx).theme().terminal.padding;
        assert_eq!(
            window.read_with(cx, |root, cx| padding(root, cx)).unwrap(),
            3.0
        );
        std::fs::write(&path, "[terminal]\npadding = 9\nfoo = 1\n").unwrap();
        wait_for_window(window, cx, |root, cx| padding(root, cx) == 9.0);
        let shown = window
            .read_with(cx, |root, _| root.shown_messages())
            .unwrap();
        assert_eq!(shown, vec!["terminal.foo: unknown setting"]);
    }

    #[gpui::test]
    fn the_window_closes_when_the_program_exits(cx: &mut TestAppContext) {
        open("exit 0", cx);
        let deadline = Instant::now() + Duration::from_secs(10);
        while !cx.windows().is_empty() {
            assert!(Instant::now() < deadline, "the window is still open");
            cx.run_until_parked();
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

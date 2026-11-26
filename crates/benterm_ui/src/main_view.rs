//! The tabs above the pane area, which switch what the area shows: the
//! terminal panes, or another view of the window such as the progress
//! board.
//!
//! Each view is a [`MainView`]. A view to come is a new variant, a label, and an arm in [`BentermWindow::main_area`]
//! and [`BentermWindow::show_view`]; the bar draws every variant of
//! [`MainView::ALL`].

use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, App, AppContext, Context, Entity, InteractiveElement, IntoElement, KeyBinding,
    MouseButton, ParentElement, StyleRefinement, Styled, Subscription, Window, actions, div, px,
};

use crate::diff_view::{DiffEvent, DiffSource, DiffView};
use crate::issue_graph_view::{
    EpicPrompt, EpicPromptEvent, EpicReader, EpicTarget, GithubEpicReader, IssueGraphEvent,
    IssueGraphView, IssueTaskDetails, NewLocalChildIssue, NewLocalEpic, OpenEpic, OpenEpicPrompt,
    OpenLocalEpic, OpenLocalIssues, OpenRepositoryIssues, prompt_overlay,
};
use crate::progress_board::{BoardEvent, ProgressBoard};
use crate::review::{self, Readiness, ReviewSent};
use crate::terminal_view::hsla;
use crate::{BentermWindow, TerminalView, sidebar};
use benterm_layout::{PaneId, WorkspaceId};
use benterm_terminal::WriteError;
use std::time::SystemTime;

actions!(
    main_view,
    [
        ShowTerminals,
        ToggleProgressBoard,
        /// Shows the diff of the selected workspace's task in the pane
        /// area, or the panes again.
        ToggleDiff,
        /// Shows the Epic and child-issue graph in the pane area, or the panes again.
        ToggleIssues,
    ]
);

/// The height of the bar, as the tab bar's.
const BAR_HEIGHT: f32 = 32.;
/// The room at the right end of the bar for the switch of the views,
/// which the tab bar leaves free.
pub(crate) const VIEW_SWITCH_WIDTH: f32 = 330.;
/// The space left and right of a tab's label.
const TAB_PADDING: f32 = 14.;

/// Binds Cmd-Shift-B and Cmd-Shift-G, which work anywhere in the window.
pub fn bind_keys(cx: &mut App) {
    let context = Some(sidebar::KEY_CONTEXT);
    cx.bind_keys([
        KeyBinding::new("cmd-shift-b", ToggleProgressBoard, context),
        KeyBinding::new("cmd-shift-g", ToggleDiff, context),
    ]);
}

/// What the pane area shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MainView {
    /// The tabs of the selected workspace and their panes.
    #[default]
    Terminals,
    /// The changes of the selected workspace's task.
    Diff,
    /// The states of every agent over time.
    ProgressBoard,
    /// The Epic and child-issue dependency graph.
    Issues,
}

impl MainView {
    /// In the order of the tabs.
    pub const ALL: [MainView; 4] = [
        Self::Terminals,
        Self::Diff,
        Self::ProgressBoard,
        Self::Issues,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Terminals => "Terminals",
            Self::Diff => "Diff",
            Self::ProgressBoard => "Progress",
            Self::Issues => "Issues",
        }
    }

    /// The name of the view's tab in tests.
    fn selector(self) -> &'static str {
        match self {
            Self::Terminals => "view-tab-terminals",
            Self::Diff => "view-tab-diff",
            Self::ProgressBoard => "view-tab-progress-board",
            Self::Issues => "view-tab-issues",
        }
    }
}

/// The issues graph view, from when it was first shown.
pub(crate) struct OpenIssues {
    pub(crate) view: Entity<IssueGraphView>,
    _events: Subscription,
}

/// The progress board, from when it was first shown. It is kept while
/// hidden, with the span the user chose.
pub(crate) struct OpenBoard {
    pub(crate) view: Entity<ProgressBoard>,
    _events: Subscription,
}

/// The diff, from when it was first shown. It is kept while hidden, on
/// the file the user chose, and watches nothing then.
pub(crate) struct OpenDiff {
    view: Entity<DiffView>,
    /// The workspace whose task the diff was last pointed at.
    workspace: Option<WorkspaceId>,
    /// Every time comments went to a task's agent, oldest first.
    sends: Vec<ReviewSent>,
    _events: Subscription,
}

impl BentermWindow {
    /// What the pane area shows now.
    pub fn main_view(&self) -> MainView {
        self.main_view
    }

    /// The progress board, while it is shown.
    pub fn progress_board(&self) -> Option<Entity<ProgressBoard>> {
        (self.main_view == MainView::ProgressBoard)
            .then(|| self.board.as_ref().map(|open| open.view.clone()))
            .flatten()
    }

    /// Every time comments went to the agent of a task, oldest first.
    pub fn review_sends(&self) -> &[ReviewSent] {
        self.diff.as_ref().map_or(&[], |open| &open.sends)
    }

    /// The agent's pane of `task`: in the task's workspace, in the order
    /// of the tabs and panes, the first that runs an agent the window
    /// started, such as the one of the task's role, or else the first
    /// whose screen shows an agent.
    pub(crate) fn task_agent(&self, task: u64, cx: &App) -> Option<(PaneId, Entity<TerminalView>)> {
        let workspace = self.workspace_of_task(task)?;
        let started: Vec<u64> = self.agents(cx).iter().map(|row| row.pane.get()).collect();
        let tabs = self.workspaces.get(workspace)?.content().tabs.read(cx);
        let mut shown = None;
        for tab in tabs.tabs() {
            let Some(grid) = tabs.grid(tab) else {
                continue;
            };
            let grid = grid.read(cx);
            for pane in grid.panes() {
                let Some(view) = grid.terminal(pane) else {
                    continue;
                };
                if started.contains(&pane.get()) {
                    return Some((pane, view.clone()));
                }
                if shown.is_none() && view.read(cx).agent_status().agent.is_some() {
                    shown = Some((pane, view.clone()));
                }
            }
        }
        shown
    }

    /// Enters the comments of `task` not sent yet in its agent's pane,
    /// if the agent may take them now, or with `confirmed` once the user
    /// said yes to an agent that is not waiting for input. Otherwise the
    /// diff asks, or says why not.
    fn send_review(&mut self, task: u64, confirmed: bool, cx: &mut Context<Self>) {
        let Some(diff) = self.diff.as_ref().map(|open| open.view.clone()) else {
            return;
        };
        let target = self.task_agent(task, cx);
        if let Some((_, view)) = &target {
            // What the terminal already holds of its program belongs to
            // the turn before this review: show it to the detector
            // before checking readiness or recording the baseline in
            // `owe_turn`.
            view.update(cx, |view, cx| view.observe_program(cx));
        }
        let pane = target.as_ref().map(|(_, view)| {
            let terminal = view.read(cx);
            review::Pane {
                state: terminal.agent_status().state,
                typing: terminal.user_is_typing(),
                exited: terminal.has_exited(),
            }
        });
        let readiness = review::readiness(pane);
        let go = match readiness {
            Readiness::Ready => true,
            Readiness::Confirm(_) => confirmed,
            Readiness::Refuse(_) => false,
        };
        let Some((pane, view)) = target.filter(|_| go) else {
            diff.update(cx, |diff, cx| diff.ask(Some(readiness), cx));
            return;
        };
        let Some(outgoing) = diff.read(cx).outgoing(task) else {
            return;
        };
        // Pasted, with Enter after it in the same write, as `agent.send`
        // does: a line break in the paste stays in the one message.
        if let Err(error) = view.read(cx).enter_message(&outgoing.text) {
            let message = match error {
                WriteError::Full => "The agent's input queue is full; it is not reading input.",
                WriteError::Closed => "The agent's program has exited.",
            };
            let refused = Readiness::Refuse(message.into());
            diff.update(cx, |diff, cx| diff.ask(Some(refused), cx));
            return;
        }
        self.owe_turn(pane, &view, cx);
        diff.update(cx, |diff, cx| diff.mark_sent(task, &outgoing.ids, cx));
        if let Some(open) = &mut self.diff {
            open.sends.push(ReviewSent {
                task,
                pane,
                comments: outgoing.ids.len(),
                at: SystemTime::now(),
            });
        }
        cx.notify();
    }

    /// The issues graph view, while it is shown.
    pub fn issue_graph_view(&self) -> Option<Entity<IssueGraphView>> {
        (self.main_view == MainView::Issues)
            .then(|| self.issues.as_ref().map(|open| open.view.clone()))
            .flatten()
    }

    /// Replaces the GitHub reader used by the Issues tab (for tests).
    pub fn set_epic_reader(
        &mut self,
        reader: std::sync::Arc<dyn EpicReader>,
        cx: &mut Context<Self>,
    ) {
        self.epic_reader = reader.clone();
        if let Some(open) = &self.issues {
            open.view.update(cx, |view, _| view.set_reader(reader));
        }
    }

    fn focused_working_directory(&self, cx: &App) -> std::path::PathBuf {
        self.focused_terminal(cx)
            .and_then(|t| t.read(cx).working_directory())
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_default()
    }

    /// Opens `epic_number` for the focused pane's working directory and shows
    /// the Issues tab.
    pub fn open_epic(&mut self, epic_number: u64, window: &mut Window, cx: &mut Context<Self>) {
        let directory = self.focused_working_directory(cx);
        self.open_epic_in(directory, epic_number, window, cx);
    }

    /// Opens `epic_number` for `directory` and shows the Issues tab.
    pub fn open_epic_in(
        &mut self,
        directory: std::path::PathBuf,
        epic_number: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_issues_target(
            EpicTarget::Directory {
                directory,
                epic_number,
            },
            false,
            window,
            cx,
        );
    }

    /// Opens the Epic `epic_number` of the local issues file of
    /// `directory`'s repository and shows the Issues tab.
    pub fn open_local_epic_in(
        &mut self,
        directory: std::path::PathBuf,
        epic_number: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_issues_target(
            EpicTarget::Directory {
                directory,
                epic_number,
            },
            true,
            window,
            cx,
        );
    }

    /// Opens all issues of the local issues file of `directory`'s
    /// repository and shows the Issues tab.
    pub fn open_local_issues_in(
        &mut self,
        directory: std::path::PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_issues_target(
            EpicTarget::RepositoryDirectory { directory },
            true,
            window,
            cx,
        );
    }

    /// Points the Issues tab at `target`, read from the local issues
    /// file when `local` and from GitHub otherwise, and shows the tab.
    fn open_issues_target(
        &mut self,
        target: EpicTarget,
        local: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let view = self.issues_view_made(window, cx);
        let github = self.epic_reader.clone();
        view.update(cx, |view, cx| {
            // The reader is changed only between GitHub and the file,
            // so that the one a test has set stays.
            if view.is_local() != local {
                view.set_reader(if local {
                    std::sync::Arc::new(crate::local_issues::LocalEpicReader::default())
                } else {
                    github
                });
            }
            view.open_target(target, cx);
        });
        self.show_view(MainView::Issues, window, cx);
    }

    /// The directory the Issues tab reads for, or the one of the focused pane.
    fn issues_directory(&self, cx: &App) -> std::path::PathBuf {
        self.issues
            .as_ref()
            .and_then(|open| open.view.read(cx).current_directory())
            .unwrap_or_else(|| self.focused_working_directory(cx))
    }

    /// Whether the Issues tab shows the issues of a local file.
    fn issues_are_local(&self, cx: &App) -> bool {
        self.issues
            .as_ref()
            .is_some_and(|open| open.view.read(cx).is_local())
    }

    /// Opens all issues in the focused pane's repository and shows the Issues tab.
    pub fn open_repository_issues(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let directory = self.issues_directory(cx);
        self.open_repository_issues_in(directory, window, cx);
    }

    /// Opens all issues of the local issues file of the repository the
    /// Issues tab reads for, or of the focused pane's.
    pub fn open_local_issues(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let directory = self.issues_directory(cx);
        self.open_local_issues_in(directory, window, cx);
    }

    /// Opens the form for a new issue of the local issues file: a child
    /// of the issue selected on the graph when `child`, and an Epic to
    /// be otherwise. The Issues tab is shown with the local issues of
    /// the repository first, when it shows something else.
    pub fn new_local_issue(&mut self, child: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.main_view != MainView::Issues || !self.issues_are_local(cx) {
            self.open_local_issues(window, cx);
        }
        if let Some(view) = self.issue_graph_view() {
            view.update(cx, |view, cx| view.open_new_issue_form(child, window, cx));
        }
    }

    /// Opens all issues in `directory`'s repository and shows the Issues tab.
    pub fn open_repository_issues_in(
        &mut self,
        directory: std::path::PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_issues_target(
            EpicTarget::RepositoryDirectory { directory },
            false,
            window,
            cx,
        );
    }

    /// The diff, while it is shown.
    pub fn diff_view(&self) -> Option<Entity<DiffView>> {
        (self.main_view == MainView::Diff)
            .then(|| self.diff.as_ref().map(|open| open.view.clone()))
            .flatten()
    }

    /// Shows `view` in the pane area and gives it the keyboard. Back to
    /// the panes, the keyboard goes to the focused pane of the selected
    /// tab, as before.
    pub fn show_view(&mut self, view: MainView, window: &mut Window, cx: &mut Context<Self>) {
        match view {
            MainView::Terminals => {
                if self.main_view != MainView::Terminals {
                    // Through `focus_selected`, which shows the panes.
                    self.focus_selected(window, cx);
                }
            }
            MainView::ProgressBoard => {
                if let Some(open) = &self.diff {
                    open.view.update(cx, |diff, cx| diff.set_shown(false, cx));
                }
                if let Some(open) = &self.issues {
                    open.view
                        .update(cx, |issues, cx| issues.set_shown(false, cx));
                }
                self.main_view = view;
                self.renaming = None;
                let board = self.board_view(window, cx);
                board.update(cx, |board, cx| board.set_shown(true, cx));
                self.sync_board(cx);
                let focus = board.read(cx).focus_handle().clone();
                window.focus(&focus, cx);
            }
            MainView::Diff => {
                if let Some(open) = &self.board {
                    open.view.update(cx, |board, cx| board.set_shown(false, cx));
                }
                if let Some(open) = &self.issues {
                    open.view
                        .update(cx, |issues, cx| issues.set_shown(false, cx));
                }
                self.main_view = view;
                self.renaming = None;
                let diff = self.diff_view_made(window, cx);
                self.sync_diff(cx);
                diff.update(cx, |diff, cx| diff.set_shown(true, cx));
                let focus = diff.read(cx).focus_handle().clone();
                window.focus(&focus, cx);
            }
            MainView::Issues => {
                if let Some(open) = &self.board {
                    open.view.update(cx, |board, cx| board.set_shown(false, cx));
                }
                if let Some(open) = &self.diff {
                    open.view.update(cx, |diff, cx| diff.set_shown(false, cx));
                }
                self.main_view = view;
                self.renaming = None;
                let issues = self.issues_view_made(window, cx);
                if !issues.read(cx).has_target_or_model() {
                    let directory = self.focused_working_directory(cx);
                    issues.update(cx, |issues, cx| {
                        issues.open_target(EpicTarget::RepositoryDirectory { directory }, cx);
                    });
                }
                self.sync_issues(cx);
                issues.update(cx, |issues, cx| issues.set_shown(true, cx));
                let focus = issues.read(cx).focus_handle().clone();
                window.focus(&focus, cx);
            }
        }
        self.look(window, cx);
        cx.notify();
    }

    /// Hides another view and shows the panes, without moving the
    /// keyboard. For whatever gives the keyboard to a pane.
    pub(crate) fn show_terminals(&mut self, cx: &mut Context<Self>) {
        if self.main_view == MainView::Terminals {
            return;
        }
        self.main_view = MainView::Terminals;
        if let Some(open) = &self.board {
            open.view.update(cx, |board, cx| board.set_shown(false, cx));
        }
        if let Some(open) = &self.diff {
            open.view.update(cx, |diff, cx| diff.set_shown(false, cx));
        }
        if let Some(open) = &self.issues {
            open.view
                .update(cx, |issues, cx| issues.set_shown(false, cx));
        }
        cx.notify();
    }

    fn toggle_progress_board(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let view = match self.main_view {
            MainView::ProgressBoard => MainView::Terminals,
            _ => MainView::ProgressBoard,
        };
        self.show_view(view, window, cx);
    }

    fn toggle_diff(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let view = match self.main_view {
            MainView::Diff => MainView::Terminals,
            _ => MainView::Diff,
        };
        self.show_view(view, window, cx);
    }

    fn toggle_issues(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let view = match self.main_view {
            MainView::Issues => MainView::Terminals,
            _ => MainView::Issues,
        };
        self.show_view(view, window, cx);
    }

    /// The issues graph view, made the first time it is asked for.
    fn issues_view_made(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<IssueGraphView> {
        if let Some(open) = &self.issues {
            return open.view.clone();
        }
        let theme = self.settings.theme().board.clone();
        let form_theme = self.settings.theme().command_palette.clone();
        let reader = self.epic_reader.clone();
        let view = cx.new(|cx| {
            let mut view = IssueGraphView::new(theme, reader, cx);
            view.set_form_theme(form_theme);
            view
        });
        let events = cx.subscribe_in(&view, window, |this, _, event, window, cx| match event {
            IssueGraphEvent::Leave => this.show_view(MainView::Terminals, window, cx),
            // From the graph, both stay with what the graph shows:
            // GitHub or the local file.
            IssueGraphEvent::OpenEpicPrompt => {
                let local = this.issues_are_local(cx);
                this.open_epic_prompt(local, window, cx)
            }
            IssueGraphEvent::OpenRepositoryIssues => {
                if this.issues_are_local(cx) {
                    this.open_local_issues(window, cx)
                } else {
                    this.open_repository_issues(window, cx)
                }
            }
            IssueGraphEvent::OpenLocalIssues => this.open_local_issues(window, cx),
            IssueGraphEvent::TaskRepositoriesChanged => this.sync_issues(cx),
            IssueGraphEvent::OpenUrl(url) => {
                let _ = this.open_browser(
                    Some(url),
                    None,
                    benterm_layout::SplitDirection::Right,
                    true,
                    window,
                    cx,
                );
            }
            IssueGraphEvent::NewTask(values) => {
                this.open_issue_task_form(values.clone(), window, cx)
            }
            IssueGraphEvent::ShowTask(workspace, diff) => {
                this.select_workspace(*workspace, window, cx);
                let view = if *diff {
                    MainView::Diff
                } else {
                    MainView::Terminals
                };
                this.show_view(view, window, cx);
            }
        });
        self.issues = Some(OpenIssues {
            view: view.clone(),
            _events: events,
        });
        view
    }

    /// Synchronises local issue agent states into the Issues graph view.
    fn sync_issues(&mut self, cx: &mut Context<Self>) {
        let Some(open) = &self.issues else {
            return;
        };
        let Some(matching) = open.view.read(cx).matching_repositories() else {
            return;
        };
        let states = self.issue_agent_states(|path| matching.contains(path), cx);
        let repositories = self.open_task_repositories();
        let details = self
            .tasks()
            .into_iter()
            .filter_map(|row| {
                let workspace = row.workspace?;
                if row.task.closed.is_some() || !matching.contains(&row.task.repository) {
                    return None;
                }
                let issue = benterm_task::issue_number_in_branch(&row.task.branch)?;
                Some((
                    issue,
                    IssueTaskDetails {
                        workspace,
                        branch: row.task.branch,
                        agents: self.workspace_agent_statuses(workspace, cx),
                    },
                ))
            })
            .collect();
        open.view.update(cx, |issues, cx| {
            issues.set_task_repositories(repositories, cx);
            issues.set_agent_states(states, cx);
            issues.set_task_details(details, cx);
        });
    }

    /// The Open Epic prompt, while it is open.
    pub fn epic_prompt(&self) -> Option<Entity<EpicPrompt>> {
        self.epic_prompt.as_ref().map(|open| open.view.clone())
    }

    /// Opens the floating prompt to enter an Epic number for the focused
    /// pane's working directory: of GitHub, or of the local issues file
    /// when `local`.
    pub(crate) fn open_epic_prompt(
        &mut self,
        local: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let directory = self
            .focused_terminal(cx)
            .and_then(|t| t.read(cx).working_directory())
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_default();
        let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
        let dir_label = crate::sidebar::directory_label(&directory, home.as_deref());
        let theme = self.settings.theme().command_palette.clone();
        let view = cx.new(|cx| {
            let prompt = EpicPrompt::new(dir_label, theme, cx);
            if local {
                prompt.for_local_issues()
            } else {
                prompt
            }
        });
        let events = cx.subscribe_in(&view, window, |this, _, event, window, cx| match event {
            EpicPromptEvent::Confirmed(number) => {
                let (dir, local) = this
                    .epic_prompt
                    .take()
                    .map(|open| (open.directory, open.local))
                    .unwrap_or_default();
                if local {
                    this.open_local_epic_in(dir, *number, window, cx);
                } else {
                    this.open_epic_in(dir, *number, window, cx);
                }
            }
            EpicPromptEvent::Cancelled => {
                this.epic_prompt = None;
                this.focus_selected(window, cx);
            }
        });
        let focus = view.read(cx).focus_handle(cx);
        let prompt_id = view.entity_id();
        let blur = cx.on_blur(&focus, window, move |this, _, cx| {
            if this
                .epic_prompt
                .as_ref()
                .is_some_and(|open| open.view.entity_id() == prompt_id)
            {
                this.epic_prompt = None;
                cx.notify();
            }
        });
        self.epic_prompt = Some(OpenEpicPrompt {
            view,
            directory,
            local,
            _subscriptions: [events, blur],
        });
        window.focus(&focus, cx);
        cx.notify();
    }

    pub(crate) fn epic_prompt_overlay(&self) -> Option<impl IntoElement + use<>> {
        self.epic_prompt.as_ref().map(prompt_overlay)
    }

    pub fn default_epic_reader() -> std::sync::Arc<dyn EpicReader> {
        std::sync::Arc::new(GithubEpicReader::default())
    }

    /// The diff, made the first time it is asked for.
    fn diff_view_made(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Entity<DiffView> {
        if let Some(open) = &self.diff {
            return open.view.clone();
        }
        let theme = self.settings.theme().clone();
        let git = self.git(self.shells.as_ref());
        let view = cx.new(|cx| DiffView::new(theme, git, cx));
        let events = cx.subscribe_in(&view, window, |this, _, event, window, cx| match event {
            DiffEvent::Leave => this.show_view(MainView::Terminals, window, cx),
            DiffEvent::Send { task, confirmed } => this.send_review(*task, *confirmed, cx),
        });
        self.diff = Some(OpenDiff {
            view: view.clone(),
            workspace: None,
            sends: Vec::new(),
            _events: events,
        });
        view
    }

    /// Points the diff at the task of the selected workspace, if another
    /// workspace has been selected since, and gives it the theme.
    fn sync_diff(&mut self, cx: &mut Context<Self>) {
        let selected = self.selected_workspace();
        let source = self.task_of_workspace(selected).map(DiffSource::of);
        let theme = self.settings.theme().clone();
        let Some(open) = &mut self.diff else {
            return;
        };
        let moved = open.workspace != Some(selected);
        open.workspace = Some(selected);
        open.view.update(cx, |diff, cx| {
            diff.set_theme(&theme, cx);
            if moved {
                diff.set_source(source, cx);
            }
        });
    }

    /// The board, made the first time it is asked for.
    fn board_view(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Entity<ProgressBoard> {
        if let Some(open) = &self.board {
            return open.view.clone();
        }
        let theme = self.settings.theme().board.clone();
        let view = cx.new(|cx| ProgressBoard::new(theme, cx));
        let events = cx.subscribe_in(&view, window, |this, _, event, window, cx| match event {
            BoardEvent::Leave => this.show_view(MainView::Terminals, window, cx),
            BoardEvent::Open(pane) => {
                // A terminal behind the window has no pane to go to, and
                // is looked at over the board.
                if !this.show_pane(*pane, window, cx) {
                    this.peek(*pane, window, cx);
                }
            }
            BoardEvent::Refresh => this.sync_graph(cx),
            BoardEvent::OpenTerminal(id) => {
                let id = benterm_ipc::PaneId::new(*id);
                if let Ok((pane, _)) = this.find_terminal(id, cx)
                    && !this.show_pane(pane, window, cx)
                {
                    this.peek(pane, window, cx);
                }
            }
        });
        self.board = Some(OpenBoard {
            view: view.clone(),
            _events: events,
        });
        view
    }

    /// The actions of the bar, for the root of the window.
    pub(crate) fn main_view_actions<E: InteractiveElement>(
        &self,
        element: E,
        cx: &mut Context<Self>,
    ) -> E {
        element
            .on_action(cx.listener(|this, _: &ShowTerminals, window, cx| {
                this.show_view(MainView::Terminals, window, cx)
            }))
            .on_action(cx.listener(|this, _: &ToggleProgressBoard, window, cx| {
                this.toggle_progress_board(window, cx)
            }))
            .on_action(cx.listener(|this, _: &ToggleDiff, window, cx| this.toggle_diff(window, cx)))
            .on_action(
                cx.listener(|this, _: &ToggleIssues, window, cx| this.toggle_issues(window, cx)),
            )
            .on_action(cx.listener(|this, _: &OpenEpic, window, cx| {
                this.open_epic_prompt(false, window, cx)
            }))
            .on_action(cx.listener(|this, _: &OpenLocalEpic, window, cx| {
                this.open_epic_prompt(true, window, cx)
            }))
            .on_action(cx.listener(|this, _: &OpenLocalIssues, window, cx| {
                this.open_local_issues(window, cx)
            }))
            .on_action(cx.listener(|this, _: &OpenRepositoryIssues, window, cx| {
                this.open_repository_issues(window, cx)
            }))
            .on_action(cx.listener(|this, _: &NewLocalEpic, window, cx| {
                this.new_local_issue(false, window, cx)
            }))
            .on_action(cx.listener(|this, _: &NewLocalChildIssue, window, cx| {
                this.new_local_issue(true, window, cx)
            }))
    }

    /// The switch of the views, at the right end of the one bar above
    /// the pane area: a group of small buttons, the shown view's lit.
    fn view_switch(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = &self.settings.theme().tabs;
        let switches = MainView::ALL.into_iter().map(|view| {
            let active = view == self.main_view;
            div()
                .id(view.selector())
                .debug_selector(|| view.selector().into())
                .px(px(10.))
                .py(px(1.))
                .rounded(px(crate::theme::RADIUS_CONTROL))
                .text_color(hsla(theme.text))
                .cursor_pointer()
                .map(|switch| {
                    if active {
                        switch
                            .bg(hsla(theme.button_hover_background))
                            .text_color(hsla(theme.active_text))
                    } else {
                        switch.hover(|switch| switch.text_color(hsla(theme.active_text)))
                    }
                })
                .child(view.label())
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, window, cx| this.show_view(view, window, cx)),
                )
        });
        div()
            .id("view-switch")
            .debug_selector(|| "view-switch".into())
            .absolute()
            .top_0()
            .right_0()
            .w(px(VIEW_SWITCH_WIDTH))
            .h(px(BAR_HEIGHT))
            .bg(hsla(theme.bar_background))
            .border_b_1()
            .border_color(hsla(theme.separator))
            .flex()
            .items_center()
            .justify_end()
            .pr(px(8.))
            .font_family(theme.font_family.clone())
            .text_size(px(crate::theme::small_text(theme.font_size)))
            .child(
                div()
                    .flex()
                    .gap(px(2.))
                    .p(px(2.))
                    .rounded(px(crate::theme::RADIUS_CARD))
                    .bg(hsla(theme.active_background))
                    .border_1()
                    .border_color(hsla(theme.separator))
                    .children(switches),
            )
    }

    /// The bar above a view that has no tabs of its own, with the name
    /// of the view where the tabs of the terminals are.
    fn view_bar(&self) -> impl IntoElement {
        let theme = &self.settings.theme().tabs;
        div()
            .id("view-bar")
            .debug_selector(|| "view-bar".into())
            .flex()
            .flex_none()
            .items_center()
            .h(px(BAR_HEIGHT))
            .px(px(TAB_PADDING))
            .bg(hsla(theme.bar_background))
            .border_b_1()
            .border_color(hsla(theme.separator))
            .font_family(theme.font_family.clone())
            .text_size(px(theme.font_size))
            .text_color(hsla(theme.active_text))
            .child(self.main_view.label())
    }

    /// The bar of views over what the pane area shows.
    pub(crate) fn main_area(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let mut shown_is_tabs = false;
        let shown = match (self.main_view, &self.board) {
            (MainView::ProgressBoard, Some(open)) => {
                let theme = self.settings.theme().board.clone();
                open.view.update(cx, |board, cx| board.set_theme(theme, cx));
                // Drawn again only when the board changes, not with the
                // output of the terminals or the rest of the window.
                let style = StyleRefinement::default().size_full();
                open.view.clone().cached(style).into_any_element()
            }
            (MainView::Diff, _) if self.diff.is_some() => {
                // The selected workspace may have changed since.
                self.sync_diff(cx);
                let view = self.diff.as_ref().map(|open| open.view.clone());
                let style = StyleRefinement::default().size_full();
                match view {
                    Some(view) => view.cached(style).into_any_element(),
                    None => {
                        shown_is_tabs = true;
                        self.tabs().clone().into_any_element()
                    }
                }
            }
            (MainView::Issues, _) if self.issues.is_some() => {
                self.sync_issues(cx);
                let theme = self.settings.theme().board.clone();
                let form_theme = self.settings.theme().command_palette.clone();
                if let Some(open) = &self.issues {
                    open.view.update(cx, |issues, cx| {
                        issues.set_theme(theme, cx);
                        issues.set_form_theme(form_theme);
                    });
                }
                let view = self.issues.as_ref().map(|open| open.view.clone());
                let style = StyleRefinement::default().size_full();
                match view {
                    Some(view) => view.cached(style).into_any_element(),
                    None => {
                        shown_is_tabs = true;
                        self.tabs().clone().into_any_element()
                    }
                }
            }
            _ => {
                shown_is_tabs = true;
                self.tabs().clone().into_any_element()
            }
        };
        // The terminals have their tabs in the bar; another view has its
        // name there.
        let tabs_shown = shown_is_tabs;
        let theme = self.settings.theme();
        let inset_border =
            crate::design::subtle_border(theme.tabs.separator, theme.sidebar.background);
        div()
            .relative()
            .flex_1()
            .min_w_0()
            .h_full()
            .pr(px(8.))
            .pb(px(8.))
            .pt(px(4.))
            .child(
                div()
                    .relative()
                    .size_full()
                    .flex()
                    .flex_col()
                    .rounded(px(crate::theme::RADIUS_PANEL))
                    .border_1()
                    .border_color(inset_border)
                    .overflow_hidden()
                    .when(!tabs_shown, |area| area.child(self.view_bar()))
                    // Before what is shown, as the bar of the views was: GPUI
                    // looks the keys of the commands up in the contexts of what
                    // was drawn last, which has to be the focused pane. Nothing
                    // is drawn over the end of the bar the switch is at.
                    .child(self.view_switch(cx))
                    .child(div().relative().flex_1().min_h_0().child(shown))
                    // The peek covers the bar too: its own bar takes its place.
                    .children(self.peek_overlay(cx)),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod issue_detail_tests {
    use benterm_terminal::PtyConfig;
    use gpui::{Modifiers, TestAppContext, VisualTestContext};

    use super::*;
    use crate::issue_graph::made_up_epic;
    use crate::settings::ConfigFeed;

    #[gpui::test]
    fn the_issue_detail_opens_its_task_workspace_and_diff(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        cx.update(crate::bind_keys);
        let window = cx
            .update(|cx| {
                crate::open_terminal_window(
                    PtyConfig::command("/bin/sh", ["-c", "sleep 100"]),
                    ConfigFeed::defaults(),
                    cx,
                )
            })
            .unwrap();
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let (first, task) = window
            .update(&mut cx, |root, window, cx| {
                let first = root.selected_workspace();
                root.new_workspace(window, cx);
                let task = root.selected_workspace();
                root.select_workspace(first, window, cx);
                (first, task)
            })
            .unwrap();
        assert_ne!(first, task);

        for (selector, expected_view) in [
            ("issue-graph-show-terminal", MainView::Terminals),
            ("issue-graph-show-diff", MainView::Diff),
        ] {
            window
                .update(&mut cx, |root, window, cx| {
                    root.select_workspace(first, window, cx);
                    root.show_view(MainView::Issues, window, cx);
                    let (epic, mut statuses) = made_up_epic();
                    statuses.remove(&122);
                    root.issue_graph_view().unwrap().update(cx, |view, cx| {
                        view.take_made_up_epic(epic, statuses, cx);
                        view.select_test_issue(122, cx);
                        view.set_task_details(
                            [(
                                122,
                                IssueTaskDetails {
                                    workspace: task,
                                    branch: "issue-122".into(),
                                    agents: Vec::new(),
                                },
                            )]
                            .into(),
                            cx,
                        );
                    });
                })
                .unwrap();
            assert_eq!(
                window
                    .read_with(&cx, |root, cx| {
                        root.issue_graph_view()
                            .unwrap()
                            .read(cx)
                            .model()
                            .unwrap()
                            .node(122)
                            .unwrap()
                            .status
                    })
                    .unwrap(),
                crate::issue_graph::IssueNodeStatus::Assigned
            );
            cx.run_until_parked();
            let button = cx
                .debug_bounds(selector)
                .expect("the task action is visible");
            cx.simulate_click(button.center(), Modifiers::none());
            cx.run_until_parked();
            assert_eq!(
                window
                    .read_with(&cx, |root, _| {
                        (root.selected_workspace(), root.main_view())
                    })
                    .unwrap(),
                (task, expected_view)
            );
        }
    }
}

//! The sidebar on the left of a window, which lists its workspaces, and
//! the keys of the workspaces.
//!
//! The order, selection, names and colors live in a
//! [`WorkspaceList`](benterm_layout::WorkspaceList), which knows nothing
//! about the screen. The workspaces of tasks are drawn under the
//! workspace of their repository, as [`sidebar_rows`] puts them; the keys
//! that select the next and the previous workspace follow the rows as
//! they are drawn, past the rows of a folded group. The window connects
//! the list to the keys, the mouse and the tabs; this file draws it.
//!
//! [`FocusSidebar`] gives the keyboard to the sidebar. The arrows then
//! move a cursor over the rows of the workspaces and of the session
//! list, Enter goes to the workspace or the agent's pane of the row, and
//! Escape gives the keyboard back. A row of the session list under the
//! cursor shows its field for a reply, and Tab gives the field the
//! keyboard.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use benterm_layout::{PaneId, WorkspaceId};
use gpui::prelude::FluentBuilder;
use gpui::{
    App, Context, FocusHandle, FontWeight, InteractiveElement, IntoElement, KeyBinding,
    MouseButton, MouseDownEvent, ParentElement, StatefulInteractiveElement, Styled, Subscription,
    Window, actions, div, px, relative,
};

use crate::sidebar_rows::{Group, SidebarRow, SidebarWorkspace, TaskPlace, sidebar_rows};
use crate::terminal_view::hsla;
use crate::{BentermWindow, MainView};

actions!(
    workspace,
    [
        /// Makes a workspace at the bottom of the list, with a shell in
        /// it, and selects it.
        NewWorkspace,
        /// Closes the selected workspace with all its tabs, or the window
        /// with its last workspace.
        CloseWorkspace,
        /// Selects the workspace below, or the first after the last.
        SelectNextWorkspace,
        /// Selects the workspace above, or the last before the first.
        SelectPreviousWorkspace,
        RenameWorkspace,
        /// Gives the keyboard to the sidebar, where the arrows go
        /// through the rows of the workspaces and the sessions and Enter
        /// goes to the one chosen.
        FocusSidebar,
        ToggleSidebarCollapse,
    ]
);

actions!(
    sidebar,
    [
        CursorDown,
        CursorUp,
        /// Goes to the workspace or the agent's pane of the row under the
        /// cursor, or folds or opens the group of a repository's name.
        Choose,
        Fold,
        Unfold,
        /// Gives the keyboard back to what the pane area shows.
        LeaveSidebar,
    ]
);

/// The key context of the window, which the workspace keys are limited to.
pub(crate) const KEY_CONTEXT: &str = "BentermWindow";
/// The key context of the sidebar while it has the keyboard.
pub(crate) const SIDEBAR_CONTEXT: &str = "Sidebar";
/// The key context of what takes the keyboard for the sidebar's cursor,
/// for the keys that would be text in a field of the sidebar.
pub(crate) const CURSOR_CONTEXT: &str = "SidebarCursor";

/// Binds the workspace keys. Ghostty has no workspaces; a workspace is
/// what a window is there, a set of tabs, so making and closing one take
/// Ghostty's keys for a window.
pub fn bind_keys(cx: &mut App) {
    let context = Some(KEY_CONTEXT);
    cx.bind_keys([
        KeyBinding::new("cmd-n", NewWorkspace, context),
        KeyBinding::new("cmd-shift-w", CloseWorkspace, context),
        KeyBinding::new("ctrl-cmd-]", SelectNextWorkspace, context),
        KeyBinding::new("ctrl-cmd-[", SelectPreviousWorkspace, context),
        KeyBinding::new("cmd-shift-r", RenameWorkspace, context),
        KeyBinding::new("cmd-shift-s", FocusSidebar, context),
        KeyBinding::new("cmd-shift-\\", ToggleSidebarCollapse, context),
    ]);
    let sidebar = Some(SIDEBAR_CONTEXT);
    cx.bind_keys([
        KeyBinding::new("down", CursorDown, sidebar),
        KeyBinding::new("up", CursorUp, sidebar),
        KeyBinding::new("enter", Choose, sidebar),
        KeyBinding::new("left", Fold, sidebar),
        KeyBinding::new("right", Unfold, sidebar),
        KeyBinding::new("escape", LeaveSidebar, sidebar),
    ]);
}

/// A row of the sidebar the cursor can be on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SidebarItem {
    Workspace(WorkspaceId),
    /// The name of a repository whose tasks have no workspace of it.
    Heading(PathBuf),
    /// The row of the session list of the agent in this terminal.
    Session(PaneId),
}

/// What the window keeps for its sidebar.
pub(crate) struct SidebarState {
    /// The repositories whose groups are folded.
    pub(crate) folded: BTreeSet<PathBuf>,
    /// Whether the sidebar is collapsed to an icon rail.
    pub(crate) collapsed: bool,
    /// Takes the keyboard for the arrows.
    focus: FocusHandle,
    /// The row the arrows are at, while the sidebar has the keyboard.
    cursor: Option<SidebarItem>,
    /// What the workspaces are shown by in the frame being drawn, so
    /// that the system is asked for a shell's directory once a frame and
    /// not once a row.
    labels: std::cell::RefCell<std::collections::HashMap<WorkspaceId, String>>,
    _focus_out: Option<Subscription>,
}

impl SidebarState {
    pub(crate) fn new(cx: &mut App) -> Self {
        Self {
            folded: BTreeSet::new(),
            collapsed: false,
            focus: cx.focus_handle(),
            cursor: None,
            labels: Default::default(),
            _focus_out: None,
        }
    }

    /// The agent whose row the cursor is at, if it is at one.
    pub(crate) fn session_at_cursor(&self) -> Option<PaneId> {
        match self.cursor {
            Some(SidebarItem::Session(pane)) => Some(pane),
            _ => None,
        }
    }

    /// Whether the cursor is at the row of the agent of `pane`.
    pub(crate) fn at_session(&self, pane: PaneId) -> bool {
        self.cursor == Some(SidebarItem::Session(pane))
    }
}

/// How far a task's row is moved in.
const INDENT: f32 = 12.;

pub(crate) const WIDTH: f32 = 240.;
/// The width of the sidebar when collapsed to an icon rail.
pub(crate) const COLLAPSED_WIDTH: f32 = 48.;
/// The space left and right of the sidebar's contents.
const PADDING: f32 = 12.;
/// The side of a workspace's icon.
const ICON: f32 = 18.;
/// The side of the mark that folds a group, which is what is pressed.
const CHEVRON: f32 = 16.;
/// The space left of that mark.
const CHEVRON_INSET: f32 = 4.;
/// The space between the icon and the name.
const GAP: f32 = 6.;
/// The thickness of the line left of the selected workspace.
const INDICATOR: f32 = 2.;
/// The side of the dot of a workspace or a session with a pane that needs
/// the user.
pub(crate) const ATTENTION_MARK: f32 = 6.;

/// The letter on a workspace's icon: the first of its name.
fn initial(name: &str) -> String {
    name.chars().next().map_or_else(String::new, |first| {
        first.to_uppercase().collect::<String>()
    })
}

/// What a directory is shown by in place of a name: its last part, `~`
/// for the home directory `home`, and the whole path if it has no parts.
pub fn directory_label(directory: &Path, home: Option<&Path>) -> String {
    if home == Some(directory) {
        return "~".to_owned();
    }
    directory.file_name().map_or_else(
        || directory.to_string_lossy().into_owned(),
        |name| name.to_string_lossy().into_owned(),
    )
}

/// Where a workspace's row is in the sidebar, beside its workspace.
struct RowPlace {
    depth: usize,
    branch: Option<String>,
    group: Option<Group>,
    /// Whether the list has a group, so that the rows leave room at
    /// their left for the mark that folds one.
    gutter: bool,
}

/// The mark of a group: pointing right while it is folded, down while
/// it is open.
fn fold_mark(folded: bool) -> &'static str {
    if folded { "\u{25b8}" } else { "\u{25be}" }
}

fn tab_count(count: usize) -> String {
    if count == 1 {
        "1 tab".into()
    } else {
        format!("{count} tabs")
    }
}

impl BentermWindow {
    /// The rows of the workspace list, from top to bottom.
    pub fn sidebar_rows(&self, cx: &App) -> Vec<SidebarRow> {
        let tasks: Vec<Option<TaskPlace>> = self
            .workspaces
            .iter()
            .map(|workspace| {
                let task = self.task_of_workspace(workspace.id())?;
                task.closed.is_none().then(|| TaskPlace {
                    repository: task.repository.clone(),
                    branch: task.branch.clone(),
                })
            })
            .collect();
        // Where a workspace is is asked of the system, and of use only
        // with tasks.
        let any_task = tasks.iter().any(Option::is_some);
        let workspaces: Vec<SidebarWorkspace> = self
            .workspaces
            .iter()
            .zip(tasks)
            .map(|(workspace, task)| {
                let directory = (any_task && task.is_none())
                    .then(|| {
                        let grid = workspace.content().tabs.read(cx).selected_grid().read(cx);
                        grid.terminal(grid.focused())?.read(cx).working_directory()
                    })
                    .flatten();
                SidebarWorkspace {
                    id: workspace.id(),
                    task,
                    directory,
                }
            })
            .collect();
        sidebar_rows(
            &workspaces,
            &self.sidebar.folded,
            self.workspaces.selected(),
        )
    }

    /// The repositories with a task in a workspace now that are folded,
    /// as the session keeps them.
    pub(crate) fn folded_repositories(&self) -> Vec<String> {
        let shown: BTreeSet<PathBuf> = self
            .workspaces
            .iter()
            .filter_map(|workspace| self.task_of_workspace(workspace.id()))
            .map(|task| task.repository.clone())
            .collect();
        self.sidebar
            .folded
            .intersection(&shown)
            .filter_map(|repository| repository.to_str().map(str::to_owned))
            .collect()
    }

    /// Folds the group of `repository`, or opens it.
    pub fn set_folded(&mut self, repository: &Path, folded: bool, cx: &mut Context<Self>) {
        if folded {
            self.sidebar.folded.insert(repository.to_owned());
        } else {
            self.sidebar.folded.remove(repository);
        }
        cx.notify();
    }

    fn toggle_folded(&mut self, repository: &Path, cx: &mut Context<Self>) {
        let folded = self.sidebar.folded.contains(repository);
        self.set_folded(repository, !folded, cx);
    }

    /// Selects the workspace `step` rows further down the sidebar, round
    /// from the last to the first. The rows hidden in a folded group are
    /// gone past.
    pub(crate) fn select_shown(
        &mut self,
        step: isize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let shown: Vec<WorkspaceId> = self
            .sidebar_rows(cx)
            .iter()
            .filter_map(SidebarRow::workspace)
            .collect();
        let selected = self.workspaces.selected();
        let Some(current) = shown.iter().position(|id| *id == selected) else {
            return;
        };
        let next = (current as isize + step).rem_euclid(shown.len() as isize) as usize;
        if shown[next] != selected {
            self.select_workspace(shown[next], window, cx);
        }
    }

    /// The rows the cursor goes through, from top to bottom.
    fn cursor_items(&self, cx: &App) -> Vec<SidebarItem> {
        let workspaces = self.sidebar_rows(cx).into_iter().map(|row| match row {
            SidebarRow::Workspace { id, .. } => SidebarItem::Workspace(id),
            SidebarRow::Heading(group) => SidebarItem::Heading(group.repository),
        });
        let sessions = self
            .session_rows(cx)
            .into_iter()
            .map(|row| SidebarItem::Session(row.pane));
        workspaces.chain(sessions).collect()
    }

    /// Whether the sidebar has the keyboard for its arrows.
    pub(crate) fn sidebar_has_keyboard(&self, window: &Window) -> bool {
        self.sidebar.focus.is_focused(window)
    }

    /// Gives the sidebar the keyboard with its cursor at the row of the
    /// agent of `pane`.
    pub(crate) fn focus_sidebar_at_session(
        &mut self,
        pane: PaneId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.focus_sidebar(window, cx);
        self.sidebar.cursor = Some(SidebarItem::Session(pane));
    }

    /// Gives the keyboard to the sidebar, with the cursor at the
    /// selected workspace.
    pub fn focus_sidebar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.renaming = None;
        self.sidebar.cursor = Some(SidebarItem::Workspace(self.workspaces.selected()));
        if self.sidebar._focus_out.is_none() {
            let focus = self.sidebar.focus.clone();
            self.sidebar._focus_out = Some(cx.on_focus_out(&focus, window, |this, _, _, cx| {
                this.sidebar.cursor = None;
                cx.notify();
            }));
        }
        window.focus(&self.sidebar.focus, cx);
        cx.notify();
    }

    fn move_cursor(&mut self, step: isize, cx: &mut Context<Self>) {
        let Some(cursor) = self.sidebar.cursor.clone() else {
            return;
        };
        let items = self.cursor_items(cx);
        if items.is_empty() {
            return;
        }
        // A row that went away leaves the cursor at the top.
        let next = match items.iter().position(|item| *item == cursor) {
            Some(current) => (current as isize + step).clamp(0, items.len() as isize - 1) as usize,
            None => 0,
        };
        self.sidebar.cursor = Some(items[next].clone());
        cx.notify();
    }

    /// The repository of the group the row under the cursor heads or
    /// is in.
    fn group_at_cursor(&self, cx: &App) -> Option<PathBuf> {
        let rows = self.sidebar_rows(cx);
        let mut group: Option<&Group> = None;
        for row in &rows {
            let (at, heads, depth) = match row {
                SidebarRow::Workspace {
                    id,
                    depth,
                    group: heads,
                    ..
                } => (
                    self.sidebar.cursor == Some(SidebarItem::Workspace(*id)),
                    heads.as_ref(),
                    *depth,
                ),
                SidebarRow::Heading(heads) => (
                    self.sidebar.cursor == Some(SidebarItem::Heading(heads.repository.clone())),
                    Some(heads),
                    0,
                ),
            };
            if depth == 0 {
                group = heads;
            }
            if at {
                return group.map(|group| group.repository.clone());
            }
        }
        None
    }

    fn fold_at_cursor(&mut self, folded: bool, cx: &mut Context<Self>) {
        if let Some(repository) = self.group_at_cursor(cx) {
            self.set_folded(&repository, folded, cx);
            // The cursor goes to the row that heads the group, if it was
            // on a row that is hidden now.
            if self
                .cursor_items(cx)
                .iter()
                .all(|item| Some(item) != self.sidebar.cursor.as_ref())
            {
                let head = self.sidebar_rows(cx).into_iter().find_map(|row| match row {
                    SidebarRow::Workspace {
                        id,
                        group: Some(group),
                        ..
                    } if group.repository == repository => Some(SidebarItem::Workspace(id)),
                    SidebarRow::Heading(group) if group.repository == repository => {
                        Some(SidebarItem::Heading(repository.clone()))
                    }
                    _ => None,
                });
                self.sidebar.cursor = head;
            }
        }
    }

    fn choose(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.sidebar.cursor.clone() {
            Some(SidebarItem::Workspace(id)) => {
                self.sidebar.cursor = None;
                if id == self.workspaces.selected() {
                    self.leave_sidebar(window, cx);
                } else {
                    self.select_workspace(id, window, cx);
                }
            }
            Some(SidebarItem::Heading(repository)) => self.toggle_folded(&repository, cx),
            // An agent behind the window has no pane to go to, and the
            // keyboard stays.
            Some(SidebarItem::Session(pane)) if self.place_of(pane, cx).is_some() => {
                self.sidebar.cursor = None;
                self.select_session(pane, window, cx);
            }
            Some(SidebarItem::Session(_)) | None => {}
        }
    }

    /// Gives the keyboard back to what the pane area shows.
    fn leave_sidebar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sidebar.cursor = None;
        match self.main_view() {
            MainView::Terminals => self.focus_selected(window, cx),
            view => self.show_view(view, window, cx),
        }
    }

    /// Toggles the sidebar between full width and the collapsed icon rail.
    pub(crate) fn toggle_sidebar_collapse(&mut self, cx: &mut Context<Self>) {
        self.sidebar.collapsed = !self.sidebar.collapsed;
        cx.notify();
    }

    /// The current sidebar width in pixels.
    pub(crate) fn sidebar_width(&self) -> f32 {
        if self.sidebar.collapsed {
            COLLAPSED_WIDTH
        } else {
            WIDTH
        }
    }

    fn collapsed_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = &self.settings.theme().sidebar;
        let tooltip_theme = self.settings.theme().menu.clone();
        let expand = div()
            .id("sidebar-expand")
            .debug_selector(|| "sidebar-expand".into())
            .flex()
            .items_center()
            .justify_center()
            .size(px(26.))
            .rounded(px(crate::theme::RADIUS_CONTROL))
            .text_color(hsla(theme.section_header_text))
            .cursor_pointer()
            .hover(|btn| {
                btn.bg(hsla(theme.item_hover_background))
                    .text_color(hsla(theme.item_text))
            })
            .tooltip(crate::tooltip::Tooltip::with_shortcut(
                "Expand sidebar",
                "⇧⌘\\",
                tooltip_theme.clone(),
            ))
            .child("»")
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| this.toggle_sidebar_collapse(cx)),
            );
        let workspaces = self.workspaces.iter().map(|workspace| {
            let id = workspace.id();
            let active = id == self.workspaces.selected();
            let label = self.drawn_label(id, cx);
            let attention = workspace.content().tabs.read(cx).needs_attention(cx);
            div()
                .id(("collapsed-workspace", id.get()))
                .relative()
                .flex()
                .items_center()
                .justify_center()
                .size(px(32.))
                .rounded(px(crate::theme::RADIUS_CARD))
                .cursor_pointer()
                .when(active, |el| el.bg(hsla(theme.item_active_background)))
                .when(!active, |el| {
                    el.hover(|el| el.bg(hsla(theme.item_hover_background)))
                })
                .tooltip(crate::tooltip::Tooltip::text(
                    label.clone(),
                    tooltip_theme.clone(),
                ))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_center()
                        .size(px(20.))
                        .rounded(px(crate::theme::RADIUS_CONTROL))
                        .bg(hsla(theme.workspace_color(workspace.color())))
                        .text_color(hsla(theme.workspace_icon_text))
                        .text_size(px(crate::theme::small_text(theme.font_size)))
                        .font_weight(FontWeight::BOLD)
                        .child(initial(&label)),
                )
                .when(attention, |el| {
                    el.child(
                        div()
                            .absolute()
                            .top(px(3.))
                            .right(px(3.))
                            .size(px(ATTENTION_MARK))
                            .rounded_full()
                            .bg(hsla(theme.attention)),
                    )
                })
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, window, cx| this.select_workspace(id, window, cx)),
                )
        });
        let session_dots = self.session_rows(cx).into_iter().map(|row| {
            let pane = row.pane;
            let color = match row.state {
                crate::session_rows::SessionState::Working => theme.session_working,
                crate::session_rows::SessionState::Waiting(_) => theme.session_waiting,
                crate::session_rows::SessionState::Done => theme.session_done,
                crate::session_rows::SessionState::Error => theme.session_error,
                crate::session_rows::SessionState::Starting
                | crate::session_rows::SessionState::Idle => theme.session_idle,
            };
            let title = format!("{} — {}", row.title(), row.state.label());
            div()
                .id(("collapsed-session", pane.get()))
                .flex()
                .items_center()
                .justify_center()
                .size(px(26.))
                .rounded(px(crate::theme::RADIUS_CONTROL))
                .cursor_pointer()
                .hover(|el| el.bg(hsla(theme.item_hover_background)))
                .tooltip(crate::tooltip::Tooltip::text(title, tooltip_theme.clone()))
                .child(div().size(px(8.)).rounded_full().bg(hsla(color)))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, window, cx| this.select_session(pane, window, cx)),
                )
        });
        div()
            .id("sidebar")
            .debug_selector(|| "sidebar".into())
            .flex()
            .flex_col()
            .flex_none()
            .items_center()
            .gap(px(6.))
            .w(px(COLLAPSED_WIDTH))
            .h_full()
            .py(px(8.))
            .bg(hsla(theme.background))
            .font_family(theme.font_family.clone())
            .text_size(px(theme.font_size))
            .child(expand)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(4.))
                    .children(workspaces),
            )
            .child(
                div()
                    .w(px(24.))
                    .h(px(1.))
                    .bg(crate::design::subtle_border(theme.border, theme.background)),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(4.))
                    .children(session_dots),
            )
    }

    /// The sidebar, as the mock's. Its upper part is the list of
    /// workspaces: a heading with a button for a new one, and a row for
    /// each, the selected one with the active background and a line on
    /// its left. Under it is the session list, which takes the rest of
    /// the height while it has rows; the workspaces then take at most
    /// half.
    pub(crate) fn sidebar(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        self.sidebar.labels.borrow_mut().clear();
        if self.sidebar.collapsed {
            return self.collapsed_sidebar(cx).into_any_element();
        }
        let (sessions, any_session) = self.session_list(window, cx);
        let theme = &self.settings.theme().sidebar;
        let tooltip_theme = self.settings.theme().menu.clone();
        let collapse = div()
            .id("sidebar-collapse")
            .debug_selector(|| "sidebar-collapse".into())
            .px(px(4.))
            .rounded(px(crate::theme::RADIUS_CONTROL))
            .cursor_pointer()
            .hover(|button| {
                button
                    .bg(hsla(theme.item_hover_background))
                    .text_color(hsla(theme.item_text))
            })
            .tooltip(crate::tooltip::Tooltip::with_shortcut(
                "Collapse sidebar",
                "⇧⌘\\",
                tooltip_theme,
            ))
            .child("«")
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| this.toggle_sidebar_collapse(cx)),
            );
        let new = div()
            .id("workspace-new")
            .debug_selector(|| "workspace-new".into())
            .px(px(4.))
            .rounded(px(crate::theme::RADIUS_CONTROL))
            .text_size(px(theme.font_size + 2.))
            .cursor_pointer()
            .hover(|button| {
                button
                    .bg(hsla(theme.item_hover_background))
                    .text_color(hsla(theme.item_text))
            })
            .child("+")
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| this.new_workspace(window, cx)),
            );
        let header = div()
            .flex()
            .flex_none()
            .items_center()
            .justify_between()
            .pt(px(10.))
            .pb(px(6.))
            .px(px(PADDING))
            .text_size(px(crate::theme::small_text(theme.font_size)))
            .text_color(hsla(theme.section_header_text))
            .child("WORKSPACES")
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(2.))
                    .child(new)
                    .child(collapse),
            );
        let mut position = 0;
        let mut headings = 0;
        let listed = self.sidebar_rows(cx);
        let gutter = listed.iter().any(|row| {
            matches!(
                row,
                SidebarRow::Heading(_) | SidebarRow::Workspace { group: Some(_), .. }
            )
        });
        let rows = listed
            .into_iter()
            .map(|row| match row {
                SidebarRow::Workspace {
                    id,
                    depth,
                    branch,
                    group,
                } => {
                    position += 1;
                    let place = RowPlace {
                        depth,
                        branch,
                        group,
                        gutter,
                    };
                    self.workspace_row(position - 1, id, place, cx)
                        .into_any_element()
                }
                SidebarRow::Heading(group) => {
                    headings += 1;
                    self.repository_heading(headings - 1, group, cx)
                        .into_any_element()
                }
            })
            .collect::<Vec<_>>();
        div()
            .id("sidebar")
            .debug_selector(|| "sidebar".into())
            .key_context(SIDEBAR_CONTEXT)
            // The fields and buttons in the sidebar take the keyboard
            // too; the arrows are the sidebar's only while it has it.
            .on_action(cx.listener(|this, _: &CursorDown, _, cx| this.move_cursor(1, cx)))
            .on_action(cx.listener(|this, _: &CursorUp, _, cx| this.move_cursor(-1, cx)))
            .on_action(cx.listener(|this, _: &Choose, window, cx| this.choose(window, cx)))
            .on_action(cx.listener(|this, _: &Fold, _, cx| this.fold_at_cursor(true, cx)))
            .on_action(cx.listener(|this, _: &Unfold, _, cx| this.fold_at_cursor(false, cx)))
            .on_action(cx.listener(|this, _: &LeaveSidebar, window, cx| {
                if this.sidebar.cursor.is_some() {
                    this.leave_sidebar(window, cx);
                }
            }))
            .flex()
            .flex_col()
            .flex_none()
            .w(px(WIDTH))
            .h_full()
            .bg(hsla(theme.background))
            .font_family(theme.font_family.clone())
            .text_size(px(theme.font_size))
            // What takes the keyboard is an element that nothing is drawn
            // in, so that a press on a row, which may give the keyboard
            // to a pane, does not take it to the sidebar.
            .child(
                div()
                    .key_context(CURSOR_CONTEXT)
                    .track_focus(&self.sidebar.focus),
            )
            .child(header)
            .child(
                div()
                    .id("workspace-list")
                    .px(px(6.))
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .map(|list| {
                        if any_session {
                            list.flex_none().max_h(relative(0.5))
                        } else {
                            list.flex_1().min_h_0()
                        }
                    })
                    .overflow_y_scroll()
                    .children(rows),
            )
            .child(sessions)
            .into_any_element()
    }

    /// The name of a repository whose tasks have no workspace of it to
    /// go under, with how many there are. A press folds or opens them.
    fn repository_heading(
        &self,
        position: usize,
        group: Group,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let theme = &self.settings.theme().sidebar;
        let at = self.sidebar.cursor == Some(SidebarItem::Heading(group.repository.clone()));
        let repository = group.repository.clone();
        div()
            .id(("repository-heading", position))
            .debug_selector(move || format!("repository-heading-{position}"))
            .flex()
            .items_center()
            .gap(px(GAP))
            .py(px(4.))
            .pl(px(CHEVRON_INSET))
            .pr(px(PADDING))
            .text_size(px(crate::theme::small_text(theme.font_size)))
            .text_color(hsla(theme.section_header_text))
            .cursor_pointer()
            .hover(|row| row.bg(hsla(theme.item_hover_background)))
            .when(at, |row| row.bg(hsla(theme.item_hover_background)))
            .child(self.fold_mark_box().child(fold_mark(group.folded)))
            .child(div().flex_1().min_w_0().truncate().child(group.name()))
            .child(self.count_pill(group.tasks, false))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, _, cx| this.toggle_folded(&repository, cx)),
            )
    }

    /// What a workspace is shown by in the frame being drawn. See
    /// [`workspace_label`](Self::workspace_label).
    pub(crate) fn drawn_label(&self, id: WorkspaceId, cx: &App) -> String {
        if let Some(label) = self.sidebar.labels.borrow().get(&id) {
            return label.clone();
        }
        let label = self.workspace_label(id, cx).unwrap_or_default();
        self.sidebar.labels.borrow_mut().insert(id, label.clone());
        label
    }

    /// The box of the mark that folds a group: what is pressed.
    fn fold_mark_box(&self) -> gpui::Div {
        let theme = &self.settings.theme().sidebar;
        div()
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .size(px(CHEVRON))
            .rounded(px(crate::theme::RADIUS_CONTROL))
            // The triangles are small in their font.
            .text_size(px(theme.font_size + 3.))
            .text_color(hsla(theme.section_header_text))
    }

    /// How many tasks a group has, apart from the mark that folds it.
    fn count_pill(&self, tasks: usize, active: bool) -> gpui::Div {
        let theme = &self.settings.theme().sidebar;
        let background = if active {
            theme.background
        } else {
            theme.item_hover_background
        };
        div()
            .flex_none()
            .px(px(6.))
            .rounded_full()
            .bg(hsla(background))
            .text_size(px(crate::theme::small_text(theme.font_size)))
            .text_color(hsla(theme.section_header_text))
            .child(tasks.to_string())
    }

    fn workspace_row(
        &self,
        position: usize,
        id: WorkspaceId,
        place: RowPlace,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let theme = &self.settings.theme().sidebar;
        let workspace = self
            .workspaces
            .get(id)
            .expect("the row is of a workspace in the list");
        let label = self.drawn_label(id, cx);
        let active = id == self.workspaces.selected();
        let at = self.sidebar.cursor == Some(SidebarItem::Workspace(id));
        let tabs = workspace.content().tabs.read(cx).tabs().len();
        let group = format!("workspace-row-{id}");
        let icon = div()
            .debug_selector(move || format!("workspace-icon-{position}"))
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .size(px(ICON))
            .rounded(px(crate::theme::RADIUS_CONTROL))
            .bg(hsla(theme.workspace_color(workspace.color())))
            .text_color(hsla(theme.workspace_icon_text))
            .text_size(px(crate::theme::small_text(theme.font_size)))
            .font_weight(FontWeight::BOLD)
            .child(initial(&label));
        // The name gives way to the field while it is being typed.
        let editor = self
            .renaming
            .as_ref()
            .filter(|renaming| renaming.id == id)
            .map(|renaming| renaming.editor.clone());
        let name = div()
            .flex_1()
            .min_w_0()
            .truncate()
            .text_color(hsla(theme.item_text))
            .when(active, |name| name.font_weight(FontWeight::SEMIBOLD))
            .child(label.clone());
        // The cross shows on the selected row and on the one under the
        // mouse.
        let close = div()
            .id(("workspace-close", id.get()))
            .debug_selector(move || format!("workspace-close-{position}"))
            .flex_none()
            .px(px(4.))
            .rounded(px(crate::theme::RADIUS_CONTROL))
            .text_color(hsla(theme.item_secondary_text))
            .hover(|close| close.text_color(hsla(theme.item_text)))
            .when(!active, |close| {
                close
                    .invisible()
                    .group_hover(group.clone(), |close| close.visible())
            })
            .child("×")
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, window, cx| {
                    // The press is not also one on the row.
                    cx.stop_propagation();
                    this.ask_to_close_workspace(id, window, cx);
                }),
            );
        // A dot after the name says that a pane of the workspace needs
        // the user.
        let attention = workspace.content().tabs.read(cx).needs_attention(cx);
        let mark = attention.then(|| {
            div()
                .debug_selector(move || format!("workspace-attention-{position}"))
                .flex_none()
                .size(px(ATTENTION_MARK))
                .rounded_full()
                .bg(hsla(theme.attention))
        });
        // The repository's workspace folds and opens its tasks with a
        // mark that says how many there are.
        let count = place.group.as_ref().map(|group| {
            self.count_pill(group.tasks, active)
                .debug_selector(move || format!("workspace-count-{position}"))
        });
        let fold = match place.group {
            Some(group) => {
                let repository = group.repository.clone();
                let fold = self
                    .fold_mark_box()
                    .id(("workspace-fold", id.get()))
                    .debug_selector(move || format!("workspace-fold-{position}"))
                    .hover(|fold| {
                        fold.bg(hsla(theme.item_active_background))
                            .text_color(hsla(theme.item_text))
                    })
                    .child(fold_mark(group.folded))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            // The press is not also one on the row.
                            cx.stop_propagation();
                            this.toggle_folded(&repository, cx);
                        }),
                    );
                Some(fold.into_any_element())
            }
            // The rows without a group leave the room of the mark, so
            // that the icons stand in one column.
            None if place.gutter && place.depth == 0 => {
                Some(div().flex_none().size(px(CHEVRON)).into_any_element())
            }
            None => None,
        };
        let marked = fold.is_some();
        let top = div()
            .flex()
            .items_center()
            .gap(px(GAP))
            .children(fold)
            .child(icon)
            .map(|top| match editor {
                Some(editor) => top.child(editor),
                None => top.child(name).children(mark).children(count).child(close),
            });
        // A task's workspace says its branch, as the mock's tasks do.
        let secondary = match place.branch {
            Some(branch) => div()
                .debug_selector(move || format!("workspace-branch-{position}"))
                .truncate()
                .child(branch),
            None => div().child(tab_count(tabs)),
        }
        .pl(px(if marked {
            CHEVRON + GAP + ICON + GAP
        } else {
            ICON + GAP
        }))
        .mt(px(2.))
        .text_size(px(crate::theme::small_text(theme.font_size)))
        .text_color(hsla(theme.item_secondary_text));
        // Where the row's first thing starts: the mark of a group at the
        // edge, a task under the icon of its group.
        let left = if place.depth > 0 {
            CHEVRON_INSET + CHEVRON + GAP + INDENT
        } else if place.gutter {
            CHEVRON_INSET
        } else {
            PADDING
        };
        div()
            .id(("workspace", id.get()))
            .debug_selector(move || format!("workspace-{position}"))
            .group(group)
            .flex()
            .flex_col()
            .relative()
            .overflow_hidden()
            .rounded(px(crate::theme::RADIUS_CARD))
            .py(px(6.))
            .pl(px((left - INDICATOR - 6.).max(6.)))
            .pr(px(PADDING - 4.))
            // A line down the left of the tasks of a group, under the
            // mark that folds it.
            .when(place.depth > 0, |row| {
                row.child(
                    div()
                        .absolute()
                        .top_0()
                        .bottom_0()
                        .left(px((CHEVRON_INSET + CHEVRON / 2. - INDICATOR - 6.).max(2.)))
                        .w(px(1.))
                        .bg(crate::design::subtle_border(theme.border, theme.background)),
                )
            })
            .border_l(px(INDICATOR))
            .border_color(gpui::transparent_black())
            .cursor_pointer()
            .map(|row| {
                if active {
                    row.bg(hsla(theme.item_active_background))
                        .border_color(hsla(theme.item_active_indicator))
                        .child(crate::design::top_highlight_bar(
                            theme.item_active_background,
                        ))
                } else if at {
                    row.bg(hsla(theme.item_hover_background))
                } else {
                    row.hover(|row| row.bg(hsla(theme.item_hover_background)))
                }
            })
            // The cursor on the selected row is a line of the text's
            // color in place of the accent.
            .when(active && at, |row| row.border_color(hsla(theme.item_text)))
            .child(top)
            .child(secondary)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    // A press in the field belongs to the field.
                    if this.renaming() == Some(id) {
                        return;
                    }
                    if event.click_count >= 2 {
                        this.start_renaming(id, window, cx);
                    } else {
                        this.select_workspace(id, window, cx);
                    }
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    if this.renaming() == Some(id) {
                        return;
                    }
                    let target = crate::context_menu::MenuTarget::Workspace(id);
                    this.open_context_menu(target, event.position, window, cx);
                }),
            )
    }
}

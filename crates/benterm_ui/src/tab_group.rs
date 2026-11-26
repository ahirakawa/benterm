//! The tabs of a window, each with a grid of panes, and the bar that shows
//! them.
//!
//! The order, the selection and the names the user gives tabs live in a
//! [`TabList`], which knows nothing about the screen. This view connects
//! the list to the keys, the mouse and the grids.

use std::collections::HashSet;
use std::path::PathBuf;

use benterm_layout::{PaneId, TabError, TabId, TabList};
use benterm_terminal::Notification;
use gpui::prelude::FluentBuilder;
use gpui::{
    Action, App, AppContext, Context, Entity, EntityId, EventEmitter, InteractiveElement,
    IntoElement, KeyBinding, MouseButton, MouseDownEvent, ParentElement, Pixels, Point, Render,
    ScrollHandle, StatefulInteractiveElement, Styled, Subscription, Window, actions, div, px,
};

use crate::APP_NAME;
use crate::name_editor::{NameEditor, NameEditorEvent};
use crate::pane_grid::{FirstPane, PaneGrid, PaneGridEvent, RestoredPanes, SpawnTerminal};
use crate::terminal_view::hsla;
use crate::theme::{TabTheme, Theme};

actions!(
    tab,
    [
        /// Opens a tab right of the selected one, with a shell in it.
        NewTab,
        /// Closes the focused pane, or the tab with its last pane, or the
        /// window with its last tab.
        Close,
        CloseOtherTabs,
        /// Selects the tab on the right, or the first after the last.
        SelectNextTab,
        /// Selects the tab on the left, or the last before the first.
        SelectPreviousTab,
        SelectLastTab,
        /// Starts typing a name for the selected tab, which it shows in
        /// place of the title of its program. An empty name takes the name
        /// away.
        RenameTab,
    ]
);

/// Selects the tab with this number, counted from 1 on the left, or the
/// last tab if there are fewer.
#[derive(Clone, Debug, PartialEq, Action)]
#[action(namespace = tab, no_json)]
pub struct GotoTab(pub usize);

/// The key context of the group, which its bindings are limited to.
const KEY_CONTEXT: &str = "TabGroup";

/// Binds the tab keys, which follow Ghostty's macOS defaults.
pub fn bind_keys(cx: &mut App) {
    let context = Some(KEY_CONTEXT);
    let mut bindings = vec![
        KeyBinding::new("cmd-t", NewTab, context),
        KeyBinding::new("cmd-w", Close, context),
        // Safari's key for the same; Ghostty has no such command.
        KeyBinding::new("cmd-alt-w", CloseOtherTabs, context),
        // Ghostty binds no key to the same; Option is for a tab as with
        // Close Other Tabs, and Shift for a workspace, as Cmd-Shift-R
        // renames one.
        KeyBinding::new("cmd-alt-r", RenameTab, context),
        KeyBinding::new("cmd-9", SelectLastTab, context),
        // The same keys as macOS reports them with the shifted character.
        // The command palette shows the binding added last, so the one
        // written as on the keyboard comes second.
        KeyBinding::new("cmd-{", SelectPreviousTab, context),
        KeyBinding::new("cmd-shift-[", SelectPreviousTab, context),
        KeyBinding::new("cmd-}", SelectNextTab, context),
        KeyBinding::new("cmd-shift-]", SelectNextTab, context),
    ];
    let numbers = [
        "cmd-1", "cmd-2", "cmd-3", "cmd-4", "cmd-5", "cmd-6", "cmd-7", "cmd-8",
    ];
    for (index, key) in numbers.into_iter().enumerate() {
        bindings.push(KeyBinding::new(key, GotoTab(index + 1), context));
    }
    cx.bind_keys(bindings);
}

const BAR_HEIGHT: f32 = 32.;
/// The space on each side of a tab's text.
const TAB_PADDING: f32 = 14.;
/// The thickness of the line under the selected tab.
const INDICATOR: f32 = 2.;
const NEW_TAB_BUTTON: f32 = 24.;

/// The side of the dot before the label of a tab with a pane that needs
/// the user.
const ATTENTION_MARK: f32 = 6.;

/// The width of the field for a tab's name, which takes the place of the
/// label while the name is typed.
const NAME_FIELD_WIDTH: f32 = 140.;

/// The label a tab shows while the program in its focused pane has set no
/// title.
const UNTITLED: &str = "Terminal";

/// What a tab group tells the window it is in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TabGroupEvent {
    /// The selected tab, or the title of its focused pane, changed.
    TitleChanged,
    /// The program of a pane asked for a notification.
    Notified(PaneId, Notification),
    Emptied,
    /// A right click on a pane asked for its menu at this point in the
    /// window.
    MenuRequested(PaneId, Point<Pixels>),
    /// A right click on a tab asked for its menu at this point in the
    /// window.
    TabMenuRequested(TabId, Point<Pixels>),
    /// A command-click on a web address in the terminal of a pane.
    OpenUrl(PaneId, String),
    /// A command-click on an existing file or folder path in the terminal
    /// of a pane.
    OpenPath(PaneId, PathBuf),
    /// A pane's program failed on startup, right before the pane closes.
    StartupFailed(PaneId, crate::toast::StartupFailure),
}

struct Tab {
    grid: Entity<PaneGrid>,
    _events: Subscription,
}

/// A tab whose name is being typed.
struct Renaming {
    id: TabId,
    editor: Entity<NameEditor>,
    _subscriptions: [Subscription; 2],
}

/// The tabs of a workspace.
pub struct TabGroup {
    tabs: TabList<Tab>,
    renaming: Option<Renaming>,
    spawn: SpawnTerminal,
    theme: Theme,
    /// The scroll of the tabs in the bar when there are more than fit.
    scroll: ScrollHandle,
}

impl TabGroup {
    /// A group with one tab whose single pane is `first`. `spawn` starts
    /// the terminals of every later pane and tab.
    pub fn new(
        first: FirstPane,
        spawn: SpawnTerminal,
        theme: Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let tab = Self::new_tab_with(first, spawn.clone(), theme.clone(), window, cx);
        Self {
            tabs: TabList::new(tab),
            renaming: None,
            spawn,
            theme,
            scroll: ScrollHandle::new(),
        }
    }

    fn new_tab_with(
        first: FirstPane,
        spawn: SpawnTerminal,
        theme: Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Tab {
        let grid = cx.new(|cx| PaneGrid::new(first, spawn, theme, window, cx));
        Self::tab_with(grid, window, cx)
    }

    /// A tab showing `grid`, whose events the group follows.
    fn tab_with(grid: Entity<PaneGrid>, window: &mut Window, cx: &mut Context<Self>) -> Tab {
        let events = cx.subscribe_in(&grid, window, |this, grid, event, window, cx| {
            this.on_grid_event(grid.entity_id(), event.clone(), window, cx);
        });
        Tab {
            grid,
            _events: events,
        }
    }

    /// A group with a tab for each of `tabs`, the panes of a saved
    /// session whose terminals have started. `selected` counts the tabs
    /// from 0. There has to be a tab.
    pub fn restored(
        tabs: Vec<RestoredPanes>,
        selected: usize,
        spawn: SpawnTerminal,
        theme: Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let tabs = tabs
            .into_iter()
            .map(|panes| {
                let (spawn, theme) = (spawn.clone(), theme.clone());
                let grid = cx.new(|cx| PaneGrid::restored(panes, spawn, theme, window, cx));
                Self::tab_with(grid, window, cx)
            })
            .collect();
        Self {
            tabs: TabList::from_tabs(tabs, selected).expect("a saved workspace has a tab"),
            renaming: None,
            spawn,
            theme,
            scroll: ScrollHandle::new(),
        }
    }

    /// The tabs from left to right.
    pub fn tabs(&self) -> Vec<TabId> {
        self.tabs.ids()
    }

    pub fn selected(&self) -> TabId {
        self.tabs.selected()
    }

    /// The panes of a tab.
    pub fn grid(&self, id: TabId) -> Option<&Entity<PaneGrid>> {
        self.tabs.get(id).map(|tab| &tab.grid)
    }

    pub fn selected_grid(&self) -> &Entity<PaneGrid> {
        &self.tabs.selected_tab().grid
    }

    /// The text a tab shows: the name the user gave it, or else the title
    /// of its focused pane.
    pub fn label(&self, id: TabId, cx: &App) -> String {
        self.name_or_title(id, cx)
            .unwrap_or_else(|| UNTITLED.into())
    }

    fn name_or_title(&self, id: TabId, cx: &App) -> Option<String> {
        if let Some(name) = self.tabs.name(id) {
            return Some(name.to_owned());
        }
        self.grid(id).and_then(|grid| grid.read(cx).title(cx))
    }

    /// The name the user gave a tab, if they gave it one.
    pub fn name(&self, id: TabId) -> Option<&str> {
        self.tabs.name(id)
    }

    /// Gives a tab a name, or takes its name away with an empty one. A
    /// name of more than one line is refused.
    pub fn rename_tab(&mut self, id: TabId, name: &str, cx: &mut Context<Self>) {
        if self.tabs.rename(id, name).is_ok() {
            if id == self.tabs.selected() {
                cx.emit(TabGroupEvent::TitleChanged);
            }
            cx.notify();
        }
    }

    /// The tab whose name is being typed, if one is.
    pub fn renaming(&self) -> Option<TabId> {
        self.renaming.as_ref().map(|renaming| renaming.id)
    }

    /// Puts a field in the place of a tab's label and gives it the
    /// keyboard, as for a workspace's name. Enter takes the name, and an
    /// empty one takes the name away; Escape or leaving the field keeps
    /// the label as it was. A tab whose name is already being typed keeps
    /// its field and what is in it.
    pub fn start_renaming(&mut self, id: TabId, window: &mut Window, cx: &mut Context<Self>) {
        if self.renaming() == Some(id) || self.tabs.get(id).is_none() {
            return;
        }
        let text = self.tabs.name(id).unwrap_or_default();
        let theme = self.theme.tabs.clone();
        let editor = cx.new(|cx| NameEditor::new(text, theme, cx));
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
    /// and gives the keyboard back to the selected tab.
    fn finish_renaming(&mut self, name: Option<&str>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(renaming) = self.renaming.take() else {
            return;
        };
        if let Some(name) = name {
            self.rename_tab(renaming.id, name, cx);
        }
        self.show_selected(true, window, cx);
    }

    /// Ends the typing of a name whose tab has closed. Returns whether it
    /// did, and so whether the keyboard was in the field.
    fn end_renaming_of_closed_tab(&mut self) -> bool {
        let closed = self
            .renaming
            .as_ref()
            .is_some_and(|renaming| self.tabs.get(renaming.id).is_none());
        if closed {
            self.renaming = None;
        }
        closed
    }

    /// Opens a tab right of the selected one with a new shell, and selects
    /// it. The shell starts in the directory of the selected tab's focused
    /// pane.
    pub fn new_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let directory = self.selected_grid().read(cx).directory_for_new_terminal(cx);
        let theme = &self.theme.terminal;
        let size = crate::pane_area::terminal_size(window, theme, cx);
        let first = FirstPane::start(&self.spawn, theme, directory.as_deref(), size);
        let first = match first {
            Ok(first) => first,
            Err(error) => {
                eprintln!("benterm: could not start a terminal for the new tab: {error}");
                return;
            }
        };
        let tab = Self::new_tab_with(first, self.spawn.clone(), self.theme.clone(), window, cx);
        self.tabs.open(tab);
        self.show_selected(true, window, cx);
    }

    /// Opens a tab right of the selected one for a terminal that has been
    /// running behind the window. With `select` the tab is shown and gets
    /// the keyboard; without, the selection and the keyboard stay where
    /// they are, so a program that opens tabs does not take them from the
    /// user.
    pub(crate) fn open_tab_adopting(
        &mut self,
        lone: crate::pane_grid::LoneTerminal,
        select: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> TabId {
        let (spawn, theme) = (self.spawn.clone(), self.theme.clone());
        let grid = cx.new(|cx| PaneGrid::adopting(lone, spawn, theme, window, cx));
        let tab = Self::tab_with(grid, window, cx);
        let before = self.tabs.selected();
        let id = self.tabs.open(tab);
        if select {
            self.show_selected(true, window, cx);
        } else {
            let _ = self.tabs.select(before);
            cx.notify();
        }
        id
    }

    /// Replaces every tab with one for a terminal that has been running
    /// behind the window, and gives it the keyboard. The tabs there were
    /// close with their panes.
    pub(crate) fn replace_with_adopted(
        &mut self,
        lone: crate::pane_grid::LoneTerminal,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (spawn, theme) = (self.spawn.clone(), self.theme.clone());
        let grid = cx.new(|cx| PaneGrid::adopting(lone, spawn, theme, window, cx));
        self.tabs = TabList::new(Self::tab_with(grid, window, cx));
        self.renaming = None;
        self.show_selected(true, window, cx);
    }

    /// Closes the focused pane of the selected tab. With its last pane the
    /// tab closes, and with the last tab the window.
    pub fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.selected_grid()
            .clone()
            .update(cx, |grid, cx| grid.close_focused(window, cx));
    }

    /// Selects a tab and gives the keyboard to its focused pane.
    pub fn select(&mut self, id: TabId, window: &mut Window, cx: &mut Context<Self>) {
        if self.tabs.select(id).is_ok() {
            self.show_selected(true, window, cx);
        }
    }

    /// Moves a tab to `position`, counted from 0 on the left.
    pub fn move_tab(&mut self, id: TabId, position: usize, cx: &mut Context<Self>) {
        if self.tabs.move_to(id, position).is_ok() {
            cx.notify();
        }
    }

    fn select_with(
        &mut self,
        change: impl FnOnce(&mut TabList<Tab>),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let before = self.tabs.selected();
        change(&mut self.tabs);
        if self.tabs.selected() != before {
            self.show_selected(true, window, cx);
        }
    }

    /// Whether the keyboard focus is in one of the group's panes.
    pub(crate) fn has_focus(&self, window: &Window, cx: &App) -> bool {
        self.tabs
            .iter()
            .any(|(_, tab)| tab.grid.read(cx).has_focus(window, cx))
    }

    /// Gives the keyboard to the selected tab's focused pane.
    pub fn focus_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.show_selected(true, window, cx);
    }

    /// Redraws with the selected tab in view in the bar, and with `focus`
    /// gives the keyboard to its focused pane.
    fn show_selected(&mut self, focus: bool, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(position) = self.tabs.position(self.tabs.selected()) {
            self.scroll.scroll_to_item(position);
        }
        if focus {
            self.selected_grid()
                .clone()
                .update(cx, |grid, cx| grid.focus_terminal(window, cx));
        }
        cx.emit(TabGroupEvent::TitleChanged);
        cx.notify();
    }

    fn tab_of(&self, grid: EntityId) -> Option<TabId> {
        self.tabs
            .iter()
            .find(|(_, tab)| tab.grid.entity_id() == grid)
            .map(|(id, _)| id)
    }

    /// Closes every tab but `id`, with their panes, and selects `id`.
    pub(crate) fn close_other_tabs(
        &mut self,
        id: TabId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.tabs.get(id).is_none() {
            return;
        }
        // The keyboard goes to the tab left only if it was in the group.
        let had_focus = self.has_focus(window, cx);
        for other in self.tabs.ids() {
            if other != id {
                let _ = self.tabs.close(other);
            }
        }
        let renamed = self.end_renaming_of_closed_tab();
        let _ = self.tabs.select(id);
        self.show_selected(had_focus || renamed, window, cx);
    }

    /// Closes a tab and its panes. The last tab empties the group.
    pub(crate) fn close_tab(&mut self, id: TabId, window: &mut Window, cx: &mut Context<Self>) {
        let was_selected = self.tabs.selected() == id;
        // The tab that takes its place gets the keyboard only if the
        // closed one had it. A group in a workspace that is not shown, or
        // one whose window is taking a name, leaves the keyboard alone.
        let had_focus = self
            .grid(id)
            .is_some_and(|grid| grid.read(cx).has_focus(window, cx));
        let closed = self.tabs.close(id);
        // The field for its name goes with it, and the keyboard with it.
        let renamed = closed.is_ok() && self.end_renaming_of_closed_tab();
        match closed {
            Ok(_) if was_selected || renamed => {
                self.show_selected(had_focus || renamed, window, cx)
            }
            Ok(_) => cx.notify(),
            Err(TabError::LastTab) => cx.emit(TabGroupEvent::Emptied),
            Err(_) => {}
        }
    }

    /// Whether a pane of a tab needs the user.
    pub fn tab_needs_attention(&self, id: TabId, cx: &App) -> bool {
        self.grid(id)
            .is_some_and(|grid| grid.read(cx).has_attention())
    }

    /// Whether a pane of any tab needs the user.
    pub fn needs_attention(&self, cx: &App) -> bool {
        self.tabs
            .iter()
            .any(|(_, tab)| tab.grid.read(cx).has_attention())
    }

    /// Marks the panes of the group that are among `marked`, and only
    /// those.
    pub(crate) fn set_attention(&mut self, marked: &HashSet<PaneId>, cx: &mut Context<Self>) {
        for (_, tab) in self.tabs.iter() {
            tab.grid
                .update(cx, |grid, cx| grid.set_attention(marked, cx));
        }
        cx.notify();
    }

    /// The title the window shows for the selected tab: its name, or the
    /// title of its focused pane.
    pub fn window_title(&self, cx: &App) -> String {
        self.name_or_title(self.tabs.selected(), cx)
            .unwrap_or_else(|| APP_NAME.into())
    }

    /// Draws every tab with `theme`, and starts new ones with it.
    pub fn set_theme(&mut self, theme: Theme, cx: &mut Context<Self>) {
        for (_, tab) in self.tabs.iter() {
            let theme = theme.clone();
            tab.grid.update(cx, |grid, cx| grid.set_theme(theme, cx));
        }
        if let Some(renaming) = &self.renaming {
            let field = theme.tabs.clone();
            renaming
                .editor
                .update(cx, |editor, cx| editor.set_theme(field, cx));
        }
        self.theme = theme;
        cx.notify();
    }

    fn on_grid_event(
        &mut self,
        grid: EntityId,
        event: PaneGridEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(id) = self.tab_of(grid) else {
            return;
        };
        match event {
            PaneGridEvent::TitleChanged => {
                if id == self.tabs.selected() {
                    cx.emit(TabGroupEvent::TitleChanged);
                }
                // The tab's label follows the title.
                cx.notify();
            }
            PaneGridEvent::Notified(pane, notification) => {
                cx.emit(TabGroupEvent::Notified(pane, notification));
            }
            PaneGridEvent::Emptied => self.close_tab(id, window, cx),
            PaneGridEvent::MenuRequested(pane, position) => {
                cx.emit(TabGroupEvent::MenuRequested(pane, position));
            }
            PaneGridEvent::OpenUrl(pane, url) => cx.emit(TabGroupEvent::OpenUrl(pane, url)),
            PaneGridEvent::OpenPath(pane, path) => cx.emit(TabGroupEvent::OpenPath(pane, path)),
            PaneGridEvent::StartupFailed(pane, failure) => {
                cx.emit(TabGroupEvent::StartupFailed(pane, failure));
            }
        }
    }

    /// The bar of tabs, as in the mock: the selected tab takes the pane
    /// background and a line under it.
    fn tab_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = &self.theme.tabs;
        let selected = self.tabs.selected();
        let tabs = self.tabs.iter().enumerate().map(|(position, (id, tab))| {
            let label = self.label(id, cx);
            let panes = tab.grid.read(cx).panes().len();
            let attention = tab.grid.read(cx).has_attention();
            let active = id == selected;
            let field = self
                .renaming
                .as_ref()
                .filter(|renaming| renaming.id == id)
                .map(|renaming| renaming.editor.clone());
            let dragged = DraggedTab {
                id,
                label: label.clone(),
                theme: theme.clone(),
            };
            div()
                .id(("tab", id.get()))
                .debug_selector(|| format!("tab-{position}"))
                .relative()
                .overflow_hidden()
                .flex()
                .flex_none()
                .items_center()
                .gap(px(6.))
                .h_full()
                .px(px(TAB_PADDING))
                .rounded_t(px(crate::theme::RADIUS_CARD))
                .text_color(hsla(theme.text))
                .cursor_pointer()
                // A tab that is not selected answers the mouse.
                .when(!active, |tab| {
                    tab.opacity(0.72).hover(|tab| {
                        tab.opacity(1.0)
                            .bg(hsla(theme.button_hover_background))
                            .text_color(hsla(theme.active_text))
                    })
                })
                .when(active, |tab| {
                    tab.bg(hsla(theme.active_background))
                        .text_color(hsla(theme.active_text))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child(crate::design::top_highlight_bar(theme.active_background))
                        .child(
                            div()
                                .absolute()
                                .left_0()
                                .right_0()
                                .bottom_0()
                                .h(px(INDICATOR))
                                .bg(hsla(theme.active_indicator)),
                        )
                })
                .when(attention, |tab| {
                    tab.child(
                        div()
                            .debug_selector(|| format!("tab-attention-{position}"))
                            .flex_none()
                            .size(px(ATTENTION_MARK))
                            .rounded_full()
                            .bg(hsla(theme.attention)),
                    )
                })
                .map(|tab| match field {
                    // A press in the field stays there, and does not
                    // select the tab, which would end the typing.
                    Some(field) => tab.child(
                        div()
                            .id("tab-name-editor")
                            .debug_selector(|| "tab-name-editor".into())
                            .flex()
                            .w(px(NAME_FIELD_WIDTH))
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .child(field),
                    ),
                    None => tab.child(label),
                })
                .when(panes > 1, |tab| {
                    tab.child(
                        div()
                            .text_size(px(crate::theme::small_text(theme.font_size)))
                            .text_color(hsla(theme.secondary_text))
                            .child(format!("{panes} panes")),
                    )
                })
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, window, cx| this.select(id, window, cx)),
                )
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |_, event: &MouseDownEvent, _, cx| {
                        cx.emit(TabGroupEvent::TabMenuRequested(id, event.position));
                    }),
                )
                .on_drag(dragged, |dragged, _, _, cx| cx.new(|_| dragged.clone()))
                .drag_over::<DraggedTab>({
                    let color = hsla(theme.active_background);
                    move |style, _, _, _| style.bg(color)
                })
                .on_drop(cx.listener(move |this, dragged: &DraggedTab, _, cx| {
                    this.move_tab(dragged.id, position, cx)
                }))
        });
        // The tabs scroll sideways when there are more than fit, and shrink
        // to leave room for the button after them.
        let list = div()
            .id("tab-list")
            .flex()
            .flex_initial()
            .min_w_0()
            .h_full()
            .overflow_x_scroll()
            .track_scroll(&self.scroll)
            .children(tabs);
        // A + right of the last tab, drawn as the one by the workspaces'
        // heading in the sidebar.
        let new = div()
            .id("tab-new")
            .debug_selector(|| "tab-new".into())
            .flex()
            .items_center()
            .justify_center()
            .size(px(NEW_TAB_BUTTON))
            .rounded(px(crate::theme::RADIUS_CONTROL))
            .text_size(px(theme.font_size + 2.))
            .text_color(hsla(theme.text))
            .cursor_pointer()
            .hover(|button| {
                button
                    .bg(hsla(theme.button_hover_background))
                    .text_color(hsla(theme.active_text))
            })
            .child("+")
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| this.new_tab(window, cx)),
            );
        let new = div()
            .id("tab-new-cell")
            .flex()
            .flex_none()
            .items_center()
            .h_full()
            .px(px(6.))
            .child(new)
            .on_drop(cx.listener(|this, dragged: &DraggedTab, _, cx| {
                this.move_tab(dragged.id, usize::MAX, cx)
            }));
        // The empty end of the bar takes a tab to the right end.
        let rest = div()
            .id("tab-bar-end")
            .flex_1()
            .h_full()
            .on_drop(cx.listener(|this, dragged: &DraggedTab, _, cx| {
                this.move_tab(dragged.id, usize::MAX, cx)
            }));
        // The end of the bar is left to the switch of the views, which
        // is drawn under it: nothing of the bar is painted there.
        let painted = div()
            .flex()
            .flex_1()
            .min_w_0()
            .h_full()
            // In a narrow window the tabs are cut at the switch.
            .overflow_hidden()
            .bg(hsla(theme.bar_background))
            .border_b_1()
            .border_color(hsla(theme.separator))
            .child(list)
            .child(new)
            .child(rest);
        div()
            .id("tab-bar")
            .debug_selector(|| "tab-bar".into())
            .flex()
            .flex_none()
            .h(px(BAR_HEIGHT))
            .font_family(theme.font_family.clone())
            .text_size(px(theme.font_size))
            .child(painted)
            .child(div().flex_none().w(px(crate::main_view::VIEW_SWITCH_WIDTH)))
    }
}

impl EventEmitter<TabGroupEvent> for TabGroup {}

impl Render for TabGroup {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .flex_col()
            .key_context(KEY_CONTEXT)
            .on_action(cx.listener(|this, _: &NewTab, window, cx| this.new_tab(window, cx)))
            .on_action(cx.listener(|this, _: &Close, window, cx| this.close(window, cx)))
            .on_action(cx.listener(|this, _: &CloseOtherTabs, window, cx| {
                this.close_other_tabs(this.selected(), window, cx)
            }))
            .on_action(cx.listener(|this, _: &SelectNextTab, window, cx| {
                this.select_with(TabList::select_next, window, cx)
            }))
            .on_action(cx.listener(|this, _: &SelectPreviousTab, window, cx| {
                this.select_with(TabList::select_previous, window, cx)
            }))
            .on_action(cx.listener(|this, _: &SelectLastTab, window, cx| {
                this.select_with(TabList::select_last, window, cx)
            }))
            .on_action(cx.listener(|this, _: &RenameTab, window, cx| {
                this.start_renaming(this.selected(), window, cx)
            }))
            .on_action(cx.listener(|this, action: &GotoTab, window, cx| {
                let number = action.0;
                this.select_with(
                    |tabs| {
                        let _ = tabs.goto(number);
                    },
                    window,
                    cx,
                )
            }))
            .child(self.tab_bar(cx))
            .child(div().flex_1().min_h_0().child(self.selected_grid().clone()))
    }
}

/// A tab being dragged, and how it looks under the mouse.
#[derive(Clone)]
struct DraggedTab {
    id: TabId,
    label: String,
    theme: TabTheme,
}

impl Render for DraggedTab {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let theme = &self.theme;
        div()
            .flex()
            .items_center()
            .h(px(BAR_HEIGHT))
            .px(px(TAB_PADDING))
            .bg(hsla(theme.active_background))
            .border_1()
            .border_color(hsla(theme.separator))
            .text_color(hsla(theme.active_text))
            .font_family(theme.font_family.clone())
            .text_size(px(theme.font_size))
            .opacity(0.9)
            .child(self.label.clone())
    }
}

#[cfg(test)]
mod tests {
    use benterm_terminal::PtyConfig;
    use gpui::{EntityInputHandler, TestAppContext, VisualTestContext};

    use crate::{bind_keys, open_terminal_window, settings::ConfigFeed};

    #[gpui::test]
    fn escape_during_tab_name_composition_keeps_the_name_and_returns_focus(
        cx: &mut TestAppContext,
    ) {
        cx.executor().allow_parking();
        cx.update(bind_keys);
        let window = cx
            .update(|cx| {
                open_terminal_window(
                    PtyConfig::command("/bin/cat", [] as [&str; 0]),
                    ConfigFeed::defaults(),
                    cx,
                )
            })
            .unwrap();
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        cx.update(|window, _| window.activate_window());
        cx.simulate_keystrokes("cmd-alt-r");
        let (tabs, id, editor) = window
            .read_with(&cx, |root, cx| {
                let tabs = root.tabs();
                let renaming = tabs.read(cx).renaming.as_ref().unwrap();
                (tabs.clone(), renaming.id, renaming.editor.clone())
            })
            .unwrap();
        editor.update_in(&mut cx, |editor, window, cx| {
            editor.replace_and_mark_text_in_range(None, "にほん", None, window, cx);
            assert!(editor.marked_text_range(window, cx).is_some());
        });

        cx.simulate_keystrokes("escape");
        tabs.update_in(&mut cx, |tabs, window, cx| {
            assert!(tabs.renaming.is_none());
            assert_eq!(tabs.name(id), None);
            assert!(tabs.selected_grid().read(cx).has_focus(window, cx));
        });
    }
}

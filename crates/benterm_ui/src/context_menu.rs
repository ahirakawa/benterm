//! The menu a right click opens on a tab, a row of the workspaces, a row
//! of the session list or a terminal pane.
//!
//! An item is an action that the menu bar, the keys and the command
//! palette run too, and it shows the keys bound to that action, unless
//! those keys do something else, as Cmd-W does for a tab. What an
//! item does is that action's operation, aimed at what was clicked, which
//! need not be what is selected: Close on a tab that is not selected
//! closes that tab. The only operations made for the menu are
//! [`CloseOtherTabs`] and [`CloseSession`], which the menu bar and the
//! palette have as well.
//!
//! A pane whose program asks for mouse reports, as vim and Claude Code
//! do, gets the right click; Shift with the right click opens the menu
//! there. A browser pane leaves the right click to the page.

use benterm_layout::{PaneId, SplitDirection, TabId, WorkspaceId};
use gpui::prelude::FluentBuilder;
use gpui::{
    Action, App, AppContext, Context, Entity, EventEmitter, FocusHandle, InteractiveElement,
    IntoElement, KeyBinding, MouseButton, ParentElement, Pixels, Point, Render, Styled,
    Subscription, Window, actions, anchored, deferred, div, px,
};

use crate::command_match::key_label;
use crate::session_list::CloseSession;
use crate::terminal_view::hsla;
use crate::theme::MenuTheme;
use crate::{
    BentermWindow, Close, CloseOtherTabs, CloseWorkspace, Copy, NewTab, NewWorkspace,
    OpenPeekAsTab, Paste, PeekNextAgent, RenameTab, RenameWorkspace, SplitDown, SplitRight,
};

actions!(
    context_menu,
    [
        SelectNextItem,
        SelectPreviousItem,
        RunItem,
        /// Closes the menu without running anything.
        Dismiss,
    ]
);

/// The key context of the menu, which its bindings are limited to.
const KEY_CONTEXT: &str = "ContextMenu";

const MIN_WIDTH: f32 = 200.;

/// Binds the arrows, Enter and Escape while the menu has the keyboard.
pub fn bind_keys(cx: &mut App) {
    let context = Some(KEY_CONTEXT);
    cx.bind_keys([
        KeyBinding::new("down", SelectNextItem, context),
        KeyBinding::new("up", SelectPreviousItem, context),
        KeyBinding::new("enter", RunItem, context),
        KeyBinding::new("escape", Dismiss, context),
    ]);
}

/// What was right-clicked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MenuTarget {
    Tab(WorkspaceId, TabId),
    Workspace(WorkspaceId),
    /// The row of the agent whose terminal has this id.
    Session(PaneId),
    Pane(PaneId),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MenuItemShown {
    pub name: String,
    /// The keys bound to the item's action, or nothing if none are.
    pub keys: String,
    /// Whether it can be chosen now.
    pub enabled: bool,
}

/// The keys bound to `action`, as the menu bar shows them: the binding
/// added last, whatever has the keyboard.
fn keys_of(action: &dyn Action, cx: &App) -> String {
    let keymap = cx.key_bindings();
    let keymap = keymap.borrow();
    keymap
        .bindings_for_action(action)
        .next_back()
        .map(|binding| {
            binding
                .keystrokes()
                .iter()
                .map(|keystroke| key_label(keystroke.modifiers(), keystroke.key()))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or_default()
}

/// An item before its keys are read.
struct MenuEntry {
    name: &'static str,
    action: Box<dyn Action>,
    /// Whether it can be chosen now.
    enabled: bool,
    /// Whether the keys of `action` do what the item does, and so are
    /// shown.
    keys: bool,
}

/// An item that does what its action's keys do.
fn entry(name: &'static str, action: impl Action, enabled: bool) -> MenuEntry {
    MenuEntry {
        name,
        action: Box::new(action),
        enabled,
        keys: true,
    }
}

/// What the menu tells the window.
pub(crate) enum ContextMenuEvent {
    Run(usize),
    Dismissed,
}

/// The menu as a view.
pub struct ContextMenu {
    items: Vec<MenuItemShown>,
    /// Counted in the items; always an enabled one, if there is one.
    selected: Option<usize>,
    focus: FocusHandle,
    theme: MenuTheme,
}

impl ContextMenu {
    fn new(items: Vec<MenuItemShown>, theme: MenuTheme, cx: &mut Context<Self>) -> Self {
        let selected = items.iter().position(|item| item.enabled);
        Self {
            items,
            selected,
            focus: cx.focus_handle(),
            theme,
        }
    }

    /// The items from top to bottom.
    pub fn items(&self) -> &[MenuItemShown] {
        &self.items
    }

    /// The item Enter runs.
    pub fn selected(&self) -> Option<usize> {
        self.selected
    }

    /// Moves the selection `step` enabled items down, and stops at the
    /// ends.
    fn step(&mut self, step: isize, cx: &mut Context<Self>) {
        let Some(mut at) = self.selected else {
            return;
        };
        loop {
            let next = at as isize + step;
            if next < 0 || next as usize >= self.items.len() {
                break;
            }
            at = next as usize;
            if self.items[at].enabled {
                self.selected = Some(at);
                cx.notify();
                break;
            }
        }
    }

    fn run(&mut self, index: usize, cx: &mut Context<Self>) {
        if self.items.get(index).is_some_and(|item| item.enabled) {
            cx.emit(ContextMenuEvent::Run(index));
        }
    }
}

impl EventEmitter<ContextMenuEvent> for ContextMenu {}

impl Render for ContextMenu {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme.clone();
        let rows = self.items.iter().enumerate().map(|(index, item)| {
            let selected = self.selected == Some(index);
            let text = if item.enabled {
                theme.text
            } else {
                theme.disabled_text
            };
            div()
                .id(("context-menu-item", index))
                .debug_selector(move || format!("context-menu-item-{index}"))
                .flex()
                .justify_between()
                .gap_4()
                .px_3()
                .py(px(4.))
                .mx_1()
                .rounded(px(crate::theme::RADIUS_CONTROL))
                .text_color(hsla(text))
                .when(selected, |row| row.bg(hsla(theme.selected_background)))
                .when(item.enabled, |row| {
                    row.cursor_pointer()
                        .hover(|row| row.bg(hsla(theme.selected_background)))
                })
                .child(item.name.clone())
                .children(crate::design::kbd_group(
                    &item.keys,
                    theme.background,
                    theme.border,
                    if item.enabled {
                        theme.key_text
                    } else {
                        theme.disabled_text
                    },
                    theme.font_size,
                ))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.run(index, cx);
                    }),
                )
        });
        div()
            .id("context-menu")
            .debug_selector(|| "context-menu".into())
            .track_focus(&self.focus)
            .key_context(KEY_CONTEXT)
            .on_action(cx.listener(|this, _: &SelectNextItem, _, cx| this.step(1, cx)))
            .on_action(cx.listener(|this, _: &SelectPreviousItem, _, cx| this.step(-1, cx)))
            .on_action(cx.listener(|this, _: &RunItem, _, cx| {
                if let Some(index) = this.selected {
                    this.run(index, cx);
                }
            }))
            .on_action(cx.listener(|_, _: &Dismiss, _, cx| cx.emit(ContextMenuEvent::Dismissed)))
            .occlude()
            .relative()
            .overflow_hidden()
            .min_w(px(MIN_WIDTH))
            .flex()
            .flex_col()
            .py_1()
            .bg(hsla(theme.background))
            .border_1()
            .border_color(hsla(theme.border))
            .rounded(px(crate::theme::RADIUS_CARD))
            .shadow_lg()
            .font_family(theme.font_family.clone())
            .text_size(px(theme.font_size))
            .child(crate::design::top_highlight_bar(theme.background))
            .children(rows)
    }
}

/// The menu while it is open in a window.
pub(crate) struct OpenContextMenu {
    view: Entity<ContextMenu>,
    target: MenuTarget,
    actions: Vec<Box<dyn Action>>,
    position: Point<Pixels>,
    _subscriptions: [Subscription; 2],
}

impl BentermWindow {
    /// The right-click menu, while it is open.
    pub fn context_menu(&self) -> Option<Entity<ContextMenu>> {
        self.context_menu.as_ref().map(|menu| menu.view.clone())
    }

    /// The items for `target`. Nothing if the target is gone.
    fn menu_items(&self, target: MenuTarget, cx: &App) -> Option<Vec<MenuEntry>> {
        Some(match target {
            MenuTarget::Tab(workspace, _) => {
                let tabs = self.workspace_tabs(workspace)?.read(cx).tabs().len();
                vec![
                    entry("New Tab", NewTab, true),
                    // The tab closes with all its panes, and Cmd-W closes
                    // only the focused pane: no key does this.
                    MenuEntry {
                        keys: false,
                        ..entry("Close", Close, true)
                    },
                    entry("Close Other Tabs", CloseOtherTabs, tabs > 1),
                    entry("Rename", RenameTab, true),
                ]
            }
            MenuTarget::Workspace(_) => vec![
                entry("New Workspace", NewWorkspace, true),
                entry("Close", CloseWorkspace, true),
                entry("Rename", RenameWorkspace, true),
            ],
            MenuTarget::Session(pane) => {
                self.terminal_with_id(pane, cx)?;
                let open = if self.place_of(pane, cx).is_some() {
                    "Go to Pane"
                } else {
                    "Open as Tab"
                };
                vec![
                    entry(open, OpenPeekAsTab, true),
                    entry("Peek", PeekNextAgent, true),
                    entry("Close", CloseSession, true),
                ]
            }
            MenuTarget::Pane(pane) => {
                let selection = self.terminal_with_id(pane, cx)?.read(cx).has_selection();
                vec![
                    entry("Split Right", SplitRight, true),
                    entry("Split Down", SplitDown, true),
                    entry("Close", Close, true),
                    entry("Copy", Copy, selection),
                    entry("Paste", Paste, true),
                ]
            }
        })
    }

    /// Opens the menu of `target` at `position`, in window coordinates,
    /// and gives it the keyboard. A menu that was open closes.
    pub(crate) fn open_context_menu(
        &mut self,
        target: MenuTarget,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.context_menu = None;
        let Some(items) = self.menu_items(target, cx) else {
            return;
        };
        let shown = items
            .iter()
            .map(|item| MenuItemShown {
                name: item.name.to_owned(),
                keys: if item.keys {
                    keys_of(item.action.as_ref(), cx)
                } else {
                    String::new()
                },
                enabled: item.enabled,
            })
            .collect();
        let actions = items.into_iter().map(|item| item.action).collect();
        let theme = self.settings.theme().menu.clone();
        let view = cx.new(|cx| ContextMenu::new(shown, theme, cx));
        let events = cx.subscribe_in(&view, window, |this, _, event, window, cx| match event {
            ContextMenuEvent::Run(index) => this.run_menu_item(*index, window, cx),
            ContextMenuEvent::Dismissed => this.close_context_menu(window, cx),
        });
        let focus = view.read(cx).focus.clone();
        let menu = view.entity_id();
        // The keyboard went somewhere else; the menu goes away.
        let blur = cx.on_blur(&focus, window, move |this, _, cx| {
            if this
                .context_menu
                .as_ref()
                .is_some_and(|open| open.view.entity_id() == menu)
            {
                this.context_menu = None;
                cx.notify();
            }
        });
        self.context_menu = Some(OpenContextMenu {
            view,
            target,
            actions,
            position,
            _subscriptions: [events, blur],
        });
        window.focus(&focus, cx);
        cx.notify();
    }

    /// Closes the menu and gives the keyboard back to the peek, if it is
    /// open, or to the focused pane of the selected tab.
    fn close_context_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.context_menu.take().is_none() {
            return;
        }
        if !self.focus_peek(window, cx) {
            self.focus_selected(window, cx);
        }
        cx.notify();
    }

    /// Closes the menu and runs the operation of its item at `index` on
    /// what was clicked.
    fn run_menu_item(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(open) = &self.context_menu else {
            return;
        };
        let target = open.target;
        let Some(action) = open.actions.get(index).map(|action| action.boxed_clone()) else {
            return;
        };
        self.close_context_menu(window, cx);
        let action = action.as_any();
        match target {
            MenuTarget::Tab(workspace, tab) => {
                let Some(tabs) = self.workspace_tabs(workspace) else {
                    return;
                };
                tabs.update(cx, |tabs, cx| {
                    if action.is::<NewTab>() {
                        tabs.new_tab(window, cx);
                    } else if action.is::<Close>() {
                        tabs.close_tab(tab, window, cx);
                    } else if action.is::<CloseOtherTabs>() {
                        tabs.close_other_tabs(tab, window, cx);
                    } else if action.is::<RenameTab>() {
                        tabs.start_renaming(tab, window, cx);
                    }
                });
            }
            MenuTarget::Workspace(id) => {
                if action.is::<NewWorkspace>() {
                    self.new_workspace(window, cx);
                } else if action.is::<CloseWorkspace>() {
                    self.ask_to_close_workspace(id, window, cx);
                } else if action.is::<RenameWorkspace>() {
                    self.start_renaming(id, window, cx);
                }
            }
            MenuTarget::Session(pane) => {
                if action.is::<OpenPeekAsTab>() {
                    if !self.show_pane(pane, window, cx) && self.peek(pane, window, cx) {
                        self.open_peek_as_tab(window, cx);
                    }
                } else if action.is::<PeekNextAgent>() {
                    self.peek(pane, window, cx);
                } else if action.is::<CloseSession>() {
                    self.close_session(pane, window, cx);
                }
            }
            MenuTarget::Pane(pane) => {
                let Some(place) = self.locate(pane.get(), cx) else {
                    return;
                };
                if action.is::<SplitRight>() || action.is::<SplitDown>() {
                    let direction = if action.is::<SplitRight>() {
                        SplitDirection::Right
                    } else {
                        SplitDirection::Down
                    };
                    place
                        .grid
                        .update(cx, |grid, cx| grid.split_at(pane, direction, window, cx));
                } else if action.is::<Close>() {
                    place
                        .grid
                        .update(cx, |grid, cx| grid.close_pane(pane, window, cx));
                } else if let Some(view) = self.terminal_with_id(pane, cx) {
                    if action.is::<Copy>() {
                        view.update(cx, |view, cx| view.copy(cx));
                    } else if action.is::<Paste>() {
                        view.update(cx, |view, cx| view.paste(cx));
                    }
                }
            }
        }
        cx.notify();
    }

    /// The menu over everything, while it is open, with a layer under it
    /// that takes a press anywhere else and closes the menu.
    pub(crate) fn context_menu_overlay(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let open = self.context_menu.as_ref()?;
        let layer = div()
            .id("context-menu-layer")
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .occlude()
            .on_any_mouse_down(cx.listener(|this, _, window, cx| {
                cx.stop_propagation();
                this.close_context_menu(window, cx);
            }))
            .child(
                anchored()
                    .position(open.position)
                    .snap_to_window_with_margin(px(8.))
                    .child(open.view.clone()),
            );
        Some(deferred(layer).with_priority(1))
    }
}

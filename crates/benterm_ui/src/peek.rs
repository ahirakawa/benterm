//! The peek: the terminal of an agent laid over the pane area, without
//! leaving the tab on view. It is the agent's own terminal that is drawn
//! there, so what is typed goes to the agent, Escape too, and the screen
//! follows the agent's output as in a pane.
//!
//! A terminal is drawn in one place at a time. The peek draws the
//! terminal it shows, alone: one behind the window, one in a tab or a
//! workspace that is not on view, one under another view such as the
//! progress board, or a pane of the tab on view, which its grid then
//! leaves out until the peek closes.
//!
//! The peek is open for as long as its terminal has the keyboard. Cmd-.
//! closes it, as does its ✕, and the keyboard goes back to what the pane
//! area shows. When the keyboard goes anywhere else, by a click on a
//! pane or a key that shows another view, the peek goes away and the
//! keyboard stays where it went. The command palette is an exception:
//! it opens over the peek, and gives the keyboard back to it. The fields
//! and buttons of the session list are another, so that an agent can be
//! answered from the list while the peek is looked at: the keyboard goes
//! back to the peek when a reply is sent, and with Escape. A
//! command-click on a web address in the peek closes it too, and the page
//! opens beside the focused pane of the tab on view, where it is seen.
//!
//! The peek covers the pane area: its bar takes the place of the tabs
//! and the terminal the place of the panes. The terminal has the whole
//! of that room and is fitted to it, so the program of a pane of a split
//! is told a larger size while it is looked at, and its own again when
//! the peek closes. A terminal behind the window and the only pane of a
//! tab have that size already.
//!
//! What opens the peek is [`BentermWindow::peek`], with the id of the
//! terminal: a row of the session list, the progress board for an agent
//! that has no pane, and whatever view comes later.

use benterm_ipc::{AgentTarget, ShowAgent};
use benterm_layout::PaneId;
use gpui::{
    AnyElement, App, Context, Entity, FocusHandle, InteractiveElement, IntoElement, KeyBinding,
    MouseButton, ParentElement, Styled, Subscription, WeakEntity, Window, actions, div, px,
};

use crate::ipc::api_id;
use crate::session_rows::SessionState;
use crate::terminal_view::hsla;
use crate::{BentermWindow, MainView, PaneGrid, TerminalEvent, TerminalView, sidebar};

actions!(
    peek,
    [
        /// Opens the peek at the first agent that is not on view, a
        /// waiting one if there is one, or goes from the agent in the
        /// peek to the next.
        PeekNextAgent,
        ClosePeek,
        /// Goes to the pane of the agent in the peek, or opens a tab for
        /// an agent that has none.
        OpenPeekAsTab,
    ]
);

/// The key context of the peek, which its bindings are limited to.
const KEY_CONTEXT: &str = "Peek";

/// The height of the peek's bar, as the tab bar it covers.
const BAR_HEIGHT: f32 = 32.;
/// The space left and right of the bar's contents.
const BAR_PADDING: f32 = 14.;
/// The thickness of the line under the bar.
const INDICATOR: f32 = 2.;

/// Binds Cmd-P, which works anywhere in the window, and the keys of the
/// open peek. Escape is not among them: it is the agent's.
///
/// Cmd-W is left to the tabs. It closes nothing while the peek has the
/// keyboard, as the peek is not inside the tabs; were it the peek's key
/// too, a second press would close the pane under the peek.
pub fn bind_keys(cx: &mut App) {
    let context = Some(KEY_CONTEXT);
    cx.bind_keys([
        KeyBinding::new("cmd-p", PeekNextAgent, Some(sidebar::KEY_CONTEXT)),
        KeyBinding::new("cmd-.", ClosePeek, context),
        KeyBinding::new("cmd-o", OpenPeekAsTab, context),
    ]);
}

/// The peek while it is open.
pub(crate) struct OpenPeek {
    pane: PaneId,
    /// The grid or the agent keeps the terminal, not the peek: a pane
    /// that is closed under the peek ends its program as it always does.
    view: WeakEntity<TerminalView>,
    /// What gives the keyboard to the terminal.
    focus: FocusHandle,
    /// A place of the size the terminal had before the peek.
    restore: gpui::Size<gpui::Pixels>,
    /// The grid on view that has the terminal in a pane, which it does
    /// not draw while the peek does.
    hidden_in: Option<Entity<PaneGrid>>,
    _subscriptions: [Subscription; 2],
}

impl BentermWindow {
    /// The terminal the peek shows, while it is open.
    pub fn peeked(&self) -> Option<PaneId> {
        self.peek.as_ref().map(|peek| peek.pane)
    }

    /// Whether a frame of the pane area draws the terminal: it is in a
    /// pane of the tab on view.
    fn is_on_view(&self, pane: PaneId, cx: &App) -> bool {
        self.main_view() == MainView::Terminals && self.grid(cx).read(cx).terminal(pane).is_some()
    }

    /// Lays the terminal with the id `pane` over the pane area and gives
    /// it the keyboard. The workspace, the tab and the view of the pane
    /// area stay as they are. A pane that is on view is given the
    /// keyboard where it is instead. Returns whether there is such a
    /// terminal in the window.
    pub fn peek(&mut self, pane: PaneId, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(view) = self.terminal_with_id(pane, cx) else {
            return false;
        };
        // One peek at a time: the one before gives its pane back.
        self.end_peek(cx);
        if self.agents.terminal_behind(pane.get()).is_some() {
            // The theme may have changed while nothing drew the terminal,
            // as for a tab that is opened for it. A font of another size
            // gives the program a new size, as it does in a pane.
            let theme = self.settings.theme().clone();
            view.update(cx, |terminal, cx| terminal.set_theme(theme, cx));
        }
        let focus = view.read(cx).focus_handle().clone();
        let events = cx.subscribe_in(&view, window, move |this, _, event, window, cx| {
            match event {
                TerminalEvent::Exited if this.peeked() == Some(pane) => this.close_peek(window, cx),
                // The grid of a pane tells the window of its own clicks; a
                // terminal behind the window has no grid to.
                TerminalEvent::OpenUrl(url) if this.place_of(pane, cx).is_none() => {
                    this.open_link(pane, url, window, cx);
                }
                TerminalEvent::OpenPath(path) if this.place_of(pane, cx).is_none() => {
                    this.open_path_in_file_browser(pane, path.clone(), window, cx);
                }
                _ => {}
            }
        });
        let gone = cx.observe_release_in(&view, window, move |this, _, window, cx| {
            if this.peeked() == Some(pane) {
                this.close_peek(window, cx);
            }
        });
        // A pane on view is drawn by the peek while it is open, and not
        // by its grid.
        let restore = view.read(cx).fitted_place();
        let hidden_in = self.is_on_view(pane, cx).then(|| self.grid(cx).clone());
        if let Some(grid) = &hidden_in {
            grid.update(cx, |grid, cx| grid.set_hidden(Some(pane), cx));
        }
        self.peek = Some(OpenPeek {
            pane,
            restore,
            hidden_in,
            view: view.downgrade(),
            focus: focus.clone(),
            _subscriptions: [events, gone],
        });
        self.renaming = None;
        window.focus(&focus, cx);
        // The agent in the peek is looked at, and the pane under it is
        // not.
        self.look(window, cx);
        cx.notify();
        true
    }

    /// Takes the peek away: the pane it showed is drawn by its grid
    /// again, and a terminal that no grid on view draws is given back
    /// the size it had before the peek gave it the whole area.
    pub(crate) fn end_peek(&mut self, cx: &mut Context<Self>) -> Option<OpenPeek> {
        let peek = self.peek.take()?;
        if let Some(grid) = &peek.hidden_in {
            grid.update(cx, |grid, cx| grid.set_hidden(None, cx));
        }
        // The size it had: a grid on view fits it again with its next
        // frame, and nothing else would fit one that is not on view.
        if let Some(view) = peek.view.upgrade() {
            view.update(cx, |view, _| view.fit(peek.restore));
        }
        Some(peek)
    }

    /// Closes the peek. If its terminal has the keyboard, the keyboard
    /// goes back to what the pane area shows: the focused pane of the
    /// selected tab, or another view such as the progress board. If
    /// something else has it, such as the command palette over the peek
    /// when the peek's program exits, it stays there.
    pub fn close_peek(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(peek) = self.end_peek(cx) else {
            return;
        };
        if peek.focus.is_focused(window) {
            match self.main_view() {
                MainView::Terminals => self.focus_selected(window, cx),
                view => self.show_view(view, window, cx),
            }
        }
        self.look(window, cx);
        cx.notify();
    }

    /// Opens the page at `url`, which the user command-clicked in the
    /// terminal with the id `pane`, in a browser pane where it is seen.
    /// From a pane, that is at the right of the pane. From the peek, the
    /// pane of the terminal is not on view, if there is one, so the peek
    /// closes, the pane area shows the panes, and the page opens at the
    /// right of the focused pane of the tab on view. Either way, a browser
    /// pane already at the right takes the page.
    pub(crate) fn open_link(
        &mut self,
        pane: PaneId,
        url: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (grid, beside) = if self.peeked() == Some(pane) {
            self.end_peek(cx);
            // The keyboard was the peek's, so it goes to the panes.
            self.focus_selected(window, cx);
            self.look(window, cx);
            cx.notify();
            let grid = self.grid(cx);
            let focused = grid.read(cx).focused();
            (grid, focused)
        } else {
            let Some(grid) = self.grid_with(pane, cx) else {
                return;
            };
            (grid, pane)
        };
        grid.update(cx, |grid, cx| grid.open_link(beside, url, window, cx));
    }

    /// The grid whose panes have the terminal with the id `pane`.
    fn grid_with(&self, pane: PaneId, cx: &App) -> Option<Entity<PaneGrid>> {
        let (workspace, tab) = self.place_of(pane, cx)?;
        let tabs = self.workspace_tabs(workspace)?;
        tabs.read(cx).grid(tab).cloned()
    }

    /// Closes the peek and shows its terminal in a tab: the tab of its
    /// pane, or a new one for an agent that was behind the window. The
    /// terminal is the same one, with its program and its screen.
    pub fn open_peek_as_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(peek) = self.end_peek(cx) else {
            return;
        };
        let pane = peek.pane;
        let shown = self.show_pane(pane, window, cx) || {
            let request = ShowAgent {
                agent: AgentTarget::Pane(api_id(pane)),
                select: true,
            };
            self.show_agent(&request, window, cx).is_ok()
        };
        if !shown {
            // The terminal went away just now.
            self.focus_selected(window, cx);
        }
        self.look(window, cx);
        cx.notify();
    }

    /// Opens the peek at the first agent of the session list that is not
    /// on view, a waiting one if there is one. With the peek open, goes
    /// to the agent after the one it shows, the first after the last.
    pub fn peek_next_agent(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let rows = self.session_rows(cx);
        let rows: Vec<_> = rows
            .iter()
            .filter(|row| !self.is_on_view(row.pane, cx))
            .collect();
        let current = self
            .peeked()
            .and_then(|pane| rows.iter().position(|row| row.pane == pane));
        let next = match current {
            Some(current) => (current + 1) % rows.len(),
            None => rows
                .iter()
                .position(|row| matches!(row.state, SessionState::Waiting(_)))
                .unwrap_or(0),
        };
        if let Some(row) = rows.get(next) {
            self.peek(row.pane, window, cx);
        }
    }

    /// Gives the keyboard to the terminal in the peek. Returns whether
    /// the peek is open.
    pub(crate) fn focus_peek(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(peek) = &self.peek else {
            return false;
        };
        window.focus(&peek.focus, cx);
        true
    }

    /// Keeps the peek to what it is for, before a frame is drawn. A peek
    /// whose terminal is gone is closed. One whose terminal has come on
    /// view in a pane goes away, so that no frame draws a terminal
    /// twice; the pane keeps the keyboard if the terminal had it. One
    /// whose terminal has not got the keyboard goes away and leaves the
    /// keyboard where it went, unless that is the command palette, the
    /// context menu, or a field or a button of the session list, which
    /// give it back.
    ///
    /// This is asked of the window, not told by the terminal losing the
    /// focus: a window that goes behind another app keeps its peek, and
    /// the keyboard is still its terminal's when the window comes back.
    pub(crate) fn check_peek(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(peek) = &self.peek else {
            return;
        };
        let pane = peek.pane;
        let has_keyboard = peek.focus.is_focused(window);
        let there = self
            .terminal_with_id(pane, cx)
            .is_some_and(|view| view.downgrade() == peek.view);
        if !there {
            self.close_peek(window, cx);
        } else if peek.hidden_in.is_none() && self.is_on_view(pane, cx) {
            // The pane came on view under the peek, as when its tab is
            // selected: it is looked at there.
            self.end_peek(cx);
            if has_keyboard {
                // The grid has the pane focused that has the keyboard.
                self.show_pane(pane, window, cx);
            }
            self.look(window, cx);
            cx.notify();
        } else if self.palette.is_none()
            && self.context_menu.is_none()
            && !has_keyboard
            && !self.session_control_has_keyboard(window, cx)
        {
            self.end_peek(cx);
            self.look(window, cx);
            cx.notify();
        }
    }

    /// The actions of the peek, for the root of the window.
    pub(crate) fn peek_actions<E: InteractiveElement>(
        &self,
        element: E,
        cx: &mut Context<Self>,
    ) -> E {
        element.on_action(
            cx.listener(|this, _: &PeekNextAgent, window, cx| this.peek_next_agent(window, cx)),
        )
    }

    /// What the bar of the peek says: the kind of the agent, its name
    /// and what it is doing, as its row of the session list.
    fn peek_title(&self, pane: PaneId, cx: &App) -> String {
        let rows = self.session_rows(cx);
        let Some(row) = rows.iter().find(|row| row.pane == pane) else {
            return "Terminal".into();
        };
        let label = row.label();
        let mut title = row.kind().to_owned();
        if !label.is_empty() {
            title.push_str(" · ");
            title.push_str(&label);
        }
        format!("{title} · {}", row.state.label())
    }

    /// A button of the bar with an optional shortcut keycap.
    fn peek_button(
        &self,
        name: &'static str,
        label: &'static str,
        shortcut: &'static str,
        action: fn(&mut Self, &mut Window, &mut Context<Self>),
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let theme = &self.settings.theme().tabs;
        div()
            .id(name)
            .debug_selector(move || name.into())
            .flex()
            .flex_none()
            .items_center()
            .gap(px(5.))
            .px(px(8.))
            .py(px(2.))
            .rounded(px(crate::theme::RADIUS_CONTROL))
            .text_color(hsla(theme.text))
            .cursor_pointer()
            .hover(|button| {
                button
                    .bg(hsla(theme.button_hover_background))
                    .text_color(hsla(theme.active_text))
            })
            .child(label)
            .children(crate::design::kbd_group(
                shortcut,
                theme.active_background,
                theme.separator,
                theme.secondary_text,
                theme.font_size,
            ))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, window, cx| action(this, window, cx)),
            )
    }

    /// The peek over the pane area, while it is open: a bar where the
    /// tab bar is, and the terminal under it.
    pub(crate) fn peek_overlay(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let peek = self.peek.as_ref()?;
        let view = peek.view.upgrade()?;
        let pane = peek.pane;
        let theme = self.settings.theme();
        let tabs = &theme.tabs;
        let has_pane = self.place_of(pane, cx).is_some();
        let keep = if has_pane {
            "Go to pane"
        } else {
            "Open as tab"
        };
        let bar = div()
            .debug_selector(|| "peek-bar".into())
            .relative()
            .overflow_hidden()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(8.))
            .h(px(BAR_HEIGHT))
            .px(px(BAR_PADDING))
            .bg(hsla(tabs.active_background))
            .border_b(px(INDICATOR))
            .border_color(hsla(tabs.active_indicator))
            .font_family(tabs.font_family.clone())
            .text_size(px(tabs.font_size))
            .child(crate::design::top_highlight_bar(tabs.active_background))
            .child(crate::design::pill(
                "PEEK",
                tabs.active_indicator,
                tabs.font_size,
                true,
            ))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(hsla(tabs.active_text))
                    .child(self.peek_title(pane, cx)),
            )
            .child(self.peek_button("peek-keep", keep, "⌘O", Self::open_peek_as_tab, cx))
            .child(self.peek_button("peek-close", "Close", "⌘.", Self::close_peek, cx));
        // The terminal has the whole of the room under the bar, and is
        // fitted to it as a pane is to its place.
        let terminal = div().size_full().child(view);
        // What is under the peek takes no click and no scroll. The
        // panes take a press wherever it is in their place, so it is
        // stopped here, after the terminal of the peek had it.
        let cover = |part: gpui::Div| {
            part.occlude()
                .on_any_mouse_down(|_, _, cx| cx.stop_propagation())
                .bg(hsla(theme.terminal.background))
        };
        let overlay = div()
            .debug_selector(|| "peek".into())
            .key_context(KEY_CONTEXT)
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .flex()
            .flex_col()
            .child(crate::browser::covers())
            .on_action(cx.listener(|this, _: &ClosePeek, window, cx| this.close_peek(window, cx)))
            .on_action(
                cx.listener(|this, _: &OpenPeekAsTab, window, cx| {
                    this.open_peek_as_tab(window, cx)
                }),
            )
            // The bar of the peek takes the place of the tabs, and leaves
            // the end of the bar to the switch of the views.
            .child(
                div()
                    .flex()
                    .flex_none()
                    .child(cover(div().flex_1().min_w_0().overflow_hidden()).child(bar))
                    .child(div().flex_none().w(px(crate::main_view::VIEW_SWITCH_WIDTH))),
            )
            .child(
                cover(div().flex_1().min_h_0().min_w_0())
                    .overflow_hidden()
                    .child(terminal),
            );
        Some(overlay.into_any_element())
    }
}

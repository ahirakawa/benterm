//! The session list in the sidebar, under the workspaces: a row for
//! every agent of the window, in a pane or behind the window.
//!
//! Which terminals get a row, what a row says and the order of the rows
//! are decided in [`session_rows`](crate::session_rows), which knows
//! nothing about the screen. This file collects the entries from the
//! window, draws the rows, and does what a row is for: going to the
//! agent's pane, and answering the agent from its row, with the buttons
//! of a permission or with a field for a line of text. Which of them a
//! row has is [`SessionRow::controls`], and whether it shows its field
//! now is [`SessionRow::shows_field`].
//!
//! The rows are read anew every time the window is drawn, so nothing
//! here has to be kept in step with the panes. The window is drawn when
//! an agent's state changes ([`BentermWindow::session_news`]), and when
//! its tabs or agents change, as it always was.
//!
//! What builds on the list:
//!
//! - A row is a [`SessionRow`]. Its `pane` names the agent's terminal
//!   for as long as it runs, also when it moves from behind the window
//!   into a tab, so it is the key for whatever is kept per agent.
//! - [`BentermWindow::session_rows`] gives the rows as they are drawn.
//! - [`BentermWindow::select_session`] is where a row is chosen. It tells
//!   an agent in a pane from one behind the window.
//! - [`BentermWindow::session_row`] draws a row from its parts: the top
//!   line (dot, kind, place), the line under it, under that the
//!   line that opens the agent's checked report, if it has one, then the
//!   field and the buttons, and last the agent's plan, if its hooks told
//!   one. A mark goes into the top line, and another line under the row
//!   goes after the plan.

use std::collections::{HashMap, HashSet};

use benterm_agent::{AgentState, PlanStatus, WaitReason};
use benterm_ipc::ReportStatus;
use benterm_layout::PaneId;
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, App, AppContext, Context, Entity, FocusHandle, FontWeight, InteractiveElement,
    IntoElement, KeyBinding, MouseButton, ParentElement, StatefulInteractiveElement, Styled,
    Subscription, Window, actions, div, px,
};

use crate::name_editor::{NameEditor, NameEditorEvent};
use crate::session_rows::{
    AnswerKey, PlanView, SessionButton, SessionEntry, SessionPlace, SessionRow, SessionState,
    session_rows,
};
use crate::sidebar::ATTENTION_MARK;
use crate::terminal_view::hsla;
use crate::theme::SidebarTheme;
use crate::{AgentRow, BentermWindow, MainView, TerminalView};

actions!(
    session_list,
    [
        /// Gives the keyboard to the first field or button of the first
        /// waiting agent of the session list, or from one such agent to
        /// the next.
        FocusReply,
        NextControl,
        /// Moves the keyboard to the field or button before.
        PreviousControl,
        PressButton,
        /// Gives the keyboard back to what the pane area shows.
        LeaveControls,
        /// Closes the terminal of the agent in the peek, or of the row
        /// under the sidebar's cursor: its pane, or the terminal behind
        /// the window, which ends its program.
        CloseSession,
        /// Opens the field of the row under the sidebar's cursor and
        /// gives it the keyboard.
        ReplyAtCursor,
    ]
);

/// The key context of the fields and buttons of the list.
const CONTROL_CONTEXT: &str = "SessionControl";
/// The key context of a button. The buttons of a row are inside an
/// element of [`CONTROL_CONTEXT`], as an element has one context.
const BUTTON_CONTEXT: &str = "SessionButton";

/// Binds the keys that move between the fields and buttons of the list,
/// and press a button.
pub fn bind_keys(cx: &mut App) {
    let controls = Some(CONTROL_CONTEXT);
    let buttons = Some(BUTTON_CONTEXT);
    cx.bind_keys([
        KeyBinding::new("tab", NextControl, controls),
        KeyBinding::new("shift-tab", PreviousControl, controls),
        KeyBinding::new("enter", PressButton, buttons),
        KeyBinding::new("space", PressButton, buttons),
        KeyBinding::new("escape", LeaveControls, buttons),
        // Cmd-. closes the peek; with Option it closes the agent too.
        KeyBinding::new("cmd-alt-.", CloseSession, Some(crate::sidebar::KEY_CONTEXT)),
        KeyBinding::new("tab", ReplyAtCursor, Some(crate::sidebar::SIDEBAR_CONTEXT)),
    ]);
}

/// The space left and right of a row, as that of the workspaces.
const PADDING: f32 = 12.;
/// The side of the dot that says what the agent is doing.
const DOT: f32 = 8.;
/// The side of the icon that says the kind of an agent.
const ICON: f32 = 16.;
/// The side of the button that opens a peek.
const PEEK_BUTTON: f32 = 20.;
/// The space between the parts of a row's top line, and between its
/// buttons.
const GAP: f32 = 6.;
/// The width of the mark before an item of a plan.
const PLAN_MARK: f32 = 10.;
/// How far a child is moved in from its parent.
const INDENT: f32 = 12.;
/// The most steps a row is moved in, so that a row deep in a family
/// keeps room for what it says.
const MAX_INDENT_STEPS: usize = 4;

/// What a window keeps for its session list.
#[derive(Default)]
pub(crate) struct Sessions {
    /// The fields for a reply, one for every row that has one.
    replies: HashMap<PaneId, Reply>,
    /// What gives the keyboard to each button of the rows.
    buttons: HashMap<(PaneId, SessionButton), FocusHandle>,
    /// The agents that were sent an answer to a permission, and have not
    /// been heard of since. Their buttons send nothing more, so that a
    /// second press does not answer the next question.
    answered: HashSet<PaneId>,
    /// The row whose field was opened to answer it, which shows its
    /// field until it is sent, left, or its agent is heard of again.
    opened: Option<PaneId>,
}

struct Reply {
    editor: Entity<NameEditor>,
    _events: Subscription,
    _focus_out: Subscription,
}

fn state_color(theme: &SidebarTheme, state: SessionState) -> benterm_core::Rgb {
    match state {
        SessionState::Working => theme.session_working,
        SessionState::Waiting(_) => theme.session_waiting,
        SessionState::Done => theme.session_done,
        SessionState::Error => theme.session_error,
        SessionState::Starting | SessionState::Idle => theme.session_idle,
    }
}

impl BentermWindow {
    /// The rows of the session list, from top to bottom: the agents in
    /// the panes, in the order of the workspaces, their tabs and the
    /// panes of each tab, and then the agents behind the window, oldest
    /// first; the children of an agent right under it.
    pub fn session_rows(&self, cx: &App) -> Vec<SessionRow> {
        session_rows(self.session_entries(true, cx))
    }

    /// Every terminal of the window that runs, as the list is told it.
    /// Reading the directory of a program asks the system, so one who
    /// has no use for the directories leaves them out.
    pub(crate) fn session_entries(&self, directories: bool, cx: &App) -> Vec<SessionEntry> {
        let marked = self.marked_panes();
        let started: HashMap<PaneId, AgentRow> = self
            .agents(cx)
            .into_iter()
            .map(|agent| (agent.pane, agent))
            .collect();
        let entry = |pane: PaneId, view: &Entity<TerminalView>, place: SessionPlace| {
            let view = view.read(cx);
            if view.has_exited() {
                return None;
            }
            let status = view.agent_status();
            let started = started.get(&pane);
            // Only a terminal that may get a row is asked where it is.
            let may_get_a_row = status.agent.is_some() || status.state != AgentState::Unknown;
            let directory = (directories && may_get_a_row && started.is_none())
                .then(|| view.working_directory())
                .flatten();
            Some(SessionEntry {
                pane,
                parent: started.and_then(|agent| agent.parent),
                agent: status.agent,
                state: status.state,
                name: started.map(|agent| agent.name.clone()),
                directory,
                place,
                unseen: self.needs_attention(pane),
                marked: marked.contains(&pane),
                plan: view.agent_plan(),
                report_status: started.and_then(|agent| agent.report_status),
                checked: self.checked_report(pane).cloned(),
                stalled: self.is_stalled(pane),
                msg: view.agent_msg(),
            })
        };
        let mut entries = Vec::new();
        for workspace in self.workspaces.iter() {
            let tabs = workspace.content().tabs.read(cx);
            for tab in tabs.tabs() {
                let Some(grid) = tabs.grid(tab) else {
                    continue;
                };
                let grid = grid.read(cx);
                let place = SessionPlace::Pane {
                    workspace: workspace.id(),
                    tab,
                };
                entries.extend(
                    grid.panes()
                        .into_iter()
                        .filter_map(|pane| entry(pane, grid.terminal(pane)?, place)),
                );
            }
        }
        entries.extend(
            self.agents
                .terminals_behind()
                .iter()
                .filter_map(|(pane, view)| entry(*pane, view, SessionPlace::Background)),
        );
        entries
    }

    /// Chooses the row of the agent whose terminal has the id `pane`. An
    /// agent in a pane gets its workspace and its tab selected and the
    /// keyboard in its pane. An agent behind the window has no pane to
    /// go to: it is peeked at, and the workspace and the tab stay as
    /// they are.
    pub fn select_session(&mut self, pane: PaneId, window: &mut Window, cx: &mut Context<Self>) {
        // Where the terminal is now, which may not be where it was when
        // its row was drawn.
        let in_a_pane = self.place_of(pane, cx).is_some();
        if in_a_pane {
            self.show_pane(pane, window, cx);
        } else {
            self.peek(pane, window, cx);
        }
    }

    /// Sends `text` to the agent whose terminal has the id `pane`, as if
    /// it were pasted there with Enter after it, as `agent send` sends a
    /// request: an agent that asks for pastes to be marked, as Codex
    /// does, takes a carriage return in a paste for a line break and
    /// only Enter after the marks for sending. Returns whether it was
    /// sent: nothing is sent without text, to a terminal that is gone,
    /// or to an agent that takes no line of text now, as one at work or
    /// one that asks a permission or a question of choices.
    pub fn reply_to_session(&mut self, pane: PaneId, text: &str, cx: &mut Context<Self>) -> bool {
        if text.trim().is_empty() {
            return false;
        }
        let Some(view) = self.terminal_with_id(pane, cx) else {
            return false;
        };
        let view = view.read(cx);
        let takes_text = matches!(
            view.agent_status().state,
            AgentState::Waiting(WaitReason::Question) | AgentState::Done | AgentState::Error
        );
        takes_text && view.enter_message(text).is_ok()
    }

    /// Does what `button` of the row of `pane` is for: going to the pane,
    /// or answering the permission the agent asks with one key. Returns
    /// whether the agent was sent a key.
    ///
    /// An agent is answered once until it is heard of again, and only
    /// while it asks a permission.
    pub fn press_session_button(
        &mut self,
        pane: PaneId,
        button: SessionButton,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if button == SessionButton::Go {
            self.select_session(pane, window, cx);
            return false;
        }
        if self.sessions.answered.contains(&pane) {
            return false;
        }
        let row = session_rows(self.session_entries(false, cx))
            .into_iter()
            .find(|row| row.pane == pane);
        let Some(key) = row.and_then(|row| row.answer_key(button)) else {
            return false;
        };
        let Some(view) = self.terminal_with_id(pane, cx) else {
            return false;
        };
        let view = view.read(cx);
        let sent = match key {
            AnswerKey::Text(text) => view.send_text(text),
            AnswerKey::Escape => view.send_key("escape"),
        };
        if sent {
            self.sessions.answered.insert(pane);
            cx.notify();
        }
        sent
    }

    /// Whether the buttons that answer the agent of `pane` wait for news
    /// of it before they send anything again.
    pub fn session_answered(&self, pane: PaneId) -> bool {
        self.sessions.answered.contains(&pane)
    }

    /// What is typed in the field of the row of `pane`, if the row has a
    /// field.
    pub fn session_reply_text(&self, pane: PaneId, cx: &App) -> Option<String> {
        let reply = self.sessions.replies.get(&pane)?;
        Some(reply.editor.read(cx).text().to_owned())
    }

    /// The state of the agent of `pane` changed, or its terminal is
    /// gone: the rows are drawn anew, and the fields are those of the
    /// agents that wait now.
    ///
    /// What was typed for the agent goes with the state it was typed
    /// in. A pane tells only changes, so news of a pane whose agent
    /// waits now is another question than the one the field was for,
    /// also when the news took so long to get here that the agent has
    /// been through other states since.
    pub(crate) fn session_news(
        &mut self,
        pane: PaneId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.sessions.answered.remove(&pane);
        if self.sessions.opened == Some(pane) {
            self.sessions.opened = None;
        }
        if let Some(reply) = self.sessions.replies.remove(&pane)
            && reply.editor.read(cx).focus_handle().is_focused(window)
        {
            self.leave_reply(window, cx);
        }
        let rows = session_rows(self.session_entries(false, cx));
        self.sync_replies(&rows, window, cx);
        cx.notify();
    }

    /// Gives the keyboard, which was in a field or a button of the list,
    /// back to the terminal in the peek if the peek is open, or else to
    /// what the pane area shows: the focused pane of the selected tab,
    /// or another view such as the progress board.
    fn leave_reply(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sessions.opened = None;
        cx.notify();
        if self.focus_peek(window, cx) {
            return;
        }
        match self.main_view() {
            MainView::Terminals => self.focus_selected(window, cx),
            view => self.show_view(view, window, cx),
        }
    }

    /// Gives the keyboard to the first field or button of the first
    /// waiting agent of the list, or of the waiting agent after the one
    /// it is at, the first after the last. Without a waiting agent, the
    /// agents at their prompt are gone through the same way; without
    /// those either, nothing happens. A row that shows no field gets
    /// its field opened.
    pub fn reply_to_waiting_agent(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let rows = self.session_rows(cx);
        self.sync_replies(&rows, window, cx);
        let mut targets: Vec<&SessionRow> = rows
            .iter()
            .filter(|row| matches!(row.state, SessionState::Waiting(_)))
            .collect();
        if targets.is_empty() {
            targets = rows.iter().collect();
        }
        let controls: Vec<(PaneId, Vec<FocusHandle>)> = targets
            .iter()
            .map(|row| (row.pane, self.controls_of(row, true, cx)))
            .filter(|(_, controls)| !controls.is_empty())
            .collect();
        let current = controls
            .iter()
            .position(|(_, row)| row.iter().any(|control| control.is_focused(window)));
        let next = current.map_or(0, |current| (current + 1) % controls.len().max(1));
        if let Some((pane, row)) = controls.get(next) {
            self.open_reply(*pane, &row[0], window, cx);
        }
    }

    /// Gives the keyboard to `control`, the first field or button of
    /// the row of `pane`, and opens the row's field if it has one.
    fn open_reply(
        &mut self,
        pane: PaneId,
        control: &FocusHandle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.sessions.replies.contains_key(&pane) {
            self.sessions.opened = Some(pane);
        }
        window.focus(control, cx);
        cx.notify();
    }

    /// Opens the field of the row under the sidebar's cursor, if the row
    /// takes a reply, and gives it the keyboard.
    fn reply_at_cursor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pane) = self.sidebar.session_at_cursor() else {
            return;
        };
        let rows = self.session_rows(cx);
        self.sync_replies(&rows, window, cx);
        let Some(field) = self
            .sessions
            .replies
            .get(&pane)
            .map(|reply| reply.editor.read(cx).focus_handle().clone())
        else {
            return;
        };
        self.open_reply(pane, &field, window, cx);
    }

    /// Whether the user chose the row of `pane` to answer it: its field
    /// was opened, the sidebar's cursor is at it, or its agent is in the
    /// peek.
    fn reply_chosen(&self, pane: PaneId) -> bool {
        self.sessions.opened == Some(pane)
            || self.sidebar.at_session(pane)
            || self.peeked() == Some(pane)
    }

    /// Whether a field or a button of the list has the keyboard. The
    /// peek stays open while one has it.
    pub(crate) fn session_control_has_keyboard(&self, window: &Window, cx: &App) -> bool {
        let field = self
            .sessions
            .replies
            .values()
            .any(|reply| reply.editor.read(cx).focus_handle().is_focused(window));
        field
            || self
                .sessions
                .buttons
                .values()
                .any(|focus| focus.is_focused(window))
    }

    /// What gives the keyboard to each field and button of `row`, in
    /// their order; the field also when the row does not show it, if
    /// `hidden`.
    fn controls_of(&self, row: &SessionRow, hidden: bool, cx: &App) -> Vec<FocusHandle> {
        let shown = hidden || row.shows_field(self.reply_chosen(row.pane));
        let field = self
            .sessions
            .replies
            .get(&row.pane)
            .filter(|_| shown)
            .map(|reply| reply.editor.read(cx).focus_handle().clone());
        let buttons = row
            .controls()
            .buttons
            .iter()
            .filter_map(|button| self.sessions.buttons.get(&(row.pane, *button)).cloned());
        field.into_iter().chain(buttons).collect()
    }

    /// Moves the keyboard from the field or button it is in to the one
    /// `step` further down the list, round from the last to the first.
    fn move_between_controls(&mut self, step: isize, window: &mut Window, cx: &mut Context<Self>) {
        let rows = self.session_rows(cx);
        self.sync_replies(&rows, window, cx);
        let controls: Vec<FocusHandle> = rows
            .iter()
            .flat_map(|row| self.controls_of(row, false, cx))
            .collect();
        let Some(current) = controls
            .iter()
            .position(|control| control.is_focused(window))
        else {
            return;
        };
        let count = controls.len() as isize;
        let next = (current as isize + step).rem_euclid(count) as usize;
        window.focus(&controls[next], cx);
    }

    /// The actions of the list, for the root of the window.
    pub(crate) fn session_list_actions<E: InteractiveElement>(
        &self,
        element: E,
        cx: &mut Context<Self>,
    ) -> E {
        element
            .on_action(cx.listener(|this, _: &FocusReply, window, cx| {
                this.reply_to_waiting_agent(window, cx)
            }))
            .on_action(cx.listener(|this, _: &NextControl, window, cx| {
                this.move_between_controls(1, window, cx)
            }))
            .on_action(cx.listener(|this, _: &PreviousControl, window, cx| {
                this.move_between_controls(-1, window, cx)
            }))
            .on_action(
                cx.listener(|this, _: &LeaveControls, window, cx| this.leave_reply(window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &ReplyAtCursor, window, cx| this.reply_at_cursor(window, cx)),
            )
            .on_action(cx.listener(|this, _: &CloseSession, window, cx| {
                let target = this.peeked().or_else(|| this.sidebar.session_at_cursor());
                if let Some(pane) = target {
                    this.close_session(pane, window, cx);
                }
            }))
    }

    /// Closes the terminal of the agent whose terminal has the id
    /// `pane`: its pane, as Close closes one, or the terminal behind the
    /// window, which hangs its program up.
    pub fn close_session(&mut self, pane: PaneId, window: &mut Window, cx: &mut Context<Self>) {
        if self.peeked() == Some(pane) {
            self.close_peek(window, cx);
        }
        self.end_agent(pane, window, cx);
        cx.notify();
    }

    /// Gives the fields for a reply the look of `theme`.
    pub(crate) fn set_session_theme(&self, theme: &SidebarTheme, cx: &mut App) {
        for reply in self.sessions.replies.values() {
            let theme = theme.clone();
            reply
                .editor
                .update(cx, |editor, cx| editor.set_theme(theme, cx));
        }
    }

    /// Makes a field for every row that takes a reply and has none, and
    /// drops the fields of the others; a row draws its field only when
    /// it shows it. A field keeps what is typed in it until its agent is
    /// heard of again. An opened field closes when the keyboard leaves
    /// it. The keyboard goes back to
    /// what the pane area shows when the field it was in is dropped;
    /// that is done after this update, which may be the drawing of the
    /// window.
    fn sync_replies(&mut self, rows: &[SessionRow], window: &mut Window, cx: &mut Context<Self>) {
        let waiting: Vec<PaneId> = rows
            .iter()
            .filter(|row| row.takes_reply())
            .map(|row| row.pane)
            .collect();
        let buttons: HashSet<(PaneId, SessionButton)> = rows
            .iter()
            .flat_map(|row| {
                row.controls()
                    .buttons
                    .iter()
                    .map(|button| (row.pane, *button))
            })
            .collect();
        let mut had_keyboard = false;
        self.sessions.buttons.retain(|key, focus| {
            let keep = buttons.contains(key);
            had_keyboard |= !keep && focus.is_focused(window);
            keep
        });
        for key in buttons {
            self.sessions
                .buttons
                .entry(key)
                .or_insert_with(|| cx.focus_handle());
        }
        self.sessions.replies.retain(|pane, reply| {
            let keep = waiting.contains(pane);
            if !keep {
                had_keyboard |= reply.editor.read(cx).focus_handle().is_focused(window);
            }
            keep
        });
        for pane in waiting {
            if self.sessions.replies.contains_key(&pane) {
                continue;
            }
            let theme = self.settings.theme().sidebar.clone();
            let editor = cx.new(|cx| {
                NameEditor::new("", theme, cx)
                    .with_placeholder("Reply")
                    .without_underline()
            });
            let events = cx.subscribe_in(
                &editor,
                window,
                move |this, editor, event, window, cx| match event {
                    NameEditorEvent::Confirmed(text) => {
                        if this.reply_to_session(pane, text, cx) {
                            editor.update(cx, |editor, cx| editor.clear(cx));
                            this.leave_reply(window, cx);
                        }
                    }
                    NameEditorEvent::Cancelled => this.leave_reply(window, cx),
                },
            );
            let focus = editor.read(cx).focus_handle().clone();
            let focus_out = cx.on_focus_out(&focus, window, move |this, _, _, cx| {
                if this.sessions.opened == Some(pane) {
                    this.sessions.opened = None;
                    cx.notify();
                }
            });
            let reply = Reply {
                editor,
                _events: events,
                _focus_out: focus_out,
            };
            self.sessions.replies.insert(pane, reply);
        }
        if had_keyboard {
            cx.defer_in(window, |this, window, cx| this.leave_reply(window, cx));
        }
    }

    /// The lower part of the sidebar: a heading with the number of
    /// agents, and a row for each. Also returns whether there is a row,
    /// which decides how the sidebar shares its height.
    pub(crate) fn session_list(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (AnyElement, bool) {
        let rows = self.session_rows(cx);
        self.sync_replies(&rows, window, cx);
        let any = !rows.is_empty();
        let theme = &self.settings.theme().sidebar;
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
            .child("SESSIONS")
            .child(rows.len().to_string());
        let rows = rows
            .iter()
            .enumerate()
            .map(|(position, row)| self.session_row(position, row, cx))
            .collect::<Vec<_>>();
        let list = div()
            .flex()
            .flex_col()
            .min_h_0()
            // Without rows the list is its heading, and the workspaces
            // have the rest.
            .map(|list| if any { list.flex_1() } else { list.flex_none() })
            .mt(px(8.))
            .border_t_1()
            .border_color(crate::design::subtle_border(theme.border, theme.background))
            .child(header)
            .child(
                div()
                    .id("session-list")
                    .debug_selector(|| "session-list".into())
                    .flex_1()
                    .min_h_0()
                    .px(px(6.))
                    .pb(px(8.))
                    .flex()
                    .flex_col()
                    .gap(px(5.))
                    .overflow_y_scroll()
                    .children(rows),
            )
            .into_any_element();
        (list, any)
    }

    /// One row, as a session of the mock: a dot for the state, the kind
    /// of the agent and where it is on the top line, the state and the
    /// agent's name or directory under it, and under that the field and
    /// the buttons the row has.
    fn session_row(&self, position: usize, row: &SessionRow, cx: &mut Context<Self>) -> AnyElement {
        let theme = &self.settings.theme().sidebar;
        let tooltip_theme = self.settings.theme().menu.clone();
        let pane = row.pane;
        let dot_color = state_color(theme, row.state);
        let is_waiting = matches!(row.state, SessionState::Waiting(_));
        let dot = div()
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .size(px(DOT + 4.))
            .rounded_full()
            .when(is_waiting, |ring| {
                ring.border_1()
                    .border_color(crate::design::with_alpha(dot_color, 0.55))
                    .bg(crate::design::with_alpha(dot_color, 0.14))
            })
            .child(div().size(px(DOT)).rounded_full().bg(hsla(dot_color)));
        // The kind is an icon, which leaves the line to what tells the
        // agent from the others of its kind.
        let kind = div()
            .id(("session-icon", pane.get()))
            .debug_selector(move || format!("session-icon-{position}"))
            .flex_none()
            .tooltip(crate::tooltip::Tooltip::text(
                row.kind(),
                tooltip_theme.clone(),
            ))
            .child(crate::icons::agent(
                row.icon(),
                hsla(theme.section_header_text),
                ICON,
            ));
        let title = div()
            .debug_selector(move || format!("session-title-{position}"))
            .min_w_0()
            .truncate()
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(hsla(theme.item_text))
            .child(row.title());
        // Where the agent is: its workspace, or behind the window.
        let place = match row.place {
            SessionPlace::Pane { workspace, .. } => {
                div().truncate().child(self.drawn_label(workspace, cx))
            }
            SessionPlace::Background => div()
                .debug_selector(move || format!("session-background-{position}"))
                .flex_none()
                .child("background"),
        }
        .ml_auto()
        .min_w_0()
        .text_size(px(crate::theme::small_text(theme.font_size)))
        .text_color(hsla(theme.item_secondary_text));
        // The attention mark of the agent's pane, tab and workspace, also
        // for an agent behind the window, which has none of them.
        let mark = row.marked.then(|| {
            div()
                .debug_selector(move || format!("session-attention-{position}"))
                .flex_none()
                .size(px(ATTENTION_MARK))
                .rounded_full()
                .bg(hsla(theme.attention))
        });
        // An agent at work whose screen and files have not changed for
        // longer than the threshold. Its state stays what it is.
        let stalled = row.stalled.then(|| {
            crate::design::pill("Stalled", theme.session_waiting, theme.font_size, false)
                .debug_selector(move || format!("session-stalled-{position}"))
        });
        // What the agent's work came to, by its last report with a
        // status. Problems are in the colours of a waiting agent and of
        // one that failed, so they are seen before the report is read.
        let report = row.report_mark().map(|mark| {
            let color = match row.report_status() {
                Some(ReportStatus::Blocked) => theme.session_error,
                Some(ReportStatus::Issues) => theme.session_waiting,
                _ => theme.item_secondary_text,
            };
            crate::design::pill(mark.label, color, theme.font_size, mark.stands_out)
                .debug_selector(move || format!("session-report-{position}"))
        });
        // Looks at the agent over the tab on view, without going to it.
        let peek = div()
            .id(("session-peek", pane.get()))
            .debug_selector(move || format!("session-peek-{position}"))
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .size(px(PEEK_BUTTON))
            .rounded(px(crate::theme::RADIUS_CONTROL))
            .cursor_pointer()
            .hover(|peek| peek.bg(hsla(theme.item_active_background)))
            .tooltip(crate::tooltip::Tooltip::text("Peek", tooltip_theme))
            .child(crate::icons::eye(
                hsla(theme.section_header_text),
                PEEK_BUTTON - 6.,
            ))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, window, cx| {
                    // A press here is not one on the row.
                    cx.stop_propagation();
                    this.peek(pane, window, cx);
                }),
            );
        let top = div()
            .flex()
            .items_center()
            .gap(px(GAP))
            .child(dot)
            .child(kind)
            .child(title)
            .children(mark)
            .children(stalled)
            .children(report)
            .child(div().flex_1())
            .child(peek);
        let says = row.state.label().to_owned();
        // The state in words, and where the agent is at the end of the
        // line.
        let says = div()
            .flex()
            .items_center()
            .gap(px(GAP))
            .mt(px(2.))
            .text_size(px(crate::theme::small_text(theme.font_size)))
            .text_color(hsla(theme.item_secondary_text))
            .child(div().flex_none().child(says))
            .child(place);
        let msg = row.msg.as_ref().map(|msg| {
            div()
                .debug_selector(move || format!("session-msg-{position}"))
                .min_w_0()
                .truncate()
                .mt(px(2.))
                .text_size(px(crate::theme::small_text(theme.font_size)))
                .text_color(hsla(theme.item_text))
                .child(msg.clone())
        });
        let report_line = self.session_report_line(position, row, cx);
        let shown = row.shows_field(self.reply_chosen(pane));
        let reply = self
            .sessions
            .replies
            .get(&pane)
            .filter(|_| shown)
            .map(|reply| {
                let focus = reply.editor.read(cx).focus_handle().clone();
                div()
                    .debug_selector(move || format!("session-reply-{position}"))
                    .key_context(CONTROL_CONTEXT)
                    .flex()
                    .mt(px(6.))
                    .px(px(6.))
                    .py(px(3.))
                    .rounded(px(crate::theme::RADIUS_CONTROL))
                    .border_1()
                    .border_color(hsla(theme.border))
                    .bg(hsla(theme.field_background))
                    .text_size(px(crate::theme::small_text(theme.font_size)))
                    .cursor_text()
                    .child(reply.editor.clone())
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, window, cx| {
                            // A press in the field is not one on the row.
                            cx.stop_propagation();
                            this.open_reply(pane, &focus, window, cx);
                        }),
                    )
            });
        let buttons = row.controls().buttons;
        let buttons = (!buttons.is_empty()).then(|| {
            div()
                .key_context(CONTROL_CONTEXT)
                .flex()
                .gap(px(GAP))
                .mt(px(6.))
                .children(
                    buttons
                        .iter()
                        .filter_map(|button| self.session_button(position, row, *button, cx)),
                )
        });
        let plan = row
            .plan_view()
            .map(|view| self.session_plan(position, &view));
        let indent = INDENT * row.depth.min(MAX_INDENT_STEPS) as f32;
        let is_error = matches!(row.state, SessionState::Error);
        let is_idle = matches!(row.state, SessionState::Idle);
        let card_border = if is_waiting {
            crate::design::with_alpha(theme.session_waiting, 0.35)
        } else if is_error {
            crate::design::with_alpha(theme.session_error, 0.35)
        } else {
            crate::design::subtle_border(theme.border, theme.background)
        };
        div()
            .id(("session", pane.get()))
            .debug_selector(move || format!("session-{position}"))
            .flex()
            .flex_col()
            .ml(px(indent))
            .py(px(8.))
            .px(px(PADDING - 3.))
            .rounded(px(crate::theme::RADIUS_CARD))
            .border_1()
            .when(row.depth > 0, |el| el.border_l_2())
            .border_color(card_border)
            .when(is_waiting, |row| {
                row.bg(crate::design::with_alpha(theme.session_waiting, 0.09))
            })
            .when(is_error, |row| {
                row.bg(crate::design::with_alpha(theme.session_error, 0.09))
            })
            .when(is_idle && !self.sidebar.at_session(pane), |row| {
                row.opacity(0.65)
            })
            .cursor_pointer()
            .hover(|row| row.bg(hsla(theme.item_hover_background)).opacity(1.0))
            .when(self.sidebar.at_session(pane), |row| {
                row.bg(hsla(theme.item_hover_background))
            })
            .child(top)
            .child(says)
            .children(msg)
            .children(report_line)
            .children(reply)
            .children(buttons)
            .children(plan)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, window, cx| this.select_session(pane, window, cx)),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &gpui::MouseDownEvent, window, cx| {
                    let target = crate::context_menu::MenuTarget::Session(pane);
                    this.open_context_menu(target, event.position, window, cx);
                }),
            )
            .into_any_element()
    }

    /// The line under a row that asks the first question of the
    /// agent's checked report, or names the report if it asks none, and
    /// opens the report in its panel. A report that ran out of rounds
    /// has its label in the colour of a waiting agent, as it waits to
    /// be looked at.
    fn session_report_line(
        &self,
        position: usize,
        row: &SessionRow,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let line = row.report_line()?;
        let theme = &self.settings.theme().sidebar;
        let pane = row.pane;
        let (label, color) = if line.stopped {
            ("Not passed", theme.session_waiting)
        } else {
            ("Report", theme.item_active_indicator)
        };
        let more = (line.more > 0).then(|| {
            div()
                .flex_none()
                .text_color(hsla(theme.item_secondary_text))
                .child(format!("+{}", line.more))
        });
        let element = div()
            .id(("session-report-line", pane.get()))
            .debug_selector(move || format!("session-report-line-{position}"))
            .flex()
            .items_center()
            .gap(px(GAP))
            .mt(px(6.))
            .px(px(6.))
            .py(px(3.))
            .rounded(px(crate::theme::RADIUS_CONTROL))
            .border_1()
            .border_color(hsla(theme.border))
            .bg(hsla(theme.field_background))
            .text_size(px(crate::theme::small_text(theme.font_size)))
            .cursor_pointer()
            .hover(|line| line.border_color(hsla(theme.item_secondary_text)))
            .child(
                div()
                    .flex_none()
                    .text_color(hsla(color))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(label),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(hsla(if line.asks {
                        theme.item_text
                    } else {
                        theme.item_secondary_text
                    }))
                    .child(line.text),
            )
            .children(more)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, window, cx| {
                    // A press here is not one on the row.
                    cx.stop_propagation();
                    this.open_report(pane, window, cx);
                }),
            );
        Some(element.into_any_element())
    }

    /// The plan under a row: how much of it is done, and its items, each
    /// after a mark that says how far it is. What a long plan leaves out
    /// is counted above and below the items.
    fn session_plan(&self, position: usize, view: &PlanView) -> AnyElement {
        let theme = &self.settings.theme().sidebar;
        let more = |count: usize| {
            (count > 0).then(|| div().pl(px(PLAN_MARK + GAP)).child(format!("{count} more")))
        };
        let items = view.items.iter().enumerate().map(|(at, item)| {
            let at = view.before + at;
            // The colours of the states of the agents: done, at work,
            // and nothing yet.
            let (name, mark, color, text) = match item.status {
                PlanStatus::Completed => (
                    "done",
                    "\u{2713}",
                    theme.session_done,
                    theme.item_secondary_text,
                ),
                PlanStatus::InProgress => (
                    "in-progress",
                    "\u{25b8}",
                    theme.session_working,
                    theme.item_text,
                ),
                PlanStatus::Pending => (
                    "pending",
                    "\u{25cb}",
                    theme.session_idle,
                    theme.item_secondary_text,
                ),
            };
            div()
                .debug_selector(move || format!("session-plan-{position}-{name}-{at}"))
                .flex()
                .gap(px(GAP))
                .child(
                    div()
                        .flex_none()
                        .w(px(PLAN_MARK))
                        .text_color(hsla(color))
                        .child(mark),
                )
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_color(hsla(text))
                        .child(item.text.clone()),
                )
        });
        let ratio = if view.total == 0 {
            0.0
        } else {
            (view.done as f32 / view.total as f32).clamp(0.0, 1.0)
        };
        let meter = div()
            .w_full()
            .h(px(3.))
            .mt(px(2.))
            .mb(px(2.))
            .rounded_full()
            .overflow_hidden()
            .bg(hsla(theme.field_background))
            .child(
                div()
                    .h_full()
                    .w(gpui::relative(ratio))
                    .rounded_full()
                    .bg(hsla(theme.session_done)),
            );
        div()
            .debug_selector(move || format!("session-plan-{position}"))
            .flex()
            .flex_col()
            .gap(px(2.))
            .mt(px(6.))
            .text_size(px(crate::theme::small_text(theme.font_size)))
            .text_color(hsla(theme.item_secondary_text))
            .child(div().truncate().child(view.summary.clone()))
            .child(meter)
            .children(more(view.before))
            .children(items)
            .children(more(view.after))
            .into_any_element()
    }

    /// One button under a row. Allow comes first and is filled, as in
    /// the mock; a button that waits for news of its agent is dimmed.
    fn session_button(
        &self,
        position: usize,
        row: &SessionRow,
        button: SessionButton,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let theme = &self.settings.theme().sidebar;
        let pane = row.pane;
        let focus = self.sessions.buttons.get(&(pane, button))?.clone();
        let waits = button != SessionButton::Go && self.sessions.answered.contains(&pane);
        let name = match button {
            SessionButton::Allow => "allow",
            SessionButton::Deny => "deny",
            SessionButton::Go => "go",
        };
        // The accent and the text on it, as the workspace icons have.
        let (background, border, text) = match button {
            SessionButton::Allow => (
                theme.item_active_indicator,
                theme.item_active_indicator,
                theme.workspace_icon_text,
            ),
            SessionButton::Deny | SessionButton::Go => {
                (theme.field_background, theme.border, theme.item_text)
            }
        };
        let element = div()
            .id((name, pane.get()))
            .debug_selector(move || format!("session-{name}-{position}"))
            .key_context(BUTTON_CONTEXT)
            .track_focus(&focus)
            .px(px(8.))
            .py(px(2.))
            .rounded(px(crate::theme::RADIUS_CONTROL))
            .border_1()
            .border_color(hsla(border))
            .focus(|style| style.border_color(hsla(theme.item_text)))
            .bg(hsla(background))
            .text_color(hsla(text))
            .text_size(px(crate::theme::small_text(theme.font_size)))
            .when(waits, |element| element.opacity(0.5))
            .when(!waits, |element| element.cursor_pointer())
            .child(button.label())
            .on_action(cx.listener(move |this, _: &PressButton, window, cx| {
                this.press_session_button(pane, button, window, cx);
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, window, cx| {
                    // A press on the button is not one on the row.
                    cx.stop_propagation();
                    this.press_session_button(pane, button, window, cx);
                }),
            );
        Some(element.into_any_element())
    }
}

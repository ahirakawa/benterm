//! The panel of a checked report: the last report of an agent that its
//! Stop hook let through ([`crate::checked_reports`]), floating over the
//! window. The line under the agent's row of the session list opens it,
//! as does R at that row in the sidebar.
//!
//! The panel reads the report of its pane every time it is drawn, so a
//! new report of the pane takes the place of the old one at once, and the
//! panel closes when its pane does.
//!
//! The panel takes the keyboard while it is open. Escape closes it and
//! gives the keyboard back to where it was: the row in the sidebar it was
//! opened from, or whatever had it before. When the keyboard goes
//! anywhere else, by a click on a pane or a key that shows another view,
//! the panel goes away and the keyboard stays where it went, as the
//! command palette does.

use benterm_layout::PaneId;
use benterm_report::ReportStatus;
use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, App, Context, FocusHandle, FontWeight, Hsla, InteractiveElement, IntoElement,
    KeyBinding, MouseButton, ParentElement, StatefulInteractiveElement, Styled, Subscription,
    Window, actions, div, px,
};

use crate::checked_reports::{CheckedReport, shown_line, shown_text};
use crate::terminal_view::hsla;
use crate::theme::{RADIUS_CARD, RADIUS_CONTROL, Theme, small_text};
use crate::{BentermWindow, MainView, sidebar};

actions!(
    report_panel,
    [
        /// Opens the checked report of the agent at the sidebar's
        /// cursor, in the peek, or in the focused pane.
        OpenReport,
        CloseReport,
    ]
);

/// The key context of the panel, which its bindings are limited to.
const KEY_CONTEXT: &str = "ReportPanel";

const WIDTH: f32 = 560.;
/// How far below the top of the window the panel floats, and how far
/// above the bottom it ends at the most.
const TOP: f32 = 48.;
/// The space around the contents of the panel.
const PADDING: f32 = 16.;
/// The space between the parts of the panel.
const GAP: f32 = 14.;

/// Binds R at the sidebar's cursor, and Escape in the panel. R is not
/// bound in the fields of the sidebar, where it is a letter.
pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("r", OpenReport, Some(sidebar::CURSOR_CONTEXT)),
        KeyBinding::new("escape", CloseReport, Some(KEY_CONTEXT)),
    ]);
}

/// Where the keyboard goes back to when the panel closes.
enum Back {
    /// The row of this agent in the sidebar, under the cursor.
    Sidebar(PaneId),
    /// What had the keyboard, or the pane area if nothing had it.
    Focus(Option<FocusHandle>),
}

/// The panel while it is open.
pub(crate) struct OpenPanel {
    pane: PaneId,
    focus: FocusHandle,
    back: Back,
    /// Selected option label per decision index, initialized from `recommend`.
    choices: std::collections::HashMap<usize, String>,
    _blur: Subscription,
}

impl BentermWindow {
    /// The pane whose report the panel shows, while it is open.
    pub fn report_panel(&self) -> Option<PaneId> {
        self.report_panel.as_ref().map(|panel| panel.pane)
    }

    pub fn report_panel_title(&self, _: &App) -> Option<String> {
        let checked = self.checked_report(self.report_panel()?)?;
        Some(shown_line(&checked.report.title))
    }

    /// Opens the panel of the checked report of the agent in `pane`, and
    /// gives it the keyboard. Returns whether the agent has a report.
    pub fn open_report(
        &mut self,
        pane: PaneId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(checked) = self.checked_report(pane) else {
            return false;
        };
        let choices = checked
            .report
            .decisions
            .iter()
            .enumerate()
            .filter_map(|(idx, d)| {
                let rec = d.recommend.trim();
                (!rec.is_empty()).then(|| (idx, rec.to_owned()))
            })
            .collect();
        let back = match self.report_panel.take() {
            // Another report was open: the keyboard goes back to where
            // it was before that one.
            Some(open) => open.back,
            None => match self.sidebar.session_at_cursor() {
                Some(at) if self.sidebar_has_keyboard(window) => Back::Sidebar(at),
                _ => Back::Focus(window.focused(cx)),
            },
        };
        let focus = cx.focus_handle();
        let watched = focus.clone();
        let blur = cx.on_blur(&focus, window, move |this, _, cx| {
            // The keyboard went somewhere else; a panel closed already is
            // not this one.
            if this
                .report_panel
                .as_ref()
                .is_some_and(|open| open.focus == watched)
            {
                this.report_panel = None;
                cx.notify();
            }
        });
        self.report_panel = Some(OpenPanel {
            pane,
            focus: focus.clone(),
            back,
            choices,
            _blur: blur,
        });
        window.focus(&focus, cx);
        cx.notify();
        true
    }

    /// Closes the panel, and gives the keyboard back to where it was
    /// when the panel opened.
    pub fn close_report(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(open) = self.report_panel.take() else {
            return;
        };
        match open.back {
            Back::Sidebar(pane) => self.focus_sidebar_at_session(pane, window, cx),
            Back::Focus(Some(focus)) => window.focus(&focus, cx),
            Back::Focus(None) => self.focus_pane_area(window, cx),
        }
        cx.notify();
    }

    /// Closes the panel of a pane that closed. What had the keyboard
    /// before may have been the pane, so it goes to the pane area,
    /// unless the panel was opened from the sidebar.
    pub(crate) fn close_report_of_closed_pane(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(open) = self.report_panel.take() else {
            return;
        };
        let had_keyboard = open.focus.is_focused(window);
        match open.back {
            Back::Sidebar(_) if had_keyboard => self.focus_sidebar(window, cx),
            _ if had_keyboard => self.focus_pane_area(window, cx),
            _ => {}
        }
        cx.notify();
    }

    /// Gives the keyboard to the peek if it is open, or else to what
    /// the pane area shows.
    fn focus_pane_area(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.focus_peek(window, cx) {
            return;
        }
        match self.main_view() {
            MainView::Terminals => self.focus_selected(window, cx),
            view => self.show_view(view, window, cx),
        }
    }

    /// Opens the report of the agent at the sidebar's cursor, in the
    /// peek, or in the focused pane, the first of them there is.
    fn open_report_here(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let pane = self
            .sidebar
            .session_at_cursor()
            .or_else(|| self.peeked())
            .unwrap_or_else(|| self.grid(cx).read(cx).focused());
        self.open_report(pane, window, cx);
    }

    /// The actions of the panel, for the root of the window.
    pub(crate) fn report_panel_actions<E: InteractiveElement>(
        &self,
        element: E,
        cx: &mut Context<Self>,
    ) -> E {
        element.on_action(
            cx.listener(|this, _: &OpenReport, window, cx| this.open_report_here(window, cx)),
        )
    }

    fn select_report_choice(&mut self, decision: usize, label: String, cx: &mut Context<Self>) {
        if let Some(open) = &mut self.report_panel {
            open.choices.insert(decision, label);
            cx.notify();
        }
    }

    fn send_report_choice(
        &mut self,
        pane: PaneId,
        decision: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let choice = self
            .report_panel
            .as_ref()
            .and_then(|open| open.choices.get(&decision).cloned());
        let question = self
            .checked_report(pane)
            .and_then(|checked| checked.report.decisions.get(decision))
            .map(|d| shown_line(&d.question));
        let Some(choice) = choice else {
            return;
        };
        let message = match question {
            Some(q) if !q.is_empty() => format!("Regarding \"{q}\": proceed with {choice}."),
            _ => format!("Proceed with {choice}."),
        };
        if self.reply_to_session(pane, &message, cx) {
            self.close_report(window, cx);
        }
    }

    /// The panel floating over the top of the window, while it is open.
    pub(crate) fn report_panel_overlay(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let open = self.report_panel.as_ref()?;
        let checked = self.checked_report(open.pane)?;
        let theme = self.settings.theme();
        let room = (window.viewport_size().height - px(2. * TOP)).max(px(120.));
        let panel = div()
            .id("report-panel")
            .debug_selector(|| "report-panel".into())
            .key_context(KEY_CONTEXT)
            .track_focus(&open.focus)
            .on_action(
                cx.listener(|this, _: &CloseReport, window, cx| this.close_report(window, cx)),
            )
            .relative()
            .overflow_hidden()
            .w(px(WIDTH))
            .max_w_full()
            .max_h(room)
            .flex()
            .flex_col()
            .bg(hsla(theme.command_palette.background))
            .border_1()
            .border_color(hsla(theme.command_palette.border))
            .rounded(px(RADIUS_CARD))
            .shadow_lg()
            .font_family(theme.sidebar.font_family.clone())
            .text_size(px(theme.sidebar.font_size))
            .text_color(hsla(theme.sidebar.item_text))
            .child(crate::design::top_highlight_bar(
                theme.command_palette.background,
            ))
            .child(self.report_head(checked, theme, cx))
            .child(
                div()
                    .id("report-panel-body")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .gap(px(GAP))
                    .p(px(PADDING))
                    .children(self.report_body(open.pane, checked, &open.choices, theme, cx)),
            );
        Some(
            div()
                .absolute()
                .top_0()
                .bottom_0()
                // Over the pane area, so that the row it was opened from
                // is still seen.
                .left(px(self.sidebar_width()))
                .right_0()
                .child(crate::design::modal_scrim(theme.terminal.background))
                .child(
                    div()
                        .absolute()
                        .top(px(TOP))
                        .left_0()
                        .right_0()
                        .px(px(PADDING))
                        .flex()
                        .justify_center()
                        .child(crate::browser::covers())
                        .child(panel),
                )
                .into_any_element(),
        )
    }

    /// The top of the panel: the title, the conclusion and the goal, and
    /// the button that closes it.
    fn report_head(
        &self,
        checked: &CheckedReport,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = &theme.sidebar;
        let report = &checked.report;
        let close = div()
            .id("report-panel-close")
            .debug_selector(|| "report-panel-close".into())
            .flex_none()
            .px(px(6.))
            .py(px(2.))
            .rounded(px(RADIUS_CONTROL))
            .text_size(px(small_text(colors.font_size)))
            .text_color(hsla(colors.item_secondary_text))
            .cursor_pointer()
            .hover(|button| {
                button
                    .bg(hsla(colors.item_hover_background))
                    .text_color(hsla(colors.item_text))
            })
            .child("✕  esc")
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| this.close_report(window, cx)),
            );
        let status = report.status().map(|status| {
            let (label, color) = match status {
                ReportStatus::Pass => ("Pass", colors.session_done),
                ReportStatus::Issues => ("Issues", colors.session_waiting),
                ReportStatus::Blocked => ("Blocked", colors.session_error),
            };
            badge(label, hsla(color), small_text(colors.font_size))
        });
        let goal = (!report.goal.trim().is_empty()).then(|| {
            div()
                .min_w_0()
                .text_color(hsla(colors.item_secondary_text))
                .child(shown_line(&report.goal))
        });
        div()
            .flex()
            .flex_col()
            .flex_none()
            .gap(px(6.))
            .px(px(PADDING))
            .pt(px(PADDING - 4.))
            .pb(px(12.))
            .border_b_1()
            .border_color(hsla(theme.command_palette.border))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(
                        div()
                            .flex_none()
                            .text_size(px(small_text(colors.font_size)))
                            .text_color(hsla(colors.section_header_text))
                            .child("REPORT"),
                    )
                    .children(status)
                    .child(div().flex_1())
                    .child(close),
            )
            .child(
                div()
                    .debug_selector(|| "report-panel-title".into())
                    .text_size(px(colors.font_size + 2.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(shown_line(&report.title)),
            )
            .children(goal)
            .into_any_element()
    }
}

/// A small label with a border in `color`.
fn badge(label: &'static str, color: Hsla, size: f32) -> impl IntoElement {
    div()
        .flex_none()
        .px(px(5.))
        .rounded(px(RADIUS_CONTROL))
        .border_1()
        .border_color(color)
        .text_color(color)
        .text_size(px(size))
        .font_weight(FontWeight::SEMIBOLD)
        .child(label)
}

/// A part of the panel under a heading.
fn section(heading: &'static str, theme: &Theme) -> gpui::Div {
    let colors = &theme.sidebar;
    div().flex().flex_col().gap(px(6.)).child(
        div()
            .text_size(px(small_text(colors.font_size)))
            .text_color(hsla(colors.section_header_text))
            .child(heading),
    )
}

/// The lines of `items`, each after a dot, or "None" without items.
fn list(items: &[String], theme: &Theme) -> Vec<AnyElement> {
    let colors = &theme.sidebar;
    if items.is_empty() {
        let none = div()
            .text_color(hsla(colors.item_secondary_text))
            .child("None");
        return vec![none.into_any_element()];
    }
    items
        .iter()
        .map(|item| {
            div()
                .flex()
                .gap(px(8.))
                .child(
                    div()
                        .flex_none()
                        .text_color(hsla(colors.item_secondary_text))
                        .child("\u{2022}"),
                )
                .child(div().min_w_0().child(shown_text(item)))
                .into_any_element()
        })
        .collect()
}

impl BentermWindow {
    /// The parts of the panel under its top, in the order they are read:
    /// what is still wrong with a report that ran out of rounds, what the
    /// work came to, what the user is asked, what was and was not verified,
    /// and what was made.
    fn report_body(
        &self,
        pane: PaneId,
        checked: &CheckedReport,
        choices: &std::collections::HashMap<usize, String>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let colors = &theme.sidebar;
        let report = &checked.report;
        let mut parts = Vec::new();
        if !checked.passed() {
            let waiting = hsla(colors.session_waiting);
            let problems = div()
                .debug_selector(|| "report-panel-problems".into())
                .flex()
                .flex_col()
                .gap(px(6.))
                .p(px(10.))
                .rounded(px(RADIUS_CONTROL))
                .border_1()
                .border_color(waiting)
                .child(
                    div()
                        .text_color(waiting)
                        .font_weight(FontWeight::SEMIBOLD)
                        .child("Stopped with problems left"),
                )
                .child(
                    div()
                        .text_color(hsla(colors.item_secondary_text))
                        .child("The agent was sent back as often as allowed. The check found:"),
                )
                .children(list(&checked.problems, theme));
            parts.push(problems.into_any_element());
        }
        let summary = shown_text(&report.summary);
        if !summary.is_empty() {
            parts.push(
                section("CONCLUSION", theme)
                    .child(summary)
                    .into_any_element(),
            );
        }
        if !report.decisions.is_empty() {
            let decisions = report.decisions.iter().enumerate().map(|(at, decision)| {
                let selected = choices.get(&at).map(String::as_str);
                self.decision_box(pane, at, decision, selected, theme, cx)
            });
            parts.push(
                section("DECIDE", theme)
                    .gap(px(10.))
                    .children(decisions)
                    .into_any_element(),
            );
        }
        parts.push(
            section("VERIFIED", theme)
                .children(list(&report.verified, theme))
                .into_any_element(),
        );
        parts.push(
            section("NOT VERIFIED", theme)
                .children(list(&report.unverified, theme))
                .into_any_element(),
        );
        if !report.deliverables.is_empty() {
            let deliverables = report
                .deliverables
                .iter()
                .enumerate()
                .map(|(idx, deliverable)| {
                    let changes = match deliverable.changes.len() {
                        0 => None,
                        1 => Some("1 change".to_owned()),
                        count => Some(format!("{count} changes")),
                    };
                    let raw_path = deliverable.path.trim().to_owned();
                    let display_path = shown_line(&deliverable.path);
                    let clickable = !raw_path.is_empty();
                    div()
                        .id(("report-deliverable", idx))
                        .flex()
                        .flex_col()
                        .px(px(8.))
                        .py(px(5.))
                        .rounded(px(RADIUS_CONTROL))
                        .border_1()
                        .border_color(crate::design::subtle_border(
                            colors.border,
                            colors.field_background,
                        ))
                        .bg(hsla(colors.field_background))
                        .when(clickable, |row| {
                            row.cursor_pointer()
                                .hover(|r| r.border_color(hsla(colors.item_active_indicator)))
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |this, _, window, cx| {
                                        cx.stop_propagation();
                                        let path = std::path::PathBuf::from(&raw_path);
                                        this.close_report(window, cx);
                                        this.open_path_in_file_browser(pane, path, window, cx);
                                    }),
                                )
                        })
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(8.))
                                .child(div().flex_1().min_w_0().truncate().child(display_path))
                                .children(changes.map(|changes| {
                                    div()
                                        .flex_none()
                                        .text_color(hsla(colors.item_secondary_text))
                                        .child(changes)
                                }))
                                .when(clickable, |line| {
                                    line.child(
                                        div()
                                            .flex_none()
                                            .text_size(px(small_text(colors.font_size)))
                                            .text_color(hsla(colors.item_active_indicator))
                                            .child("Open →"),
                                    )
                                }),
                        )
                        .child(
                            div()
                                .text_color(hsla(colors.item_secondary_text))
                                .child(shown_text(&deliverable.summary)),
                        )
                });
            parts.push(
                section("DELIVERABLES", theme)
                    .gap(px(6.))
                    .children(deliverables)
                    .into_any_element(),
            );
        }
        parts
    }

    /// One thing the user is asked to decide: the question, the choices
    /// with what each leads to, the recommended one marked, and why.
    fn decision_box(
        &self,
        pane: PaneId,
        at: usize,
        decision: &benterm_report::Decision,
        selected: Option<&str>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = &theme.sidebar;
        let accent = colors.item_active_indicator;
        let recommend = decision.recommend.trim();
        let active_label = selected.unwrap_or(recommend);
        let options = decision
            .options
            .iter()
            .enumerate()
            .map(|(opt_idx, choice)| {
                let label = choice.label.trim().to_owned();
                let recommended = label == recommend;
                let chosen = label == active_label && !label.is_empty();
                let click_label = label.clone();
                div()
                    .id(("report-decision-opt", at * 100 + opt_idx))
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .px(px(10.))
                    .py(px(7.))
                    .rounded(px(RADIUS_CONTROL))
                    .border_1()
                    .border_color(if chosen {
                        hsla(accent)
                    } else {
                        crate::design::subtle_border(colors.border, colors.field_background)
                    })
                    .when(chosen, |card| {
                        card.bg(crate::design::with_alpha(accent, 0.11))
                    })
                    .cursor_pointer()
                    .hover(|card| card.border_color(hsla(accent)))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.select_report_choice(at, click_label.clone(), cx);
                        }),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .child(
                                div()
                                    .flex_none()
                                    .size(px(10.))
                                    .rounded_full()
                                    .border_1()
                                    .border_color(if chosen {
                                        hsla(accent)
                                    } else {
                                        hsla(colors.item_secondary_text)
                                    })
                                    .when(chosen, |dot| dot.bg(hsla(accent))),
                            )
                            .child(
                                div()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(shown_line(&choice.label)),
                            )
                            .when(recommended, |line| {
                                line.child(crate::design::pill(
                                    "Recommended",
                                    accent,
                                    colors.font_size,
                                    false,
                                ))
                            }),
                    )
                    .child(
                        div()
                            .pl(px(16.))
                            .text_color(hsla(colors.item_secondary_text))
                            .child(shown_text(&choice.effect)),
                    )
            });
        let reason = shown_text(&decision.reason);
        let reason = (!reason.is_empty()).then(|| {
            div()
                .text_color(hsla(colors.item_secondary_text))
                .child(format!("Why: {reason}"))
        });
        let send = (!active_label.is_empty()).then(|| {
            div()
                .id(("report-decision-send", at))
                .flex_none()
                .self_end()
                .px(px(10.))
                .py(px(3.))
                .rounded(px(RADIUS_CONTROL))
                .bg(hsla(accent))
                .text_color(hsla(colors.workspace_icon_text))
                .text_size(px(small_text(colors.font_size)))
                .font_weight(FontWeight::SEMIBOLD)
                .cursor_pointer()
                .child("Send choice to agent")
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.send_report_choice(pane, at, window, cx);
                    }),
                )
        });
        div()
            .debug_selector(move || format!("report-panel-decision-{at}"))
            .flex()
            .flex_col()
            .gap(px(8.))
            .p(px(10.))
            .rounded(px(RADIUS_CONTROL))
            .bg(hsla(colors.field_background))
            .border_1()
            .border_color(hsla(colors.border))
            .child(
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(shown_text(&decision.question)),
            )
            .children(options)
            .children(reason)
            .children(send)
            .into_any_element()
    }
}

//! The in-app notifications: what a pane's agent or program wants the user
//! to know, shown for a while in the bottom right corner of the window.
//!
//! They come from the same events as the attention marks, and are shown
//! when a macOS notification would be posted, whether or not macOS shows
//! that one. A click goes to the pane, or opens the peek of an agent
//! behind the window.

use std::collections::VecDeque;
use std::path::Path;
use std::time::{Duration, Instant};

use benterm_agent::WaitReason;
use benterm_layout::PaneId;
use gpui::{
    App, Context, FontWeight, InteractiveElement, IntoElement, MouseButton, ParentElement,
    StatefulInteractiveElement, Styled, Task, Window, div, px,
};

use crate::attention::{AttentionEvent, AttentionKind, PROGRAM_SOURCE, agent_name};
use crate::terminal_view::hsla;
use crate::{APP_NAME, BentermWindow};

/// How many are shown at once. An older one goes when a newer one would
/// be one too many.
pub const MAX_SHOWN: usize = 3;

/// How long one is shown.
pub const SHOWN_FOR: Duration = Duration::from_secs(6);

/// A non-zero exit within this time of starting is reported as a startup
/// failure.
pub const STARTUP_FAILURE_WINDOW: Duration = Duration::from_secs(2);

/// The exit code a shell uses when a command could not be found, which is
/// reported even after [`STARTUP_FAILURE_WINDOW`].
pub const COMMAND_NOT_FOUND_EXIT_CODE: i32 = 127;

/// The maximum number of non-empty lines from the end of a terminal's
/// output shown in a startup failure notification.
pub const FAILURE_OUTPUT_LINES: usize = 5;

/// What happened, which gives the notification the color of its dot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastKind {
    Done,
    Waiting,
    Osc,
    Error,
}

/// A pane's program exited with a non-zero code shortly after starting, or
/// with 127 (command not found).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartupFailure {
    pub exit_code: i32,
    /// The last non-empty lines of the terminal's output, oldest first,
    /// at most [`FAILURE_OUTPUT_LINES`].
    pub output: Vec<String>,
}

/// Whether a program that exited with `exit_code` after `elapsed` counts
/// as a startup failure, using `window` as the threshold.
pub fn is_startup_failure_within(
    exit_code: Option<i32>,
    elapsed: Duration,
    window: Duration,
) -> bool {
    match exit_code {
        Some(0) | None => false,
        Some(COMMAND_NOT_FOUND_EXIT_CODE) => true,
        Some(_) => elapsed <= window,
    }
}

/// Like [`is_startup_failure_within`] with the default
/// [`STARTUP_FAILURE_WINDOW`].
pub fn is_startup_failure(exit_code: Option<i32>, elapsed: Duration) -> bool {
    is_startup_failure_within(exit_code, elapsed, STARTUP_FAILURE_WINDOW)
}

/// Takes up to `max_lines` non-empty lines from the end of `text`,
/// preserving their order, with trailing whitespace trimmed.
pub fn last_non_empty_lines(text: &str, max_lines: usize) -> Vec<String> {
    if max_lines == 0 {
        return Vec::new();
    }
    let mut lines: Vec<String> = text
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.trim().is_empty())
        .rev()
        .take(max_lines)
        .map(str::to_owned)
        .collect();
    lines.reverse();
    lines
}

impl StartupFailure {
    /// Builds a startup failure from a program's exit code, runtime, and
    /// terminal output, or `None` when the exit does not count as a
    /// startup failure.
    pub fn from_exit(exit_code: Option<i32>, elapsed: Duration, output: &str) -> Option<Self> {
        Self::from_exit_within(exit_code, elapsed, STARTUP_FAILURE_WINDOW, output)
    }

    /// Like [`from_exit`](Self::from_exit) with a custom threshold.
    pub fn from_exit_within(
        exit_code: Option<i32>,
        elapsed: Duration,
        window: Duration,
        output: &str,
    ) -> Option<Self> {
        if !is_startup_failure_within(exit_code, elapsed, window) {
            return None;
        }
        Some(Self {
            exit_code: exit_code?,
            output: last_non_empty_lines(output, FAILURE_OUTPUT_LINES),
        })
    }
}

/// A notification as it is shown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Toast {
    /// The pane a click goes to.
    pub pane: PaneId,
    pub kind: ToastKind,
    /// Who it is from: the agent and the folder it works in.
    pub source: String,
    /// What happened.
    pub title: String,
    /// What a program said, under the title.
    pub body: Option<String>,
}

#[derive(Debug)]
struct ShownToast {
    toast: Toast,
    resumed_at: Instant,
    remaining: Duration,
}

impl ShownToast {
    fn remaining_at(&self, now: Instant, paused: bool) -> Duration {
        if paused {
            self.remaining
        } else {
            self.remaining
                .saturating_sub(now.saturating_duration_since(self.resumed_at))
        }
    }
}

/// The notifications being shown, from the oldest to the newest.
#[derive(Debug)]
pub struct Toasts {
    limit: usize,
    duration: Duration,
    paused: bool,
    shown: VecDeque<ShownToast>,
}

impl Default for Toasts {
    fn default() -> Self {
        Self::new(MAX_SHOWN, SHOWN_FOR)
    }
}

impl Toasts {
    /// Shows at most `limit` at once, each for `duration`.
    pub fn new(limit: usize, duration: Duration) -> Self {
        Self {
            limit,
            duration,
            paused: false,
            shown: VecDeque::new(),
        }
    }

    pub fn limit(&self) -> usize {
        self.limit
    }

    pub fn duration(&self) -> Duration {
        self.duration
    }

    pub fn is_paused(&self) -> bool {
        self.paused
    }

    /// Pauses expiration timers while the user hovers over the toast stack.
    pub fn pause(&mut self, now: Instant) {
        if self.paused {
            return;
        }
        for entry in &mut self.shown {
            entry.remaining = entry.remaining_at(now, false);
            entry.resumed_at = now;
        }
        self.paused = true;
    }

    /// Resumes expiration timers after the pointer leaves the toast stack.
    pub fn resume(&mut self, now: Instant) {
        if !self.paused {
            return;
        }
        for entry in &mut self.shown {
            entry.resumed_at = now;
        }
        self.paused = false;
    }

    /// Shows `toast` from `now`, in the place of an older one of its pane.
    pub fn push(&mut self, toast: Toast, now: Instant) {
        self.dismiss(toast.pane);
        self.shown.push_back(ShownToast {
            toast,
            resumed_at: now,
            remaining: self.duration,
        });
        while self.shown.len() > self.limit {
            self.shown.pop_front();
        }
    }

    /// The ones whose time is not up at `now`, from the oldest to the
    /// newest.
    pub fn shown(&self, now: Instant) -> Vec<&Toast> {
        let paused = self.paused;
        self.shown
            .iter()
            .filter(|entry| !entry.remaining_at(now, paused).is_zero())
            .map(|entry| &entry.toast)
            .collect()
    }

    /// Stops showing the one of `pane`. Returns whether there was one.
    pub fn dismiss(&mut self, pane: PaneId) -> bool {
        let before = self.shown.len();
        self.shown.retain(|entry| entry.toast.pane != pane);
        self.shown.len() != before
    }

    /// Drops the ones whose time is up at `now`. Returns whether any went.
    pub fn expire(&mut self, now: Instant) -> bool {
        if self.paused {
            return false;
        }
        let before = self.shown.len();
        self.shown
            .retain(|entry| !entry.remaining_at(now, false).is_zero());
        self.shown.len() != before
    }

    /// When the next one's time is up, or `None` while paused or empty.
    pub fn next_expiry(&self) -> Option<Instant> {
        if self.paused {
            return None;
        }
        self.shown
            .iter()
            .map(|entry| entry.resumed_at + entry.remaining)
            .min()
    }
}

const WIDTH: f32 = 320.;
/// How far the notifications are from the corner, and from each other.
const MARGIN: f32 = 12.;
const GAP: f32 = 8.;

/// What a window keeps for its notifications.
#[derive(Default)]
pub(crate) struct WindowToasts {
    toasts: Toasts,
    /// Takes away the next one whose time is up.
    _expiry: Option<Task<()>>,
}

impl BentermWindow {
    /// The notifications shown now, from the oldest to the newest, which
    /// is nearest the corner.
    pub fn shown_toasts(&self, cx: &App) -> Vec<Toast> {
        let now = cx.background_executor().now();
        let toasts = &self.attention.toasts.toasts;
        toasts.shown(now).into_iter().cloned().collect()
    }

    pub(crate) fn show_toast(&mut self, event: &AttentionEvent, cx: &mut Context<Self>) {
        let toast = self.toast_of(event, cx);
        let now = cx.background_executor().now();
        self.attention.toasts.toasts.push(toast, now);
        self.expire_toasts(cx);
    }

    /// Shows an in-app notification for a pane's program that failed on
    /// startup.
    pub(crate) fn show_startup_failure(
        &mut self,
        pane: PaneId,
        failure: &StartupFailure,
        cx: &mut Context<Self>,
    ) {
        let toast = self.failure_toast(pane, failure, cx);
        let now = cx.background_executor().now();
        self.attention.toasts.toasts.push(toast, now);
        self.expire_toasts(cx);
    }

    fn failure_toast(&self, pane: PaneId, failure: &StartupFailure, cx: &App) -> Toast {
        let title = if failure.exit_code == COMMAND_NOT_FOUND_EXIT_CODE {
            format!("Command not found (exit {})", failure.exit_code)
        } else {
            format!("Exited with code {}", failure.exit_code)
        };
        let body = (!failure.output.is_empty()).then(|| failure.output.join("\n"));
        Toast {
            pane,
            kind: ToastKind::Error,
            source: self.toast_source(pane, None, cx),
            title,
            body,
        }
    }

    /// Takes away the notifications whose time is up, and wakes up again
    /// when the next one's is.
    fn expire_toasts(&mut self, cx: &mut Context<Self>) {
        let now = cx.background_executor().now();
        let toasts = &mut self.attention.toasts;
        toasts.toasts.expire(now);
        cx.notify();
        toasts._expiry = toasts.toasts.next_expiry().map(|at| {
            let wait = at.saturating_duration_since(now);
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(wait).await;
                let _ = this.update(cx, |this, cx| this.expire_toasts(cx));
            })
        });
    }

    /// Goes to the pane of a clicked notification, or opens the peek of
    /// an agent behind the window. The notification goes away.
    fn open_toast(&mut self, pane: PaneId, window: &mut Window, cx: &mut Context<Self>) {
        self.attention.toasts.toasts.dismiss(pane);
        self.open_notified(pane, window, cx);
        self.expire_toasts(cx);
    }

    /// Dismisses a single toast without navigating to its pane.
    fn dismiss_toast(&mut self, pane: PaneId, cx: &mut Context<Self>) {
        if self.attention.toasts.toasts.dismiss(pane) {
            if self
                .attention
                .toasts
                .toasts
                .shown(cx.background_executor().now())
                .is_empty()
            {
                self.attention
                    .toasts
                    .toasts
                    .resume(cx.background_executor().now());
            }
            self.expire_toasts(cx);
        }
    }

    /// Pauses toast auto-expiry while hovered.
    fn pause_toasts(&mut self, cx: &mut Context<Self>) {
        let now = cx.background_executor().now();
        self.attention.toasts.toasts.pause(now);
        self.attention.toasts._expiry = None;
        cx.notify();
    }

    /// Resumes toast auto-expiry when the pointer leaves.
    fn resume_toasts(&mut self, cx: &mut Context<Self>) {
        let now = cx.background_executor().now();
        self.attention.toasts.toasts.resume(now);
        self.expire_toasts(cx);
    }

    /// What the notification of an event says.
    fn toast_of(&self, event: &AttentionEvent, cx: &App) -> Toast {
        let (kind, agent, title, body) = match &event.kind {
            AttentionKind::Done { agent } => (ToastKind::Done, *agent, "Finished".into(), None),
            AttentionKind::Waiting { agent, reason } => {
                let title = match reason {
                    WaitReason::Permission | WaitReason::Trust => "Asks for permission",
                    WaitReason::Question | WaitReason::Choice => "Has a question",
                };
                (ToastKind::Waiting, *agent, title.into(), None)
            }
            AttentionKind::Osc { title, body } => match title {
                Some(title) => (ToastKind::Osc, None, title.clone(), Some(body.clone())),
                None => (ToastKind::Osc, None, body.clone(), None),
            },
        };
        let source = match &event.kind {
            AttentionKind::Osc { .. } => self.toast_source(event.pane, Some(PROGRAM_SOURCE), cx),
            _ => self.toast_source(event.pane, agent.map(|agent| agent_name(Some(agent))), cx),
        };
        Toast {
            pane: event.pane,
            kind,
            source,
            title,
            body,
        }
    }

    /// Who a notification is from: `sender`, the agent if it is known or
    /// [`PROGRAM_SOURCE`] for a program that asked with OSC 9 or OSC 777,
    /// and the folder its pane works in. For an agent behind the window,
    /// which has no pane, the name it was started under comes between
    /// them.
    fn toast_source(&self, pane: PaneId, sender: Option<&str>, cx: &App) -> String {
        let place = match self.terminal_of(pane, cx) {
            Some(view) => folder_of(&view, cx).into_iter().collect(),
            None => self.behind_parts(pane, cx),
        };
        let parts: Vec<String> = sender.map(str::to_owned).into_iter().chain(place).collect();
        if parts.is_empty() {
            APP_NAME.into()
        } else {
            parts.join(" · ")
        }
    }

    /// For a terminal behind the window, the name its agent was started
    /// under and the folder it works in, those that are known. Nothing
    /// for any other terminal.
    pub(crate) fn behind_parts(&self, pane: PaneId, cx: &App) -> Vec<String> {
        let Some((_, view)) = self.agents.terminal_behind(pane.get()) else {
            return Vec::new();
        };
        let name = self.agents.name_of(pane.get()).map(str::to_owned);
        name.into_iter().chain(folder_of(&view, cx)).collect()
    }

    /// The terminal of a pane, in whichever workspace and tab it is.
    fn terminal_of(&self, pane: PaneId, cx: &App) -> Option<gpui::Entity<crate::TerminalView>> {
        self.workspaces.iter().find_map(|workspace| {
            let tabs = workspace.content().tabs.read(cx);
            tabs.tabs().into_iter().find_map(|tab| {
                let grid = tabs.grid(tab)?.read(cx);
                grid.terminal(pane).cloned()
            })
        })
    }

    /// The notifications in the bottom right corner, while there are any.
    pub(crate) fn toast_overlay(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let shown = self.shown_toasts(cx);
        if shown.is_empty() {
            return None;
        }
        let paused = self.attention.toasts.toasts.is_paused();
        let theme = &self.settings.theme().toast;
        let rows: Vec<_> = shown
            .into_iter()
            .enumerate()
            .map(|(index, toast)| {
                let dot = match toast.kind {
                    ToastKind::Done => theme.done,
                    ToastKind::Waiting => theme.waiting,
                    ToastKind::Osc => theme.message,
                    ToastKind::Error => theme.error,
                };
                let secondary = hsla(theme.secondary_text);
                let pane = toast.pane;
                let close = div()
                    .id(("toast-close", index))
                    .debug_selector(move || format!("toast-close-{index}"))
                    .flex_none()
                    .px(px(4.))
                    .rounded(px(crate::theme::RADIUS_CONTROL))
                    .text_size(px(crate::theme::small_text(theme.font_size)))
                    .text_color(secondary)
                    .hover(|btn| btn.bg(hsla(theme.border)).text_color(hsla(theme.text)))
                    .child("×")
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.dismiss_toast(pane, cx);
                        }),
                    );
                div()
                    .id(("toast", index))
                    .debug_selector(move || format!("toast-{index}"))
                    .occlude()
                    .relative()
                    .overflow_hidden()
                    .flex()
                    .items_start()
                    .gap_3()
                    .px_3()
                    .pt_2()
                    .pb(px(10.))
                    .bg(hsla(theme.background))
                    .border_1()
                    .border_color(hsla(theme.border))
                    .rounded(px(crate::theme::RADIUS_CARD))
                    .shadow_lg()
                    .cursor_pointer()
                    .hover(|style| style.bg(hsla(theme.hover_background)))
                    .on_click(
                        cx.listener(move |this, _, window, cx| this.open_toast(pane, window, cx)),
                    )
                    .child(crate::design::top_highlight_bar(theme.background))
                    .child(
                        div()
                            .flex_none()
                            .mt(px(theme.font_size * 0.5))
                            .size(px(8.))
                            .rounded_full()
                            .bg(hsla(dot)),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(6.))
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .truncate()
                                            .text_color(secondary)
                                            .text_size(px(crate::theme::small_text(
                                                theme.font_size,
                                            )))
                                            .child(toast.source),
                                    )
                                    .child(close),
                            )
                            .child(
                                div()
                                    .truncate()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(toast.title),
                            )
                            .children(toast.body.map(|body| {
                                div().flex().flex_col().text_color(secondary).children(
                                    body.lines()
                                        .map(|line| div().truncate().child(line.to_owned())),
                                )
                            })),
                    )
                    .child(div().absolute().left_0().right_0().bottom_0().h(px(2.)).bg(
                        crate::design::with_alpha(dot, if paused { 0.85 } else { 0.45 }),
                    ))
            })
            .collect();
        Some(
            div()
                .id("toast-stack")
                .absolute()
                .right(px(MARGIN))
                .bottom(px(MARGIN))
                .w(px(WIDTH))
                .flex()
                .flex_col()
                .gap(px(GAP))
                .font_family(theme.font_family.clone())
                .text_size(px(theme.font_size))
                .text_color(hsla(theme.text))
                .on_hover(cx.listener(|this, hovered, _, cx| {
                    if *hovered {
                        this.pause_toasts(cx);
                    } else {
                        this.resume_toasts(cx);
                    }
                }))
                .child(crate::browser::covers())
                .children(rows),
        )
    }
}

/// The name of the folder a terminal works in.
fn folder_of(view: &gpui::Entity<crate::TerminalView>, cx: &App) -> Option<String> {
    let directory = view.read(cx).working_directory()?;
    folder_name(&directory)
}

/// The name of a folder as the user knows it: its last part, or `~` for
/// the home folder.
fn folder_name(directory: &Path) -> Option<String> {
    let home = std::env::var_os("HOME");
    if home.is_some_and(|home| directory == Path::new(&home)) {
        return Some("~".into());
    }
    directory
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_toast() -> Toast {
        Toast {
            pane: benterm_layout::PaneTree::new(benterm_layout::Size {
                width: 10,
                height: 10,
            })
            .focused(),
            kind: ToastKind::Waiting,
            source: "Claude Code".into(),
            title: "Asks for permission".into(),
            body: None,
        }
    }

    #[test]
    fn pause_freezes_expiry_and_resume_continues_with_remaining_time() {
        let start = Instant::now();
        let mut toasts = Toasts::new(3, Duration::from_secs(6));
        toasts.push(sample_toast(), start);

        // Pause after 2s (4s remaining).
        let at_pause = start + Duration::from_secs(2);
        toasts.pause(at_pause);
        assert!(toasts.is_paused());
        assert!(toasts.next_expiry().is_none());

        // Even 10s later while paused, the toast does not expire.
        let still_paused = at_pause + Duration::from_secs(10);
        assert!(!toasts.expire(still_paused));
        assert_eq!(toasts.shown(still_paused).len(), 1);

        // Resume at still_paused: should still be alive 3s later, expired at 4s.
        toasts.resume(still_paused);
        assert!(!toasts.is_paused());
        assert_eq!(toasts.shown(still_paused + Duration::from_secs(3)).len(), 1);
        assert!(toasts.expire(still_paused + Duration::from_secs(4)));
        assert!(
            toasts
                .shown(still_paused + Duration::from_secs(4))
                .is_empty()
        );
    }
}

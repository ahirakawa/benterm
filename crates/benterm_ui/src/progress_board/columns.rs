//! The kanban of the board, under the timeline or the graph: a column
//! for each step of a task, as in the mock's progress board.
//!
//! Which column a task is in comes from [`crate::kanban`]. A card that
//! changed its column since the board last dealt the cards pulses
//! twice, as a band that begins does.

use std::f32::consts::TAU;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use benterm_github::{Github, PullRequestState};
use benterm_task::{PullRequestLink, TaskStore};
use gpui::prelude::FluentBuilder;

use gpui::{
    Animation, AnimationExt, AppContext, Context, ElementId, InteractiveElement, IntoElement,
    ParentElement, SharedString, StatefulInteractiveElement, Styled, div, px,
};

use super::{PULSE, ProgressBoard, now_ms};
use crate::kanban::{Card, Column, Dot};
use crate::terminal_view::hsla;

/// The least height of the kanban, so that a tall timeline leaves room
/// for a card or two.
const LEAST_HEIGHT: f32 = 140.;

impl ProgressBoard {
    pub(crate) fn sync_pull_requests(
        &mut self,
        repository: Option<PathBuf>,
        branches: Vec<(u64, String, String)>,
        store: Arc<TaskStore>,
        cx: &mut Context<Self>,
    ) {
        if self.repository != repository {
            self.repository = repository;
            self.last_pr_refresh = None;
            self.pr_refresh = None;
            cx.notify();
        }
        self.pr_store = Some(store);
        self.pr_branches = branches;
        self.refresh_pull_requests(cx);
    }

    pub(super) fn refresh_pull_requests(&mut self, cx: &mut Context<Self>) {
        let (Some(directory), Some(store)) = (self.repository.clone(), self.pr_store.clone())
        else {
            return;
        };
        if self.pr_branches.is_empty()
            || self.pr_refresh.is_some()
            || self
                .last_pr_refresh
                .is_some_and(|last| last.elapsed() < Duration::from_secs(60))
        {
            return;
        }
        let branches: Vec<_> = self
            .pr_branches
            .iter()
            .cloned()
            .filter_map(|(id, branch, base)| {
                let existing = self.pull_requests.get(&id);
                if existing.is_some_and(|pr| pr.state == PullRequestState::Merged) {
                    return None;
                }
                Some((id, branch, base))
            })
            .collect();
        if branches.is_empty() {
            return;
        }

        self.last_pr_refresh = Some(Instant::now());
        let fetch = cx.background_spawn(async move {
            let github = Github::default();
            let Ok(repository) = github.repository_from_dir(&directory) else {
                return Vec::new();
            };
            let Ok(stored) = store.tasks() else {
                return Vec::new();
            };
            let slug = repository.to_string();
            let mut updates = Vec::new();
            for (id, branch, base) in branches {
                let Some(task) = stored.iter().find(|task| task.id == id) else {
                    continue;
                };
                let number = task
                    .pull_request
                    .as_ref()
                    .filter(|link| link.repository == slug)
                    .map(|link| link.number);
                match github.task_pull_request(&repository, &branch, &base, number) {
                    Ok(Some(pr)) => {
                        if number.is_none() {
                            let link = PullRequestLink {
                                repository: slug.clone(),
                                number: pr.number,
                            };
                            if !store
                                .link_pull_request(id, link)
                                .is_ok_and(|saved| saved.number == pr.number)
                            {
                                continue;
                            }
                        }
                        updates.push((id, pr));
                    }
                    Ok(None) => {}
                    Err(_) => break,
                }
            }
            updates
        });
        self.pr_refresh = Some(cx.spawn(async move |this, cx| {
            let updates = fetch.await;
            let _ = this.update(cx, |this, cx| {
                this.pr_refresh = None;
                this.pull_requests.extend(updates);
                if this.deal() {
                    cx.notify();
                }
            });
        }));
    }

    /// The cards again, from the tasks and the records the board has.
    /// A card in another column than before is marked as moved now.
    /// Returns whether a card changed.
    pub(super) fn deal(&mut self) -> bool {
        // Not the present of the timeline, which stands still while the
        // graph is shown.
        let now = now_ms();
        let cards: Vec<Card> = self
            .tasks
            .iter()
            .map(|task| {
                let mut card = crate::kanban::card(task, &self.records, now);
                if let Some(pr) = self.pull_requests.get(&task.id) {
                    let status = match pr.state {
                        PullRequestState::Draft => "draft",
                        PullRequestState::Open => "open",
                        PullRequestState::Closed => "closed",
                        PullRequestState::Merged => {
                            card.column = Column::Done;
                            card.dot = Dot::Done;
                            "merged"
                        }
                    };
                    card.meta = format!("PR #{} {status} · {}", pr.number, card.meta);
                }
                card
            })
            .collect();
        for card in &cards {
            let before = self.cards.iter().find(|old| old.task == card.task);
            if before.is_some_and(|old| old.column != card.column) {
                self.moved.insert(card.task, now);
            }
        }
        self.moved
            .retain(|task, _| cards.iter().any(|card| card.task == *task));
        let changed = cards != self.cards;
        self.cards = cards;
        changed
    }

    fn dot_color(&self, dot: Dot) -> gpui::Hsla {
        let theme = &self.theme;
        hsla(match dot {
            Dot::Idle => theme.idle,
            Dot::Working => theme.working,
            Dot::Waiting => theme.waiting,
            Dot::Error => theme.error,
            Dot::Done => theme.done,
        })
    }

    fn card_element(&self, card: &Card, place: (usize, usize)) -> gpui::AnyElement {
        let theme = &self.theme;
        let selector = format!("kanban-card-{}-{}", place.0, place.1);
        let element = div()
            .id(ElementId::Name(format!("kanban-card-{}", card.task).into()))
            .debug_selector(move || selector)
            .flex_none()
            .px(px(8.))
            .py(px(7.))
            .mb(px(6.))
            .bg(hsla(theme.node_background))
            .border_1()
            .border_color(hsla(theme.border))
            .rounded(px(crate::theme::RADIUS_CARD))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .text_color(hsla(theme.title_text))
                    .child(
                        div()
                            .flex_none()
                            .size(px(8.))
                            .rounded_full()
                            .bg(self.dot_color(card.dot)),
                    )
                    .child(div().overflow_hidden().child(card.name.clone())),
            )
            .child(
                div()
                    .mt(px(4.))
                    .overflow_hidden()
                    .text_size(px(crate::theme::small_text(theme.font_size)))
                    .text_color(hsla(theme.axis_text))
                    .child(card.meta.clone()),
            );
        let moved = self.moved.get(&card.task).copied();
        match moved.filter(|_| self.shown_ms.is_some()) {
            Some(at_ms) => {
                let id = SharedString::from(format!("kanban-pulse-{}-{at_ms}", card.task));
                element
                    .with_animation(
                        ElementId::Name(id),
                        Animation::new(PULSE),
                        |element, delta| {
                            // Two pulses down and back, ending as drawn.
                            let dip = 0.5 - 0.5 * (TAU * 2. * delta).cos();
                            element.opacity(1. - 0.6 * dip)
                        },
                    )
                    .into_any_element()
            }
            None => element.into_any_element(),
        }
    }

    pub(super) fn kanban(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = &self.theme;
        let repository = self
            .repository
            .as_deref()
            .map(crate::sidebar_rows::repository_name);
        let columns = Column::ALL
            .into_iter()
            .enumerate()
            .filter(|(_, column)| self.show_completed || *column != Column::Done)
            .map(|(index, column)| {
                let cards: Vec<&Card> = self
                    .cards
                    .iter()
                    .filter(|card| card.column == column)
                    .collect();
                let selector = format!("kanban-column-{index}");
                div()
                    .id(ElementId::Name(selector.clone().into()))
                    .debug_selector(move || selector)
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .min_h_0()
                    .p(px(8.))
                    .bg(hsla(theme.background))
                    .rounded(px(crate::theme::RADIUS_CARD))
                    .child(
                        div()
                            .flex()
                            .justify_between()
                            .mb(px(6.))
                            .text_size(px(crate::theme::small_text(theme.font_size)))
                            .text_color(hsla(theme.label_text))
                            .child(column.label())
                            .child(cards.len().to_string()),
                    )
                    .child(
                        div()
                            .id(ElementId::Name(format!("kanban-cards-{index}").into()))
                            .flex()
                            .flex_col()
                            .min_h_0()
                            .overflow_y_scroll()
                            .children(cards.iter().enumerate().map(|(position, card)| {
                                self.card_element(card, (index, position))
                            })),
                    )
            });
        div()
            .id("kanban")
            .flex()
            .flex_1()
            .min_h(px(LEAST_HEIGHT))
            .flex_col()
            .gap(px(6.))
            .when_some(repository.clone(), |panel, name| {
                panel.child(
                    div()
                        .flex()
                        .justify_between()
                        .items_center()
                        .text_color(hsla(theme.label_text))
                        .child(name)
                        .child(
                            div()
                                .id("toggle-completed-tasks")
                                .debug_selector(|| "toggle-completed-tasks".into())
                                .self_end()
                                .cursor_pointer()
                                .text_color(hsla(theme.label_text))
                                .child(if self.show_completed {
                                    "Hide completed"
                                } else {
                                    "Show completed"
                                })
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.show_completed = !this.show_completed;
                                    cx.notify();
                                })),
                        ),
                )
            })
            .when(repository.is_some() || !self.cards.is_empty(), |panel| {
                panel.child(
                    div()
                        .flex()
                        .flex_1()
                        .min_h_0()
                        .gap(px(10.))
                        .children(columns),
                )
            })
            .when(repository.is_none() && self.cards.is_empty(), |panel| {
                panel.child(
                    div()
                        .text_color(hsla(theme.label_text))
                        .child("No repository for this workspace"),
                )
            })
    }
}

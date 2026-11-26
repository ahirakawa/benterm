//! The graph of the board: the agents as boxes in rows, a line from each
//! to the agents it started, and a dashed arrow for the messages one gave
//! another, as the mock's graph of issues is drawn.
//!
//! The boxes are elements, so that they take clicks. The lines are
//! painted behind them in one canvas. The graph is drawn again when the
//! window hands it another graph, when the user selects something, and
//! when an arrow has grown fainter with time; nothing moves in it
//! otherwise. A box whose agent changed state pulses twice, and a dot
//! runs once along the arrow of a message that was just sent.

use std::f32::consts::TAU;
use std::time::Duration;

use gpui::prelude::FluentBuilder;
use gpui::{
    Animation, AnimationExt, AnyElement, Bounds, Context, ElementId, Hsla, InteractiveElement,
    IntoElement, MouseButton, MouseDownEvent, ParentElement, PathBuilder, Pixels, Point,
    SharedString, StatefulInteractiveElement, Styled, canvas, div, fill, point, px, size,
};

use super::{BoardEvent, ProgressBoard};
use crate::graph::{
    Arrow, Curve, Message, Node, arrows, clock, layout, message_curve, prominence, shrink,
};
use crate::terminal_view::hsla;
use crate::timeline::BandState;

/// The box of an agent, and the space between two boxes.
const NODE_WIDTH: f32 = 160.;
const NODE_HEIGHT: f32 = 48.;
const GAP_X: f32 = 32.;
const GAP_Y: f32 = 72.;
/// How narrow the boxes of a graph that is wider than its place get,
/// as a share of their width, before the graph is scrolled instead.
const LEAST_SHRINK: f32 = 0.5;
/// The space around the graph.
const MARGIN: f32 = 28.;
/// The bar in the color of the state at the left of a box, as in the mock.
const STATE_BAR: f32 = 5.;
/// How strong the box of an ended agent is drawn.
const ENDED_OPACITY: f32 = 0.45;
/// The line from parent to child, as in the mock.
const LINK_WIDTH: f32 = 1.5;
/// The arrow of a message: of long ago, just sent, and selected.
const ARROW_WIDTH: f32 = 1.3;
const FRESH_ARROW_WIDTH: f32 = 2.5;
const SELECTED_ARROW_WIDTH: f32 = 2.5;
const DASHES: [f32; 2] = [5., 4.];
const HEAD: f32 = 9.;
/// How near a click has to be to an arrow to select it.
const REACH: f32 = 6.;
/// The mark in the middle of an arrow, which says how many messages went.
const MARK: f32 = 22.;
/// The space between the dots of the background, as in the mock.
const DOTS: f32 = 20.;
/// How long a box whose state changed pulses.
const PULSE: Duration = Duration::from_millis(1400);
/// How long the dot of a new message takes along its arrow.
const SPARK: Duration = Duration::from_millis(900);
const SPARK_SIZE: f32 = 8.;
/// How many of the latest messages of an arrow the detail lists.
const DETAIL_MESSAGES: usize = 3;
const DETAIL_WIDTH: f32 = 300.;

/// The graph as it is drawn: the places in pixels from its top left.
pub(super) struct Drawing {
    /// Each node with the middle of its box.
    nodes: Vec<(Node, (f32, f32))>,
    /// The lines from the bottom of a parent to the top of a child.
    links: Vec<((f32, f32), (f32, f32))>,
    arrows: Vec<(Arrow, Curve)>,
    size: (f32, f32),
    /// How wide a box is: less than usual in a graph that would not fit.
    node_width: f32,
}

impl ProgressBoard {
    pub fn graph_nodes(&self) -> Vec<Node> {
        let shown = crate::graph::shown(&self.nodes, self.hide_ended);
        shown.into_iter().cloned().collect()
    }

    /// The arrows of the messages between the nodes the graph shows.
    pub fn graph_arrows(&self) -> Vec<Arrow> {
        let nodes = self.graph_nodes();
        arrows(self.messages.iter(), |id| {
            nodes.iter().any(|node| node.id == id)
        })
    }

    /// Whether the ended agents are left out of the graph.
    pub fn hides_ended(&self) -> bool {
        self.hide_ended
    }

    /// The messages of the selected arrow, the latest first.
    pub fn selected_messages(&self) -> Vec<Message> {
        let Some((from, to)) = self.selected_arrow else {
            return Vec::new();
        };
        let of_arrow = |message: &&Message| message.from == from && message.to == to;
        self.messages
            .iter()
            .rev()
            .filter(of_arrow)
            .cloned()
            .collect()
    }

    /// The time of day the message was sent, on the clock of the user.
    pub fn clock_of(&self, message: &Message) -> String {
        clock(message.at_ms, self.utc_offset)
    }

    /// What the graph says about the selected arrow, line by line.
    pub fn detail_lines(&self) -> Vec<String> {
        let messages = self.selected_messages();
        let Some(latest) = messages.first() else {
            return Vec::new();
        };
        let name = |id: u64| {
            let node = self.nodes.iter().find(|node| node.id == id);
            node.map_or_else(|| format!("pane {id}"), |node| node.name.clone())
        };
        let count = match messages.len() {
            1 => "1 message".to_owned(),
            count => format!("{count} messages"),
        };
        let mut lines = vec![format!(
            "{} → {} · {count}",
            name(latest.from),
            name(latest.to)
        )];
        for message in messages.iter().take(DETAIL_MESSAGES) {
            lines.push(format!(
                "{} · {}",
                self.clock_of(message),
                message.kind.label()
            ));
            lines.push(message.line.clone());
        }
        lines
    }

    /// Where in the window the point a `share` of the way along the arrow
    /// from `from` to `to` was last drawn.
    pub fn point_on_arrow(&self, from: u64, to: u64, share: f32) -> Option<Point<Pixels>> {
        let origin = self.graph_origin.get()?;
        let drawing = self.drawing();
        let mut found = drawing.arrows.iter();
        let (_, curve) = found.find(|(arrow, _)| (arrow.from, arrow.to) == (from, to))?;
        let (x, y) = curve.at(share);
        Some(origin + point(px(x), px(y)))
    }

    /// Takes the graph of the window as it is now. A node whose state is
    /// another than before pulses. The window hands the board its graph
    /// whenever the graph changes; a picture of the board is made with a
    /// made-up one.
    pub fn take_graph(&mut self, nodes: Vec<Node>, messages: Vec<Message>, cx: &mut Context<Self>) {
        if nodes == self.nodes && messages == self.messages {
            return;
        }
        let now = super::now_ms();
        for node in &nodes {
            let before = self.nodes.iter().find(|before| before.id == node.id);
            if before.is_some_and(|b| (b.state, b.closed) != (node.state, node.closed)) {
                self.changed.insert(node.id, now);
            }
        }
        self.changed
            .retain(|id, _| nodes.iter().any(|node| node.id == *id));
        self.nodes = nodes;
        self.messages = messages;
        self.graph_now_ms = now;
        self.keep_selection();
        self.looks = self.looks();
        cx.notify();
    }

    /// Forgets a selection of what the graph shows no more.
    fn keep_selection(&mut self) {
        let nodes = self.graph_nodes();
        if let Some(selected) = self.selected_node
            && !nodes.iter().any(|node| node.id == selected)
        {
            self.selected_node = None;
        }
        if let Some(selected) = self.selected_arrow
            && !self
                .graph_arrows()
                .iter()
                .any(|arrow| (arrow.from, arrow.to) == selected)
        {
            self.selected_arrow = None;
        }
    }

    /// How much each arrow stands out now. The graph is drawn again with
    /// time only when this has changed.
    fn looks(&self) -> Vec<f32> {
        let arrows = self.graph_arrows();
        arrows.iter().map(|arrow| self.prominence(arrow)).collect()
    }

    fn prominence(&self, arrow: &Arrow) -> f32 {
        prominence(self.graph_now_ms.saturating_sub(arrow.latest.at_ms))
    }

    /// Whether something that happened at `at_ms` is shown with a
    /// movement that lasts `lasting`: it happened since the graph came
    /// into view, and the movement has not had the time to end. What is
    /// drawn later for another reason does not move again.
    fn is_news(&self, at_ms: u64, lasting: Duration) -> bool {
        let lasting = 2 * lasting.as_millis() as u64;
        self.graph_since_ms.is_some_and(|since| at_ms >= since)
            && super::now_ms().saturating_sub(at_ms) < lasting
    }

    /// Lets the arrows grow fainter as time passes, and asks the window
    /// for the graph again while it has closed nodes, which the window
    /// keeps only for a time.
    pub(super) fn age_graph(&mut self, cx: &mut Context<Self>) {
        if self.nodes.iter().any(|node| node.closed) {
            cx.emit(BoardEvent::Refresh);
        }
        self.graph_now_ms = super::now_ms();
        let looks = self.looks();
        if looks != self.looks {
            self.looks = looks;
            cx.notify();
        }
    }

    pub(super) fn toggle_ended(&mut self, cx: &mut Context<Self>) {
        self.hide_ended = !self.hide_ended;
        self.keep_selection();
        self.looks = self.looks();
        cx.notify();
    }

    /// Selects the node after or before the selected one, in the order
    /// the graph has them.
    pub(super) fn select_node_by(&mut self, step: isize, cx: &mut Context<Self>) {
        let nodes = self.graph_nodes();
        let ids: Vec<u64> = nodes.iter().map(|node| node.id).collect();
        if ids.is_empty() {
            return;
        }
        let position = self
            .selected_node
            .and_then(|selected| ids.iter().position(|id| *id == selected));
        let next = match position {
            None if step > 0 => 0,
            None => ids.len() - 1,
            Some(position) => position.saturating_add_signed(step).min(ids.len() - 1),
        };
        self.selected_node = Some(ids[next]);
        cx.notify();
    }

    /// Selects the arrow after the selected one, and the first after the
    /// last.
    pub(super) fn select_next_arrow(&mut self, cx: &mut Context<Self>) {
        let arrows = self.graph_arrows();
        let pairs: Vec<(u64, u64)> = arrows.iter().map(|a| (a.from, a.to)).collect();
        if pairs.is_empty() {
            return;
        }
        let position = self
            .selected_arrow
            .and_then(|selected| pairs.iter().position(|pair| *pair == selected));
        let next = position.map_or(0, |position| (position + 1) % pairs.len());
        self.selected_arrow = Some(pairs[next]);
        cx.notify();
    }

    pub(super) fn open_selected_node(&mut self, cx: &mut Context<Self>) {
        if let Some(id) = self.selected_node {
            self.open_node(id, cx);
        }
    }

    /// Goes to the terminal of a node, unless it has closed.
    fn open_node(&mut self, id: u64, cx: &mut Context<Self>) {
        let node = self.nodes.iter().find(|node| node.id == id);
        if node.is_some_and(|node| !node.closed) {
            self.selected_node = Some(id);
            cx.emit(BoardEvent::OpenTerminal(id));
            cx.notify();
        }
    }

    /// A click in the graph that no box and no mark took: on an arrow it
    /// selects the arrow, and anywhere else it selects nothing.
    fn click_graph(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let nearest = self.graph_origin.get().and_then(|origin| {
            let at = position - origin;
            let at = (f32::from(at.x), f32::from(at.y));
            let drawing = self.drawing();
            let reached = drawing.arrows.iter().filter_map(|(arrow, curve)| {
                let distance = curve.distance(at);
                (distance <= REACH).then_some((distance, (arrow.from, arrow.to)))
            });
            reached
                .min_by(|a, b| a.0.total_cmp(&b.0))
                .map(|(_, pair)| pair)
        });
        self.selected_arrow = nearest;
        self.selected_node = None;
        cx.notify();
    }

    /// Where everything of the graph is drawn.
    pub(super) fn drawing(&self) -> Drawing {
        let nodes = self.graph_nodes();
        let ids: Vec<(u64, Option<u64>)> = nodes.iter().map(|n| (n.id, n.parent)).collect();
        let layout = layout(&ids);
        let columns = layout.columns().max(1.);
        // A graph wider than its place has narrower boxes, closer
        // together, so that all of them are in view.
        let natural = columns * NODE_WIDTH + (columns - 1.) * GAP_X;
        let available = self.graph_width.get() - 2. * MARGIN;
        let shrunk = shrink(natural, available, LEAST_SHRINK);
        let (node_width, gap_x) = (NODE_WIDTH * shrunk, GAP_X * shrunk);
        let center = |id: u64| {
            let spot = layout.spot(id)?;
            Some((
                MARGIN + spot.column * (node_width + gap_x) + node_width / 2.,
                MARGIN + spot.row as f32 * (NODE_HEIGHT + GAP_Y) + NODE_HEIGHT / 2.,
            ))
        };
        let links = layout
            .links
            .iter()
            .filter_map(|(parent, child)| {
                let (parent, child) = (center(*parent)?, center(*child)?);
                Some((
                    (parent.0, parent.1 + NODE_HEIGHT / 2.),
                    (child.0, child.1 - NODE_HEIGHT / 2.),
                ))
            })
            .collect();
        let half = (node_width / 2., NODE_HEIGHT / 2.);
        let arrows = arrows(self.messages.iter(), |id| layout.spot(id).is_some())
            .into_iter()
            .filter_map(|arrow| {
                let curve = message_curve(center(arrow.from)?, center(arrow.to)?, half);
                Some((arrow, curve))
            })
            .collect();
        let rows = layout.rows().max(1) as f32;
        let size = (
            2. * MARGIN + columns * node_width + (columns - 1.) * gap_x,
            2. * MARGIN + rows * NODE_HEIGHT + (rows - 1.) * GAP_Y,
        );
        let nodes = nodes
            .into_iter()
            .filter_map(|node| {
                let center = center(node.id)?;
                Some((node, center))
            })
            .collect();
        Drawing {
            nodes,
            links,
            arrows,
            size,
            node_width,
        }
    }

    fn node_element(
        &self,
        node: &Node,
        center: (f32, f32),
        width: f32,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = &self.theme;
        let id = node.id;
        let color = hsla(node.color(theme));
        let selected = self.selected_node == Some(id);
        let opacity = if node.ended() { ENDED_OPACITY } else { 1. };
        let kind = match (node.agent, node.background) {
            (Some(agent), _) => agent,
            (None, true) => "agent",
            (None, false) => "pane",
        };
        let active = matches!(
            (node.closed, node.state),
            (false, Some(BandState::Working | BandState::Waiting))
        );
        let element = div()
            .id(ElementId::Name(format!("graph-node-{id}").into()))
            .debug_selector(move || format!("graph-node-{id}"))
            .absolute()
            .left(px(center.0 - width / 2.))
            .top(px(center.1 - NODE_HEIGHT / 2.))
            .w(px(width))
            .h(px(NODE_HEIGHT))
            .flex()
            .overflow_hidden()
            .rounded(px(crate::theme::RADIUS_PANEL))
            .bg(hsla(theme.node_background))
            .border_1()
            .border_color(color)
            .when(selected, |node| {
                node.border_2().border_color(hsla(theme.selected_border))
            })
            .when(!node.closed, |node| {
                node.cursor_pointer()
                    .hover(|node| node.border_color(hsla(theme.selected_border)))
            })
            .opacity(opacity)
            .child(div().flex_none().w(px(STATE_BAR)).h_full().bg(color))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .px(px(9.))
                    .flex()
                    .flex_col()
                    .justify_center()
                    .child(
                        div()
                            .text_size(px(crate::theme::small_text(theme.font_size)))
                            .text_color(hsla(theme.axis_text))
                            .truncate()
                            .child(format!("{kind} · {}", node.state_label())),
                    )
                    .child(
                        div()
                            .text_color(hsla(theme.title_text))
                            .truncate()
                            .child(node.name.clone()),
                    ),
            )
            .when(active, |node| {
                node.child(
                    div()
                        .flex_none()
                        .mt(px(9.))
                        .mr(px(9.))
                        .size(px(10.))
                        .rounded_full()
                        .bg(color),
                )
            })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.open_node(id, cx);
                }),
            );
        let changed = self.changed.get(&id).copied();
        match changed.filter(|at| self.is_news(*at, PULSE)) {
            Some(at) => {
                let id = SharedString::from(format!("graph-pulse-{id}-{at}"));
                element
                    .with_animation(
                        ElementId::Name(id),
                        Animation::new(PULSE),
                        move |element, delta| {
                            // Two pulses down and back, ending as drawn.
                            let dip = 0.5 - 0.5 * (TAU * 2. * delta).cos();
                            element.opacity(opacity * (1. - 0.65 * dip))
                        },
                    )
                    .into_any_element()
            }
            None => element.into_any_element(),
        }
    }

    /// The mark in the middle of an arrow, which takes the clicks for it
    /// and says how many messages went that way.
    fn mark_element(&self, arrow: &Arrow, curve: &Curve, cx: &mut Context<Self>) -> AnyElement {
        let theme = &self.theme;
        let pair = (arrow.from, arrow.to);
        let (from, to) = pair;
        let (x, y) = curve.at(0.5);
        let selected = self.selected_arrow == Some(pair);
        let edge = match selected {
            true => hsla(theme.selected_border),
            false => hsla(theme.message_line).opacity(self.prominence(arrow)),
        };
        div()
            .id(ElementId::Name(format!("graph-arrow-{from}-{to}").into()))
            .debug_selector(move || format!("graph-arrow-{from}-{to}"))
            .absolute()
            .left(px(x - MARK / 2.))
            .top(px(y - MARK / 2.))
            .size(px(MARK))
            .flex()
            .items_center()
            .justify_center()
            .rounded_full()
            .bg(hsla(theme.panel))
            .border_1()
            .border_color(edge)
            .cursor_pointer()
            .text_size(px(crate::theme::small_text(theme.font_size)))
            .text_color(hsla(theme.label_text))
            .hover(|mark| mark.border_color(hsla(theme.selected_border)))
            .child(arrow.count.to_string())
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.selected_arrow = Some(pair);
                    this.selected_node = None;
                    cx.notify();
                }),
            )
            .into_any_element()
    }

    /// The dot that runs along the arrow of a message sent while the
    /// board is shown, once.
    fn spark_element(&self, arrow: &Arrow, curve: Curve) -> Option<AnyElement> {
        if !self.is_news(arrow.latest.at_ms, SPARK) {
            return None;
        }
        let id = SharedString::from(format!("graph-spark-{}", arrow.latest.id));
        let dot = div()
            .debug_selector(|| "graph-spark".into())
            .absolute()
            .size(px(SPARK_SIZE))
            .rounded_full()
            .bg(hsla(self.theme.selected_border))
            .opacity(0.);
        let moving = dot.with_animation(
            ElementId::Name(id),
            Animation::new(SPARK),
            move |dot, delta| {
                let (x, y) = curve.at(delta);
                // It is gone once it has arrived.
                let opacity = if delta < 1. { 1. } else { 0. };
                dot.left(px(x - SPARK_SIZE / 2.))
                    .top(px(y - SPARK_SIZE / 2.))
                    .opacity(opacity)
            },
        );
        Some(moving.into_any_element())
    }

    /// What the canvas behind the boxes paints: the lines from parent to
    /// child, and the arrows.
    fn lines(
        &self,
        drawing: &Drawing,
    ) -> impl FnOnce(Bounds<Pixels>, &mut gpui::Window) + 'static + use<> {
        let theme = &self.theme;
        let link_color = hsla(theme.link_line);
        let links = drawing.links.clone();
        let arrows: Vec<(Curve, Hsla, f32)> = drawing
            .arrows
            .iter()
            .map(|(arrow, curve)| {
                let stands_out = self.prominence(arrow);
                if self.selected_arrow == Some((arrow.from, arrow.to)) {
                    let color = hsla(theme.selected_border);
                    return (*curve, color, SELECTED_ARROW_WIDTH);
                }
                // From the faintest, 0.25, to 1.
                let share = (stands_out - 0.25) / 0.75;
                let width = ARROW_WIDTH + (FRESH_ARROW_WIDTH - ARROW_WIDTH) * share;
                let color = hsla(theme.message_line).opacity(stands_out);
                (*curve, color, width)
            })
            .collect();
        move |bounds, window| {
            let at = |(x, y): (f32, f32)| bounds.origin + point(px(x), px(y));
            for (from, to) in links {
                // Down out of the parent and down into the child.
                let bend = (to.1 - from.1) / 2.;
                let mut line = PathBuilder::stroke(px(LINK_WIDTH));
                line.move_to(at(from));
                line.cubic_bezier_to(at(to), at((from.0, from.1 + bend)), at((to.0, to.1 - bend)));
                if let Ok(path) = line.build() {
                    window.paint_path(path, link_color);
                }
            }
            for (curve, color, width) in arrows {
                let mut line = PathBuilder::stroke(px(width)).dash_array(&DASHES.map(px));
                line.move_to(at(curve.start));
                line.curve_to(at(curve.end), at(curve.control));
                if let Ok(path) = line.build() {
                    window.paint_path(path, color);
                }
                let [left, right] = curve.head(HEAD);
                let mut head = PathBuilder::fill();
                head.move_to(at(curve.end));
                head.line_to(at(left));
                head.line_to(at(right));
                head.close();
                if let Ok(path) = head.build() {
                    window.paint_path(path, color);
                }
            }
        }
    }

    /// What is written about the selected arrow, over a corner of the
    /// graph.
    fn detail(&self) -> Option<AnyElement> {
        let theme = &self.theme;
        let mut lines = self.detail_lines().into_iter();
        let heading = lines.next()?;
        let rest: Vec<String> = lines.collect();
        let messages = rest.chunks(2).map(|message| {
            let (when, line) = (message[0].clone(), message.get(1).cloned());
            div()
                .flex()
                .flex_col()
                .gap(px(2.))
                .pt(px(6.))
                .border_t_1()
                .border_color(hsla(theme.border))
                .child(
                    div()
                        .text_size(px(crate::theme::small_text(theme.font_size)))
                        .text_color(hsla(theme.label_text))
                        .child(when),
                )
                .child(div().text_color(hsla(theme.title_text)).children(line))
        });
        Some(
            div()
                .id("graph-detail")
                .debug_selector(|| "graph-detail".into())
                .absolute()
                .bottom(px(10.))
                .left(px(10.))
                .w(px(DETAIL_WIDTH))
                .flex()
                .flex_col()
                .gap(px(6.))
                .p(px(10.))
                .rounded(px(crate::theme::RADIUS_PANEL))
                .bg(hsla(theme.node_background))
                .border_1()
                .border_color(hsla(theme.selected_border))
                .occlude()
                .child(
                    div()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(hsla(theme.title_text))
                        .child(heading),
                )
                .children(messages)
                .into_any_element(),
        )
    }

    /// The button that leaves the ended agents out, or shows them again.
    pub(super) fn ended_button(&self, cx: &Context<Self>) -> impl IntoElement + use<> {
        let theme = &self.theme;
        div()
            .id("graph-hide-ended")
            .debug_selector(|| "graph-hide-ended".into())
            .px(px(6.))
            .rounded(px(crate::theme::RADIUS_CONTROL))
            .cursor_pointer()
            .text_color(hsla(theme.label_text))
            .hover(|button| button.bg(hsla(theme.row_hover_background)))
            .when(self.hide_ended, |button| {
                button
                    .bg(hsla(theme.range_active_background))
                    .text_color(hsla(theme.title_text))
            })
            .child("Hide ended")
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| this.toggle_ended(cx)),
            )
    }

    /// The graph, in the place of the rows of the timeline.
    pub(super) fn graph(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = &self.theme;
        let drawing = self.drawing();
        let (width, height) = drawing.size;
        let dot_color = hsla(theme.grid_line);
        let dots = canvas(
            |_, _, _| (),
            move |bounds, (), window, _| {
                let across = (f32::from(bounds.size.width) / DOTS) as usize;
                let down = (f32::from(bounds.size.height) / DOTS) as usize;
                for row in 0..=down {
                    for column in 0..=across {
                        let at = point(px(column as f32 * DOTS), px(row as f32 * DOTS));
                        let dot = Bounds::new(bounds.origin + at, size(px(1.), px(1.)));
                        window.paint_quad(fill(dot, dot_color));
                    }
                }
            },
        )
        .absolute()
        .size_full();
        // How wide the place is, which the boxes of a wide graph are
        // fitted to: when it has changed, the graph is drawn once more.
        let known = self.graph_width.clone();
        let board = cx.entity_id();
        let measure = canvas(
            move |bounds, _, cx| {
                let width = f32::from(bounds.size.width);
                if (width - known.get()).abs() > 0.5 {
                    known.set(width);
                    cx.defer(move |cx| cx.notify(board));
                }
            },
            |_, (), _, _| (),
        )
        .absolute()
        .size_full();
        let origin = self.graph_origin.clone();
        let paint = self.lines(&drawing);
        let lines = canvas(
            move |bounds, _, _| origin.set(Some(bounds.origin)),
            move |bounds, (), window, _| paint(bounds, window),
        )
        .absolute()
        .size_full();
        let empty = drawing.nodes.is_empty().then(|| {
            div()
                .absolute()
                .top(px(12.))
                .left(px(12.))
                .text_color(hsla(theme.label_text))
                .child("No agent runs in this window.")
        });
        let nodes: Vec<AnyElement> = drawing
            .nodes
            .iter()
            .map(|(node, center)| self.node_element(node, *center, drawing.node_width, cx))
            .collect();
        let marks: Vec<AnyElement> = drawing
            .arrows
            .iter()
            .map(|(arrow, curve)| self.mark_element(arrow, curve, cx))
            .collect();
        let sparks: Vec<AnyElement> = drawing
            .arrows
            .iter()
            .filter_map(|(arrow, curve)| self.spark_element(arrow, *curve))
            .collect();
        // The boxes and the lines, in the middle if there is room.
        let layer = div()
            .relative()
            .flex_none()
            .w(px(width))
            .h(px(height))
            .child(lines)
            .children(nodes)
            .children(marks)
            .children(sparks);
        div()
            .relative()
            .flex_1()
            .min_h_0()
            .rounded(px(crate::theme::RADIUS_CARD))
            .bg(hsla(theme.background))
            .overflow_hidden()
            .child(measure)
            .child(
                div()
                    .id("board-graph")
                    .debug_selector(|| "board-graph".into())
                    .size_full()
                    .overflow_scroll()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, event: &MouseDownEvent, _, cx| {
                            this.click_graph(event.position, cx)
                        }),
                    )
                    .child(
                        div()
                            .relative()
                            .flex()
                            .justify_center()
                            .min_w(px(width))
                            .min_h(px(height))
                            .w_full()
                            .h_full()
                            .child(dots)
                            .child(layer),
                    ),
            )
            .children(empty)
            .children(self.detail())
    }
}

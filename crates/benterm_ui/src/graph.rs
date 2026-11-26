//! The graph of the progress board: the agents of a window as nodes, a
//! line from each to the agents it started, and an arrow for the
//! messages one gave another.
//!
//! Nothing here knows about the screen or the window. The window tells
//! which terminals there are and keeps the [`Messages`]; the board asks
//! [`layout`] where the nodes go, in columns and rows, and draws them.
//!
//! A node is named by the number of its terminal's pane id, which no
//! other terminal of the app's run ever has.

use std::collections::{HashMap, HashSet, VecDeque};

use benterm_core::Rgb;

use crate::theme::BoardTheme;
use crate::timeline::BandState;

/// One terminal in the graph.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Node {
    pub id: u64,
    /// The pane that started the agent, also when it is not in the graph.
    pub parent: Option<u64>,
    /// The name the agent was started under, or where its pane is.
    pub name: String,
    /// The agent recognised in the terminal: `claude`, `codex`, or none.
    pub agent: Option<&'static str>,
    /// None for a terminal whose agent is in none of the four states, as
    /// one that was started and not recognised yet, or a shell that
    /// started agents.
    pub state: Option<BandState>,
    /// The terminal has closed. The node is kept for a while, with the
    /// state it had last, and there is nothing to go to.
    pub closed: bool,
    /// Whether the terminal is behind the window, in no tab.
    pub background: bool,
}

impl Node {
    /// One of the four colors of the timeline, by the state.
    pub fn color(&self, theme: &BoardTheme) -> Rgb {
        match self.state {
            Some(state) => state.color(theme),
            None => theme.idle,
        }
    }

    /// Whether the agent has nothing more to do: its turn is over, or its
    /// terminal has closed.
    pub fn ended(&self) -> bool {
        self.closed || self.state == Some(BandState::Done)
    }

    /// What the node says under its name.
    pub fn state_label(&self) -> &'static str {
        match (self.closed, self.state) {
            (true, _) => "Closed",
            (_, Some(state)) => state.label(),
            (_, None) => "Idle",
        }
    }
}

/// The nodes the graph shows. With `hide_ended`, the ended ones are left
/// out, except those a shown node hangs under: without them the node
/// would look as if nobody had started it.
pub fn shown(nodes: &[Node], hide_ended: bool) -> Vec<&Node> {
    if !hide_ended {
        return nodes.iter().collect();
    }
    let by_id: HashMap<u64, &Node> = nodes.iter().map(|node| (node.id, node)).collect();
    let mut kept: HashSet<u64> = HashSet::new();
    for node in nodes.iter().filter(|node| !node.ended()) {
        let mut at = Some(node);
        // Up to the top, or to where another node has been already.
        while let Some(node) = at
            && kept.insert(node.id)
        {
            at = node.parent.and_then(|parent| by_id.get(&parent).copied());
        }
    }
    nodes
        .iter()
        .filter(|node| kept.contains(&node.id))
        .collect()
}

/// Where a node is: the column counts from the left in widths of a node,
/// and may lie between two; the row counts from the top.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Spot {
    pub id: u64,
    pub column: f32,
    pub row: usize,
}

/// Where the nodes of a graph are, and the lines from parent to child.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Layout {
    /// In the order the nodes were given.
    pub spots: Vec<Spot>,
    /// Parent first, in the order the children were given in.
    pub links: Vec<(u64, u64)>,
}

impl Layout {
    pub fn spot(&self, id: u64) -> Option<Spot> {
        self.spots.iter().find(|spot| spot.id == id).copied()
    }

    /// How many columns the graph is wide.
    pub fn columns(&self) -> f32 {
        let last = self
            .spots
            .iter()
            .map(|spot| spot.column)
            .fold(-1., f32::max);
        last + 1.
    }

    /// How many rows the graph is high.
    pub fn rows(&self) -> usize {
        self.spots
            .iter()
            .map(|spot| spot.row + 1)
            .max()
            .unwrap_or(0)
    }
}

/// Puts the nodes, given as an id and the id of the parent, in rows: a
/// parent above its children, the children side by side in the order
/// given, and the parent over their middle. A node whose parent is not
/// among the nodes stands in the top row, as one that has none.
///
/// The places depend on the ids, the parents and the order alone, so a
/// node stays where it is while its agent changes state or sends.
pub fn layout(nodes: &[(u64, Option<u64>)]) -> Layout {
    let ids: HashSet<u64> = nodes.iter().map(|(id, _)| *id).collect();
    let mut children: HashMap<u64, Vec<u64>> = HashMap::new();
    let mut tops = Vec::new();
    for (id, parent) in nodes {
        match parent {
            Some(parent) if parent != id && ids.contains(parent) => {
                children.entry(*parent).or_default().push(*id);
            }
            _ => tops.push(*id),
        }
    }
    let mut placer = Placer {
        children,
        placed: HashMap::new(),
        links: Vec::new(),
        next_column: 0.,
    };
    for top in tops {
        placer.place(top, 0);
    }
    // What is left names each other as parents, which the app never
    // makes. The first of them stands at the top, and the line that
    // would close the ring is left out.
    for (id, _) in nodes {
        if !placer.placed.contains_key(id) {
            placer.place(*id, 0);
        }
    }
    let spots = nodes
        .iter()
        .filter_map(|(id, _)| {
            let (column, row) = *placer.placed.get(id)?;
            Some(Spot {
                id: *id,
                column,
                row,
            })
        })
        .collect();
    let position: HashMap<u64, usize> = nodes.iter().map(|(id, _)| *id).zip(0..).collect();
    let mut links = placer.links;
    links.sort_by_key(|(_, child)| position[child]);
    Layout { spots, links }
}

struct Placer {
    children: HashMap<u64, Vec<u64>>,
    /// The column and the row of each node that has a place.
    placed: HashMap<u64, (f32, usize)>,
    links: Vec<(u64, u64)>,
    /// The column of the next node that has no children.
    next_column: f32,
}

impl Placer {
    /// Places `id` and all under it, and returns the column of `id`.
    fn place(&mut self, id: u64, row: usize) -> f32 {
        // Before the children, so that a ring ends here.
        self.placed.insert(id, (0., row));
        let children = self.children.get(&id).cloned().unwrap_or_default();
        let mut columns = Vec::new();
        for child in children {
            if self.placed.contains_key(&child) {
                continue;
            }
            self.links.push((id, child));
            columns.push(self.place(child, row + 1));
        }
        let column = match (columns.first(), columns.last()) {
            (Some(first), Some(last)) => (first + last) / 2.,
            _ => {
                self.next_column += 1.;
                self.next_column - 1.
            }
        };
        self.placed.insert(id, (column, row));
        column
    }
}

/// By how much a graph that is `natural` wide is shrunk to fit a place
/// that is `available` wide: 1 when it fits or the place is not known,
/// and never less than `least`, under which it is scrolled instead.
pub fn shrink(natural: f32, available: f32, least: f32) -> f32 {
    if available <= 0. || natural <= available {
        return 1.;
    }
    (available / natural).max(least)
}

/// What one agent gave another.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MessageKind {
    /// Sent with `agent send`.
    Request,
    /// Sent to the parent with `agent report`.
    Report,
}

impl MessageKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Request => "Request",
            Self::Report => "Report",
        }
    }
}

/// A message the app took from one terminal for another.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Message {
    /// Counts up with every message of the window.
    pub id: u64,
    pub kind: MessageKind,
    pub from: u64,
    pub to: u64,
    /// When the app took the message, in milliseconds since 1970.
    pub at_ms: u64,
    /// The first line of the text.
    pub line: String,
}

/// The latest messages between the terminals of a window, oldest first.
/// They are kept in memory, for as long as the window is open.
#[derive(Debug, Default)]
pub struct Messages {
    list: VecDeque<Message>,
    next_id: u64,
}

impl Messages {
    /// How many messages are kept.
    pub const CAPACITY: usize = 256;

    pub fn push(&mut self, kind: MessageKind, from: u64, to: u64, at_ms: u64, text: &str) {
        if self.list.len() == Self::CAPACITY {
            self.list.pop_front();
        }
        self.list.push_back(Message {
            id: self.next_id,
            kind,
            from,
            to,
            at_ms,
            line: first_line(text),
        });
        self.next_id += 1;
    }

    pub fn iter(&self) -> impl Iterator<Item = &Message> {
        self.list.iter()
    }
}

/// The most characters of a message's first line that are kept.
const LINE_LIMIT: usize = 200;

/// The first line of `text` that has something in it, without the
/// characters a terminal would act on, and cut if it is long.
pub fn first_line(text: &str) -> String {
    let line = text.lines().map(str::trim).find(|line| !line.is_empty());
    let mut kept: String = line
        .unwrap_or_default()
        .chars()
        .map(|ch| if ch == '\t' { ' ' } else { ch })
        .filter(|ch| !ch.is_control())
        .collect();
    if let Some((cut, _)) = kept.char_indices().nth(LINE_LIMIT) {
        kept.truncate(cut);
        kept.push('…');
    }
    kept
}

/// The messages from one node to another, drawn as one arrow.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Arrow {
    pub from: u64,
    pub to: u64,
    /// How many messages went this way.
    pub count: usize,
    pub latest: Message,
}

/// One arrow for each direction between two nodes that a message went,
/// in the order of the first message of each. Messages from or to a node
/// for which `shown` says no are left out, and those a terminal sent
/// itself.
pub fn arrows<'a>(
    messages: impl Iterator<Item = &'a Message>,
    shown: impl Fn(u64) -> bool,
) -> Vec<Arrow> {
    let mut arrows: Vec<Arrow> = Vec::new();
    let mut index: HashMap<(u64, u64), usize> = HashMap::new();
    for message in messages {
        if message.from == message.to || !shown(message.from) || !shown(message.to) {
            continue;
        }
        match index.get(&(message.from, message.to)) {
            Some(position) => {
                let arrow = &mut arrows[*position];
                arrow.count += 1;
                arrow.latest = message.clone();
            }
            None => {
                index.insert((message.from, message.to), arrows.len());
                arrows.push(Arrow {
                    from: message.from,
                    to: message.to,
                    count: 1,
                    latest: message.clone(),
                });
            }
        }
    }
    arrows
}

/// How long it takes a message to lose half of what makes it stand out.
const HALF_LIFE_MS: f32 = 120_000.;
/// How much a message of long ago still stands out.
const FAINTEST: f32 = 0.25;
/// The steps between the faintest and the strongest.
const STEPS: f32 = 12.;

/// How much the arrow of a message sent `age_ms` ago stands out: 1 when
/// it was just sent, less with time, and never under a quarter. The
/// answer moves in steps, so that an arrow looks the same for a while.
pub fn prominence(age_ms: u64) -> f32 {
    let fresh = 0.5f32.powf(age_ms as f32 / HALF_LIFE_MS);
    let stepped = (fresh * STEPS).round() / STEPS;
    FAINTEST + (1. - FAINTEST) * stepped
}

/// The time of day of `at_ms`, milliseconds since 1970, on a clock that
/// is `offset_seconds` ahead of UTC.
pub fn clock(at_ms: u64, offset_seconds: i64) -> String {
    const DAY: i64 = 24 * 3600;
    let seconds = ((at_ms / 1000) as i64 + offset_seconds).rem_euclid(DAY);
    format!(
        "{:02}:{:02}:{:02}",
        seconds / 3600,
        seconds / 60 % 60,
        seconds % 60
    )
}

/// A bent line, as the arrow of a message is drawn: a curve from `start`
/// to `end` that is pulled towards `control`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Curve {
    pub start: (f32, f32),
    pub control: (f32, f32),
    pub end: (f32, f32),
}

/// How far the middle of a message's line is bent away from the straight
/// line between its two nodes, as a share of that line's length, and at
/// most.
const BEND: f32 = 0.12;
const MOST_BEND: f32 = 22.;
/// How far the middle of a line between two nodes of a row stays from
/// the nodes of the row.
const ROW_CLEARANCE: f32 = 22.;

/// The line of a message between two nodes, given the middle of each and
/// half the width and height of a node. It leaves the edge of the sender
/// and ends at the edge of the receiver, and bends to one side of the
/// way it goes, the same for every line: the lines of the two directions bend apart, and neither
/// lies on the line from parent to child.
pub fn message_curve(from: (f32, f32), to: (f32, f32), half: (f32, f32)) -> Curve {
    let (dx, dy) = (to.0 - from.0, to.1 - from.1);
    let length = (dx * dx + dy * dy).sqrt().max(1.);
    // Between two nodes of a row the line goes around the nodes that
    // may stand between them, with room for the mark in its middle.
    let same_row = dy.abs() < half.1;
    let bend = match same_row {
        true => half.1 + ROW_CLEARANCE,
        false => (length * BEND).min(MOST_BEND),
    };
    // Across the way, turned the same for every line.
    let (nx, ny) = (-dy / length, dx / length);
    let control = (
        (from.0 + to.0) / 2. + nx * bend * 2.,
        (from.1 + to.1) / 2. + ny * bend * 2.,
    );
    Curve {
        start: edge(from, control, half),
        control,
        end: edge(to, control, half),
    }
}

/// Where the way from the middle of a node towards `target` leaves the
/// node.
fn edge(center: (f32, f32), target: (f32, f32), half: (f32, f32)) -> (f32, f32) {
    let (dx, dy) = (target.0 - center.0, target.1 - center.1);
    if dx == 0. && dy == 0. {
        return center;
    }
    let by_width = if dx == 0. {
        f32::MAX
    } else {
        half.0 / dx.abs()
    };
    let by_height = if dy == 0. {
        f32::MAX
    } else {
        half.1 / dy.abs()
    };
    let share = by_width.min(by_height).min(1.);
    (center.0 + dx * share, center.1 + dy * share)
}

impl Curve {
    /// How many straight pieces the curve is taken as, to tell how far a
    /// point is from it.
    const PIECES: usize = 16;

    /// The point a `share` of the way along the curve, from 0 to 1.
    pub fn at(&self, share: f32) -> (f32, f32) {
        let rest = 1. - share;
        let along = |start: f32, control: f32, end: f32| {
            rest * rest * start + 2. * rest * share * control + share * share * end
        };
        (
            along(self.start.0, self.control.0, self.end.0),
            along(self.start.1, self.control.1, self.end.1),
        )
    }

    /// How far `point` is from the curve.
    pub fn distance(&self, point: (f32, f32)) -> f32 {
        let points = (0..=Self::PIECES).map(|n| self.at(n as f32 / Self::PIECES as f32));
        let mut nearest = f32::MAX;
        let mut before = self.start;
        for next in points.skip(1) {
            nearest = nearest.min(distance_to_piece(point, before, next));
            before = next;
        }
        nearest
    }

    /// The two corners at the back of the arrowhead at the end of the
    /// curve, `length` long and half as wide on each side.
    pub fn head(&self, length: f32) -> [(f32, f32); 2] {
        let (dx, dy) = (self.end.0 - self.control.0, self.end.1 - self.control.1);
        let along = (dx * dx + dy * dy).sqrt().max(1.);
        let (ux, uy) = (dx / along, dy / along);
        let back = (self.end.0 - ux * length, self.end.1 - uy * length);
        let (sx, sy) = (-uy * length / 2., ux * length / 2.);
        [(back.0 + sx, back.1 + sy), (back.0 - sx, back.1 - sy)]
    }
}

fn distance_to_piece(point: (f32, f32), start: (f32, f32), end: (f32, f32)) -> f32 {
    let (dx, dy) = (end.0 - start.0, end.1 - start.1);
    let squared = dx * dx + dy * dy;
    let share = if squared == 0. {
        0.
    } else {
        (((point.0 - start.0) * dx + (point.1 - start.1) * dy) / squared).clamp(0., 1.)
    };
    let (x, y) = (start.0 + dx * share, start.1 + dy * share);
    ((point.0 - x).powi(2) + (point.1 - y).powi(2)).sqrt()
}

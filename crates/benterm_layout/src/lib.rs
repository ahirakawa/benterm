//! Panes form a split tree. Workspaces and their tabs are ordered lists.
//! Sizes use whole units so layout works in pixels or cells.

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};

mod shape;
mod tabs;
mod workspaces;

pub use crate::shape::TreeShape;
pub use crate::tabs::{TabError, TabId, TabList};
pub use crate::workspaces::is_numbered_name;
pub use crate::workspaces::{
    Workspace, WorkspaceColor, WorkspaceError, WorkspaceId, WorkspaceList, ends_a_line,
};

/// Identifies a pane. Ids come from one counter for the whole process, so
/// no two panes share an id, even in different trees, and an id is never
/// reused after its pane is closed. This lets an id name a pane from
/// outside the app.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PaneId(u64);

impl PaneId {
    /// An id no pane in the process has had yet.
    fn next() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        PaneId(NEXT.fetch_add(1, Ordering::Relaxed))
    }

    /// The number behind the id.
    pub fn get(self) -> u64 {
        self.0
    }
}

impl fmt::Display for PaneId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// A width and a height.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Size {
    pub width: u32,
    pub height: u32,
}

impl Size {
    pub const fn new(width: u32, height: u32) -> Self {
        Self { width, height }
    }
}

/// A rectangle whose origin is the top left corner of the tab.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl Rect {
    pub const fn new(x: u32, y: u32, width: u32, height: u32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }
}

/// A border between panes, which can be drawn and dragged.
///
/// Calling [`PaneTree::resize`] with `pane` and `side` moves this border.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PaneBorder {
    /// The pane on the left of an upright border, or above a flat one.
    pub pane: PaneId,
    /// [`Direction::Right`] for an upright border, [`Direction::Down`] for
    /// a flat one.
    pub side: Direction,
    /// Where the border runs: a rectangle with no width for an upright
    /// border and with no height for a flat one.
    pub line: Rect,
}

/// Where a split puts the new pane.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SplitDirection {
    /// To the right of the pane, which becomes the left half.
    Right,
    /// Below the pane, which becomes the top half.
    Down,
}

/// A direction on the screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

impl Direction {
    /// The opposite direction.
    pub const fn opposite(self) -> Self {
        match self {
            Self::Left => Self::Right,
            Self::Right => Self::Left,
            Self::Up => Self::Down,
            Self::Down => Self::Up,
        }
    }
}

/// Why an operation on the tree was refused. A refused operation leaves the
/// tree unchanged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LayoutError {
    UnknownPane(PaneId),
    /// The tab is too small to give every pane the minimum size after the
    /// split.
    TooSmall,
    /// The last pane of a tab cannot be closed by the tree.
    LastPane,
    /// The pane has no border on that side to move, because it touches the
    /// edge of the tab there.
    NoBorder,
}

impl fmt::Display for LayoutError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownPane(id) => write!(f, "no pane has the id {id}"),
            Self::TooSmall => f.write_str("the tab is too small for another pane"),
            Self::LastPane => f.write_str("the last pane of a tab cannot be closed"),
            Self::NoBorder => f.write_str("the pane has no border on that side"),
        }
    }
}

impl std::error::Error for LayoutError {}

/// The panes of one tab and which of them is focused.
#[derive(Clone, Debug)]
pub struct PaneTree {
    root: Node,
    focused: PaneId,
    /// The pane temporarily zoomed to fill the tab, if any.
    zoomed: Option<PaneId>,
    min_pane: Size,
}

impl PaneTree {
    /// A tree with a single focused pane. No pane will be made smaller than
    /// `min_pane` while the tab is at least [`PaneTree::min_size`].
    pub fn new(min_pane: Size) -> Self {
        let first = PaneId::next();
        Self {
            root: Node::Leaf(first),
            focused: first,
            zoomed: None,
            min_pane,
        }
    }

    /// The smallest pane size the tree keeps.
    pub fn min_pane(&self) -> Size {
        self.min_pane
    }

    /// The panes, from top left to bottom right in tree order.
    pub fn panes(&self) -> Vec<PaneId> {
        let mut panes = Vec::new();
        self.root.collect_panes(&mut panes);
        panes
    }

    /// Whether a pane with this id is in the tree.
    pub fn contains(&self, id: PaneId) -> bool {
        self.root.contains(id)
    }

    pub fn focused(&self) -> PaneId {
        self.focused
    }

    /// Focuses a pane, for example after a click. Focusing another pane
    /// while zoomed unzooms the tree so the focused pane is shown.
    pub fn focus(&mut self, id: PaneId) -> Result<(), LayoutError> {
        self.check_known(id)?;
        if self.zoomed.is_some_and(|zoomed| zoomed != id) {
            self.zoomed = None;
        }
        self.focused = id;
        Ok(())
    }

    /// The pane currently zoomed to fill the tab, if any.
    pub fn zoomed(&self) -> Option<PaneId> {
        self.zoomed
    }

    /// Toggles zooming the focused pane to fill the tab, returning the zoomed
    /// pane if zoom is now active. Does nothing when the tab has only one
    /// pane.
    pub fn toggle_zoom(&mut self) -> Option<PaneId> {
        if self.zoomed.is_some() {
            self.zoomed = None;
        } else if !matches!(self.root, Node::Leaf(_)) {
            self.zoomed = Some(self.focused);
        }
        self.zoomed
    }

    /// Exits zoom if a pane is currently zoomed.
    pub fn unzoom(&mut self) {
        self.zoomed = None;
    }

    /// The smallest tab that gives every pane at least the minimum size.
    pub fn min_size(&self) -> Size {
        min_size_of(&self.root, self.min_pane)
    }

    fn unzoomed_layout(&self, tab: Size) -> Vec<(PaneId, Rect)> {
        let mut out = Vec::new();
        self.root.layout(
            Rect::new(0, 0, tab.width, tab.height),
            self.min_pane,
            &mut |node, rect| {
                if let Node::Leaf(id) = node {
                    out.push((*id, rect));
                }
            },
        );
        out
    }

    /// The position and size of every shown pane in a tab of size `tab`, in
    /// the order of [`PaneTree::panes`]. When a pane is zoomed, only that
    /// pane is returned at the full tab size.
    ///
    /// The rectangles never overlap and always cover the tab exactly. Each is
    /// at least the minimum pane size when `tab` is at least
    /// [`PaneTree::min_size`].
    pub fn layout(&self, tab: Size) -> Vec<(PaneId, Rect)> {
        if let Some(id) = self.zoomed {
            return vec![(id, Rect::new(0, 0, tab.width, tab.height))];
        }
        self.unzoomed_layout(tab)
    }

    /// The borders of every split in a tab of size `tab`, outer splits
    /// first. Empty while a pane is zoomed.
    pub fn borders(&self, tab: Size) -> Vec<PaneBorder> {
        if self.zoomed.is_some() {
            return Vec::new();
        }
        let mut out = Vec::new();
        self.root.layout(
            Rect::new(0, 0, tab.width, tab.height),
            self.min_pane,
            &mut |node, rect| {
                if let Node::Split {
                    axis,
                    ratio,
                    first,
                    second,
                } = node
                {
                    let length = first_length(*axis, *ratio, first, second, rect, self.min_pane);
                    let (side, line) = match axis {
                        Axis::Horizontal => (
                            Direction::Right,
                            Rect::new(rect.x + length, rect.y, 0, rect.height),
                        ),
                        Axis::Vertical => (
                            Direction::Down,
                            Rect::new(rect.x, rect.y + length, rect.width, 0),
                        ),
                    };
                    // Inside `first`, its last pane is always in the second
                    // half of every split, so the nearest border on its
                    // `side` is this one.
                    out.push(PaneBorder {
                        pane: first.last_pane(),
                        side,
                        line,
                    });
                }
            },
        );
        out
    }

    /// Splits a pane in two and focuses the new pane, which takes half of the
    /// space. Fails if the tab would then be smaller than
    /// [`PaneTree::min_size`].
    pub fn split(
        &mut self,
        id: PaneId,
        direction: SplitDirection,
        tab: Size,
    ) -> Result<PaneId, LayoutError> {
        self.check_known(id)?;
        let axis = match direction {
            SplitDirection::Right => Axis::Horizontal,
            SplitDirection::Down => Axis::Vertical,
        };
        let mut root = self.root.clone();
        root.fix_ratios(Rect::new(0, 0, tab.width, tab.height), self.min_pane);
        let leaf = root.find_mut(id).expect("the pane was checked to exist");
        // A refused split leaves a gap in the ids, which is harmless.
        let new = PaneId::next();
        *leaf = Node::Split {
            axis,
            ratio: 0.5,
            first: Box::new(Node::Leaf(id)),
            second: Box::new(Node::Leaf(new)),
        };
        let min = min_size_of(&root, self.min_pane);
        if min.width > tab.width || min.height > tab.height {
            return Err(LayoutError::TooSmall);
        }
        self.root = root;
        self.focused = new;
        self.zoomed = None;
        Ok(new)
    }

    /// Closes a pane. The pane next to it in the tree takes its space, and
    /// the other panes keep their place in a tab of size `tab`. If the
    /// closed pane was focused, the pane that took its space gets the focus.
    pub fn close(&mut self, id: PaneId, tab: Size) -> Result<(), LayoutError> {
        self.check_known(id)?;
        if matches!(self.root, Node::Leaf(_)) {
            return Err(LayoutError::LastPane);
        }
        self.root
            .fix_ratios(Rect::new(0, 0, tab.width, tab.height), self.min_pane);
        let heir = self
            .root
            .remove(id)
            .expect("a pane that is not the root has a parent");
        if self.focused == id {
            self.focused = heir;
        }
        if self.zoomed == Some(id) || matches!(self.root, Node::Leaf(_)) {
            self.zoomed = None;
        }
        Ok(())
    }

    /// Moves the border on the `side` of a pane by `delta` units in a tab of
    /// size `tab`. A positive `delta` moves the border towards `side`, which
    /// makes the pane bigger. The border stops where a pane would go below
    /// the minimum size.
    pub fn resize(
        &mut self,
        id: PaneId,
        side: Direction,
        delta: i32,
        tab: Size,
    ) -> Result<(), LayoutError> {
        self.check_known(id)?;
        let rect = Rect::new(0, 0, tab.width, tab.height);
        let mut root = self.root.clone();
        root.fix_ratios(rect, self.min_pane);
        match root.move_border(rect, self.min_pane, id, side, delta) {
            Border::Moved => {
                self.root = root;
                Ok(())
            }
            Border::Wanted => Err(LayoutError::NoBorder),
            Border::NotHere => unreachable!("the pane was checked to exist"),
        }
    }

    /// Moves a border of `id` towards `direction` by `amount` units in a tab
    /// of size `tab`: the border on `direction`'s side of the pane if it has
    /// one, or else the border on the opposite side. Fails with
    /// [`LayoutError::NoBorder`] if the pane has no border on either side.
    pub fn resize_towards(
        &mut self,
        id: PaneId,
        direction: Direction,
        amount: i32,
        tab: Size,
    ) -> Result<(), LayoutError> {
        match self.resize(id, direction, amount, tab) {
            Err(LayoutError::NoBorder) => self.resize(id, direction.opposite(), -amount, tab),
            result => result,
        }
    }

    /// The pane next to pane `id` in `direction`, in a tab of size `tab`.
    /// Of the panes that share that edge, the one sharing the longest part of
    /// it wins, and on a tie the one nearer the top or the left. Returns
    /// `None` if the pane touches the edge of the tab there, or is not in the
    /// tree.
    pub fn neighbor(&self, id: PaneId, direction: Direction, tab: Size) -> Option<PaneId> {
        let layout = self.unzoomed_layout(tab);
        let from = layout.iter().find(|(pane, _)| *pane == id)?.1;
        layout
            .iter()
            .filter_map(|(pane, to)| {
                let shared = shared_edge(&from, to, direction)?;
                let start = match direction {
                    Direction::Left | Direction::Right => to.y,
                    Direction::Up | Direction::Down => to.x,
                };
                Some((shared, std::cmp::Reverse(start), *pane))
            })
            .max()
            .map(|(_, _, pane)| pane)
    }

    /// Focuses the pane next to the focused pane in `direction`, and returns
    /// it. If a pane was zoomed and a neighbor exists, zoom is cleared as
    /// focus moves. Returns `None` and keeps the focus (and zoom) if there is
    /// no pane there.
    pub fn focus_neighbor(&mut self, direction: Direction, tab: Size) -> Option<PaneId> {
        let next = self.neighbor(self.focused, direction, tab)?;
        self.focused = next;
        self.zoomed = None;
        Some(next)
    }

    fn check_known(&self, id: PaneId) -> Result<(), LayoutError> {
        if self.contains(id) {
            Ok(())
        } else {
            Err(LayoutError::UnknownPane(id))
        }
    }
}

/// How far [`Node::move_border`] got.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Border {
    /// The pane is not below the node.
    NotHere,
    /// The pane is below the node, but no split below it has a border on
    /// that side of the pane.
    Wanted,
    Moved,
}

/// The direction along which a split divides its space.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Axis {
    /// Side by side: the split divides the width.
    Horizontal,
    /// One above the other: the split divides the height.
    Vertical,
}

#[derive(Clone, Debug)]
enum Node {
    Leaf(PaneId),
    Split {
        axis: Axis,
        /// The share of the space that goes to `first`, before the minimum
        /// sizes are applied.
        ratio: f64,
        first: Box<Node>,
        second: Box<Node>,
    },
}

impl Node {
    fn collect_panes(&self, out: &mut Vec<PaneId>) {
        match self {
            Node::Leaf(id) => out.push(*id),
            Node::Split { first, second, .. } => {
                first.collect_panes(out);
                second.collect_panes(out);
            }
        }
    }

    fn contains(&self, id: PaneId) -> bool {
        match self {
            Node::Leaf(leaf) => *leaf == id,
            Node::Split { first, second, .. } => first.contains(id) || second.contains(id),
        }
    }

    fn find_mut(&mut self, id: PaneId) -> Option<&mut Node> {
        match self {
            Node::Leaf(leaf) if *leaf == id => Some(self),
            Node::Leaf(_) => None,
            Node::Split { first, second, .. } => first.find_mut(id).or_else(|| second.find_mut(id)),
        }
    }

    fn first_pane(&self) -> PaneId {
        match self {
            Node::Leaf(id) => *id,
            Node::Split { first, .. } => first.first_pane(),
        }
    }

    fn last_pane(&self) -> PaneId {
        match self {
            Node::Leaf(id) => *id,
            Node::Split { second, .. } => second.last_pane(),
        }
    }

    /// Removes the pane below this node and puts its sibling in the place of
    /// their split. Returns the pane of the sibling that touched the removed
    /// pane, or `None` if the pane is not below this node.
    fn remove(&mut self, id: PaneId) -> Option<PaneId> {
        let Node::Split { first, second, .. } = self else {
            return None;
        };
        let (heir, survivor) = if matches!(**first, Node::Leaf(leaf) if leaf == id) {
            (
                second.first_pane(),
                std::mem::replace(&mut **second, Node::Leaf(id)),
            )
        } else if matches!(**second, Node::Leaf(leaf) if leaf == id) {
            (
                first.last_pane(),
                std::mem::replace(&mut **first, Node::Leaf(id)),
            )
        } else {
            return first.remove(id).or_else(|| second.remove(id));
        };
        *self = survivor;
        Some(heir)
    }

    /// The smallest length along `axis` that keeps every pane at least `min`.
    fn min_extent(&self, axis: Axis, min: Size) -> u32 {
        match self {
            Node::Leaf(_) => extent(min, axis),
            Node::Split {
                axis: split_axis,
                first,
                second,
                ..
            } => {
                let a = first.min_extent(axis, min);
                let b = second.min_extent(axis, min);
                if *split_axis == axis {
                    a.saturating_add(b)
                } else {
                    a.max(b)
                }
            }
        }
    }

    /// Calls `visit` with every node and the rectangle it gets inside `rect`.
    fn layout(&self, rect: Rect, min: Size, visit: &mut impl FnMut(&Node, Rect)) {
        visit(self, rect);
        if let Node::Split {
            axis,
            ratio,
            first,
            second,
        } = self
        {
            let (a, b) = divide(
                rect,
                *axis,
                first_length(*axis, *ratio, first, second, rect, min),
            );
            first.layout(a, min, visit);
            second.layout(b, min, visit);
        }
    }

    /// Moves the nearest border on the `side` of pane `id`, for a node laid
    /// out in `rect`.
    fn move_border(
        &mut self,
        rect: Rect,
        min: Size,
        id: PaneId,
        side: Direction,
        delta: i32,
    ) -> Border {
        let Node::Split {
            axis,
            ratio,
            first,
            second,
        } = self
        else {
            return match self {
                Node::Leaf(leaf) if *leaf == id => Border::Wanted,
                _ => Border::NotHere,
            };
        };
        let length = first_length(*axis, *ratio, first, second, rect, min);
        let (a, b) = divide(rect, *axis, length);
        let (in_first, found) = match first.move_border(a, min, id, side, delta) {
            Border::NotHere => (false, second.move_border(b, min, id, side, delta)),
            found => (true, found),
        };
        if found != Border::Wanted {
            return found;
        }
        let towards_second = match (side, *axis) {
            (Direction::Right, Axis::Horizontal) | (Direction::Down, Axis::Vertical) => true,
            (Direction::Left, Axis::Horizontal) | (Direction::Up, Axis::Vertical) => false,
            _ => return Border::Wanted,
        };
        if in_first != towards_second {
            return Border::Wanted;
        }
        let total = extent(Size::new(rect.width, rect.height), *axis);
        let lo = first.min_extent(*axis, min);
        let lo_second = second.min_extent(*axis, min);
        // When the space is too small for both minimums, the border stays.
        if let Some(hi) = total.checked_sub(lo_second).filter(|hi| *hi >= lo) {
            let change = if in_first {
                i64::from(delta)
            } else {
                -i64::from(delta)
            };
            let moved = (i64::from(length) + change).clamp(i64::from(lo), i64::from(hi));
            *ratio = moved as f64 / f64::from(total);
        }
        Border::Moved
    }

    /// Sets every ratio to the share its first child has in `rect` now, so
    /// that a later change to the tree keeps what is on the screen.
    fn fix_ratios(&mut self, rect: Rect, min: Size) {
        if let Node::Split {
            axis,
            ratio,
            first,
            second,
        } = self
        {
            let total = extent(Size::new(rect.width, rect.height), *axis);
            let length = first_length(*axis, *ratio, first, second, rect, min);
            if total > 0 {
                *ratio = f64::from(length) / f64::from(total);
            }
            let (a, b) = divide(rect, *axis, length);
            first.fix_ratios(a, min);
            second.fix_ratios(b, min);
        }
    }
}

/// How long the first child of a split is inside `rect`.
fn first_length(axis: Axis, ratio: f64, first: &Node, second: &Node, rect: Rect, min: Size) -> u32 {
    let total = extent(Size::new(rect.width, rect.height), axis);
    let lo = first.min_extent(axis, min);
    let lo_second = second.min_extent(axis, min);
    if let Some(hi) = total.checked_sub(lo_second).filter(|hi| *hi >= lo) {
        let wanted = (ratio * f64::from(total)).round() as u32;
        wanted.clamp(lo, hi)
    } else {
        // The space is too small for both minimums: share it in
        // proportion to them, so that neither side is left empty when
        // it can be avoided. The sum is above `total`, so it is not zero.
        let sum = u64::from(lo) + u64::from(lo_second);
        (u64::from(total) * u64::from(lo) / sum) as u32
    }
}

/// How long a stretch of the `direction` edge of `from` the rectangle `to`
/// touches from the other side, or `None` if it touches none of it.
fn shared_edge(from: &Rect, to: &Rect, direction: Direction) -> Option<u32> {
    let (touching, from_span, to_span) = match direction {
        Direction::Left => (to.x + to.width == from.x, vertical(from), vertical(to)),
        Direction::Right => (from.x + from.width == to.x, vertical(from), vertical(to)),
        Direction::Up => (to.y + to.height == from.y, horizontal(from), horizontal(to)),
        Direction::Down => (
            from.y + from.height == to.y,
            horizontal(from),
            horizontal(to),
        ),
    };
    let start = from_span.0.max(to_span.0);
    let end = from_span.1.min(to_span.1);
    (touching && start < end).then(|| end - start)
}

fn horizontal(rect: &Rect) -> (u32, u32) {
    (rect.x, rect.x + rect.width)
}

fn vertical(rect: &Rect) -> (u32, u32) {
    (rect.y, rect.y + rect.height)
}

fn extent(size: Size, axis: Axis) -> u32 {
    match axis {
        Axis::Horizontal => size.width,
        Axis::Vertical => size.height,
    }
}

fn min_size_of(node: &Node, min: Size) -> Size {
    Size::new(
        node.min_extent(Axis::Horizontal, min),
        node.min_extent(Axis::Vertical, min),
    )
}

/// Cuts `rect` along `axis` so that the first part is `length` long.
fn divide(rect: Rect, axis: Axis, length: u32) -> (Rect, Rect) {
    match axis {
        Axis::Horizontal => (
            Rect::new(rect.x, rect.y, length, rect.height),
            Rect::new(rect.x + length, rect.y, rect.width - length, rect.height),
        ),
        Axis::Vertical => (
            Rect::new(rect.x, rect.y, rect.width, length),
            Rect::new(rect.x, rect.y + length, rect.width, rect.height - length),
        ),
    }
}

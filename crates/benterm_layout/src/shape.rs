//! A tree taken apart into plain values, and built again from them.
//!
//! A shape has no pane ids, which belong to one run of the app. This is
//! what lets the panes of a tab be saved and made again after a restart.

use crate::{Axis, Node, PaneId, PaneTree, Size, SplitDirection};

/// The splits of a tree, with something of the caller's in each pane.
#[derive(Clone, Debug, PartialEq)]
pub enum TreeShape<P> {
    Pane(P),
    Split {
        /// Where `second` is, seen from `first`.
        direction: SplitDirection,
        /// The share of the space `first` gets before the minimum sizes
        /// are applied, from 0 to 1.
        ratio: f64,
        first: Box<TreeShape<P>>,
        second: Box<TreeShape<P>>,
    },
}

impl<P> TreeShape<P> {
    /// The same splits with `change` applied to every pane, from top left
    /// to bottom right in tree order.
    pub fn map<Q>(self, change: &mut impl FnMut(P) -> Q) -> TreeShape<Q> {
        match self {
            TreeShape::Pane(pane) => TreeShape::Pane(change(pane)),
            TreeShape::Split {
                direction,
                ratio,
                first,
                second,
            } => {
                let first = Box::new(first.map(change));
                let second = Box::new(second.map(change));
                TreeShape::Split {
                    direction,
                    ratio,
                    first,
                    second,
                }
            }
        }
    }

    /// How many panes the shape has; at least one.
    pub fn pane_count(&self) -> usize {
        match self {
            TreeShape::Pane(_) => 1,
            TreeShape::Split { first, second, .. } => first.pane_count() + second.pane_count(),
        }
    }
}

impl PaneTree {
    /// The splits of the tree with the id of each pane.
    pub fn shape(&self) -> TreeShape<PaneId> {
        shape_of(&self.root)
    }

    /// A tree with the splits of `shape` and a new pane for each of its
    /// panes. Returns the new ids with what the shape held, in the order
    /// of [`PaneTree::panes`].
    ///
    /// `focused` counts the panes in that order from 0; past the last
    /// pane it means the last. A ratio that is not finite becomes a half,
    /// and one outside 0 to 1 the nearer of the two.
    pub fn from_shape<P>(
        shape: TreeShape<P>,
        focused: usize,
        min_pane: Size,
    ) -> (Self, Vec<(PaneId, P)>) {
        let mut panes = Vec::new();
        let root = node_of(shape, &mut panes);
        let focused = panes[focused.min(panes.len() - 1)].0;
        let tree = Self {
            root,
            focused,
            zoomed: None,
            min_pane,
        };
        (tree, panes)
    }
}

fn shape_of(node: &Node) -> TreeShape<PaneId> {
    match node {
        Node::Leaf(id) => TreeShape::Pane(*id),
        Node::Split {
            axis,
            ratio,
            first,
            second,
        } => TreeShape::Split {
            direction: match axis {
                Axis::Horizontal => SplitDirection::Right,
                Axis::Vertical => SplitDirection::Down,
            },
            ratio: *ratio,
            first: Box::new(shape_of(first)),
            second: Box::new(shape_of(second)),
        },
    }
}

fn node_of<P>(shape: TreeShape<P>, panes: &mut Vec<(PaneId, P)>) -> Node {
    match shape {
        TreeShape::Pane(content) => {
            let id = PaneId::next();
            panes.push((id, content));
            Node::Leaf(id)
        }
        TreeShape::Split {
            direction,
            ratio,
            first,
            second,
        } => Node::Split {
            axis: match direction {
                SplitDirection::Right => Axis::Horizontal,
                SplitDirection::Down => Axis::Vertical,
            },
            ratio: if ratio.is_finite() {
                ratio.clamp(0.0, 1.0)
            } else {
                0.5
            },
            first: Box::new(node_of(*first, panes)),
            second: Box::new(node_of(*second, panes)),
        },
    }
}

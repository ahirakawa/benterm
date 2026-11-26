//! The borders between panes, which the UI draws and lets the user drag.

use benterm_layout::{Direction, PaneBorder, PaneTree, Rect, Size, SplitDirection};

const MIN: Size = Size::new(10, 4);
const TAB: Size = Size::new(100, 40);

#[test]
fn a_single_pane_has_no_borders() {
    let tree = PaneTree::new(MIN);
    assert!(tree.borders(TAB).is_empty());
}

#[test]
fn a_split_right_has_one_upright_border_owned_by_the_left_pane() {
    let mut tree = PaneTree::new(MIN);
    let left = tree.focused();
    tree.split(left, SplitDirection::Right, TAB).unwrap();
    assert_eq!(
        tree.borders(TAB),
        vec![PaneBorder {
            pane: left,
            side: Direction::Right,
            line: Rect::new(50, 0, 0, 40),
        }]
    );
}

#[test]
fn a_nested_split_has_a_border_only_as_long_as_its_half() {
    let mut tree = PaneTree::new(MIN);
    let left = tree.focused();
    let top = tree.split(left, SplitDirection::Right, TAB).unwrap();
    tree.split(top, SplitDirection::Down, TAB).unwrap();
    assert_eq!(
        tree.borders(TAB),
        vec![
            PaneBorder {
                pane: left,
                side: Direction::Right,
                line: Rect::new(50, 0, 0, 40),
            },
            PaneBorder {
                pane: top,
                side: Direction::Down,
                line: Rect::new(50, 20, 50, 0),
            },
        ]
    );
}

#[test]
fn resizing_the_pane_of_a_border_moves_that_border() {
    // (a | b) above c: the border under the top row belongs to b, the last
    // pane of the top row.
    let mut tree = PaneTree::new(MIN);
    let a = tree.focused();
    let c = tree.split(a, SplitDirection::Down, TAB).unwrap();
    let b = tree.split(a, SplitDirection::Right, TAB).unwrap();
    let borders = tree.borders(TAB);
    let under_top = borders
        .iter()
        .find(|border| border.side == Direction::Down)
        .copied()
        .unwrap();
    assert_eq!(under_top.pane, b);
    assert_eq!(under_top.line, Rect::new(0, 20, 100, 0));

    tree.resize(under_top.pane, under_top.side, 5, TAB).unwrap();

    let moved = tree.borders(TAB);
    assert!(moved.contains(&PaneBorder {
        line: Rect::new(0, 25, 100, 0),
        ..under_top
    }));
    let rect_of = |id| {
        tree.layout(TAB)
            .into_iter()
            .find(|(p, _)| *p == id)
            .unwrap()
            .1
    };
    assert_eq!(rect_of(c), Rect::new(0, 25, 100, 15));
}

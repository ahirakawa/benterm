//! Moving the borders between panes.

use benterm_layout::{Direction, LayoutError, PaneId, PaneTree, Rect, Size, SplitDirection};

const MIN: Size = Size::new(10, 4);
const TAB: Size = Size::new(100, 40);

fn left_and_right() -> (PaneTree, PaneId, PaneId) {
    let mut tree = PaneTree::new(MIN);
    let left = tree.focused();
    let right = tree.split(left, SplitDirection::Right, TAB).unwrap();
    (tree, left, right)
}

fn widths(tree: &PaneTree, tab: Size) -> Vec<u32> {
    tree.layout(tab).iter().map(|(_, r)| r.width).collect()
}

#[test]
fn moving_the_right_border_of_the_left_pane_widens_it() {
    let (mut tree, left, _) = left_and_right();
    tree.resize(left, Direction::Right, 10, TAB).unwrap();
    assert_eq!(widths(&tree, TAB), vec![60, 40]);
    tree.resize(left, Direction::Right, -30, TAB).unwrap();
    assert_eq!(widths(&tree, TAB), vec![30, 70]);
}

#[test]
fn moving_the_left_border_of_the_right_pane_widens_it() {
    let (mut tree, _, right) = left_and_right();
    tree.resize(right, Direction::Left, 10, TAB).unwrap();
    assert_eq!(widths(&tree, TAB), vec![40, 60]);
}

#[test]
fn a_border_stops_at_the_minimum_size() {
    let (mut tree, left, _) = left_and_right();
    tree.resize(left, Direction::Right, 1000, TAB).unwrap();
    assert_eq!(widths(&tree, TAB), vec![90, 10]);
    tree.resize(left, Direction::Right, -1000, TAB).unwrap();
    assert_eq!(widths(&tree, TAB), vec![10, 90]);
}

#[test]
fn a_pane_has_no_border_on_the_edge_of_the_tab() {
    let (mut tree, left, right) = left_and_right();
    assert_eq!(
        tree.resize(left, Direction::Left, 5, TAB),
        Err(LayoutError::NoBorder)
    );
    assert_eq!(
        tree.resize(right, Direction::Up, 5, TAB),
        Err(LayoutError::NoBorder)
    );
    assert_eq!(widths(&tree, TAB), vec![50, 50]);
}

#[test]
fn the_nearest_border_on_that_side_moves() {
    let (mut tree, left, right) = left_and_right();
    let bottom = tree.split(right, SplitDirection::Down, TAB).unwrap();
    tree.resize(bottom, Direction::Up, 5, TAB).unwrap();
    tree.resize(bottom, Direction::Left, 10, TAB).unwrap();
    assert_eq!(
        tree.layout(TAB),
        vec![
            (left, Rect::new(0, 0, 40, 40)),
            (right, Rect::new(40, 0, 60, 15)),
            (bottom, Rect::new(40, 15, 60, 25)),
        ]
    );
}

#[test]
fn the_shares_are_kept_when_the_tab_grows() {
    let (mut tree, left, _) = left_and_right();
    tree.resize(left, Direction::Right, 10, TAB).unwrap();
    assert_eq!(widths(&tree, Size::new(200, 40)), vec![120, 80]);
}

#[test]
fn resizing_an_unknown_pane_is_refused() {
    let (mut tree, left, right) = left_and_right();
    tree.close(right, TAB).unwrap();
    assert_eq!(
        tree.resize(right, Direction::Left, 5, TAB),
        Err(LayoutError::UnknownPane(right))
    );
    assert_eq!(widths(&tree, TAB), vec![100]);
    assert!(tree.contains(left));
}

#[test]
fn resize_towards_moves_the_border_on_that_side_or_the_opposite_side() {
    let (mut tree, left, right) = left_and_right();
    // Moving right from the left pane widens the left pane.
    tree.resize_towards(left, Direction::Right, 10, TAB)
        .unwrap();
    assert_eq!(widths(&tree, TAB), vec![60, 40]);

    // Moving left from the left pane has no left border, so it pulls the
    // right border left and narrows the left pane.
    tree.resize_towards(left, Direction::Left, 20, TAB).unwrap();
    assert_eq!(widths(&tree, TAB), vec![40, 60]);

    // Moving right from the right pane has no right border, so it pushes the
    // left border right and widens the left pane.
    tree.resize_towards(right, Direction::Right, 15, TAB)
        .unwrap();
    assert_eq!(widths(&tree, TAB), vec![55, 45]);

    // Moving left from the right pane widens the right pane.
    tree.resize_towards(right, Direction::Left, 15, TAB)
        .unwrap();
    assert_eq!(widths(&tree, TAB), vec![40, 60]);
}

#[test]
fn resize_towards_stops_at_the_minimum_size_and_refuses_a_lone_pane() {
    let (mut tree, left, right) = left_and_right();
    tree.resize_towards(left, Direction::Right, 1000, TAB)
        .unwrap();
    assert_eq!(widths(&tree, TAB), vec![90, 10]);
    tree.resize_towards(left, Direction::Left, 1000, TAB)
        .unwrap();
    assert_eq!(widths(&tree, TAB), vec![10, 90]);

    // A lone pane has no border on either side.
    tree.close(right, TAB).unwrap();
    assert_eq!(
        tree.resize_towards(left, Direction::Right, 10, TAB),
        Err(LayoutError::NoBorder)
    );
    assert_eq!(
        tree.resize_towards(left, Direction::Down, 10, TAB),
        Err(LayoutError::NoBorder)
    );
}

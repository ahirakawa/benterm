//! Choosing the pane next to another one.

use benterm_layout::{Direction, PaneTree, Size, SplitDirection};

const MIN: Size = Size::new(10, 4);
const TAB: Size = Size::new(100, 40);

#[test]
fn the_panes_of_two_halves_are_next_to_each_other() {
    let mut tree = PaneTree::new(MIN);
    let left = tree.focused();
    let right = tree.split(left, SplitDirection::Right, TAB).unwrap();
    assert_eq!(tree.neighbor(left, Direction::Right, TAB), Some(right));
    assert_eq!(tree.neighbor(right, Direction::Left, TAB), Some(left));
    assert_eq!(tree.neighbor(left, Direction::Left, TAB), None);
    assert_eq!(tree.neighbor(left, Direction::Up, TAB), None);
    assert_eq!(tree.neighbor(right, Direction::Down, TAB), None);
}

#[test]
fn the_pane_sharing_more_of_the_edge_wins() {
    let mut tree = PaneTree::new(MIN);
    let left = tree.focused();
    let top = tree.split(left, SplitDirection::Right, TAB).unwrap();
    let bottom = tree.split(top, SplitDirection::Down, TAB).unwrap();
    tree.resize(bottom, Direction::Up, 10, TAB).unwrap();
    // top is 10 high and bottom 30, both to the right of left.
    assert_eq!(tree.neighbor(left, Direction::Right, TAB), Some(bottom));
    assert_eq!(tree.neighbor(top, Direction::Left, TAB), Some(left));
    assert_eq!(tree.neighbor(bottom, Direction::Up, TAB), Some(top));
}

#[test]
fn on_a_tie_the_pane_nearer_the_top_wins() {
    let mut tree = PaneTree::new(MIN);
    let left = tree.focused();
    let top = tree.split(left, SplitDirection::Right, TAB).unwrap();
    tree.split(top, SplitDirection::Down, TAB).unwrap();
    assert_eq!(tree.neighbor(left, Direction::Right, TAB), Some(top));
}

#[test]
fn a_pane_across_a_border_is_found_beyond_its_split() {
    let mut tree = PaneTree::new(MIN);
    let top_left = tree.focused();
    let right = tree.split(top_left, SplitDirection::Right, TAB).unwrap();
    let bottom_left = tree.split(top_left, SplitDirection::Down, TAB).unwrap();
    let bottom_right = tree.split(right, SplitDirection::Down, TAB).unwrap();
    assert_eq!(
        tree.neighbor(bottom_left, Direction::Right, TAB),
        Some(bottom_right)
    );
    assert_eq!(
        tree.neighbor(bottom_right, Direction::Left, TAB),
        Some(bottom_left)
    );
    assert_eq!(
        tree.neighbor(right, Direction::Down, TAB),
        Some(bottom_right)
    );
}

#[test]
fn focusing_a_neighbour_moves_the_focus() {
    let mut tree = PaneTree::new(MIN);
    let left = tree.focused();
    let right = tree.split(left, SplitDirection::Right, TAB).unwrap();
    assert_eq!(tree.focus_neighbor(Direction::Left, TAB), Some(left));
    assert_eq!(tree.focused(), left);
    assert_eq!(tree.focus_neighbor(Direction::Left, TAB), None);
    assert_eq!(tree.focused(), left);
    assert_eq!(tree.focus_neighbor(Direction::Right, TAB), Some(right));
}

#[test]
fn an_unknown_pane_has_no_neighbour() {
    let mut tree = PaneTree::new(MIN);
    let left = tree.focused();
    let right = tree.split(left, SplitDirection::Right, TAB).unwrap();
    tree.close(right, TAB).unwrap();
    assert_eq!(tree.neighbor(right, Direction::Left, TAB), None);
}

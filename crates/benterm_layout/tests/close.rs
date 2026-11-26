//! Closing panes.

use benterm_layout::{LayoutError, PaneTree, Rect, Size, SplitDirection};

const MIN: Size = Size::new(10, 4);
const TAB: Size = Size::new(100, 40);

#[test]
fn closing_one_of_two_panes_gives_the_other_the_whole_tab() {
    let mut tree = PaneTree::new(MIN);
    let left = tree.focused();
    let right = tree.split(left, SplitDirection::Right, TAB).unwrap();
    tree.close(right, TAB).unwrap();
    assert_eq!(tree.layout(TAB), vec![(left, Rect::new(0, 0, 100, 40))]);
    assert_eq!(tree.focused(), left);
    assert!(!tree.contains(right));
    assert_eq!(tree.min_size(), MIN);
}

#[test]
fn closing_a_pane_widens_its_neighbour_and_keeps_the_others() {
    let mut tree = PaneTree::new(MIN);
    let left = tree.focused();
    let top = tree.split(left, SplitDirection::Right, TAB).unwrap();
    let bottom = tree.split(top, SplitDirection::Down, TAB).unwrap();
    let middle = tree.split(top, SplitDirection::Right, TAB).unwrap();
    // left | (top | middle) over bottom
    assert_eq!(tree.layout(TAB)[1], (top, Rect::new(50, 0, 25, 20)));
    tree.close(middle, TAB).unwrap();
    assert_eq!(
        tree.layout(TAB),
        vec![
            (left, Rect::new(0, 0, 50, 40)),
            (top, Rect::new(50, 0, 50, 20)),
            (bottom, Rect::new(50, 20, 50, 20)),
        ]
    );
    assert_eq!(tree.focused(), top);
}

#[test]
fn closing_a_pane_that_is_not_focused_keeps_the_focus() {
    let mut tree = PaneTree::new(MIN);
    let left = tree.focused();
    let right = tree.split(left, SplitDirection::Right, TAB).unwrap();
    tree.close(left, TAB).unwrap();
    assert_eq!(tree.focused(), right);
    assert_eq!(tree.layout(TAB), vec![(right, Rect::new(0, 0, 100, 40))]);
}

#[test]
fn the_focus_moves_to_the_pane_beside_the_closed_one() {
    let mut tree = PaneTree::new(MIN);
    let left = tree.focused();
    let right = tree.split(left, SplitDirection::Right, TAB).unwrap();
    let lower_left = tree.split(left, SplitDirection::Down, TAB).unwrap();
    // (left over lower_left) | right
    tree.focus(right).unwrap();
    tree.close(right, TAB).unwrap();
    // Both panes on the left touch the closed one; the tree picks the last.
    assert_eq!(tree.focused(), lower_left);
}

#[test]
fn closing_keeps_the_borders_that_are_on_the_screen() {
    let mut tree = PaneTree::new(MIN);
    let left = tree.focused();
    let middle = tree.split(left, SplitDirection::Right, TAB).unwrap();
    let right = tree.split(middle, SplitDirection::Right, TAB).unwrap();
    // In a narrow tab the left pane is held at its minimum width, although
    // its share is a half.
    let narrow = Size::new(30, 40);
    assert_eq!(tree.layout(narrow)[0], (left, Rect::new(0, 0, 10, 40)));
    tree.close(right, narrow).unwrap();
    assert_eq!(
        tree.layout(narrow),
        vec![
            (left, Rect::new(0, 0, 10, 40)),
            (middle, Rect::new(10, 0, 20, 40)),
        ]
    );
}

#[test]
fn the_last_pane_cannot_be_closed() {
    let mut tree = PaneTree::new(MIN);
    let only = tree.focused();
    assert_eq!(tree.close(only, TAB), Err(LayoutError::LastPane));
    assert_eq!(tree.panes(), vec![only]);
}

#[test]
fn closing_an_unknown_pane_is_refused() {
    let mut tree = PaneTree::new(MIN);
    let left = tree.focused();
    let right = tree.split(left, SplitDirection::Right, TAB).unwrap();
    tree.close(right, TAB).unwrap();
    assert_eq!(tree.close(right, TAB), Err(LayoutError::UnknownPane(right)));
}

#[test]
fn the_id_of_a_closed_pane_is_not_handed_out_again() {
    let mut tree = PaneTree::new(MIN);
    let left = tree.focused();
    let right = tree.split(left, SplitDirection::Right, TAB).unwrap();
    tree.close(right, TAB).unwrap();
    let again = tree.split(left, SplitDirection::Right, TAB).unwrap();
    assert_ne!(again, right);
    assert_ne!(again, left);
}

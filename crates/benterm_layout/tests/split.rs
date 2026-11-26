//! Splitting panes and laying them out.

use benterm_layout::{LayoutError, PaneTree, Rect, Size, SplitDirection};

const MIN: Size = Size::new(10, 4);
const TAB: Size = Size::new(100, 40);

#[test]
fn a_new_tree_has_one_focused_pane_covering_the_tab() {
    let tree = PaneTree::new(MIN);
    let panes = tree.panes();
    assert_eq!(panes.len(), 1);
    assert_eq!(tree.focused(), panes[0]);
    assert_eq!(tree.layout(TAB), vec![(panes[0], Rect::new(0, 0, 100, 40))]);
    assert_eq!(tree.min_size(), MIN);
}

#[test]
fn splitting_right_puts_the_new_focused_pane_in_the_right_half() {
    let mut tree = PaneTree::new(MIN);
    let first = tree.focused();
    let second = tree.split(first, SplitDirection::Right, TAB).unwrap();
    assert_ne!(first, second);
    assert_eq!(tree.focused(), second);
    assert_eq!(
        tree.layout(TAB),
        vec![
            (first, Rect::new(0, 0, 50, 40)),
            (second, Rect::new(50, 0, 50, 40)),
        ]
    );
    assert_eq!(tree.min_size(), Size::new(20, 4));
}

#[test]
fn splitting_down_puts_the_new_pane_in_the_bottom_half() {
    let mut tree = PaneTree::new(MIN);
    let first = tree.focused();
    let second = tree.split(first, SplitDirection::Down, TAB).unwrap();
    assert_eq!(
        tree.layout(TAB),
        vec![
            (first, Rect::new(0, 0, 100, 20)),
            (second, Rect::new(0, 20, 100, 20)),
        ]
    );
    assert_eq!(tree.min_size(), Size::new(10, 8));
}

#[test]
fn splitting_a_half_again_splits_only_that_half() {
    let mut tree = PaneTree::new(MIN);
    let left = tree.focused();
    let right = tree.split(left, SplitDirection::Right, TAB).unwrap();
    let bottom = tree.split(right, SplitDirection::Down, TAB).unwrap();
    assert_eq!(
        tree.layout(TAB),
        vec![
            (left, Rect::new(0, 0, 50, 40)),
            (right, Rect::new(50, 0, 50, 20)),
            (bottom, Rect::new(50, 20, 50, 20)),
        ]
    );
    assert_eq!(tree.min_size(), Size::new(20, 8));
}

#[test]
fn an_odd_size_is_shared_without_a_gap() {
    let mut tree = PaneTree::new(MIN);
    let first = tree.focused();
    let second = tree.split(first, SplitDirection::Right, TAB).unwrap();
    let layout = tree.layout(Size::new(101, 40));
    assert_eq!(layout[0].0, first);
    assert_eq!(layout[1].0, second);
    assert_eq!(layout[0].1.width + layout[1].1.width, 101);
    assert_eq!(layout[1].1.x, layout[0].1.width);
}

#[test]
fn a_split_that_would_go_below_the_minimum_is_refused() {
    let mut tree = PaneTree::new(MIN);
    let first = tree.focused();
    let small = Size::new(19, 40);
    assert_eq!(
        tree.split(first, SplitDirection::Right, small),
        Err(LayoutError::TooSmall)
    );
    assert_eq!(tree.panes(), vec![first]);
    assert!(tree.split(first, SplitDirection::Down, small).is_ok());
}

#[test]
fn splitting_an_unknown_pane_is_refused() {
    let mut tree = PaneTree::new(MIN);
    let first = tree.focused();
    // A copy hands out the id that the tree itself has not used yet.
    let missing = tree
        .clone()
        .split(first, SplitDirection::Right, TAB)
        .unwrap();
    assert!(!tree.contains(missing));
    assert_eq!(
        tree.split(missing, SplitDirection::Down, TAB),
        Err(LayoutError::UnknownPane(missing))
    );
    assert_eq!(tree.panes(), vec![first]);
}

#[test]
fn focusing_a_pane_changes_the_focus() {
    let mut tree = PaneTree::new(MIN);
    let first = tree.focused();
    tree.split(first, SplitDirection::Right, TAB).unwrap();
    tree.focus(first).unwrap();
    assert_eq!(tree.focused(), first);
}

#[test]
fn a_tab_smaller_than_the_minimum_is_still_covered_exactly() {
    let mut tree = PaneTree::new(MIN);
    let first = tree.focused();
    let second = tree.split(first, SplitDirection::Right, TAB).unwrap();
    let layout = tree.layout(Size::new(5, 2));
    assert_eq!(layout[0].0, first);
    assert_eq!(layout[1].0, second);
    assert_eq!(layout[0].1.width + layout[1].1.width, 5);
    assert!(layout.iter().all(|(_, r)| r.height == 2));
}

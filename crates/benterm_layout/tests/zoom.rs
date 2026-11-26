//! Temporarily zooming one pane to fill the tab, and returning to the splits.

use benterm_layout::{Direction, PaneTree, Rect, Size, SplitDirection};

const MIN: Size = Size::new(10, 4);
const TAB: Size = Size::new(100, 40);

#[test]
fn toggling_zoom_fills_the_tab_with_the_focused_pane_and_restores_the_splits() {
    let mut tree = PaneTree::new(MIN);
    let left = tree.focused();
    let right = tree.split(left, SplitDirection::Right, TAB).unwrap();
    tree.resize(left, Direction::Right, 15, TAB).unwrap();
    let before = tree.layout(TAB);
    assert_eq!(
        before,
        vec![
            (left, Rect::new(0, 0, 65, 40)),
            (right, Rect::new(65, 0, 35, 40)),
        ]
    );
    assert_eq!(tree.zoomed(), None);

    // Zooming the focused pane shows only it at the full tab size and hides
    // the split borders.
    assert_eq!(tree.toggle_zoom(), Some(right));
    assert_eq!(tree.zoomed(), Some(right));
    assert_eq!(tree.layout(TAB), vec![(right, Rect::new(0, 0, 100, 40))]);
    assert!(tree.borders(TAB).is_empty());
    // All panes remain in the tree, and the saved shape keeps the original splits.
    assert_eq!(tree.panes(), vec![left, right]);

    // Toggling zoom again restores the exact original ratios and borders.
    assert_eq!(tree.toggle_zoom(), None);
    assert_eq!(tree.zoomed(), None);
    assert_eq!(tree.layout(TAB), before);
    assert_eq!(tree.borders(TAB).len(), 1);
}

#[test]
fn zooming_a_single_pane_does_nothing() {
    let mut tree = PaneTree::new(MIN);
    let only = tree.focused();
    assert_eq!(tree.toggle_zoom(), None);
    assert_eq!(tree.zoomed(), None);
    assert_eq!(tree.layout(TAB), vec![(only, Rect::new(0, 0, 100, 40))]);
}

#[test]
fn focusing_a_neighbor_while_zoomed_unzooms_and_moves_focus() {
    let mut tree = PaneTree::new(MIN);
    let left = tree.focused();
    let right = tree.split(left, SplitDirection::Right, TAB).unwrap();
    tree.toggle_zoom();
    assert_eq!(tree.zoomed(), Some(right));

    // Moving towards an edge with no neighbor keeps zoom active.
    assert_eq!(tree.focus_neighbor(Direction::Right, TAB), None);
    assert_eq!(tree.zoomed(), Some(right));
    assert_eq!(tree.focused(), right);

    // Moving to a real neighbor unzooms and focuses that neighbor.
    assert_eq!(tree.focus_neighbor(Direction::Left, TAB), Some(left));
    assert_eq!(tree.zoomed(), None);
    assert_eq!(tree.focused(), left);
    assert_eq!(tree.layout(TAB).len(), 2);
}

#[test]
fn focusing_splitting_or_closing_the_zoomed_pane_unzooms() {
    let mut tree = PaneTree::new(MIN);
    let left = tree.focused();
    let right = tree.split(left, SplitDirection::Right, TAB).unwrap();

    // Direct focus of another pane unzooms.
    tree.toggle_zoom();
    assert_eq!(tree.zoomed(), Some(right));
    tree.focus(left).unwrap();
    assert_eq!(tree.zoomed(), None);

    // Splitting while zoomed unzooms so the new pane is visible.
    tree.toggle_zoom();
    assert_eq!(tree.zoomed(), Some(left));
    let bottom = tree.split(left, SplitDirection::Down, TAB).unwrap();
    assert_eq!(tree.zoomed(), None);
    assert_eq!(tree.focused(), bottom);

    // Closing the zoomed pane unzooms.
    tree.toggle_zoom();
    assert_eq!(tree.zoomed(), Some(bottom));
    tree.close(bottom, TAB).unwrap();
    assert_eq!(tree.zoomed(), None);

    // Closing another pane so only one remains also unzooms.
    tree.toggle_zoom();
    assert_eq!(tree.zoomed(), Some(left));
    tree.close(right, TAB).unwrap();
    assert_eq!(tree.zoomed(), None);
}

#[test]
fn shape_and_from_shape_do_not_include_zoom() {
    let mut tree = PaneTree::new(MIN);
    let left = tree.focused();
    let _right = tree.split(left, SplitDirection::Right, TAB).unwrap();
    tree.resize(left, Direction::Right, 20, TAB).unwrap();
    let shape_before = tree.shape();

    tree.toggle_zoom();
    assert!(tree.zoomed().is_some());
    let shape_zoomed = tree.shape();
    assert_eq!(shape_zoomed, shape_before);

    let (restored, _) = PaneTree::from_shape(shape_zoomed, 1, MIN);
    assert_eq!(restored.zoomed(), None);
    let widths: Vec<u32> = restored
        .layout(TAB)
        .into_iter()
        .map(|(_, r)| r.width)
        .collect();
    assert_eq!(widths, vec![70, 30]);
}

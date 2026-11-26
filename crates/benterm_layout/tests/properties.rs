//! Properties that hold after any sequence of operations on a tree.

use std::collections::HashSet;

use benterm_layout::{Direction, LayoutError, PaneId, PaneTree, Rect, Size, SplitDirection};
use proptest::collection::vec;
use proptest::prelude::*;
use proptest::sample::Index;

const MIN: Size = Size::new(4, 2);

#[derive(Clone, Debug)]
enum Op {
    Split(Index, SplitDirection),
    Close(Index),
    Resize(Index, Direction, i32),
    FocusNeighbor(Direction),
    Focus(Index),
    ResizeTab(Size),
}

fn split_direction() -> impl Strategy<Value = SplitDirection> {
    prop_oneof![Just(SplitDirection::Right), Just(SplitDirection::Down)]
}

fn direction() -> impl Strategy<Value = Direction> {
    prop_oneof![
        Just(Direction::Left),
        Just(Direction::Right),
        Just(Direction::Up),
        Just(Direction::Down),
    ]
}

fn tab_size() -> impl Strategy<Value = Size> {
    (1u32..200, 1u32..100).prop_map(|(w, h)| Size::new(w, h))
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        3 => (any::<Index>(), split_direction()).prop_map(|(i, d)| Op::Split(i, d)),
        2 => any::<Index>().prop_map(Op::Close),
        2 => (any::<Index>(), direction(), -60i32..60).prop_map(|(i, d, n)| Op::Resize(i, d, n)),
        2 => direction().prop_map(Op::FocusNeighbor),
        1 => any::<Index>().prop_map(Op::Focus),
        1 => tab_size().prop_map(Op::ResizeTab),
    ]
}

fn area(r: &Rect) -> u64 {
    u64::from(r.width) * u64::from(r.height)
}

fn overlap(a: &Rect, b: &Rect) -> bool {
    a.x < b.x + b.width && b.x < a.x + a.width && a.y < b.y + b.height && b.y < a.y + a.height
}

/// Checks that the layout covers the tab exactly, and that every pane has at
/// least the minimum size when the tab is big enough.
fn check_layout(tree: &PaneTree, tab: Size) -> Result<(), TestCaseError> {
    let layout = tree.layout(tab);
    let ids: Vec<PaneId> = layout.iter().map(|(id, _)| *id).collect();
    prop_assert_eq!(&ids, &tree.panes());
    prop_assert!(tree.contains(tree.focused()));
    for (_, r) in &layout {
        prop_assert!(r.x + r.width <= tab.width && r.y + r.height <= tab.height);
    }
    let total: u64 = layout.iter().map(|(_, r)| area(r)).sum();
    prop_assert_eq!(total, u64::from(tab.width) * u64::from(tab.height));
    for (i, (_, a)) in layout.iter().enumerate() {
        for (_, b) in &layout[i + 1..] {
            prop_assert!(!overlap(a, b), "{:?} overlaps {:?}", a, b);
        }
    }
    if fits(tree, tab) {
        for (id, r) in &layout {
            prop_assert!(
                r.width >= MIN.width && r.height >= MIN.height,
                "pane {} is {:?} in {:?}",
                id,
                r,
                tab
            );
        }
    }
    Ok(())
}

fn rect_of(layout: &[(PaneId, Rect)], id: PaneId) -> Rect {
    layout.iter().find(|(p, _)| *p == id).unwrap().1
}

fn touches_edge(rect: &Rect, side: Direction, tab: Size) -> bool {
    match side {
        Direction::Left => rect.x == 0,
        Direction::Right => rect.x + rect.width == tab.width,
        Direction::Up => rect.y == 0,
        Direction::Down => rect.y + rect.height == tab.height,
    }
}

/// Whether `to` shares a stretch of the `direction` edge of `from`.
fn adjacent(from: &Rect, to: &Rect, direction: Direction) -> bool {
    let overlap = |a0: u32, a1: u32, b0: u32, b1: u32| a0.max(b0) < a1.min(b1);
    match direction {
        Direction::Left => {
            to.x + to.width == from.x
                && overlap(from.y, from.y + from.height, to.y, to.y + to.height)
        }
        Direction::Right => {
            from.x + from.width == to.x
                && overlap(from.y, from.y + from.height, to.y, to.y + to.height)
        }
        Direction::Up => {
            to.y + to.height == from.y
                && overlap(from.x, from.x + from.width, to.x, to.x + to.width)
        }
        Direction::Down => {
            from.y + from.height == to.y
                && overlap(from.x, from.x + from.width, to.x, to.x + to.width)
        }
    }
}

fn fits(tree: &PaneTree, tab: Size) -> bool {
    let min = tree.min_size();
    tab.width >= min.width && tab.height >= min.height
}

/// Checks that the space of the closed pane went to the panes whose
/// rectangles changed, and that those panes took no space from the panes that
/// stayed. When the tab was too small for the minimum sizes before the close,
/// the other panes may move, so only that something changed is checked.
fn check_close(
    before: &[(PaneId, Rect)],
    closed: PaneId,
    after: &[(PaneId, Rect)],
    fitted: bool,
) -> Result<(), TestCaseError> {
    let closed_rect = before.iter().find(|(id, _)| *id == closed).unwrap().1;
    let old = |id: PaneId| before.iter().find(|(b, _)| *b == id).map(|(_, r)| *r);
    let mut changed = HashSet::new();
    for (id, new) in after {
        let old = old(*id).expect("no pane appears on close");
        if old != *new {
            changed.insert(*id);
        }
    }
    if area(&closed_rect) == 0 {
        return Ok(());
    }
    prop_assert!(
        !changed.is_empty(),
        "nothing took the space of the closed pane"
    );
    if !fitted {
        return Ok(());
    }
    // A cell may change hands only between the closed pane and the panes
    // that changed.
    let owner_before = |x: u32, y: u32| {
        before
            .iter()
            .find(|(_, r)| r.x <= x && x < r.x + r.width && r.y <= y && y < r.y + r.height)
            .map(|(id, _)| *id)
            .unwrap()
    };
    for (id, new) in after.iter().filter(|(id, _)| changed.contains(id)) {
        for y in new.y..new.y + new.height {
            for x in new.x..new.x + new.width {
                let previous = owner_before(x, y);
                prop_assert!(
                    previous == *id || previous == closed || changed.contains(&previous),
                    "pane {} took ({}, {}) from pane {}, which did not change",
                    id,
                    x,
                    y,
                    previous
                );
            }
        }
    }
    Ok(())
}

/// Checks that there is one border per split, that each lies on the edge of
/// its pane, and that dragging it moves it.
fn check_borders(tree: &PaneTree, tab: Size) -> Result<(), TestCaseError> {
    let borders = tree.borders(tab);
    prop_assert_eq!(borders.len(), tree.panes().len() - 1);
    if !fits(tree, tab) {
        return Ok(());
    }
    let layout = tree.layout(tab);
    for border in &borders {
        let rect = rect_of(&layout, border.pane);
        let line = border.line;
        match border.side {
            Direction::Right => {
                prop_assert_eq!(line.width, 0);
                prop_assert_eq!(line.x, rect.x + rect.width);
                prop_assert!(line.y <= rect.y && rect.y + rect.height <= line.y + line.height);
            }
            Direction::Down => {
                prop_assert_eq!(line.height, 0);
                prop_assert_eq!(line.y, rect.y + rect.height);
                prop_assert!(line.x <= rect.x && rect.x + rect.width <= line.x + line.width);
            }
            side => prop_assert!(false, "a border on the {:?} side", side),
        }
        let mut dragged = tree.clone();
        prop_assert_eq!(dragged.resize(border.pane, border.side, 0, tab), Ok(()));
        prop_assert!(dragged.borders(tab).contains(border));
    }
    Ok(())
}

/// Runs the operations, checking the layout after each one and that no id is
/// handed out twice.
fn run(start: Size, ops: &[Op]) -> Result<(), TestCaseError> {
    let mut tree = PaneTree::new(MIN);
    let mut tab = start;
    let mut issued: HashSet<PaneId> = tree.panes().into_iter().collect();
    check_layout(&tree, tab)?;
    check_borders(&tree, tab)?;
    for op in ops {
        match op {
            Op::Split(index, direction) => {
                let panes = tree.panes();
                let target = *index.get(&panes);
                if let Ok(new) = tree.split(target, *direction, tab) {
                    prop_assert!(issued.insert(new), "id {} was handed out twice", new);
                    prop_assert_eq!(tree.focused(), new);
                }
            }
            Op::Close(index) => {
                let panes = tree.panes();
                let target = *index.get(&panes);
                let before = tree.layout(tab);
                let fitted = fits(&tree, tab);
                match tree.close(target, tab) {
                    Ok(()) => {
                        prop_assert!(panes.len() > 1);
                        prop_assert!(!tree.contains(target));
                        check_close(&before, target, &tree.layout(tab), fitted)?;
                    }
                    Err(_) => prop_assert_eq!(panes.len(), 1),
                }
            }
            Op::Resize(index, side, delta) => {
                let panes = tree.panes();
                let target = *index.get(&panes);
                let before = rect_of(&tree.layout(tab), target);
                let fitted = fits(&tree, tab);
                let result = tree.resize(target, *side, *delta, tab);
                // In a tab that is too small, empty panes make the edges
                // ambiguous.
                if fitted {
                    prop_assert_eq!(
                        result == Err(LayoutError::NoBorder),
                        touches_edge(&before, *side, tab)
                    );
                }
                if result.is_ok() && fitted {
                    let after = rect_of(&tree.layout(tab), target);
                    let (old, new) = match side {
                        Direction::Left | Direction::Right => (before.width, after.width),
                        Direction::Up | Direction::Down => (before.height, after.height),
                    };
                    prop_assert!(
                        (*delta >= 0 && new >= old) || (*delta <= 0 && new <= old),
                        "moving by {} changed the pane from {} to {}",
                        delta,
                        old,
                        new
                    );
                }
            }
            Op::FocusNeighbor(direction) => {
                let from = tree.focused();
                let layout = tree.layout(tab);
                let from_rect = rect_of(&layout, from);
                let adjacent: Vec<PaneId> = layout
                    .iter()
                    .filter(|(_, r)| adjacent(&from_rect, r, *direction))
                    .map(|(id, _)| *id)
                    .collect();
                let result = tree.focus_neighbor(*direction, tab);
                match result {
                    Some(to) => {
                        prop_assert!(tree.contains(to));
                        prop_assert_eq!(tree.focused(), to);
                        prop_assert!(adjacent.contains(&to), "{} is not next to {}", to, from);
                    }
                    None => {
                        prop_assert_eq!(tree.focused(), from);
                        prop_assert!(adjacent.is_empty(), "{:?} were next to {}", adjacent, from);
                    }
                }
                if fits(&tree, tab) {
                    prop_assert_eq!(result.is_none(), touches_edge(&from_rect, *direction, tab));
                }
            }
            Op::Focus(index) => {
                let panes = tree.panes();
                let target = *index.get(&panes);
                tree.focus(target).unwrap();
                prop_assert_eq!(tree.focused(), target);
            }
            Op::ResizeTab(size) => tab = *size,
        }
        check_layout(&tree, tab)?;
        check_borders(&tree, tab)?;
    }
    Ok(())
}

proptest! {
    #[test]
    fn the_layout_covers_the_tab_after_any_operations(
        start in tab_size(),
        ops in vec(op(), 0..60),
    ) {
        run(start, &ops)?;
    }

    #[test]
    fn a_split_succeeds_exactly_when_the_tab_stays_big_enough(
        start in tab_size(),
        ops in vec(op(), 0..30),
        index in any::<Index>(),
        direction in split_direction(),
    ) {
        let mut tree = PaneTree::new(MIN);
        let mut tab = start;
        for op in &ops {
            match op {
                Op::Split(i, d) => {
                    let panes = tree.panes();
                    let _ = tree.split(*i.get(&panes), *d, tab);
                }
                Op::Close(i) => {
                    let panes = tree.panes();
                    let _ = tree.close(*i.get(&panes), tab);
                }
                Op::Resize(i, side, delta) => {
                    let panes = tree.panes();
                    let _ = tree.resize(*i.get(&panes), *side, *delta, tab);
                }
                Op::FocusNeighbor(direction) => {
                    tree.focus_neighbor(*direction, tab);
                }
                Op::Focus(i) => {
                    let panes = tree.panes();
                    tree.focus(*i.get(&panes)).unwrap();
                }
                Op::ResizeTab(size) => tab = *size,
            }
        }
        let before = tree.panes();
        let mut grown = tree.clone();
        match tree.split(*index.get(&before), direction, tab) {
            Ok(_) => {
                let min = tree.min_size();
                prop_assert!(tab.width >= min.width && tab.height >= min.height);
            }
            Err(_) => {
                prop_assert_eq!(tree.panes(), before);
                // The same split with no size limit would need more room.
                let huge = Size::new(u32::MAX / 2, u32::MAX / 2);
                grown.split(*index.get(&tree.panes()), direction, huge).unwrap();
                let min = grown.min_size();
                prop_assert!(tab.width < min.width || tab.height < min.height);
            }
        }
    }
}

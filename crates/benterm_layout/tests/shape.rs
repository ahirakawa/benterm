//! A tree, a tab list and a workspace list can be taken apart into plain
//! values and built again from them, which is what saving a session needs.

use benterm_layout::{
    Direction, PaneId, PaneTree, Size, SplitDirection, TabList, TreeShape, WorkspaceColor,
    WorkspaceList,
};
use proptest::collection::vec;
use proptest::prelude::*;
use proptest::sample::Index;

const MIN: Size = Size::new(4, 2);
const TAB: Size = Size::new(200, 100);

fn pane<P>(content: P) -> Box<TreeShape<P>> {
    Box::new(TreeShape::Pane(content))
}

#[test]
fn the_shape_of_a_new_tree_is_one_pane() {
    let tree = PaneTree::new(MIN);
    assert_eq!(tree.shape(), TreeShape::Pane(tree.focused()));
}

#[test]
fn the_shape_names_every_split_with_its_direction_and_ratio() {
    let mut tree = PaneTree::new(MIN);
    let a = tree.focused();
    let b = tree.split(a, SplitDirection::Right, TAB).unwrap();
    let c = tree.split(b, SplitDirection::Down, TAB).unwrap();
    tree.resize(a, Direction::Right, -50, TAB).unwrap();
    assert_eq!(
        tree.shape(),
        TreeShape::Split {
            direction: SplitDirection::Right,
            ratio: 0.25,
            first: pane(a),
            second: Box::new(TreeShape::Split {
                direction: SplitDirection::Down,
                ratio: 0.5,
                first: pane(b),
                second: pane(c),
            }),
        }
    );
}

#[test]
fn a_tree_built_from_a_shape_has_new_panes_in_the_same_places() {
    let shape = TreeShape::Split {
        direction: SplitDirection::Right,
        ratio: 0.25,
        first: pane("a"),
        second: Box::new(TreeShape::Split {
            direction: SplitDirection::Down,
            ratio: 0.5,
            first: pane("b"),
            second: pane("c"),
        }),
    };
    let (tree, panes) = PaneTree::from_shape(shape, 1, MIN);
    let ids: Vec<PaneId> = panes.iter().map(|(id, _)| *id).collect();
    let contents: Vec<&str> = panes.iter().map(|(_, content)| *content).collect();
    assert_eq!(contents, ["a", "b", "c"]);
    assert_eq!(tree.panes(), ids);
    assert_eq!(tree.focused(), ids[1]);
    let rects: Vec<_> = tree.layout(TAB).into_iter().map(|(_, rect)| rect).collect();
    assert_eq!(
        (rects[0].width, rects[1].width, rects[1].height),
        (50, 150, 50)
    );
}

#[test]
fn panes_built_from_a_shape_have_ids_no_other_pane_has() {
    let other = PaneTree::new(MIN);
    let (tree, _) = PaneTree::from_shape(TreeShape::Pane(()), 0, MIN);
    assert_ne!(tree.focused(), other.focused());
}

#[test]
fn a_focus_past_the_last_pane_falls_on_the_last_pane() {
    let shape = TreeShape::Split {
        direction: SplitDirection::Right,
        ratio: 0.5,
        first: pane(()),
        second: pane(()),
    };
    let (tree, _) = PaneTree::from_shape(shape, 7, MIN);
    assert_eq!(tree.focused(), tree.panes()[1]);
}

#[test]
fn a_ratio_that_is_no_share_is_replaced() {
    for (ratio, width) in [(f64::NAN, 100), (f64::INFINITY, 100), (-3.0, 4), (7.5, 196)] {
        let shape = TreeShape::Split {
            direction: SplitDirection::Right,
            ratio,
            first: pane(()),
            second: pane(()),
        };
        let (tree, _) = PaneTree::from_shape(shape, 0, MIN);
        assert_eq!(tree.layout(TAB)[0].1.width, width, "ratio {ratio}");
        let TreeShape::Split { ratio: kept, .. } = tree.shape() else {
            panic!("the tree has a split");
        };
        assert!((0.0..=1.0).contains(&kept), "ratio {ratio} became {kept}");
    }
}

#[test]
fn map_changes_what_the_panes_hold_and_keeps_the_splits() {
    let shape = TreeShape::Split {
        direction: SplitDirection::Down,
        ratio: 0.3,
        first: pane(1),
        second: pane(2),
    };
    assert_eq!(shape.pane_count(), 2);
    assert_eq!(
        shape.map(&mut |n| n * 10),
        TreeShape::Split {
            direction: SplitDirection::Down,
            ratio: 0.3,
            first: pane(10),
            second: pane(20),
        }
    );
}

#[test]
fn tabs_are_built_in_order_with_one_selected() {
    let tabs = TabList::from_tabs(vec!['a', 'b', 'c'], 1).unwrap();
    let held: Vec<char> = tabs.iter().map(|(_, tab)| *tab).collect();
    assert_eq!(held, ['a', 'b', 'c']);
    assert_eq!(*tabs.selected_tab(), 'b');
    assert_eq!(tabs.selected_index(), 1);
}

#[test]
fn a_selection_past_the_last_tab_falls_on_the_last_tab() {
    let tabs = TabList::from_tabs(vec!['a', 'b'], 9).unwrap();
    assert_eq!(*tabs.selected_tab(), 'b');
}

#[test]
fn no_tabs_make_no_list() {
    assert!(TabList::<char>::from_tabs(Vec::new(), 0).is_none());
}

#[test]
fn workspaces_are_built_with_their_names_and_colors() {
    let color = |index| WorkspaceColor::from_index(index);
    let workspaces = WorkspaceList::from_saved(
        vec![
            (Some("api"), color(4), 'a'),
            (Some("web"), color(4), 'b'),
            (None, None, 'c'),
            (Some(" \n "), color(99), 'd'),
        ],
        1,
        7,
    )
    .unwrap();
    let names: Vec<&str> = workspaces.iter().map(|w| w.name()).collect();
    // A workspace without a name it can show is numbered by its place.
    assert_eq!(names, ["api", "web", "Workspace 3", "Workspace 4"]);
    let colors: Vec<usize> = workspaces.iter().map(|w| w.color().index()).collect();
    // A workspace without a color takes the least used one.
    assert_eq!(colors, [4, 4, 0, 1]);
    assert_eq!(*workspaces.selected_workspace().content(), 'b');
    assert_eq!(workspaces.selected_index(), 1);
    assert_eq!(workspaces.made(), 7);
}

#[test]
fn a_workspace_added_after_the_saved_ones_is_numbered_after_them() {
    let mut workspaces =
        WorkspaceList::from_saved(vec![(Some("api"), None, 0), (None, None, 1)], 0, 5).unwrap();
    let id = workspaces.add(None, 2);
    assert_eq!(workspaces.get(id).unwrap().name(), "Workspace 6");
    // A count below the number of workspaces is not believed.
    let mut workspaces =
        WorkspaceList::from_saved(vec![(Some("api"), None, 0), (Some("web"), None, 1)], 0, 0)
            .unwrap();
    let id = workspaces.add(None, 2);
    assert_eq!(workspaces.get(id).unwrap().name(), "Workspace 3");
}

#[test]
fn no_workspaces_make_no_list() {
    assert!(WorkspaceList::<char>::from_saved(Vec::new(), 0, 0).is_none());
}

#[test]
fn a_color_has_a_place_below_the_count() {
    assert_eq!(WorkspaceColor::from_index(5).map(|c| c.index()), Some(5));
    assert!(WorkspaceColor::from_index(WorkspaceColor::COUNT).is_none());
}

#[derive(Clone, Debug)]
enum Op {
    Split(Index, SplitDirection),
    Close(Index),
    Resize(Index, Direction, i32),
    Focus(Index),
}

fn op() -> impl Strategy<Value = Op> {
    let split = prop_oneof![Just(SplitDirection::Right), Just(SplitDirection::Down)];
    let side = prop_oneof![
        Just(Direction::Left),
        Just(Direction::Right),
        Just(Direction::Up),
        Just(Direction::Down),
    ];
    prop_oneof![
        3 => (any::<Index>(), split).prop_map(|(i, d)| Op::Split(i, d)),
        1 => any::<Index>().prop_map(Op::Close),
        2 => (any::<Index>(), side, -60i32..60).prop_map(|(i, d, n)| Op::Resize(i, d, n)),
        1 => any::<Index>().prop_map(Op::Focus),
    ]
}

proptest! {
    /// Whatever was done to a tree, the tree built from its shape puts
    /// its panes in the same places in a tab of any size, focuses the
    /// pane in the same place, and has the same shape again.
    #[test]
    fn a_tree_built_from_the_shape_of_a_tree_lays_out_the_same(
        ops in vec(op(), 0..40),
        (width, height) in (1u32..400, 1u32..200),
    ) {
        let mut tree = PaneTree::new(MIN);
        for op in ops {
            let panes = tree.panes();
            match op {
                Op::Split(i, direction) => {
                    let _ = tree.split(*i.get(&panes), direction, TAB);
                }
                Op::Close(i) => {
                    let _ = tree.close(*i.get(&panes), TAB);
                }
                Op::Resize(i, side, delta) => {
                    let _ = tree.resize(*i.get(&panes), side, delta, TAB);
                }
                Op::Focus(i) => tree.focus(*i.get(&panes)).unwrap(),
            }
        }
        let focused = tree.panes().iter().position(|id| *id == tree.focused()).unwrap();
        let (built, panes) = PaneTree::from_shape(tree.shape(), focused, MIN);

        let old: Vec<PaneId> = panes.iter().map(|(_, old)| *old).collect();
        prop_assert_eq!(&old, &tree.panes());
        let tab = Size::new(width, height);
        let rects = |tree: &PaneTree| -> Vec<_> {
            tree.layout(tab).into_iter().map(|(_, rect)| rect).collect()
        };
        prop_assert_eq!(rects(&built), rects(&tree));
        prop_assert_eq!(built.panes()[focused], built.focused());
        let ids: Vec<PaneId> = panes.iter().map(|(new, _)| *new).collect();
        let mut ids = ids.into_iter();
        let renamed = tree.shape().map(&mut |_| ids.next().unwrap());
        prop_assert_eq!(built.shape(), renamed);
    }
}

#[test]
fn a_count_of_workspaces_that_cannot_grow_stays_as_it_is() {
    let mut workspaces =
        WorkspaceList::from_saved(vec![(Some("api"), None, 0)], 0, u64::MAX).unwrap();
    let id = workspaces.add(None, 1);
    assert_eq!(workspaces.made(), u64::MAX);
    assert_eq!(
        workspaces.get(id).unwrap().name(),
        format!("Workspace {}", u64::MAX)
    );
}

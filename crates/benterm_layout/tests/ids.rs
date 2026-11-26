//! Pane ids stay unique across every tree in the process, so that one
//! number names one pane in the whole app (`BENTERM_PANE_ID`).

use std::collections::HashSet;

use benterm_layout::{PaneId, PaneTree, Size, SplitDirection};

const MIN: Size = Size::new(10, 4);
const TAB: Size = Size::new(400, 200);

#[test]
fn two_trees_never_share_a_pane_id() {
    let mut a = PaneTree::new(MIN);
    let mut b = PaneTree::new(MIN);
    for _ in 0..3 {
        a.split(a.focused(), SplitDirection::Right, TAB).unwrap();
        b.split(b.focused(), SplitDirection::Down, TAB).unwrap();
    }
    let a: HashSet<PaneId> = a.panes().into_iter().collect();
    let b: HashSet<PaneId> = b.panes().into_iter().collect();
    assert!(a.is_disjoint(&b), "{a:?} and {b:?} overlap");
}

#[test]
fn a_cloned_tree_gives_new_panes_ids_the_original_never_uses() {
    let mut original = PaneTree::new(MIN);
    let mut copy = original.clone();
    let from_original = original
        .split(original.focused(), SplitDirection::Right, TAB)
        .unwrap();
    let from_copy = copy
        .split(copy.focused(), SplitDirection::Right, TAB)
        .unwrap();
    assert_ne!(from_original, from_copy);
}

#[test]
fn trees_on_other_threads_never_share_a_pane_id() {
    let threads: Vec<_> = (0..4)
        .map(|_| {
            std::thread::spawn(|| {
                let mut tree = PaneTree::new(MIN);
                for _ in 0..5 {
                    tree.split(tree.focused(), SplitDirection::Right, Size::new(4000, 40))
                        .unwrap();
                }
                tree.panes()
            })
        })
        .collect();
    let mut seen = HashSet::new();
    for thread in threads {
        for id in thread.join().unwrap() {
            assert!(seen.insert(id), "{id} was issued twice");
        }
    }
}

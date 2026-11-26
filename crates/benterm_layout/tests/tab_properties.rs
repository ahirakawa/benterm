//! Properties that hold after any sequence of operations on a tab list.

use std::collections::{HashMap, HashSet};

use benterm_layout::{TabError, TabId, TabList};
use proptest::collection::vec;
use proptest::prelude::*;
use proptest::sample::Index;

#[derive(Clone, Debug)]
enum Op {
    Open,
    Close(Index),
    CloseSelected,
    Select(Index),
    Goto(usize),
    SelectLast,
    Next,
    Previous,
    Move(Index, usize),
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        3 => Just(Op::Open),
        2 => any::<Index>().prop_map(Op::Close),
        1 => Just(Op::CloseSelected),
        1 => any::<Index>().prop_map(Op::Select),
        1 => (0usize..12).prop_map(Op::Goto),
        1 => Just(Op::SelectLast),
        1 => Just(Op::Next),
        1 => Just(Op::Previous),
        2 => (any::<Index>(), 0usize..12).prop_map(|(i, p)| Op::Move(i, p)),
    ]
}

/// The list is never empty, ids are unique, the selected tab is in it, and
/// every tab still holds what it was opened with.
fn check(tabs: &TabList<u32>, held: &HashMap<TabId, u32>) -> Result<(), TestCaseError> {
    let ids = tabs.ids();
    prop_assert!(tabs.count() >= 1);
    prop_assert_eq!(ids.len(), tabs.count());
    prop_assert_eq!(ids.iter().collect::<HashSet<_>>().len(), ids.len());
    prop_assert!(ids.contains(&tabs.selected()));
    prop_assert_eq!(tabs.get(tabs.selected()), Some(tabs.selected_tab()));
    for (position, (id, tab)) in tabs.iter().enumerate() {
        prop_assert_eq!(ids[position], id);
        prop_assert_eq!(tabs.position(id), Some(position));
        prop_assert_eq!(held.get(&id), Some(tab));
    }
    prop_assert_eq!(held.len(), ids.len());
    Ok(())
}

fn without(ids: &[TabId], id: TabId) -> Vec<TabId> {
    ids.iter().copied().filter(|other| *other != id).collect()
}

fn apply(
    tabs: &mut TabList<u32>,
    held: &mut HashMap<TabId, u32>,
    next: &mut u32,
    op: Op,
) -> Result<(), TestCaseError> {
    let before = tabs.ids();
    let selected = tabs.selected();
    let at = tabs.position(selected).unwrap();
    match op {
        Op::Open => {
            *next += 1;
            let id = tabs.open(*next);
            held.insert(id, *next);
            prop_assert!(!before.contains(&id));
            prop_assert_eq!(tabs.selected(), id);
            prop_assert_eq!(tabs.position(id), Some(at + 1));
            prop_assert_eq!(without(&tabs.ids(), id), before);
        }
        Op::Close(index) => {
            let id = before[index.index(before.len())];
            close(tabs, held, id, &before, selected, at)?;
        }
        Op::CloseSelected => close(tabs, held, selected, &before, selected, at)?,
        Op::Select(index) => {
            let id = before[index.index(before.len())];
            prop_assert_eq!(tabs.select(id), Ok(()));
            prop_assert_eq!(tabs.selected(), id);
            prop_assert_eq!(tabs.ids(), before);
        }
        Op::Goto(0) => {
            prop_assert_eq!(tabs.goto(0), Err(TabError::NoTabZero));
            prop_assert_eq!(tabs.selected(), selected);
        }
        Op::Goto(number) => {
            prop_assert_eq!(tabs.goto(number), Ok(()));
            prop_assert_eq!(tabs.selected(), before[number.min(before.len()) - 1]);
        }
        Op::SelectLast => {
            tabs.select_last();
            prop_assert_eq!(tabs.selected(), *before.last().unwrap());
        }
        Op::Next => {
            tabs.select_next();
            prop_assert_eq!(tabs.selected(), before[(at + 1) % before.len()]);
            tabs.select_previous();
            prop_assert_eq!(tabs.selected(), selected);
            tabs.select_next();
        }
        Op::Previous => {
            tabs.select_previous();
            let count = before.len();
            prop_assert_eq!(tabs.selected(), before[(at + count - 1) % count]);
        }
        Op::Move(index, position) => {
            let id = before[index.index(before.len())];
            prop_assert_eq!(tabs.move_to(id, position), Ok(()));
            let after = tabs.ids();
            prop_assert_eq!(after.len(), before.len());
            prop_assert_eq!(tabs.position(id), Some(position.min(before.len() - 1)));
            prop_assert_eq!(without(&after, id), without(&before, id));
            prop_assert_eq!(tabs.selected(), selected);
        }
    }
    prop_assert_eq!(tabs.ids().len(), held.len());
    Ok(())
}

fn close(
    tabs: &mut TabList<u32>,
    held: &mut HashMap<TabId, u32>,
    id: TabId,
    before: &[TabId],
    selected: TabId,
    at: usize,
) -> Result<(), TestCaseError> {
    if before.len() == 1 {
        prop_assert_eq!(tabs.close(id), Err(TabError::LastTab));
        prop_assert_eq!(tabs.ids(), before);
        return Ok(());
    }
    prop_assert_eq!(tabs.close(id), Ok(held[&id]));
    held.remove(&id);
    let rest = without(before, id);
    prop_assert_eq!(tabs.ids(), rest.clone());
    if id == selected {
        prop_assert_eq!(tabs.selected(), rest[at.min(rest.len() - 1)]);
    } else {
        prop_assert_eq!(tabs.selected(), selected);
    }
    prop_assert_eq!(tabs.select(id), Err(TabError::UnknownTab(id)));
    Ok(())
}

proptest! {
    #[test]
    fn every_sequence_keeps_the_list_whole(ops in vec(op(), 0..60)) {
        let mut tabs = TabList::new(0);
        let mut held = HashMap::from([(tabs.selected(), 0)]);
        let mut next = 0;
        check(&tabs, &held)?;
        for op in ops {
            apply(&mut tabs, &mut held, &mut next, op)?;
            check(&tabs, &held)?;
        }
    }

    #[test]
    fn next_as_many_times_as_there_are_tabs_comes_back(
        ops in vec(op(), 0..30),
    ) {
        let mut tabs = TabList::new(0);
        let mut held = HashMap::from([(tabs.selected(), 0)]);
        let mut next = 0;
        for op in ops {
            apply(&mut tabs, &mut held, &mut next, op)?;
        }
        let start = tabs.selected();
        for _ in 0..tabs.count() {
            tabs.select_next();
        }
        prop_assert_eq!(tabs.selected(), start);
    }
}

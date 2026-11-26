//! The order and selection of tabs, by example.

use benterm_layout::{TabError, TabList};

/// A list of `n` tabs holding 0, 1, ... with the first one selected.
fn list(n: u32) -> TabList<u32> {
    let mut tabs = TabList::new(0);
    for i in 1..n {
        tabs.select_last();
        tabs.open(i);
    }
    let first = tabs.ids()[0];
    tabs.select(first).unwrap();
    tabs
}

fn contents(tabs: &TabList<u32>) -> Vec<u32> {
    tabs.iter().map(|(_, tab)| *tab).collect()
}

fn selected(tabs: &TabList<u32>) -> u32 {
    *tabs.selected_tab()
}

#[test]
fn a_new_list_has_one_selected_tab() {
    let tabs = TabList::new("first");
    assert_eq!(tabs.count(), 1);
    assert_eq!(tabs.ids(), vec![tabs.selected()]);
    assert_eq!(*tabs.selected_tab(), "first");
    assert_eq!(tabs.get(tabs.selected()), Some(&"first"));
    assert_eq!(tabs.position(tabs.selected()), Some(0));
}

#[test]
fn a_new_tab_opens_right_of_the_selected_one_and_is_selected() {
    let mut tabs = list(3);
    let id = tabs.open(9);
    assert_eq!(contents(&tabs), vec![0, 9, 1, 2]);
    assert_eq!(tabs.selected(), id);
    assert_eq!(selected(&tabs), 9);
}

#[test]
fn every_tab_gets_a_new_id() {
    let mut a = TabList::new(0);
    let b = TabList::new(0);
    let first = a.selected();
    let second = a.open(1);
    a.close(second).unwrap();
    let third = a.open(2);
    let mut ids = vec![first, second, third, b.selected()];
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), 4);
}

#[test]
fn goto_counts_from_one_and_stops_at_the_last_tab() {
    let mut tabs = list(4);
    tabs.goto(3).unwrap();
    assert_eq!(selected(&tabs), 2);
    tabs.goto(1).unwrap();
    assert_eq!(selected(&tabs), 0);
    tabs.goto(8).unwrap();
    assert_eq!(selected(&tabs), 3);
    assert_eq!(tabs.goto(0), Err(TabError::NoTabZero));
    assert_eq!(selected(&tabs), 3);
}

#[test]
fn select_last_selects_the_rightmost_tab() {
    let mut tabs = list(12);
    tabs.select_last();
    assert_eq!(selected(&tabs), 11);
}

#[test]
fn next_and_previous_wrap_around() {
    let mut tabs = list(3);
    tabs.select_previous();
    assert_eq!(selected(&tabs), 2);
    tabs.select_next();
    assert_eq!(selected(&tabs), 0);
    tabs.select_next();
    assert_eq!(selected(&tabs), 1);
}

#[test]
fn closing_the_selected_tab_selects_the_one_on_its_right() {
    let mut tabs = list(3);
    tabs.goto(2).unwrap();
    assert_eq!(tabs.close(tabs.selected()), Ok(1));
    assert_eq!(contents(&tabs), vec![0, 2]);
    assert_eq!(selected(&tabs), 2);
}

#[test]
fn closing_the_selected_rightmost_tab_selects_the_one_on_its_left() {
    let mut tabs = list(3);
    tabs.select_last();
    assert_eq!(tabs.close(tabs.selected()), Ok(2));
    assert_eq!(selected(&tabs), 1);
}

#[test]
fn closing_another_tab_keeps_the_selection() {
    let mut tabs = list(3);
    tabs.goto(2).unwrap();
    let first = tabs.ids()[0];
    assert_eq!(tabs.close(first), Ok(0));
    assert_eq!(selected(&tabs), 1);
}

#[test]
fn the_last_tab_cannot_be_closed() {
    let mut tabs = TabList::new(0);
    assert_eq!(tabs.close(tabs.selected()), Err(TabError::LastTab));
    assert_eq!(tabs.count(), 1);
}

#[test]
fn unknown_tabs_are_refused() {
    let mut tabs = list(2);
    let gone = tabs.ids()[1];
    tabs.close(gone).unwrap();
    assert_eq!(tabs.select(gone), Err(TabError::UnknownTab(gone)));
    assert_eq!(tabs.close(gone), Err(TabError::UnknownTab(gone)));
    assert_eq!(tabs.move_to(gone, 0), Err(TabError::UnknownTab(gone)));
    assert_eq!(tabs.position(gone), None);
    assert_eq!(tabs.get(gone), None);
}

#[test]
fn a_moved_tab_ends_up_at_the_position_and_stays_selected() {
    let mut tabs = list(4);
    let first = tabs.ids()[0];
    tabs.move_to(first, 2).unwrap();
    assert_eq!(contents(&tabs), vec![1, 2, 0, 3]);
    assert_eq!(tabs.selected(), first);
    let last = tabs.ids()[3];
    tabs.move_to(last, 0).unwrap();
    assert_eq!(contents(&tabs), vec![3, 1, 2, 0]);
    tabs.move_to(last, 99).unwrap();
    assert_eq!(contents(&tabs), vec![1, 2, 0, 3]);
}

#[test]
fn a_tab_has_no_name_until_it_is_given_one() {
    let mut tabs = list(2);
    let [first, second] = [tabs.ids()[0], tabs.ids()[1]];
    assert_eq!(tabs.name(first), None);
    tabs.rename(second, "review").unwrap();
    assert_eq!(tabs.name(second), Some("review"));
    assert_eq!(tabs.name(first), None);
}

#[test]
fn a_name_is_trimmed_and_an_empty_one_takes_the_name_away() {
    let mut tabs = list(1);
    let id = tabs.selected();
    tabs.rename(id, "  review ").unwrap();
    assert_eq!(tabs.name(id), Some("review"));
    tabs.rename(id, "   ").unwrap();
    assert_eq!(tabs.name(id), None);
}

#[test]
fn a_name_of_more_than_one_line_is_refused() {
    let mut tabs = list(1);
    let id = tabs.selected();
    tabs.rename(id, "review").unwrap();
    assert_eq!(tabs.rename(id, "a\nb"), Err(TabError::InvalidName));
    assert_eq!(tabs.name(id), Some("review"));
}

#[test]
fn a_name_stays_with_its_tab_when_tabs_move_or_close() {
    let mut tabs = list(3);
    let ids = tabs.ids();
    tabs.rename(ids[1], "review").unwrap();
    tabs.move_to(ids[1], 0).unwrap();
    tabs.close(ids[0]).unwrap();
    assert_eq!(tabs.name(ids[1]), Some("review"));
    assert_eq!(tabs.name(ids[2]), None);
    let unknown = ids[0];
    assert_eq!(
        tabs.rename(unknown, "x"),
        Err(TabError::UnknownTab(unknown))
    );
    assert_eq!(tabs.name(unknown), None);
}

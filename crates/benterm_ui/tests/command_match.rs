//! Which commands the palette shows for what has been typed, and in what
//! order.

use std::collections::BTreeSet;

use benterm_ui::command_match::{command_name, key_label, matches};
use proptest::prelude::*;

#[test]
fn an_action_is_named_by_its_namespace_and_its_words() {
    assert_eq!(command_name("tab::SelectNextTab"), "Tab: Select Next Tab");
    assert_eq!(command_name("pane::SplitRight"), "Pane: Split Right");
    assert_eq!(
        command_name("workspace::NewWorkspace"),
        "Workspace: New Workspace"
    );
    assert_eq!(command_name("terminal::Copy"), "Terminal: Copy");
    assert_eq!(
        command_name("command_palette::Toggle"),
        "Command Palette: Toggle"
    );
    assert_eq!(command_name("NoNamespace"), "No Namespace");
}

#[test]
fn letters_are_matched_in_order_whatever_their_case() {
    let names = [
        "Tab: New Tab",
        "Pane: Split Right",
        "Workspace: New Workspace",
    ];
    assert_eq!(matches("split", &names), vec![1]);
    assert_eq!(matches("SPLIT", &names), vec![1]);
    // The letters need not be next to each other.
    assert_eq!(matches("psr", &names), vec![1]);
    assert_eq!(matches("xyz", &names), Vec::<usize>::new());
}

#[test]
fn spaces_in_what_is_typed_do_not_have_to_be_in_the_name() {
    let names = ["Tab: New Tab"];
    assert_eq!(matches("newtab", &names), vec![0]);
    assert_eq!(matches("new  tab", &names), vec![0]);
}

#[test]
fn a_name_that_starts_with_the_query_comes_first() {
    let names = ["Workspace: New Tab", "Tab: New Tab", "Pane: Tab"];
    assert_eq!(matches("tab", &names), vec![1, 0, 2]);
}

#[test]
fn a_word_that_starts_with_the_query_comes_before_one_that_only_holds_it() {
    let names = ["Pane: Tablet", "Pane: Stable", "Pane: Tab Bar"];
    // "Tablet" and "Tab Bar" have a word that starts with "tab"; "Stable"
    // only holds it. The two keep their order.
    assert_eq!(matches("tab", &names), vec![0, 2, 1]);
}

#[test]
fn letters_next_to_each_other_come_before_letters_apart() {
    let names = ["Pane: Focus Left", "Tab: Close", "Pane: Clear Old Stuff"];
    assert_eq!(matches("clos", &names), vec![1, 2]);
}

/// What the palette is expected to match, written plainly: the letters of
/// the query, without spaces, in order in the name, whatever their case.
fn reference_matches(query: &str, name: &str) -> bool {
    let name = name.to_lowercase();
    let mut rest = name.chars();
    query
        .to_lowercase()
        .chars()
        .filter(|ch| !ch.is_whitespace())
        .all(|wanted| rest.any(|ch| ch == wanted))
}

fn names() -> impl Strategy<Value = Vec<String>> {
    proptest::collection::vec("[A-Za-z :日本]{0,12}", 0..12)
}

fn query() -> impl Strategy<Value = String> {
    "[A-Za-z :日]{0,5}"
}

proptest! {
    /// Nothing typed shows every command in its order.
    #[test]
    fn nothing_typed_shows_everything_in_order(names in names(), spaces in " {0,3}") {
        prop_assert_eq!(matches(&spaces, &names), (0..names.len()).collect::<Vec<_>>());
    }

    #[test]
    fn exactly_the_matching_names_are_shown_once(names in names(), query in query()) {
        let shown = matches(&query, &names);
        let set: BTreeSet<usize> = shown.iter().copied().collect();
        prop_assert_eq!(set.len(), shown.len());
        let expected: BTreeSet<usize> = (0..names.len())
            .filter(|&i| reference_matches(&query, &names[i]))
            .collect();
        prop_assert_eq!(set, expected);
    }

    /// Typing one more letter never shows a name that was not shown.
    #[test]
    fn typing_more_only_takes_names_away(names in names(), query in query(), more in "[A-Za-z 日]") {
        let before: BTreeSet<usize> = matches(&query, &names).into_iter().collect();
        let after: BTreeSet<usize> = matches(&format!("{query}{more}"), &names).into_iter().collect();
        prop_assert!(after.is_subset(&before));
    }

    /// The case of the letters typed does not matter.
    #[test]
    fn case_does_not_matter(names in names(), query in "[A-Za-z ]{0,5}") {
        prop_assert_eq!(matches(&query.to_uppercase(), &names), matches(&query.to_lowercase(), &names));
    }

    /// A name that starts with the query is shown before every name that
    /// does not.
    #[test]
    fn names_starting_with_the_query_come_first(names in names(), query in "[A-Za-z]{1,3}") {
        let shown = matches(&query, &names);
        let starts = |i: usize| names[i].to_lowercase().starts_with(&query.to_lowercase());
        let first_other = shown.iter().position(|&i| !starts(i)).unwrap_or(shown.len());
        prop_assert!(shown[first_other..].iter().all(|&i| !starts(i)));
    }

    /// A name that holds the query as it was typed is shown before every
    /// name that only has its letters apart.
    #[test]
    fn whole_queries_come_before_scattered_letters(names in names(), query in "[A-Za-z]{1,3}") {
        let shown = matches(&query, &names);
        let holds = |i: usize| names[i].to_lowercase().contains(&query.to_lowercase());
        let first_scattered = shown.iter().position(|&i| !holds(i)).unwrap_or(shown.len());
        prop_assert!(shown[first_scattered..].iter().all(|&i| !holds(i)));
    }

    /// Names that are the same are shown in the order they were given.
    #[test]
    fn the_same_names_keep_their_order(name in "[A-Za-z :]{0,8}", count in 0usize..5, query in query()) {
        let names = vec![name; count];
        let shown = matches(&query, &names);
        prop_assert!(shown.windows(2).all(|pair| pair[0] < pair[1]));
    }
}

fn keys(text: &'static str) -> String {
    let keystroke = gpui::Keystroke::parse(text).unwrap();
    key_label(&keystroke.modifiers, &keystroke.key)
}

#[test]
fn keys_are_written_as_macos_menus_write_them() {
    assert_eq!(keys("cmd-t"), "⌘T");
    assert_eq!(keys("cmd-shift-d"), "⇧⌘D");
    assert_eq!(keys("ctrl-cmd-]"), "⌃⌘]");
    assert_eq!(keys("cmd-alt-left"), "⌥⌘←");
    assert_eq!(keys("ctrl-alt-shift-cmd-up"), "⌃⌥⇧⌘↑");
    assert_eq!(keys("cmd-alt-right"), "⌥⌘→");
    assert_eq!(keys("cmd-alt-down"), "⌥⌘↓");
    assert_eq!(keys("cmd-1"), "⌘1");
    assert_eq!(keys("enter"), "↩");
    assert_eq!(keys("escape"), "⎋");
    assert_eq!(keys("backspace"), "⌫");
    assert_eq!(keys("tab"), "⇥");
    assert_eq!(keys("cmd-space"), "⌘Space");
    assert_eq!(keys("f5"), "F5");
}

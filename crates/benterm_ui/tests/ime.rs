//! The text an input method is composing, and what of it is sent.

use benterm_ui::ime::Composition;
use proptest::prelude::*;

#[test]
fn nothing_is_marked_at_first() {
    let composition = Composition::default();
    assert_eq!(composition.marked(), None);
    assert_eq!(composition.marked_range_utf16(), None);
    assert_eq!(composition.selected_range_utf16(), 0..0);
}

#[test]
fn composing_text_is_marked_and_committing_sends_only_what_was_committed() {
    let mut composition = Composition::default();
    composition.mark("に");
    composition.mark("にほ");
    composition.mark("にほんご");
    assert_eq!(composition.marked(), Some("にほんご"));
    composition.mark("日本語");
    assert_eq!(composition.marked(), Some("日本語"));

    assert_eq!(
        composition.commit("日本語"),
        Some("日本語".as_bytes().to_vec())
    );
    assert_eq!(composition.marked(), None);
}

#[test]
fn text_committed_without_composing_is_sent() {
    let mut composition = Composition::default();
    assert_eq!(composition.commit("a"), Some(b"a".to_vec()));
}

#[test]
fn committing_nothing_ends_the_composition_and_sends_nothing() {
    let mut composition = Composition::default();
    composition.mark("にほ");
    assert_eq!(composition.commit(""), None);
    assert_eq!(composition.marked(), None);
}

#[test]
fn marking_nothing_or_clearing_ends_the_composition() {
    let mut composition = Composition::default();
    composition.mark("に");
    composition.mark("");
    assert_eq!(composition.marked(), None);

    composition.mark("に");
    composition.clear();
    assert_eq!(composition.marked(), None);
    assert_eq!(composition.marked_range_utf16(), None);
}

#[test]
fn ranges_are_in_utf16_code_units() {
    let mut composition = Composition::default();
    composition.mark("日本語");
    assert_eq!(composition.marked_range_utf16(), Some(0..3));
    assert_eq!(composition.selected_range_utf16(), 3..3);

    // U+20BB7 takes two code units.
    composition.mark("𠮷a");
    assert_eq!(composition.marked_range_utf16(), Some(0..3));
    assert_eq!(composition.selected_range_utf16(), 3..3);
}

#[test]
fn text_of_a_range_is_cut_from_the_marked_text() {
    let mut composition = Composition::default();
    composition.mark("にほんご");
    assert_eq!(composition.text_in_range_utf16(1..3), (1..3, "ほん".into()));
    // A range past the end is clamped.
    assert_eq!(composition.text_in_range_utf16(2..9), (2..4, "んご".into()));
    assert_eq!(composition.text_in_range_utf16(7..9), (4..4, String::new()));

    composition.clear();
    assert_eq!(composition.text_in_range_utf16(0..2), (0..0, String::new()));
}

#[test]
fn columns_before_an_offset_count_wide_characters_twice() {
    let mut composition = Composition::default();
    composition.mark("aにb𠮷c");
    // Offsets in UTF-16: a=0, に=1, b=2, 𠮷=3..5, c=5.
    let columns: Vec<usize> = (0..=6)
        .map(|offset| composition.columns_before_utf16(offset))
        .collect();
    assert_eq!(columns, vec![0, 1, 3, 4, 4, 6, 7]);
    // Past the end, the whole text.
    assert_eq!(composition.columns_before_utf16(99), 7);
    assert_eq!(composition.columns(), 7);
}

#[test]
fn segments_place_what_follows_a_wide_character_by_its_column() {
    let mut composition = Composition::default();
    assert_eq!(composition.segments(), vec![]);
    composition.mark("abc");
    assert_eq!(composition.segments(), vec![(0, "abc")]);
    composition.mark("aにbcほん");
    assert_eq!(
        composition.segments(),
        vec![(0, "aに"), (3, "bcほ"), (7, "ん")]
    );
}

#[derive(Debug, Clone)]
enum Step {
    Mark(String),
    Commit(String),
    Clear,
}

fn step() -> impl Strategy<Value = Step> {
    let text = prop_oneof!["[a-zぁ-ん日本語]{0,4}", ".{0,4}"];
    prop_oneof![
        text.clone().prop_map(Step::Mark),
        text.prop_map(Step::Commit),
        Just(Step::Clear),
    ]
}

proptest! {
    /// Whatever the input method does, the program gets each committed
    /// text once, in order, and nothing of what was only marked.
    #[test]
    fn exactly_the_committed_text_is_sent(steps in prop::collection::vec(step(), 0..24)) {
        let mut composition = Composition::default();
        let mut sent = Vec::new();
        let mut committed = Vec::new();
        for step in steps {
            match step {
                Step::Mark(text) => {
                    composition.mark(&text);
                    prop_assert_eq!(composition.marked(), (!text.is_empty()).then_some(&*text));
                }
                Step::Commit(text) => {
                    sent.extend(composition.commit(&text).unwrap_or_default());
                    committed.extend(text.into_bytes());
                    prop_assert_eq!(composition.marked(), None);
                }
                Step::Clear => {
                    composition.clear();
                    prop_assert_eq!(composition.marked(), None);
                }
            }
        }
        prop_assert_eq!(sent, committed);
    }

    #[test]
    fn any_range_of_any_marked_text_is_cut_without_panicking(
        text in ".{0,8}",
        start in 0usize..20,
        len in 0usize..20,
    ) {
        let mut composition = Composition::default();
        composition.mark(&text);
        let units = text.encode_utf16().count();
        let (range, cut) = composition.text_in_range_utf16(start..start + len);
        prop_assert!(range.start <= range.end && range.end <= units);
        prop_assert!(cut.encode_utf16().count() <= units);
        prop_assert!(composition.columns_before_utf16(start) <= composition.columns());
        let pieces: String = composition.segments().iter().map(|(_, piece)| *piece).collect();
        prop_assert_eq!(pieces, text);
    }
}

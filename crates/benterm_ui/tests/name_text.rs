//! The text of a name field, as the keys and the input method change it.

use benterm_ui::name_editor::{NameText, wrap_line};
use proptest::prelude::*;

#[test]
fn a_field_starts_with_all_of_its_text_selected() {
    let name = NameText::new("Workspace 1");
    assert_eq!(name.text(), "Workspace 1");
    assert!(name.all_selected());
    assert_eq!(name.selected_range_utf16(), 0..11);
    assert_eq!(name.marked_range_utf16(), None);
}

#[test]
fn typing_takes_the_place_of_the_selected_text_and_then_adds_to_it() {
    let mut name = NameText::new("Workspace 1");
    name.replace(None, "w");
    assert_eq!(name.text(), "w");
    assert!(!name.all_selected());
    name.replace(None, "eb");
    assert_eq!(name.text(), "web");
    assert_eq!(name.selected_range_utf16(), 3..3);
}

#[test]
fn backspace_clears_selected_text_and_then_takes_one_character() {
    let mut name = NameText::new("old");
    name.delete_backward();
    assert_eq!(name.text(), "");
    name.replace(None, "日本語");
    name.delete_backward();
    assert_eq!(name.text(), "日本");
    name.delete_backward();
    name.delete_backward();
    name.delete_backward();
    assert_eq!(name.text(), "");
}

#[test]
fn control_characters_are_left_out() {
    let mut name = NameText::new("");
    name.replace(None, "a\nb\u{1b}c\td");
    assert_eq!(name.text(), "abcd");
}

#[test]
fn line_separators_are_left_out() {
    let mut name = NameText::new("");
    name.replace(None, "a\u{2028}b\u{2029}c");
    assert_eq!(name.text(), "abc");
}

#[test]
fn the_selection_while_converting_is_the_one_the_input_method_names() {
    let mut name = NameText::new("");
    name.replace(None, "a😀b");
    // The selection is counted from the start of the new text.
    name.replace_and_mark(Some(1..3), "日本語", Some(1..2));
    assert_eq!(name.text(), "a日本語b");
    assert_eq!(name.marked_range_utf16(), Some(1..4));
    assert_eq!(name.selected_range_utf16(), 2..3);
    // Without one the caret is at the end of what is being converted.
    name.replace_and_mark(None, "日本", None);
    assert_eq!(name.text(), "a日本b");
    assert_eq!(name.selected_range_utf16(), 3..3);
    // One that starts inside a character or ends past the new text is
    // brought to the nearest place in it.
    name.replace_and_mark(None, "😀x", Some(1..9));
    assert_eq!(name.marked_range_utf16(), Some(1..4));
    assert_eq!(name.selected_range_utf16(), 1..4);
    // Once the text is committed the caret is after it.
    name.replace(None, "語");
    assert_eq!(name.text(), "a語b");
    assert_eq!(name.selected_range_utf16(), 2..2);
}

#[test]
fn the_selection_while_converting_skips_what_was_left_out() {
    let mut name = NameText::new("");
    name.replace(None, "a😀b");
    // The input method counts the line break, which the field leaves out.
    name.replace_and_mark(Some(1..3), "\n日本語", Some(2..3));
    assert_eq!(name.text(), "a日本語b");
    assert_eq!(name.marked_range_utf16(), Some(1..4));
    assert_eq!(name.selected_range_utf16(), 2..3);
    // Nothing is marked or selected if all of it is left out.
    name.replace_and_mark(None, "\n", Some(0..1));
    assert_eq!(name.text(), "ab");
    assert_eq!(name.marked_range_utf16(), None);
    // The caret is where the text that was left out would have been.
    assert_eq!(name.selected_range_utf16(), 1..1);
}

#[test]
fn converted_text_is_marked_until_it_is_committed() {
    let mut name = NameText::new("old");
    // The input method shows what is being converted, updates it, and
    // commits it in its place.
    name.replace_and_mark(None, "に", None);
    assert_eq!(name.text(), "に");
    assert_eq!(name.marked_range_utf16(), Some(0..1));
    name.replace_and_mark(None, "にほん", None);
    assert_eq!(name.text(), "にほん");
    assert_eq!(name.marked_range_utf16(), Some(0..3));
    name.replace(None, "日本");
    assert_eq!(name.text(), "日本");
    assert_eq!(name.marked_range_utf16(), None);
    // The next conversion goes after what is there.
    name.replace_and_mark(None, "ご", None);
    assert_eq!(name.text(), "日本ご");
    assert_eq!(name.marked_range_utf16(), Some(2..3));
    name.replace(None, "語");
    assert_eq!(name.text(), "日本語");
}

#[test]
fn an_emptied_conversion_leaves_nothing_marked() {
    let mut name = NameText::new("a");
    name.replace(None, "a");
    name.replace_and_mark(None, "に", None);
    name.replace_and_mark(None, "", None);
    assert_eq!(name.text(), "a");
    assert_eq!(name.marked_range_utf16(), None);
}

#[test]
fn unmarking_keeps_the_text() {
    let mut name = NameText::new("");
    name.replace_and_mark(None, "にほん", None);
    name.unmark();
    assert_eq!(name.text(), "にほん");
    assert_eq!(name.marked_range_utf16(), None);
    name.replace(None, "ご");
    assert_eq!(name.text(), "にほんご");
}

#[test]
fn a_range_named_by_the_input_method_is_counted_in_utf16() {
    // The emoji is two UTF-16 units and four bytes.
    let mut name = NameText::new("");
    name.replace(None, "a😀b");
    assert_eq!(name.selected_range_utf16(), 4..4);
    assert_eq!(name.text_in_range_utf16(1..3), (1..3, "😀".to_owned()));
    // A range that ends inside a character is moved back to its start.
    assert_eq!(name.text_in_range_utf16(0..2), (0..1, "a".to_owned()));
    assert_eq!(name.text_in_range_utf16(3..99), (3..4, "b".to_owned()));
    name.replace(Some(1..3), "é");
    assert_eq!(name.text(), "aéb");
    name.replace_and_mark(Some(0..1), "あい", None);
    assert_eq!(name.text(), "あいéb");
    assert_eq!(name.marked_range_utf16(), Some(0..2));
}

#[derive(Clone, Debug)]
enum Op {
    Replace(Option<(usize, usize)>, String),
    Mark(Option<(usize, usize)>, String, Option<(usize, usize)>),
    Unmark,
    Backspace,
}

fn op() -> impl Strategy<Value = Op> {
    let range = || proptest::option::of((0usize..12, 0usize..12));
    let text = || "[a-c日本😀\n\u{2028}]{0,3}";
    prop_oneof![
        (range(), text()).prop_map(|(r, t)| Op::Replace(r, t)),
        (range(), text(), range()).prop_map(|(r, t, s)| Op::Mark(r, t, s)),
        Just(Op::Unmark),
        Just(Op::Backspace),
    ]
}

fn ends_a_line(ch: char) -> bool {
    ch.is_control() || ch == '\u{2028}' || ch == '\u{2029}'
}

proptest! {
    /// Whatever the input method asks for, the ranges the field reports
    /// are inside the text and on character boundaries, the selection is
    /// inside what is being converted, and nothing that ends a line gets
    /// in.
    #[test]
    fn ranges_stay_inside_the_text(start in "[a-c日😀]{0,4}", ops in proptest::collection::vec(op(), 0..40)) {
        let mut name = NameText::new(&start);
        for op in ops {
            let range = |r: Option<(usize, usize)>| r.map(|(a, b)| a.min(b)..a.max(b));
            match op {
                Op::Replace(r, text) => name.replace(range(r), &text),
                Op::Mark(r, text, s) => name.replace_and_mark(range(r), &text, range(s)),
                Op::Unmark => name.unmark(),
                Op::Backspace => name.delete_backward(),
            }
            let len = name.text().encode_utf16().count();
            prop_assert!(!name.text().chars().any(ends_a_line));
            let selected = name.selected_range_utf16();
            prop_assert!(selected.start <= selected.end && selected.end <= len);
            if let Some(marked) = name.marked_range_utf16() {
                prop_assert!(marked.start < marked.end && marked.end <= len);
                prop_assert!(marked.start <= selected.start && selected.end <= marked.end);
                // The selection can be read back whole too.
                let (taken, _) = name.text_in_range_utf16(selected.clone());
                prop_assert_eq!(taken, selected);
                // The marked text can be read back whole.
                let (taken, text) = name.text_in_range_utf16(marked.clone());
                prop_assert_eq!(taken, marked);
                prop_assert!(!text.is_empty());
            }
            let (taken, text) = name.text_in_range_utf16(0..len);
            prop_assert_eq!(taken, 0..len);
            prop_assert_eq!(text, name.text());
        }
    }
}

#[test]
fn the_caret_moves_through_the_text_and_typing_goes_where_it_is() {
    let mut name = NameText::new("");
    name.replace(None, "abc");
    name.move_left(false);
    name.replace(None, "x");
    assert_eq!(name.text(), "abxc");
    assert_eq!(name.caret(), 3);
    name.move_to_start(false);
    name.replace(None, ">");
    assert_eq!(name.text(), ">abxc");
    name.move_to_end(false);
    name.replace(None, "!");
    assert_eq!(name.text(), ">abxc!");
    // Past either end the caret stays at the end.
    name.move_right(false);
    assert_eq!(name.caret(), name.text().len());
}

#[test]
fn the_caret_steps_over_whole_characters() {
    let mut name = NameText::new("");
    name.replace(None, "日本a");
    name.move_left(false);
    name.move_left(false);
    assert_eq!(name.caret(), "日".len());
    name.delete_backward();
    assert_eq!(name.text(), "本a");
    name.delete_forward();
    assert_eq!(name.text(), "a");
}

#[test]
fn a_selection_is_made_with_shift_and_typed_over_or_deleted() {
    let mut name = NameText::new("");
    name.replace(None, "hello world");
    for _ in 0..5 {
        name.move_left(true);
    }
    assert_eq!(name.selected_text(), "world");
    assert_eq!(name.selected_range_utf16(), 6..11);
    name.replace(None, "there");
    assert_eq!(name.text(), "hello there");
    name.move_to_start(true);
    name.delete_backward();
    assert_eq!(name.text(), "");
}

#[test]
fn moving_without_shift_goes_to_the_end_of_the_selection() {
    let mut name = NameText::new("abc");
    assert!(name.all_selected());
    name.move_left(false);
    assert_eq!(name.caret(), 0);
    assert!(!name.all_selected());
    name.select_all();
    name.move_right(false);
    assert_eq!(name.caret(), 3);
}

#[test]
fn cut_takes_the_selection_and_copy_leaves_it() {
    let mut name = NameText::new("abc def");
    name.move_to_end(false);
    for _ in 0..3 {
        name.move_left(true);
    }
    assert_eq!(name.selected_text(), "def");
    assert_eq!(name.cut(), "def");
    assert_eq!(name.text(), "abc ");
    assert_eq!(name.cut(), "");
}

#[test]
fn an_edit_is_undone_and_done_again() {
    let mut name = NameText::new("one");
    name.replace(None, "two");
    name.replace(None, "!");
    assert_eq!(name.text(), "two!");
    name.undo();
    assert_eq!(name.text(), "two");
    name.undo();
    assert_eq!(name.text(), "one");
    assert!(name.all_selected());
    // With nothing left to undo, nothing happens.
    name.undo();
    assert_eq!(name.text(), "one");
    name.redo();
    name.redo();
    assert_eq!(name.text(), "two!");
    // An edit after an undo drops what was undone.
    name.undo();
    name.replace(None, "?");
    name.redo();
    assert_eq!(name.text(), "two?");
}

#[test]
fn a_field_of_lines_keeps_its_line_breaks() {
    let mut text = NameText::lines("");
    text.replace(None, "one\ntwo");
    assert_eq!(text.text(), "one\ntwo");
    let mut name = NameText::new("");
    name.replace(None, "one\ntwo");
    assert_eq!(name.text(), "onetwo");
}

#[test]
fn putting_nothing_in_place_of_nothing_is_no_edit() {
    let mut name = NameText::new("");
    name.replace(None, "a");
    name.undo();
    // A line break is left out of a name: nothing is put anywhere.
    name.replace(None, "\n");
    name.redo();
    assert_eq!(name.text(), "a");
}

#[test]
fn the_selection_while_converting_counts_the_line_breaks_a_field_keeps() {
    let mut text = NameText::lines("");
    text.replace_and_mark(None, "あ\nい", Some(2..3));
    assert_eq!(text.text(), "あ\nい");
    assert_eq!(text.selected_range_utf16(), 2..3);
}

#[test]
fn moving_up_and_down_preserves_column_and_clamps_to_shorter_lines() {
    let mut text = NameText::lines("abcdef\n12\nxyz");
    text.set_caret(4, false); // "abcd|ef"
    text.move_down(false);
    assert_eq!(text.caret(), "abcdef\n12".len()); // clamped to line 2 end
    text.move_down(false);
    assert_eq!(text.caret(), "abcdef\n12\nxy".len()); // col 2 of line 3
    text.move_up(false);
    assert_eq!(text.caret(), "abcdef\n12".len());
    text.move_up(false);
    assert_eq!(text.caret(), 2); // "ab|cdef"
}

#[test]
fn wrap_line_splits_long_lines_at_spaces_or_character_boundaries() {
    // 10px per ASCII character.
    let mono = |line: &str| {
        let s = line.to_owned();
        move |byte: usize| s[..byte].chars().count() as f32 * 10.
    };
    let line = "hello world again";
    let ranges = wrap_line(line, 110., mono(line));
    let parts: Vec<&str> = ranges.into_iter().map(|r| &line[r]).collect();
    assert_eq!(parts, ["hello ", "world again"]);

    let continuous = "abcdefghij";
    let ranges = wrap_line(continuous, 40., mono(continuous));
    let parts: Vec<&str> = ranges.into_iter().map(|r| &continuous[r]).collect();
    assert_eq!(parts, ["abcd", "efgh", "ij"]);
}

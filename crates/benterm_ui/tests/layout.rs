//! Turning a row of cells into what gets painted.

use benterm_core::{AlacrittyCore, GridSize, Rgb, Row, TerminalCore, Underline};
use benterm_ui::layout::{RowLayout, layout_row};
use benterm_ui::theme::{TerminalTheme, Theme};

fn theme() -> TerminalTheme {
    Theme::default().terminal
}

/// The first row of a 20-column screen after `input`.
fn row(input: &str) -> Row {
    let mut core = AlacrittyCore::new(GridSize::new(2, 20), 0);
    core.feed(input.as_bytes());
    core.screen_rows(0..1).remove(0)
}

fn layout(input: &str) -> RowLayout {
    layout_row(&row(input), &theme())
}

fn texts(layout: &RowLayout) -> Vec<(u16, &str)> {
    layout
        .segments
        .iter()
        .map(|segment| (segment.col, segment.text.as_str()))
        .collect()
}

#[test]
fn a_blank_row_paints_nothing() {
    let layout = layout("");
    assert!(layout.segments.is_empty());
    assert!(layout.backgrounds.is_empty());
}

#[test]
fn text_is_one_segment_without_trailing_blanks() {
    assert_eq!(texts(&layout("ab  cd")), [(0, "ab  cd")]);
}

#[test]
fn leading_blanks_move_the_segment() {
    assert_eq!(texts(&layout("   x")), [(3, "x")]);
}

/// Text after a wide character is positioned by its own column, because
/// the wide glyph's advance is not trusted to be two cells.
#[test]
fn a_wide_character_ends_its_segment() {
    assert_eq!(texts(&layout("a日本b")), [(0, "a日"), (3, "本"), (5, "b")]);
}

#[test]
fn combining_characters_stay_with_their_base() {
    assert_eq!(texts(&layout("e\u{301}x")), [(0, "e\u{301}x")]);
}

#[test]
fn runs_follow_the_colors() {
    let layout = layout("a\x1b[31mbc\x1b[0md");
    let theme = theme();
    let runs = &layout.segments[0].runs;
    let seen: Vec<(usize, Rgb)> = runs.iter().map(|run| (run.len, run.style.fg)).collect();
    assert_eq!(
        seen,
        [
            (1, theme.foreground),
            (2, theme.ansi[1]),
            (1, theme.foreground)
        ]
    );
}

#[test]
fn run_lengths_are_in_bytes() {
    let layout = layout("\x1b[31mé\x1b[0mx");
    let lens: Vec<usize> = layout.segments[0].runs.iter().map(|run| run.len).collect();
    assert_eq!(lens, [2, 1]);
}

#[test]
fn blanks_between_words_do_not_split_runs() {
    let layout = layout("\x1b[31mab\x1b[0m \x1b[31mcd");
    assert_eq!(layout.segments[0].runs.len(), 1);
}

#[test]
fn attributes_reach_the_run_style() {
    let layout = layout("\x1b[1mb\x1b[0;3mi\x1b[0;4mu\x1b[0;9ms\x1b[0;4:3mc");
    let styles: Vec<_> = layout.segments[0]
        .runs
        .iter()
        .map(|run| run.style)
        .collect();
    assert!(styles[0].bold && !styles[0].italic);
    assert!(styles[1].italic && !styles[1].bold);
    assert_eq!(styles[2].underline, Underline::Single);
    assert!(styles[3].strikeout);
    assert_eq!(styles[4].underline, Underline::Curly);
}

#[test]
fn an_underlined_blank_is_still_painted() {
    assert_eq!(texts(&layout("\x1b[4m  ")), [(0, "  ")]);
}

#[test]
fn backgrounds_merge_adjacent_cells_of_one_color() {
    let layout = layout("a\x1b[41mbc\x1b[42md\x1b[0me");
    let theme = theme();
    let spans: Vec<(u16, u16, Rgb)> = layout
        .backgrounds
        .iter()
        .map(|span| (span.start, span.end, span.color))
        .collect();
    assert_eq!(spans, [(1, 3, theme.ansi[1]), (3, 4, theme.ansi[2])]);
}

#[test]
fn a_wide_character_has_a_background_two_cells_wide() {
    let layout = layout("\x1b[44m日");
    assert_eq!(
        (layout.backgrounds[0].start, layout.backgrounds[0].end),
        (0, 2)
    );
}

#[test]
fn inverse_swaps_foreground_and_background() {
    let layout = layout("\x1b[7mx");
    let theme = theme();
    assert_eq!(layout.segments[0].runs[0].style.fg, theme.background);
    assert_eq!(layout.backgrounds[0].color, theme.foreground);
}

#[test]
fn hidden_text_takes_the_background_color() {
    let layout = layout("\x1b[41;8mx");
    assert_eq!(layout.segments[0].runs[0].style.fg, theme().ansi[1]);
}

#[test]
fn dim_text_moves_toward_the_background() {
    let layout = layout("\x1b[2mx");
    let theme = theme();
    let fg = layout.segments[0].runs[0].style.fg;
    assert_ne!(fg, theme.foreground);
    assert!(fg.0 < theme.foreground.0 && fg.0 > theme.background.0);
}

/// A blank joins the run before it to save a run, but not when that run
/// draws a line: the line would continue under the blank.
#[test]
fn a_blank_after_decorated_text_is_not_decorated() {
    for decoration in ["\x1b[4m", "\x1b[9m"] {
        let layout = layout(&format!("{decoration}ab\x1b[0m cd"));
        let runs = &layout.segments[0].runs;
        assert_eq!(runs[0].len, 2, "the decorated run covers only its text");
        let plain = runs[1].style;
        assert_eq!(plain.underline, Underline::None);
        assert!(!plain.strikeout);
    }
}

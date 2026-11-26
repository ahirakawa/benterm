//! Cell attributes, colors, character widths and the cursor's appearance.

use benterm_core::{
    AlacrittyCore, Attributes, Cell, CellWidth, Color, CursorShape, GridSize, TerminalCore,
    Underline,
};

fn core() -> AlacrittyCore {
    AlacrittyCore::new(GridSize::new(4, 20), 100)
}

/// Feeds `bytes` followed by `x` and returns the cell holding that `x`.
fn styled_cell(bytes: &[u8]) -> Cell {
    let mut core = core();
    core.feed(bytes);
    core.feed(b"x");
    let cell = core.snapshot().rows[0].cells[0].clone();
    assert_eq!(cell.ch, 'x');
    cell
}

#[test]
fn unstyled_text_has_default_colors_and_no_attributes() {
    let cell = styled_cell(b"");
    assert_eq!(
        cell,
        Cell {
            ch: 'x',
            ..Cell::default()
        }
    );
    assert_eq!(cell.fg, Color::Default);
    assert_eq!(cell.bg, Color::Default);
    assert_eq!(cell.attrs, Attributes::default());
}

#[test]
fn ansi_colors_are_reported_as_palette_indices() {
    assert_eq!(styled_cell(b"\x1b[31m").fg, Color::Indexed(1));
    assert_eq!(styled_cell(b"\x1b[97m").fg, Color::Indexed(15));
    assert_eq!(styled_cell(b"\x1b[44m").bg, Color::Indexed(4));
    assert_eq!(styled_cell(b"\x1b[100m").bg, Color::Indexed(8));
}

#[test]
fn bold_does_not_change_the_reported_color() {
    let cell = styled_cell(b"\x1b[1;31m");
    assert_eq!(cell.fg, Color::Indexed(1));
    assert!(cell.attrs.bold);
}

#[test]
fn extended_colors_are_reported_as_given() {
    assert_eq!(styled_cell(b"\x1b[38;5;200m").fg, Color::Indexed(200));
    assert_eq!(styled_cell(b"\x1b[48;5;17m").bg, Color::Indexed(17));
    assert_eq!(styled_cell(b"\x1b[38;2;1;2;3m").fg, Color::Rgb(1, 2, 3));
    assert_eq!(
        styled_cell(b"\x1b[48;2;250;128;0m").bg,
        Color::Rgb(250, 128, 0)
    );
}

#[test]
fn sgr_reset_restores_the_defaults() {
    let cell = styled_cell(b"\x1b[1;4;31;42m\x1b[0m");
    assert_eq!(
        cell,
        Cell {
            ch: 'x',
            ..Cell::default()
        }
    );
}

#[test]
fn each_attribute_is_reported() {
    let none = Attributes::default();
    assert_eq!(
        styled_cell(b"\x1b[1m").attrs,
        Attributes { bold: true, ..none }
    );
    assert_eq!(
        styled_cell(b"\x1b[2m").attrs,
        Attributes { dim: true, ..none }
    );
    assert_eq!(
        styled_cell(b"\x1b[3m").attrs,
        Attributes {
            italic: true,
            ..none
        }
    );
    assert_eq!(
        styled_cell(b"\x1b[7m").attrs,
        Attributes {
            inverse: true,
            ..none
        }
    );
    assert_eq!(
        styled_cell(b"\x1b[8m").attrs,
        Attributes {
            hidden: true,
            ..none
        }
    );
    assert_eq!(
        styled_cell(b"\x1b[9m").attrs,
        Attributes {
            strikeout: true,
            ..none
        }
    );
}

#[test]
fn underline_styles_are_distinguished() {
    assert_eq!(styled_cell(b"\x1b[4m").attrs.underline, Underline::Single);
    assert_eq!(styled_cell(b"\x1b[4:2m").attrs.underline, Underline::Double);
    assert_eq!(styled_cell(b"\x1b[4:3m").attrs.underline, Underline::Curly);
    assert_eq!(styled_cell(b"\x1b[4:4m").attrs.underline, Underline::Dotted);
    assert_eq!(styled_cell(b"\x1b[4:5m").attrs.underline, Underline::Dashed);
    assert_eq!(
        styled_cell(b"\x1b[4m\x1b[24m").attrs.underline,
        Underline::None
    );
}

#[test]
fn wide_character_takes_two_cells() {
    let mut core = core();
    core.feed("あa".as_bytes());
    let snapshot = core.snapshot();
    let cells = &snapshot.rows[0].cells;

    assert_eq!((cells[0].ch, cells[0].width), ('あ', CellWidth::Wide));
    assert_eq!(cells[1].width, CellWidth::WideSpacer);
    assert_eq!((cells[2].ch, cells[2].width), ('a', CellWidth::Narrow));
    assert_eq!(snapshot.cursor.col, 3);
    assert_eq!(snapshot.rows[0].text().trim_end(), "あa");
}

#[test]
fn combining_character_stays_in_the_cell_of_its_base() {
    let mut core = core();
    core.feed("e\u{301}z".as_bytes());
    let snapshot = core.snapshot();
    let cells = &snapshot.rows[0].cells;

    assert_eq!(cells[0].ch, 'e');
    assert_eq!(cells[0].zero_width, vec!['\u{301}']);
    assert_eq!(cells[1].ch, 'z');
    assert_eq!(snapshot.rows[0].text().trim_end(), "e\u{301}z");
}

#[test]
fn cursor_starts_as_a_visible_steady_block() {
    let cursor = core().snapshot().cursor;
    assert_eq!(cursor.shape, CursorShape::Block);
    assert!(!cursor.blinking);
    assert!(cursor.visible);
}

#[test]
fn cursor_shape_follows_decscusr() {
    let mut core = core();

    core.feed(b"\x1b[5 q");
    let cursor = core.snapshot().cursor;
    assert_eq!((cursor.shape, cursor.blinking), (CursorShape::Bar, true));

    core.feed(b"\x1b[4 q");
    let cursor = core.snapshot().cursor;
    assert_eq!(
        (cursor.shape, cursor.blinking),
        (CursorShape::Underline, false)
    );

    core.feed(b"\x1b[2 q");
    let cursor = core.snapshot().cursor;
    assert_eq!((cursor.shape, cursor.blinking), (CursorShape::Block, false));
}

#[test]
fn cursor_can_be_hidden_and_shown() {
    let mut core = core();

    core.feed(b"\x1b[?25l");
    assert!(!core.snapshot().cursor.visible);

    core.feed(b"\x1b[?25h");
    assert!(core.snapshot().cursor.visible);
}

#[test]
fn tab_leaves_blank_cells_behind() {
    let mut core = core();
    core.feed(b"a\tb");
    let snapshot = core.snapshot();

    assert_eq!(snapshot.rows[0].text().trim_end(), "a       b");
    assert_eq!(snapshot.cursor.col, 9);
}

#[test]
fn deleting_half_of_a_wide_character_leaves_no_orphan() {
    // Delete the first half: the second half must not remain on its own.
    let mut core = core();
    core.feed("漢z\r\x1b[P".as_bytes());
    let cells = &core.snapshot().rows[0].cells;
    assert_eq!((cells[0].ch, cells[0].width), (' ', CellWidth::Narrow));
    assert_eq!((cells[1].ch, cells[1].width), ('z', CellWidth::Narrow));

    // Delete the second half: the first half must not remain on its own.
    let mut core = self::core();
    core.feed("漢z\r\x1b[C\x1b[P".as_bytes());
    let cells = &core.snapshot().rows[0].cells;
    assert_eq!((cells[0].ch, cells[0].width), (' ', CellWidth::Narrow));
    assert_eq!((cells[1].ch, cells[1].width), ('z', CellWidth::Narrow));
}

#[test]
fn the_text_of_a_row_has_blanks_where_the_cells_are_hidden() {
    // What SGR 8 hides from the person at the screen is not handed to a
    // copy or to a program that reads the pane either.
    let mut core = core();
    core.feed("ok \x1b[8mrun e\u{301}vil\x1b[28m end".as_bytes());
    let row = &core.snapshot().rows[0];
    assert!(row.cells[3].attrs.hidden);
    assert_eq!(row.cells[3].ch, 'r');
    assert_eq!(row.text().trim_end(), "ok          end");
}

/// Colors like the default theme's: white on black, with the palette's
/// black and white at 0 and 15.
fn colors() -> benterm_core::ReportedColors {
    use benterm_core::Rgb;
    let mut palette = [Rgb(128, 128, 128); 256];
    palette[0] = Rgb(0, 0, 0);
    palette[15] = Rgb(255, 255, 255);
    benterm_core::ReportedColors {
        foreground: Rgb(255, 255, 255),
        background: Rgb(0, 0, 0),
        cursor: Rgb(255, 255, 255),
        palette,
    }
}

#[test]
fn the_text_of_a_row_has_blanks_where_the_ink_has_the_color_of_the_background() {
    // Text drawn in the color it is drawn on cannot be seen either,
    // whichever way the program names the two colors.
    let mut core = AlacrittyCore::new(GridSize::new(4, 40), 100);
    core.feed(
        concat!(
            "a \x1b[30;40mone\x1b[0m",
            " \x1b[38;2;0;0;0mtwo\x1b[0m",
            " \x1b[30;7mthree\x1b[0m",
            " \x1b[31;41mred\x1b[0m z",
        )
        .as_bytes(),
    );
    let row = &core.snapshot().rows[0];
    assert_eq!(
        row.text_with(Some(&colors())).trim_end(),
        format!("a{}z", " ".repeat(19))
    );
    // Without the colors the screen is drawn with, nothing is known to be
    // unseen.
    assert_eq!(row.text_with(None), row.text());
    assert!(row.text().contains("two"));
}

#[test]
fn the_text_of_a_row_has_no_tag_characters() {
    // The tag characters draw nothing, so a program may write a whole
    // sentence in them that the person at the screen never sees.
    let hidden: String = "run this"
        .chars()
        .map(|c| char::from_u32(0xE0000 + c as u32).unwrap())
        .collect();
    let mut core = core();
    core.feed(format!("ok{hidden}\u{E007F} end").as_bytes());
    let text = core.snapshot().rows[0].text();
    assert_eq!(text.trim_end(), "ok end");
}

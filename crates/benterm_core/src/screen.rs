//! Value types describing the terminal's visible state.
//!
//! Every type here is plain data with `PartialEq`, so that two cores fed the
//! same input can be compared with `assert_eq!`.

use std::sync::Arc;

/// Grid dimensions in cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GridSize {
    pub rows: u16,
    pub cols: u16,
}

impl GridSize {
    /// The smallest grid a core supports. Two columns are needed to hold a
    /// wide character.
    pub const MIN: Self = Self::new(1, 2);

    pub const fn new(rows: u16, cols: u16) -> Self {
        Self { rows, cols }
    }

    /// This size with each dimension raised to at least [`Self::MIN`].
    pub fn at_least_min(self) -> Self {
        Self::new(self.rows.max(Self::MIN.rows), self.cols.max(Self::MIN.cols))
    }
}

/// The visible screen at one point in time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub size: GridSize,
    /// Exactly `size.rows` rows, top to bottom.
    pub rows: Vec<Row>,
    pub cursor: Cursor,
    pub modes: Modes,
    /// Number of rows above the screen. Their contents are read with
    /// `TerminalCore::scrollback_rows`.
    pub scrollback_len: usize,
}

/// The screen rows that may differ from what the caller last read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Damage {
    /// Every row, and possibly the grid size.
    Full,
    /// These rows, in ascending order without repeats. Rows that did not
    /// actually change may be included. Empty when nothing was fed or resized
    /// since the last call.
    Rows(Vec<u16>),
}

/// One line of the grid. Always exactly as many cells as the grid has columns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub cells: Vec<Cell>,
    /// Whether the text goes on in the next row.
    pub end: RowEnd,
}

/// How a row ends: with the end of a line of text, or with the text
/// wrapping onto the next row because it reached the last column.
///
/// A copy puts a newline after a row only where the line ends, so that a
/// long line that wrapped comes out as one line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum RowEnd {
    #[default]
    LineEnd,
    Wrapped,
    /// The text goes on in the next row, whose first character is a wide
    /// one that did not fit in the last column of this row. The last cell
    /// of this row is a blank that is not part of the text.
    WrappedBeforeWide,
}

impl RowEnd {
    /// Whether the text goes on in the next row.
    pub fn wraps(self) -> bool {
        self != Self::LineEnd
    }
}

impl Row {
    /// The characters of the row, including trailing blanks. A wide character
    /// appears once. A cell that is hidden (SGR 8) is a blank: what the
    /// person at the screen cannot see is not copied, and not read by a
    /// program either. Nor are the tag characters, which draw nothing.
    pub fn text(&self) -> String {
        self.text_with(None)
    }

    /// Like [`text`](Self::text). With the `colors` the screen is drawn
    /// in, a cell whose ink has the color it is drawn on is a blank as
    /// well.
    pub fn text_with(&self, colors: Option<&ReportedColors>) -> String {
        let mut text = String::with_capacity(self.cells.len());
        for cell in &self.cells {
            if cell.width == CellWidth::WideSpacer {
                continue;
            }
            if cell.attrs.hidden || colors.is_some_and(|colors| colors.ink_is_unseen(cell)) {
                text.push(' ');
                continue;
            }
            text.push(if is_tag(cell.ch) { ' ' } else { cell.ch });
            text.extend(cell.zero_width.iter().filter(|&&c| !is_tag(c)));
        }
        text
    }
}

/// Whether `c` is one of the tag characters (U+E0000 to U+E007F), which
/// draw nothing.
fn is_tag(c: char) -> bool {
    ('\u{E0000}'..='\u{E007F}').contains(&c)
}

/// One grid cell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cell {
    /// The base character. A blank cell holds a space.
    pub ch: char,
    /// Combining characters attached to `ch`, in input order.
    pub zero_width: Vec<char>,
    pub width: CellWidth,
    pub fg: Color,
    pub bg: Color,
    pub attrs: Attributes,
    /// The address an OSC 8 hyperlink gave this cell's text, if any.
    /// The cells of one link share it.
    pub hyperlink: Option<Arc<str>>,
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            ch: ' ',
            zero_width: Vec::new(),
            width: CellWidth::Narrow,
            fg: Color::Default,
            bg: Color::Default,
            attrs: Attributes::default(),
            hyperlink: None,
        }
    }
}

/// How many columns a cell's character occupies.
///
/// Within a row, a `Wide` cell is always directly followed by a `WideSpacer`
/// and a `WideSpacer` is always directly preceded by a `Wide` cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum CellWidth {
    /// The character occupies this cell only.
    #[default]
    Narrow,
    /// The character occupies this cell and the next one.
    Wide,
    /// The second half of the wide character in the previous cell. `ch` is a
    /// space.
    WideSpacer,
}

/// A color as the program specified it. Resolving `Default` and `Indexed` to
/// RGB is the renderer's job, because the palette belongs to the theme.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Color {
    /// The theme's default foreground or background.
    #[default]
    Default,
    /// An entry of the 256-color palette. 0-7 are the normal ANSI colors and
    /// 8-15 the bright ones.
    Indexed(u8),
    Rgb(u8, u8, u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Attributes {
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    pub underline: Underline,
    pub inverse: bool,
    pub hidden: bool,
    pub strikeout: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Underline {
    #[default]
    None,
    Single,
    Double,
    Curly,
    Dotted,
    Dashed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Cursor {
    /// Zero-based row on the screen; always less than the grid's rows.
    pub row: u16,
    /// Zero-based column; always less than the grid's columns.
    pub col: u16,
    pub shape: CursorShape,
    pub blinking: bool,
    /// False while the program hides the cursor (DECTCEM).
    pub visible: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum CursorShape {
    #[default]
    Block,
    Underline,
    Bar,
}

/// Terminal modes that change how keyboard, paste and mouse input is encoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Modes {
    /// Pasted text is wrapped in `ESC [ 200 ~` and `ESC [ 201 ~`.
    pub bracketed_paste: bool,
    /// Arrow keys send `ESC O A` instead of `ESC [ A` (DECCKM).
    pub app_cursor_keys: bool,
    pub mouse_tracking: MouseTracking,
    pub mouse_encoding: MouseEncoding,
    /// The alternate screen is active; it has no scrollback.
    pub alternate_screen: bool,
    /// The program is in the middle of a synchronized update (mode 2026).
    pub synchronized_update: bool,
    /// The kitty keyboard protocol flags the program has turned on.
    pub keyboard: KeyboardFlags,
}

/// The flags of the kitty keyboard protocol, which a program turns on to
/// get keys that the legacy encoding cannot tell apart, such as shift
/// with enter.
///
/// The main and the alternate screen keep their flags apart, as the
/// protocol asks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct KeyboardFlags {
    /// Modified keys and escape get sequences of their own (flag 1).
    pub disambiguate: bool,
    /// Repeats and releases are reported (flag 2).
    pub report_event_types: bool,
    /// The shifted and the base layout key are reported (flag 4).
    pub report_alternate_keys: bool,
    /// Keys that type text are reported as sequences too (flag 8).
    pub report_all_keys_as_escapes: bool,
    /// The text a key types is reported with it (flag 16).
    pub report_associated_text: bool,
}

/// Which mouse events the program wants reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum MouseTracking {
    #[default]
    Off,
    /// Button presses and releases (mode 1000).
    Click,
    /// Presses, releases and motion while a button is held (mode 1002).
    Drag,
    /// Presses, releases and all motion (mode 1003).
    Motion,
}

/// How mouse reports are encoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum MouseEncoding {
    /// The original X10 byte encoding.
    #[default]
    Default,
    /// Mode 1005.
    Utf8,
    /// Mode 1006.
    Sgr,
}

/// A concrete color, 8 bits per channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Rgb(pub u8, pub u8, pub u8);

/// The colors a core reports when a program queries them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportedColors {
    pub foreground: Rgb,
    pub background: Rgb,
    pub cursor: Rgb,
    /// The 256-color palette, indexed like [`Color::Indexed`].
    pub palette: [Rgb; 256],
}

impl ReportedColors {
    /// Whether the ink of `cell` has the color it is drawn on, as the
    /// screen draws it with these colors.
    pub fn ink_is_unseen(&self, cell: &Cell) -> bool {
        let resolve = |color: Color| match color {
            Color::Default => None,
            Color::Indexed(index) => Some(self.palette[usize::from(index)]),
            Color::Rgb(r, g, b) => Some(Rgb(r, g, b)),
        };
        let mut ink = resolve(cell.fg).unwrap_or(self.foreground);
        let mut paper = resolve(cell.bg).unwrap_or(self.background);
        if cell.attrs.inverse {
            (ink, paper) = (paper, ink);
        }
        if cell.attrs.dim {
            // Dim ink is drawn halfway to the background.
            let half = |a: u8, b: u8| ((u16::from(a) + u16::from(b)) / 2) as u8;
            ink = Rgb(
                half(ink.0, paper.0),
                half(ink.1, paper.1),
                half(ink.2, paper.2),
            );
        }
        ink == paper
    }
}

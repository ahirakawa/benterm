//! Turns a row of cells into what the terminal element paints.
//!
//! Nothing here depends on GPUI, so the decisions about colors, runs and
//! positions are tested without a window.

use benterm_core::{Cell, CellWidth, Rgb, Row, Underline};

use crate::theme::TerminalTheme;

/// What to paint for one row.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct RowLayout {
    /// Cells whose background differs from the pane's, left to right.
    pub backgrounds: Vec<BackgroundSpan>,
    /// The text, left to right.
    pub segments: Vec<TextSegment>,
}

/// Columns `start..end` filled with `color`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BackgroundSpan {
    pub start: u16,
    pub end: u16,
    pub color: Rgb,
}

/// A piece of text that starts at column `col` and has one character per
/// cell, except that its last character may be a wide one.
///
/// A segment ends after a wide character so that whatever follows is
/// positioned by its own column: the font's advance for a wide glyph is
/// not trusted to be exactly two cells.
#[derive(Debug, Clone, PartialEq)]
pub struct TextSegment {
    pub col: u16,
    pub text: String,
    /// Styles covering `text` from start to end.
    pub runs: Vec<StyleRun>,
}

/// `len` bytes of a segment's text drawn in one style.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StyleRun {
    pub len: usize,
    pub style: GlyphStyle,
}

/// How a character is drawn, with colors already resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GlyphStyle {
    pub fg: Rgb,
    pub bold: bool,
    pub italic: bool,
    pub underline: Underline,
    pub strikeout: bool,
}

impl GlyphStyle {
    /// Whether the style paints something besides the glyph itself.
    fn draws_line(&self) -> bool {
        self.underline != Underline::None || self.strikeout
    }
}

/// The foreground of a cell and its background if that is not the pane's.
fn cell_colors(cell: &Cell, theme: &TerminalTheme) -> (Rgb, Option<Rgb>) {
    let mut fg = theme.resolve(cell.fg).unwrap_or(theme.foreground);
    let mut bg = theme.resolve(cell.bg);
    if cell.attrs.inverse {
        (fg, bg) = (bg.unwrap_or(theme.background), Some(fg));
    }
    let behind = bg.unwrap_or(theme.background);
    if cell.attrs.dim {
        fg = mix(fg, behind);
    }
    if cell.attrs.hidden {
        fg = behind;
    }
    (fg, bg)
}

/// The color halfway between `a` and `b`.
fn mix(a: Rgb, b: Rgb) -> Rgb {
    let half = |a: u8, b: u8| ((u16::from(a) + u16::from(b)) / 2) as u8;
    Rgb(half(a.0, b.0), half(a.1, b.1), half(a.2, b.2))
}

/// A cell that puts no ink on the screen apart from its background.
fn is_blank(cell: &Cell) -> bool {
    cell.ch == ' '
        && cell.zero_width.is_empty()
        && cell.attrs.underline == Underline::None
        && !cell.attrs.strikeout
}

/// Lays out one row.
pub fn layout_row(row: &Row, theme: &TerminalTheme) -> RowLayout {
    let mut layout = RowLayout::default();
    // The segment being built and the number of trailing blank bytes in
    // it, which are cut off if nothing follows them.
    let mut open: Option<TextSegment> = None;
    let mut trailing_blanks = 0;

    let mut close = |open: &mut Option<TextSegment>, trailing_blanks: &mut usize| {
        if let Some(mut segment) = open.take() {
            segment.text.truncate(segment.text.len() - *trailing_blanks);
            let mut cut = *trailing_blanks;
            while cut > 0 {
                let Some(last) = segment.runs.last_mut() else {
                    break;
                };
                let taken = cut.min(last.len);
                last.len -= taken;
                cut -= taken;
                if last.len == 0 {
                    segment.runs.pop();
                }
            }
            layout.segments.push(segment);
        }
        *trailing_blanks = 0;
    };

    for (col, cell) in row.cells.iter().enumerate() {
        let col = col as u16;
        if cell.width == CellWidth::WideSpacer {
            continue;
        }
        let (fg, bg) = cell_colors(cell, theme);
        let width = if cell.width == CellWidth::Wide { 2 } else { 1 };

        if let Some(color) = bg {
            match layout.backgrounds.last_mut() {
                Some(span) if span.end == col && span.color == color => span.end = col + width,
                _ => layout.backgrounds.push(BackgroundSpan {
                    start: col,
                    end: col + width,
                    color,
                }),
            }
        }

        let blank = is_blank(cell);
        if blank && open.is_none() {
            continue;
        }
        let segment = open.get_or_insert_with(|| TextSegment {
            col,
            text: String::new(),
            runs: Vec::new(),
        });
        let before = segment.text.len();
        segment.text.push(cell.ch);
        segment.text.extend(&cell.zero_width);
        let len = segment.text.len() - before;

        let style = GlyphStyle {
            fg,
            bold: cell.attrs.bold,
            italic: cell.attrs.italic,
            underline: cell.attrs.underline,
            strikeout: cell.attrs.strikeout,
        };
        match segment.runs.last_mut() {
            // A blank shows nothing of its style, so it joins the run
            // before it instead of starting a new one, unless that run
            // draws a line, which would then continue under the blank.
            Some(run) if run.style == style || (blank && !run.style.draws_line()) => {
                run.len += len;
            }
            _ => segment.runs.push(StyleRun { len, style }),
        }

        trailing_blanks = if blank { trailing_blanks + len } else { 0 };
        if cell.width == CellWidth::Wide {
            close(&mut open, &mut trailing_blanks);
        }
    }
    close(&mut open, &mut trailing_blanks);
    layout
}

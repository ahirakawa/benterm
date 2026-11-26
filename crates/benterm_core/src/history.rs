//! Rows as bytes that show them again.
//!
//! What a terminal showed is kept across a restart as the bytes a program
//! would have written to show it: text, line ends and colors, and nothing
//! else. Feeding them is the one way in that every core has, so a core
//! needs nothing more to take back what another core showed.

use crate::screen::{Attributes, Cell, CellWidth, Color, Row, RowEnd, Underline};

/// The bytes that make a core show `rows` again: fed to a core with an
/// empty screen that is as wide as the rows, they give the same rows, and
/// leave the cursor at the start of the row below them.
///
/// The bytes are text, `\r\n` where a line ends, and the escape sequence
/// that sets colors and attributes (SGR). A row that wraps is written
/// without a line end, so a core of another width wraps the line where
/// it fits there.
///
/// A row that wraps comes back as one only if the row after it is given
/// too. A control character in a cell, which no core puts there, comes
/// back as a blank.
pub fn history_bytes(rows: &[Row]) -> Vec<u8> {
    let mut out = String::new();
    let mut style = Style::default();
    // The background of the first cell of a row that the text of the
    // row before goes on into.
    let mut entered: Option<Color> = None;
    for (index, row) in rows.iter().enumerate() {
        let next = rows.get(index + 1);
        let end = match row.end {
            _ if next.is_none() => RowEnd::LineEnd,
            // A core leaves the blank itself only before a wide
            // character, and in the colors of that character.
            RowEnd::WrappedBeforeWide if !left_before(row, next) => RowEnd::Wrapped,
            end => end,
        };
        let cells = match end {
            RowEnd::LineEnd => {
                // The blanks at the end are in a new core anyway. A row
                // the text went on into has them only once something is
                // written there, and has them with the background of
                // what is written if the screen scrolled for the row.
                let least = match entered {
                    None => 0,
                    Some(Color::Default) => 1,
                    Some(_) => row.cells.len(),
                };
                let blank = Cell::default();
                let used = row.cells.iter().rposition(|cell| *cell != blank);
                &row.cells[..used.map_or(0, |last| last + 1).max(least)]
            }
            RowEnd::Wrapped => &row.cells[..],
            // The blank before the wide character is left by the core.
            RowEnd::WrappedBeforeWide => &row.cells[..row.cells.len().saturating_sub(1)],
        };
        for cell in cells {
            if cell.width == CellWidth::WideSpacer {
                continue;
            }
            let wanted = Style::of(cell);
            if wanted != style {
                wanted.write(&mut out);
                style = wanted;
            }
            out.push(if cell.ch.is_control() { ' ' } else { cell.ch });
            out.extend(cell.zero_width.iter().filter(|ch| !ch.is_control()));
        }
        if end == RowEnd::LineEnd {
            // A screen that scrolls for the new row makes it with the
            // background in use.
            if style != Style::default() {
                style = Style::default();
                style.write(&mut out);
            }
            out.push_str("\r\n");
            entered = None;
        } else {
            entered = next.and_then(|next| next.cells.first()).map(|cell| cell.bg);
        }
    }
    out.into_bytes()
}

/// [`history_bytes`] of the last of `rows`: of all of them if that is
/// at most `max` bytes, else of the later half, and so on. Rows with a
/// color change in every cell take many times the bytes of plain ones,
/// and the newest rows are the ones to keep.
pub fn history_bytes_within(rows: &[Row], max: usize) -> Vec<u8> {
    let mut rows = rows;
    loop {
        let bytes = history_bytes(rows);
        if bytes.len() <= max {
            return bytes;
        }
        rows = &rows[rows.len().div_ceil(2)..];
    }
}

/// Whether `bytes` are such as [`history_bytes`] writes: text, `\r`,
/// `\n` and SGR sequences, ending with a line end after which no color
/// or attribute is set. Bytes that are fed to a core as what a terminal
/// showed before are checked with this first, so that a file which is
/// not one of ours, or only the start of one, cannot make the core
/// answer the program, change a mode, raise an event, or color what the
/// program writes next.
pub fn is_history(bytes: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return false;
    };
    let mut styled = false;
    let mut rest = text;
    while let Some(ch) = rest.chars().next() {
        rest = &rest[ch.len_utf8()..];
        match ch {
            '\r' | '\n' => {}
            '\x1b' => {
                let Some(sequence) = rest.strip_prefix('[') else {
                    return false;
                };
                let Some(end) = sequence.find(|ch| !matches!(ch, '0'..='9' | ';' | ':')) else {
                    return false;
                };
                if !sequence[end..].starts_with('m') {
                    return false;
                }
                styled = !matches!(&sequence[..end], "" | "0");
                rest = &sequence[end + 1..];
            }
            ch if ch.is_control() => return false,
            _ => {}
        }
    }
    text.is_empty() || (text.ends_with("\r\n") && !styled)
}

/// Whether a core that wraps the wide character at the start of `next`
/// leaves the blank that `row` ends with.
fn left_before(row: &Row, next: Option<&Row>) -> bool {
    let first = next.and_then(|next| next.cells.first());
    let blank = row.cells.last();
    first.zip(blank).is_some_and(|(first, blank)| {
        first.width == CellWidth::Wide && Style::of(first) == Style::of(blank)
    })
}

/// What SGR sets for the cells written after it.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
struct Style {
    fg: Color,
    bg: Color,
    attrs: Attributes,
}

impl Style {
    fn of(cell: &Cell) -> Self {
        Self {
            fg: cell.fg,
            bg: cell.bg,
            attrs: cell.attrs,
        }
    }

    /// Writes the sequence that sets this style whatever was set before.
    fn write(self, out: &mut String) {
        out.push_str("\x1b[0");
        let attrs = self.attrs;
        let flags = [
            (attrs.bold, ";1"),
            (attrs.dim, ";2"),
            (attrs.italic, ";3"),
            (attrs.inverse, ";7"),
            (attrs.hidden, ";8"),
            (attrs.strikeout, ";9"),
        ];
        for (set, code) in flags {
            if set {
                out.push_str(code);
            }
        }
        out.push_str(match attrs.underline {
            Underline::None => "",
            Underline::Single => ";4",
            Underline::Double => ";4:2",
            Underline::Curly => ";4:3",
            Underline::Dotted => ";4:4",
            Underline::Dashed => ";4:5",
        });
        for (color, code) in [(self.fg, 38), (self.bg, 48)] {
            match color {
                Color::Default => {}
                Color::Indexed(index) => out.push_str(&format!(";{code};5;{index}")),
                Color::Rgb(r, g, b) => out.push_str(&format!(";{code};2;{r};{g};{b}")),
            }
        }
        out.push('m');
    }
}

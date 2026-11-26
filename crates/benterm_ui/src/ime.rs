//! The text an input method is composing.
//!
//! While the user converts kana to kanji, the input method keeps replacing
//! a marked text that only the view shows. The program gets a text when
//! the input method commits it, and [`Composition::commit`] is the only
//! place that hands out bytes, so marked text cannot reach the program.
//!
//! The input method counts in UTF-16 code units, hence the ranges.

use std::ops::Range;

use unicode_width::UnicodeWidthChar;

/// The marked text of the terminal's input method, if it is composing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Composition {
    marked: Option<String>,
}

impl Composition {
    /// The input method shows `text` as what is being composed. An empty
    /// text ends the composition.
    pub fn mark(&mut self, text: &str) {
        self.marked = (!text.is_empty()).then(|| text.to_owned());
    }

    /// The input method commits `text`, which ends the composition.
    /// Returns the bytes to send to the program, if any.
    pub fn commit(&mut self, text: &str) -> Option<Vec<u8>> {
        self.marked = None;
        (!text.is_empty()).then(|| text.as_bytes().to_vec())
    }

    /// Drops the marked text without sending anything.
    pub fn clear(&mut self) {
        self.marked = None;
    }

    /// The text being composed.
    pub fn marked(&self) -> Option<&str> {
        self.marked.as_deref()
    }

    fn text(&self) -> &str {
        self.marked.as_deref().unwrap_or_default()
    }

    /// The range the marked text covers, starting at 0.
    pub fn marked_range_utf16(&self) -> Option<Range<usize>> {
        let marked = self.marked.as_ref()?;
        Some(0..marked.encode_utf16().count())
    }

    /// The selection the input method is told about: the end of the
    /// marked text.
    pub fn selected_range_utf16(&self) -> Range<usize> {
        let end = self.text().encode_utf16().count();
        end..end
    }

    /// The part of the marked text in `range`, and the range it really
    /// covers after clamping to the text.
    pub fn text_in_range_utf16(&self, range: Range<usize>) -> (Range<usize>, String) {
        let units: Vec<u16> = self.text().encode_utf16().collect();
        let end = range.end.min(units.len());
        let start = range.start.min(end);
        (start..end, String::from_utf16_lossy(&units[start..end]))
    }

    /// How many cells the marked text takes before the UTF-16 offset
    /// `offset`. A wide character takes two.
    pub fn columns_before_utf16(&self, offset: usize) -> usize {
        let mut units = 0;
        let mut columns = 0;
        for ch in self.text().chars() {
            units += ch.len_utf16();
            if units > offset {
                break;
            }
            columns += ch.width().unwrap_or(0);
        }
        columns
    }

    /// How many cells the marked text takes.
    pub fn columns(&self) -> usize {
        self.columns_before_utf16(usize::MAX)
    }

    /// The marked text cut into pieces that have one character per cell,
    /// except that a piece may end in a wide character. Each piece comes
    /// with the column it starts at, so that whatever follows a wide
    /// character is placed by its column and not by the font's advance.
    pub fn segments(&self) -> Vec<(usize, &str)> {
        let text = self.text();
        let mut segments = Vec::new();
        let (mut start, mut start_col, mut col) = (0, 0, 0);
        for (index, ch) in text.char_indices() {
            let width = ch.width().unwrap_or(0);
            col += width;
            if width > 1 {
                let end = index + ch.len_utf8();
                segments.push((start_col, &text[start..end]));
                (start, start_col) = (end, col);
            }
        }
        if start < text.len() {
            segments.push((start_col, &text[start..]));
        }
        segments
    }
}

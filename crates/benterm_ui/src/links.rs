//! The web addresses a terminal shows, for a command-click to open.
//!
//! An address is either written out in the text, or given to the text by
//! the program with an OSC 8 hyperlink. Only `http` and `https` addresses
//! are links, as only those open in a browser pane.
//!
//! An address written out ends at a space, at a character that no address
//! has unescaped, or where text other than ASCII begins. Punctuation at
//! its end, and a closing bracket that no bracket inside the address
//! opens, belong to the sentence around it rather than to the address.

use std::ops::Range;
use std::path::{Component, Path, PathBuf};

use benterm_core::{Cell, CellWidth, Row, RowEnd};

use crate::browser::is_page_url;
use crate::selection::Point;

/// An address on the screen and the cells it covers, from `start` to
/// `end` in reading order, both included.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    pub url: String,
    pub start: Point,
    pub end: Point,
}

/// A file or folder path candidate written out on the screen, before
/// checking whether it exists on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathCandidate {
    /// The path text without any trailing `:line` or `:line:col` suffix.
    pub path: String,
    /// The cells covered on screen (including any `:line` / `:line:col`).
    pub start: Point,
    pub end: Point,
}

impl Link {
    /// The columns of `line` the link covers, in a grid of `cols`
    /// columns; empty if it covers none there.
    pub fn columns(&self, line: usize, cols: u16) -> Range<u16> {
        if line < self.start.line || line > self.end.line {
            return 0..0;
        }
        let start = if line == self.start.line {
            self.start.col
        } else {
            0
        };
        let end = if line == self.end.line {
            self.end.col.saturating_add(1)
        } else {
            cols
        };
        start..end.min(cols)
    }
}

/// Where in `text` the `http` and `https` addresses are.
pub fn find_urls(text: &str) -> Vec<Range<usize>> {
    let mut found = Vec::new();
    let mut from = 0;
    while let Some(start) = next_scheme(text, from) {
        let rest = &text[start..];
        let len = rest.find(|c| !is_url_char(c)).unwrap_or(rest.len());
        let end = start + trimmed_len(&rest[..len]);
        if is_page_url(&text[start..end]) {
            found.push(start..end);
            from = end;
        } else {
            // The scheme is ASCII, so the next byte starts a character.
            from = start + 1;
        }
    }
    found
}

/// Where the next `http://` or `https://` that starts a word is, at or
/// after byte `from`.
fn next_scheme(text: &str, from: usize) -> Option<usize> {
    let starts_scheme = |at: usize| {
        let rest = &text.as_bytes()[at..];
        ["http://", "https://"].iter().any(|scheme| {
            rest.get(..scheme.len())
                .is_some_and(|start| start.eq_ignore_ascii_case(scheme.as_bytes()))
        })
    };
    text[from..]
        .char_indices()
        .map(|(offset, _)| from + offset)
        .find(|&at| {
            let in_word = text[..at]
                .chars()
                .next_back()
                .is_some_and(|before| before.is_ascii_alphanumeric());
            !in_word && starts_scheme(at)
        })
}

/// Whether `c` can be part of an address written out.
fn is_url_char(c: char) -> bool {
    c.is_ascii_graphic() && !matches!(c, '<' | '>' | '"' | '`' | '{' | '}' | '|' | '\\' | '^')
}

/// The length of `url` without the punctuation and the unopened closing
/// brackets at its end.
fn trimmed_len(url: &str) -> usize {
    let mut url = url;
    while let Some(last) = url.chars().next_back() {
        let trailing = match last {
            '.' | ',' | ';' | ':' | '!' | '?' | '\'' | '*' => true,
            ')' => unopened(url, '(', ')'),
            ']' => unopened(url, '[', ']'),
            _ => false,
        };
        if !trailing {
            break;
        }
        url = &url[..url.len() - last.len_utf8()];
    }
    url.len()
}

/// Whether `text` closes more brackets than it opens.
fn unopened(text: &str, open: char, close: char) -> bool {
    let count = |bracket| text.chars().filter(|&c| c == bracket).count();
    count(close) > count(open)
}

/// The link at `at` in `rows`, the rows of one line of text, the first
/// of which is line `first_line`. A cell of an OSC 8 hyperlink opens
/// the hyperlink's address, whatever its text says.
pub fn link_at(rows: &[Row], first_line: usize, at: Point) -> Option<Link> {
    let cells = text_cells(rows, first_line);
    let index = cells.iter().position(|cell| {
        cell.point.line == at.line && (cell.point.col..=cell.last_col()).contains(&at.col)
    })?;
    match &cells[index].cell.hyperlink {
        Some(url) => hyperlink_at(&cells, index, url),
        None => written_link_at(&cells, index),
    }
}

/// Whether `url` names a user or a password before its host, as in
/// `https://github.com:login@evil.example/`. A long one pushes the host
/// out of what is shown of the address, so such an address is no link.
fn has_userinfo(url: &str) -> bool {
    url::Url::parse(url).is_ok_and(|url| !url.username().is_empty() || url.password().is_some())
}

/// What is shown of `url` while command is held over its link: its host,
/// then the address. The host is as the parser of addresses reads it,
/// so a host written in letters that look like others shows as the
/// `xn--` name it is. The characters that set the direction of the text
/// are left out, with which the address would show in another order
/// than it is read in.
pub fn shown_target(url: &str) -> String {
    let address: String = url
        .chars()
        .filter(|ch| !crate::agents::changes_direction(*ch))
        .collect();
    match url::Url::parse(url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_owned))
    {
        Some(host) => format!("{host} — {address}"),
        None => address,
    }
}

/// The hyperlink to `url` that the cell at `index` is part of.
fn hyperlink_at(cells: &[TextCell], index: usize, url: &str) -> Option<Link> {
    if !is_page_url(url) || has_userinfo(url) {
        return None;
    }
    let same = |cell: &TextCell| cell.cell.hyperlink.as_deref() == Some(url);
    let first = cells[..index]
        .iter()
        .rposition(|cell| !same(cell))
        .map_or(0, |before| before + 1);
    let last = cells[index..]
        .iter()
        .position(|cell| !same(cell))
        .map_or(cells.len(), |after| index + after)
        - 1;
    Some(Link {
        url: url.to_owned(),
        start: cells[first].point,
        end: cells[last].last_point(),
    })
}

/// The address written out in the text that the cell at `index` is part
/// of.
fn written_link_at(cells: &[TextCell], index: usize) -> Option<Link> {
    let (text, starts) = line_text(cells);
    let at = starts[index];
    let url = find_urls(&text).into_iter().find(|url| url.contains(&at))?;
    if has_userinfo(&text[url.clone()]) {
        return None;
    }
    let first = starts.partition_point(|&start| start < url.start);
    let last = starts.partition_point(|&start| start < url.end) - 1;
    Some(Link {
        url: text[url].to_owned(),
        start: cells[first].point,
        end: cells[last].last_point(),
    })
}

/// The file or folder path candidate at `at` in `rows`, the rows of one
/// line of text, the first of which is line `first_line`. Cells with an
/// OSC 8 hyperlink are not treated as path candidates.
pub fn path_at(rows: &[Row], first_line: usize, at: Point) -> Option<PathCandidate> {
    let cells = text_cells(rows, first_line);
    let index = cells.iter().position(|cell| {
        cell.point.line == at.line && (cell.point.col..=cell.last_col()).contains(&at.col)
    })?;
    if cells[index].cell.hyperlink.is_some() {
        return None;
    }
    let (text, starts) = line_text(&cells);
    let byte_at = starts[index];
    let (span, path) = find_path_candidate(&text, byte_at)?;
    let first = starts.partition_point(|&start| start < span.start);
    let last = starts.partition_point(|&start| start < span.end) - 1;
    Some(PathCandidate {
        path: path.to_owned(),
        start: cells[first].point,
        end: cells[last].last_point(),
    })
}

/// Builds the line text and the starting byte offset in `text` for each
/// cell in `cells`.
fn line_text(cells: &[TextCell]) -> (String, Vec<usize>) {
    let mut text = String::new();
    let mut starts = Vec::with_capacity(cells.len());
    for cell in cells {
        starts.push(text.len());
        text.push(cell.cell.ch);
        text.extend(&cell.cell.zero_width);
    }
    (text, starts)
}

/// Whether `c` can be part of a path token written out in terminal text.
fn is_path_char(c: char) -> bool {
    !c.is_whitespace()
        && !c.is_control()
        && !matches!(c, '<' | '>' | '"' | '\'' | '`' | '{' | '}' | '|')
}

/// Finds the path token covering byte offset `at` in `text`, returning
/// the covered byte span (including any `:line` / `:line:col` suffix)
/// and the path portion without the suffix.
fn find_path_candidate(text: &str, at: usize) -> Option<(Range<usize>, &str)> {
    let ch = text.get(at..)?.chars().next()?;
    if !is_path_char(ch) {
        return None;
    }
    let mut start = at;
    for (offset, c) in text[..at].char_indices().rev() {
        if !is_path_char(c) {
            break;
        }
        start = offset;
    }
    let mut end = at + ch.len_utf8();
    for (offset, c) in text[end..].char_indices() {
        if !is_path_char(c) {
            break;
        }
        end = at + ch.len_utf8() + offset + c.len_utf8();
    }
    let mut slice = &text[start..end];
    while let Some(first) = slice.chars().next() {
        if matches!(first, '(' | '[' | ',' | ';' | ':') {
            start += first.len_utf8();
            slice = &text[start..end];
        } else {
            break;
        }
    }
    while let Some(last) = slice.chars().next_back() {
        let trailing = match last {
            '.' | ',' | ';' | ':' | '!' | '?' | '*' => true,
            ')' => unopened(slice, '(', ')'),
            ']' => unopened(slice, '[', ']'),
            _ => false,
        };
        if !trailing {
            break;
        }
        end -= last.len_utf8();
        slice = &text[start..end];
    }
    if slice.is_empty() || !(start..end).contains(&at) || slice.contains("://") {
        return None;
    }
    let path = strip_line_col(slice);
    if path.is_empty() {
        return None;
    }
    Some((start..end, path))
}

/// Strips a trailing `:line` or `:line:col` suffix from `token`, if any.
fn strip_line_col(token: &str) -> &str {
    let Some((before_last, last)) = token.rsplit_once(':') else {
        return token;
    };
    if before_last.is_empty() || last.is_empty() || !last.bytes().all(|b| b.is_ascii_digit()) {
        return token;
    }
    if let Some((before_line, line)) = before_last.rsplit_once(':')
        && !before_line.is_empty()
        && !line.is_empty()
        && line.bytes().all(|b| b.is_ascii_digit())
    {
        return before_line;
    }
    before_last
}

/// Resolves `raw` against `base` (when relative) and lexically normalizes
/// `.` and `..` without touching the file system.
pub fn resolve_path(raw: &str, base: Option<&Path>) -> Option<PathBuf> {
    let joined = if raw == "~" {
        PathBuf::from(std::env::var_os("HOME")?)
    } else if let Some(rest) = raw.strip_prefix("~/") {
        PathBuf::from(std::env::var_os("HOME")?).join(rest)
    } else if Path::new(raw).is_absolute() {
        PathBuf::from(raw)
    } else {
        base?.join(raw)
    };
    Some(normalize_lexical(&joined))
}

/// Resolves `raw` against `base` and verifies on disk that a file or
/// folder exists there. Intended to be called off the UI thread.
pub fn resolve_existing_path(raw: &str, base: Option<&Path>) -> Option<PathBuf> {
    let resolved = resolve_path(raw, base)?;
    std::fs::metadata(&resolved).ok()?;
    if Path::new(raw).is_absolute() || raw == "~" || raw.starts_with("~/") {
        return Some(std::fs::canonicalize(&resolved).unwrap_or(resolved));
    }
    Some(resolved)
}

/// Lexically normalizes `.` and `..` components and strips trailing slashes.
fn normalize_lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push(component.as_os_str());
                }
            }
            Component::RootDir | Component::Prefix(_) | Component::Normal(_) => {
                out.push(component.as_os_str());
            }
        }
    }
    out
}

/// A cell that holds a character of the text, and where it is.
struct TextCell<'a> {
    point: Point,
    cell: &'a Cell,
}

impl TextCell<'_> {
    /// The last column the character takes up.
    fn last_col(&self) -> u16 {
        match self.cell.width {
            CellWidth::Wide => self.point.col.saturating_add(1),
            _ => self.point.col,
        }
    }

    fn last_point(&self) -> Point {
        Point {
            col: self.last_col(),
            ..self.point
        }
    }
}

/// The cells of `rows` that hold a character, in reading order. The
/// second half of a wide character, and the blank that a wide character
/// too wide for the end of a row leaves there, hold none, as in a copy.
fn text_cells(rows: &[Row], first_line: usize) -> Vec<TextCell<'_>> {
    let mut cells = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let used = match row.end {
            RowEnd::WrappedBeforeWide => row.cells.len().saturating_sub(1),
            RowEnd::LineEnd | RowEnd::Wrapped => row.cells.len(),
        };
        for (col, cell) in row.cells[..used].iter().enumerate() {
            if cell.width == CellWidth::WideSpacer {
                continue;
            }
            cells.push(TextCell {
                point: Point {
                    line: first_line + index,
                    col: col as u16,
                },
                cell,
            });
        }
    }
    cells
}

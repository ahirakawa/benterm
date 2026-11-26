//! In-pane terminal text search (Cmd-F).
//!
//! Searches across both scrollback history and the active screen, supporting
//! case-insensitive (default) and case-sensitive matching, wide characters,
//! and wrapped logical lines.

use std::ops::Range;

use benterm_core::{CellWidth, Row, RowEnd};
use gpui::{App, KeyBinding, actions};

actions!(
    terminal_search,
    [
        /// Opens the search bar for the focused terminal pane.
        OpenSearch,
        FindNext,
        FindPrevious,
    ]
);

/// Key context active while the terminal search bar has focus.
pub(crate) const KEY_CONTEXT: &str = "TerminalSearch";

/// Binds keys for terminal search.
///
/// `cmd-f` opens search from a focused terminal pane (or re-focuses it when
/// already open). `shift-enter`, `cmd-g`, and `cmd-shift-g` navigate matches
/// while the search bar has focus; binding `cmd-shift-g` inside
/// [`KEY_CONTEXT`] lets it take precedence over window-level bindings while
/// searching without interfering with them when search is closed.
pub fn bind_keys(cx: &mut App) {
    let terminal = Some(crate::terminal_view::KEY_CONTEXT);
    let search = Some(KEY_CONTEXT);
    cx.bind_keys([
        KeyBinding::new("cmd-f", OpenSearch, terminal),
        KeyBinding::new("cmd-f", OpenSearch, search),
        KeyBinding::new("shift-enter", FindPrevious, search),
        KeyBinding::new("cmd-g", FindNext, search),
        KeyBinding::new("cmd-shift-g", FindPrevious, search),
    ]);
}

/// A single match in the terminal buffer (scrollback + screen).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchMatch {
    /// The first line containing this match (0 = oldest scrollback row).
    pub start_line: usize,
    /// Column spans covered by this match, one per row it touches.
    pub spans: Vec<(usize, Range<u16>)>,
}

/// Finds all non-overlapping matches of `query` in `rows`.
///
/// `first_line` is the line index of `rows[0]` (typically `0` when searching
/// from the oldest scrollback row). Adjacent rows connected by [`RowEnd::wraps`]
/// are searched as a single logical line so matches spanning a wrap boundary
/// are found.
pub fn find_matches(
    rows: &[Row],
    first_line: usize,
    query: &str,
    case_sensitive: bool,
) -> Vec<SearchMatch> {
    if query.is_empty() || rows.is_empty() {
        return Vec::new();
    }
    let needle = if case_sensitive {
        query.to_owned()
    } else {
        query.to_lowercase()
    };
    if needle.is_empty() {
        return Vec::new();
    }

    let mut matches = Vec::new();
    let mut row_idx = 0;
    while row_idx < rows.len() {
        let group_start = row_idx;
        while row_idx + 1 < rows.len() && rows[row_idx].end.wraps() {
            row_idx += 1;
        }
        let group_end = row_idx + 1;
        search_logical_line(
            &rows[group_start..group_end],
            first_line + group_start,
            &needle,
            case_sensitive,
            &mut matches,
        );
        row_idx = group_end;
    }
    matches
}

/// Searches one logical line (`group` of wrapped rows starting at `base_line`)
/// and appends any non-overlapping matches of `needle` to `out`.
fn search_logical_line(
    group: &[Row],
    base_line: usize,
    needle: &str,
    case_sensitive: bool,
    out: &mut Vec<SearchMatch>,
) {
    // Build the logical line text together with a per-byte mapping back to
    // `(line, col_start, col_end)`. Mapping per byte keeps Unicode case-folding
    // expansions (where 1 char becomes multiple chars) completely safe.
    let mut text = String::new();
    let mut byte_map: Vec<(usize, u16, u16)> = Vec::new();

    for (offset, row) in group.iter().enumerate() {
        let line = base_line + offset;
        let is_last_in_group = offset + 1 == group.len();
        let mut cell_len = match row.end {
            RowEnd::WrappedBeforeWide => row.cells.len().saturating_sub(1),
            _ => row.cells.len(),
        };
        if is_last_in_group && !row.end.wraps() {
            while cell_len > 0 {
                let cell = &row.cells[cell_len - 1];
                if cell.ch == ' ' && cell.zero_width.is_empty() && cell.width == CellWidth::Narrow {
                    cell_len -= 1;
                } else {
                    break;
                }
            }
        }
        for (col, cell) in row.cells[..cell_len].iter().enumerate() {
            if cell.width == CellWidth::WideSpacer {
                continue;
            }
            let col_start = col as u16;
            let col_end =
                col_start.saturating_add(if cell.width == CellWidth::Wide { 2 } else { 1 });
            let mut piece = String::with_capacity(4);
            piece.push(cell.ch);
            piece.extend(&cell.zero_width);
            let mapped = if case_sensitive {
                piece
            } else {
                piece.to_lowercase()
            };
            let bytes = mapped.len();
            text.push_str(&mapped);
            byte_map.extend(std::iter::repeat_n((line, col_start, col_end), bytes));
        }
    }

    if text.len() < needle.len() {
        return;
    }

    let mut pos = 0;
    while pos + needle.len() <= text.len() {
        let Some(found) = text[pos..].find(needle) else {
            break;
        };
        let match_start = pos + found;
        let match_end = match_start + needle.len();
        let mut spans: Vec<(usize, Range<u16>)> = Vec::new();
        for &(line, col_start, col_end) in &byte_map[match_start..match_end] {
            if let Some((last_line, last_range)) = spans.last_mut()
                && *last_line == line
            {
                last_range.start = last_range.start.min(col_start);
                last_range.end = last_range.end.max(col_end);
            } else {
                spans.push((line, col_start..col_end));
            }
        }
        if let Some(&(start_line, _)) = spans.first() {
            out.push(SearchMatch { start_line, spans });
        }
        pos = match_end;
    }
}

/// Pure search state: current query, match list, active match index, and options.
#[derive(Debug, Clone, Default)]
pub(crate) struct SearchState {
    query: String,
    case_sensitive: bool,
    matches: Vec<SearchMatch>,
    current: Option<usize>,
}

impl SearchState {
    pub(crate) fn query(&self) -> &str {
        &self.query
    }

    pub(crate) fn case_sensitive(&self) -> bool {
        self.case_sensitive
    }

    pub(crate) fn toggle_case_sensitive(&mut self) {
        self.case_sensitive = !self.case_sensitive;
    }

    pub(crate) fn matches(&self) -> &[SearchMatch] {
        &self.matches
    }

    pub(crate) fn current_index(&self) -> Option<usize> {
        self.current
    }

    pub(crate) fn current_match(&self) -> Option<&SearchMatch> {
        self.current.and_then(|idx| self.matches.get(idx))
    }

    /// Recomputes matches against `rows`. When `reset_current` is true (e.g.
    /// query or case-sensitivity changed), selects the first match; otherwise
    /// preserves the current index clamped to the new match count.
    pub(crate) fn update(
        &mut self,
        query: &str,
        rows: &[Row],
        first_line: usize,
        reset_current: bool,
    ) {
        self.query = query.to_owned();
        self.matches = find_matches(rows, first_line, query, self.case_sensitive);
        if self.matches.is_empty() {
            self.current = None;
        } else if reset_current {
            self.current = Some(0);
        } else {
            self.current = Some(
                self.current
                    .unwrap_or(0)
                    .min(self.matches.len().saturating_sub(1)),
            );
        }
    }

    /// Advances to the next match, wrapping around to the first.
    pub(crate) fn next(&mut self) -> Option<&SearchMatch> {
        if self.matches.is_empty() {
            self.current = None;
            return None;
        }
        let next = match self.current {
            Some(idx) => (idx + 1) % self.matches.len(),
            None => 0,
        };
        self.current = Some(next);
        self.matches.get(next)
    }

    /// Moves to the previous match, wrapping around to the last.
    pub(crate) fn previous(&mut self) -> Option<&SearchMatch> {
        if self.matches.is_empty() {
            self.current = None;
            return None;
        }
        let prev = match self.current {
            Some(0) | None => self.matches.len() - 1,
            Some(idx) => idx - 1,
        };
        self.current = Some(prev);
        self.matches.get(prev)
    }

    /// Human-readable counter (`"1/3"`, `"0/0"`, or `""` when query is empty).
    pub(crate) fn status_label(&self) -> String {
        if self.query.is_empty() {
            return String::new();
        }
        if self.matches.is_empty() {
            return "0/0".to_owned();
        }
        let current = self.current.unwrap_or(0) + 1;
        format!("{current}/{}", self.matches.len())
    }
}

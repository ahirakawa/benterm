//! Which lines are visible, for scrolling by pixels instead of by rows.

use benterm_core::GridSize;
use benterm_terminal::WindowSize;

/// The grid that fits a pane of `pane` (width, height) pixels with `padding`
/// on every side and cells of `cell` (width, height) pixels. Cells smaller
/// than a pixel are treated as one pixel.
pub fn grid_size(pane: (f32, f32), padding: f32, cell: (f32, f32)) -> GridSize {
    let fit = |pane: f32, cell: f32| {
        let cells = ((pane - 2.0 * padding) / cell.max(1.0)).floor();
        cells.clamp(0.0, f32::from(u16::MAX)) as u16
    };
    GridSize::new(fit(pane.1, cell.1), fit(pane.0, cell.0)).at_least_min()
}

/// The size of the terminal of a pane: the grid that fits the pane, as
/// [`grid_size`] has it, and the pixels its cells cover.
///
/// This is the one rule for the size of every terminal. A pane on view
/// is given its own bounds. A pane that is not on view is given the place
/// its grid would put it in if the window showed the grid now, and a
/// terminal without a pane the whole of the window's pane area.
pub fn terminal_size(pane: (f32, f32), padding: f32, cell: (f32, f32)) -> WindowSize {
    let grid = grid_size(pane, padding, cell);
    let pixels = |cells: u16, cell: f32| (cell * f32::from(cells)) as u16;
    WindowSize {
        rows: grid.rows,
        cols: grid.cols,
        pixel_width: pixels(grid.cols, cell.0),
        pixel_height: pixels(grid.rows, cell.1),
    }
}

/// How far the view is scrolled up from the bottom, in pixels.
///
/// Lines are numbered from the oldest scrollback row (0) to the last
/// screen row (`scrollback_len + rows - 1`).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Scroll {
    /// Zero shows the screen itself; `scrollback_len * line_height` shows
    /// the oldest row at the top.
    offset: f32,
}

/// The lines to draw for one frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Visible {
    /// The line at the top of the pane.
    pub first_line: usize,
    /// How many pixels of `first_line` are cut off above the pane.
    pub shift: f32,
    /// Number of lines that are at least partly visible.
    pub line_count: usize,
}

impl Scroll {
    pub fn is_at_bottom(&self) -> bool {
        self.offset == 0.0
    }

    /// Moves the view up by `delta` pixels (down if negative), stopping at
    /// the oldest row and at the bottom.
    pub fn scroll_by(&mut self, delta: f32, line_height: f32, scrollback_len: usize) {
        let max = scrollback_len as f32 * line_height;
        self.offset = (self.offset + delta).clamp(0.0, max);
    }

    /// Call when the number of scrollback rows changes or oldest rows are
    /// evicted. A view that is scrolled up keeps showing the same lines while
    /// output arrives; a view at the bottom stays at the bottom.
    pub fn scrollback_changed(
        &mut self,
        old_len: usize,
        new_len: usize,
        evicted: usize,
        line_height: f32,
    ) {
        if !self.is_at_bottom() {
            let pushed = (new_len as f64 - old_len as f64) + evicted as f64;
            self.offset += (pushed * f64::from(line_height)) as f32;
        }
        self.scroll_by(0.0, line_height, new_len);
    }

    /// The lines visible in a pane of `rows` rows.
    pub fn visible(&self, line_height: f32, rows: u16, scrollback_len: usize) -> Visible {
        let max = scrollback_len as f32 * line_height;
        let top = max - self.offset.clamp(0.0, max);
        let first_line = (top / line_height).floor() as usize;
        let shift = top - first_line as f32 * line_height;
        Visible {
            first_line,
            shift,
            line_count: usize::from(rows) + usize::from(shift > 0.0),
        }
    }

    /// Scrolls so that `line` is visible in a viewport of `rows` rows,
    /// centering it when it is outside the currently visible range.
    pub fn scroll_to_line(
        &mut self,
        line: usize,
        line_height: f32,
        rows: u16,
        scrollback_len: usize,
    ) {
        let visible = self.visible(line_height, rows, scrollback_len);
        let rows_usize = usize::from(rows).max(1);
        let first_full = visible.first_line + usize::from(visible.shift > 0.0);
        let last_full = visible.first_line + rows_usize;
        if line >= first_full && line < last_full {
            return;
        }
        let max = scrollback_len as f32 * line_height;
        let target_first = line.saturating_sub(rows_usize / 2);
        let desired_offset = (max - target_first as f32 * line_height).clamp(0.0, max);
        let delta = desired_offset - self.offset;
        self.scroll_by(delta, line_height, scrollback_len);
    }
}

//! Smooth scrolling through the scrollback.

use benterm_core::{AlacrittyCore, GridSize, TerminalCore};
use benterm_terminal::WindowSize;
use benterm_ui::viewport::{Scroll, grid_size, terminal_size};

const LINE: f32 = 20.0;

#[test]
fn at_the_bottom_the_screen_rows_are_visible() {
    let visible = Scroll::default().visible(LINE, 10, 100);
    assert_eq!(visible.first_line, 100);
    assert_eq!(visible.shift, 0.0);
    assert_eq!(visible.line_count, 10);
}

#[test]
fn scrolling_up_by_part_of_a_line_shows_one_more_line() {
    let mut scroll = Scroll::default();
    scroll.scroll_by(5.0, LINE, 100);
    let visible = scroll.visible(LINE, 10, 100);
    assert_eq!(visible.first_line, 99);
    assert_eq!(visible.shift, 15.0);
    assert_eq!(visible.line_count, 11);
}

#[test]
fn scrolling_up_by_whole_lines_stays_aligned() {
    let mut scroll = Scroll::default();
    scroll.scroll_by(3.0 * LINE, LINE, 100);
    let visible = scroll.visible(LINE, 10, 100);
    assert_eq!(
        (visible.first_line, visible.shift, visible.line_count),
        (97, 0.0, 10)
    );
}

#[test]
fn scrolling_stops_at_the_oldest_line_and_at_the_bottom() {
    let mut scroll = Scroll::default();
    scroll.scroll_by(1e9, LINE, 100);
    assert_eq!(scroll.visible(LINE, 10, 100).first_line, 0);
    assert_eq!(scroll.visible(LINE, 10, 100).shift, 0.0);
    scroll.scroll_by(-1e12, LINE, 100);
    assert!(scroll.is_at_bottom());
    assert_eq!(scroll.visible(LINE, 10, 100).first_line, 100);
}

#[test]
fn without_scrollback_nothing_scrolls() {
    let mut scroll = Scroll::default();
    scroll.scroll_by(50.0, LINE, 0);
    assert!(scroll.is_at_bottom());
}

#[test]
fn new_output_keeps_the_bottom_at_the_bottom() {
    let mut scroll = Scroll::default();
    scroll.scrollback_changed(100, 105, 0, LINE);
    assert!(scroll.is_at_bottom());
    // Also at the scrollback limit when lines are evicted.
    scroll.scrollback_changed(105, 105, 3, LINE);
    assert!(scroll.is_at_bottom());
}

#[test]
fn new_output_does_not_move_a_scrolled_view() {
    let mut scroll = Scroll::default();
    scroll.scroll_by(50.0, LINE, 100);
    let before = scroll.visible(LINE, 10, 100);
    scroll.scrollback_changed(100, 105, 0, LINE);
    let after = scroll.visible(LINE, 10, 105);
    assert_eq!(after.first_line, before.first_line);
    assert_eq!(after.shift, before.shift);
}

#[test]
fn a_shrinking_scrollback_clamps_the_view() {
    let mut scroll = Scroll::default();
    scroll.scroll_by(90.0 * LINE, LINE, 100);
    scroll.scrollback_changed(100, 0, 0, LINE);
    assert!(scroll.is_at_bottom());
}

#[test]
fn output_at_the_scrollback_limit_keeps_the_same_first_visible_line() {
    let mut core = AlacrittyCore::new(GridSize::new(3, 12), 5);
    core.feed(b"line 0\r\nline 1\r\nline 2\r\nline 3\r\nline 4\r\nline 5\r\nline 6\r\nline 7");
    let old_len = core.scrollback_len();
    let old_evicted = core.scrollback_evicted();
    assert_eq!(old_len, 5);

    let mut scroll = Scroll::default();
    // Scroll up 3 lines so "line 2" (scrollback index 2) is at the top.
    scroll.scroll_by(3.0 * LINE, LINE, old_len);
    let before = scroll.visible(LINE, 3, old_len);
    assert_eq!(before.first_line, 2);
    assert_eq!(
        core.scrollback_rows(before.first_line..before.first_line + 1)[0]
            .text()
            .trim_end(),
        "line 2"
    );

    // One more line arrives at the limit, evicting "line 0".
    core.feed(b"\r\nline 8");
    let new_len = core.scrollback_len();
    let evicted = (core.scrollback_evicted() - old_evicted) as usize;
    assert_eq!(new_len, 5);
    assert_eq!(evicted, 1);
    scroll.scrollback_changed(old_len, new_len, evicted, LINE);

    let after = scroll.visible(LINE, 3, new_len);
    assert_eq!(after.first_line, 1);
    let top_row = core.scrollback_rows(after.first_line..after.first_line + 1);
    assert_eq!(top_row[0].text().trim_end(), "line 2");

    // Evicting past "line 2" clamps the view to the oldest remaining line.
    let old_evicted = core.scrollback_evicted();
    core.feed(b"\r\nline 9\r\nline 10");
    let evicted = (core.scrollback_evicted() - old_evicted) as usize;
    assert_eq!(evicted, 2);
    scroll.scrollback_changed(new_len, core.scrollback_len(), evicted, LINE);
    let clamped = scroll.visible(LINE, 3, core.scrollback_len());
    assert_eq!(clamped.first_line, 0);
    assert_eq!(core.scrollback_rows(0..1)[0].text().trim_end(), "line 3");
}

#[test]
fn the_grid_fills_the_pane_inside_its_padding() {
    // 820 - 2 * 10 = 800 pixels hold 100 cells of 8; 420 - 20 = 400 hold 20 rows of 20.
    assert_eq!(
        grid_size((820.0, 420.0), 10.0, (8.0, 20.0)),
        GridSize::new(20, 100)
    );
}

#[test]
fn a_partial_cell_is_not_part_of_the_grid() {
    assert_eq!(
        grid_size((835.0, 439.0), 10.0, (8.0, 20.0)),
        GridSize::new(20, 101)
    );
}

#[test]
fn a_tiny_pane_and_a_missing_cell_size_still_give_a_grid() {
    assert_eq!(grid_size((5.0, 5.0), 10.0, (8.0, 20.0)), GridSize::MIN);
    assert_eq!(
        grid_size((800.0, 400.0), 0.0, (0.0, 0.0)),
        GridSize::new(400, 800)
    );
}

#[test]
fn a_terminal_has_the_grid_of_its_pane_and_the_pixels_its_cells_cover() {
    // 101 cells of 8 pixels and 20 rows of 20; the rest of the pane is
    // padding and a partial cell.
    let size = terminal_size((835.0, 439.0), 10.0, (8.0, 20.0));
    assert_eq!(
        size,
        WindowSize {
            rows: 20,
            cols: 101,
            pixel_width: 808,
            pixel_height: 400,
        }
    );
    let grid = grid_size((835.0, 439.0), 10.0, (8.0, 20.0));
    assert_eq!((size.rows, size.cols), (grid.rows, grid.cols));
}

#[test]
fn a_terminal_in_a_tiny_pane_has_the_smallest_grid() {
    let size = terminal_size((5.0, 5.0), 10.0, (8.0, 20.0));
    assert_eq!(
        (size.rows, size.cols),
        (GridSize::MIN.rows, GridSize::MIN.cols)
    );
    assert_eq!((size.pixel_width, size.pixel_height), (16, 20));
}

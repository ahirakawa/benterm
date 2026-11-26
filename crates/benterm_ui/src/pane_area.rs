//! Where a window shows its panes, and the size of a terminal there.
//!
//! Only the grid on view is laid out, so only its panes are measured. A
//! terminal that is not on view is sized from the same measure: a pane of
//! a grid behind has the place that grid would get, and a terminal with a
//! pane to itself, or with no pane at all, has the whole area. The size
//! itself is [`terminal_size`](crate::viewport::terminal_size) for all of
//! them.
//!
//! A terminal gets that size when it starts and when its grid gives its
//! pane another place: when a pane of the grid is split or closed. A change of the window alone is not told to the
//! terminals behind: dragging the edge of the window passes through many
//! sizes, and each would make every program behind draw its screen again.
//! They are fitted to the size the window has by then when they are
//! shown, or when a pane of their grid is split or closed.
//!
//! The measure is kept per window, not read from a grid: a grid keeps the
//! place it was last drawn in, which is out of date once the window has
//! changed while another grid was shown.

use std::collections::HashMap;

use benterm_terminal::WindowSize;
use gpui::{App, Bounds, Global, Pixels, Window, WindowId, px};

use crate::BentermWindow;
use crate::pane_grid::lone_pane_bounds;
use crate::terminal_view::TerminalView;
use crate::theme::TerminalTheme;

/// Where each window last laid out a grid, in window coordinates.
#[derive(Default)]
struct PaneAreas(HashMap<WindowId, Bounds<Pixels>>);

impl Global for PaneAreas {}

/// Notes where `window` laid out the grid it shows in this frame.
///
/// With the first measure of a window, the grids behind are laid out in
/// it too. Their terminals started before there was a measure, as those
/// of a session that is made again do, and no later frame fits them
/// until they are shown.
pub(crate) fn measured(area: Bounds<Pixels>, window: &Window, cx: &mut App) {
    if area.size.width <= px(0.) || area.size.height <= px(0.) {
        return;
    }
    let id = window.window_handle().window_id();
    let first = cx
        .default_global::<PaneAreas>()
        .0
        .insert(id, area)
        .is_none();
    if first {
        // After the frame: the window's views cannot be updated while
        // they are drawn.
        window.defer(cx, move |window, cx| {
            if let Some(Some(root)) = window.root::<BentermWindow>() {
                root.update(cx, |root, cx| root.lay_out_grids_never_shown(area, cx));
            }
        });
    }
}

impl BentermWindow {
    /// Lays out every grid that no frame has laid out yet in `area`, and
    /// tells its terminals their sizes.
    fn lay_out_grids_never_shown(&self, area: Bounds<Pixels>, cx: &mut App) {
        for workspace in self.workspaces() {
            let Some(tabs) = self.workspace_tabs(workspace) else {
                continue;
            };
            let grids: Vec<_> = {
                let tabs = tabs.read(cx);
                let ids = tabs.tabs();
                ids.into_iter()
                    .filter_map(|tab| tabs.grid(tab).cloned())
                    .collect()
            };
            for grid in grids {
                grid.update(cx, |grid, cx| grid.lay_out_if_never_shown(area, cx));
            }
        }
    }
}

/// Where `window` shows its panes, as of its last frame. `None` before
/// the first frame.
pub(crate) fn pane_area(window: &Window, cx: &App) -> Option<Bounds<Pixels>> {
    let id = window.window_handle().window_id();
    cx.try_global::<PaneAreas>()?.0.get(&id).copied()
}

/// The size of a terminal drawn with `theme` that fills the pane area of
/// `window`: the first pane of a new tab or workspace, shown or not, and
/// a terminal that has no pane. `None` before the first frame.
pub(crate) fn terminal_size(
    window: &Window,
    theme: &TerminalTheme,
    cx: &App,
) -> Option<WindowSize> {
    let place = lone_pane_bounds(pane_area(window, cx)?);
    Some(TerminalView::size_in(place.size, theme, cx))
}

#[cfg(test)]
mod tests {
    use gpui::{point, size};

    use super::*;

    /// A frame lays the panes out in whole pixels, so a terminal started
    /// for an area with half a pixel must not count that half: it could
    /// be the one that makes room for another row, and the first frame
    /// would then take the row away again.
    #[test]
    fn a_pane_to_itself_has_the_whole_pixels_of_the_area() {
        let area = Bounds::new(point(px(240.), px(32.)), size(px(860.5), px(420.5)));
        let place = lone_pane_bounds(area);
        assert_eq!(place.origin, area.origin);
        assert_eq!(place.size, size(px(860.), px(420.)));
    }
}

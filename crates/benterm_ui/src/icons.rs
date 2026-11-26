//! The small icons of the sidebar, drawn as paths of our own: the kinds
//! of the agents, and the eye that opens a peek. Each is drawn in one
//! color of the theme, so the shapes are what tells them apart.

use gpui::{Bounds, Hsla, IntoElement, PathBuilder, Pixels, Styled, Window, canvas, point, px};

use crate::session_rows::AgentIcon;

/// The width of the lines of an icon.
const STROKE: f32 = 1.5;

/// Strokes the line through `points`, given in sixteenths of the icon's
/// side, closing it if `closed`.
fn stroke(
    window: &mut Window,
    bounds: Bounds<Pixels>,
    color: Hsla,
    points: &[(f32, f32)],
    closed: bool,
) {
    let side = f32::from(bounds.size.width);
    let at = |(x, y): (f32, f32)| bounds.origin + point(px(x / 16. * side), px(y / 16. * side));
    let Some((first, rest)) = points.split_first() else {
        return;
    };
    let mut line = PathBuilder::stroke(px(STROKE));
    line.move_to(at(*first));
    for next in rest {
        line.line_to(at(*next));
    }
    if closed {
        line.close();
    }
    if let Ok(path) = line.build() {
        window.paint_path(path, color);
    }
}

/// The lines of the icon of a kind of agent.
fn agent_lines(icon: AgentIcon) -> &'static [(&'static [(f32, f32)], bool)] {
    match icon {
        // A star of three strokes.
        AgentIcon::ClaudeCode => &[
            (&[(8., 2.), (8., 14.)], false),
            (&[(2.8, 5.), (13.2, 11.)], false),
            (&[(2.8, 11.), (13.2, 5.)], false),
        ],
        // A hexagon with a prompt in it.
        AgentIcon::Codex => &[
            (
                &[
                    (8., 1.5),
                    (13.6, 4.75),
                    (13.6, 11.25),
                    (8., 14.5),
                    (2.4, 11.25),
                    (2.4, 4.75),
                ],
                true,
            ),
            (&[(6., 6.2), (7.8, 8.), (6., 9.8)], false),
            (&[(8.8, 10.), (10.6, 10.)], false),
        ],
        // A diamond.
        AgentIcon::Agent => &[(&[(8., 2.), (14., 8.), (8., 14.), (2., 8.)], true)],
        // A window with a prompt in it.
        AgentIcon::Program => &[
            (&[(2.5, 3.), (13.5, 3.), (13.5, 13.), (2.5, 13.)], true),
            (&[(5., 6.5), (7., 8.), (5., 9.5)], false),
            (&[(8.5, 10.), (11., 10.)], false),
        ],
    }
}

/// The icon of a kind of agent, `side` points wide and high.
pub(crate) fn agent(icon: AgentIcon, color: Hsla, side: f32) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |bounds, (), window, _| {
            for (points, closed) in agent_lines(icon) {
                stroke(window, bounds, color, points, *closed);
            }
        },
    )
    .flex_none()
    .size(px(side))
}

/// An eye, for what opens a peek.
pub(crate) fn eye(color: Hsla, side: f32) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |bounds, (), window, _| {
            let scale = f32::from(bounds.size.width) / 16.;
            let at = |x: f32, y: f32| bounds.origin + point(px(x * scale), px(y * scale));
            let mut lids = PathBuilder::stroke(px(STROKE));
            lids.move_to(at(1.5, 8.));
            lids.curve_to(at(14.5, 8.), at(8., 0.5));
            lids.curve_to(at(1.5, 8.), at(8., 15.5));
            if let Ok(path) = lids.build() {
                window.paint_path(path, color);
            }
            let mut pupil = PathBuilder::fill();
            pupil.move_to(at(8., 5.8));
            pupil.curve_to(at(10.2, 8.), at(10.2, 5.8));
            pupil.curve_to(at(8., 10.2), at(10.2, 10.2));
            pupil.curve_to(at(5.8, 8.), at(5.8, 10.2));
            pupil.curve_to(at(8., 5.8), at(5.8, 5.8));
            pupil.close();
            if let Ok(path) = pupil.build() {
                window.paint_path(path, color);
            }
        },
    )
    .flex_none()
    .size(px(side))
}

/// A folder icon on the same 16x16 / 1.5px stroke grid.
pub(crate) fn folder(open: bool, color: Hsla, side: f32) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |bounds, (), window, _| {
            if open {
                stroke(
                    window,
                    bounds,
                    color,
                    &[
                        (2., 12.5),
                        (2., 4.),
                        (6.2, 4.),
                        (7.6, 5.6),
                        (13.5, 5.6),
                        (13.5, 7.2),
                    ],
                    false,
                );
                stroke(
                    window,
                    bounds,
                    color,
                    &[(2., 12.5), (3.8, 7.6), (14.5, 7.6), (12.8, 12.5)],
                    true,
                );
            } else {
                stroke(
                    window,
                    bounds,
                    color,
                    &[
                        (2., 4.),
                        (6.2, 4.),
                        (7.6, 5.6),
                        (14., 5.6),
                        (14., 12.5),
                        (2., 12.5),
                    ],
                    true,
                );
            }
        },
    )
    .flex_none()
    .size(px(side))
}

/// A document icon with a folded corner on the 16x16 / 1.5px stroke grid.
pub(crate) fn file(color: Hsla, side: f32) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |bounds, (), window, _| {
            stroke(
                window,
                bounds,
                color,
                &[(3.5, 2.), (9.5, 2.), (12.5, 5.), (12.5, 14.), (3.5, 14.)],
                true,
            );
            stroke(
                window,
                bounds,
                color,
                &[(9.5, 2.), (9.5, 5.), (12.5, 5.)],
                false,
            );
        },
    )
    .flex_none()
    .size(px(side))
}

/// A magnifying-glass search icon on the 16x16 / 1.5px stroke grid.
pub(crate) fn search(color: Hsla, side: f32) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |bounds, (), window, _| {
            let scale = f32::from(bounds.size.width) / 16.;
            let at = |x: f32, y: f32| bounds.origin + point(px(x * scale), px(y * scale));
            let mut ring = PathBuilder::stroke(px(STROKE));
            ring.move_to(at(7., 2.5));
            ring.curve_to(at(11.5, 7.), at(11.5, 2.5));
            ring.curve_to(at(7., 11.5), at(11.5, 11.5));
            ring.curve_to(at(2.5, 7.), at(2.5, 11.5));
            ring.curve_to(at(7., 2.5), at(2.5, 2.5));
            ring.close();
            if let Ok(path) = ring.build() {
                window.paint_path(path, color);
            }
            stroke(window, bounds, color, &[(10.4, 10.4), (13.8, 13.8)], false);
        },
    )
    .flex_none()
    .size(px(side))
}

/// A crisp close (`×`) icon on the 16x16 / 1.5px stroke grid.
pub(crate) fn close(color: Hsla, side: f32) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |bounds, (), window, _| {
            stroke(window, bounds, color, &[(4., 4.), (12., 12.)], false);
            stroke(window, bounds, color, &[(12., 4.), (4., 12.)], false);
        },
    )
    .flex_none()
    .size(px(side))
}

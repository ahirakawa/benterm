//! Shared visual design tokens and UI primitives derived from [`Theme`].
//!
//! All colors are derived from the user's configured theme colors so that
//! Dark, Light, and custom themes (such as Solarized) keep consistent depth,
//! contrast, and hierarchy without adding new config keys.

use benterm_core::Rgb;
use gpui::{
    Div, FontWeight, Hsla, ParentElement, SharedString, Styled, div, hsla as gpui_hsla, px,
};

use crate::terminal_view::hsla;
use crate::theme::{RADIUS_CONTROL, lifted, small_text};

/// Returns whether `color` is light enough to be treated as a light surface.
pub fn is_light(color: Rgb) -> bool {
    let luma = 0.299 * f32::from(color.0) + 0.587 * f32::from(color.1) + 0.114 * f32::from(color.2);
    luma > 140.
}

/// Returns `color` with its alpha channel set to `alpha` (`0.0..=1.0`).
pub fn with_alpha(color: Rgb, alpha: f32) -> Hsla {
    let mut c = hsla(color);
    c.a = alpha.clamp(0., 1.);
    c
}

/// A subtle 1px top inner highlight color for elevated surfaces (cards,
/// floating panels, active tabs, and buttons).
pub fn top_highlight(surface: Rgb) -> Hsla {
    if is_light(surface) {
        gpui_hsla(0., 0., 1., 0.72)
    } else {
        gpui_hsla(0., 0., 1., 0.08)
    }
}

/// A softer separator color between `border` and `background` for inset
/// frames and card outlines.
pub fn subtle_border(border: Rgb, background: Rgb) -> Hsla {
    let mix = |from: u8, to: u8| {
        let (from, to) = (f32::from(from), f32::from(to));
        (from + (to - from) * 0.45).round() as u8
    };
    hsla(Rgb(
        mix(border.0, background.0),
        mix(border.1, background.1),
        mix(border.2, background.2),
    ))
}

/// A translucent backdrop color placed behind floating modals to separate them
/// from bright or busy terminal output.
pub fn scrim(background: Rgb) -> Hsla {
    if is_light(background) {
        gpui_hsla(0., 0., 0., 0.22)
    } else {
        gpui_hsla(0., 0., 0., 0.46)
    }
}

/// A 1px highlight strip pinned to the top edge of an elevated, `relative()`
/// and `overflow_hidden()` surface.
pub fn top_highlight_bar(surface: Rgb) -> Div {
    div()
        .absolute()
        .top_0()
        .left_0()
        .right_0()
        .h(px(1.))
        .bg(top_highlight(surface))
}

/// A translucent backdrop covering the window behind a floating overlay.
/// Does not call `occlude()` so clicking outside still blurs and closes the
/// overlay naturally.
pub fn modal_scrim(background: Rgb) -> Div {
    div().absolute().inset_0().bg(scrim(background))
}

/// A single keyboard keycap badge (`⌘`, `K`, `Esc`, etc.).
pub fn kbd(
    label: impl Into<SharedString>,
    surface: Rgb,
    border: Rgb,
    text: Rgb,
    font_size: f32,
) -> Div {
    let key_bg = lifted(surface, text);
    div()
        .relative()
        .overflow_hidden()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .min_w(px(18.))
        .px(px(5.))
        .py(px(1.))
        .rounded(px(RADIUS_CONTROL))
        .bg(hsla(key_bg))
        .border_1()
        .border_b_2()
        .border_color(hsla(border))
        .text_size(px(small_text(font_size)))
        .text_color(hsla(text))
        .child(top_highlight_bar(key_bg))
        .child(label.into())
}

/// A group of keyboard keycaps built from a space-separated shortcut string
/// such as `"⌘ K"` or `"⌘⇧B"`. Returns `None` when `keys` is empty.
pub fn kbd_group(keys: &str, surface: Rgb, border: Rgb, text: Rgb, font_size: f32) -> Option<Div> {
    let parts: Vec<&str> = keys.split_whitespace().filter(|p| !p.is_empty()).collect();
    if parts.is_empty() {
        return None;
    }
    Some(
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(3.))
            .children(
                parts
                    .into_iter()
                    .map(|part| kbd(part.to_owned(), surface, border, text, font_size)),
            ),
    )
}

/// A rounded status pill with a subtle translucent tint background and border.
pub fn pill(label: impl Into<SharedString>, color: Rgb, font_size: f32, emphasized: bool) -> Div {
    let base = div()
        .flex_none()
        .px(px(6.))
        .py(px(1.))
        .rounded_full()
        .bg(with_alpha(color, 0.13))
        .border_1()
        .border_color(with_alpha(color, 0.38))
        .text_size(px(small_text(font_size)))
        .text_color(hsla(color))
        .child(label.into());
    if emphasized {
        base.font_weight(FontWeight::SEMIBOLD)
    } else {
        base
    }
}

/// A calm empty-state block with a primary message and optional keyboard-hint
/// actions so users immediately see what to press next.
#[allow(clippy::too_many_arguments)]
pub fn empty_state(
    message: impl Into<SharedString>,
    hints: &[(&str, &str)],
    surface: Rgb,
    border: Rgb,
    text: Rgb,
    muted: Rgb,
    font_size: f32,
) -> Div {
    let hint_row = (!hints.is_empty()).then(|| {
        div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(12.))
            .mt(px(8.))
            .text_size(px(small_text(font_size)))
            .text_color(hsla(muted))
            .children(hints.iter().map(|(keys, label)| {
                div()
                    .flex()
                    .items_center()
                    .gap(px(5.))
                    .children(kbd_group(keys, surface, border, muted, font_size))
                    .child((*label).to_owned())
            }))
    });
    div()
        .relative()
        .overflow_hidden()
        .flex()
        .flex_col()
        .gap(px(2.))
        .p(px(14.))
        .rounded(px(crate::theme::RADIUS_CARD))
        .bg(with_alpha(surface, 0.55))
        .border_1()
        .border_color(subtle_border(border, surface))
        .child(top_highlight_bar(surface))
        .child(div().text_color(hsla(text)).child(message.into()))
        .children(hint_row)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_light_and_dark_surfaces() {
        assert!(!is_light(Rgb(0x1e, 0x1f, 0x22)));
        assert!(!is_light(Rgb(0x00, 0x2b, 0x36)));
        assert!(is_light(Rgb(0xf5, 0xf5, 0xf4)));
        assert!(is_light(Rgb(0xff, 0xff, 0xff)));
    }

    #[test]
    fn kbd_group_ignores_blank_strings() {
        let surface = Rgb(0x25, 0x26, 0x2a);
        let border = Rgb(0x39, 0x3b, 0x41);
        let text = Rgb(0xd4, 0xd6, 0xdb);
        assert!(kbd_group("", surface, border, text, 12.).is_none());
        assert!(kbd_group("   ", surface, border, text, 12.).is_none());
        assert!(kbd_group("⌘ K", surface, border, text, 12.).is_some());
    }

    #[test]
    fn alpha_clamps_to_unit_interval() {
        let low = with_alpha(Rgb(100, 120, 140), -0.5);
        let high = with_alpha(Rgb(100, 120, 140), 2.0);
        assert_eq!(low.a, 0.0);
        assert_eq!(high.a, 1.0);
    }

    #[test]
    fn scrim_is_lighter_on_light_surfaces() {
        assert!(scrim(Rgb(0xf5, 0xf5, 0xf4)).a < scrim(Rgb(0x1e, 0x1f, 0x22)).a);
    }
}

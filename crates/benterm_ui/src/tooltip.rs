//! A small floating label shown when the mouse rests on an icon or button.

use gpui::prelude::FluentBuilder;
use gpui::{
    AnyView, App, AppContext, Context, InteractiveElement, IntoElement, ParentElement, Render,
    SharedString, Styled, Window, div, px,
};

use crate::terminal_view::hsla;
use crate::theme::{MenuTheme, RADIUS_CONTROL, small_text};

/// A tooltip view built on demand when the mouse rests over a target.
pub struct Tooltip {
    text: SharedString,
    shortcut: Option<SharedString>,
    theme: MenuTheme,
}

impl Tooltip {
    /// A tooltip builder showing `text`.
    pub fn text(
        text: impl Into<SharedString>,
        theme: MenuTheme,
    ) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
        let text = text.into();
        move |_, cx| {
            let text = text.clone();
            let theme = theme.clone();
            cx.new(move |_| Self {
                text,
                shortcut: None,
                theme,
            })
            .into()
        }
    }

    /// A tooltip builder showing `text` with a key `shortcut` beside it.
    pub fn with_shortcut(
        text: impl Into<SharedString>,
        shortcut: impl Into<SharedString>,
        theme: MenuTheme,
    ) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
        let text = text.into();
        let shortcut = shortcut.into();
        move |_, cx| {
            let text = text.clone();
            let shortcut = shortcut.clone();
            let theme = theme.clone();
            cx.new(move |_| Self {
                text,
                shortcut: Some(shortcut),
                theme,
            })
            .into()
        }
    }
}

impl Render for Tooltip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let theme = &self.theme;
        let text = self.text.clone();
        div()
            .debug_selector(|| "tooltip".into())
            .relative()
            .overflow_hidden()
            .flex()
            .items_center()
            .gap(px(6.))
            .px(px(8.))
            .py(px(4.))
            .rounded(px(RADIUS_CONTROL))
            .bg(hsla(theme.background))
            .border_1()
            .border_color(hsla(theme.border))
            .shadow_md()
            .font_family(theme.font_family.clone())
            .text_size(px(small_text(theme.font_size)))
            .text_color(hsla(theme.text))
            .child(crate::design::top_highlight_bar(theme.background))
            .child(
                div()
                    .debug_selector({
                        let text = text.clone();
                        move || format!("tooltip:{text}")
                    })
                    .child(text),
            )
            .when_some(self.shortcut.clone(), |tooltip, shortcut| {
                tooltip.children(crate::design::kbd_group(
                    shortcut.as_ref(),
                    theme.background,
                    theme.border,
                    theme.key_text,
                    theme.font_size,
                ))
            })
    }
}

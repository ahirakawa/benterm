//! The command palette: Cmd-K lists the commands the focused pane can run,
//! with their keys, and runs the one chosen by name.
//!
//! The commands are the GPUI actions that the views around the focused
//! pane handle, and their keys are the ones bound to those actions now, so
//! nothing here lists them a second time.

use gpui::prelude::FluentBuilder;
use gpui::{
    Action, App, AppContext, Context, Entity, EventEmitter, FontWeight, InteractiveElement,
    IntoElement, KeyBinding, MouseButton, ParentElement, Render, ScrollHandle,
    StatefulInteractiveElement, Styled, Subscription, Window, actions, div, px,
};

use crate::BentermWindow;
use crate::command_match::{command_name, key_label, match_ranges, matches, split_category};
use crate::name_editor::{NameEditor, NameEditorEvent};
use crate::roles::{LaunchRole, role_label};
use crate::sidebar;
use crate::terminal_view::hsla;
use crate::theme::CommandPaletteTheme;

actions!(
    command_palette,
    [
        ToggleCommandPalette,
        SelectNextCommand,
        SelectPreviousCommand,
    ]
);

/// The key context of the palette, which its bindings are limited to.
const KEY_CONTEXT: &str = "CommandPalette";

const WIDTH: f32 = 520.;
/// How far below the top of the window the palette floats.
const TOP: f32 = 48.;
/// The most rows shown before the list scrolls.
const MAX_LIST_HEIGHT: f32 = 320.;

/// Binds Cmd-K, which works anywhere in the window, and the arrows that
/// move the selection while the palette has the keyboard.
pub fn bind_keys(cx: &mut App) {
    let context = Some(KEY_CONTEXT);
    cx.bind_keys([
        KeyBinding::new("cmd-k", ToggleCommandPalette, Some(sidebar::KEY_CONTEXT)),
        KeyBinding::new("down", SelectNextCommand, context),
        KeyBinding::new("up", SelectPreviousCommand, context),
    ]);
}

/// A command as the palette shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShownCommand {
    pub name: String,
    /// The keys that run it, or nothing if none are bound.
    pub keys: String,
}

/// A command the palette can run.
struct Command {
    shown: ShownCommand,
    action: Box<dyn Action>,
}

/// The commands that the focused element can run, in order of name. The
/// palette's own command is left out.
fn available_commands(window: &Window, cx: &App) -> Vec<Command> {
    let mut commands: Vec<Command> = window
        .available_actions(cx)
        .into_iter()
        // A role is listed by its name, from the roles the window has.
        .filter(|action| {
            !action.as_any().is::<ToggleCommandPalette>() && !action.as_any().is::<LaunchRole>()
        })
        .map(|action| {
            let keys = window
                .highest_precedence_binding_for_action(action.as_ref())
                .map(|binding| {
                    binding
                        .keystrokes()
                        .iter()
                        .map(|keystroke| key_label(keystroke.modifiers(), keystroke.key()))
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .unwrap_or_default();
            Command {
                shown: ShownCommand {
                    name: command_name(action.name()),
                    keys,
                },
                action,
            }
        })
        .collect();
    commands.sort_by(|a, b| a.shown.name.cmp(&b.shown.name));
    commands
}

/// What the palette tells the window.
pub enum CommandPaletteEvent {
    /// A command was chosen.
    Run(Box<dyn Action>),
    /// The palette was closed without choosing.
    Dismissed,
}

/// The palette as a view.
pub struct CommandPalette {
    commands: Vec<Command>,
    editor: Entity<NameEditor>,
    /// Counted in the shown commands. Typing goes back to the best match.
    selected: usize,
    /// What was typed when the selection was last reset.
    query: String,
    theme: CommandPaletteTheme,
    scroll: ScrollHandle,
    _subscriptions: [Subscription; 2],
}

impl CommandPalette {
    fn new(commands: Vec<Command>, theme: CommandPaletteTheme, cx: &mut Context<Self>) -> Self {
        let editor = cx.new(|cx| NameEditor::new("", theme.clone(), cx));
        let typed = cx.observe(&editor, |this, editor, cx| {
            let query = editor.read(cx).text();
            if query != this.query {
                this.query = query.to_owned();
                this.selected = 0;
                this.scroll.scroll_to_item(0);
            }
            cx.notify();
        });
        let events = cx.subscribe(&editor, |this, _, event, cx| match event {
            NameEditorEvent::Confirmed(_) => {
                if let Some(index) = this.selected(cx) {
                    this.run(index, cx);
                }
            }
            NameEditorEvent::Cancelled => cx.emit(CommandPaletteEvent::Dismissed),
        });
        Self {
            commands,
            editor,
            selected: 0,
            query: String::new(),
            theme,
            scroll: ScrollHandle::new(),
            _subscriptions: [typed, events],
        }
    }

    /// The indexes of the commands that match what is typed, best first.
    fn shown_indexes(&self, cx: &App) -> Vec<usize> {
        let names: Vec<&str> = self
            .commands
            .iter()
            .map(|command| command.shown.name.as_str())
            .collect();
        matches(self.editor.read(cx).text(), &names)
    }

    /// The commands that match what is typed, best first.
    pub fn shown(&self, cx: &App) -> Vec<ShownCommand> {
        self.shown_indexes(cx)
            .into_iter()
            .map(|index| self.commands[index].shown.clone())
            .collect()
    }

    /// Which of the shown commands Enter runs, if any is shown.
    pub fn selected(&self, cx: &App) -> Option<usize> {
        let count = self.shown_indexes(cx).len();
        (count > 0).then(|| self.selected.min(count - 1))
    }

    /// The field for what is typed.
    pub fn editor(&self) -> &Entity<NameEditor> {
        &self.editor
    }

    /// Runs the shown command at `index`.
    fn run(&mut self, index: usize, cx: &mut Context<Self>) {
        if let Some(&command) = self.shown_indexes(cx).get(index) {
            let action = self.commands[command].action.boxed_clone();
            cx.emit(CommandPaletteEvent::Run(action));
        }
    }

    fn select_next(&mut self, _: &SelectNextCommand, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(selected) = self.selected(cx) {
            let last = self.shown_indexes(cx).len() - 1;
            self.select((selected + 1).min(last), cx);
        }
    }

    fn select_previous(
        &mut self,
        _: &SelectPreviousCommand,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(selected) = self.selected(cx) {
            self.select(selected.saturating_sub(1), cx);
        }
    }

    fn select(&mut self, index: usize, cx: &mut Context<Self>) {
        self.selected = index;
        self.scroll.scroll_to_item(index);
        cx.notify();
    }

    fn highlighted_name(&self, text: &str, query: &str) -> gpui::Div {
        let theme = &self.theme;
        let ranges = match_ranges(query, text);
        if ranges.is_empty() {
            return div().truncate().child(text.to_owned());
        }
        let mut container = div().flex().items_center().truncate();
        let mut cursor = 0;
        for range in ranges {
            if range.start > cursor {
                container = container.child(text[cursor..range.start].to_owned());
            }
            container = container.child(
                div()
                    .text_color(hsla(theme.accent))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(text[range.start..range.end].to_owned()),
            );
            cursor = range.end;
        }
        if cursor < text.len() {
            container = container.child(text[cursor..].to_owned());
        }
        container
    }

    fn row(
        &self,
        index: usize,
        command: &ShownCommand,
        selected: bool,
        show_group_prefix: bool,
        query: &str,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = &self.theme;
        let label = if show_group_prefix {
            command.name.as_str()
        } else {
            split_category(&command.name).1
        };
        let keys = crate::design::kbd_group(
            &command.keys,
            theme.background,
            theme.border,
            theme.secondary_text,
            theme.font_size,
        );
        div()
            .id(("command", index))
            .debug_selector(|| format!("command-{index}"))
            .flex()
            .items_center()
            .justify_between()
            .gap_4()
            .mx_1()
            .px(px(10.))
            .py(px(5.))
            .rounded(px(crate::theme::RADIUS_CONTROL))
            .cursor_pointer()
            .when(selected, |row| row.bg(hsla(theme.selected_background)))
            .when(!selected, |row| {
                row.hover(|row| row.bg(hsla(theme.selected_background)))
            })
            .child(self.highlighted_name(label, query))
            .children(keys)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, _, cx| this.run(index, cx)),
            )
    }

    fn footer(&self) -> impl IntoElement {
        let theme = &self.theme;
        let hint = |key: &str, label: &'static str| {
            div()
                .flex()
                .items_center()
                .gap(px(5.))
                .children(crate::design::kbd_group(
                    key,
                    theme.input_background,
                    theme.border,
                    theme.secondary_text,
                    theme.font_size,
                ))
                .child(label)
        };
        div()
            .flex()
            .items_center()
            .gap(px(14.))
            .px_3()
            .py(px(6.))
            .bg(hsla(theme.input_background))
            .border_t_1()
            .border_color(crate::design::subtle_border(
                theme.border,
                theme.input_background,
            ))
            .text_size(px(crate::theme::small_text(theme.font_size)))
            .text_color(hsla(theme.secondary_text))
            .child(hint("↑ ↓", "Select"))
            .child(hint("↩", "Run"))
            .child(hint("Esc", "Close"))
    }
}

impl EventEmitter<CommandPaletteEvent> for CommandPalette {}

impl Render for CommandPalette {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let shown = self.shown(cx);
        let selected = self.selected(cx);
        let query = self.editor.read(cx).text().to_owned();
        let grouped = query.trim().is_empty();
        let theme = &self.theme;
        let mut rows = Vec::with_capacity(shown.len());
        let mut last_category: Option<String> = None;
        for (index, command) in shown.iter().enumerate() {
            if grouped {
                let category = split_category(&command.name)
                    .0
                    .unwrap_or("General")
                    .to_owned();
                if last_category.as_deref() != Some(category.as_str()) {
                    rows.push(
                        div()
                            .px_3()
                            .pt(px(if index == 0 { 4. } else { 8. }))
                            .pb(px(2.))
                            .text_size(px(crate::theme::small_text(theme.font_size)))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(hsla(theme.secondary_text))
                            .child(category.to_uppercase())
                            .into_any_element(),
                    );
                    last_category = Some(category);
                }
            }
            rows.push(
                self.row(
                    index,
                    command,
                    selected == Some(index),
                    !grouped,
                    &query,
                    cx,
                )
                .into_any_element(),
            );
        }
        let empty = shown.is_empty().then(|| {
            div()
                .px_3()
                .py(px(10.))
                .text_color(hsla(theme.secondary_text))
                .child("No matching commands")
        });
        div()
            .id("command-palette")
            .debug_selector(|| "command-palette".into())
            .key_context(KEY_CONTEXT)
            .on_action(cx.listener(Self::select_next))
            .on_action(cx.listener(Self::select_previous))
            .occlude()
            .relative()
            .w(px(WIDTH))
            .flex()
            .flex_col()
            .overflow_hidden()
            .bg(hsla(theme.background))
            .border_1()
            .border_color(hsla(theme.border))
            .rounded(px(crate::theme::RADIUS_CARD))
            .shadow_lg()
            .font_family(theme.font_family.clone())
            .text_size(px(theme.font_size))
            .text_color(hsla(theme.text))
            .child(crate::design::top_highlight_bar(theme.background))
            .child(
                div()
                    .flex()
                    .px_3()
                    .py_2()
                    .bg(hsla(theme.input_background))
                    .border_b_1()
                    .border_color(hsla(theme.input_border))
                    .child(self.editor.clone()),
            )
            .child(
                div()
                    .id("commands")
                    .flex()
                    .flex_col()
                    .py_1()
                    .max_h(px(MAX_LIST_HEIGHT))
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .children(rows)
                    .children(empty),
            )
            .child(self.footer())
    }
}

/// The palette while it is open in a window.
pub(crate) struct OpenPalette {
    view: Entity<CommandPalette>,
    _subscriptions: [Subscription; 2],
}

impl BentermWindow {
    /// The command palette, while it is open.
    pub fn command_palette(&self) -> Option<Entity<CommandPalette>> {
        self.palette.as_ref().map(|palette| palette.view.clone())
    }

    /// Opens the palette with the commands of the focused element and
    /// gives it the keyboard, or closes it if it is open.
    pub fn toggle_command_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.palette.is_some() {
            self.close_command_palette(None, window, cx);
            return;
        }
        let mut commands = available_commands(window, cx);
        commands.extend(self.settings.roles().iter().map(|role| Command {
            shown: ShownCommand {
                name: format!("Role: {}", role_label(role)),
                keys: String::new(),
            },
            action: Box::new(LaunchRole(role.name.clone())),
        }));
        commands.sort_by(|a, b| a.shown.name.cmp(&b.shown.name));
        let theme = self.settings.theme().command_palette.clone();
        let view = cx.new(|cx| CommandPalette::new(commands, theme, cx));
        let events = cx.subscribe_in(&view, window, |this, _, event, window, cx| {
            let action = match event {
                CommandPaletteEvent::Run(action) => Some(action.boxed_clone()),
                CommandPaletteEvent::Dismissed => None,
            };
            this.close_command_palette(action, window, cx);
        });
        let focus = view.read(cx).editor.read(cx).focus_handle().clone();
        let palette = view.entity_id();
        // The keyboard went somewhere else, such as a pane that was
        // clicked; the palette goes away. A palette closed already is
        // not this one.
        let blur = cx.on_blur(&focus, window, move |this, _, cx| {
            if this
                .palette
                .as_ref()
                .is_some_and(|open| open.view.entity_id() == palette)
            {
                this.palette = None;
                cx.notify();
            }
        });
        self.palette = Some(OpenPalette {
            view,
            _subscriptions: [events, blur],
        });
        window.focus(&focus, cx);
        cx.notify();
    }

    /// Closes the palette, gives the keyboard back to the focused pane of
    /// the selected tab, or to the peek the palette was opened over, and
    /// runs `action` there if there is one.
    fn close_command_palette(
        &mut self,
        action: Option<Box<dyn Action>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.palette.take().is_none() {
            return;
        }
        // Back to the peek, if the palette was opened over it.
        if !self.focus_peek(window, cx) {
            self.focus_selected(window, cx);
        }
        if let Some(action) = action {
            window.dispatch_action(action, cx);
        }
    }

    /// The palette floating over the top of the window, while it is open.
    pub(crate) fn command_palette_overlay(&self) -> Option<impl IntoElement> {
        let terminal_bg = self.settings.theme().terminal.background;
        self.palette.as_ref().map(|palette| {
            div()
                .absolute()
                .inset_0()
                .child(crate::design::modal_scrim(terminal_bg))
                .child(
                    div()
                        .absolute()
                        .top(px(TOP))
                        .left_0()
                        .w_full()
                        .flex()
                        .justify_center()
                        .child(crate::browser::covers())
                        .child(palette.view.clone()),
                )
        })
    }
}

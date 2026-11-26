//! Every color, font and spacing the UI uses, and what keys send.
//!
//! Views read these values from a [`Theme`] and never hard-code them. A
//! theme is built from the user's config by [`Theme::from_config`].

use benterm_config::{Config, Rgba};
use benterm_core::{Color, ReportedColors, Rgb};
use benterm_layout::WorkspaceColor;

use crate::input::KeyOptions;

/// The monospace font every macOS has, used when none of the configured
/// families is installed.
pub const SYSTEM_MONOSPACE_FONT: &str = "Menlo";

/// The size of the small text of a view whose text is `font_size`:
/// headings of sections, what is said under a name, badges. No text of
/// the UI is smaller.
pub fn small_text(font_size: f32) -> f32 {
    font_size - 1.
}

/// The corners of what is pressed or typed into: buttons, fields, badges.
pub const RADIUS_CONTROL: f32 = 4.;
/// The corners of what floats over the window: forms, menus, toasts.
pub const RADIUS_CARD: f32 = 6.;
/// The corners of the wide panels of the progress board.
pub const RADIUS_PANEL: f32 = 8.;

/// `color` moved a little toward `toward`, for the same thing under the
/// mouse: it stays the color it was, and is seen to answer.
pub fn lifted(color: Rgb, toward: Rgb) -> Rgb {
    let mix = |from: u8, to: u8| {
        let (from, to) = (f32::from(from), f32::from(to));
        (from + (to - from) * 0.1).round() as u8
    };
    Rgb(
        mix(color.0, toward.0),
        mix(color.1, toward.1),
        mix(color.2, toward.2),
    )
}

thread_local! {
    /// How often this thread asked the system for its fonts.
    static FONT_LOOKUPS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// How often views on this thread have asked the system for the list of
/// its fonts. Making the list takes the system long enough to be felt, so
/// the tests check that drawing does not ask.
pub fn font_lookups() -> usize {
    FONT_LOOKUPS.get()
}

/// The first of the theme's font families that is installed, else the
/// system monospace font. This asks the system for the list of its
/// fonts, which takes it some 200 milliseconds: a view asks when its
/// theme changes, never when it draws.
pub fn installed_mono_font(theme: &TerminalTheme, cx: &gpui::App) -> gpui::SharedString {
    FONT_LOOKUPS.set(FONT_LOOKUPS.get() + 1);
    let installed = cx.text_system().all_font_names();
    theme.installed_font_family(&installed).to_owned().into()
}

/// The theme of the whole application.
#[derive(Debug, Clone, PartialEq)]
pub struct Theme {
    pub terminal: TerminalTheme,
    pub notice: NoticeTheme,
    pub sidebar: SidebarTheme,
    pub tabs: TabTheme,
    pub panes: PaneTheme,
    pub command_palette: CommandPaletteTheme,
    pub menu: MenuTheme,
    pub toast: ToastTheme,
    pub board: BoardTheme,
    pub diff: DiffTheme,
    pub search: SearchTheme,
    /// Not a look, but read by the terminals from the same config and
    /// applied in the same way when the file is saved.
    pub keys: KeyOptions,
}

impl Default for Theme {
    fn default() -> Self {
        Self::from_config(&Config::default())
    }
}

impl Theme {
    pub fn from_config(config: &Config) -> Self {
        let c = &config.colors.terminal;
        let settings = &config.terminal;
        let palette = &config.palette;
        Self {
            terminal: TerminalTheme {
                background: rgb(c.background),
                foreground: rgb(c.foreground),
                cursor: rgb(c.cursor),
                cursor_text: rgb(c.cursor_text),
                ansi: [
                    c.black,
                    c.red,
                    c.green,
                    c.yellow,
                    c.blue,
                    c.magenta,
                    c.cyan,
                    c.white,
                    c.bright_black,
                    c.bright_red,
                    c.bright_green,
                    c.bright_yellow,
                    c.bright_blue,
                    c.bright_magenta,
                    c.bright_cyan,
                    c.bright_white,
                ]
                .map(rgb),
                font_families: settings.font.family.clone(),
                font_size: settings.font.size,
                line_height: settings.line_height,
                padding: settings.padding,
            },
            sidebar: SidebarTheme::from_config(config),
            notice: NoticeTheme {
                background: rgb(palette.elevated),
                text: rgb(palette.text),
                border: rgb(palette.border),
                accent: rgb(palette.warning),
                // The config never gives an empty list.
                font_family: config.ui.font.family[0].clone(),
                font_size: config.ui.font.size,
            },
            tabs: TabTheme::from_config(config),
            panes: PaneTheme::from_config(config),
            command_palette: CommandPaletteTheme::from_config(config),
            menu: MenuTheme::from_config(config),
            toast: ToastTheme::from_config(config),
            board: BoardTheme::from_config(config),
            diff: DiffTheme::from_config(config),
            search: SearchTheme::from_config(config),
            keys: KeyOptions {
                option_as_meta: settings.option_as_meta,
            },
        }
    }
}

/// The terminal grid has no transparency, so alpha is dropped.
fn rgb(color: Rgba) -> Rgb {
    Rgb(color.r, color.g, color.b)
}

/// How a terminal pane looks.
#[derive(Debug, Clone, PartialEq)]
pub struct TerminalTheme {
    pub background: Rgb,
    pub foreground: Rgb,
    pub cursor: Rgb,
    /// The character under a filled block cursor.
    pub cursor_text: Rgb,
    /// The ANSI colors: 0-7 normal, 8-15 bright.
    pub ansi: [Rgb; 16],
    /// In order of preference.
    pub font_families: Vec<String>,
    /// In pixels.
    pub font_size: f32,
    /// Row height as a multiple of the font size.
    pub line_height: f32,
    /// Space between the pane's edge and the grid, in pixels.
    pub padding: f32,
}

/// The temporary bar that shows problems with the config file.
#[derive(Debug, Clone, PartialEq)]
pub struct NoticeTheme {
    pub background: Rgb,
    pub text: Rgb,
    pub border: Rgb,
    /// The mark in front of each message.
    pub accent: Rgb,
    pub font_family: String,
    /// In pixels.
    pub font_size: f32,
}

/// The sidebar on the left, which lists the workspaces.
#[derive(Debug, Clone, PartialEq)]
pub struct SidebarTheme {
    pub background: Rgb,
    /// The line between the sidebar and the tabs.
    pub border: Rgb,
    pub section_header_text: Rgb,
    pub item_text: Rgb,
    /// The small line under an item's name.
    pub item_secondary_text: Rgb,
    pub item_hover_background: Rgb,
    pub item_active_background: Rgb,
    /// The line left of the selected item.
    pub item_active_indicator: Rgb,
    /// The mark of a workspace with a pane that needs the user.
    pub attention: Rgb,
    /// The colors of the workspace icons, by [`WorkspaceColor::index`].
    pub workspace_colors: [Rgb; WorkspaceColor::COUNT],
    /// The letter on a workspace icon.
    pub workspace_icon_text: Rgb,
    /// The dots of the session list, by what the agent is doing.
    pub session_working: Rgb,
    pub session_waiting: Rgb,
    pub session_done: Rgb,
    /// An agent at its prompt with nothing new, or one still starting.
    pub session_idle: Rgb,
    pub session_error: Rgb,
    /// The field for a reply in a row of the session list.
    pub field_background: Rgb,
    pub font_family: String,
    /// In pixels.
    pub font_size: f32,
}

impl SidebarTheme {
    fn from_config(config: &Config) -> Self {
        let c = &config.colors.sidebar;
        let w = &config.colors.workspace;
        let status = &config.colors.status;
        Self {
            background: rgb(c.background),
            border: rgb(c.border),
            section_header_text: rgb(c.section_header_text),
            item_text: rgb(c.item_text),
            item_secondary_text: rgb(c.item_secondary_text),
            item_hover_background: rgb(c.item_hover_background),
            item_active_background: rgb(c.item_active_background),
            item_active_indicator: rgb(c.item_active_indicator),
            attention: rgb(c.attention),
            workspace_colors: [w.icon_1, w.icon_2, w.icon_3, w.icon_4, w.icon_5, w.icon_6].map(rgb),
            workspace_icon_text: rgb(w.icon_text),
            session_working: rgb(status.running),
            session_waiting: rgb(status.waiting),
            session_done: rgb(status.done),
            session_idle: rgb(status.idle),
            session_error: rgb(status.error),
            field_background: rgb(config.palette.background),
            // The config never gives an empty list.
            font_family: config.ui.font.family[0].clone(),
            font_size: config.ui.font.size,
        }
    }

    pub fn workspace_color(&self, color: WorkspaceColor) -> Rgb {
        self.workspace_colors[color.index()]
    }
}

/// The bar of tabs above the panes.
#[derive(Debug, Clone, PartialEq)]
pub struct TabTheme {
    pub bar_background: Rgb,
    pub text: Rgb,
    /// The small text after a tab's title, such as its number of panes.
    pub secondary_text: Rgb,
    pub active_background: Rgb,
    pub active_text: Rgb,
    /// The line under the selected tab.
    pub active_indicator: Rgb,
    /// The lines between tabs and under the bar.
    pub separator: Rgb,
    /// The mark of a tab with a pane that needs the user.
    pub attention: Rgb,
    /// The background of the + button for a new tab under the mouse.
    pub button_hover_background: Rgb,
    pub font_family: String,
    /// In pixels.
    pub font_size: f32,
}

impl TabTheme {
    fn from_config(config: &Config) -> Self {
        let c = &config.colors.tab;
        Self {
            bar_background: rgb(c.bar_background),
            text: rgb(c.text),
            secondary_text: rgb(c.secondary_text),
            active_background: rgb(c.active_background),
            active_text: rgb(c.active_text),
            active_indicator: rgb(c.active_indicator),
            separator: rgb(c.separator),
            attention: rgb(c.attention),
            // The config has no tab color for it, so it takes the palette
            // color the sidebar's buttons take by default.
            button_hover_background: rgb(config.palette.elevated),
            // The config never gives an empty list.
            font_family: config.ui.font.family[0].clone(),
            font_size: config.ui.font.size,
        }
    }
}

/// The lines the pane grid draws over its panes.
#[derive(Debug, Clone, PartialEq)]
pub struct PaneTheme {
    /// The line between panes.
    pub divider: Rgb,
    /// The outline of the focused pane when there is more than one.
    pub focus_outline: Rgb,
    /// The outline of a pane that needs the user.
    pub attention: Rgb,
}

impl PaneTheme {
    fn from_config(config: &Config) -> Self {
        let c = &config.colors.pane;
        Self {
            divider: rgb(c.divider),
            focus_outline: rgb(c.focus_outline),
            attention: rgb(c.attention),
        }
    }
}

/// The command palette, which floats over the window.
#[derive(Debug, Clone, PartialEq)]
pub struct CommandPaletteTheme {
    pub background: Rgb,
    pub border: Rgb,
    /// The names of the commands, and what is typed.
    pub text: Rgb,
    /// The keys of the commands.
    pub secondary_text: Rgb,
    /// The command that Enter runs.
    pub selected_background: Rgb,
    /// The line under the field, and the background of selected text.
    pub accent: Rgb,
    /// The field for what is typed.
    pub input_background: Rgb,
    pub input_border: Rgb,
    /// What went wrong, in a form that floats as the palette does.
    pub error: Rgb,
    pub font_family: String,
    /// In pixels.
    pub font_size: f32,
}

impl CommandPaletteTheme {
    fn from_config(config: &Config) -> Self {
        let palette = &config.palette;
        let input = &config.colors.input;
        Self {
            background: rgb(palette.elevated),
            border: rgb(palette.border),
            text: rgb(palette.text),
            secondary_text: rgb(palette.text_muted),
            selected_background: rgb(palette.active),
            accent: rgb(palette.accent),
            input_background: rgb(input.background),
            input_border: rgb(input.border),
            error: rgb(palette.error),
            // The config never gives an empty list.
            font_family: config.ui.font.family[0].clone(),
            font_size: config.ui.font.size,
        }
    }
}

/// The menu a right click opens.
#[derive(Debug, Clone, PartialEq)]
pub struct MenuTheme {
    pub background: Rgb,
    pub border: Rgb,
    /// The names of the items.
    pub text: Rgb,
    /// The keys of the items.
    pub key_text: Rgb,
    /// An item that cannot be chosen now.
    pub disabled_text: Rgb,
    /// The item Enter runs.
    pub selected_background: Rgb,
    pub font_family: String,
    /// In pixels.
    pub font_size: f32,
}

impl MenuTheme {
    fn from_config(config: &Config) -> Self {
        let c = &config.colors.menu;
        Self {
            background: rgb(c.background),
            border: rgb(c.border),
            text: rgb(c.text),
            key_text: rgb(c.key_text),
            disabled_text: rgb(c.disabled_text),
            selected_background: rgb(c.selected_background),
            // The config never gives an empty list.
            font_family: config.ui.font.family[0].clone(),
            font_size: config.ui.font.size,
        }
    }
}

/// The notifications shown for a while in a corner of the window.
#[derive(Debug, Clone, PartialEq)]
pub struct ToastTheme {
    pub background: Rgb,
    pub border: Rgb,
    /// What happened.
    pub text: Rgb,
    /// Who it is from, and what a program said.
    pub secondary_text: Rgb,
    /// The background under the mouse, which a click takes to the pane.
    pub hover_background: Rgb,
    /// The dot of an agent that finished.
    pub done: Rgb,
    /// The dot of an agent that waits for the user.
    pub waiting: Rgb,
    /// The dot of a notification a program asked for.
    pub message: Rgb,
    /// The dot of a program that failed on startup.
    pub error: Rgb,
    pub font_family: String,
    /// In pixels.
    pub font_size: f32,
}

impl ToastTheme {
    fn from_config(config: &Config) -> Self {
        let c = &config.colors.toast;
        Self {
            background: rgb(c.background),
            border: rgb(c.border),
            text: rgb(c.text),
            secondary_text: rgb(c.secondary_text),
            hover_background: rgb(c.hover_background),
            done: rgb(c.done),
            waiting: rgb(c.waiting),
            message: rgb(c.message),
            error: rgb(config.colors.status.error),
            // The config never gives an empty list.
            font_family: config.ui.font.family[0].clone(),
            font_size: config.ui.font.size,
        }
    }
}

/// The progress board, opened with the tab above the panes.
#[derive(Debug, Clone, PartialEq)]
pub struct BoardTheme {
    pub background: Rgb,
    /// The panel the timeline is drawn in.
    pub panel: Rgb,
    pub border: Rgb,
    /// The heading of a panel.
    pub title_text: Rgb,
    /// The name of each agent's row, and the legend.
    pub label_text: Rgb,
    /// The times under the timeline.
    pub axis_text: Rgb,
    /// The lines at the times under the timeline.
    pub grid_line: Rgb,
    /// The dashed line at the present time.
    pub now_line: Rgb,
    /// The row under the mouse or the keyboard.
    pub row_hover_background: Rgb,
    /// The chosen width of the timeline.
    pub range_active_background: Rgb,
    /// The box of an agent in the graph.
    pub node_background: Rgb,
    /// The line from an agent to one it started.
    pub link_line: Rgb,
    /// The arrow of a message between two agents.
    pub message_line: Rgb,
    /// The edge of what is selected in the graph.
    pub selected_border: Rgb,
    /// A terminal of the graph whose agent is in none of the four
    /// states.
    pub idle: Rgb,
    pub working: Rgb,
    pub waiting: Rgb,
    pub done: Rgb,
    pub error: Rgb,
    /// The Epic node color in the issue graph.
    pub epic: Rgb,
    pub font_family: String,
    /// In pixels.
    pub font_size: f32,
}

/// The diff of a task, opened above the panes. Its text is drawn in the
/// font of the terminals.
#[derive(Debug, Clone, PartialEq)]
pub struct DiffTheme {
    pub background: Rgb,
    pub text: Rgb,
    /// The branches beside the file's name, and the notices.
    pub secondary_text: Rgb,
    pub border: Rgb,
    pub added_background: Rgb,
    pub removed_background: Rgb,
    /// The `@@` line in front of each run of changes.
    pub hunk_background: Rgb,
    pub hunk_text: Rgb,
    pub line_number_text: Rgb,
    pub file_list_background: Rgb,
    pub file_active_background: Rgb,
    /// The marks of added, modified and removed files in the list.
    pub added_marker: Rgb,
    pub modified_marker: Rgb,
    pub removed_marker: Rgb,
    /// The lines selected for a comment.
    pub selected_background: Rgb,
    /// Behind the comments on the lines, and what is asked before they
    /// are sent.
    pub comment_background: Rgb,
    /// The edge of a comment, its field, and the button that sends them.
    pub accent: Rgb,
    /// The font of the file list and the headings.
    pub font_family: String,
    /// In pixels.
    pub font_size: f32,
}

impl DiffTheme {
    fn from_config(config: &Config) -> Self {
        let c = &config.colors.diff;
        let palette = &config.palette;
        Self {
            background: rgb(palette.background),
            text: rgb(palette.text),
            secondary_text: rgb(palette.text_muted),
            border: rgb(palette.border),
            added_background: rgb(c.added_background),
            removed_background: rgb(c.removed_background),
            hunk_background: rgb(c.hunk_background),
            hunk_text: rgb(c.hunk_text),
            line_number_text: rgb(c.line_number_text),
            file_list_background: rgb(c.file_list_background),
            file_active_background: rgb(c.file_active_background),
            added_marker: rgb(c.added_marker),
            modified_marker: rgb(c.modified_marker),
            removed_marker: rgb(c.removed_marker),
            selected_background: rgb(palette.active),
            comment_background: rgb(palette.elevated),
            accent: rgb(palette.accent),
            // The config never gives an empty list.
            font_family: config.ui.font.family[0].clone(),
            font_size: config.ui.font.size,
        }
    }
}

impl BoardTheme {
    fn from_config(config: &Config) -> Self {
        let c = &config.colors.board;
        let status = &config.colors.status;
        Self {
            background: rgb(c.background),
            panel: rgb(c.panel),
            border: rgb(c.border),
            title_text: rgb(c.title_text),
            label_text: rgb(c.label_text),
            axis_text: rgb(c.axis_text),
            grid_line: rgb(c.grid_line),
            now_line: rgb(c.now_line),
            row_hover_background: rgb(c.row_hover_background),
            range_active_background: rgb(c.range_active_background),
            node_background: rgb(c.node_background),
            link_line: rgb(c.link_line),
            message_line: rgb(c.message_line),
            selected_border: rgb(c.selected_border),
            idle: rgb(status.idle),
            working: rgb(status.running),
            waiting: rgb(status.waiting),
            done: rgb(status.done),
            error: rgb(status.error),
            // Matches `--purple` (#b48ef0) in the mock.
            epic: Rgb(0xb4, 0x8e, 0xf0),
            // The config never gives an empty list.
            font_family: config.ui.font.family[0].clone(),
            font_size: config.ui.font.size,
        }
    }
}

/// The in-pane terminal search bar and its match highlights.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchTheme {
    pub background: Rgb,
    pub border: Rgb,
    pub text: Rgb,
    pub secondary_text: Rgb,
    pub input_background: Rgb,
    pub input_border: Rgb,
    pub accent: Rgb,
    /// Background tint for non-active search matches.
    pub match_highlight: Rgb,
    /// Background tint for the currently selected search match.
    pub current_match_highlight: Rgb,
    /// Text color for the match counter when nothing matches (`0/0`).
    pub no_match_text: Rgb,
    pub font_family: String,
    /// In pixels.
    pub font_size: f32,
}

impl SearchTheme {
    fn from_config(config: &Config) -> Self {
        let palette = &config.palette;
        let input = &config.colors.input;
        Self {
            background: rgb(palette.elevated),
            border: rgb(palette.border),
            text: rgb(palette.text),
            secondary_text: rgb(palette.text_muted),
            input_background: rgb(input.background),
            input_border: rgb(input.border),
            accent: rgb(palette.accent),
            match_highlight: rgb(palette.warning),
            current_match_highlight: rgb(palette.accent),
            no_match_text: rgb(palette.error),
            // The config never gives an empty list.
            font_family: config.ui.font.family[0].clone(),
            font_size: config.ui.font.size,
        }
    }
}

impl TerminalTheme {
    /// The color a cell asks for, or `None` if it asks for the default
    /// foreground or background.
    pub fn resolve(&self, color: Color) -> Option<Rgb> {
        match color {
            Color::Default => None,
            Color::Rgb(r, g, b) => Some(Rgb(r, g, b)),
            Color::Indexed(index) => Some(self.indexed(index)),
        }
    }

    /// An entry of the 256-color palette: the ANSI colors, the 6x6x6 color
    /// cube, then 24 grays.
    fn indexed(&self, index: u8) -> Rgb {
        const CUBE: [u8; 6] = [0, 95, 135, 175, 215, 255];
        match index {
            0..=15 => self.ansi[usize::from(index)],
            16..=231 => {
                let index = usize::from(index - 16);
                Rgb(CUBE[index / 36], CUBE[index / 6 % 6], CUBE[index % 6])
            }
            232..=255 => {
                let gray = 8 + 10 * (index - 232);
                Rgb(gray, gray, gray)
            }
        }
    }

    /// The colors to answer color queries with.
    pub fn reported_colors(&self) -> ReportedColors {
        let mut palette = [Rgb::default(); 256];
        for index in 0..=255u8 {
            palette[usize::from(index)] = self.indexed(index);
        }
        ReportedColors {
            foreground: self.foreground,
            background: self.background,
            cursor: self.cursor,
            palette,
        }
    }

    /// The first of `font_families` among the `installed` font names, else
    /// the system monospace font.
    pub fn installed_font_family(&self, installed: &[String]) -> &str {
        self.font_families
            .iter()
            .find(|family| installed.contains(family))
            .map_or(SYSTEM_MONOSPACE_FONT, String::as_str)
    }

    /// The height of a row in pixels. Whole pixels, so that the backgrounds
    /// of adjacent rows meet without a seam.
    pub fn row_height(&self) -> f32 {
        (self.font_size * self.line_height).ceil().max(1.)
    }
}

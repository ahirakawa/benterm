//! The settings and their defaults.
//!
//! The defaults follow the dark theme of the mock. The palette holds the
//! mock's base colors, and every component color defaults to one of them, so
//! changing a palette color also changes the components that use it unless
//! the file sets those components too.

use crate::color::Rgba;
use crate::ipc::IpcSettings;
use crate::pty_log::PtyLogSettings;
use crate::read::Reader;
use crate::report::ReportSettings;
use crate::role::Agent;
use crate::session::SessionSettings;
use crate::stall::StallSettings;
use crate::worktrees::WorktreeSettings;
use toml::Table;

const fn hex(v: u32) -> Rgba {
    Rgba::rgb((v >> 16) as u8, (v >> 8) as u8, v as u8)
}

macro_rules! color_group {
    (
        $(#[$meta:meta])*
        $name:ident from $ctx:ty {
            $( $field:ident = $default:expr, )*
        }
    ) => {
        $(#[$meta])*
        #[derive(Clone, Debug, PartialEq)]
        pub struct $name {
            $( pub $field: Rgba, )*
        }

        impl $name {
            pub(crate) const KEYS: &'static [&'static str] = &[$(stringify!($field)),*];

            fn derive(ctx: &$ctx) -> Self {
                Self {
                    $( $field: {
                        let f: fn(&$ctx) -> Rgba = $default;
                        f(ctx)
                    }, )*
                }
            }

            fn read(r: &mut Reader, table: Option<&Table>, path: &str, ctx: &$ctx) -> Self {
                let d = Self::derive(ctx);
                r.unknown_keys(table, path, Self::KEYS);
                Self {
                    $( $field: r.color(table, path, stringify!($field), d.$field), )*
                }
            }
        }
    };
}

color_group! {
    /// The base colors of the mock (`--bg`, `--fg`, `--accent` and so on).
    Palette from () {
        background = |_| hex(0x1e1f22),
        panel = |_| hex(0x25262a),
        elevated = |_| hex(0x2d2f34),
        active = |_| hex(0x34363c),
        border = |_| hex(0x393b41),
        text = |_| hex(0xd4d6db),
        text_muted = |_| hex(0x8b8f98),
        text_dim = |_| hex(0x5f636b),
        accent = |_| hex(0x6aa1ff),
        success = |_| hex(0x7fc97f),
        warning = |_| hex(0xf0b35a),
        error = |_| hex(0xef6f6f),
        diff_added = |_| hex(0x1f3326),
        diff_removed = |_| hex(0x3a2224),
        terminal_background = |_| hex(0x1a1b1e),
    }
}

color_group! {
    /// The terminal grid. The mock has no ANSI colors, so the 16 colors are
    /// plain ones chosen to sit next to the palette.
    TerminalColors from Palette {
        foreground = |p| p.text,
        background = |p| p.terminal_background,
        cursor = |p| p.text,
        cursor_text = |p| p.terminal_background,
        selection_background = |p| Rgba { a: 0x4d, ..p.accent },
        black = |_| hex(0x393b41),
        red = |_| hex(0xef6f6f),
        green = |_| hex(0x7fc97f),
        yellow = |_| hex(0xf0b35a),
        blue = |_| hex(0x6aa1ff),
        magenta = |_| hex(0xc58af9),
        cyan = |_| hex(0x5fc4d0),
        white = |_| hex(0xd4d6db),
        bright_black = |_| hex(0x5f636b),
        bright_red = |_| hex(0xff8b8b),
        bright_green = |_| hex(0x9be09b),
        bright_yellow = |_| hex(0xffc978),
        bright_blue = |_| hex(0x8cb8ff),
        bright_magenta = |_| hex(0xd6a6ff),
        bright_cyan = |_| hex(0x7fdbe6),
        bright_white = |_| hex(0xffffff),
    }
}

color_group! {
    TitlebarColors from Palette {
        background = |p| p.panel,
        text = |p| p.text_muted,
        border = |p| p.border,
    }
}

color_group! {
    SidebarColors from Palette {
        background = |p| p.panel,
        border = |p| p.border,
        section_header_text = |p| p.text_muted,
        item_text = |p| p.text,
        item_secondary_text = |p| p.text_muted,
        item_hover_background = |p| p.elevated,
        item_active_background = |p| p.active,
        item_active_indicator = |p| p.accent,
        attention = |p| p.warning,
    }
}

color_group! {
    /// The icons of the workspaces in the sidebar. A workspace is given
    /// one of the six when it is made and keeps it until it closes.
    WorkspaceColors from Palette {
        icon_1 = |p| p.accent,
        icon_2 = |p| p.warning,
        icon_3 = |p| p.success,
        icon_4 = |_| hex(0xc58af9),
        icon_5 = |_| hex(0x5fc4d0),
        icon_6 = |_| hex(0xe07fa8),
        icon_text = |p| p.background,
    }
}

color_group! {
    TabColors from Palette {
        bar_background = |p| p.panel,
        text = |p| p.text_muted,
        secondary_text = |p| p.text_muted,
        active_background = |p| p.background,
        active_text = |p| p.text,
        active_indicator = |p| p.accent,
        separator = |p| p.border,
        attention = |p| p.warning,
    }
}

color_group! {
    PaneColors from Palette {
        header_background = |p| p.panel,
        header_text = |p| p.text_muted,
        header_border = |p| p.border,
        focus_outline = |p| p.accent,
        divider = |p| p.border,
        attention = |p| p.warning,
    }
}

color_group! {
    DiffColors from Palette {
        added_background = |p| p.diff_added,
        removed_background = |p| p.diff_removed,
        hunk_background = |p| p.panel,
        hunk_text = |p| p.text_dim,
        line_number_text = |p| p.text_dim,
        file_list_background = |p| p.panel,
        file_active_background = |p| p.active,
        added_marker = |p| p.success,
        modified_marker = |p| p.warning,
        removed_marker = |p| p.error,
        comment_background = |p| p.elevated,
        comment_border = |p| p.border,
    }
}

color_group! {
    /// Agent states, shown in the sidebar and the progress board.
    StatusColors from Palette {
        running = |p| p.accent,
        waiting = |p| p.warning,
        done = |p| p.success,
        idle = |p| p.text_dim,
        error = |p| p.error,
    }
}

color_group! {
    ButtonColors from Palette {
        background = |p| p.accent,
        text = |_| hex(0xffffff),
        secondary_background = |p| p.elevated,
        secondary_text = |p| p.text,
        secondary_border = |p| p.border,
    }
}

color_group! {
    InputColors from Palette {
        background = |p| p.background,
        text = |p| p.text,
        border = |p| p.border,
    }
}

color_group! {
    StatusbarColors from Palette {
        background = |p| p.panel,
        text = |p| p.text_muted,
        border = |p| p.border,
        key_background = |p| p.elevated,
    }
}

color_group! {
    /// The notifications shown for a while in a corner of the window.
    ToastColors from Palette {
        background = |p| p.elevated,
        border = |p| p.border,
        text = |p| p.text,
        secondary_text = |p| p.text_muted,
        hover_background = |p| p.active,
        done = |p| p.success,
        waiting = |p| p.warning,
        message = |p| p.accent,
    }
}

color_group! {
    /// The menu a right click opens on a tab, a workspace, a row of the
    /// session list or a pane.
    MenuColors from Palette {
        background = |p| p.elevated,
        border = |p| p.border,
        text = |p| p.text,
        key_text = |p| p.text_muted,
        disabled_text = |p| p.text_dim,
        selected_background = |p| p.active,
    }
}

color_group! {
    /// The progress board, opened above the panes. Its four states take
    /// the colors of `colors.status`.
    BoardColors from Palette {
        background = |p| p.background,
        panel = |p| p.panel,
        border = |p| p.border,
        title_text = |p| p.text,
        label_text = |p| p.text_muted,
        axis_text = |p| p.text_dim,
        grid_line = |p| p.border,
        now_line = |p| p.text,
        row_hover_background = |p| p.elevated,
        range_active_background = |p| p.active,
        node_background = |p| p.elevated,
        link_line = |p| p.border,
        message_line = |p| p.text_muted,
        selected_border = |p| p.accent,
    }
}

/// The colors of each component.
#[derive(Clone, Debug, PartialEq)]
pub struct Colors {
    pub terminal: TerminalColors,
    pub titlebar: TitlebarColors,
    pub sidebar: SidebarColors,
    pub workspace: WorkspaceColors,
    pub tab: TabColors,
    pub pane: PaneColors,
    pub diff: DiffColors,
    pub status: StatusColors,
    pub button: ButtonColors,
    pub input: InputColors,
    pub statusbar: StatusbarColors,
    pub toast: ToastColors,
    pub menu: MenuColors,
    pub board: BoardColors,
}

impl Colors {
    pub(crate) const KEYS: &'static [(&'static str, &'static [&'static str])] = &[
        ("terminal", TerminalColors::KEYS),
        ("titlebar", TitlebarColors::KEYS),
        ("sidebar", SidebarColors::KEYS),
        ("workspace", WorkspaceColors::KEYS),
        ("tab", TabColors::KEYS),
        ("pane", PaneColors::KEYS),
        ("diff", DiffColors::KEYS),
        ("status", StatusColors::KEYS),
        ("button", ButtonColors::KEYS),
        ("input", InputColors::KEYS),
        ("statusbar", StatusbarColors::KEYS),
        ("toast", ToastColors::KEYS),
        ("menu", MenuColors::KEYS),
        ("board", BoardColors::KEYS),
    ];

    fn derive(p: &Palette) -> Self {
        Self {
            terminal: TerminalColors::derive(p),
            titlebar: TitlebarColors::derive(p),
            sidebar: SidebarColors::derive(p),
            workspace: WorkspaceColors::derive(p),
            tab: TabColors::derive(p),
            pane: PaneColors::derive(p),
            diff: DiffColors::derive(p),
            status: StatusColors::derive(p),
            button: ButtonColors::derive(p),
            input: InputColors::derive(p),
            statusbar: StatusbarColors::derive(p),
            toast: ToastColors::derive(p),
            menu: MenuColors::derive(p),
            board: BoardColors::derive(p),
        }
    }

    fn read(r: &mut Reader, table: Option<&Table>, p: &Palette) -> Self {
        let path = "colors";
        r.unknown_keys(
            table,
            path,
            &Self::KEYS.iter().map(|(k, _)| *k).collect::<Vec<_>>(),
        );
        macro_rules! sub {
            ($ty:ident, $name:literal) => {{
                let t = r.table(table, path, $name);
                $ty::read(r, t, concat!("colors.", $name), p)
            }};
        }
        Self {
            terminal: sub!(TerminalColors, "terminal"),
            titlebar: sub!(TitlebarColors, "titlebar"),
            sidebar: sub!(SidebarColors, "sidebar"),
            workspace: sub!(WorkspaceColors, "workspace"),
            tab: sub!(TabColors, "tab"),
            pane: sub!(PaneColors, "pane"),
            diff: sub!(DiffColors, "diff"),
            status: sub!(StatusColors, "status"),
            button: sub!(ButtonColors, "button"),
            input: sub!(InputColors, "input"),
            statusbar: sub!(StatusbarColors, "statusbar"),
            toast: sub!(ToastColors, "toast"),
            menu: sub!(MenuColors, "menu"),
            board: sub!(BoardColors, "board"),
        }
    }
}

/// A font: families in order of preference, and a size in points.
#[derive(Clone, Debug, PartialEq)]
pub struct Font {
    /// Never empty. Later families are fallbacks for missing fonts.
    pub family: Vec<String>,
    pub size: f32,
}

/// Font sizes outside this range are replaced by the default.
pub const FONT_SIZE_RANGE: (f32, f32) = (6.0, 72.0);
/// Line heights, as a multiple of the font size.
pub const LINE_HEIGHT_RANGE: (f32, f32) = (1.0, 3.0);
/// Padding around the terminal grid, in points.
pub const PADDING_RANGE: (f32, f32) = (0.0, 100.0);

/// The terminal grid: its font, line height and padding.
#[derive(Clone, Debug, PartialEq)]
pub struct TerminalSettings {
    pub font: Font,
    /// A multiple of the font size.
    pub line_height: f32,
    /// Space between the pane edge and the grid, in points.
    pub padding: f32,
    /// Option with a key sends the key with an escape in front instead of
    /// typing the character the layout gives it.
    pub option_as_meta: bool,
}

impl TerminalSettings {
    pub(crate) const KEYS: &'static [&'static str] = &[
        "font_family",
        "font_size",
        "line_height",
        "padding",
        "option_as_meta",
    ];

    fn read(r: &mut Reader, table: Option<&Table>) -> Self {
        let d = Self::default();
        let path = "terminal";
        r.unknown_keys(table, path, Self::KEYS);
        Self {
            font: Font {
                family: r.families(table, path, "font_family", d.font.family),
                size: r.number(table, path, "font_size", d.font.size, FONT_SIZE_RANGE),
            },
            line_height: r.number(table, path, "line_height", d.line_height, LINE_HEIGHT_RANGE),
            padding: r.number(table, path, "padding", d.padding, PADDING_RANGE),
            option_as_meta: r.boolean(table, path, "option_as_meta", d.option_as_meta),
        }
    }
}

impl Default for TerminalSettings {
    fn default() -> Self {
        Self {
            font: Font {
                // Menlo ships with every macOS, so it stands in when Sarasa
                // Fixed J is not installed.
                family: vec!["Sarasa Fixed J".into(), "Menlo".into()],
                size: 13.0,
            },
            line_height: 1.5,
            padding: 10.0,
            // As in Ghostty and Terminal.app: option types the layout's
            // characters.
            option_as_meta: false,
        }
    }
}

/// Everything outside the terminal grid: tabs, sidebar, diff view.
#[derive(Clone, Debug, PartialEq)]
pub struct UiSettings {
    pub font: Font,
}

impl UiSettings {
    pub(crate) const KEYS: &'static [&'static str] = &["font_family", "font_size"];

    fn read(r: &mut Reader, table: Option<&Table>) -> Self {
        let d = Self::default();
        let path = "ui";
        r.unknown_keys(table, path, Self::KEYS);
        Self {
            font: Font {
                family: r.families(table, path, "font_family", d.font.family),
                size: r.number(table, path, "font_size", d.font.size, FONT_SIZE_RANGE),
            },
        }
    }
}

impl Default for UiSettings {
    fn default() -> Self {
        Self {
            font: Font {
                // GPUI's name for the macOS system font.
                family: vec![".SystemUIFont".into()],
                size: 13.0,
            },
        }
    }
}

/// Defaults for new tasks.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TaskSettings {
    pub default_agent: Option<TaskAgent>,
}

impl TaskSettings {
    fn read(r: &mut Reader, table: Option<&Table>) -> Self {
        let path = "tasks";
        r.unknown_keys(table, path, &["default_agent"]);
        Self {
            default_agent: r
                .table(table, path, "default_agent")
                .and_then(|table| TaskAgent::read(r, table)),
        }
    }
}

/// An agent started directly in a new task.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskAgent {
    pub agent: Agent,
    pub model: Option<String>,
}

impl TaskAgent {
    const KEYS: &'static [&'static str] = &["agent", "model"];

    fn read(r: &mut Reader, table: &Table) -> Option<Self> {
        let table = Some(table);
        let path = "tasks.default_agent";
        r.unknown_keys(table, path, Self::KEYS);
        let agent = r.optional_string(table, path, "agent");
        let model = r
            .optional_string(table, path, "model")
            .filter(|model| !model.trim().is_empty());
        let agent = match agent.as_deref() {
            Some("claude") => Some(Agent::Claude),
            Some("codex") => Some(Agent::Codex),
            None | Some("") => None,
            Some(_) => {
                r.warn(
                    path,
                    "agent",
                    crate::read::Problem::WrongType {
                        expected: "\"claude\" or \"codex\"",
                    },
                );
                None
            }
        };
        agent.map(|agent| Self { agent, model })
    }
}

/// All settings.
#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    pub palette: Palette,
    pub colors: Colors,
    pub terminal: TerminalSettings,
    pub ui: UiSettings,
    pub pty_log: PtyLogSettings,
    pub session: SessionSettings,
    pub worktrees: WorktreeSettings,
    pub tasks: TaskSettings,
    pub stall: StallSettings,
    pub report: ReportSettings,
    pub ipc: IpcSettings,
}

impl Config {
    pub(crate) const KEYS: &'static [&'static str] = &[
        "palette",
        "colors",
        "terminal",
        "ui",
        "pty_log",
        "session",
        "worktrees",
        "tasks",
        "stall",
        "report",
        "ipc",
    ];

    pub(crate) fn read(r: &mut Reader, root: &Table) -> Self {
        let root = Some(root);
        r.unknown_keys(root, "", Self::KEYS);
        let palette_table = r.table(root, "", "palette");
        let palette = Palette::read(r, palette_table, "palette", &());
        let colors_table = r.table(root, "", "colors");
        let colors = Colors::read(r, colors_table, &palette);
        let terminal_table = r.table(root, "", "terminal");
        let terminal = TerminalSettings::read(r, terminal_table);
        let ui_table = r.table(root, "", "ui");
        let ui = UiSettings::read(r, ui_table);
        let pty_log_table = r.table(root, "", "pty_log");
        let pty_log = PtyLogSettings::read(r, pty_log_table);
        let session_table = r.table(root, "", "session");
        let session = SessionSettings::read(r, session_table);
        let worktrees_table = r.table(root, "", "worktrees");
        let worktrees = WorktreeSettings::read(r, worktrees_table);
        let tasks_table = r.table(root, "", "tasks");
        let tasks = TaskSettings::read(r, tasks_table);
        let stall_table = r.table(root, "", "stall");
        let stall = StallSettings::read(r, stall_table);
        let report_table = r.table(root, "", "report");
        let report = ReportSettings::read(r, report_table);
        let ipc_table = r.table(root, "", "ipc");
        let ipc = IpcSettings::read(r, ipc_table);
        Self {
            palette,
            colors,
            terminal,
            ui,
            pty_log,
            session,
            worktrees,
            tasks,
            stall,
            report,
            ipc,
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        let palette = Palette::derive(&());
        Self {
            colors: Colors::derive(&palette),
            palette,
            terminal: TerminalSettings::default(),
            ui: UiSettings::default(),
            pty_log: PtyLogSettings::default(),
            session: SessionSettings::default(),
            worktrees: WorktreeSettings::default(),
            tasks: TaskSettings::default(),
            stall: StallSettings::default(),
            report: ReportSettings::default(),
            ipc: IpcSettings::default(),
        }
    }
}

/// Every dotted key the file accepts, such as `colors.tab.text`.
pub fn setting_keys() -> Vec<String> {
    let mut keys = Vec::new();
    let mut add = |section: &str, names: &[&str]| {
        keys.extend(names.iter().map(|n| format!("{section}.{n}")));
    };
    add("palette", Palette::KEYS);
    for (group, names) in Colors::KEYS {
        add(&format!("colors.{group}"), names);
    }
    add("terminal", TerminalSettings::KEYS);
    add("ui", UiSettings::KEYS);
    add("pty_log", PtyLogSettings::KEYS);
    add("session", SessionSettings::KEYS);
    add("worktrees", WorktreeSettings::KEYS);
    add("tasks.default_agent", TaskAgent::KEYS);
    add("stall", StallSettings::KEYS);
    add("report", ReportSettings::KEYS);
    add("ipc", IpcSettings::KEYS);
    keys
}

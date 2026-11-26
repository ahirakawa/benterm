//! The theme built from the config, and how terminal colors resolve
//! against it.

use benterm_config::{Config, parse};
use benterm_core::{Color, Rgb};
use benterm_layout::WorkspaceColor;
use benterm_ui::theme::Theme;

fn from_toml(text: &str) -> Theme {
    Theme::from_config(&parse(text).unwrap().config)
}

#[test]
fn the_default_theme_is_the_dark_theme_of_the_mock() {
    let terminal = Theme::default().terminal;
    assert_eq!(terminal.background, Rgb(0x1a, 0x1b, 0x1e));
    assert_eq!(terminal.foreground, Rgb(0xd4, 0xd6, 0xdb));
    assert_eq!(terminal.cursor, terminal.foreground);
    assert_eq!(terminal.ansi[1], Rgb(0xef, 0x6f, 0x6f));
    assert_eq!(terminal.ansi[2], Rgb(0x7f, 0xc9, 0x7f));
    assert_eq!(terminal.ansi[3], Rgb(0xf0, 0xb3, 0x5a));
    assert_eq!(terminal.ansi[4], Rgb(0x6a, 0xa1, 0xff));
    assert_eq!(terminal.padding, 10.0);
    assert_eq!(terminal.font_size, 13.0);
}

#[test]
fn small_text_is_one_point_smaller_and_no_more() {
    let theme = Theme::default();
    assert_eq!(theme.sidebar.font_size, 13.0);
    assert_eq!(benterm_ui::theme::small_text(theme.sidebar.font_size), 12.0);
}

#[test]
fn the_default_color_resolves_to_none() {
    assert_eq!(Theme::default().terminal.resolve(Color::Default), None);
}

#[test]
fn rgb_colors_resolve_to_themselves() {
    let terminal = Theme::default().terminal;
    assert_eq!(terminal.resolve(Color::Rgb(1, 2, 3)), Some(Rgb(1, 2, 3)));
}

#[test]
fn the_first_16_indexes_are_the_ansi_colors() {
    let terminal = Theme::default().terminal;
    for index in 0..16u8 {
        assert_eq!(
            terminal.resolve(Color::Indexed(index)),
            Some(terminal.ansi[usize::from(index)])
        );
    }
}

#[test]
fn indexes_16_to_231_are_the_color_cube() {
    let terminal = Theme::default().terminal;
    let cube = |index| terminal.resolve(Color::Indexed(index)).unwrap();
    assert_eq!(cube(16), Rgb(0, 0, 0));
    assert_eq!(cube(21), Rgb(0, 0, 255));
    assert_eq!(cube(196), Rgb(255, 0, 0));
    assert_eq!(cube(231), Rgb(255, 255, 255));
    assert_eq!(cube(67), Rgb(95, 135, 175));
}

#[test]
fn indexes_232_to_255_are_the_gray_ramp() {
    let terminal = Theme::default().terminal;
    assert_eq!(terminal.resolve(Color::Indexed(232)), Some(Rgb(8, 8, 8)));
    assert_eq!(
        terminal.resolve(Color::Indexed(255)),
        Some(Rgb(238, 238, 238))
    );
}

#[test]
fn reported_colors_are_the_theme_colors() {
    let terminal = Theme::default().terminal;
    let reported = terminal.reported_colors();
    assert_eq!(reported.foreground, terminal.foreground);
    assert_eq!(reported.background, terminal.background);
    assert_eq!(reported.cursor, terminal.cursor);
    assert_eq!(reported.palette[4], terminal.ansi[4]);
    assert_eq!(reported.palette[232], Rgb(8, 8, 8));
}

#[test]
fn the_default_font_is_sarasa_fixed_j() {
    let terminal = Theme::default().terminal;
    assert_eq!(terminal.font_families[0], "Sarasa Fixed J");
    let installed = ["Menlo".to_owned(), "Sarasa Fixed J".to_owned()];
    assert_eq!(terminal.installed_font_family(&installed), "Sarasa Fixed J");
}

#[test]
fn a_missing_font_falls_back_to_the_system_monospace_font() {
    let terminal = Theme::default().terminal;
    let installed = ["Menlo".to_owned(), "Helvetica".to_owned()];
    assert_eq!(terminal.installed_font_family(&installed), "Menlo");
}

#[test]
fn the_default_theme_is_the_theme_of_the_default_config() {
    assert_eq!(Theme::default(), Theme::from_config(&Config::default()));
}

#[test]
fn terminal_colors_come_from_the_config() {
    let terminal = from_toml(
        r##"
        [colors.terminal]
        foreground = "#010203"
        background = "#040506"
        cursor = "#070809"
        cursor_text = "#0a0b0c"
        black = "#100000"
        red = "#110000"
        bright_black = "#180000"
        bright_white = "#1f0000"
        "##,
    )
    .terminal;
    assert_eq!(terminal.foreground, Rgb(1, 2, 3));
    assert_eq!(terminal.background, Rgb(4, 5, 6));
    assert_eq!(terminal.cursor, Rgb(7, 8, 9));
    assert_eq!(terminal.cursor_text, Rgb(10, 11, 12));
    assert_eq!(terminal.ansi[0], Rgb(0x10, 0, 0));
    assert_eq!(terminal.ansi[1], Rgb(0x11, 0, 0));
    assert_eq!(terminal.ansi[8], Rgb(0x18, 0, 0));
    assert_eq!(terminal.ansi[15], Rgb(0x1f, 0, 0));
}

#[test]
fn the_ansi_colors_follow_the_order_of_their_names() {
    let c = Config::default().colors.terminal;
    let names = [
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
    ];
    let terminal = Theme::default().terminal;
    for (index, color) in names.iter().enumerate() {
        assert_eq!(terminal.ansi[index], Rgb(color.r, color.g, color.b));
    }
}

#[test]
fn a_palette_color_reaches_the_terminal_through_its_default() {
    let terminal = from_toml("[palette]\nterminal_background = \"#202122\"").terminal;
    assert_eq!(terminal.background, Rgb(0x20, 0x21, 0x22));
    assert_eq!(terminal.cursor_text, Rgb(0x20, 0x21, 0x22));
}

#[test]
fn font_line_height_and_padding_come_from_the_config() {
    let terminal = from_toml(
        r#"
        [terminal]
        font_family = ["Fira Code", "Monaco"]
        font_size = 16
        line_height = 1.25
        padding = 4
        "#,
    )
    .terminal;
    assert_eq!(terminal.font_families, vec!["Fira Code", "Monaco"]);
    assert_eq!(terminal.font_size, 16.0);
    assert_eq!(terminal.line_height, 1.25);
    assert_eq!(terminal.padding, 4.0);
}

#[test]
fn the_first_installed_family_is_used() {
    let terminal =
        from_toml("[terminal]\nfont_family = [\"Nope\", \"Monaco\", \"Menlo\"]").terminal;
    let installed = ["Menlo".to_owned(), "Monaco".to_owned()];
    assert_eq!(terminal.installed_font_family(&installed), "Monaco");
}

#[test]
fn without_any_installed_family_the_system_monospace_font_is_used() {
    let terminal = from_toml("[terminal]\nfont_family = \"Nope\"").terminal;
    assert_eq!(terminal.installed_font_family(&[]), "Menlo");
}

#[test]
fn the_row_height_is_the_font_size_times_the_line_height_in_whole_pixels() {
    let terminal = from_toml("[terminal]\nfont_size = 12.5\nline_height = 1.5").terminal;
    // 18.75, rounded up so that rows meet without a seam.
    assert_eq!(terminal.row_height(), 19.0);
    let terminal = from_toml("[terminal]\nfont_size = 10\nline_height = 1.2").terminal;
    assert_eq!(terminal.row_height(), 12.0);
}

#[test]
fn the_notice_uses_the_palette_and_the_ui_font() {
    let notice = from_toml(
        r##"
        [palette]
        elevated = "#111111"
        text = "#222222"
        warning = "#333333"
        [ui]
        font_family = "Helvetica"
        font_size = 14
        "##,
    )
    .notice;
    assert_eq!(notice.background, Rgb(0x11, 0x11, 0x11));
    assert_eq!(notice.text, Rgb(0x22, 0x22, 0x22));
    assert_eq!(notice.accent, Rgb(0x33, 0x33, 0x33));
    assert_eq!(notice.font_family, "Helvetica");
    assert_eq!(notice.font_size, 14.0);
}

#[test]
fn the_tab_bar_takes_the_tab_colors_and_ui_font_of_the_config() {
    let tabs = Theme::default().tabs;
    assert_eq!(tabs.bar_background, Rgb(0x25, 0x26, 0x2a));
    assert_eq!(tabs.active_background, Rgb(0x1e, 0x1f, 0x22));
    assert_eq!(tabs.active_indicator, Rgb(0x6a, 0xa1, 0xff));
    assert_eq!(tabs.text, Rgb(0x8b, 0x8f, 0x98));
    assert_eq!(tabs.active_text, Rgb(0xd4, 0xd6, 0xdb));
    assert_eq!(tabs.secondary_text, Rgb(0x8b, 0x8f, 0x98));
    assert_eq!(tabs.separator, Rgb(0x39, 0x3b, 0x41));

    let tabs = from_toml(
        "[colors.tab]\nactive_indicator = \"#010203\"\nseparator = \"#040506\"\n\
         [ui]\nfont_family = \"Helvetica\"\nfont_size = 14",
    )
    .tabs;
    assert_eq!(tabs.active_indicator, Rgb(1, 2, 3));
    assert_eq!(tabs.separator, Rgb(4, 5, 6));
    assert_eq!(tabs.font_family, "Helvetica");
    assert_eq!(tabs.font_size, 14.0);
}

#[test]
fn the_new_tab_button_lights_up_like_the_new_workspace_button() {
    // The + in the tab bar is drawn as the + by the workspaces' heading:
    // the bar's muted text, and under the mouse the hover background and
    // the bright text.
    let theme = Theme::default();
    assert_eq!(
        theme.tabs.button_hover_background,
        theme.sidebar.item_hover_background
    );
    assert_eq!(theme.tabs.button_hover_background, Rgb(0x2d, 0x2f, 0x34));

    let tabs = from_toml("[palette]\nelevated = \"#010203\"").tabs;
    assert_eq!(tabs.button_hover_background, Rgb(1, 2, 3));
}

#[test]
fn the_pane_lines_take_the_pane_colors_of_the_config() {
    let panes = Theme::default().panes;
    assert_eq!(panes.divider, Rgb(0x39, 0x3b, 0x41));
    assert_eq!(panes.focus_outline, Rgb(0x6a, 0xa1, 0xff));

    let panes =
        from_toml("[colors.pane]\ndivider = \"#010203\"\nfocus_outline = \"#040506\"").panes;
    assert_eq!(panes.divider, Rgb(1, 2, 3));
    assert_eq!(panes.focus_outline, Rgb(4, 5, 6));
}

#[test]
fn the_pane_lines_follow_the_palette_through_their_defaults() {
    let panes = from_toml("[palette]\nborder = \"#010203\"\naccent = \"#040506\"").panes;
    assert_eq!(panes.divider, Rgb(1, 2, 3));
    assert_eq!(panes.focus_outline, Rgb(4, 5, 6));
}

#[test]
fn the_sidebar_takes_the_sidebar_colors_and_ui_font_of_the_config() {
    let sidebar = Theme::default().sidebar;
    assert_eq!(sidebar.background, Rgb(0x25, 0x26, 0x2a));
    assert_eq!(sidebar.border, Rgb(0x39, 0x3b, 0x41));
    assert_eq!(sidebar.section_header_text, Rgb(0x8b, 0x8f, 0x98));
    assert_eq!(sidebar.item_text, Rgb(0xd4, 0xd6, 0xdb));
    assert_eq!(sidebar.item_secondary_text, Rgb(0x8b, 0x8f, 0x98));
    assert_eq!(sidebar.item_hover_background, Rgb(0x2d, 0x2f, 0x34));
    assert_eq!(sidebar.item_active_background, Rgb(0x34, 0x36, 0x3c));
    assert_eq!(sidebar.item_active_indicator, Rgb(0x6a, 0xa1, 0xff));

    let sidebar = from_toml(
        "[colors.sidebar]\nbackground = \"#010203\"\nitem_active_indicator = \"#040506\"\n\
         [ui]\nfont_family = \"Helvetica\"\nfont_size = 14",
    )
    .sidebar;
    assert_eq!(sidebar.background, Rgb(1, 2, 3));
    assert_eq!(sidebar.item_active_indicator, Rgb(4, 5, 6));
    assert_eq!(sidebar.font_family, "Helvetica");
    assert_eq!(sidebar.font_size, 14.0);
}

#[test]
fn every_workspace_color_is_read_from_the_config() {
    let sidebar = from_toml(
        "[colors.workspace]\nicon_1 = \"#010101\"\nicon_2 = \"#020202\"\n\
         icon_3 = \"#030303\"\nicon_4 = \"#040404\"\nicon_5 = \"#050505\"\n\
         icon_6 = \"#060606\"\nicon_text = \"#070707\"",
    )
    .sidebar;
    for color in WorkspaceColor::all() {
        let n = color.index() as u8 + 1;
        assert_eq!(sidebar.workspace_color(color), Rgb(n, n, n));
    }
    assert_eq!(sidebar.workspace_icon_text, Rgb(7, 7, 7));
}

#[test]
fn the_default_workspace_colors_all_differ() {
    let sidebar = Theme::default().sidebar;
    let colors = WorkspaceColor::all().map(|color| sidebar.workspace_color(color));
    assert_eq!(colors[0], Rgb(0x6a, 0xa1, 0xff));
    for (i, a) in colors.iter().enumerate() {
        for b in &colors[i + 1..] {
            assert_ne!(a, b);
        }
    }
}

#[test]
fn the_command_palette_is_drawn_like_the_mock_by_default() {
    let palette = Theme::default().command_palette;
    assert_eq!(palette.background, Rgb(0x2d, 0x2f, 0x34));
    assert_eq!(palette.border, Rgb(0x39, 0x3b, 0x41));
    assert_eq!(palette.text, Rgb(0xd4, 0xd6, 0xdb));
    assert_eq!(palette.secondary_text, Rgb(0x8b, 0x8f, 0x98));
    assert_eq!(palette.selected_background, Rgb(0x34, 0x36, 0x3c));
    assert_eq!(palette.accent, Rgb(0x6a, 0xa1, 0xff));
    assert_eq!(palette.input_background, Rgb(0x1e, 0x1f, 0x22));
    assert_eq!(palette.input_border, Rgb(0x39, 0x3b, 0x41));
}

#[test]
fn the_command_palette_uses_the_palette_the_input_colors_and_the_ui_font() {
    let palette = from_toml(
        r##"
        [palette]
        elevated = "#111111"
        text = "#222222"
        text_muted = "#333333"
        active = "#444444"
        accent = "#555555"
        border = "#666666"
        [colors.input]
        background = "#777777"
        border = "#888888"
        [ui]
        font_family = "Helvetica"
        font_size = 14
        "##,
    )
    .command_palette;
    assert_eq!(palette.background, Rgb(0x11, 0x11, 0x11));
    assert_eq!(palette.text, Rgb(0x22, 0x22, 0x22));
    assert_eq!(palette.secondary_text, Rgb(0x33, 0x33, 0x33));
    assert_eq!(palette.selected_background, Rgb(0x44, 0x44, 0x44));
    assert_eq!(palette.accent, Rgb(0x55, 0x55, 0x55));
    assert_eq!(palette.border, Rgb(0x66, 0x66, 0x66));
    assert_eq!(palette.input_background, Rgb(0x77, 0x77, 0x77));
    assert_eq!(palette.input_border, Rgb(0x88, 0x88, 0x88));
    assert_eq!(palette.font_family, "Helvetica");
    assert_eq!(palette.font_size, 14.0);
}

#[test]
fn the_attention_marks_take_the_warning_color_unless_the_config_sets_their_own() {
    let theme = Theme::default();
    let warning = Rgb(0xf0, 0xb3, 0x5a);
    assert_eq!(theme.tabs.attention, warning);
    assert_eq!(theme.sidebar.attention, warning);
    assert_eq!(theme.panes.attention, warning);

    let theme = from_toml("[palette]\nwarning = \"#010203\"");
    assert_eq!(theme.tabs.attention, Rgb(1, 2, 3));
    assert_eq!(theme.sidebar.attention, Rgb(1, 2, 3));
    assert_eq!(theme.panes.attention, Rgb(1, 2, 3));

    let theme = from_toml(
        "[colors.tab]\nattention = \"#040506\"\n[colors.sidebar]\nattention = \"#070809\"\n\
         [colors.pane]\nattention = \"#0a0b0c\"",
    );
    assert_eq!(theme.tabs.attention, Rgb(4, 5, 6));
    assert_eq!(theme.sidebar.attention, Rgb(7, 8, 9));
    assert_eq!(theme.panes.attention, Rgb(10, 11, 12));
}

#[test]
fn the_in_app_notifications_take_the_palette_unless_the_config_sets_their_own() {
    let toast = Theme::default().toast;
    assert_eq!(toast.background, Rgb(0x2d, 0x2f, 0x34));
    assert_eq!(toast.border, Rgb(0x39, 0x3b, 0x41));
    assert_eq!(toast.text, Rgb(0xd4, 0xd6, 0xdb));
    assert_eq!(toast.secondary_text, Rgb(0x8b, 0x8f, 0x98));
    assert_eq!(toast.hover_background, Rgb(0x34, 0x36, 0x3c));
    assert_eq!(toast.done, Rgb(0x7f, 0xc9, 0x7f));
    assert_eq!(toast.waiting, Rgb(0xf0, 0xb3, 0x5a));
    assert_eq!(toast.message, Rgb(0x6a, 0xa1, 0xff));

    let toast = from_toml("[palette]\nsuccess = \"#010203\"\nelevated = \"#040506\"").toast;
    assert_eq!(toast.done, Rgb(1, 2, 3));
    assert_eq!(toast.background, Rgb(4, 5, 6));

    let toast = from_toml(
        "[colors.toast]\nbackground = \"#010203\"\nwaiting = \"#040506\"\n\
         message = \"#070809\"\n[ui]\nfont_size = 14",
    )
    .toast;
    assert_eq!(toast.background, Rgb(1, 2, 3));
    assert_eq!(toast.waiting, Rgb(4, 5, 6));
    assert_eq!(toast.message, Rgb(7, 8, 9));
    assert_eq!(toast.font_size, 14.0);
}

#[test]
fn the_progress_board_is_drawn_like_the_mock_by_default() {
    let board = Theme::default().board;
    assert_eq!(board.background, Rgb(0x1e, 0x1f, 0x22));
    assert_eq!(board.panel, Rgb(0x25, 0x26, 0x2a));
    assert_eq!(board.border, Rgb(0x39, 0x3b, 0x41));
    assert_eq!(board.title_text, Rgb(0xd4, 0xd6, 0xdb));
    assert_eq!(board.label_text, Rgb(0x8b, 0x8f, 0x98));
    assert_eq!(board.axis_text, Rgb(0x5f, 0x63, 0x6b));
    assert_eq!(board.grid_line, Rgb(0x39, 0x3b, 0x41));
    assert_eq!(board.now_line, Rgb(0xd4, 0xd6, 0xdb));
    assert_eq!(board.row_hover_background, Rgb(0x2d, 0x2f, 0x34));
    assert_eq!(board.range_active_background, Rgb(0x34, 0x36, 0x3c));
    // The graph, as the mock's graph of issues.
    assert_eq!(board.node_background, Rgb(0x2d, 0x2f, 0x34));
    assert_eq!(board.link_line, Rgb(0x39, 0x3b, 0x41));
    assert_eq!(board.message_line, Rgb(0x8b, 0x8f, 0x98));
    assert_eq!(board.selected_border, Rgb(0x6a, 0xa1, 0xff));
    assert_eq!(board.idle, Rgb(0x5f, 0x63, 0x6b));
    // The four states take the colors of the agent states.
    assert_eq!(board.working, Rgb(0x6a, 0xa1, 0xff));
    assert_eq!(board.waiting, Rgb(0xf0, 0xb3, 0x5a));
    assert_eq!(board.done, Rgb(0x7f, 0xc9, 0x7f));
    assert_eq!(board.error, Rgb(0xef, 0x6f, 0x6f));
}

#[test]
fn the_progress_board_takes_its_colors_and_the_state_colors_from_the_config() {
    let board = from_toml(
        "[colors.board]\npanel = \"#010203\"\nnow_line = \"#040506\"\n\
         [colors.status]\nrunning = \"#070809\"\nerror = \"#0a0b0c\"\n\
         [ui]\nfont_size = 14",
    )
    .board;
    assert_eq!(board.panel, Rgb(1, 2, 3));
    assert_eq!(board.now_line, Rgb(4, 5, 6));
    assert_eq!(board.working, Rgb(7, 8, 9));
    assert_eq!(board.error, Rgb(10, 11, 12));
    assert_eq!(board.font_size, 14.0);

    let board = from_toml("[palette]\nwarning = \"#010203\"\nactive = \"#040506\"").board;
    assert_eq!(board.waiting, Rgb(1, 2, 3));
    assert_eq!(board.range_active_background, Rgb(4, 5, 6));
}

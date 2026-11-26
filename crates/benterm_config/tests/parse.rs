use benterm_config::{Config, Loaded, Problem, Rgba, Warning, load, parse};

fn color(s: &str) -> Rgba {
    s.parse().unwrap()
}

fn ok(text: &str) -> Loaded {
    parse(text).expect("text should be TOML")
}

fn warning(key: &str, problem: Problem) -> Warning {
    Warning {
        key: key.into(),
        problem,
    }
}

#[test]
fn defaults_follow_the_dark_theme_of_the_mock() {
    let c = Config::default();
    assert_eq!(c.palette.background, color("#1e1f22"));
    assert_eq!(c.palette.border, color("#393b41"));
    assert_eq!(c.palette.accent, color("#6aa1ff"));
    assert_eq!(c.colors.terminal.background, color("#1a1b1e"));
    assert_eq!(c.colors.terminal.foreground, color("#d4d6db"));
    assert_eq!(c.colors.tab.active_background, color("#1e1f22"));
    assert_eq!(c.colors.sidebar.background, color("#25262a"));
    // The first workspace is the accent color, the next ones are told
    // apart from it and from each other.
    let icons = [
        c.colors.workspace.icon_1,
        c.colors.workspace.icon_2,
        c.colors.workspace.icon_3,
        c.colors.workspace.icon_4,
        c.colors.workspace.icon_5,
        c.colors.workspace.icon_6,
    ];
    assert_eq!(icons[0], c.palette.accent);
    for (i, a) in icons.iter().enumerate() {
        for b in &icons[i + 1..] {
            assert_ne!(a, b);
        }
    }
    assert_eq!(c.colors.workspace.icon_text, c.palette.background);
    assert_eq!(c.colors.diff.added_background, color("#1f3326"));
    assert_eq!(c.colors.diff.removed_background, color("#3a2224"));
    // Sarasa Fixed J first, then Menlo, the monospace font every macOS has.
    assert_eq!(c.terminal.font.family, vec!["Sarasa Fixed J", "Menlo"]);
    assert_eq!(c.terminal.font.size, 13.0);
    assert_eq!(c.terminal.line_height, 1.5);
    assert_eq!(c.terminal.padding, 10.0);
    // Option types the layout's characters, as on any Mac.
    assert!(!c.terminal.option_as_meta);
    assert_eq!(c.ui.font.size, 13.0);
    // What is read beside a name, such as a branch or the number of
    // panes, is one step dimmer than the name and no more.
    assert_eq!(c.colors.sidebar.item_secondary_text, c.palette.text_muted);
    assert_eq!(c.colors.tab.secondary_text, c.palette.text_muted);
}

#[test]
fn empty_text_gives_defaults_without_warnings() {
    assert_eq!(ok(""), Loaded::default());
}

#[test]
fn a_partial_file_overrides_only_what_it_sets() {
    let loaded = ok("[colors.tab]\ntext = \"#ff0000\"\n\n[terminal]\nfont_size = 14\n");
    assert_eq!(loaded.warnings, vec![]);
    let mut expected = Config::default();
    expected.colors.tab.text = color("#ff0000");
    expected.terminal.font.size = 14.0;
    assert_eq!(loaded.config, expected);
}

#[test]
fn component_colors_follow_the_palette() {
    let c = ok("[palette]\naccent = \"#ff0000\"\n").config;
    assert_eq!(c.palette.accent, color("#ff0000"));
    assert_eq!(c.colors.tab.active_indicator, color("#ff0000"));
    assert_eq!(c.colors.pane.focus_outline, color("#ff0000"));
    assert_eq!(c.colors.terminal.selection_background, color("#ff00004d"));
}

#[test]
fn a_component_color_set_in_the_file_beats_the_palette() {
    let c = ok("[palette]\naccent = \"#ff0000\"\n[colors.tab]\nactive_indicator = \"#00ff00\"\n")
        .config;
    assert_eq!(c.colors.tab.active_indicator, color("#00ff00"));
    assert_eq!(c.colors.pane.focus_outline, color("#ff0000"));
}

#[test]
fn a_malformed_color_falls_back_and_warns_without_losing_its_neighbors() {
    let loaded = ok("[palette]\naccent = \"#12\"\nborder = \"#000000\"\n");
    assert_eq!(
        loaded.warnings,
        vec![warning(
            "palette.accent",
            Problem::InvalidColor("#12".into())
        )]
    );
    assert_eq!(
        loaded.config.palette.accent,
        Config::default().palette.accent
    );
    assert_eq!(loaded.config.palette.border, color("#000000"));
}

#[test]
fn a_value_of_the_wrong_type_falls_back_and_warns() {
    let loaded = ok("[colors.terminal]\nred = 3\n");
    assert_eq!(
        loaded.warnings,
        vec![warning(
            "colors.terminal.red",
            Problem::WrongType {
                expected: "a color string"
            }
        )]
    );
    assert_eq!(loaded.config, Config::default());
}

#[test]
fn an_out_of_range_number_falls_back_and_warns() {
    let loaded = ok("[terminal]\nfont_size = 200\nline_height = 0.5\npadding = 4.5\n");
    assert_eq!(
        loaded.warnings,
        vec![
            warning(
                "terminal.font_size",
                Problem::OutOfRange {
                    min: 6.0,
                    max: 72.0
                }
            ),
            warning(
                "terminal.line_height",
                Problem::OutOfRange { min: 1.0, max: 3.0 }
            ),
        ]
    );
    assert_eq!(loaded.config.terminal.font.size, 13.0);
    assert_eq!(loaded.config.terminal.line_height, 1.5);
    assert_eq!(loaded.config.terminal.padding, 4.5);
}

#[test]
fn non_finite_numbers_are_out_of_range() {
    let loaded = ok("[ui]\nfont_size = nan\n[terminal]\npadding = inf\n");
    assert_eq!(loaded.warnings.len(), 2);
    assert_eq!(loaded.config, Config::default());
}

#[test]
fn a_number_given_as_a_string_is_the_wrong_type() {
    let loaded = ok("[terminal]\nfont_size = \"14\"\n");
    assert_eq!(
        loaded.warnings,
        vec![warning(
            "terminal.font_size",
            Problem::WrongType {
                expected: "a number"
            }
        )]
    );
}

#[test]
fn unknown_keys_are_reported_at_every_level() {
    let loaded = ok(concat!(
        "theme = \"dark\"\n",
        "[palette]\nfoo = \"#000000\"\n",
        "[colors.nope]\nx = 1\n",
        "[colors.tab]\nbar = 1\n",
        "[terminal]\nfont = \"Menlo\"\n",
    ));
    let keys: Vec<_> = loaded.warnings.iter().map(|w| w.key.as_str()).collect();
    assert_eq!(
        keys,
        vec![
            "theme",
            "palette.foo",
            "colors.nope",
            "colors.tab.bar",
            "terminal.font"
        ]
    );
    assert!(
        loaded
            .warnings
            .iter()
            .all(|w| w.problem == Problem::UnknownKey)
    );
    assert_eq!(loaded.config, Config::default());
}

#[test]
fn a_section_that_is_not_a_table_falls_back_and_warns() {
    let loaded = ok("palette = 3\ncolors = { tab = \"red\" }\n");
    assert_eq!(
        loaded.warnings,
        vec![
            warning(
                "palette",
                Problem::WrongType {
                    expected: "a table"
                }
            ),
            warning(
                "colors.tab",
                Problem::WrongType {
                    expected: "a table"
                }
            ),
        ]
    );
    assert_eq!(loaded.config, Config::default());
}

#[test]
fn a_font_family_may_be_one_name_or_a_list() {
    let c = ok("[terminal]\nfont_family = \"Menlo\"\n[ui]\nfont_family = [\"A\", \"B\"]\n").config;
    assert_eq!(c.terminal.font.family, vec!["Menlo".to_string()]);
    assert_eq!(c.ui.font.family, vec!["A".to_string(), "B".to_string()]);
}

#[test]
fn an_empty_font_family_falls_back_and_warns() {
    for value in ["\"\"", "\"  \"", "[]", "[\"Menlo\", \"\"]"] {
        let loaded = ok(&format!("[terminal]\nfont_family = {value}\n"));
        assert_eq!(
            loaded.warnings,
            vec![warning("terminal.font_family", Problem::EmptyFontFamily)],
            "{value}"
        );
        assert_eq!(loaded.config, Config::default(), "{value}");
    }
}

#[test]
fn a_font_family_list_with_a_non_string_is_the_wrong_type() {
    let loaded = ok("[terminal]\nfont_family = [\"Menlo\", 1]\n");
    assert_eq!(
        loaded.warnings,
        vec![warning(
            "terminal.font_family",
            Problem::WrongType {
                expected: "a string or a list of strings"
            }
        )]
    );
}

#[test]
fn text_that_is_not_toml_is_an_error() {
    let err = parse("[palette\naccent = ").unwrap_err();
    assert_eq!(err.key, "");
    assert!(matches!(err.problem, Problem::Syntax(_)));
}

#[test]
fn load_turns_a_syntax_error_into_defaults_and_a_warning() {
    let loaded = load("[palette\n");
    assert_eq!(loaded.config, Config::default());
    assert_eq!(loaded.warnings.len(), 1);
    assert!(matches!(loaded.warnings[0].problem, Problem::Syntax(_)));
}

#[test]
fn warnings_name_the_key_and_the_value() {
    let w = warning("palette.accent", Problem::InvalidColor("#12".into()));
    let s = w.to_string();
    assert!(s.contains("palette.accent"), "{s}");
    assert!(s.contains("#12"), "{s}");
}

#[test]
fn option_can_be_set_to_send_meta() {
    let loaded = ok("[terminal]\noption_as_meta = true\n");
    assert_eq!(loaded.warnings, vec![]);
    assert!(loaded.config.terminal.option_as_meta);
}

#[test]
fn option_as_meta_that_is_not_a_boolean_falls_back_and_warns() {
    let loaded = ok("[terminal]\noption_as_meta = \"left\"\n");
    assert!(!loaded.config.terminal.option_as_meta);
    assert_eq!(
        loaded.warnings,
        vec![warning(
            "terminal.option_as_meta",
            Problem::WrongType {
                expected: "true or false"
            }
        )]
    );
}

#[test]
fn workspace_icon_colors_can_be_set() {
    let c =
        ok("[palette]\naccent = \"#ff0000\"\n[colors.workspace]\nicon_2 = \"#010203\"\n").config;
    assert_eq!(c.colors.workspace.icon_1, color("#ff0000"));
    assert_eq!(c.colors.workspace.icon_2, color("#010203"));
}

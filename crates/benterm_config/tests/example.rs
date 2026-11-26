//! The sample file in `examples/config.toml` stays in step with the code.

use benterm_config::{Config, parse, setting_keys};
use toml::{Table, Value};

const EXAMPLE: &str = include_str!("../examples/config.toml");

fn leaf_keys(table: &Table, prefix: &str, out: &mut Vec<String>) {
    for (k, v) in table {
        let path = if prefix.is_empty() {
            k.clone()
        } else {
            format!("{prefix}.{k}")
        };
        match v {
            Value::Table(t) => leaf_keys(t, &path, out),
            _ => out.push(path),
        }
    }
}

#[test]
fn the_example_reads_without_warnings_and_matches_the_defaults() {
    let loaded = parse(EXAMPLE).unwrap();
    assert_eq!(loaded.warnings, vec![]);
    assert_eq!(loaded.config, Config::default());
}

#[test]
fn the_example_lists_every_setting() {
    let mut in_example = Vec::new();
    leaf_keys(&toml::from_str(EXAMPLE).unwrap(), "", &mut in_example);
    in_example.sort();
    let mut all = setting_keys();
    all.sort();
    assert_eq!(in_example, all);
}

#[test]
fn setting_keys_cover_every_section() {
    let keys = setting_keys();
    for k in [
        "palette.accent",
        "colors.terminal.bright_white",
        "colors.tab.active_background",
        "colors.sidebar.background",
        "colors.workspace.icon_1",
        "colors.workspace.icon_6",
        "colors.workspace.icon_text",
        "colors.diff.removed_background",
        "colors.menu.selected_background",
        "terminal.font_family",
        "terminal.line_height",
        "terminal.padding",
        "ui.font_size",
        "pty_log.enabled",
        "pty_log.dir",
        "session.scrollback_lines",
    ] {
        assert!(keys.iter().any(|x| x == k), "{k}");
    }
}

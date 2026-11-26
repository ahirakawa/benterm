//! How config events change the theme and the messages shown to the user.

use std::io;

use benterm_config::{ConfigEvent, Loaded, Problem, Warning, load, parse};
use benterm_core::Rgb;
use benterm_ui::settings::Settings;
use benterm_ui::theme::Theme;

fn applied(text: &str) -> ConfigEvent {
    ConfigEvent::Applied(parse(text).unwrap())
}

fn syntax_error() -> Warning {
    parse("[terminal").unwrap_err()
}

#[test]
fn the_theme_starts_from_the_loaded_config() {
    let settings = Settings::new(load("[terminal]\npadding = 3"));
    assert_eq!(settings.theme().terminal.padding, 3.0);
}

#[test]
fn warnings_of_the_loaded_config_are_messages() {
    let settings = Settings::new(load("[terminal]\npadding = -1\nfoo = 1"));
    assert_eq!(
        settings.messages(),
        vec![
            "terminal.foo: unknown setting",
            "terminal.padding: must be between 0 and 100",
        ]
    );
}

#[test]
fn a_config_without_warnings_has_no_messages() {
    assert!(Settings::new(Loaded::default()).messages().is_empty());
}

#[test]
fn an_applied_config_replaces_the_theme_and_the_messages() {
    let mut settings = Settings::new(load("[terminal]\nfoo = 1"));
    let changed = settings.apply(applied(
        "[colors.terminal]\nbackground = \"#010203\"\n[ui]\nbar = 1",
    ));
    assert!(changed);
    assert_eq!(settings.theme().terminal.background, Rgb(1, 2, 3));
    assert_eq!(settings.messages(), vec!["ui.bar: unknown setting"]);
}

#[test]
fn a_change_of_warnings_alone_leaves_the_theme_unchanged() {
    let mut settings = Settings::new(Loaded::default());
    let changed = settings.apply(applied("[ui]\nbar = 1"));
    assert!(!changed);
    assert_eq!(*settings.theme(), Theme::default());
    assert_eq!(settings.messages(), vec!["ui.bar: unknown setting"]);
}

#[test]
fn a_rejected_file_keeps_the_theme_and_says_so_first() {
    let mut settings = Settings::new(load("[terminal]\npadding = 3\nfoo = 1"));
    let changed = settings.apply(ConfigEvent::Rejected(syntax_error()));
    assert!(!changed);
    assert_eq!(settings.theme().terminal.padding, 3.0);
    let messages = settings.messages();
    assert_eq!(messages.len(), 2);
    assert!(messages[0].starts_with("the file is not valid TOML"));
    assert!(messages[0].ends_with("The previous settings stay in use."));
    // The settings in use still have their own problem.
    assert_eq!(messages[1], "terminal.foo: unknown setting");
}

#[test]
fn an_applied_config_clears_a_rejection() {
    let mut settings = Settings::new(Loaded::default());
    settings.apply(ConfigEvent::Rejected(syntax_error()));
    settings.apply(applied("[terminal]\npadding = 2"));
    assert!(settings.messages().is_empty());
}

#[test]
fn a_file_that_is_not_watched_is_reported() {
    let mut settings = Settings::new(Loaded::default());
    settings.not_watched(&io::Error::other("no watcher"));
    assert_eq!(
        settings.messages(),
        vec!["changes to the config file are not applied until restart: no watcher"]
    );
}

#[test]
fn an_unreadable_file_is_a_message() {
    let unreadable = Warning {
        key: String::new(),
        problem: Problem::Unreadable("permission denied".into()),
    };
    let mut settings = Settings::new(Loaded::default());
    settings.apply(ConfigEvent::Rejected(unreadable));
    assert_eq!(
        settings.messages(),
        vec!["the file could not be read: permission denied. The previous settings stay in use."]
    );
}

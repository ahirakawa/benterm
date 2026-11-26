use benterm_config::{Config, Problem, config_path_from, load_file};
use std::ffi::OsStr;
use std::path::PathBuf;

fn os(s: &str) -> Option<&OsStr> {
    Some(OsStr::new(s))
}

#[test]
fn the_path_is_under_xdg_config_home_when_set() {
    assert_eq!(
        config_path_from(os("/x/config"), os("/home/u")),
        Some(PathBuf::from("/x/config/benterm/config.toml"))
    );
}

#[test]
fn the_path_is_under_home_dot_config_otherwise() {
    let expected = Some(PathBuf::from("/home/u/.config/benterm/config.toml"));
    assert_eq!(config_path_from(None, os("/home/u")), expected);
    assert_eq!(config_path_from(os(""), os("/home/u")), expected);
    assert_eq!(config_path_from(os("relative"), os("/home/u")), expected);
}

#[test]
fn there_is_no_path_without_an_absolute_directory() {
    assert_eq!(config_path_from(None, None), None);
    assert_eq!(config_path_from(os("rel"), os("")), None);
    assert_eq!(config_path_from(None, os("rel")), None);
}

#[test]
fn a_missing_file_gives_defaults_without_warnings() {
    let dir = tempfile::tempdir().unwrap();
    let loaded = load_file(&dir.path().join("config.toml"));
    assert_eq!(loaded.config, Config::default());
    assert_eq!(loaded.warnings, vec![]);
}

#[test]
fn a_file_is_read() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(&path, "[terminal]\npadding = 4\nbogus = 1\n").unwrap();
    let loaded = load_file(&path);
    assert_eq!(loaded.config.terminal.padding, 4.0);
    assert_eq!(loaded.warnings.len(), 1);
}

#[test]
fn a_file_that_is_not_utf8_gives_defaults_and_a_warning() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(&path, b"[terminal]\npadding = \xff\n").unwrap();
    let loaded = load_file(&path);
    assert_eq!(loaded.config, Config::default());
    assert_eq!(loaded.warnings.len(), 1);
    assert!(matches!(loaded.warnings[0].problem, Problem::Unreadable(_)));
}

#[test]
fn a_directory_in_place_of_the_file_gives_defaults_and_a_warning() {
    let dir = tempfile::tempdir().unwrap();
    let loaded = load_file(dir.path());
    assert_eq!(loaded.config, Config::default());
    assert_eq!(loaded.warnings.len(), 1);
    assert!(matches!(loaded.warnings[0].problem, Problem::Unreadable(_)));
}

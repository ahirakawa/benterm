//! The `[pty_log]` section: whether to record PTY logs and where.

use std::path::PathBuf;

use benterm_config::{Config, Loaded, Problem, PtyLogSettings, Warning, parse};

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
fn recording_is_off_by_default() {
    let settings = Config::default().pty_log;
    assert!(!settings.enabled);
    assert_eq!(
        settings.dir,
        PathBuf::from("Library/Application Support/benterm/pty-logs")
    );
    assert_eq!(settings, PtyLogSettings::default());
}

#[test]
fn the_section_turns_recording_on_and_sets_the_directory() {
    let loaded = ok("[pty_log]\nenabled = true\ndir = \"logs/benterm\"\n");
    assert_eq!(loaded.warnings, vec![]);
    assert_eq!(
        loaded.config.pty_log,
        PtyLogSettings {
            enabled: true,
            dir: PathBuf::from("logs/benterm"),
        }
    );
}

#[test]
fn bad_values_keep_the_defaults_and_are_reported() {
    let loaded = ok("[pty_log]\nenabled = \"yes\"\ndir = 3\ncolor = 1\n");
    assert_eq!(loaded.config.pty_log, PtyLogSettings::default());
    let mut warnings = loaded.warnings;
    warnings.sort_by(|a, b| a.key.cmp(&b.key));
    assert_eq!(
        warnings,
        vec![
            warning("pty_log.color", Problem::UnknownKey),
            warning("pty_log.dir", Problem::WrongType { expected: "a path" }),
            warning(
                "pty_log.enabled",
                Problem::WrongType {
                    expected: "true or false"
                }
            ),
        ]
    );
}

#[test]
fn an_empty_directory_is_reported() {
    let loaded = ok("[pty_log]\ndir = \"\"\n");
    assert_eq!(loaded.config.pty_log, PtyLogSettings::default());
    assert_eq!(
        loaded.warnings,
        vec![warning(
            "pty_log.dir",
            Problem::WrongType { expected: "a path" }
        )]
    );
}

#[test]
fn a_section_that_is_not_a_table_is_reported() {
    let loaded = ok("pty_log = true\n");
    assert_eq!(loaded.config.pty_log, PtyLogSettings::default());
    assert_eq!(
        loaded.warnings,
        vec![warning(
            "pty_log",
            Problem::WrongType {
                expected: "a table"
            }
        )]
    );
}

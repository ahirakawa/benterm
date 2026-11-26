//! The `[worktrees]` section: where the worktrees of tasks are put.

use std::path::{Path, PathBuf};

use benterm_config::{Config, Problem, Warning, WorktreeSettings, parse};

#[test]
fn worktrees_go_below_the_home_directory_by_default() {
    let settings = Config::default().worktrees;
    assert_eq!(
        settings.directory,
        PathBuf::from(".local/share/benterm/worktrees")
    );
    assert_eq!(
        settings.root(Some(Path::new("/Users/me"))),
        Some(PathBuf::from("/Users/me/.local/share/benterm/worktrees"))
    );
    // Without a home directory there is nowhere to put them.
    assert_eq!(settings.root(None), None);
    assert_eq!(settings.root(Some(Path::new("relative"))), None);
}

#[test]
fn the_section_sets_the_directory() {
    let root = |text: &str| {
        let loaded = parse(text).unwrap();
        assert_eq!(loaded.warnings, vec![]);
        loaded.config.worktrees.root(Some(Path::new("/Users/me")))
    };
    assert_eq!(
        root("[worktrees]\ndirectory = \"/Volumes/work/trees\"\n"),
        Some(PathBuf::from("/Volumes/work/trees"))
    );
    assert_eq!(
        root("[worktrees]\ndirectory = \"~/trees\"\n"),
        Some(PathBuf::from("/Users/me/trees"))
    );
    assert_eq!(
        root("[worktrees]\ndirectory = \"work/trees\"\n"),
        Some(PathBuf::from("/Users/me/work/trees"))
    );
    // An absolute path needs no home directory.
    let loaded = parse("[worktrees]\ndirectory = \"/w\"\n").unwrap();
    assert_eq!(
        loaded.config.worktrees.root(None),
        Some(PathBuf::from("/w"))
    );
}

#[test]
fn bad_values_keep_the_default_and_are_reported() {
    let loaded = parse("[worktrees]\ndirectory = 3\nkeep = true\n").unwrap();
    assert_eq!(loaded.config.worktrees, WorktreeSettings::default());
    let mut warnings = loaded.warnings;
    warnings.sort_by(|a, b| a.key.cmp(&b.key));
    let warning = |key: &str, problem| Warning {
        key: key.into(),
        problem,
    };
    assert_eq!(
        warnings,
        vec![
            warning(
                "worktrees.directory",
                Problem::WrongType { expected: "a path" }
            ),
            warning("worktrees.keep", Problem::UnknownKey),
        ]
    );
}

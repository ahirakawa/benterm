use std::path::Path;
use std::process::Command;

use xtask::latency::GHOSTTY_APP;
use xtask::latency::setup::{
    GHOSTTY_COPY_ID, MARKER, Step, benterm_config, ghostty_binary, ghostty_config, ghostty_copy,
    plan_keys, split_plan, zshrc,
};

#[test]
fn benterm_reads_its_config_without_a_warning() {
    let loaded = benterm_config::parse(&benterm_config("Menlo", 13.0)).unwrap();
    assert_eq!(loaded.warnings, vec![]);
    let table: toml::Table = toml::from_str(&benterm_config("Sarasa Fixed J", 12.5)).unwrap();
    assert_eq!(
        table["terminal"]["font_family"].as_str(),
        Some("Sarasa Fixed J")
    );
    assert_eq!(table["terminal"]["font_size"].as_float(), Some(12.5));
    // Saving the scrollback is work Ghostty does not do.
    assert_eq!(table["session"]["scrollback_lines"].as_integer(), Some(0));
    assert_eq!(table["pty_log"]["enabled"].as_bool(), Some(false));
}

#[test]
fn ghostty_has_the_same_font_and_the_keys_benterm_splits_with() {
    let config = ghostty_config("Menlo", 13.0);
    for line in [
        "font-family = \"Menlo\"",
        "font-size = 13",
        "keybind = super+d=new_split:right",
        "keybind = super+shift+d=new_split:down",
        "keybind = super+alt+left=goto_split:left",
        "window-save-state = never",
        "shell-integration = none",
        "cursor-style-blink = false",
        "term = xterm-256color",
    ] {
        assert!(config.lines().any(|l| l == line), "{line:?} in\n{config}");
    }
}

#[test]
fn ghostty_takes_its_config_when_it_is_installed() {
    let binary = ghostty_binary(Path::new(GHOSTTY_APP));
    if !binary.exists() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("config");
    std::fs::write(&file, ghostty_config("Menlo", 13.0)).unwrap();
    let output = Command::new(binary)
        .arg("+validate-config")
        .arg(format!("--config-file={}", file.display()))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn the_prompt_starts_with_cells_of_the_marker_color() {
    let dir = tempfile::tempdir().unwrap();
    let rc = dir.path().join(".zshrc");
    std::fs::write(
        &rc,
        zshrc(
            Path::new("/tmp/x task/it's"),
            100,
            Path::new("/tmp/started"),
        ),
    )
    .unwrap();
    // zsh says what the prompt comes to once its escapes are expanded.
    let output = Command::new("/bin/zsh")
        .args([
            "-f",
            "-c",
            "source \"$1\" && print -rn -- \"${(%)PROMPT}\"",
            "zsh",
        ])
        .arg(&rc)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let prompt = String::from_utf8(output.stdout).unwrap();
    let [red, green, blue] = MARKER;
    assert_eq!(
        prompt,
        format!("\u{1b}[48;2;{red};{green};{blue}m    \u{1b}[0m ")
    );
}

#[test]
fn the_shell_starts_the_writer_with_one_letter() {
    let rc = zshrc(Path::new("/tmp/x task/it's"), 25, Path::new("/tmp/started"));
    // The paths are quoted for the shell: the function is told back with
    // the words it runs.
    let output = Command::new("/bin/zsh")
        .args([
            "-f",
            "-c",
            "source /dev/stdin <<<\"$1\" && print -rl -- ${(z)functions[f]}",
            "zsh",
        ])
        .arg(&rc)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let words = String::from_utf8(output.stdout).unwrap();
    assert_eq!(
        words.lines().collect::<Vec<_>>(),
        [
            "exec",
            "'/tmp/x task/it'\\''s'",
            "flood",
            "--rate",
            "25",
            "--started-dir",
            "/tmp/started"
        ]
    );
}

#[test]
fn ten_panes_are_two_columns_of_five_beside_the_pane_that_is_typed_in() {
    use Step::{FocusLeft as L, SplitDown as D, SplitRight as R};
    assert_eq!(split_plan(10), [R, R, D, D, D, D, L, D, D, D, D, L]);
    assert_eq!(plan_keys(&split_plan(10)), "R,R,D,D,D,D,L,D,D,D,D,L");
    assert_eq!(split_plan(1), [R, L]);
    assert_eq!(split_plan(2), [R, R, L, L]);
    assert_eq!(split_plan(3), [R, R, L, D, L]);
}

#[test]
fn a_plan_splits_once_for_every_pane_and_ends_in_the_first_pane() {
    for panes in 1..=16 {
        let plan = split_plan(panes);
        let splits = plan.iter().filter(|step| **step != Step::FocusLeft).count();
        assert_eq!(splits, panes as usize, "{plan:?}");
        // A column to the right is left with one step to the left each.
        let columns = plan.iter().filter(|s| **s == Step::SplitRight).count();
        let lefts = plan.iter().filter(|s| **s == Step::FocusLeft).count();
        assert_eq!(lefts, columns, "{plan:?}");
        assert_eq!(plan.last(), Some(&Step::FocusLeft));
    }
}

/// Ghostty keeps some settings per app and not per config file: whether
/// its windows come back, whether it looks for updates. A Ghostty started
/// for a run would write them over the user's. The run starts a copy
/// that macOS takes for another app.
#[test]
fn the_ghostty_that_is_measured_is_a_copy_with_a_name_of_its_own() {
    if !Path::new(GHOSTTY_APP).exists() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let copy = ghostty_copy(Path::new(GHOSTTY_APP), dir.path()).unwrap();
    assert_eq!(copy, dir.path().join("Ghostty.app"));
    let identifier = Command::new("plutil")
        .args(["-extract", "CFBundleIdentifier", "raw"])
        .arg(copy.join("Contents/Info.plist"))
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&identifier.stdout).trim(),
        GHOSTTY_COPY_ID
    );
    xtask::verify(&copy).unwrap();
    assert!(ghostty_binary(&copy).exists());
    // A second run replaces the copy.
    ghostty_copy(Path::new(GHOSTTY_APP), dir.path()).unwrap();
    xtask::verify(&copy).unwrap();
}

#[test]
fn a_ghostty_that_is_not_there_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    assert!(ghostty_copy(&dir.path().join("Nothing.app"), dir.path()).is_err());
}

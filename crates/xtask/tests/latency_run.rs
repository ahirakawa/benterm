use std::path::Path;

use xtask::latency::App;
use xtask::latency::run::{Probe, RunDirs, app_command, key_blocks, probe_arguments, start_order};

fn strings(list: &[&str]) -> Vec<String> {
    list.iter().map(|text| text.to_string()).collect()
}

fn pairs(list: &[(&str, &str)]) -> Vec<(String, String)> {
    list.iter()
        .map(|(name, value)| (name.to_string(), value.to_string()))
        .collect()
}

const BENTERM: &str = "/work/target/release/benterm";
const GHOSTTY: &str = "/Applications/Ghostty.app/Contents/MacOS/ghostty";

#[test]
fn benterm_starts_with_a_home_of_its_own_so_the_users_session_is_not_touched() {
    let dirs = RunDirs::in_dir(Path::new("/tmp/run-1"));
    let command = app_command(App::Benterm, &dirs, Path::new(BENTERM), Path::new(GHOSTTY));
    assert_eq!(command.command, strings(&[BENTERM]));
    assert_eq!(
        command.environment,
        pairs(&[
            ("SHELL", "/bin/zsh"),
            ("ZDOTDIR", "/tmp/run-1/zsh"),
            ("HOME", "/tmp/run-1/home"),
            ("XDG_CONFIG_HOME", "/tmp/run-1/home/.config"),
            ("XDG_STATE_HOME", "/tmp/run-1/home/state"),
        ])
    );
    assert_eq!(
        dirs.benterm_config,
        Path::new("/tmp/run-1/home/.config/benterm/config.toml")
    );
}

#[test]
fn ghostty_starts_with_the_same_shell_and_only_the_config_it_is_given() {
    let dirs = RunDirs::in_dir(Path::new("/tmp/run-1"));
    let command = app_command(App::Ghostty, &dirs, Path::new(BENTERM), Path::new(GHOSTTY));
    assert_eq!(
        command.command,
        strings(&[
            GHOSTTY,
            "--config-default-files=false",
            "--config-file=/tmp/run-1/ghostty.conf"
        ])
    );
    assert_eq!(
        command.environment,
        pairs(&[("SHELL", "/bin/zsh"), ("ZDOTDIR", "/tmp/run-1/zsh")])
    );
}

#[test]
fn the_probe_is_told_what_to_start_and_how_to_find_the_prompt() {
    let dirs = RunDirs::in_dir(Path::new("/tmp/run-1"));
    let command = app_command(App::Ghostty, &dirs, Path::new(BENTERM), Path::new(GHOSTTY));
    assert_eq!(
        probe_arguments(&Probe::Start, &command, &dirs),
        strings(&[
            "start",
            "--env",
            "SHELL=/bin/zsh",
            "--env",
            "ZDOTDIR=/tmp/run-1/zsh",
            "--marker",
            "ff00ff",
            "--marker-cells",
            "4",
            "--",
            GHOSTTY,
            "--config-default-files=false",
            "--config-file=/tmp/run-1/ghostty.conf",
        ])
    );
    let keys = probe_arguments(&Probe::Keys { count: 100 }, &command, &dirs);
    assert_eq!(keys[0], "keys");
    assert!(
        keys.windows(2)
            .any(|pair| pair == strings(&["--count", "100"]))
    );
    assert!(!keys.contains(&"--plan".to_string()));
}

#[test]
fn beside_busy_panes_the_probe_is_given_the_keys_that_open_them() {
    let dirs = RunDirs::in_dir(Path::new("/tmp/run-1"));
    let command = app_command(App::Benterm, &dirs, Path::new(BENTERM), Path::new(GHOSTTY));
    let probe = Probe::KeysUnderLoad {
        count: 100,
        panes: 10,
    };
    let arguments = probe_arguments(&probe, &command, &dirs);
    let has = |pair: &[&str]| arguments.windows(2).any(|found| found == strings(pair));
    assert!(has(&["--count", "100"]));
    assert!(has(&["--plan", "R,R,D,D,D,D,L,D,D,D,D,L"]));
    assert!(has(&["--started-dir", "/tmp/run-1/started"]));
    let command_at = arguments.iter().position(|a| a == "--").unwrap();
    assert_eq!(arguments[command_at + 1..], strings(&[BENTERM]));
}

#[test]
fn the_apps_take_turns_at_going_first() {
    let apps = [App::Benterm, App::Ghostty];
    assert_eq!(start_order(&apps, 0), [App::Benterm, App::Ghostty]);
    assert_eq!(start_order(&apps, 1), [App::Ghostty, App::Benterm]);
    assert_eq!(start_order(&apps, 2), [App::Benterm, App::Ghostty]);
    assert_eq!(start_order(&[App::Ghostty], 1), [App::Ghostty]);
}

#[test]
fn the_keys_of_each_app_are_two_blocks_with_the_other_app_between() {
    // First and last, and second and third: a machine that gets busier
    // or quieter during the run does so for both apps.
    let apps = [App::Benterm, App::Ghostty];
    assert_eq!(
        key_blocks(&apps, 100),
        [
            (App::Benterm, 50),
            (App::Ghostty, 50),
            (App::Ghostty, 50),
            (App::Benterm, 50)
        ]
    );
    assert_eq!(
        key_blocks(&apps, 5),
        [
            (App::Benterm, 3),
            (App::Ghostty, 3),
            (App::Ghostty, 2),
            (App::Benterm, 2)
        ]
    );
    assert_eq!(key_blocks(&[App::Benterm], 100), [(App::Benterm, 100)]);
    assert_eq!(key_blocks(&apps, 1), [(App::Benterm, 1), (App::Ghostty, 1)]);
}

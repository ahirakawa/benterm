use std::ffi::OsStr;
use std::path::Path;

use xtask::latency::redraws::RedrawOptions;
use xtask::latency::{App, LatencyOptions};
use xtask::{AD_HOC, Task, parse_args, target_dir};

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|arg| arg.to_string()).collect()
}

#[test]
fn bundle_signs_ad_hoc_by_default() {
    assert_eq!(
        parse_args(&args(&["bundle"])).unwrap(),
        Task::Bundle {
            identity: AD_HOC.to_string(),
            runtime_shaders: false,
            dmg: false,
        }
    );
}

#[test]
fn bundle_takes_a_signing_identity() {
    assert_eq!(
        parse_args(&args(&["bundle", "--sign", "Developer ID Application"])).unwrap(),
        Task::Bundle {
            identity: "Developer ID Application".to_string(),
            runtime_shaders: false,
            dmg: false,
        }
    );
}

#[test]
fn sign_without_an_identity_is_an_error() {
    assert!(parse_args(&args(&["bundle", "--sign"])).is_err());
}

#[test]
fn dmg_and_bundle_accept_runtime_shaders() {
    for (command, dmg) in [("dmg", true), ("bundle", false)] {
        assert_eq!(
            parse_args(&args(&[command, "--runtime-shaders"])).unwrap(),
            Task::Bundle {
                identity: AD_HOC.to_string(),
                runtime_shaders: true,
                dmg,
            }
        );
    }
}

#[test]
fn unknown_commands_and_options_are_errors() {
    assert!(parse_args(&args(&["unknown"])).is_err());
    assert!(parse_args(&args(&["bundle", "--debug"])).is_err());
}

#[test]
fn no_command_or_help_asks_for_the_usage() {
    assert_eq!(parse_args(&args(&[])).unwrap(), Task::Help);
    assert_eq!(parse_args(&args(&["--help"])).unwrap(), Task::Help);
}

#[test]
fn the_target_directory_follows_cargo_target_dir() {
    let workspace = Path::new("/work/benterm");
    assert_eq!(target_dir(workspace, None), workspace.join("target"));
    assert_eq!(
        target_dir(workspace, Some(OsStr::new("/elsewhere/out"))),
        Path::new("/elsewhere/out")
    );
    assert_eq!(
        target_dir(workspace, Some(OsStr::new("out"))),
        workspace.join("out")
    );
}

#[test]
fn flood_takes_a_rate_and_a_directory_to_say_it_started_in() {
    assert_eq!(
        parse_args(&args(&["flood"])).unwrap(),
        Task::Flood {
            rate: 100,
            pane: 0,
            started_dir: None
        }
    );
    assert_eq!(
        parse_args(&args(&[
            "flood",
            "--rate",
            "20",
            "--pane",
            "7",
            "--started-dir",
            "/tmp/started"
        ]))
        .unwrap(),
        Task::Flood {
            rate: 20,
            pane: 7,
            started_dir: Some(Path::new("/tmp/started").to_path_buf())
        }
    );
}

#[test]
fn flood_refuses_a_rate_it_cannot_keep() {
    assert!(parse_args(&args(&["flood", "--rate", "0"])).is_err());
    assert!(parse_args(&args(&["flood", "--rate", "100001"])).is_err());
    assert!(parse_args(&args(&["flood", "--rate", "fast"])).is_err());
    assert!(parse_args(&args(&["flood", "--rate"])).is_err());
}

#[test]
fn latency_measures_both_apps_a_hundred_keys_and_ten_starts_by_default() {
    let Task::Latency(options) = parse_args(&args(&["latency"])).unwrap() else {
        panic!("latency is a task");
    };
    assert_eq!(options, LatencyOptions::default());
    assert_eq!(options.keys, 100);
    assert_eq!(options.starts, 10);
    assert_eq!(options.panes, 10);
    assert_eq!(options.rate, 100);
    assert_eq!(options.apps, vec![App::Benterm, App::Ghostty]);
    assert_eq!(options.font, "Menlo");
    assert_eq!(options.ghostty, Path::new("/Applications/Ghostty.app"));
    assert_eq!(options.out, None);
    assert!(!options.check);
}

#[test]
fn latency_takes_its_counts_its_font_and_its_apps() {
    let Task::Latency(options) = parse_args(&args(&[
        "latency",
        "--keys",
        "200",
        "--starts",
        "12",
        "--rate",
        "50",
        "--font",
        "Sarasa Fixed J",
        "--font-size",
        "12.5",
        "--app",
        "ghostty",
        "--ghostty",
        "/opt/Ghostty.app",
        "--out",
        "results",
    ]))
    .unwrap() else {
        panic!("latency is a task");
    };
    assert_eq!(options.keys, 200);
    assert_eq!(options.starts, 12);
    assert_eq!(options.rate, 50);
    assert_eq!(options.font, "Sarasa Fixed J");
    assert_eq!(options.font_size, 12.5);
    assert_eq!(options.apps, vec![App::Ghostty]);
    assert_eq!(options.ghostty, Path::new("/opt/Ghostty.app"));
    assert_eq!(options.out, Some(Path::new("results").to_path_buf()));
}

#[test]
fn latency_smoke_is_a_short_run_and_check_only_asks_for_the_permissions() {
    let Task::Latency(smoke) = parse_args(&args(&["latency", "--smoke"])).unwrap() else {
        panic!("latency is a task");
    };
    assert_eq!((smoke.keys, smoke.starts), (5, 2));
    let Task::Latency(check) = parse_args(&args(&["latency", "--check"])).unwrap() else {
        panic!("latency is a task");
    };
    assert!(check.check);
}

#[test]
fn latency_refuses_counts_of_nothing_and_unknown_apps() {
    assert!(parse_args(&args(&["latency", "--keys", "0"])).is_err());
    assert!(parse_args(&args(&["latency", "--starts", "0"])).is_err());
    assert!(parse_args(&args(&["latency", "--app", "kitty"])).is_err());
    assert!(parse_args(&args(&["latency", "--font-size", "big"])).is_err());
    assert!(parse_args(&args(&["latency", "--fast"])).is_err());
}

#[test]
fn redraws_counts_ten_writers_for_thirty_seconds_by_default() {
    let Task::Redraws(options) = parse_args(&args(&["redraws"])).unwrap() else {
        panic!("redraws is a task");
    };
    assert_eq!(
        options,
        RedrawOptions {
            seconds: 30,
            panes: 10,
            rate: 100,
            out: None,
        }
    );
}

#[test]
fn redraws_takes_its_time_its_writers_and_where_the_results_go() {
    let Task::Redraws(options) = parse_args(&args(&[
        "redraws",
        "--seconds",
        "60",
        "--panes",
        "4",
        "--rate",
        "1000",
        "--out",
        "/tmp/results",
    ]))
    .unwrap() else {
        panic!("redraws is a task");
    };
    assert_eq!(
        options,
        RedrawOptions {
            seconds: 60,
            panes: 4,
            rate: 1000,
            out: Some("/tmp/results".into()),
        }
    );
}

#[test]
fn redraws_smoke_is_a_short_run() {
    let Task::Redraws(options) = parse_args(&args(&["redraws", "--smoke"])).unwrap() else {
        panic!("redraws is a task");
    };
    assert_eq!(options.seconds, 3);
}

#[test]
fn redraws_refuses_counts_of_nothing_and_unknown_options() {
    assert!(parse_args(&args(&["redraws", "--seconds", "0"])).is_err());
    assert!(parse_args(&args(&["redraws", "--panes", "0"])).is_err());
    assert!(parse_args(&args(&["redraws", "--rate", "0"])).is_err());
    assert!(parse_args(&args(&["redraws", "--rate", "1000000"])).is_err());
    assert!(parse_args(&args(&["redraws", "--seconds"])).is_err());
    assert!(parse_args(&args(&["redraws", "--keys", "5"])).is_err());
}

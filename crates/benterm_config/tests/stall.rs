//! The `[stall]` section: how long an agent at work may go without a
//! change on its screen or in the files of its directory before it is
//! marked as stalled.

use std::time::Duration;

use benterm_config::{Config, Loaded, Problem, STALL_SECONDS_RANGE, StallSettings, parse};

fn ok(text: &str) -> Loaded {
    parse(text).expect("text should be TOML")
}

fn problems(loaded: &Loaded) -> Vec<(&str, &Problem)> {
    let warnings = loaded.warnings.iter();
    warnings.map(|w| (w.key.as_str(), &w.problem)).collect()
}

#[test]
fn ten_minutes_without_a_change_mark_an_agent_by_default() {
    let settings = Config::default().stall;
    assert_eq!(settings.after_seconds, 600);
    assert_eq!(settings.threshold(), Duration::from_secs(600));
    assert_eq!(settings, StallSettings::default());
    assert_eq!(ok("").config.stall, StallSettings::default());
}

#[test]
fn the_section_sets_the_time() {
    let loaded = ok("[stall]\nafter_seconds = 90\n");
    assert_eq!(loaded.config.stall.threshold(), Duration::from_secs(90));
    assert_eq!(loaded.warnings, vec![]);
}

#[test]
fn a_second_and_a_day_are_the_ends_of_the_range() {
    assert_eq!(STALL_SECONDS_RANGE, (1, 86_400));
    for seconds in [1, 86_400] {
        let loaded = ok(&format!("[stall]\nafter_seconds = {seconds}\n"));
        assert_eq!(loaded.config.stall.after_seconds, seconds);
        assert_eq!(loaded.warnings, vec![]);
    }
}

#[test]
fn a_time_out_of_range_is_replaced_by_the_default_with_a_warning() {
    for seconds in ["0", "-5", "86401"] {
        let loaded = ok(&format!("[stall]\nafter_seconds = {seconds}\n"));
        assert_eq!(loaded.config.stall, StallSettings::default(), "{seconds}");
        let range = Problem::OutOfRange {
            min: 1.0,
            max: 86_400.0,
        };
        assert_eq!(problems(&loaded), [("stall.after_seconds", &range)]);
    }
}

#[test]
fn an_unknown_key_is_warned_about() {
    let loaded = ok("[stall]\nafter_minutes = 5\n");
    assert_eq!(loaded.config.stall, StallSettings::default());
    assert_eq!(
        problems(&loaded),
        [("stall.after_minutes", &Problem::UnknownKey)]
    );
}

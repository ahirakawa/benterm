//! The `[report]` section: when a turn of an agent needs a report, and
//! the limits its report is held to.

use benterm_config::{Config, Loaded, Problem, ReportSettings, parse};

fn ok(text: &str) -> Loaded {
    parse(text).expect("text should be TOML")
}

fn problems(loaded: &Loaded) -> Vec<(&str, &Problem)> {
    let warnings = loaded.warnings.iter();
    warnings.map(|w| (w.key.as_str(), &w.problem)).collect()
}

#[test]
fn the_check_is_off_by_default_and_has_its_thresholds_and_limits() {
    let settings = Config::default().report;
    assert_eq!(
        settings,
        ReportSettings {
            enabled: false,
            edited_files: 3,
            tool_calls: 20,
            max_rounds: 2,
            summary_max_chars: 200,
            question_max_chars: 40,
            reply_max_chars: 400,
        }
    );
    assert_eq!(ok("").config.report, settings);
}

#[test]
fn the_section_sets_the_thresholds_and_the_limits() {
    let loaded = ok(
        "[report]\nenabled = true\nedited_files = 5\ntool_calls = 40\n\
         max_rounds = 1\nsummary_max_chars = 400\nquestion_max_chars = 80\n\
         reply_max_chars = 800\n",
    );
    assert_eq!(loaded.warnings, vec![]);
    assert_eq!(
        loaded.config.report,
        ReportSettings {
            enabled: true,
            edited_files: 5,
            tool_calls: 40,
            max_rounds: 1,
            summary_max_chars: 400,
            question_max_chars: 80,
            reply_max_chars: 800,
        }
    );
}

/// No file can send an agent back without end: five rounds are the
/// most, and none is allowed, which only teaches the form.
#[test]
fn the_rounds_go_from_none_to_five() {
    for rounds in [0, 5] {
        let loaded = ok(&format!("[report]\nmax_rounds = {rounds}\n"));
        assert_eq!(loaded.config.report.max_rounds, rounds);
        assert_eq!(loaded.warnings, vec![]);
    }
    let loaded = ok("[report]\nmax_rounds = 6\n");
    assert_eq!(loaded.config.report, ReportSettings::default());
    let range = Problem::OutOfRange { min: 0.0, max: 5.0 };
    assert_eq!(problems(&loaded), [("report.max_rounds", &range)]);
}

#[test]
fn a_threshold_of_nothing_is_replaced_by_the_default_with_a_warning() {
    for key in [
        "edited_files",
        "tool_calls",
        "summary_max_chars",
        "question_max_chars",
        "reply_max_chars",
    ] {
        let loaded = ok(&format!("[report]\n{key} = 0\n"));
        assert_eq!(loaded.config.report, ReportSettings::default(), "{key}");
        let warned: Vec<&str> = loaded.warnings.iter().map(|w| w.key.as_str()).collect();
        assert_eq!(warned, [format!("report.{key}")]);
    }
}

#[test]
fn a_wrong_type_and_an_unknown_key_are_warned_about() {
    let loaded = ok("[report]\nenabled = \"yes\"\nlanguage = \"ja\"\n");
    assert_eq!(loaded.config.report, ReportSettings::default());
    let expected = Problem::WrongType {
        expected: "true or false",
    };
    assert_eq!(
        problems(&loaded),
        [
            ("report.language", &Problem::UnknownKey),
            ("report.enabled", &expected)
        ]
    );
}

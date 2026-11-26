//! The `[session]` section: how much of what a pane showed is kept for
//! the next start.

use benterm_config::{Config, Loaded, Problem, SCROLLBACK_LINES_RANGE, SessionSettings, parse};

fn ok(text: &str) -> Loaded {
    parse(text).expect("text should be TOML")
}

fn problems(loaded: &Loaded) -> Vec<(&str, &Problem)> {
    let warnings = loaded.warnings.iter();
    warnings.map(|w| (w.key.as_str(), &w.problem)).collect()
}

#[test]
fn two_thousand_rows_are_kept_by_default() {
    assert_eq!(Config::default().session.scrollback_lines, 2000);
    assert_eq!(Config::default().session, SessionSettings::default());
    assert_eq!(ok("").config.session, SessionSettings::default());
}

#[test]
fn the_section_sets_how_many_rows_are_kept() {
    let loaded = ok("[session]\nscrollback_lines = 500\n");
    assert_eq!(loaded.config.session.scrollback_lines, 500);
    assert_eq!(loaded.warnings, vec![]);
}

#[test]
fn none_and_as_many_as_a_pane_holds_are_the_ends_of_the_range() {
    assert_eq!(SCROLLBACK_LINES_RANGE, (0, 10_000));
    for lines in [0, 10_000] {
        let loaded = ok(&format!("[session]\nscrollback_lines = {lines}\n"));
        assert_eq!(loaded.config.session.scrollback_lines, lines);
        assert_eq!(loaded.warnings, vec![]);
    }
}

#[test]
fn a_number_out_of_range_is_replaced_by_the_default_with_a_warning() {
    for lines in ["-1", "10001", "99999999999"] {
        let loaded = ok(&format!("[session]\nscrollback_lines = {lines}\n"));
        assert_eq!(loaded.config.session, SessionSettings::default(), "{lines}");
        let range = Problem::OutOfRange {
            min: 0.0,
            max: 10_000.0,
        };
        assert_eq!(problems(&loaded), [("session.scrollback_lines", &range)]);
    }
}

#[test]
fn what_is_not_a_whole_number_is_replaced_by_the_default_with_a_warning() {
    for lines in ["1.5", "\"many\"", "true"] {
        let loaded = ok(&format!(
            "[session]\nscrollback_lines = {lines}\nrows = 1\n"
        ));
        assert_eq!(loaded.config.session, SessionSettings::default(), "{lines}");
        let wrong = Problem::WrongType {
            expected: "a whole number",
        };
        assert_eq!(
            problems(&loaded),
            [
                ("session.rows", &Problem::UnknownKey),
                ("session.scrollback_lines", &wrong)
            ]
        );
    }
}

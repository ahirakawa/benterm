use xtask::latency::App;
use xtask::latency::report::{Measurement, Scenario, csv, parse_probe, table};

#[test]
fn the_probe_tells_samples_misses_and_what_it_saw() {
    let output = parse_probe(
        "info refresh_hz=50\nsample 20.125\nmiss\nsample 39.5\ninfo cpu_busy_percent=12.5\n",
    )
    .unwrap();
    assert_eq!(output.samples, [20.125, 39.5]);
    assert_eq!(output.misses, 1);
    assert_eq!(output.info["refresh_hz"], "50");
    assert_eq!(output.info["cpu_busy_percent"], "12.5");
}

#[test]
fn a_line_the_probe_should_not_write_is_an_error() {
    assert!(parse_probe("sample fast\n").is_err());
    assert!(parse_probe("sample -3\n").is_err());
    assert!(parse_probe("info no-equals\n").is_err());
    assert!(parse_probe("frame 1 2 3\n").is_err());
    assert_eq!(parse_probe("").unwrap().samples, Vec::<f64>::new());
}

fn measurement(app: App, scenario: Scenario, samples: &[f64], misses: u32) -> Measurement {
    Measurement {
        app,
        scenario,
        samples: samples.to_vec(),
        misses,
        info: Default::default(),
    }
}

#[test]
fn the_table_has_a_row_for_each_run_with_its_median_and_95th_percentile() {
    let hundred: Vec<f64> = (1..=100).map(f64::from).collect();
    let rows = [
        measurement(App::Benterm, Scenario::Start, &[300.0, 310.0, 320.0], 0),
        measurement(App::Ghostty, Scenario::Start, &[200.0, 250.0], 0),
        measurement(App::Benterm, Scenario::Keys, &hundred, 2),
        measurement(App::Ghostty, Scenario::KeysUnderLoad, &[], 5),
    ];
    assert_eq!(
        table(&rows),
        "\
| Measured | App | Samples | Median ms | p95 ms | Mean ms | Min ms | Max ms | Missed |
|---|---|---|---|---|---|---|---|---|
| Start to prompt | benterm | 3 | 310.0 | 320.0 | 310.0 | 300.0 | 320.0 | 0 |
| Start to prompt | ghostty | 2 | 225.0 | 250.0 | 225.0 | 200.0 | 250.0 | 0 |
| Key to screen, one pane | benterm | 100 | 50.5 | 95.0 | 50.5 | 1.0 | 100.0 | 2 |
| Key to screen, beside busy panes | ghostty | 0 | - | - | - | - | - | 5 |
"
    );
}

#[test]
fn the_samples_are_kept_one_on_a_line() {
    let rows = [
        measurement(App::Benterm, Scenario::Keys, &[20.5, 40.0], 0),
        measurement(App::Ghostty, Scenario::Start, &[200.0], 0),
    ];
    assert_eq!(
        csv(&rows),
        "app,measured,sample,ms\nbenterm,keys,1,20.500\nbenterm,keys,2,40.000\nghostty,start,1,200.000\n"
    );
}

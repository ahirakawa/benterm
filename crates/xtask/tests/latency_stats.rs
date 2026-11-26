use xtask::latency::stats::{Summary, median, percentile, summarize};

#[test]
fn the_median_of_an_odd_count_is_the_middle_value() {
    assert_eq!(median(&[30.0, 10.0, 20.0]), Some(20.0));
}

#[test]
fn the_median_of_an_even_count_is_the_mean_of_the_two_middle_values() {
    assert_eq!(median(&[40.0, 10.0, 20.0, 30.0]), Some(25.0));
}

#[test]
fn nothing_has_no_median_and_no_percentile() {
    assert_eq!(median(&[]), None);
    assert_eq!(percentile(&[], 95.0), None);
    assert_eq!(summarize(&[]), None);
}

#[test]
fn a_percentile_is_the_value_at_the_nearest_rank() {
    // 1, 2, ... 100: the 95th of 100 values is the 95th smallest.
    let values: Vec<f64> = (1..=100).rev().map(f64::from).collect();
    assert_eq!(percentile(&values, 95.0), Some(95.0));
    assert_eq!(percentile(&values, 100.0), Some(100.0));
    assert_eq!(percentile(&values, 1.0), Some(1.0));
    // ceil(0.95 * 10) = 10, and ceil(0.95 * 20) = 19.
    let ten: Vec<f64> = (1..=10).map(f64::from).collect();
    assert_eq!(percentile(&ten, 95.0), Some(10.0));
    let twenty: Vec<f64> = (1..=20).map(f64::from).collect();
    assert_eq!(percentile(&twenty, 95.0), Some(19.0));
    assert_eq!(percentile(&[7.0], 95.0), Some(7.0));
}

#[test]
fn a_summary_has_the_count_the_median_the_95th_percentile_and_the_mean() {
    let values: Vec<f64> = (1..=100).map(f64::from).collect();
    assert_eq!(
        summarize(&values),
        Some(Summary {
            count: 100,
            median: 50.5,
            p95: 95.0,
            mean: 50.5,
            min: 1.0,
            max: 100.0,
        })
    );
}

#[test]
fn a_summary_does_not_depend_on_the_order_of_the_samples() {
    let sorted = [16.6, 16.7, 33.3, 33.4, 50.0];
    let shuffled = [33.4, 16.6, 50.0, 33.3, 16.7];
    assert_eq!(summarize(&sorted), summarize(&shuffled));
    assert_eq!(summarize(&sorted).unwrap().median, 33.3);
}

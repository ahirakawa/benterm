//! The numbers a run is told by: the median, the 95th percentile and
//! the mean of its samples.

/// What a set of samples comes to.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Summary {
    pub count: usize,
    pub median: f64,
    pub p95: f64,
    pub mean: f64,
    pub min: f64,
    pub max: f64,
}

/// The samples, smallest first.
fn sorted(samples: &[f64]) -> Vec<f64> {
    let mut sorted = samples.to_vec();
    sorted.sort_by(f64::total_cmp);
    sorted
}

/// The middle sample, or the mean of the two in the middle of an even
/// count. `None` without samples.
pub fn median(samples: &[f64]) -> Option<f64> {
    let sorted = sorted(samples);
    let middle = sorted.len() / 2;
    match sorted.len() {
        0 => None,
        count if count % 2 == 1 => Some(sorted[middle]),
        _ => Some((sorted[middle - 1] + sorted[middle]) / 2.0),
    }
}

/// The sample at the nearest rank: the smallest sample that at least
/// `percent` of the samples do not exceed. It is always one of the
/// samples, so a slow one is not averaged away. `None` without samples.
pub fn percentile(samples: &[f64], percent: f64) -> Option<f64> {
    let sorted = sorted(samples);
    if sorted.is_empty() {
        return None;
    }
    let rank = (percent / 100.0 * sorted.len() as f64).ceil() as usize;
    Some(sorted[rank.clamp(1, sorted.len()) - 1])
}

/// The summary of the samples. `None` without samples.
pub fn summarize(samples: &[f64]) -> Option<Summary> {
    let sorted = sorted(samples);
    Some(Summary {
        count: sorted.len(),
        median: median(&sorted)?,
        p95: percentile(&sorted, 95.0)?,
        mean: sorted.iter().sum::<f64>() / sorted.len() as f64,
        min: *sorted.first()?,
        max: *sorted.last()?,
    })
}

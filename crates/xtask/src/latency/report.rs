//! What the probe wrote, and the table and the file of samples made
//! from it.

use std::collections::BTreeMap;
use std::fmt::Write;

use anyhow::{Context, Result, bail};

use super::App;
use super::stats::summarize;

/// What was timed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Scenario {
    /// From starting the app until the prompt of its shell shows.
    Start,
    /// From pressing a key until the screen changes where it is typed.
    Keys,
    /// The same while other panes of the window write lines.
    KeysUnderLoad,
}

impl Scenario {
    /// The name in the table.
    pub fn title(self) -> &'static str {
        match self {
            Scenario::Start => "Start to prompt",
            Scenario::Keys => "Key to screen, one pane",
            Scenario::KeysUnderLoad => "Key to screen, beside busy panes",
        }
    }

    /// The name in the file of samples.
    pub fn name(self) -> &'static str {
        match self {
            Scenario::Start => "start",
            Scenario::Keys => "keys",
            Scenario::KeysUnderLoad => "keys-under-load",
        }
    }
}

/// What one run of the probe wrote.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProbeOutput {
    /// Milliseconds, in the order they were measured.
    pub samples: Vec<f64>,
    /// Keys whose change did not show in time.
    pub misses: u32,
    /// What the probe saw besides: the refresh rate, the load.
    pub info: BTreeMap<String, String>,
}

/// Reads the lines of the probe: `sample MILLISECONDS`, `miss` and
/// `info KEY=VALUE`.
pub fn parse_probe(text: &str) -> Result<ProbeOutput> {
    let mut output = ProbeOutput::default();
    for line in text.lines() {
        let (kind, rest) = line.split_once(' ').unwrap_or((line, ""));
        match kind {
            "sample" => {
                let sample: f64 = rest
                    .parse()
                    .ok()
                    .filter(|sample: &f64| sample.is_finite() && *sample >= 0.0)
                    .with_context(|| format!("not a time: {line:?}"))?;
                output.samples.push(sample);
            }
            "miss" if rest.is_empty() => output.misses += 1,
            "info" => {
                let (key, value) = rest
                    .split_once('=')
                    .with_context(|| format!("not KEY=VALUE: {line:?}"))?;
                output.info.insert(key.to_string(), value.to_string());
            }
            _ => bail!("the probe wrote a line that is not understood: {line:?}"),
        }
    }
    Ok(output)
}

/// The samples of one scenario in one app.
#[derive(Clone, Debug, PartialEq)]
pub struct Measurement {
    pub app: App,
    pub scenario: Scenario,
    pub samples: Vec<f64>,
    pub misses: u32,
    pub info: BTreeMap<String, String>,
}

/// A Markdown table with a row for each measurement, in the order given.
pub fn table(measurements: &[Measurement]) -> String {
    let mut table = String::from(
        "| Measured | App | Samples | Median ms | p95 ms | Mean ms | Min ms | Max ms | Missed |\n\
         |---|---|---|---|---|---|---|---|---|\n",
    );
    for measurement in measurements {
        let numbers = match summarize(&measurement.samples) {
            Some(s) => format!(
                "{:.1} | {:.1} | {:.1} | {:.1} | {:.1}",
                s.median, s.p95, s.mean, s.min, s.max
            ),
            None => "- | - | - | - | -".to_string(),
        };
        writeln!(
            table,
            "| {} | {} | {} | {numbers} | {} |",
            measurement.scenario.title(),
            measurement.app.name(),
            measurement.samples.len(),
            measurement.misses,
        )
        .expect("writing to a string does not fail");
    }
    table
}

/// Every sample on a line of its own, for whoever wants to look closer.
pub fn csv(measurements: &[Measurement]) -> String {
    let mut csv = String::from("app,measured,sample,ms\n");
    for measurement in measurements {
        for (index, sample) in measurement.samples.iter().enumerate() {
            writeln!(
                csv,
                "{},{},{},{sample:.3}",
                measurement.app.name(),
                measurement.scenario.name(),
                index + 1,
            )
            .expect("writing to a string does not fail");
        }
    }
    csv
}

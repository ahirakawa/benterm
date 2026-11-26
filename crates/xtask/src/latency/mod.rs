//! Measures Benterm against Ghostty from outside both apps: how long a key
//! takes to show on the screen, and how long the app takes to show the
//! prompt of its shell. `cargo xtask latency` runs it.

pub mod flood;
pub mod probe;
pub mod redraws;
pub mod report;
pub mod run;
pub mod setup;
pub mod stats;

use std::path::PathBuf;

/// A terminal that is measured.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum App {
    Benterm,
    Ghostty,
}

impl App {
    /// The name on the command line and in the results.
    pub fn name(self) -> &'static str {
        match self {
            App::Benterm => "benterm",
            App::Ghostty => "ghostty",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        [App::Benterm, App::Ghostty]
            .into_iter()
            .find(|app| app.name() == name)
    }
}

/// Where Ghostty is installed unless `--ghostty` names another place.
pub const GHOSTTY_APP: &str = "/Applications/Ghostty.app";

/// What `cargo xtask latency` was asked to measure.
#[derive(Clone, Debug, PartialEq)]
pub struct LatencyOptions {
    /// The keys pressed in each run of key presses.
    pub keys: u32,
    /// The starts of each app that are timed.
    pub starts: u32,
    /// The panes that write lines while keys are pressed in one more.
    pub panes: u32,
    /// The lines a second each of those panes writes.
    pub rate: u32,
    /// The font both apps draw with.
    pub font: String,
    /// Its size in points.
    pub font_size: f64,
    pub apps: Vec<App>,
    /// The installed Ghostty.app. A copy of it is what runs.
    pub ghostty: PathBuf,
    /// Where the results go, instead of a new directory under the target
    /// directory.
    pub out: Option<PathBuf>,
    /// Only tell whether the permissions are there, and ask for them.
    pub check: bool,
}

impl Default for LatencyOptions {
    fn default() -> Self {
        Self {
            keys: 100,
            starts: 10,
            panes: 10,
            rate: 100,
            font: "Menlo".to_string(),
            font_size: 13.0,
            apps: vec![App::Benterm, App::Ghostty],
            ghostty: PathBuf::from(GHOSTTY_APP),
            out: None,
            check: false,
        }
    }
}

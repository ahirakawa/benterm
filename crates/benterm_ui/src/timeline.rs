//! The timeline of the progress board: one lane per pane that had an
//! agent, with a band for each state the agent was in, placed by time.
//!
//! The lanes are made from the records of the agents' history (see
//! [`benterm_session::history`]). A state lasts from its record until
//! the next record of the same pane, or until now. A record without an
//! agent, or with a state nothing showed, ends the band before it and
//! starts none.

use std::collections::HashMap;
use std::time::Duration;

use benterm_core::Rgb;
use benterm_session::Agent;
use benterm_session::history::{Record, State};

use crate::theme::BoardTheme;

/// The four states the board shows, each in a color of its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BandState {
    Working,
    /// For permission or for an answer alike.
    Waiting,
    Done,
    Error,
}

impl BandState {
    /// In the order of the legend.
    pub const ALL: [BandState; 4] = [Self::Working, Self::Waiting, Self::Done, Self::Error];

    /// The band a recorded state is shown as, if any.
    pub fn of(state: State) -> Option<Self> {
        match state {
            State::Working => Some(Self::Working),
            State::Waiting(_) => Some(Self::Waiting),
            State::Done => Some(Self::Done),
            State::Error => Some(Self::Error),
            State::Unknown => None,
        }
    }

    pub fn color(self, theme: &BoardTheme) -> Rgb {
        match self {
            Self::Working => theme.working,
            Self::Waiting => theme.waiting,
            Self::Done => theme.done,
            Self::Error => theme.error,
        }
    }

    /// The name in the legend.
    pub fn label(self) -> &'static str {
        match self {
            Self::Working => "Working",
            Self::Waiting => "Waiting",
            Self::Done => "Done",
            Self::Error => "Error",
        }
    }
}

/// A time in one state, in milliseconds since 1970.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Band {
    pub start_ms: u64,
    pub end_ms: u64,
    pub state: BandState,
}

/// What the agents of one pane did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lane {
    /// The key of the pane in the history, which lasts across restarts.
    pub key: String,
    /// The id the pane had in its last record: that of this run if the
    /// pane is still open.
    pub pane: u64,
    /// The last agent the pane had; `None` too when a program that is
    /// no agent was the last to have a state there.
    pub agent: Option<Agent>,
    /// Oldest first, none overlapping.
    pub bands: Vec<Band>,
}

impl Lane {
    /// When the lane's last band started, if it has one.
    pub fn last_change_ms(&self) -> Option<u64> {
        self.bands.last().map(|band| band.start_ms)
    }
}

/// The lanes of `records`, which are oldest first, in the order their
/// panes first come up. A band still going ends at `now_ms`.
pub fn lanes(records: &[Record], now_ms: u64) -> Vec<Lane> {
    let mut lanes: Vec<Lane> = Vec::new();
    // The lane of each key, and whether its last band is still going.
    let mut index: HashMap<&str, (usize, bool)> = HashMap::new();
    for record in records {
        let (position, open) = *index.entry(&record.key).or_insert_with(|| {
            lanes.push(Lane {
                key: record.key.clone(),
                pane: record.pane,
                agent: None,
                bands: Vec::new(),
            });
            (lanes.len() - 1, false)
        });
        let lane = &mut lanes[position];
        lane.pane = record.pane;
        // A program that is no agent has a state when it tells one
        // itself (OSC 7501), and the lane is then no agent's. A record
        // without a state ends what was in the pane and names nothing.
        let state = BandState::of(record.state);
        if record.agent.is_some() || state.is_some() {
            lane.agent = record.agent;
        }
        if open {
            let last = lane.bands.last_mut().expect("an open lane has a band");
            if Some(last.state) == state {
                // Nothing the board shows changed.
                continue;
            }
            last.end_ms = record.at_ms;
        }
        if let Some(state) = state {
            lane.bands.push(Band {
                start_ms: record.at_ms,
                end_ms: record.at_ms,
                state,
            });
        }
        index.insert(&record.key, (position, state.is_some()));
    }
    for (position, open) in index.into_values() {
        if open && let Some(last) = lanes[position].bands.last_mut() {
            last.end_ms = now_ms.max(last.start_ms);
        }
    }
    lanes
}

/// The stretch of `lane` its agent has stalled in, given the last change
/// seen of it at `changed_ms`: the end of its band of work, from that
/// change, or from the start of the band if the change is older, until
/// `now_ms`. `None` when the lane's last band is no work going on now.
pub fn stalled_stretch(lane: &Lane, changed_ms: u64, now_ms: u64) -> Option<Band> {
    let last = lane.bands.last()?;
    if last.state != BandState::Working || last.end_ms < now_ms {
        return None;
    }
    Some(Band {
        start_ms: changed_ms.clamp(last.start_ms, now_ms),
        end_ms: now_ms,
        state: BandState::Working,
    })
}

/// The lanes with a band in the timeline that ends at `now_ms` and is
/// `span` long.
pub fn visible_lanes(
    lanes: &[Lane],
    now_ms: u64,
    span: Duration,
) -> impl Iterator<Item = &Lane> + '_ {
    let start = start_of(now_ms, span);
    lanes
        .iter()
        .filter(move |lane| lane.bands.iter().any(|band| band.end_ms > start))
}

/// Where the timeline that ends at `now_ms` and is `span` long starts.
fn start_of(now_ms: u64, span: Duration) -> u64 {
    now_ms.saturating_sub(span.as_millis() as u64)
}

/// The left and right of `band` in a timeline `width` wide that ends at
/// `now_ms` and is `span` long. `None` if no part of the band is in it.
pub fn band_x(band: &Band, now_ms: u64, span: Duration, width: f32) -> Option<(f32, f32)> {
    let start = start_of(now_ms, span);
    if band.end_ms <= start || band.start_ms >= now_ms {
        return None;
    }
    let x = |at: u64| time_x(at, now_ms, span, width);
    Some((x(band.start_ms.max(start)), x(band.end_ms.min(now_ms))))
}

/// The place of the time `at_ms` in a timeline `width` wide that ends at
/// `now_ms` and is `span` long, from its left edge.
pub fn time_x(at_ms: u64, now_ms: u64, span: Duration, width: f32) -> f32 {
    let start = start_of(now_ms, span);
    let length = span.as_millis().max(1) as f64;
    let share = (at_ms.clamp(start, now_ms) - start) as f64 / length;
    (share * f64::from(width)) as f32
}

/// How far back the timeline goes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TimeSpan {
    FifteenMinutes,
    #[default]
    Hour,
    SixHours,
    Day,
}

impl TimeSpan {
    /// From the narrowest to the widest.
    pub const ALL: [TimeSpan; 4] = [Self::FifteenMinutes, Self::Hour, Self::SixHours, Self::Day];

    pub fn length(self) -> Duration {
        let minutes = match self {
            Self::FifteenMinutes => 15,
            Self::Hour => 60,
            Self::SixHours => 6 * 60,
            Self::Day => 24 * 60,
        };
        Duration::from_secs(minutes * 60)
    }

    /// The time between two lines of the timeline.
    pub fn tick(self) -> Duration {
        let minutes = match self {
            Self::FifteenMinutes => 5,
            Self::Hour => 15,
            Self::SixHours => 60,
            Self::Day => 4 * 60,
        };
        Duration::from_secs(minutes * 60)
    }

    /// The name on the button that chooses the span.
    pub fn label(self) -> &'static str {
        match self {
            Self::FifteenMinutes => "15m",
            Self::Hour => "1h",
            Self::SixHours => "6h",
            Self::Day => "24h",
        }
    }

    /// The next wider span, or this one if it is the widest.
    pub fn wider(self) -> Self {
        let position = Self::ALL.iter().position(|span| *span == self).unwrap_or(0);
        Self::ALL[(position + 1).min(Self::ALL.len() - 1)]
    }

    /// The next narrower span, or this one if it is the narrowest.
    pub fn narrower(self) -> Self {
        let position = Self::ALL.iter().position(|span| *span == self).unwrap_or(0);
        Self::ALL[position.saturating_sub(1)]
    }
}

/// How long before now a line of the timeline is, as written under it:
/// "now", "-15m", "-2h".
pub fn tick_label(before: Duration) -> String {
    let minutes = before.as_secs() / 60;
    match minutes {
        0 => "now".into(),
        m if m % 60 == 0 => format!("-{}h", m / 60),
        m => format!("-{m}m"),
    }
}

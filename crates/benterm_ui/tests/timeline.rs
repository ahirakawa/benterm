//! Where the progress board puts the bands of the agents' states.

use std::time::Duration;

use benterm_core::Rgb;
use benterm_session::Agent;
use benterm_session::history::{Record, State, Waiting};
use benterm_ui::theme::Theme;
use benterm_ui::timeline::{
    Band, BandState, TimeSpan, band_x, lanes, stalled_stretch, visible_lanes,
};

const MINUTE: u64 = 60_000;

fn record(at_ms: u64, key: &str, pane: u64, agent: Option<Agent>, state: State) -> Record {
    Record {
        at_ms,
        key: key.into(),
        pane,
        agent,
        state,
    }
}

#[test]
fn a_band_of_the_last_hour_is_placed_by_its_share_of_the_hour() {
    let now = 1_000 * MINUTE;
    let band = Band {
        start_ms: now - 30 * MINUTE,
        end_ms: now - 10 * MINUTE,
        state: BandState::Working,
    };
    let hour = Duration::from_secs(3600);
    assert_eq!(band_x(&band, now, hour, 600.), Some((300., 500.)));
}

#[test]
fn a_band_that_began_before_the_timeline_starts_at_its_left_edge() {
    let now = 1_000 * MINUTE;
    let band = Band {
        start_ms: now - 90 * MINUTE,
        end_ms: now - 30 * MINUTE,
        state: BandState::Done,
    };
    let hour = Duration::from_secs(3600);
    assert_eq!(band_x(&band, now, hour, 600.), Some((0., 300.)));
}

#[test]
fn a_band_that_ended_before_the_timeline_is_not_placed() {
    let now = 1_000 * MINUTE;
    let band = Band {
        start_ms: now - 90 * MINUTE,
        end_ms: now - 61 * MINUTE,
        state: BandState::Done,
    };
    let hour = Duration::from_secs(3600);
    assert_eq!(band_x(&band, now, hour, 600.), None);
}

#[test]
fn the_four_states_take_the_four_colors_of_the_theme() {
    let board = Theme::default().board;
    let colors: Vec<Rgb> = BandState::ALL.iter().map(|s| s.color(&board)).collect();
    assert_eq!(
        colors,
        vec![board.working, board.waiting, board.done, board.error]
    );
    let mut distinct = colors.clone();
    distinct.sort_by_key(|c| (c.0, c.1, c.2));
    distinct.dedup();
    assert_eq!(distinct.len(), 4);
}

#[test]
fn every_state_of_a_record_has_one_band_state() {
    assert_eq!(BandState::of(State::Working), Some(BandState::Working));
    let permission = State::Waiting(Waiting::Permission);
    assert_eq!(BandState::of(permission), Some(BandState::Waiting));
    let question = State::Waiting(Waiting::Question);
    assert_eq!(BandState::of(question), Some(BandState::Waiting));
    assert_eq!(BandState::of(State::Done), Some(BandState::Done));
    assert_eq!(BandState::of(State::Error), Some(BandState::Error));
    assert_eq!(BandState::of(State::Unknown), None);
}

#[test]
fn each_pane_is_a_lane_whose_states_last_until_the_next_change() {
    let claude = Some(Agent::Claude);
    let records = vec![
        record(0, "a", 1, claude, State::Working),
        record(5, "b", 2, Some(Agent::Codex), State::Working),
        record(10, "a", 1, claude, State::Waiting(Waiting::Question)),
        record(15, "b", 2, None, State::Unknown),
        record(20, "a", 1, claude, State::Done),
    ];
    let lanes = lanes(&records, 30);
    assert_eq!(lanes.len(), 2);
    assert_eq!(lanes[0].key, "a");
    assert_eq!(lanes[0].pane, 1);
    assert_eq!(lanes[0].agent, claude);
    let band = |start_ms, end_ms, state| Band {
        start_ms,
        end_ms,
        state,
    };
    assert_eq!(
        lanes[0].bands,
        vec![
            band(0, 10, BandState::Working),
            band(10, 20, BandState::Waiting),
            band(20, 30, BandState::Done),
        ]
    );
    // The agent went away: its lane keeps what it did, and no more.
    assert_eq!(lanes[1].key, "b");
    assert_eq!(lanes[1].agent, Some(Agent::Codex));
    assert_eq!(lanes[1].bands, vec![band(5, 15, BandState::Working)]);
}

/// A program that is no agent tells its state with OSC 7501.
#[test]
fn the_states_of_a_program_that_is_no_agent_are_bands_of_a_lane_without_an_agent() {
    let records = vec![
        record(0, "a", 1, Some(Agent::Claude), State::Working),
        record(10, "a", 1, None, State::Unknown),
        record(20, "a", 1, None, State::Working),
        record(30, "a", 1, None, State::Done),
        record(40, "a", 1, None, State::Unknown),
    ];
    let lanes = lanes(&records, 50);
    assert_eq!(lanes.len(), 1);
    let band = |start_ms, end_ms, state| Band {
        start_ms,
        end_ms,
        state,
    };
    assert_eq!(
        lanes[0].bands,
        vec![
            band(0, 10, BandState::Working),
            band(20, 30, BandState::Working),
            band(30, 40, BandState::Done),
        ]
    );
    // The lane is that of what was in the pane last.
    assert_eq!(lanes[0].agent, None);
}

#[test]
fn a_lane_takes_the_pane_id_of_its_last_record() {
    // The app started again, and the pane came back with another id.
    let records = vec![
        record(0, "a", 1, Some(Agent::Claude), State::Working),
        record(10, "a", 7, Some(Agent::Claude), State::Done),
    ];
    assert_eq!(lanes(&records, 20)[0].pane, 7);
}

#[test]
fn only_the_lanes_with_a_band_in_the_timeline_are_shown() {
    let now = 1_000 * MINUTE;
    let records = vec![
        record(
            now - 200 * MINUTE,
            "old",
            1,
            Some(Agent::Claude),
            State::Working,
        ),
        record(now - 120 * MINUTE, "old", 1, None, State::Unknown),
        record(
            now - 20 * MINUTE,
            "new",
            2,
            Some(Agent::Codex),
            State::Working,
        ),
    ];
    let all = lanes(&records, now);
    let keys = |span: TimeSpan| -> Vec<String> {
        visible_lanes(&all, now, span.length())
            .map(|lane| lane.key.clone())
            .collect()
    };
    assert_eq!(keys(TimeSpan::Hour), vec!["new"]);
    assert_eq!(keys(TimeSpan::Day), vec!["old", "new"]);
}

#[test]
fn the_timeline_widens_and_narrows_through_its_spans() {
    assert_eq!(TimeSpan::default(), TimeSpan::Hour);
    assert_eq!(TimeSpan::Hour.wider(), TimeSpan::SixHours);
    assert_eq!(TimeSpan::Day.wider(), TimeSpan::Day);
    assert_eq!(TimeSpan::Hour.narrower(), TimeSpan::FifteenMinutes);
    assert_eq!(
        TimeSpan::FifteenMinutes.narrower(),
        TimeSpan::FifteenMinutes
    );
    let lengths: Vec<u64> = TimeSpan::ALL.iter().map(|s| s.length().as_secs()).collect();
    assert_eq!(lengths, vec![900, 3600, 6 * 3600, 24 * 3600]);
    // A whole number of ticks fits every span.
    for span in TimeSpan::ALL {
        assert_eq!(span.length().as_secs() % span.tick().as_secs(), 0);
    }
}

/// An agent at work whose screen and files have not changed since
/// `changed_ms` has the end of its band of work marked from then on.
#[test]
fn a_stalled_agent_has_its_band_of_work_marked_since_its_last_change() {
    let claude = Some(Agent::Claude);
    let records = vec![
        record(0, "a", 1, claude, State::Done),
        record(10, "a", 1, claude, State::Working),
        record(20, "b", 2, claude, State::Working),
        record(25, "b", 2, claude, State::Done),
    ];
    let lanes = lanes(&records, 100);
    let stretch = |start_ms, end_ms| Band {
        start_ms,
        end_ms,
        state: BandState::Working,
    };
    assert_eq!(stalled_stretch(&lanes[0], 40, 100), Some(stretch(40, 100)));
    // A change older than the band: the whole band is marked.
    assert_eq!(stalled_stretch(&lanes[0], 5, 100), Some(stretch(10, 100)));
    // A lane whose agent is no longer at work has nothing marked.
    assert_eq!(stalled_stretch(&lanes[1], 22, 100), None);
}

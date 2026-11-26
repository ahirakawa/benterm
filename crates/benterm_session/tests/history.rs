//! The history of the agents' states: what is recorded, what a restart
//! finds again, what goes after the retention time, and who may write.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use benterm_session::history::{self, Change, History, Record, State, Waiting};
use benterm_session::{Agent, SessionStore};
use proptest::prelude::*;

const DAY: Duration = Duration::from_secs(24 * 60 * 60);
const WEEK: Duration = Duration::from_secs(7 * 24 * 60 * 60);

const KEY: &str = "0123456789abcdef0123456789abcdef";
const OTHER: &str = "fedcba9876543210fedcba9876543210";

/// A moment well after 1970, in whole milliseconds.
fn noon() -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000)
}

fn ms(time: SystemTime) -> u64 {
    let since = time.duration_since(SystemTime::UNIX_EPOCH).unwrap();
    u64::try_from(since.as_millis()).unwrap()
}

fn session_file(dir: &tempfile::TempDir) -> PathBuf {
    dir.path().join("session.json")
}

/// The right to write beside the session file of `dir`.
fn store(dir: &tempfile::TempDir) -> Arc<SessionStore> {
    let opened = benterm_session::open(&session_file(dir));
    Arc::new(opened.store.expect("nobody else holds the lock"))
}

fn open(dir: &tempfile::TempDir, now: SystemTime) -> History {
    History::open(store(dir), WEEK, now)
}

fn change(key: &str, agent: Option<Agent>, state: State) -> Change {
    Change {
        key: key.to_owned(),
        pane: 7,
        agent,
        state,
    }
}

fn claude(state: State) -> Change {
    change(KEY, Some(Agent::Claude), state)
}

fn file(dir: &tempfile::TempDir) -> PathBuf {
    history::history_path(&session_file(dir))
}

fn lines(path: &Path) -> Vec<String> {
    let text = fs::read_to_string(path).unwrap();
    text.lines().map(str::to_owned).collect()
}

fn states(records: &[Record]) -> Vec<State> {
    records.iter().map(|record| record.state).collect()
}

#[test]
fn a_change_is_recorded_with_its_time_pane_agent_state_and_reason() {
    let dir = tempfile::tempdir().unwrap();
    let mut history = open(&dir, noon());
    history.record(claude(State::Working), noon());
    let asked = change(
        OTHER,
        Some(Agent::Codex),
        State::Waiting(Waiting::Permission),
    );
    history.record(asked, noon() + Duration::from_secs(5));
    history.record(claude(State::Done), noon() + Duration::from_secs(9));

    let records = history.records(noon() + Duration::from_secs(9));
    assert_eq!(records.len(), 3);
    assert_eq!(
        records[0],
        Record {
            at_ms: ms(noon()),
            key: KEY.to_owned(),
            pane: 7,
            agent: Some(Agent::Claude),
            state: State::Working,
        }
    );
    assert_eq!(records[1].key, OTHER);
    assert_eq!(records[1].agent, Some(Agent::Codex));
    assert_eq!(records[1].state, State::Waiting(Waiting::Permission));
    assert_eq!(records[2].state, State::Done);
    assert!(records[2].at_ms > records[0].at_ms);
    assert_eq!(records[2].at(), noon() + Duration::from_secs(9));
}

#[test]
fn the_file_has_one_line_for_each_record_after_a_line_with_the_version() {
    let dir = tempfile::tempdir().unwrap();
    let mut history = open(&dir, noon());
    history.record(claude(State::Working), noon());
    history.record(claude(State::Waiting(Waiting::Question)), noon() + DAY);

    let lines = lines(&file(&dir));
    assert_eq!(lines.len(), 3);
    let value = |line: &str| serde_json::from_str::<serde_json::Value>(line).unwrap();
    assert_eq!(value(&lines[0]), serde_json::json!({"agent_history": 1}));
    assert_eq!(
        value(&lines[1]),
        serde_json::json!({
            "at_ms": ms(noon()), "key": KEY, "pane": 7,
            "agent": "claude", "state": "working", "reason": null,
        })
    );
    assert_eq!(value(&lines[2])["state"], "waiting");
    assert_eq!(value(&lines[2])["reason"], "question");
    let mode = fs::metadata(file(&dir)).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600, "only the user reads the history");
}

#[test]
fn the_same_state_is_not_recorded_twice_in_a_row_for_a_pane() {
    let dir = tempfile::tempdir().unwrap();
    let mut history = open(&dir, noon());
    assert!(history.record(claude(State::Working), noon()).is_some());
    assert!(
        history
            .record(claude(State::Working), noon() + DAY)
            .is_none()
    );
    // Another pane in the same state is another matter.
    let other = change(OTHER, Some(Agent::Claude), State::Working);
    assert!(history.record(other, noon() + DAY).is_some());
    // And so is another agent in the pane.
    let codex = change(KEY, Some(Agent::Codex), State::Working);
    assert!(history.record(codex, noon() + DAY).is_some());
    assert_eq!(history.records(noon() + DAY).len(), 3);
    assert_eq!(lines(&file(&dir)).len(), 4);
}

#[test]
fn a_pane_that_never_had_an_agent_has_no_records() {
    let dir = tempfile::tempdir().unwrap();
    let mut history = open(&dir, noon());
    assert!(
        history
            .record(change(KEY, None, State::Unknown), noon())
            .is_none()
    );
    history.end(KEY, noon());
    history.end_all(noon());
    assert!(history.records(noon()).is_empty());
}

#[test]
fn the_times_of_the_records_only_go_up_even_when_the_clock_goes_back() {
    let dir = tempfile::tempdir().unwrap();
    let mut history = open(&dir, noon());
    history.record(claude(State::Working), noon());
    // Two changes in the same millisecond, and one from before.
    history.record(claude(State::Done), noon());
    history.record(claude(State::Working), noon() - DAY);
    let times: Vec<u64> = history.records(noon()).iter().map(|r| r.at_ms).collect();
    assert_eq!(times, [ms(noon()), ms(noon()) + 1, ms(noon()) + 2]);
}

#[test]
fn the_records_are_there_again_after_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let mut history = open(&dir, noon());
    history.record(claude(State::Working), noon());
    history.record(claude(State::Done), noon() + Duration::from_secs(60));
    history.end_all(noon() + Duration::from_secs(90));
    let before = history.records(noon() + DAY);
    assert_eq!(
        states(&before),
        [State::Working, State::Done, State::Unknown]
    );
    drop(history);

    let history = open(&dir, noon() + DAY);
    assert_eq!(history.records(noon() + DAY), before);
    assert_eq!(history.records(noon() + DAY)[0].key, KEY);
}

#[test]
fn a_state_that_a_crash_left_open_ends_where_the_file_ends() {
    let dir = tempfile::tempdir().unwrap();
    let mut history = open(&dir, noon());
    history.record(claude(State::Working), noon());
    let other = change(OTHER, Some(Agent::Codex), State::Done);
    history.record(other, noon() + Duration::from_secs(60));
    // The app is gone without a word.
    drop(history);

    let mut history = open(&dir, noon() + DAY);
    let records = history.records(noon() + DAY);
    assert_eq!(records.len(), 4);
    let last = ms(noon() + Duration::from_secs(60));
    // Not at the time of the start: nothing ran while the app was gone.
    for (record, at_ms) in records[2..].iter().zip([last + 1, last + 2]) {
        assert_eq!(record.state, State::Unknown);
        assert_eq!(record.agent, None);
        assert_eq!(record.at_ms, at_ms);
    }
    assert_eq!(lines(&file(&dir)).len(), 5, "they are in the file too");
    // The agent that is started again in the pane is a change.
    assert!(
        history
            .record(claude(State::Working), noon() + DAY)
            .is_some()
    );
}

#[test]
fn records_older_than_the_retention_time_are_gone_at_the_start() {
    let dir = tempfile::tempdir().unwrap();
    let mut history = History::open(store(&dir), DAY, noon());
    history.record(claude(State::Working), noon());
    history.record(claude(State::Done), noon() + Duration::from_secs(1));
    history.end_all(noon() + Duration::from_secs(2));
    let other = change(OTHER, Some(Agent::Codex), State::Working);
    history.record(other, noon() + DAY + DAY - Duration::from_secs(1));
    history.end_all(noon() + DAY + DAY);
    drop(history);

    let history = History::open(store(&dir), DAY, noon() + DAY + DAY);
    let records = history.records(noon() + DAY + DAY);
    assert_eq!(states(&records), [State::Working, State::Unknown]);
    assert!(records.iter().all(|record| record.key == OTHER));
    assert_eq!(lines(&file(&dir)).len(), 3, "the file has lost them too");
}

#[test]
fn a_record_that_grows_too_old_while_the_app_runs_is_no_longer_read() {
    let dir = tempfile::tempdir().unwrap();
    let mut history = History::open(store(&dir), DAY, noon());
    history.record(claude(State::Working), noon());
    history.record(claude(State::Done), noon() + DAY);
    assert_eq!(history.records(noon() + DAY).len(), 2);
    let later = noon() + DAY + Duration::from_secs(1);
    assert_eq!(states(&history.records(later)), [State::Done]);
    assert_eq!(lines(&file(&dir)).len(), 3, "the file is not rewritten yet");

    // Once the oldest line is a day past its time, the file loses it.
    let much_later = noon() + DAY + DAY + Duration::from_secs(1);
    history.record(claude(State::Working), much_later);
    assert_eq!(states(&history.records(much_later)), [State::Working]);
    assert_eq!(lines(&file(&dir)).len(), 2);
    // And the next record goes to the file that took its place.
    history.record(claude(State::Done), much_later);
    assert_eq!(lines(&file(&dir)).len(), 3);
}

#[test]
fn records_after_a_given_time_can_be_asked_for() {
    let dir = tempfile::tempdir().unwrap();
    let mut history = open(&dir, noon());
    history.record(claude(State::Working), noon());
    history.record(claude(State::Done), noon() + Duration::from_secs(1));
    let first = history.records(noon() + DAY)[0].at_ms;
    let later = history.records_since(first, noon() + DAY);
    assert_eq!(states(&later), [State::Done]);
}

#[test]
fn a_history_without_the_lock_reads_the_file_and_never_writes_it() {
    let dir = tempfile::tempdir().unwrap();
    let mut first = open(&dir, noon());
    first.record(claude(State::Working), noon());
    let written = fs::read(file(&dir)).unwrap();

    // The first app holds the lock, so the second gets no store.
    assert!(benterm_session::open(&session_file(&dir)).store.is_none());
    let mut second = History::read_only(&session_file(&dir), WEEK, noon());
    assert_eq!(states(&second.records(noon())), [State::Working]);
    let other = change(OTHER, Some(Agent::Codex), State::Done);
    assert!(
        second.record(other, noon()).is_some(),
        "it keeps its own in memory"
    );
    second.end_all(noon());
    assert_eq!(second.records(noon()).len(), 3);
    assert_eq!(fs::read(file(&dir)).unwrap(), written);
    // A state the first app's file leaves open is the first app's.
    assert_eq!(first.records(noon()).len(), 1);
}

#[test]
fn a_history_without_a_file_keeps_its_records_in_memory() {
    let mut history = History::unsaved(WEEK);
    history.record(claude(State::Working), noon());
    assert_eq!(states(&history.records(noon())), [State::Working]);
}

#[test]
fn a_line_that_a_crash_cut_short_is_dropped_and_the_next_record_is_whole() {
    let dir = tempfile::tempdir().unwrap();
    let mut history = open(&dir, noon());
    history.record(claude(State::Working), noon());
    history.end_all(noon() + Duration::from_secs(1));
    drop(history);
    let mut text = fs::read(file(&dir)).unwrap();
    text.extend_from_slice(br#"{"at_ms":18000000"#);
    fs::write(file(&dir), text).unwrap();

    let mut history = open(&dir, noon() + DAY);
    assert_eq!(history.records(noon() + DAY).len(), 2);
    history.record(claude(State::Done), noon() + DAY);
    drop(history);
    let history = open(&dir, noon() + DAY);
    assert_eq!(
        states(&history.records(noon() + DAY)),
        [State::Working, State::Unknown, State::Done, State::Unknown]
    );
    assert!(
        lines(&file(&dir))
            .iter()
            .all(|line| serde_json::from_str::<serde_json::Value>(line).is_ok())
    );
}

#[test]
fn a_line_this_build_cannot_read_is_passed_over_and_kept() {
    let dir = tempfile::tempdir().unwrap();
    let at = ms(noon());
    let strange = format!(
        r#"{{"at_ms":{},"key":"{KEY}","pane":1,"agent":"gemini","state":"dreaming","reason":null,"new":1}}"#,
        at + 1
    );
    let old = format!(
        r#"{{"at_ms":{},"key":"{KEY}","pane":1,"agent":"claude","state":"done","reason":null}}"#,
        ms(noon() - WEEK - DAY)
    );
    let text = format!(
        "{{\"agent_history\":1}}\n{old}\n\
         {{\"at_ms\":{at},\"key\":\"{KEY}\",\"pane\":1,\"agent\":\"claude\",\"state\":\"done\",\"reason\":null,\"new\":2}}\n\
         {strange}\nnot json at all\n"
    );
    fs::write(file(&dir), text).unwrap();

    // The start rewrites the file, because a line is too old.
    let history = open(&dir, noon());
    let records = history.records(noon());
    assert_eq!(states(&records), [State::Done, State::Unknown]);
    assert!(records[1].at_ms > at + 1, "times go on after every line");
    let lines = lines(&file(&dir));
    assert_eq!(lines.len(), 4, "the header, two old lines and the end");
    assert!(lines[1].contains(r#""new":2"#));
    assert_eq!(lines[2], strange);
}

#[test]
fn a_file_of_a_newer_benterm_is_left_as_it_is() {
    let dir = tempfile::tempdir().unwrap();
    let text = format!(
        "{{\"agent_history\":2}}\n{{\"at_ms\":{},\"key\":\"{KEY}\",\"pane\":1,\"agent\":\"claude\",\"state\":\"done\",\"reason\":null}}\n",
        ms(noon() - WEEK - DAY)
    );
    fs::write(file(&dir), &text).unwrap();

    let mut history = open(&dir, noon());
    assert!(history.records(noon()).is_empty());
    assert!(history.record(claude(State::Working), noon()).is_some());
    history.end_all(noon());
    drop(history);
    assert_eq!(fs::read_to_string(file(&dir)).unwrap(), text);
}

#[test]
fn a_file_that_is_not_a_history_is_kept_as_a_copy_and_a_new_one_started() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(file(&dir), "garbage\n").unwrap();

    let mut history = open(&dir, noon());
    assert!(history.records(noon()).is_empty());
    history.record(claude(State::Working), noon());
    assert_eq!(lines(&file(&dir)).len(), 2);
    let mut copy = file(&dir).into_os_string();
    copy.push(".corrupt");
    assert_eq!(fs::read_to_string(copy).unwrap(), "garbage\n");
}

fn done_line(at_ms: u64) -> String {
    format!(
        r#"{{"at_ms":{at_ms},"key":"{KEY}","pane":1,"agent":"claude","state":"done","reason":null}}"#
    )
}

fn gone_line(at_ms: u64) -> String {
    format!(
        r#"{{"at_ms":{at_ms},"key":"{KEY}","pane":1,"agent":null,"state":"unknown","reason":null}}"#
    )
}

fn write_history(dir: &tempfile::TempDir, lines: &[String]) {
    let mut text = String::from("{\"agent_history\":1}\n");
    for line in lines {
        text.push_str(line);
        text.push('\n');
    }
    fs::write(file(dir), text).unwrap();
}

#[test]
fn a_line_whose_time_does_not_follow_the_one_before_is_passed_over_and_dropped() {
    let dir = tempfile::tempdir().unwrap();
    let at = ms(noon());
    // A time from the far future, as a clock that was wrong for a while
    // leaves it, would keep every later record up there with it.
    let lines = [
        done_line(at),
        gone_line(at + 10),
        done_line(at + 5),
        done_line(at + 10),
        done_line(u64::MAX),
        done_line(at + 10 * 24 * 60 * 60 * 1000),
    ];
    write_history(&dir, &lines);

    let mut history = open(&dir, noon());
    let times = |history: &History| -> Vec<u64> {
        let records = history.records(noon() + DAY);
        records.iter().map(|record| record.at_ms).collect()
    };
    assert_eq!(times(&history), [at, at + 10]);
    assert_eq!(lines_of(&dir).len(), 3, "the file has lost the others");
    history.record(claude(State::Working), noon() + Duration::from_secs(1));
    assert_eq!(times(&history), [at, at + 10, at + 1000]);
    assert_eq!(history.records_since(at + 10, noon()).len(), 1);
}

fn lines_of(dir: &tempfile::TempDir) -> Vec<String> {
    lines(&file(dir))
}

#[test]
fn a_link_in_the_place_of_the_history_is_not_followed() {
    let dir = tempfile::tempdir().unwrap();
    let elsewhere = dir.path().join("elsewhere.jsonl");
    let text = format!("{{\"agent_history\":1}}\n{}\n", done_line(ms(noon())));
    fs::write(&elsewhere, &text).unwrap();
    std::os::unix::fs::symlink(&elsewhere, file(&dir)).unwrap();

    let mut history = open(&dir, noon());
    assert!(!history.is_saved());
    assert!(history.records(noon()).is_empty());
    history.record(claude(State::Working), noon() + Duration::from_secs(1));
    history.end_all(noon() + Duration::from_secs(2));
    drop(history);
    assert_eq!(fs::read_to_string(&elsewhere).unwrap(), text);
    assert!(fs::symlink_metadata(file(&dir)).unwrap().is_symlink());
}

#[test]
fn a_history_that_cannot_be_made_fit_to_add_to_is_still_read() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(&dir);
    let at = ms(noon());
    // The line that was cut short has to go, but nothing can be put in
    // the directory.
    let text = format!(
        "{{\"agent_history\":1}}\n{}\n{}\n{{\"at_ms\":",
        done_line(at),
        gone_line(at + 1)
    );
    fs::write(file(&dir), text).unwrap();
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o500)).unwrap();

    let mut history = History::open(store, WEEK, noon());
    let read = history.records(noon());
    let recorded = history.record(claude(State::Working), noon());
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();

    assert_eq!(states(&read), [State::Done, State::Unknown]);
    assert!(!history.is_saved());
    assert!(recorded.is_some(), "it goes on in memory");
}

fn any_state() -> impl Strategy<Value = (Option<Agent>, State)> {
    let agent = prop_oneof![Just(Agent::Claude), Just(Agent::Codex)];
    let state = prop_oneof![
        Just(State::Working),
        Just(State::Waiting(Waiting::Permission)),
        Just(State::Waiting(Waiting::Question)),
        Just(State::Done),
        Just(State::Error),
        Just(State::Unknown),
    ];
    prop_oneof![
        1 => Just((None, State::Unknown)),
        8 => (agent, state).prop_map(|(agent, state)| (Some(agent), state)),
    ]
}

/// A step of a run: a change of one of three panes, the end of a pane, a
/// restart of the app, each after the clock moved forward or back.
#[derive(Clone, Debug)]
enum Step {
    Change(usize, (Option<Agent>, State)),
    End(usize),
    Restart,
}

fn steps() -> impl Strategy<Value = Vec<(i64, Step)>> {
    let step = prop_oneof![
        12 => (0usize..3, any_state()).prop_map(|(pane, state)| Step::Change(pane, state)),
        2 => (0usize..3).prop_map(Step::End),
        1 => Just(Step::Restart),
    ];
    // Mostly forward by up to a minute, sometimes back.
    let moved = prop_oneof![4 => 0i64..60_000, 1 => -5_000i64..0];
    prop::collection::vec((moved, step), 0..60)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// Whatever the panes do and whenever the app restarts, the times of
    /// the records go up, no pane has the same state twice in a row, and
    /// the file holds what is read.
    #[test]
    fn the_records_of_a_pane_go_forward_and_never_repeat(steps in steps()) {
        let keys = [KEY, OTHER, "00000000000000000000000000000000"];
        let dir = tempfile::tempdir().unwrap();
        let mut now = noon();
        let mut history = open(&dir, now);
        for (moved, step) in steps {
            now = if moved >= 0 {
                now + Duration::from_millis(moved.unsigned_abs())
            } else {
                now - Duration::from_millis(moved.unsigned_abs())
            };
            match step {
                Step::Change(pane, (agent, state)) => {
                    history.record(change(keys[pane], agent, state), now);
                }
                Step::End(pane) => history.end(keys[pane], now),
                Step::Restart => {
                    drop(history);
                    history = open(&dir, now);
                }
            }
        }
        // Far enough on that the clock going back hid nothing.
        let end = noon() + DAY;
        let records = history.records(end);
        for pair in records.windows(2) {
            prop_assert!(pair[0].at_ms < pair[1].at_ms);
        }
        for key in keys {
            let of_pane: Vec<_> = records.iter().filter(|r| r.key == key).collect();
            for pair in of_pane.windows(2) {
                prop_assert!(
                    (pair[0].agent, pair[0].state) != (pair[1].agent, pair[1].state),
                    "{:?} twice", pair[0]
                );
            }
            if let Some(first) = of_pane.first() {
                prop_assert!(first.agent.is_some(), "a pane starts with an agent");
            }
        }
        for record in &records {
            prop_assert!(record.agent.is_some() || record.state == State::Unknown);
        }
        drop(history);
        let read = History::read_only(&session_file(&dir), WEEK, end);
        prop_assert_eq!(read.records(end), records);
    }
}

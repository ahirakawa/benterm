//! Replaying a log into a terminal core.

use benterm_core::{AlacrittyCore, GridSize, TerminalCore};
use benterm_log::{LogReader, LogWriter, Record, replay, screen_text};
use proptest::collection::vec;
use proptest::prelude::*;

const SCROLLBACK_LIMIT: usize = 50;

fn write_log(initial_size: GridSize, records: &[Record]) -> Vec<u8> {
    let mut writer = LogWriter::new(Vec::new(), initial_size).unwrap();
    for record in records {
        writer.write_record(record).unwrap();
    }
    writer.into_inner()
}

fn replayed(bytes: &[u8]) -> AlacrittyCore {
    // The size given here is replaced by the log's initial size.
    let mut core = AlacrittyCore::new(GridSize::new(3, 3), SCROLLBACK_LIMIT);
    replay(LogReader::new(bytes).unwrap(), &mut core).unwrap();
    core
}

fn grid_size() -> impl Strategy<Value = GridSize> {
    (1u16..=30, 2u16..=90).prop_map(|(rows, cols)| GridSize::new(rows, cols))
}

fn record() -> impl Strategy<Value = Record> {
    prop_oneof![
        2 => vec(any::<u8>(), 0..40).prop_map(Record::Output),
        3 => "[ -~\r\n漢]{0,60}".prop_map(|text| Record::Output(text.into_bytes())),
        1 => grid_size().prop_map(Record::Resize),
    ]
}

proptest! {
    #[test]
    fn replaying_a_log_gives_the_same_screen_as_the_live_core(
        initial_size in grid_size(),
        records in vec(record(), 0..30),
    ) {
        let mut live = AlacrittyCore::new(initial_size, SCROLLBACK_LIMIT);
        for record in &records {
            match record {
                Record::Output(bytes) => live.feed(bytes),
                Record::Resize(size) => live.resize(*size),
            }
        }

        let replayed = replayed(&write_log(initial_size, &records));

        prop_assert_eq!(replayed.snapshot(), live.snapshot());
        let len = live.scrollback_len();
        prop_assert_eq!(replayed.scrollback_rows(0..len), live.scrollback_rows(0..len));
    }
}

#[test]
fn a_resize_in_the_log_applies_between_the_outputs_around_it() {
    let bytes = write_log(
        GridSize::new(2, 10),
        &[
            Record::Output(b"abcdefgh".to_vec()),
            Record::Resize(GridSize::new(2, 4)),
            Record::Output(b"\r\nxy".to_vec()),
        ],
    );
    let core = replayed(&bytes);
    assert_eq!(core.size(), GridSize::new(2, 4));
    assert_eq!(screen_text(&core.snapshot()).lines().last(), Some("xy"));
}

#[test]
fn screen_text_has_one_line_per_row_without_trailing_blanks() {
    let mut core = AlacrittyCore::new(GridSize::new(3, 8), SCROLLBACK_LIMIT);
    core.feed("a b  \r\n\r\n漢字".as_bytes());
    assert_eq!(screen_text(&core.snapshot()), "a b\n\n漢字\n");
}

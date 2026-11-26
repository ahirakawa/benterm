//! Writing and reading the log format.

use benterm_core::GridSize;
use benterm_log::{LogReader, LogWriter, ReadError, Record};
use proptest::collection::vec;
use proptest::prelude::*;

fn write_log(initial_size: GridSize, records: &[Record]) -> Vec<u8> {
    let mut writer = LogWriter::new(Vec::new(), initial_size).unwrap();
    for record in records {
        writer.write_record(record).unwrap();
    }
    writer.into_inner()
}

fn read_log(bytes: &[u8]) -> Result<(GridSize, Vec<Record>), ReadError> {
    let reader = LogReader::new(bytes)?;
    let initial_size = reader.initial_size();
    let records = reader.collect::<Result<Vec<_>, _>>()?;
    Ok((initial_size, records))
}

fn grid_size() -> impl Strategy<Value = GridSize> {
    (any::<u16>(), any::<u16>()).prop_map(|(rows, cols)| GridSize::new(rows, cols))
}

fn record() -> impl Strategy<Value = Record> {
    prop_oneof![
        3 => vec(any::<u8>(), 0..300).prop_map(Record::Output),
        1 => grid_size().prop_map(Record::Resize),
    ]
}

proptest! {
    #[test]
    fn reading_a_written_log_gives_back_the_same_records(
        initial_size in grid_size(),
        records in vec(record(), 0..40),
    ) {
        let bytes = write_log(initial_size, &records);
        let (read_size, read_records) = read_log(&bytes).unwrap();
        prop_assert_eq!(read_size, initial_size);
        prop_assert_eq!(read_records, records);
    }

    #[test]
    fn a_log_cut_inside_a_record_reports_truncation(
        records in vec(record(), 1..10),
        cut in any::<prop::sample::Index>(),
    ) {
        let whole = write_log(GridSize::new(24, 80), &records);
        let last_start = write_log(GridSize::new(24, 80), &records[..records.len() - 1]).len();
        let cut = last_start + 1 + cut.index(whole.len() - last_start - 1);
        prop_assume!(cut < whole.len());

        let result = read_log(&whole[..cut]);
        prop_assert!(matches!(result, Err(ReadError::Truncated)), "{:?}", result);
    }
}

#[test]
fn the_byte_layout_is_fixed() {
    let bytes = write_log(
        GridSize::new(24, 80),
        &[
            Record::Output(b"hi".to_vec()),
            Record::Resize(GridSize::new(30, 100)),
        ],
    );
    let mut expected = Vec::new();
    expected.extend_from_slice(b"BENTERMLOG");
    expected.extend_from_slice(&1u16.to_le_bytes());
    expected.extend_from_slice(&24u16.to_le_bytes());
    expected.extend_from_slice(&80u16.to_le_bytes());
    expected.push(1);
    expected.extend_from_slice(&2u32.to_le_bytes());
    expected.extend_from_slice(b"hi");
    expected.push(2);
    expected.extend_from_slice(&30u16.to_le_bytes());
    expected.extend_from_slice(&100u16.to_le_bytes());
    assert_eq!(bytes, expected);
}

#[test]
fn a_log_with_only_a_header_has_no_records() {
    let bytes = write_log(GridSize::new(5, 7), &[]);
    assert_eq!(read_log(&bytes).unwrap(), (GridSize::new(5, 7), vec![]));
}

#[test]
fn data_without_the_magic_bytes_is_not_a_log() {
    let result = read_log(b"NOTALOG!!!\x01\x00\x18\x00\x50\x00");
    assert!(matches!(result, Err(ReadError::NotALog)), "{result:?}");
}

#[test]
fn a_newer_version_is_rejected() {
    let mut bytes = write_log(GridSize::new(24, 80), &[]);
    bytes[10..12].copy_from_slice(&2u16.to_le_bytes());
    let result = read_log(&bytes);
    assert!(
        matches!(result, Err(ReadError::UnsupportedVersion(2))),
        "{result:?}"
    );
}

#[test]
fn an_unknown_record_type_is_rejected() {
    let mut bytes = write_log(GridSize::new(24, 80), &[]);
    bytes.push(9);
    let result = read_log(&bytes);
    assert!(
        matches!(result, Err(ReadError::UnknownRecord(9))),
        "{result:?}"
    );
}

#[test]
fn a_cut_header_reports_truncation() {
    let bytes = write_log(GridSize::new(24, 80), &[]);
    let result = read_log(&bytes[..11]);
    assert!(matches!(result, Err(ReadError::Truncated)), "{result:?}");
}

#[test]
fn a_huge_length_with_little_data_reports_truncation_without_allocating_it() {
    let mut bytes = write_log(GridSize::new(24, 80), &[]);
    bytes.push(1);
    bytes.extend_from_slice(&u32::MAX.to_le_bytes());
    bytes.extend_from_slice(b"abc");
    let result = read_log(&bytes);
    assert!(matches!(result, Err(ReadError::Truncated)), "{result:?}");
}

use benterm_config::{ConfigEvent, Loaded, Problem, Reloader, load, parse};
use proptest::prelude::*;
use std::io;

const FULL: &str = "[palette]\naccent = \"#ff0000\"\n\n[terminal]\nfont_size = 14\npadding = 4\n";

fn bytes(s: &str) -> io::Result<Vec<u8>> {
    Ok(s.as_bytes().to_vec())
}

fn missing() -> io::Result<Vec<u8>> {
    Err(io::Error::from(io::ErrorKind::NotFound))
}

#[test]
fn a_valid_file_is_applied() {
    let mut r = Reloader::new(Loaded::default());
    let expected = parse(FULL).unwrap();
    assert_eq!(
        r.update(bytes(FULL)),
        Some(ConfigEvent::Applied(expected.clone()))
    );
    assert_eq!(r.current(), &expected);
}

#[test]
fn the_same_file_twice_is_reported_once() {
    let mut r = Reloader::new(Loaded::default());
    assert!(r.update(bytes(FULL)).is_some());
    assert_eq!(r.update(bytes(FULL)), None);
}

#[test]
fn a_change_that_only_changes_warnings_is_applied() {
    let mut r = Reloader::new(parse(FULL).unwrap());
    let with_bogus = format!("{FULL}bogus = 1\n");
    match r.update(bytes(&with_bogus)) {
        Some(ConfigEvent::Applied(l)) => assert_eq!(l.warnings.len(), 1),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_file_cut_off_mid_save_keeps_the_previous_config() {
    let mut r = Reloader::new(parse(FULL).unwrap());
    let event = r.update(bytes("[palette]\naccent = \"#00"));
    assert!(matches!(
        event,
        Some(ConfigEvent::Rejected(ref w)) if matches!(w.problem, Problem::Syntax(_))
    ));
    assert_eq!(r.current(), &parse(FULL).unwrap());
}

#[test]
fn the_same_rejection_is_reported_once_until_a_file_is_applied() {
    let mut r = Reloader::new(Loaded::default());
    assert!(r.update(bytes("[x")).is_some());
    assert_eq!(r.update(bytes("[x")), None);
    assert!(r.update(bytes(FULL)).is_some());
    assert!(r.update(bytes("[x")).is_some());
}

#[test]
fn a_file_that_is_not_utf8_keeps_the_previous_config() {
    let mut r = Reloader::new(parse(FULL).unwrap());
    let event = r.update(Ok(vec![0xff, 0xfe]));
    assert!(matches!(
        event,
        Some(ConfigEvent::Rejected(ref w)) if matches!(w.problem, Problem::Unreadable(_))
    ));
    assert_eq!(r.current(), &parse(FULL).unwrap());
}

#[test]
fn a_read_error_keeps_the_previous_config() {
    let mut r = Reloader::new(parse(FULL).unwrap());
    let event = r.update(Err(io::Error::from(io::ErrorKind::PermissionDenied)));
    assert!(matches!(event, Some(ConfigEvent::Rejected(_))));
    assert_eq!(r.current(), &parse(FULL).unwrap());
}

#[test]
fn a_missing_file_keeps_the_previous_config_silently() {
    let mut r = Reloader::new(parse(FULL).unwrap());
    assert_eq!(r.update(missing()), None);
    assert_eq!(r.current(), &parse(FULL).unwrap());
}

#[test]
fn an_empty_file_is_valid_and_gives_defaults() {
    let mut r = Reloader::new(parse(FULL).unwrap());
    assert_eq!(
        r.update(bytes("")),
        Some(ConfigEvent::Applied(Loaded::default()))
    );
}

/// What a reader of the file could have seen while it was being saved.
#[derive(Clone, Debug)]
enum Read {
    Bytes(Vec<u8>),
    Missing,
    Denied,
}

impl Read {
    fn result(&self) -> io::Result<Vec<u8>> {
        match self {
            Read::Bytes(b) => Ok(b.clone()),
            Read::Missing => missing(),
            Read::Denied => Err(io::Error::from(io::ErrorKind::PermissionDenied)),
        }
    }
}

fn read() -> impl Strategy<Value = Read> {
    prop_oneof![
        (0..=FULL.len()).prop_map(|n| Read::Bytes(FULL.as_bytes()[..n].to_vec())),
        prop::collection::vec(any::<u8>(), 0..64).prop_map(Read::Bytes),
        "\\PC{0,40}".prop_map(|s| Read::Bytes(s.into_bytes())),
        Just(Read::Missing),
        Just(Read::Denied),
    ]
}

proptest! {
    /// The config in use is always the one from the last read that was
    /// valid TOML, or the initial one if there was none.
    #[test]
    fn the_config_comes_from_the_last_valid_read(reads in prop::collection::vec(read(), 0..12)) {
        let initial = parse(FULL).unwrap();
        let mut r = Reloader::new(initial.clone());
        let mut expected = initial;
        for read in reads {
            if let Read::Bytes(b) = &read
                && let Ok(text) = std::str::from_utf8(b)
                && let Ok(l) = parse(text)
            {
                expected = l;
            }
            let before = r.current().clone();
            let event = r.update(read.result());
            match &event {
                Some(ConfigEvent::Applied(l)) => {
                    prop_assert_eq!(l, r.current());
                    prop_assert_ne!(&before, r.current());
                }
                Some(ConfigEvent::Rejected(_)) | None => prop_assert_eq!(&before, r.current()),
            }
            prop_assert_eq!(r.current(), &expected);
        }
    }

    /// However a save is interrupted, finishing it applies the whole file.
    #[test]
    fn finishing_a_save_applies_the_whole_file(cut in 0..=FULL.len()) {
        let mut r = Reloader::new(Loaded::default());
        r.update(bytes(&FULL[..cut]));
        r.update(bytes(FULL));
        prop_assert_eq!(r.current(), &load(FULL));
    }
}

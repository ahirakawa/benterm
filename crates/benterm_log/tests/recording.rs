//! Recording to a file under the home directory.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use benterm_core::GridSize;
use benterm_log::{LogReader, Record, RecordingConfig, RecordingError};

/// A fresh directory that stands in for the home directory.
struct FakeHome(PathBuf);

impl FakeHome {
    fn new(name: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("benterm-recording-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("home")).unwrap();
        Self(root)
    }

    fn home(&self) -> PathBuf {
        self.0.join("home")
    }

    /// A directory next to the home directory, outside it.
    fn outside(&self) -> PathBuf {
        self.0.join("outside")
    }
}

impl Drop for FakeHome {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn enabled(dir: impl Into<PathBuf>) -> RecordingConfig {
    RecordingConfig {
        enabled: true,
        dir: dir.into(),
    }
}

fn read(path: &Path) -> (GridSize, Vec<Record>) {
    let reader = LogReader::new(fs::File::open(path).unwrap()).unwrap();
    let size = reader.initial_size();
    (size, reader.collect::<Result<_, _>>().unwrap())
}

fn mode(path: &Path) -> u32 {
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[test]
fn recording_is_off_by_default_and_creates_nothing() {
    let fake = FakeHome::new("default");
    let config = RecordingConfig::default();

    assert!(!config.enabled);
    assert!(config.dir.is_relative());
    let recorder = config.start(&fake.home(), GridSize::new(24, 80)).unwrap();
    assert!(recorder.is_none());
    assert_eq!(fs::read_dir(fake.home()).unwrap().count(), 0);
}

#[test]
fn outputs_and_resizes_are_in_the_file_in_order_while_recording() {
    let fake = FakeHome::new("order");
    let mut recorder = enabled("logs")
        .start(&fake.home(), GridSize::new(24, 80))
        .unwrap()
        .unwrap();
    recorder.output(b"one").unwrap();
    recorder.resize(GridSize::new(30, 100)).unwrap();
    recorder.output(b"two").unwrap();

    let home = fs::canonicalize(fake.home()).unwrap();
    assert!(recorder.path().starts_with(home.join("logs")));
    // Read before the recorder is dropped: every record is flushed at once
    // so the log survives a crash of the app.
    assert_eq!(
        read(recorder.path()),
        (
            GridSize::new(24, 80),
            vec![
                Record::Output(b"one".to_vec()),
                Record::Resize(GridSize::new(30, 100)),
                Record::Output(b"two".to_vec()),
            ]
        )
    );
}

#[test]
fn only_the_user_can_read_the_log_and_its_directory() {
    let fake = FakeHome::new("mode");
    let recorder = enabled("a/logs")
        .start(&fake.home(), GridSize::new(24, 80))
        .unwrap()
        .unwrap();

    assert_eq!(mode(recorder.path()), 0o600);
    assert_eq!(mode(&fake.home().join("a/logs")), 0o700);
}

#[test]
fn each_recorder_gets_its_own_file() {
    let fake = FakeHome::new("distinct");
    let config = enabled("logs");
    let first = config
        .start(&fake.home(), GridSize::new(24, 80))
        .unwrap()
        .unwrap();
    let second = config
        .start(&fake.home(), GridSize::new(24, 80))
        .unwrap()
        .unwrap();

    assert_ne!(first.path(), second.path());
}

#[test]
fn an_absolute_directory_inside_home_is_accepted() {
    let fake = FakeHome::new("absolute");
    let recorder = enabled(fake.home().join("logs"))
        .start(&fake.home(), GridSize::new(24, 80))
        .unwrap();

    assert!(recorder.is_some());
}

#[test]
fn a_directory_outside_home_is_rejected_before_anything_is_created() {
    let fake = FakeHome::new("outside");
    let result = enabled(fake.outside()).start(&fake.home(), GridSize::new(24, 80));

    assert!(matches!(result, Err(RecordingError::OutsideHome(_))));
    assert!(!fake.outside().exists());
}

#[test]
fn a_relative_directory_that_climbs_out_of_home_is_rejected() {
    let fake = FakeHome::new("climb");
    let result = enabled("../outside").start(&fake.home(), GridSize::new(24, 80));

    assert!(matches!(result, Err(RecordingError::OutsideHome(_))));
    assert!(!fake.outside().exists());
}

#[test]
fn a_symlink_in_home_that_points_outside_is_rejected() {
    let fake = FakeHome::new("symlink");
    fs::create_dir(fake.outside()).unwrap();
    std::os::unix::fs::symlink(fake.outside(), fake.home().join("link")).unwrap();

    let result = enabled("link/logs").start(&fake.home(), GridSize::new(24, 80));

    assert!(matches!(result, Err(RecordingError::OutsideHome(_))));
    assert_eq!(fs::read_dir(fake.outside()).unwrap().count(), 0);
}

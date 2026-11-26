//! Recording the PTY output of a running terminal to a file.

use std::fmt;
use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{self, BufWriter};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use benterm_core::GridSize;

use crate::format::{LogWriter, Record};

/// Whether and where to record. Recording is off unless `enabled` is set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordingConfig {
    pub enabled: bool,
    /// The directory for the logs. A relative path is taken relative to the
    /// home directory. The directory must be inside the home directory.
    pub dir: PathBuf,
}

impl Default for RecordingConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            dir: PathBuf::from("Library/Application Support/benterm/pty-logs"),
        }
    }
}

impl RecordingConfig {
    /// Starts a new log for one terminal whose grid is `initial_size`.
    /// Returns `None` when recording is disabled.
    pub fn start(
        &self,
        home: &Path,
        initial_size: GridSize,
    ) -> Result<Option<Recorder>, RecordingError> {
        if !self.enabled {
            return Ok(None);
        }
        let dir = create_dir_in_home(home, &self.dir)?;
        let path = dir.join(file_name());
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .map_err(RecordingError::Io)?;
        let mut writer =
            LogWriter::new(BufWriter::new(file), initial_size).map_err(RecordingError::Io)?;
        writer.flush().map_err(RecordingError::Io)?;
        Ok(Some(Recorder { writer, path }))
    }
}

/// Creates `dir` one level at a time below `home`, checking after each step
/// that the path so far still resolves to a place inside `home`. A symlink
/// pointing out of `home` is caught before anything is created at its
/// target. New directories are readable by the user only.
fn create_dir_in_home(home: &Path, dir: &Path) -> Result<PathBuf, RecordingError> {
    let outside = || RecordingError::OutsideHome(dir.to_path_buf());
    let relative = if dir.is_absolute() {
        dir.strip_prefix(home).map_err(|_| outside())?
    } else {
        dir
    };
    let home = fs::canonicalize(home).map_err(RecordingError::Io)?;

    let mut current = home.clone();
    for component in relative.components() {
        match component {
            Component::Normal(name) => current.push(name),
            Component::CurDir => continue,
            _ => return Err(outside()),
        }
        if !current.exists() {
            DirBuilder::new()
                .mode(0o700)
                .create(&current)
                .map_err(RecordingError::Io)?;
        }
        current = fs::canonicalize(&current).map_err(RecordingError::Io)?;
        if !current.starts_with(&home) {
            return Err(outside());
        }
    }
    Ok(current)
}

/// A name no other recorder uses: the time, the process and a counter.
fn file_name() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    let count = NEXT.fetch_add(1, Ordering::Relaxed);
    format!("pty-{seconds}-{}-{count}.bentermlog", std::process::id())
}

/// Writes one terminal's PTY output and resizes to a log file.
///
/// Call [`output`](Self::output) with the bytes fed to the core and
/// [`resize`](Self::resize) with the size given to the core, in the order the
/// core receives them. Each call writes through to the file, so the log is
/// complete up to the last call even if the app crashes.
pub struct Recorder {
    writer: LogWriter<BufWriter<File>>,
    path: PathBuf,
}

impl Recorder {
    pub fn output(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.writer.write_record(&Record::Output(bytes.to_vec()))?;
        self.writer.flush()
    }

    pub fn resize(&mut self, size: GridSize) -> io::Result<()> {
        self.writer.write_record(&Record::Resize(size))?;
        self.writer.flush()
    }

    /// The log file.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Why recording could not start.
#[derive(Debug)]
pub enum RecordingError {
    /// The configured directory is not inside the home directory.
    OutsideHome(PathBuf),
    Io(io::Error),
}

impl fmt::Display for RecordingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OutsideHome(dir) => write!(
                f,
                "the PTY log directory {} is not inside the home directory",
                dir.display()
            ),
            Self::Io(error) => write!(f, "cannot create the PTY log: {error}"),
        }
    }
}

impl std::error::Error for RecordingError {}

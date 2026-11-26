//! Starting a terminal's PTY log from the `[pty_log]` settings.

use std::io;
use std::path::Path;

use benterm_config::PtyLogSettings;
use benterm_core::GridSize;
use benterm_log::{Recorder, RecordingConfig, RecordingError};

/// Starts a log for a terminal whose grid starts at `initial_size`, if the
/// settings enable recording. `home` is the directory the settings' `dir`
/// must be inside; recording cannot start without one.
pub fn start_recorder(
    settings: &PtyLogSettings,
    home: Option<&Path>,
    initial_size: GridSize,
) -> Result<Option<Recorder>, RecordingError> {
    if !settings.enabled {
        return Ok(None);
    }
    let Some(home) = home else {
        let error = io::Error::new(io::ErrorKind::NotFound, "HOME is not set");
        return Err(RecordingError::Io(error));
    };
    let config = RecordingConfig {
        enabled: true,
        dir: settings.dir.clone(),
    };
    config.start(home, initial_size)
}

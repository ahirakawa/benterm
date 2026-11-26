//! Watching the file and reporting each save.

use crate::file::load_file;
use crate::read::Loaded;
use crate::reload::{ConfigEvent, Reloader};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread::{self, JoinHandle};
use std::time::Duration;
use std::{fs, io};

/// How long the file must go without changes before it is read. An editor
/// that writes a file in pieces sends events until it finishes, so waiting
/// for quiet avoids reading a file that is valid TOML but not yet complete.
const SETTLE: Duration = Duration::from_millis(100);

/// Watches the config file and calls back on a background thread after each
/// save. Dropping it stops the watching.
pub struct ConfigWatcher {
    watcher: Option<RecommendedWatcher>,
    thread: Option<JoinHandle<()>>,
}

impl ConfigWatcher {
    /// Starts watching `path` and returns the config read from it. Creates
    /// the directory of `path` if it does not exist, so that a file created
    /// later is seen.
    ///
    /// The directory is watched rather than the file, because many editors
    /// save by writing a new file and renaming it over the old one.
    pub fn start(
        path: impl Into<PathBuf>,
        on_event: impl FnMut(ConfigEvent) + Send + 'static,
    ) -> io::Result<(Self, Loaded)> {
        let path = path.into();
        let invalid = || io::Error::new(io::ErrorKind::InvalidInput, "not a file path");
        let name = path.file_name().ok_or_else(invalid)?.to_owned();
        let dir = path.parent().ok_or_else(invalid)?;
        let dir = if dir.as_os_str().is_empty() {
            PathBuf::from(".")
        } else {
            dir.to_path_buf()
        };
        fs::create_dir_all(&dir)?;
        let dir = dir.canonicalize()?;
        let path = dir.join(&name);

        let (tx, rx) = mpsc::channel();
        let mut watcher = notify::recommended_watcher(tx).map_err(io::Error::other)?;
        watcher
            .watch(&dir, RecursiveMode::NonRecursive)
            .map_err(io::Error::other)?;
        // Read only after the watch starts, so no save is missed in between.
        let loaded = load_file(&path);
        let reloader = Reloader::new(loaded.clone());
        let thread = thread::Builder::new()
            .name("benterm-config-watch".into())
            .spawn(move || run(rx, path, name, reloader, on_event))?;
        Ok((
            Self {
                watcher: Some(watcher),
                thread: Some(thread),
            },
            loaded,
        ))
    }
}

impl Drop for ConfigWatcher {
    fn drop(&mut self) {
        // Dropping the watcher closes the channel, which ends the thread.
        drop(self.watcher.take());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn run(
    rx: Receiver<notify::Result<Event>>,
    path: PathBuf,
    name: OsString,
    mut reloader: Reloader,
    mut on_event: impl FnMut(ConfigEvent),
) {
    let concerns_file = |event: &notify::Result<Event>| match event {
        Ok(e) => {
            !matches!(e.kind, EventKind::Access(_))
                && e.paths.iter().any(|p| p.file_name() == Some(&name))
        }
        // Events may have been lost, so read the file to be safe.
        Err(_) => true,
    };
    while let Ok(event) = rx.recv() {
        if !concerns_file(&event) {
            continue;
        }
        loop {
            match rx.recv_timeout(SETTLE) {
                Ok(_) => {}
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
        if let Some(event) = reloader.update(fs::read(&path)) {
            on_event(event);
        }
    }
}

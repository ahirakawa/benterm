//! Replays a PTY log and prints the final screen.
//!
//! Usage: `benterm-replay <log>`

use std::fs::File;
use std::io::{BufReader, Write};
use std::process::ExitCode;

use benterm_core::{AlacrittyCore, TerminalCore};
use benterm_log::{LogReader, replay, screen_text};

/// Only the screen is printed, so the scrollback just has to be large enough
/// for the screen to come out the same as in the app.
const SCROLLBACK_LIMIT: usize = 10_000;

fn main() -> ExitCode {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let [path] = args.as_slice() else {
        eprintln!("usage: benterm-replay <log>");
        return ExitCode::from(2);
    };

    let result = File::open(path)
        .map_err(|error| error.to_string())
        .and_then(|file| {
            let log = LogReader::new(BufReader::new(file)).map_err(|error| error.to_string())?;
            let mut core = AlacrittyCore::new(log.initial_size(), SCROLLBACK_LIMIT);
            replay(log, &mut core).map_err(|error| error.to_string())?;
            Ok(screen_text(&core.snapshot()))
        });

    match result {
        Ok(text) => {
            if std::io::stdout().write_all(text.as_bytes()).is_err() {
                return ExitCode::FAILURE;
            }
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("benterm-replay: {}: {error}", path.display());
            ExitCode::FAILURE
        }
    }
}

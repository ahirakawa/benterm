mod common;

use std::time::{Duration, Instant};

use benterm_pty::{PtyConfig, PtySession};
use common::{assert_bytes_eq, collect_to_exit, read_slowly};

fn sh(script: &str) -> PtyConfig {
    PtyConfig::command("/bin/sh", ["-c", script])
}

/// Turns off the terminal's output processing, then runs `script`, so the
/// master reads exactly the bytes the script writes.
///
/// With output processing on (ONLCR), the kernel writes CR LF for each LF,
/// and macOS writes the CR twice when the output queue fills between the
/// CR and the LF. That is harmless on screen but makes the bytes depend on
/// how fast the reader keeps up.
fn raw_output(script: &str) -> String {
    format!("stty -opost && {script}")
}

/// A large output written right before the child exits arrives whole
/// and in order before the exit event.
#[test]
fn large_output_arrives_whole_and_in_order() {
    let (_session, events) = PtySession::spawn(sh(&raw_output("seq 1 200000"))).unwrap();
    let collected = collect_to_exit(&events);
    assert_bytes_eq(&collected.output, &seq(200_000));
    assert!(collected.exit.unwrap().success());
}

fn seq(last: u32) -> Vec<u8> {
    (1..=last)
        .flat_map(|n| format!("{n}\n").into_bytes())
        .collect()
}

/// The premise of the test above, checked on a PTY without `PtySession`:
/// with output processing off, output read after the kernel output queue
/// has filled up many times is exactly what the child wrote.
#[test]
fn output_is_exact_when_the_reader_falls_behind() {
    let output = read_slowly(&raw_output("seq 1 20000"));
    assert_bytes_eq(&output, &seq(20_000));
}

/// The shell exits while a background job it started
/// still has the terminal open. The exit event must arrive and the channel
/// must close without waiting for the job. (On macOS the kernel revokes the
/// terminal when the session leader exits, so the master reports EOF here.)
#[test]
fn exit_is_detected_while_a_background_job_holds_the_terminal() {
    let started = Instant::now();
    let (_session, events) = PtySession::spawn(sh("sleep 30 & echo bye")).unwrap();
    let collected = collect_to_exit(&events);
    assert!(collected.text().contains("bye"), "{:?}", collected.text());
    assert!(collected.exit.unwrap().success());
    assert!(started.elapsed() < Duration::from_secs(5));
}

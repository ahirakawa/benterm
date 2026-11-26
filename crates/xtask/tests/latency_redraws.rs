use std::path::Path;
use std::time::Duration;

use xtask::latency::redraws::{Counts, agent_run_arguments, parse_counts, redraw_table};

#[test]
fn the_counts_are_read_from_the_file_benterm_writes() {
    let counts = parse_counts(
        "elapsed_ms 2500\nframes 120\nvisible_redraws 30\nhidden_redraws 900\n\
         wake_locks 3720\nwake_lock_wait_us 1500\n",
    )
    .unwrap();
    assert_eq!(
        counts,
        Counts {
            elapsed: Duration::from_millis(2500),
            frames: 120,
            visible_redraws: 30,
            hidden_redraws: 900,
            wake_locks: 3720,
            wake_lock_wait: Duration::from_micros(1500),
        }
    );
}

#[test]
fn a_file_that_is_not_the_counts_is_an_error() {
    assert!(parse_counts("frames many\n").is_err());
    assert!(parse_counts("frames\n").is_err());
    assert!(parse_counts("pixels 3\n").is_err());
}

#[test]
fn the_counts_of_a_stretch_are_the_later_counts_less_the_earlier() {
    let earlier = Counts {
        elapsed: Duration::from_millis(1000),
        frames: 10,
        visible_redraws: 1,
        hidden_redraws: 2,
        wake_locks: 12,
        wake_lock_wait: Duration::from_micros(30),
    };
    let later = Counts {
        elapsed: Duration::from_millis(4000),
        frames: 70,
        visible_redraws: 3,
        hidden_redraws: 102,
        wake_locks: 432,
        wake_lock_wait: Duration::from_micros(330),
    };
    assert_eq!(
        later - earlier,
        Counts {
            elapsed: Duration::from_millis(3000),
            frames: 60,
            visible_redraws: 2,
            hidden_redraws: 100,
            wake_locks: 420,
            wake_lock_wait: Duration::from_micros(300),
        }
    );
}

#[test]
fn the_table_puts_the_quiet_stretch_beside_the_busy_one() {
    let quiet = Counts {
        elapsed: Duration::from_secs(2),
        frames: 4,
        ..Counts::default()
    };
    let busy = Counts {
        elapsed: Duration::from_secs(4),
        frames: 240,
        visible_redraws: 0,
        hidden_redraws: 400,
        wake_locks: 1600,
        wake_lock_wait: Duration::from_micros(1000),
    };
    assert_eq!(
        redraw_table(&quiet, &busy, 10),
        "\
| Counted | No output | 10 terminals behind the window writing |
|---|---|---|
| Seconds | 2.0 | 4.0 |
| Frames drawn | 4 (2.0/s) | 240 (60.0/s) |
| Redraws asked by terminals on view | 0 (0.0/s) | 0 (0.0/s) |
| Redraws asked by terminals not on view | 0 (0.0/s) | 400 (100.0/s) |
| State locks taken on wakes | 0 (0.0/s) | 1600 (400.0/s) |
| Locks per wake | - | 4.0 |
| Lock wait per wake, µs | - | 2.5 |
| Lock wait in all, ms | 0.0 | 1.0 |
"
    );
}

#[test]
fn a_stretch_of_no_time_has_no_rates() {
    let none = Counts::default();
    let table = redraw_table(&none, &none, 1);
    assert!(
        table.contains("| Frames drawn | 0 (-) | 0 (-) |"),
        "{table}"
    );
}

#[test]
fn each_writer_runs_as_an_agent_behind_the_window() {
    assert_eq!(
        agent_run_arguments(
            "redraws-3",
            Path::new("/build/xtask"),
            100,
            3,
            Path::new("/tmp/run/started")
        ),
        [
            "agent",
            "run",
            "--name",
            "redraws-3",
            "--",
            "/build/xtask",
            "flood",
            "--rate",
            "100",
            "--pane",
            "3",
            "--started-dir",
            "/tmp/run/started",
        ]
    );
}
